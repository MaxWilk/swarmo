//! The load engine.
//!
//! Two execution engines share one scheduler and one metrics pipeline:
//!
//!   Engine A (native)   — declarative scenarios, pure Rust, no JS in the hot path.
//!   Engine B (scripted) — `*.user.js`, one QuickJS context per virtual user.
//!
//! Coordinated omission: in the open model, latency is measured from the time an
//! iteration was *scheduled* to start, not from when a worker actually picked it
//! up, so queueing delay is visible in the percentiles rather than hidden.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::Rng;
use swarmo_core::model::*;
use swarmo_core::store::Protocol;
use swarmo_core::{finalize, now_millis, ResolvedRequest, VarScope};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

/// Buckets in the exported latency distribution. Enough to show shape, few
/// enough to keep the summary small.
const DISTRIBUTION_BUCKETS: usize = 40;

use crate::auth::AuthTokenState;
use crate::capture::capture_from_body;
use crate::metrics::{self, Aggregator, MetricMsg, MetricsSink, RunCounters, Sample};
use crate::plan::{LoadPlan, PlanKind, PreparedAction, PreparedStep};

const SCHEDULER_TICK: Duration = Duration::from_millis(100);
/// How often the open-model scheduler wakes to issue arrivals.
///
/// This bounds the worst-case burst: every arrival due within one tick is
/// released together, so at 20,000/s a 10ms tick meant slugs of 200 requests
/// at once — and the generator then measured its own burst-queueing as server
/// latency. 2ms keeps bursts small without per-request timers, whose wake-up
/// jitter on Windows costs more than it saves.
const TICKET_TICK: Duration = Duration::from_millis(2);
/// Iterations queued longer than this are dropped rather than run late.
const MAX_QUEUE_AGE: Duration = Duration::from_secs(10);

pub struct RunControl {
    pub run_id: String,
    pub cancel: CancellationToken,
    pub snapshots: broadcast::Sender<Snapshot>,
}

pub struct RunOutput {
    pub summary: RunSummary,
    pub timeline: Vec<Snapshot>,
}

/// Execute a plan to completion. Returns once every worker has stopped.
/// Hold Windows' timer resolution at 1ms for the duration of a run.
///
/// Windows wakes sleepers on a ~15.6ms heartbeat by default, which is far too
/// coarse for a scheduler that spaces arrivals microseconds apart: every wait
/// picks up 0–15ms of jitter, and that jitter lands in the latency figures.
/// Raising the resolution is what every soft-real-time Windows program does;
/// scoping it to the run puts the battery-friendly default back afterwards.
struct TimerResolution;

impl TimerResolution {
    fn hold() -> Self {
        #[cfg(windows)]
        unsafe {
            timeBeginPeriod(1);
        }
        TimerResolution
    }
}

impl Drop for TimerResolution {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            timeEndPeriod(1);
        }
    }
}

#[cfg(windows)]
#[link(name = "winmm")]
extern "system" {
    fn timeBeginPeriod(period: u32) -> u32;
    fn timeEndPeriod(period: u32) -> u32;
}

