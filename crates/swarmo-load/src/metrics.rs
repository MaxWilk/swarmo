//! The metrics pipeline: bounded sample channel -> aggregator task -> 1 Hz
//! snapshots.
//!
//! Memory is O(distinct tags), never O(requests): samples are folded into HDR
//! histograms on arrival and never retained individually.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use hdrhistogram::Histogram;
use swarmo_core::model::{
    CheckStats, DistBucket, ErrorCount, RoundStats, Snapshot, StatusCount, TagStats, Threshold,
    ThresholdOp, ThresholdResult,
};
use swarmo_core::store::Protocol;
use tokio::sync::{broadcast, mpsc};

/// Bounded so a slow aggregator can never stall the request hot path.
pub const CHANNEL_CAPACITY: usize = 65_536;

/// 1µs .. 60s at 3 significant figures.
const HIST_LOW: u64 = 1;
// One hour: a configurable timeout can legitimately exceed a minute, and a
// clamped maximum below the mean it sits under is a nonsense to display.
const HIST_HIGH: u64 = 3_600_000_000;
const HIST_SIGFIG: u8 = 3;
/// Distinct error messages kept before the rest are pooled.
const MAX_ERROR_KINDS: usize = 20;
const OTHER_ERRORS: &str = "(other errors)";

#[derive(Debug, Clone)]
pub struct Sample {
    pub tag: Arc<str>,
    pub protocol: Protocol,
    pub status: u16,
    pub ok: bool,
    pub latency_us: u64,
    pub bytes_in: u64,
    /// Request body bytes put on the wire (HTTP body / encoded protobuf).
    pub bytes_out: u64,
    /// Why the request failed, when it failed without a response.
    pub error: Option<Arc<str>>,
}

#[derive(Debug, Clone)]
pub enum MetricMsg {
    Sample(Sample),
    Checks(Vec<(String, u64, u64)>),
    /// A round of a fixed-count run began. Samples until the matching end
    /// belong to it as well as to the run as a whole.
    RoundStarted {
        index: u32,
        planned: u64,
        concurrency: u32,
    },
    /// A round finished. `wall_sec` is the round alone, without its pause.
    RoundEnded {
        wall_sec: f64,
        gap_sec: u64,
    },
}

/// Shared counters the scheduler and hot path update directly.
#[derive(Debug, Default)]
pub struct RunCounters {
    pub samples_dropped: AtomicU64,
    pub dropped_iterations: AtomicU64,
    pub active_vus: AtomicU64,
    pub target_vus: AtomicU64,
    pub vus_saturated: AtomicU64,
}

/// A cheap, cloneable handle for emitting metrics from worker tasks.
#[derive(Clone)]
pub struct MetricsSink {
    tx: mpsc::Sender<MetricMsg>,
    counters: Arc<RunCounters>,
}