pub async fn run(plan: LoadPlan, ctrl: RunControl) -> RunOutput {
    let _timer = TimerResolution::hold();
    let started_at = now_millis();
    let start = Instant::now();

    let counters = Arc::new(RunCounters::default());
    let (sink, rx) = metrics::channel(counters.clone());

    // The aggregator owns all metric state and emits snapshots at 1 Hz.
    let agg_cancel = ctrl.cancel.clone();
    let stop_reason: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let abort_reason: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let agg_handle = tokio::spawn(aggregator_task(
        rx,
        plan.thresholds.clone(),
        counters.clone(),
        ctrl.snapshots.clone(),
        agg_cancel,
        start,
        plan.stop_when.clone(),
        stop_reason.clone(),
        abort_reason.clone(),
    ));

    let client = match build_client(&plan) {
        Ok(c) => c,
        Err(e) => {
            drop(sink);
            let (mut agg, timeline) = agg_handle
                .await
                .unwrap_or_else(|_| (Aggregator::new(Vec::new(), counters.clone()), Vec::new()));
            return finish(
                &plan,
                &ctrl,
                &mut agg,
                timeline,
                started_at,
                start,
                RunState::Errored,
                Some(format!("could not create the HTTP client: {e}")),
                counters,
                // The run never started, so there is nothing to stop early.
                None,
            );
        }
    };

    // One pool of gRPC channels for the whole run. A single HTTP/2 connection
    // shares one flow-control window, so a wide pool matters under load.
    let grpc_channels = Arc::new(swarmo_grpc::ChannelPool::new(plan.grpc_channels()));

    let workers_cancel = ctrl.cancel.clone();
    let exec_error = match &plan.kind {
        PlanKind::Native { steps } => {
            let steps = Arc::new(steps.clone());
            run_native(
                &plan,
                steps,
                client,
                sink.clone(),
                workers_cancel,
                grpc_channels.clone(),
            )
            .await;
            None
        }
        PlanKind::Scripted {
            source,
            user_mix,
            grpc,
        } => run_scripted(
            &plan,
            source.clone(),
            user_mix.clone(),
            scripted::ClientConfig {
                verify_tls: plan.verify_tls,
                timeout_ms: plan.timeout_ms,
                new_connection_per_iteration: plan.new_connection_per_iteration,
            },
            sink.clone(),
            workers_cancel,
            grpc.as_ref().map(|g| scripted::GrpcContext {
                descriptors: g.descriptors.clone(),
                channels: grpc_channels.clone(),
                source: g.source.clone(),
                verify_tls: g.verify_tls,
                timeout_ms: g.timeout_ms,
                max_response_bytes: g.max_response_bytes,
            }),
        )
        .await
        .err(),
    };

    // Dropping the sink closes the channel, which ends the aggregator.
    drop(sink);
    let (mut agg, timeline) = agg_handle
        .await
        .unwrap_or_else(|_| (Aggregator::new(Vec::new(), counters.clone()), Vec::new()));

    let stopped_because = stop_reason
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let aborted = abort_reason
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();

    let state = if exec_error.is_some() {
        RunState::Errored
    } else if aborted.is_some() {
        // The cancel token fired, but it was the run's own threshold that
        // pulled it — a failure, not a stop.
        RunState::Failed
    } else if stopped_because.is_some() {
        // Ending on a stop condition is the run doing its job, so its verdict
        // still comes from the thresholds rather than being "stopped".
        RunState::Running
    } else if ctrl.cancel.is_cancelled() {
        RunState::Stopped
    } else {
        RunState::Running // refined in finish() by threshold results
    };

    finish(
        &plan,
        &ctrl,
        &mut agg,
        timeline,
        started_at,
        start,
        state,
        exec_error,
        counters,
        stopped_because,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish(
    plan: &LoadPlan,
    ctrl: &RunControl,
    agg: &mut Aggregator,
    timeline: Vec<Snapshot>,
    started_at: u64,
    start: Instant,
    state: RunState,
    error: Option<String>,
    counters: Arc<RunCounters>,
    stopped_because: Option<String>,
) -> RunOutput {
    let thresholds = agg.evaluate_thresholds();
    let all_passed = thresholds.iter().all(|t| t.passed);

    let state = match state {
        RunState::Running => {
            if all_passed {
                RunState::Passed
            } else {
                RunState::Failed
            }
        }
        other => other,
    };

    let elapsed = start.elapsed().as_secs_f64();
    let total = agg.total_requests();

    let summary = RunSummary {
        version: swarmo_core::model::FORMAT_VERSION,
        run_id: ctrl.run_id.clone(),
        scenario_name: plan.name.clone(),
        scenario_ref: plan.scenario_ref.clone(),
        scenario_id: plan.scenario_id.clone(),
        started_at,
        ended_at: now_millis(),
        duration_sec: elapsed,
        state,
        error,
        total_requests: total,
        total_errors: agg.overall_stats().errors,
        error_rate: agg.error_rate(),
        rps: if elapsed > 0.0 {
            total as f64 / elapsed
        } else {
            0.0
        },
        overall: agg.overall_stats(),
        per_tag: agg.per_tag_stats(),
        checks: agg.check_stats(),
        thresholds,
        samples_dropped: counters.samples_dropped.load(Ordering::Relaxed),
        dropped_iterations: counters.dropped_iterations.load(Ordering::Relaxed),
        status_codes: agg.status_code_counts(),
        errors_by_message: agg.error_counts(),
        bytes_in: agg.bytes_in(),
        bytes_per_sec: if elapsed > 0.0 {
            agg.bytes_in() as f64 / elapsed
        } else {
            0.0
        },
        bytes_out: agg.bytes_out(),
        bytes_out_per_sec: if elapsed > 0.0 {
            agg.bytes_out() as f64 / elapsed
        } else {
            0.0
        },
        peak_rps: agg.peak_rps(),
        latency_distribution: agg.latency_distribution(DISTRIBUTION_BUCKETS),
        token_refreshes: plan.token_refreshes(),
        rounds: agg.rounds(),
        stopped_because,
    };

    RunOutput { summary, timeline }
}

fn build_client(plan: &LoadPlan) -> Result<reqwest::Client, reqwest::Error> {
    let mut b = reqwest::Client::builder()
        .danger_accept_invalid_certs(!plan.verify_tls)
        .tcp_nodelay(true)
        .pool_idle_timeout(Duration::from_secs(90))
        .user_agent(concat!("Swarmo/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_millis(plan.timeout_ms.max(1)));

    if plan.new_connection_per_iteration {
        b = b.pool_max_idle_per_host(0);
    }
    b.build()
}

// ---------------------------------------------------------------------------
// Aggregator task
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn aggregator_task(
    mut rx: mpsc::Receiver<MetricMsg>,
    thresholds: Vec<Threshold>,
    counters: Arc<RunCounters>,
    snapshots: broadcast::Sender<Snapshot>,
    cancel: CancellationToken,
    start: Instant,
    stop_when: Option<StopCondition>,
    // Set when a stop condition ends the run, so the summary can say why.
    stop_reason: Arc<Mutex<Option<String>>>,
    // Set when an abortOnFail threshold ends it, so that reads as a failure
    // rather than as the user pressing Stop.
    abort_reason: Arc<Mutex<Option<String>>>,
) -> (Aggregator, Vec<Snapshot>) {
    let mut agg = Aggregator::new(thresholds, counters);
    let mut timeline: Vec<Snapshot> = Vec::new();
    // Consecutive intervals the condition has held for.
    let mut breaches: u32 = 0;
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await; // the first tick completes immediately

    let mut last_tick = Instant::now();

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Some(m) => agg.apply(m),
                    // All senders dropped: the run is over.
                    None => break,
                }
            }
            _ = ticker.tick() => {
                let interval = last_tick.elapsed().as_secs_f64();
                last_tick = Instant::now();
                let snap = agg.snapshot(start.elapsed().as_secs(), interval);

                if agg.should_abort(&snap.threshold_results) {
                    let failed: Vec<&str> = snap
                        .threshold_results
                        .iter()
                        .filter(|r| !r.passed)
                        .map(|r| r.description.as_str())
                        .collect();
                    let reason = format!(
                        "aborted: a threshold marked abortOnFail failed ({})",
                        failed.join("; ")
                    );
                    tracing::warn!("{reason}");
                    *abort_reason.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
                    cancel.cancel();
                }

                // A stop condition ends the run because it found what it was
                // looking for, so it is recorded apart from a threshold
                // failure or the user cancelling.
                if let Some(condition) = &stop_when {
                    if condition.breached_by(&snap) {
                        breaches += 1;
                        if breaches >= condition.for_intervals.max(1) {
                            let value = condition.value_in(&snap).unwrap_or_default();
                            let reason = condition.describe(value);
                            tracing::info!("{reason}");
                            *stop_reason.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
                            cancel.cancel();
                        }
                    } else {
                        // Consecutive intervals only: one bad second in an
                        // otherwise healthy run is noise, not a breaking point.
                        breaches = 0;
                    }
                }

                let _ = snapshots.send(snap.clone());
                timeline.push(snap);
            }
        }
    }

    // A final snapshot so the timeline ends with complete numbers.
    let interval = last_tick.elapsed().as_secs_f64().max(0.001);
    let snap = agg.snapshot(start.elapsed().as_secs(), interval);
    let _ = snapshots.send(snap.clone());
    timeline.push(snap);

    (agg, timeline)
}

// ---------------------------------------------------------------------------
// Engine A: native
// ---------------------------------------------------------------------------

async fn run_native(
    plan: &LoadPlan,
    steps: Arc<Vec<PreparedStep>>,
    client: reqwest::Client,
    sink: MetricsSink,
    cancel: CancellationToken,
    grpc_channels: Arc<swarmo_grpc::ChannelPool>,
) {
    if !plan.rounds.is_empty() {
        // A fixed budget of iterations beats both duration modes when the
        // question is "how long does this batch take", so it wins over mode.
        return run_fixed_native(plan, steps, client, sink, cancel, grpc_channels).await;
    }
    match plan.mode {
        LoadMode::Closed => {
            run_closed_native(plan, steps, client, sink, cancel, grpc_channels).await
        }
        LoadMode::Open => run_open_native(plan, steps, client, sink, cancel, grpc_channels).await,
    }
}

/// Run each round in turn: N iterations at concurrency C, then the gap.
///
/// The batch-benchmark shape: every worker pulls the next iteration off one
/// shared counter until the round's budget is spent, so the wall time answers
/// "how fast can this service chew through N requests at concurrency C" —
/// which neither a duration nor a rate can ask directly. Repeating that with a
/// pause between is how the answer stops being a single number: whether the
/// second blast is quicker because a cache is warm, or slower because
/// something did not recover, only shows up when you run it twice.
async fn run_fixed_native(
    plan: &LoadPlan,
    steps: Arc<Vec<PreparedStep>>,
    client: reqwest::Client,
    sink: MetricsSink,
    cancel: CancellationToken,
    grpc_channels: Arc<swarmo_grpc::ChannelPool>,
) {
    let base_vars = Arc::new(plan.base_vars.clone());

    for (i, round) in plan.rounds.iter().enumerate() {
        if cancel.is_cancelled() {
            break;
        }

        // The gap belongs *between* rounds, so it is taken before every round
        // but the first. Waiting after the last one would pad the run's wall
        // time with idle seconds and quietly depress its overall rate.
        if i > 0 && round_gap_before(plan, i) > 0 {
            let gap = Duration::from_secs(round_gap_before(plan, i));
            tokio::select! {
                _ = tokio::time::sleep(gap) => {}
                _ = cancel.cancelled() => break,
            }
        }

        // Timed around the round itself, never around its pause: the whole
        // point of the number is "how long did this burst take", and folding
        // an idle gap into it would make every round but the last look slower
        // than it was.
        sink.round_started(i as u32 + 1, round.iterations, round.concurrency)
            .await;
        let started = Instant::now();
        run_one_round(
            round,
            &steps,
            &client,
            &sink,
            &cancel,
            &base_vars,
            &grpc_channels,
        )
        .await;
        sink.round_ended(started.elapsed().as_secs_f64(), round.gap_sec)
            .await;
    }
    sink.counters().active_vus.store(0, Ordering::Relaxed);
    sink.counters().target_vus.store(0, Ordering::Relaxed);
}

/// The pause owed before round `i`, which is the gap the round before it set.
fn round_gap_before(plan: &LoadPlan, i: usize) -> u64 {
    plan.rounds.get(i - 1).map(|r| r.gap_sec).unwrap_or(0)
}

/// One round: claim from a shared counter until its budget is spent.
#[allow(clippy::too_many_arguments)]
async fn run_one_round(
    round: &Blast,
    steps: &Arc<Vec<PreparedStep>>,
    client: &reqwest::Client,
    sink: &MetricsSink,
    cancel: &CancellationToken,
    base_vars: &Arc<HashMap<String, String>>,
    grpc_channels: &Arc<swarmo_grpc::ChannelPool>,
) {
    let budget = round.iterations;
    // Concurrency alone decides the worker count here — maxVus belongs to the
    // duration modes and is not even shown for a fixed run, so letting it cap
    // this silently would be a trap. It is already clamped to the hard cap.
    let workers_n = round.concurrency.max(1);
    let taken = Arc::new(AtomicU64::new(0));
    // Fired once the last iteration is claimed: no worker will start another,
    // so a pause after its final step would only pad the round's wall time.
    let ending = CancellationToken::new();

    sink.counters()
        .target_vus
        .store(workers_n as u64, Ordering::Relaxed);

    let mut workers = Vec::new();
    for slot in 0..workers_n {
        let vu = NativeVu {
            steps: steps.clone(),
            client: client.clone(),
            sink: sink.clone(),
            cancel: cancel.clone(),
            ending: ending.clone(),
            base_vars: base_vars.clone(),
            grpc_channels: grpc_channels.clone(),
            vu_id: slot as usize,
        };
        let taken = taken.clone();
        let counters = sink.counters().clone();
        workers.push(tokio::spawn(async move {
            counters.active_vus.fetch_add(1, Ordering::Relaxed);
            loop {
                if vu.cancel.is_cancelled() {
                    break;
                }
                // Claim before running, so the budget is exact: N claims
                // means N iterations, no matter how workers interleave.
                let claimed = taken.fetch_add(1, Ordering::Relaxed);
                if claimed >= budget {
                    break;
                }
                if claimed + 1 == budget {
                    vu.ending.cancel();
                }
                vu.run_iteration(None).await;
            }
            counters.active_vus.fetch_sub(1, Ordering::Relaxed);
        }));
    }

    for w in workers {
        let _ = w.await;
    }
    sink.counters().active_vus.store(0, Ordering::Relaxed);
}