impl MetricsSink {
    /// Never blocks: on a full channel the sample is dropped and counted.
    pub fn sample(&self, s: Sample) {
        if self.tx.try_send(MetricMsg::Sample(s)).is_err() {
            self.counters
                .samples_dropped
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn checks(&self, checks: Vec<(String, u64, u64)>) {
        if checks.is_empty() {
            return;
        }
        // Dropped telemetry is dropped telemetry: a lost batch of check
        // results skews `checks.rate` exactly as a lost sample skews the
        // rest, so it counts under the same warning rather than vanishing.
        if self.tx.try_send(MetricMsg::Checks(checks)).is_err() {
            self.counters
                .samples_dropped
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Mark the start of a round, so its samples can be split out.
    ///
    /// Awaited, unlike a sample: these markers are sent from the engine's own
    /// loop, not a worker hot path, and dropping one is not a lost data point
    /// but a lost round. `round_ended` in particular fires right after a burst
    /// of thousands of samples — precisely when the channel is fullest — and a
    /// dropped end marker would leave the round open for the next one to
    /// overwrite. Awaiting also keeps the marker behind every sample already
    /// queued, so the round's stats close over exactly its own requests.
    pub async fn round_started(&self, index: u32, planned: u64, concurrency: u32) {
        let _ = self
            .tx
            .send(MetricMsg::RoundStarted {
                index,
                planned,
                concurrency,
            })
            .await;
    }

    /// Close the current round off at the wall time it took.
    pub async fn round_ended(&self, wall_sec: f64, gap_sec: u64) {
        let _ = self
            .tx
            .send(MetricMsg::RoundEnded { wall_sec, gap_sec })
            .await;
    }

    pub fn dropped_iteration(&self) {
        // The atomic is the record; nothing is sent, so a burst of drops does
        // not also crowd real samples out of the channel.
        self.counters
            .dropped_iterations
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn counters(&self) -> &Arc<RunCounters> {
        &self.counters
    }
}

/// A round in progress: the same per-tag machinery, scoped to one round.
struct RoundAccum {
    index: u32,
    planned: u64,
    concurrency: u32,
    agg: TagAgg,
}

struct TagAgg {
    cumulative: Histogram<u64>,
    count: u64,
    errors: u64,
    sum_us: u128,
    bytes_in: u64,
    bytes_out: u64,
}

impl TagAgg {
    fn new() -> Self {
        Self {
            cumulative: Histogram::new_with_bounds(HIST_LOW, HIST_HIGH, HIST_SIGFIG).unwrap(),
            count: 0,
            errors: 0,
            sum_us: 0,
            bytes_in: 0,
            bytes_out: 0,
        }
    }

    fn record(&mut self, s: &Sample) {
        let v = s.latency_us.clamp(HIST_LOW, HIST_HIGH);
        let _ = self.cumulative.record(v);
        self.count += 1;
        self.sum_us += s.latency_us as u128;
        self.bytes_in += s.bytes_in;
        self.bytes_out += s.bytes_out;
        if !s.ok {
            self.errors += 1;
        }
    }

    fn stats(&self, tag: &str) -> TagStats {
        TagStats {
            tag: tag.to_string(),
            count: self.count,
            errors: self.errors,
            // An empty histogram reports a min of u64::MAX, which would render
            // as an absurd latency; report zero until something is recorded.
            min: if self.count == 0 {
                0.0
            } else {
                ms(self.cumulative.min())
            },
            p50: ms(self.cumulative.value_at_quantile(0.50)),
            p90: ms(self.cumulative.value_at_quantile(0.90)),
            p95: ms(self.cumulative.value_at_quantile(0.95)),
            p99: ms(self.cumulative.value_at_quantile(0.99)),
            p999: ms(self.cumulative.value_at_quantile(0.999)),
            max: ms(self.cumulative.max()),
            avg: if self.count == 0 {
                0.0
            } else {
                (self.sum_us as f64 / self.count as f64) / 1000.0
            },
            bytes_in: self.bytes_in,
            bytes_out: self.bytes_out,
        }
    }
}

fn ms(micros: u64) -> f64 {
    micros as f64 / 1000.0
}

/// Accumulated state for the whole run. Owned by the aggregator task.
pub struct Aggregator {
    per_tag: HashMap<Arc<str>, TagAgg>,
    overall: TagAgg,
    checks: HashMap<String, (u64, u64)>,
    status_codes: HashMap<(Protocol, u16), u64>,
    errors_by_message: HashMap<Arc<str>, u64>,
    /// Requests seen since the last snapshot, for the instantaneous rate.
    interval_count: u64,
    /// Failures and latencies since the last snapshot.
    ///
    /// Kept apart from the cumulative figures because they answer a different
    /// question. A run that goes ten minutes clean and then falls over has a
    /// cumulative error rate near zero; only the recent window says it is
    /// failing *now*, which is what stopping at a breaking point depends on.
    interval_errors: u64,
    interval_hist: Histogram<u64>,
    /// The round being recorded, if a fixed-count run is under way.
    current_round: Option<RoundAccum>,
    /// Rounds already finished, in order.
    rounds: Vec<RoundStats>,
    /// Bytes seen since the last snapshot, for instantaneous throughput.
    interval_bytes: u64,
    interval_bytes_out: u64,
    /// The highest one-second rate any interval reached.
    peak_rps: f64,
    thresholds: Vec<Threshold>,
    counters: Arc<RunCounters>,
    /// When recording began, for rates taken over the whole run.
    started: Instant,
}

impl Aggregator {
    pub fn new(thresholds: Vec<Threshold>, counters: Arc<RunCounters>) -> Self {
        Self {
            per_tag: HashMap::new(),
            overall: TagAgg::new(),
            checks: HashMap::new(),
            status_codes: HashMap::new(),
            errors_by_message: HashMap::new(),
            interval_count: 0,
            interval_errors: 0,
            current_round: None,
            rounds: Vec::new(),
            interval_hist: Histogram::new_with_bounds(HIST_LOW, HIST_HIGH, HIST_SIGFIG).unwrap(),
            interval_bytes: 0,
            interval_bytes_out: 0,
            peak_rps: 0.0,
            thresholds,
            counters,
            started: Instant::now(),
        }
    }

    pub fn apply(&mut self, msg: MetricMsg) {
        match msg {
            MetricMsg::Sample(s) => {
                self.overall.record(&s);
                *self.status_codes.entry((s.protocol, s.status)).or_insert(0) += 1;
                if let Some(err) = &s.error {
                    // Bounded so a server emitting a unique message per request
                    // cannot grow this without limit. The common failures are
                    // few, and they are the ones worth naming.
                    if self.errors_by_message.len() < MAX_ERROR_KINDS
                        || self.errors_by_message.contains_key(err)
                    {
                        *self.errors_by_message.entry(err.clone()).or_insert(0) += 1;
                    } else {
                        *self
                            .errors_by_message
                            .entry(Arc::from(OTHER_ERRORS))
                            .or_insert(0) += 1;
                    }
                }
                self.interval_count += 1;
                self.interval_bytes += s.bytes_in;
                self.interval_bytes_out += s.bytes_out;
                if !s.ok {
                    self.interval_errors += 1;
                }
                let _ = self
                    .interval_hist
                    .record(s.latency_us.clamp(HIST_LOW, HIST_HIGH));
                self.per_tag
                    .entry(s.tag.clone())
                    .or_insert_with(TagAgg::new)
                    .record(&s);
                if let Some(round) = self.current_round.as_mut() {
                    round.agg.record(&s);
                }
            }
            MetricMsg::Checks(list) => {
                for (name, passes, fails) in list {
                    let e = self.checks.entry(name).or_insert((0, 0));
                    e.0 += passes;
                    e.1 += fails;
                }
            }
            MetricMsg::RoundStarted {
                index,
                planned,
                concurrency,
            } => {
                self.current_round = Some(RoundAccum {
                    index,
                    planned,
                    concurrency,
                    agg: TagAgg::new(),
                });
            }
            MetricMsg::RoundEnded { wall_sec, gap_sec } => {
                if let Some(round) = self.current_round.take() {
                    let stats = round.agg.stats(&format!("round {}", round.index));
                    self.rounds.push(RoundStats {
                        index: round.index,
                        planned: round.planned,
                        concurrency: round.concurrency,
                        wall_sec,
                        // Measured within the round, so the pauses between
                        // rounds cannot drag the figure down.
                        rps: if wall_sec > 0.0 {
                            stats.count as f64 / wall_sec
                        } else {
                            0.0
                        },
                        gap_sec,
                        stats,
                    });
                }
            }
        }
    }

    /// The rounds recorded so far, in execution order.
    pub fn rounds(&self) -> Vec<RoundStats> {
        self.rounds.clone()
    }

    pub fn total_requests(&self) -> u64 {
        self.overall.count
    }

    pub fn error_rate(&self) -> f64 {
        if self.overall.count == 0 {
            0.0
        } else {
            self.overall.errors as f64 / self.overall.count as f64
        }
    }

    fn check_failure_rate(&self) -> f64 {
        let (p, f) = self
            .checks
            .values()
            .fold((0u64, 0u64), |(p, f), (a, b)| (p + a, f + b));
        if p + f == 0 {
            0.0
        } else {
            f as f64 / (p + f) as f64
        }
    }

    pub fn per_tag_stats(&self) -> Vec<TagStats> {
        let mut v: Vec<TagStats> = self
            .per_tag
            .iter()
            .map(|(tag, agg)| agg.stats(tag))
            .collect();
        v.sort_by(|a, b| b.count.cmp(&a.count).then(a.tag.cmp(&b.tag)));
        v
    }

    pub fn overall_stats(&self) -> TagStats {
        self.overall.stats("overall")
    }

    pub fn check_stats(&self) -> Vec<CheckStats> {
        let mut v: Vec<CheckStats> = self
            .checks
            .iter()
            .map(|(name, (p, f))| CheckStats {
                name: name.clone(),
                passes: *p,
                fails: *f,
            })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn status_code_counts(&self) -> Vec<StatusCount> {
        let mut v: Vec<StatusCount> = self
            .status_codes
            .iter()
            .map(|((protocol, code), count)| StatusCount {
                protocol: *protocol,
                code: *code,
                count: *count,
            })
            .collect();
        v.sort_by(|a, b| b.count.cmp(&a.count).then(a.code.cmp(&b.code)));
        v
    }

    /// Failures grouped by message, most frequent first.
    pub fn error_counts(&self) -> Vec<ErrorCount> {
        let mut v: Vec<ErrorCount> = self
            .errors_by_message
            .iter()
            .map(|(message, count)| ErrorCount {
                message: message.to_string(),
                count: *count,
            })
            .collect();
        v.sort_by(|a, b| b.count.cmp(&a.count).then(a.message.cmp(&b.message)));
        v
    }

    pub fn bytes_in(&self) -> u64 {
        self.overall.bytes_in
    }

    pub fn bytes_out(&self) -> u64 {
        self.overall.bytes_out
    }

    pub fn peak_rps(&self) -> f64 {
        self.peak_rps
    }

    /// The shape of the latency distribution, as evenly spaced buckets.
    ///
    /// The histogram itself is far too detailed to ship in a summary, so it is
    /// resampled onto a fixed number of linear buckets. The step is rounded to
    /// a 1-2-5 series so the axis reads in round numbers rather than in
    /// arbitrary fractions of whatever the slowest request happened to be.
    pub fn latency_distribution(&self, buckets: usize) -> Vec<DistBucket> {
        if self.overall.count == 0 || buckets == 0 {
            return Vec::new();
        }
        let max_us = self.overall.cumulative.max().max(1);
        let step_us = nice_step(max_us as f64 / buckets as f64).max(1.0) as u64;

        self.overall
            .cumulative
            .iter_linear(step_us)
            .map(|v| DistBucket {
                upper_ms: v.value_iterated_to() as f64 / 1000.0,
                count: v.count_since_last_iteration(),
            })
            .collect()
    }

    /// Resolve a threshold's metric+stat against current cumulative state.
    fn metric_value(&self, metric: &str, stat: &str) -> Option<f64> {
        let dur = |q: f64| ms(self.overall.cumulative.value_at_quantile(q));
        match metric {
            "http_req_duration" => Some(match stat {
                "p50" | "med" => dur(0.50),
                "p90" => dur(0.90),
                "p95" => dur(0.95),
                "p99" => dur(0.99),
                "max" => ms(self.overall.cumulative.max()),
                "avg" => self.overall.stats("").avg,
                _ => return None,
            }),
            "http_req_failed" => Some(match stat {
                "rate" => self.error_rate(),
                "count" => self.overall.errors as f64,
                _ => return None,
            }),
            "http_reqs" => Some(match stat {
                "count" => self.overall.count as f64,
                // Requests per second over the run so far — the figure the
                // summary reports as rps.
                "rate" => {
                    let secs = self.started.elapsed().as_secs_f64();
                    if secs > 0.0 {
                        self.overall.count as f64 / secs
                    } else {
                        0.0
                    }
                }
                _ => return None,
            }),
            "checks" => Some(match stat {
                "rate" => self.check_failure_rate(),
                "count" => self.checks.values().map(|(_, f)| *f).sum::<u64>() as f64,
                _ => return None,
            }),
            _ => None,
        }
    }

    pub fn evaluate_thresholds(&self) -> Vec<ThresholdResult> {
        self.thresholds
            .iter()
            .map(|t| {
                let actual = self.metric_value(&t.metric, &t.stat);
                let target = t.target();
                let passed = match actual {
                    None => false,
                    Some(a) => match t.op {
                        ThresholdOp::Lt => a < target,
                        ThresholdOp::Lte => a <= target,
                        ThresholdOp::Gt => a > target,
                        ThresholdOp::Gte => a >= target,
                    },
                };
                ThresholdResult {
                    description: match actual {
                        Some(_) => t.describe(),
                        None => format!("{} (unknown metric)", t.describe()),
                    },
                    metric: t.metric.clone(),
                    stat: t.stat.clone(),
                    target,
                    actual: actual.unwrap_or(f64::NAN),
                    passed,
                }
            })
            .collect()
    }

    /// True when a failing threshold is marked `abortOnFail`.
    pub fn should_abort(&self, results: &[ThresholdResult]) -> bool {
        self.thresholds
            .iter()
            .zip(results)
            .any(|(t, r)| t.abort_on_fail && !r.passed)
    }

    /// Build a snapshot and reset the per-interval counters.
    pub fn snapshot(&mut self, elapsed_sec: u64, interval_sec: f64) -> Snapshot {
        let overall = self.overall_stats();
        let rps = if interval_sec > 0.0 {
            self.interval_count as f64 / interval_sec
        } else {
            0.0
        };
        let bytes_per_sec = if interval_sec > 0.0 {
            self.interval_bytes as f64 / interval_sec
        } else {
            0.0
        };
        // Read before resetting: these describe the interval just ended.
        let interval_error_rate = if self.interval_count == 0 {
            0.0
        } else {
            self.interval_errors as f64 / self.interval_count as f64
        };
        let interval_p95 = ms(self.interval_hist.value_at_quantile(0.95));
        let interval_p99 = ms(self.interval_hist.value_at_quantile(0.99));

        self.interval_count = 0;
        self.interval_errors = 0;
        self.interval_hist.reset();
        let bytes_out_per_sec = if interval_sec > 0.0 {
            self.interval_bytes_out as f64 / interval_sec
        } else {
            0.0
        };
        self.interval_bytes = 0;
        self.interval_bytes_out = 0;
        if rps > self.peak_rps {
            self.peak_rps = rps;
        }

        let c = &self.counters;
        Snapshot {
            elapsed_sec,
            active_vus: c.active_vus.load(Ordering::Relaxed) as u32,
            target_vus: c.target_vus.load(Ordering::Relaxed) as f64,
            rps,
            error_rate: self.error_rate(),
            p50: overall.p50,
            p95: overall.p95,
            p99: overall.p99,
            per_tag: self.per_tag_stats(),
            checks: self.check_stats(),
            total_requests: self.overall.count,
            total_errors: self.overall.errors,
            samples_dropped: c.samples_dropped.load(Ordering::Relaxed),
            dropped_iterations: c.dropped_iterations.load(Ordering::Relaxed),
            vus_saturated: c.vus_saturated.load(Ordering::Relaxed) > 0,
            threshold_results: self.evaluate_thresholds(),
            bytes_in: self.overall.bytes_in,
            bytes_out: self.overall.bytes_out,
            bytes_per_sec,
            bytes_out_per_sec,
            interval_error_rate,
            interval_p95,
            interval_p99,
        }
    }
}

/// Round a step up to the nearest 1, 2 or 5 times a power of ten.
///
/// Axis labels made of round numbers can be read at a glance; labels like
/// "37.4 ms" cannot.
fn nice_step(raw: f64) -> f64 {
    if raw <= 0.0 {
        return 1.0;
    }
    let magnitude = 10f64.powf(raw.log10().floor());
    let normalised = raw / magnitude;
    let stepped = if normalised <= 1.0 {
        1.0
    } else if normalised <= 2.0 {
        2.0
    } else if normalised <= 5.0 {
        5.0
    } else {
        10.0
    };
    stepped * magnitude
}

/// Flatten an error and everything that caused it into one string.
///
/// Client libraries put the useful part — "connection refused", "operation
/// timed out", "dns error" — in the error's *source*, not its `Display`.
/// reqwest's top-level message is only `error sending request for url (…)`,
/// so classifying on `to_string()` alone sees nothing worth matching and
/// buckets every failure by URL.
pub fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(e) = source {
        out.push_str(": ");
        out.push_str(&e.to_string());
        source = e.source();
    }
    out
}

/// Classify an error by its whole cause chain.
///
/// The fallback bucket uses the innermost cause rather than the outermost:
/// the outer message carries the URL, which would give one bucket per target.
pub fn classify_err(err: &dyn std::error::Error) -> Arc<str> {
    let chain = error_chain(err);
    let matched = classify_error(&chain);
    // `classify_error` falls back to the text it was given; for a chain that
    // is the URL-bearing outer message, so substitute the innermost cause.
    if !is_known_kind(&matched) {
        let mut innermost: &dyn std::error::Error = err;
        while let Some(e) = innermost.source() {
            innermost = e;
        }
        return truncate_kind(&innermost.to_string());
    }
    matched
}

fn is_known_kind(kind: &str) -> bool {
    matches!(
        kind,
        "Connection refused"
            | "Timed out"
            | "DNS resolution failed"
            | "TLS error"
            | "Connection reset"
            | "Out of local sockets"
            | "Cancelled"
    )
}

fn truncate_kind(msg: &str) -> Arc<str> {
    let line = msg.lines().next().unwrap_or(msg).trim();
    let cut = line.char_indices().nth(120).map(|(i, _)| i);
    Arc::from(match cut {
        Some(i) => &line[..i],
        None => line,
    })
}

/// Reduce a transport error message to a short, stable kind.
///
/// Raw client errors embed the URL and often a port, so grouping on the whole
/// message would produce one bucket per target and bury the pattern. What is
/// worth knowing is the kind of failure, which is a small fixed set.
pub fn classify_error(msg: &str) -> Arc<str> {
    // Both spellings of every failure: the Unix phrasing and the Windows one.
    // They are worded nothing alike — Windows says "actively refused" where
    // Unix says "connection refused" — so a list of only one platform's
    // wording sends every failure on the other into the fallback bucket.
    let m = msg.to_ascii_lowercase();
    let kind = if m.contains("connection refused") || m.contains("actively refused") {
        "Connection refused"
    } else if m.contains("timed out")
        || m.contains("timeout")
        || m.contains("deadline")
        || m.contains("did not properly respond")
    {
        "Timed out"
    } else if m.contains("dns")
        || m.contains("resolve")
        || m.contains("name or service")
        || m.contains("nodename")
        || m.contains("lookup address")
        || m.contains("no such host")
    {
        "DNS resolution failed"
    } else if m.contains("certificate") || m.contains("tls") || m.contains("ssl") {
        "TLS error"
    } else if m.contains("connection reset")
        || m.contains("broken pipe")
        || m.contains("forcibly closed")
        || m.contains("connection aborted")
    {
        "Connection reset"
    } else if m.contains("too many open files")
        || m.contains("address in use")
        || m.contains("only one usage of each socket address")
        || m.contains("no buffer space")
    {
        "Out of local sockets"
    } else if m.contains("cancel") {
        "Cancelled"
    } else {
        // Keep the first line so an unrecognised failure is still legible,
        // but bounded so one long message cannot dominate a report.
        return truncate_kind(msg);
    };
    Arc::from(kind)
}

/// Create the sink and the receiving end of the metric channel.
pub fn channel(counters: Arc<RunCounters>) -> (MetricsSink, mpsc::Receiver<MetricMsg>) {
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    (MetricsSink { tx, counters }, rx)
}

pub type SnapshotTx = broadcast::Sender<Snapshot>;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(tag: &str, ok: bool, ms: u64) -> Sample {
        Sample {
            tag: Arc::from(tag),
            protocol: Protocol::Http,
            status: if ok { 200 } else { 500 },
            ok,
            latency_us: ms * 1000,
            bytes_in: 0,
            bytes_out: 0,
            error: None,
        }
    }

    fn failed_sample(tag: &str, msg: &str) -> Sample {
        Sample {
            tag: Arc::from(tag),
            protocol: Protocol::Http,
            status: 0,
            ok: false,
            latency_us: 1_000,
            bytes_in: 0,
            bytes_out: 0,
            error: Some(classify_error(msg)),
        }
    }

    fn agg() -> Aggregator {
        Aggregator::new(Vec::new(), Arc::new(RunCounters::default()))
    }

    #[test]
    fn zero_means_different_things_per_protocol() {
        let mut a = agg();
        // An HTTP request that never got a response, and a successful gRPC
        // call, both record status 0. They must not merge into one row.
        a.apply(MetricMsg::Sample(failed_sample("t", "connection refused")));
        a.apply(MetricMsg::Sample(Sample {
            tag: Arc::from("t"),
            protocol: Protocol::Grpc,
            status: 0,
            ok: true,
            latency_us: 1_000,
            bytes_in: 0,
            bytes_out: 0,
            error: None,
        }));

        let counts = a.status_code_counts();
        assert_eq!(counts.len(), 2, "{counts:?}");
        assert!(counts
            .iter()
            .any(|c| c.protocol == Protocol::Http && c.code == 0 && c.count == 1));
        assert!(counts
            .iter()
            .any(|c| c.protocol == Protocol::Grpc && c.code == 0 && c.count == 1));
    }

    #[test]
    fn failures_are_grouped_by_kind() {
        let mut a = agg();
        for _ in 0..3 {
            a.apply(MetricMsg::Sample(failed_sample(
                "t",
                "error sending request for url (http://a.test/1): connection refused",
            )));
        }
        // Same kind of failure, different URL — one bucket, not two.
        a.apply(MetricMsg::Sample(failed_sample(
            "t",
            "error sending request for url (http://b.test/9): Connection refused",
        )));
        a.apply(MetricMsg::Sample(failed_sample("t", "operation timed out")));

        let errors = a.error_counts();
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert_eq!(errors[0].message, "Connection refused");
        assert_eq!(errors[0].count, 4);
        assert_eq!(errors[1].message, "Timed out");
    }

    #[test]
    fn a_cause_chain_is_classified_by_its_innermost_cause() {
        #[derive(Debug)]
        struct Inner;
        impl std::fmt::Display for Inner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "connection refused (os error 111)")
            }
        }
        impl std::error::Error for Inner {}

        #[derive(Debug)]
        struct Outer(Inner);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "error sending request for url (http://a.test/1)")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        // The outer message says nothing useful and carries the URL; the cause
        // is where the real reason lives.
        assert_eq!(&*classify_err(&Outer(Inner)), "Connection refused");
    }