async fn run_closed_native(
    plan: &LoadPlan,
    steps: Arc<Vec<PreparedStep>>,
    client: reqwest::Client,
    sink: MetricsSink,
    cancel: CancellationToken,
    grpc_channels: Arc<swarmo_grpc::ChannelPool>,
) {
    let active = Arc::new(AtomicU64::new(0));
    let target = Arc::new(AtomicU64::new(0));
    let base_vars = Arc::new(plan.base_vars.clone());
    let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let ending = CancellationToken::new();

    let start = Instant::now();
    let total = plan.total_duration();
    let mut ticker = tokio::time::interval(SCHEDULER_TICK);

    loop {
        if cancel.is_cancelled() {
            break;
        }
        let elapsed = start.elapsed();
        if elapsed >= total {
            break;
        }

        let want = plan.target_at(elapsed).round().max(0.0) as u64;
        let want = want.min(plan.max_vus as u64);
        target.store(want, Ordering::Relaxed);
        sink.counters().target_vus.store(want, Ordering::Relaxed);

        let have = active.load(Ordering::Relaxed);
        for slot in have..want {
            active.fetch_add(1, Ordering::Relaxed);
            let vu = NativeVu {
                steps: steps.clone(),
                client: client.clone(),
                sink: sink.clone(),
                cancel: cancel.clone(),
                ending: ending.clone(),
                base_vars: base_vars.clone(),
                grpc_channels: grpc_channels.clone(),
                vu_id: slot as usize,
            };
            let active_c = active.clone();
            let target_c = target.clone();
            let counters = sink.counters().clone();
            handles.push(tokio::spawn(async move {
                loop {
                    if vu.cancel.is_cancelled() {
                        break;
                    }
                    // Exit only if we are genuinely above target (CAS avoids
                    // several VUs all exiting on the same overshoot).
                    let cur = active_c.load(Ordering::Relaxed);
                    let tgt = target_c.load(Ordering::Relaxed);
                    if cur > tgt
                        && active_c
                            .compare_exchange(cur, cur - 1, Ordering::SeqCst, Ordering::Relaxed)
                            .is_ok()
                    {
                        counters.active_vus.store(cur - 1, Ordering::Relaxed);
                        return;
                    }
                    vu.run_iteration(None).await;
                }
                active_c.fetch_sub(1, Ordering::Relaxed);
            }));
        }
        sink.counters()
            .active_vus
            .store(active.load(Ordering::Relaxed), Ordering::Relaxed);

        tokio::select! {
            _ = ticker.tick() => {}
            _ = cancel.cancelled() => break,
        }
    }

    // Ramp to zero and let in-flight iterations finish.
    ending.cancel();
    target.store(0, Ordering::Relaxed);
    sink.counters().target_vus.store(0, Ordering::Relaxed);
    for h in handles {
        let _ = h.await;
    }
    sink.counters().active_vus.store(0, Ordering::Relaxed);
}

async fn run_open_native(
    plan: &LoadPlan,
    steps: Arc<Vec<PreparedStep>>,
    client: reqwest::Client,
    sink: MetricsSink,
    cancel: CancellationToken,
    grpc_channels: Arc<swarmo_grpc::ChannelPool>,
) {
    // Tickets carry the instant the iteration was *supposed* to begin.
    let (tx, rx) = mpsc::channel::<Instant>(plan.max_vus as usize * 4 + 64);
    let rx = Arc::new(tokio::sync::Mutex::new(rx));
    let base_vars = Arc::new(plan.base_vars.clone());
    let ending = CancellationToken::new();

    let mut workers = Vec::new();
    for slot in 0..plan.max_vus {
        let vu = NativeVu {
            steps: steps.clone(),
            client: client.clone(),
            sink: sink.clone(),
            cancel: cancel.clone(),
            ending: ending.clone(),
            base_vars: base_vars.clone(),
            grpc_channels: grpc_channels.clone(),
            vu_id: slot as usize,
        };
        let rx = rx.clone();
        let counters = sink.counters().clone();
        let sink_c = sink.clone();
        workers.push(tokio::spawn(async move {
            loop {
                let ticket = {
                    let mut guard = rx.lock().await;
                    tokio::select! {
                        t = guard.recv() => t,
                        _ = vu.cancel.cancelled() => None,
                    }
                };
                let Some(scheduled) = ticket else { break };

                // Deliberately no sleep-until-scheduled here: a per-request
                // timer costs several milliseconds of wake-up jitter on
                // Windows, which was measured to cost more latency than the
                // burstiness it removed. The small tick bounds the burst
                // instead, and the spread timestamps keep the accounting
                // honest: a request that starts late is measured from when it
                // *should* have started.
                let queued = scheduled.elapsed();
                if queued > MAX_QUEUE_AGE {
                    sink_c.dropped_iteration();
                    continue;
                }
                counters.active_vus.fetch_add(1, Ordering::Relaxed);
                vu.run_iteration(Some(queued)).await;
                counters.active_vus.fetch_sub(1, Ordering::Relaxed);
            }
        }));
    }

    // The scheduler issues tickets one tick ahead, each carrying the exact
    // instant its iteration should begin. Arrivals are spaced 1/rate apart —
    // the shape an open model promises — rather than dumped in a burst at
    // every tick, which both distorts what the server experiences and pollutes
    // the latency figures with the generator's own queueing.
    let start = Instant::now();
    let total = plan.total_duration();
    let mut ticker = tokio::time::interval(TICKET_TICK);
    // Fractional arrivals carried between ticks. Integrating the rate this
    // way keeps totals exact while the rate changes — computing a fixed gap
    // from the instantaneous rate does not: at the foot of a ramp, 1/rate is
    // minutes, and the "next" arrival lands beyond the end of the run.
    let mut credit = 0.0f64;

    loop {
        if cancel.is_cancelled() {
            break;
        }
        let now = Instant::now();
        // Compared as elapsed time: `start + total` panics when a scaled
        // repeat block has saturated the run length.
        if now - start >= total {
            break;
        }

        let rate = plan.target_at(now - start).max(0.0);
        sink.counters()
            .target_vus
            .store(rate.round() as u64, Ordering::Relaxed);
        credit += rate * TICKET_TICK.as_secs_f64();

        let due = credit.floor() as u32;
        if due > 0 {
            credit -= f64::from(due);
            // Stamps are spread evenly across the tick rather than all set to
            // "now": the stamp is the moment the iteration was *supposed* to
            // begin, and that is what latency is measured from.
            let slice = TICKET_TICK / due;
            let mut scheduled = now;
            for _ in 0..due {
                match tx.try_send(scheduled) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        // Every worker is busy: record saturation, drop the arrival.
                        sink.counters().vus_saturated.store(1, Ordering::Relaxed);
                        sink.dropped_iteration();
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                }
                scheduled += slice;
            }
        }

        tokio::select! {
            _ = ticker.tick() => {}
            _ = cancel.cancelled() => break,
        }
    }

    ending.cancel();
    drop(tx);
    for w in workers {
        let _ = w.await;
    }
    sink.counters().active_vus.store(0, Ordering::Relaxed);
}

/// What one send produced, before it is recorded.
///
/// Separating the attempt from the recording is what makes an automatic retry
/// honest: only the final attempt becomes a sample, so a refreshed token does
/// not double the request count or count a rejection the user never saw.
enum Attempt {
    Http {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
        elapsed: Duration,
        /// Request body bytes that were sent.
        bytes_out: u64,
    },
    Grpc {
        res: Box<swarmo_grpc::GrpcResult>,
        elapsed: Duration,
    },
    /// A WebSocket handshake refused with 401 or 403. Recorded exactly as a
    /// failed connect; kept apart only so the token can be refreshed.
    WsAuthRejected {
        kind: Arc<str>,
        elapsed: Duration,
    },
    /// A whole WebSocket session; its samples are cut from the result.
    Ws {
        res: Box<swarmo_ws::WsResult>,
        /// Any queueing delay the open model charged this iteration.
        extra: Duration,
    },
    /// The request never produced a response.
    Failed {
        protocol: Protocol,
        /// Zero for HTTP, UNAVAILABLE for gRPC — what each protocol uses for
        /// "the call did not happen".
        status: u16,
        kind: Arc<str>,
        elapsed: Duration,
    },
    Cancelled,
}

impl Attempt {
    /// Whether the server turned this away for its credentials.
    ///
    /// gRPC 16 and 7 are UNAUTHENTICATED and PERMISSION_DENIED, the direct
    /// equivalents of 401 and 403.
    fn rejected_for_auth(&self) -> bool {
        match self {
            Attempt::Http { status, .. } => *status == 401 || *status == 403,
            Attempt::Grpc { res, .. } => res.code == 16 || res.code == 7,
            Attempt::WsAuthRejected { .. } => true,
            _ => false,
        }
    }
}

/// Put the step's current token into the request, returning the epoch it came
/// from so a rejection can name the token that was refused.
fn apply_auth_header(
    resolved: &mut ResolvedRequest,
    state: Option<&AuthTokenState>,
) -> Option<u64> {
    let state = state?;
    let (value, epoch) = state.header_value();
    set_header(&mut resolved.headers, &state.header_name, value);
    Some(epoch)
}

/// The same for a WebSocket handshake.
fn apply_ws_auth(
    resolved: &mut swarmo_core::ResolvedWsRequest,
    state: Option<&AuthTokenState>,
) -> Option<u64> {
    let state = state?;
    let (value, epoch) = state.header_value();
    set_header(&mut resolved.headers, &state.header_name, value);
    Some(epoch)
}

/// The same for gRPC, whose metadata keys are always lowercase.
fn apply_auth_metadata(
    resolved: &mut swarmo_core::ResolvedGrpcRequest,
    state: Option<&AuthTokenState>,
) -> Option<u64> {
    let state = state?;
    let (value, epoch) = state.header_value();
    set_header(
        &mut resolved.metadata,
        &state.header_name.to_ascii_lowercase(),
        value,
    );
    Some(epoch)
}

/// Set a header, replacing any existing one with the same name.
fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some(slot) = headers
        .iter_mut()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
    {
        slot.1 = value;
        return;
    }
    headers.push((name.to_string(), value));
}

/// Partition steps into the groups an iteration runs.
///
/// A step marked `parallel` joins the group of the step above it; everything
/// else starts a group of its own. The first step can never join upwards, so
/// a stray flag on it is treated as sequential rather than as an error.
fn step_groups(steps: &[PreparedStep]) -> Vec<std::ops::Range<usize>> {
    let mut groups = Vec::new();
    let mut start = 0;
    for i in 1..=steps.len() {
        let extends = i < steps.len() && steps[i].parallel;
        if !extends {
            groups.push(start..i);
            start = i;
        }
    }
    groups
}

struct NativeVu {
    steps: Arc<Vec<PreparedStep>>,
    client: reqwest::Client,
    sink: MetricsSink,
    /// Aborts everything, in-flight requests included: the user pressed Stop.
    cancel: CancellationToken,
    /// No further iteration will start. Unlike `cancel` this lets requests in
    /// flight finish, and only cuts short the pause after an iteration's last
    /// step, which would otherwise hold the run open past its end.
    ending: CancellationToken,
    base_vars: Arc<HashMap<String, String>>,
    grpc_channels: Arc<swarmo_grpc::ChannelPool>,
    /// Which channel this virtual user draws from, so load spreads across the
    /// pool instead of piling onto one HTTP/2 connection.
    vu_id: usize,
}