    #[test]
    fn an_unrecognised_failure_falls_back_to_its_innermost_cause() {
        #[derive(Debug)]
        struct Inner;
        impl std::fmt::Display for Inner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "something unusual happened")
            }
        }
        impl std::error::Error for Inner {}

        #[derive(Debug)]
        struct Outer(Inner);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "error sending request for url (http://a.test/1)")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        // Not the outer message: that would give one bucket per URL.
        assert_eq!(&*classify_err(&Outer(Inner)), "something unusual happened");
    }

    #[test]
    fn both_platforms_wordings_reach_the_same_bucket() {
        // Windows and Unix word these nothing alike; a report should not show
        // the same failure under two names depending on who ran it.
        for msg in [
            "connection refused (os error 111)",
            "No connection could be made because the target machine actively refused it. (os error 10061)",
        ] {
            assert_eq!(&*classify_error(msg), "Connection refused", "{msg}");
        }
        for msg in [
            "connection reset by peer",
            "An existing connection was forcibly closed by the remote host. (os error 10054)",
        ] {
            assert_eq!(&*classify_error(msg), "Connection reset", "{msg}");
        }
        for msg in [
            "failed to lookup address information: nodename nor servname provided",
            "No such host is known. (os error 11001)",
        ] {
            assert_eq!(&*classify_error(msg), "DNS resolution failed", "{msg}");
        }
        for msg in [
            "too many open files (os error 24)",
            "Only one usage of each socket address is normally permitted. (os error 10048)",
        ] {
            assert_eq!(&*classify_error(msg), "Out of local sockets", "{msg}");
        }
    }

    #[test]
    fn error_kinds_are_bounded() {
        let mut a = agg();
        // A server emitting a unique message per request must not be able to
        // grow the breakdown without limit.
        for i in 0..MAX_ERROR_KINDS * 3 {
            a.apply(MetricMsg::Sample(failed_sample("t", &format!("weird {i}"))));
        }
        let errors = a.error_counts();
        assert!(errors.len() <= MAX_ERROR_KINDS + 1, "{}", errors.len());
        let total: u64 = errors.iter().map(|e| e.count).sum();
        assert_eq!(total as usize, MAX_ERROR_KINDS * 3, "no failure was lost");
    }

    #[test]
    fn min_and_the_far_tail_are_reported() {
        let mut a = agg();
        // 995 fast and 5 slow: the slow ones sit inside the top 0.1% but
        // outside the top 1%, so only p99.9 should see them.
        for _ in 0..995 {
            a.apply(MetricMsg::Sample(sample("t", true, 10)));
        }
        for _ in 0..5 {
            a.apply(MetricMsg::Sample(sample("t", true, 5_000)));
        }

        let s = a.overall_stats();
        assert!((s.min - 10.0).abs() < 1.0, "min was {}", s.min);
        assert!(s.p99 < 100.0, "p99 was {}", s.p99);
        assert!(s.p999 > 1_000.0, "p99.9 was {}", s.p999);
    }

    #[test]
    fn an_empty_run_reports_a_zero_minimum() {
        // An untouched histogram reports u64::MAX as its minimum, which would
        // render as an absurd latency.
        assert_eq!(agg().overall_stats().min, 0.0);
    }

    #[test]
    fn throughput_and_peak_rate_are_tracked() {
        let mut a = agg();
        for _ in 0..10 {
            a.apply(MetricMsg::Sample(Sample {
                tag: Arc::from("t"),
                protocol: Protocol::Http,
                status: 200,
                ok: true,
                latency_us: 1_000,
                bytes_in: 100,
                bytes_out: 0,
                error: None,
            }));
        }
        let first = a.snapshot(1, 1.0);
        assert_eq!(first.bytes_in, 1_000);
        assert!((first.bytes_per_sec - 1_000.0).abs() < 1.0);
        assert!((first.rps - 10.0).abs() < 0.1);

        // A quiet second must not lower the peak already reached.
        let second = a.snapshot(2, 1.0);
        assert_eq!(second.rps, 0.0);
        assert_eq!(second.bytes_in, 1_000, "cumulative, not per interval");
        assert!((a.peak_rps() - 10.0).abs() < 0.1);
    }

    #[test]
    fn a_bimodal_run_shows_two_humps() {
        let mut a = agg();
        for _ in 0..1_000 {
            a.apply(MetricMsg::Sample(sample("t", true, 10)));
        }
        for _ in 0..1_000 {
            a.apply(MetricMsg::Sample(sample("t", true, 200)));
        }

        let dist = a.latency_distribution(40);
        let total: u64 = dist.iter().map(|b| b.count).sum();
        assert_eq!(total, 2_000, "every request must land in a bucket");

        // Two separate concentrations of mass, not one smear: this is exactly
        // what percentiles cannot show.
        let occupied: Vec<&DistBucket> = dist.iter().filter(|b| b.count > 0).collect();
        assert_eq!(occupied.len(), 2, "{occupied:?}");
        assert!(occupied[0].upper_ms < 50.0, "{:?}", occupied[0]);
        assert!(occupied[1].upper_ms > 150.0, "{:?}", occupied[1]);
        assert_eq!(occupied[0].count, 1_000);
        assert_eq!(occupied[1].count, 1_000);
    }

    #[test]
    fn an_empty_run_has_no_distribution() {
        assert!(agg().latency_distribution(40).is_empty());
    }

    #[test]
    fn bucket_steps_are_round_numbers() {
        // An axis labelled in round numbers can be read at a glance.
        assert_eq!(nice_step(0.9), 1.0);
        assert_eq!(nice_step(1.5), 2.0);
        assert_eq!(nice_step(3.0), 5.0);
        assert_eq!(nice_step(7.0), 10.0);
        assert_eq!(nice_step(11.0), 20.0);
        assert_eq!(nice_step(230.0), 500.0);
        // Never zero, or bucketing would not terminate.
        assert!(nice_step(0.0) > 0.0);
        assert!(nice_step(-5.0) > 0.0);
    }

    #[test]
    fn percentiles_and_counts() {
        let mut a = agg();
        for i in 1..=100 {
            a.apply(MetricMsg::Sample(sample("t", true, i)));
        }
        let s = a.overall_stats();
        assert_eq!(s.count, 100);
        assert!((s.p50 - 50.0).abs() < 2.0, "p50 was {}", s.p50);
        assert!((s.p95 - 95.0).abs() < 2.0, "p95 was {}", s.p95);
        assert!((s.avg - 50.5).abs() < 1.0, "avg was {}", s.avg);
    }

    #[test]
    fn error_rate_tracked() {
        let mut a = agg();
        for _ in 0..9 {
            a.apply(MetricMsg::Sample(sample("t", true, 1)));
        }
        a.apply(MetricMsg::Sample(sample("t", false, 1)));
        assert!((a.error_rate() - 0.1).abs() < 1e-9);
    }

    #[test]
    fn tags_aggregate_separately_and_sort_by_count() {
        let mut a = agg();
        a.apply(MetricMsg::Sample(sample("rare", true, 1)));
        for _ in 0..5 {
            a.apply(MetricMsg::Sample(sample("common", true, 1)));
        }
        let tags = a.per_tag_stats();
        assert_eq!(tags[0].tag, "common");
        assert_eq!(tags[0].count, 5);
        assert_eq!(tags[1].count, 1);
    }

    #[test]
    fn checks_accumulate() {
        let mut a = agg();
        a.apply(MetricMsg::Checks(vec![("ok".into(), 2, 1)]));
        a.apply(MetricMsg::Checks(vec![("ok".into(), 3, 0)]));
        let c = a.check_stats();
        assert_eq!(c[0].passes, 5);
        assert_eq!(c[0].fails, 1);
    }

    #[test]
    fn threshold_pass_and_fail() {
        let thresholds = vec![
            Threshold {
                metric: "http_req_duration".into(),
                stat: "p95".into(),
                op: ThresholdOp::Lt,
                value_ms: Some(500.0),
                value: None,
                abort_on_fail: false,
            },
            Threshold {
                metric: "http_req_failed".into(),
                stat: "rate".into(),
                op: ThresholdOp::Lt,
                value_ms: None,
                value: Some(0.01),
                abort_on_fail: false,
            },
        ];
        let mut a = Aggregator::new(thresholds, Arc::new(RunCounters::default()));
        for _ in 0..99 {
            a.apply(MetricMsg::Sample(sample("t", true, 10)));
        }
        a.apply(MetricMsg::Sample(sample("t", false, 10)));

        let r = a.evaluate_thresholds();
        assert!(r[0].passed, "p95 {} should be < 500", r[0].actual);
        assert!(
            !r[1].passed,
            "error rate {} should fail < 0.01",
            r[1].actual
        );
    }

    #[test]
    fn unknown_metric_fails_visibly() {
        let mut a = Aggregator::new(
            vec![Threshold {
                metric: "made_up".into(),
                stat: "p95".into(),
                op: ThresholdOp::Lt,
                value_ms: Some(1.0),
                value: None,
                abort_on_fail: false,
            }],
            Arc::new(RunCounters::default()),
        );
        a.apply(MetricMsg::Sample(sample("t", true, 1)));
        let r = a.evaluate_thresholds();
        assert!(!r[0].passed);
        assert!(r[0].description.contains("unknown metric"));
    }

    #[test]
    fn http_reqs_rate_is_requests_per_second() {
        // Documented and offered by the editor, but it used to read as an
        // unknown metric and fail every run it was set on.
        let mut a = Aggregator::new(
            vec![Threshold {
                metric: "http_reqs".into(),
                stat: "rate".into(),
                op: ThresholdOp::Gt,
                value_ms: None,
                value: Some(5.0),
                abort_on_fail: false,
            }],
            Arc::new(RunCounters::default()),
        );
        a.started = Instant::now() - std::time::Duration::from_secs(10);
        for _ in 0..100 {
            a.apply(MetricMsg::Sample(sample("t", true, 1)));
        }
        let r = a.evaluate_thresholds();
        assert!(!r[0].description.contains("unknown metric"));
        assert!((r[0].actual - 10.0).abs() < 0.5, "rate {}", r[0].actual);
        assert!(r[0].passed);
    }

    #[test]
    fn snapshot_resets_the_interval_rate() {
        let mut a = agg();
        for _ in 0..10 {
            a.apply(MetricMsg::Sample(sample("t", true, 1)));
        }
        let s1 = a.snapshot(1, 1.0);
        assert_eq!(s1.rps, 10.0);
        let s2 = a.snapshot(2, 1.0);
        assert_eq!(s2.rps, 0.0);
        assert_eq!(s2.total_requests, 10);
    }

    #[tokio::test]
    async fn full_channel_drops_rather_than_blocks() {
        let counters = Arc::new(RunCounters::default());
        let (tx, rx) = mpsc::channel(2);
        let sink = MetricsSink {
            tx,
            counters: counters.clone(),
        };
        for _ in 0..10 {
            sink.sample(sample("t", true, 1));
        }
        assert_eq!(counters.samples_dropped.load(Ordering::Relaxed), 8);
        drop(rx);
    }

    #[tokio::test]
    async fn round_markers_survive_a_saturated_channel() {
        // A round ends the instant a burst of samples has been queued, which
        // is when the channel is fullest. Samples may be dropped under that
        // pressure — that is counted — but the round marker must not be, or
        // the round's stats disappear without a trace.
        let counters = Arc::new(RunCounters::default());
        let (tx, mut rx) = mpsc::channel(4);
        let sink = MetricsSink {
            tx,
            counters: counters.clone(),
        };

        sink.round_started(1, 100, 8).await;
        // Fill the channel past capacity: everything after the first few is
        // dropped by design, and the channel is now full.
        for _ in 0..50 {
            sink.sample(sample("t", true, 1));
        }
        assert!(counters.samples_dropped.load(Ordering::Relaxed) > 0);

        // Drain on the side so the awaited end marker can get through.
        let mut agg = Aggregator::new(vec![], counters.clone());
        let drain = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                agg.apply(msg);
            }
            agg
        });
        sink.round_ended(0.5, 2).await;
        drop(sink);
        let agg = drain.await.unwrap();

        let rounds = agg.rounds();
        assert_eq!(
            rounds.len(),
            1,
            "the round must be recorded despite the full channel"
        );
        assert_eq!(rounds[0].index, 1);
        assert_eq!(rounds[0].wall_sec, 0.5);
        // It closed over exactly the samples that made it through.
        let delivered = 50 - counters.samples_dropped.load(Ordering::Relaxed);
        assert_eq!(rounds[0].stats.count, delivered);
    }

    #[test]
    fn a_snapshot_reports_the_bytes_sent_so_far() {
        let mut a = Aggregator::new(vec![], Arc::new(RunCounters::default()));
        let mut s = sample("t", true, 1);
        s.bytes_out = 1234;
        a.apply(MetricMsg::Sample(s));
        let snap = a.snapshot(1, 1.0);
        assert_eq!(snap.bytes_out, 1234, "was hardcoded to zero");
    }

    #[test]
    fn abort_on_fail_only_for_marked_thresholds() {
        let mut a = Aggregator::new(
            vec![Threshold {
                metric: "http_req_failed".into(),
                stat: "rate".into(),
                op: ThresholdOp::Lt,
                value_ms: None,
                value: Some(0.0),
                abort_on_fail: true,
            }],
            Arc::new(RunCounters::default()),
        );
        a.apply(MetricMsg::Sample(sample("t", false, 1)));
        let r = a.evaluate_thresholds();
        assert!(a.should_abort(&r));
    }
}