impl NativeVu {
    /// One pass over every step. `queue_delay` (open model) is folded into the
    /// first group's latency so queueing is not hidden from the percentiles.
    async fn run_iteration(&self, queue_delay: Option<Duration>) {
        let mut scope = VarScope::new();
        scope.push_layer((*self.base_vars).clone());
        scope.push_layer(HashMap::new());

        let groups = step_groups(&self.steps);
        let last_group = groups.len().saturating_sub(1);
        for (gi, group) in groups.into_iter().enumerate() {
            if self.cancel.is_cancelled() {
                return;
            }
            // Every member of the first group genuinely started late by the
            // queueing delay, not just the first step.
            let extra = if gi == 0 { queue_delay } else { None };

            // Members run concurrently and are all joined before anything
            // after the group starts — like a client fetching a page and its
            // assets, or a batch fanned out to several endpoints. A group of
            // one is simply a sequential step.
            let outcomes = futures::future::join_all(
                group
                    .clone()
                    .map(|idx| self.execute_step(&self.steps[idx], &scope, extra)),
            )
            .await;

            // Captures land after the whole group, in step order, so the
            // result is deterministic no matter which member finished first.
            let mut failed = false;
            for (idx, outcome) in group.clone().zip(outcomes) {
                match outcome {
                    Some(body) => {
                        for cap in &self.steps[idx].capture {
                            if let Some(value) = extract(&body, cap) {
                                scope.set(cap.as_var.clone(), value);
                            }
                        }
                    }
                    None => failed = true,
                }
            }
            if failed {
                // Failures were already recorded; later groups usually depend
                // on this one, so the iteration ends here.
                return;
            }

            // Think time follows the group as a whole: the longest member's
            // roll, since sleeping each member's time in sequence would turn
            // a parallel group back into a serial one.
            let ms = group
                .filter_map(|idx| self.steps[idx].think_time_ms)
                .map(|[lo, hi]| {
                    if hi > lo {
                        rand::thread_rng().gen_range(lo..=hi)
                    } else {
                        lo
                    }
                })
                .max()
                .unwrap_or(0);
            // After the final group the pause only paces the next iteration,
            // so it is dropped once there will not be one.
            let trailing = gi == last_group;
            if ms > 0 && !(trailing && self.ending.is_cancelled()) {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
                    _ = self.cancel.cancelled() => return,
                    _ = self.ending.cancelled(), if trailing => return,
                }
            }
        }
    }

    /// Run one step to completion: resolve, send, retry once on an auth
    /// rejection, and record the final attempt.
    async fn execute_step(
        &self,
        step: &PreparedStep,
        scope: &VarScope,
        extra: Option<Duration>,
    ) -> Option<CapturedBody> {
        // Measured from here through the final attempt, so a refresh and a
        // retry show up as the wall time they really cost rather than being
        // quietly laundered out of the percentiles.
        let t0 = Instant::now();

        let attempt = match &step.action {
            PreparedAction::Http(merged) => {
                let mut resolved = finalize(merged, scope);
                let epoch = apply_auth_header(&mut resolved, step.auth_token.as_deref());
                let first = self.attempt_http(&resolved, t0, extra).await;

                match self
                    .refreshed_epoch(&first, step.auth_token.as_deref(), epoch)
                    .await
                {
                    // The token was stale: send the same request again with
                    // the new one. Once only — a real permission failure
                    // must not turn into a loop.
                    Some(()) => {
                        apply_auth_header(&mut resolved, step.auth_token.as_deref());
                        self.attempt_http(&resolved, t0, extra).await
                    }
                    None => first,
                }
            }
            PreparedAction::Ws(merged) => {
                let mut resolved = swarmo_core::finalize_ws(merged, scope);
                let epoch = apply_ws_auth(&mut resolved, step.auth_token.as_deref());
                let first = self.attempt_ws(&resolved, extra).await;

                // A handshake refused for a stale token is retried once with
                // a fresh one, exactly as an HTTP request is.
                match self
                    .refreshed_epoch(&first, step.auth_token.as_deref(), epoch)
                    .await
                {
                    Some(()) => {
                        apply_ws_auth(&mut resolved, step.auth_token.as_deref());
                        self.attempt_ws(&resolved, extra).await
                    }
                    None => first,
                }
            }
            PreparedAction::Grpc {
                merged,
                descriptors,
                prepared_message,
            } => {
                let mut resolved = swarmo_core::finalize_grpc(merged, scope);
                let epoch = apply_auth_metadata(&mut resolved, step.auth_token.as_deref());
                let first = self
                    .attempt_grpc(
                        &resolved,
                        descriptors,
                        prepared_message.as_deref(),
                        t0,
                        extra,
                    )
                    .await;

                match self
                    .refreshed_epoch(&first, step.auth_token.as_deref(), epoch)
                    .await
                {
                    Some(()) => {
                        apply_auth_metadata(&mut resolved, step.auth_token.as_deref());
                        self.attempt_grpc(
                            &resolved,
                            descriptors,
                            prepared_message.as_deref(),
                            t0,
                            extra,
                        )
                        .await
                    }
                    None => first,
                }
            }
        };

        // Recorded once, for the final attempt only: an automatic retry
        // must not inflate the request count or the error rate.
        self.record(attempt, &step.tag)
    }

    /// One HTTP attempt, with nothing recorded.
    ///
    /// Kept separate from recording so that a request retried after an auth
    /// refresh counts once, not twice.
    async fn attempt_http(
        &self,
        resolved: &ResolvedRequest,
        t0: Instant,
        extra_latency: Option<Duration>,
    ) -> Attempt {
        let extra = extra_latency.unwrap_or(Duration::ZERO);
        // What the body puts on the wire; form fields are close enough that
        // the encoding overhead is noise next to the values themselves.
        let bytes_out = match &resolved.body {
            swarmo_core::ResolvedBody::Bytes { text, .. } => text.len() as u64,
            swarmo_core::ResolvedBody::Form { fields } => fields
                .iter()
                .map(|(k, v)| (k.len() + v.len() + 2) as u64)
                .sum(),
            _ => 0,
        };

        let url = match swarmo_http::normalize_url(&resolved.url) {
            Ok(u) => u,
            Err(e) => {
                tracing::debug!("invalid url in load step: {e}");
                return Attempt::Failed {
                    protocol: Protocol::Http,
                    status: 0,
                    kind: crate::metrics::classify_err(&e),
                    elapsed: t0.elapsed() + extra,
                };
            }
        };

        let builder = match swarmo_http::build_request(&self.client, resolved, &url).await {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!("could not build load request: {e}");
                return Attempt::Failed {
                    protocol: Protocol::Http,
                    status: 0,
                    kind: crate::metrics::classify_err(&e),
                    elapsed: t0.elapsed() + extra,
                };
            }
        };

        let resp = tokio::select! {
            r = builder.send() => r,
            _ = self.cancel.cancelled() => return Attempt::Cancelled,
        };

        match resp {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let headers: Vec<(String, String)> = resp
                    .headers()
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.as_str().to_string(),
                            v.to_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect();
                let body = resp.text().await.unwrap_or_default();
                Attempt::Http {
                    status,
                    headers,
                    body,
                    elapsed: t0.elapsed() + extra,
                    bytes_out,
                }
            }
            Err(e) => {
                tracing::debug!("load request failed: {e}");
                // reqwest knows a timeout for certain; everything else is read
                // off the cause chain.
                let kind = if e.is_timeout() {
                    Arc::from("Timed out")
                } else {
                    crate::metrics::classify_err(&e)
                };
                Attempt::Failed {
                    protocol: Protocol::Http,
                    status: 0,
                    kind,
                    elapsed: t0.elapsed() + extra,
                }
            }
        }
    }

    /// One unary gRPC attempt, with nothing recorded.
    /// Run one WebSocket session. Frames are not kept: the load engine wants
    /// the numbers, and a transcript per iteration would be memory for
    /// nothing.
    async fn attempt_ws(
        &self,
        resolved: &swarmo_core::ResolvedWsRequest,
        extra_latency: Option<Duration>,
    ) -> Attempt {
        let extra = extra_latency.unwrap_or(Duration::ZERO);
        let t0 = Instant::now();
        match swarmo_ws::run(resolved, &self.cancel, false).await {
            Ok(res) if res.error.as_deref() == Some("cancelled") => Attempt::Cancelled,
            Ok(res) => Attempt::Ws {
                res: Box::new(res),
                extra,
            },
            Err(e) => {
                tracing::debug!("WebSocket session failed: {e}");
                if ws_rejected_for_auth(&e) {
                    return Attempt::WsAuthRejected {
                        kind: crate::metrics::classify_err(&e),
                        elapsed: t0.elapsed() + extra,
                    };
                }
                // 1006 is the reserved "never closed properly" code; 0 is
                // what an answered message records, and this is not one.
                Attempt::Failed {
                    protocol: Protocol::Ws,
                    status: 1006,
                    kind: crate::metrics::classify_err(&e),
                    elapsed: t0.elapsed() + extra,
                }
            }
        }
    }

    async fn attempt_grpc(
        &self,
        resolved: &swarmo_core::ResolvedGrpcRequest,
        descriptors: &Arc<swarmo_grpc::DescriptorSource>,
        prepared_message: Option<&prost_reflect::DynamicMessage>,
        t0: Instant,
        extra_latency: Option<Duration>,
    ) -> Attempt {
        let extra = extra_latency.unwrap_or(Duration::ZERO);

        let call = swarmo_grpc::call_unary_with(
            &self.grpc_channels,
            descriptors,
            resolved,
            self.vu_id,
            prepared_message,
        );
        let outcome = tokio::select! {
            r = call => r,
            _ = self.cancel.cancelled() => return Attempt::Cancelled,
        };

        match outcome {
            Ok(res) => Attempt::Grpc {
                res: Box::new(res),
                elapsed: t0.elapsed() + extra,
            },
            Err(e) => {
                tracing::debug!("gRPC call failed: {e}");
                // Client-side failures (a bad message, an unreachable host) are
                // reported as UNAVAILABLE, which is what gRPC uses for "the call
                // did not happen".
                Attempt::Failed {
                    protocol: Protocol::Grpc,
                    status: 14,
                    kind: crate::metrics::classify_err(&e),
                    elapsed: t0.elapsed() + extra,
                }
            }
        }
    }

    /// Refresh the step's token if this attempt was rejected for auth.
    ///
    /// `Some(())` means a fresh token is installed and the caller should try
    /// again; `None` means there is nothing to retry with.
    async fn refreshed_epoch(
        &self,
        attempt: &Attempt,
        state: Option<&AuthTokenState>,
        epoch: Option<u64>,
    ) -> Option<()> {
        let (state, epoch) = (state?, epoch?);
        if !attempt.rejected_for_auth() || self.cancel.is_cancelled() {
            return None;
        }
        match state.refresh(epoch).await {
            Ok(()) => Some(()),
            Err(e) => {
                // Keep the rejection: it says more about what went wrong than
                // a failed refresh would.
                tracing::debug!("auth token refresh failed: {e}");
                None
            }
        }
    }

    /// Record one attempt and hand back its body for captures.
    fn record(&self, attempt: Attempt, tag: &str) -> Option<CapturedBody> {
        match attempt {
            Attempt::Cancelled => None,
            Attempt::WsAuthRejected { kind, elapsed } => self.record(
                Attempt::Failed {
                    protocol: Protocol::Ws,
                    status: 1006,
                    kind,
                    elapsed,
                },
                tag,
            ),
            Attempt::Failed {
                protocol,
                status,
                kind,
                elapsed,
            } => {
                self.sink.sample(Sample {
                    tag: Arc::from(tag),
                    protocol,
                    status,
                    ok: false,
                    latency_us: elapsed.as_micros() as u64,
                    bytes_in: 0,
                    bytes_out: 0,
                    error: Some(kind),
                });
                None
            }
            Attempt::Http {
                status,
                headers,
                body,
                elapsed,
                bytes_out,
            } => {
                self.sink.sample(Sample {
                    tag: Arc::from(tag),
                    protocol: Protocol::Http,
                    status,
                    ok: (200..400).contains(&status),
                    latency_us: elapsed.as_micros() as u64,
                    bytes_in: body.len() as u64,
                    bytes_out,
                    error: None,
                });
                Some(CapturedBody { body, headers })
            }
            Attempt::Ws { res, extra } => {
                // The connect is its own sample under its own tag: a handshake
                // and a message round trip are different things, and one p95
                // covering both would describe neither.
                let exchanged_out: u64 = res.exchanges.iter().map(|e| e.bytes_out).sum();
                let exchanged_in: u64 = res.exchanges.iter().map(|e| e.bytes_in).sum();
                let connect_ms = res.connect_ms.unwrap_or(res.duration_ms);
                self.sink.sample(Sample {
                    tag: Arc::from(format!("{tag} connect").as_str()),
                    protocol: Protocol::Ws,
                    // The same rule the client's history uses: 101 only for a
                    // session that connected and then held; one that broke
                    // afterwards lands under the code it ended on.
                    status: if res.connected && res.error.is_none() {
                        101
                    } else {
                        res.close_code.unwrap_or(1006)
                    },
                    ok: res.connected && res.error.is_none(),
                    latency_us: (Duration::from_secs_f64(connect_ms / 1000.0) + extra).as_micros()
                        as u64,
                    // Traffic no exchange claimed — fire-and-forget sends and
                    // unsolicited frames — is charged to the session.
                    bytes_in: res.bytes_in.saturating_sub(exchanged_in),
                    bytes_out: res.bytes_out.saturating_sub(exchanged_out),
                    error: res.error.as_deref().map(Arc::from),
                });
                for e in &res.exchanges {
                    // The executor stamps a timed-out wait with the time it
                    // actually spent waiting, so a failure costs what it
                    // cost rather than reading as instant.
                    let latency_us = e
                        .latency_ms
                        .map(|ms| Duration::from_secs_f64(ms / 1000.0).as_micros() as u64)
                        .unwrap_or(0);
                    self.sink.sample(Sample {
                        tag: Arc::from(tag),
                        protocol: Protocol::Ws,
                        status: if e.timed_out { 1006 } else { 0 },
                        ok: !e.timed_out,
                        latency_us,
                        bytes_in: e.bytes_in,
                        bytes_out: e.bytes_out,
                        error: e.timed_out.then(|| Arc::from("No reply within the wait")),
                    });
                }
                // A session has no single body to capture from.
                None
            }
            Attempt::Grpc { res, elapsed } => {
                self.sink.sample(Sample {
                    tag: Arc::from(tag),
                    protocol: Protocol::Grpc,
                    status: res.code,
                    ok: res.ok(),
                    latency_us: elapsed.as_micros() as u64,
                    bytes_in: res.response_bytes,
                    bytes_out: res.request_bytes,
                    error: None,
                });

                if !res.ok() {
                    // A non-OK status has no message body, so later steps that
                    // depend on captures from it cannot proceed meaningfully.
                    return None;
                }
                let res = *res;
                let mut headers = res.headers;
                headers.extend(res.trailers);
                Some(CapturedBody {
                    body: res.response_raw_json,
                    headers,
                })
            }
        }
    }
}

/// Whether a WebSocket connect failed because the server refused the
/// handshake's credentials.
///
/// The client reports a refused upgrade only as text (`HTTP error: 401
/// Unauthorized`), so this reads it off the message.
fn ws_rejected_for_auth(e: &swarmo_ws::WsError) -> bool {
    match e {
        swarmo_ws::WsError::Connect { detail, .. } => {
            detail.contains("HTTP error: 401") || detail.contains("HTTP error: 403")
        }
        _ => false,
    }
}

struct CapturedBody {
    body: String,
    headers: Vec<(String, String)>,
}

fn extract(captured: &CapturedBody, cap: &Capture) -> Option<String> {
    match cap.from {
        CaptureSource::Body => {
            let path = cap.json_path.as_deref()?;
            capture_from_body(&captured.body, path)
        }
        CaptureSource::Header => {
            let name = cap.name.as_deref()?;
            captured
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        }
    }
}

// ---------------------------------------------------------------------------
// Engine B: scripted
// ---------------------------------------------------------------------------

mod scripted;
pub use scripted::run_scripted;

#[cfg(test)]
mod tests {
    use super::*;

    fn http(status: u16) -> Attempt {
        Attempt::Http {
            status,
            headers: Vec::new(),
            body: String::new(),
            elapsed: Duration::from_millis(1),
            bytes_out: 0,
        }
    }

    fn prepared(parallel: bool) -> PreparedStep {
        PreparedStep {
            action: PreparedAction::Http(Box::new(swarmo_core::MergedRequest {
                id: String::new(),
                name: "s".into(),
                method: "GET".into(),
                url: "http://x/".into(),
                params: vec![],
                headers: vec![],
                auth: Auth::None,
                body: Body::None,
                settings: RequestSettings::default(),
                pre_scripts: vec![],
                post_scripts: vec![],
            })),
            tag: "t".into(),
            think_time_ms: None,
            capture: vec![],
            auth_token: None,
            parallel,
        }
    }

    #[test]
    fn sequential_steps_each_form_their_own_group() {
        let steps = vec![prepared(false), prepared(false), prepared(false)];
        assert_eq!(step_groups(&steps), vec![0..1, 1..2, 2..3]);
    }

    #[test]
    fn parallel_steps_join_the_step_above() {
        // A → (B ∥ C) → D
        let steps = vec![
            prepared(false),
            prepared(true),
            prepared(true),
            prepared(false),
        ];
        assert_eq!(step_groups(&steps), vec![0..3, 3..4]);
    }

    #[test]
    fn a_parallel_flag_on_the_first_step_has_nothing_to_join() {
        // There is no step above; treating it as sequential beats erroring.
        let steps = vec![prepared(true), prepared(false)];
        assert_eq!(step_groups(&steps), vec![0..1, 1..2]);
    }

    #[test]
    fn an_entirely_parallel_list_is_one_group() {
        let steps = vec![prepared(false), prepared(true), prepared(true)];
        assert_eq!(step_groups(&steps), vec![0..3]);
    }

    #[test]
    fn no_steps_means_no_groups() {
        assert_eq!(step_groups(&[]), Vec::<std::ops::Range<usize>>::new());
    }

    #[test]
    fn only_credential_rejections_trigger_a_retry() {
        assert!(http(401).rejected_for_auth());
        assert!(http(403).rejected_for_auth());
        // A retry must not fire for anything else: re-sending a 500 or a 429
        // would double the load exactly when the server is struggling.
        for status in [200, 404, 429, 500, 503] {
            assert!(!http(status).rejected_for_auth(), "status {status}");
        }
        // A request that never landed has no credential to blame.
        assert!(!Attempt::Failed {
            protocol: Protocol::Http,
            status: 0,
            kind: Arc::from("Connection refused"),
            elapsed: Duration::ZERO,
        }
        .rejected_for_auth());
        assert!(!Attempt::Cancelled.rejected_for_auth());
    }

    #[test]
    fn a_websocket_handshake_refused_for_credentials_triggers_a_retry() {
        let refused = |detail: &str| swarmo_ws::WsError::Connect {
            url: "ws://x/".into(),
            detail: detail.into(),
        };
        assert!(ws_rejected_for_auth(&refused(
            "HTTP error: 401 Unauthorized"
        )));
        assert!(ws_rejected_for_auth(&refused("HTTP error: 403 Forbidden")));
        assert!(!ws_rejected_for_auth(&refused("HTTP error: 404 Not Found")));
        assert!(!ws_rejected_for_auth(&refused("Connection refused")));
        assert!(Attempt::WsAuthRejected {
            kind: Arc::from("x"),
            elapsed: Duration::ZERO,
        }
        .rejected_for_auth());
    }

    #[test]
    fn a_replaced_header_does_not_accumulate() {
        // A retry re-applies the header; if it appended instead of replacing,
        // the request would carry both the stale and the fresh credential.
        let mut headers = vec![
            ("accept".to_string(), "application/json".to_string()),
            ("Authorization".to_string(), "Bearer stale".to_string()),
        ];
        set_header(&mut headers, "authorization", "Bearer fresh".to_string());

        assert_eq!(headers.len(), 2);
        let auth: Vec<&(String, String)> = headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .collect();
        assert_eq!(auth.len(), 1, "the stale credential was left behind");
        assert_eq!(auth[0].1, "Bearer fresh");
    }

    #[test]
    fn a_header_is_added_when_the_request_had_none() {
        let mut headers = vec![("accept".to_string(), "*/*".to_string())];
        set_header(&mut headers, "Authorization", "Bearer t".to_string());
        assert_eq!(headers.len(), 2);
        assert_eq!(
            headers[1],
            ("Authorization".to_string(), "Bearer t".to_string())
        );
    }
}
