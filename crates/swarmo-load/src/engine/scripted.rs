//! Engine B: virtual users driven by a `*.user.js` script.
//!
//! QuickJS values are not `Send`, so each virtual user owns an OS thread with
//! its own JS context and its own current-thread tokio runtime. That is why
//! scripted runs are capped at `MAX_SCRIPTED_VUS`: threads are the scarce
//! resource here, not sockets. Engine A remains the high-throughput path.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use swarmo_core::model::LoadMode;
use swarmo_core::store::Protocol;
use swarmo_core::VarScope;
use swarmo_script::{
    HostGrpcRequest, HostGrpcResponse, HostRequest, HostResponse, ScriptHost, UserMixEntry,
    VuEngine,
};
use tokio_util::sync::CancellationToken;

use crate::metrics::{MetricsSink, Sample};
use crate::plan::LoadPlan;

const THREAD_STACK: usize = 1024 * 1024;
const SCHEDULER_TICK: Duration = Duration::from_millis(100);
const TICKET_TICK: Duration = Duration::from_millis(10);
const MAX_QUEUE_AGE: Duration = Duration::from_secs(10);

/// Everything a scripted virtual user needs to make gRPC calls. Built once per
/// run and shared by every virtual-user thread.
#[derive(Clone)]
pub struct GrpcContext {
    pub descriptors: Arc<swarmo_grpc::DescriptorSource>,
    pub channels: Arc<swarmo_grpc::ChannelPool>,
    pub source: swarmo_core::ProtoSource,
    pub verify_tls: bool,
    pub timeout_ms: u64,
    pub max_response_bytes: u64,
}

/// The bridge that lets sandboxed scripts perform HTTP and gRPC, and sleep.
struct LoadHost {
    rt: tokio::runtime::Runtime,
    client: reqwest::Client,
    sink: MetricsSink,
    cancel: CancellationToken,
    stop: Arc<AtomicBool>,
    scope: VarScope,
    grpc: Option<GrpcContext>,
    vu_id: usize,
    /// Set when a request fails in a way already recorded as a failed sample,
    /// so the throw a script sees for it is not mistaken for a script bug.
    request_failed: AtomicBool,
    /// Open model: how long the current iteration's arrival sat queued, in
    /// microseconds. Charged to the iteration's first request, as the native
    /// engine does, so queueing shows in the percentiles instead of vanishing.
    queue_delay_us: AtomicU64,
}

impl LoadHost {
    /// The queueing delay still owed by this iteration; only the first
    /// request pays it.
    fn take_queue_delay(&self) -> Duration {
        Duration::from_micros(self.queue_delay_us.swap(0, Ordering::Relaxed))
    }
}

impl ScriptHost for LoadHost {
    fn send_request(&self, req: HostRequest) -> Result<HostResponse, String> {
        // `stop` cannot be awaited, so it is checked here: without this every
        // virtual user kept sending new requests after the run had ended.
        if self.is_cancelled() {
            return Err("cancelled".to_string());
        }
        let tag: Arc<str> = Arc::from(
            req.tag
                .clone()
                .unwrap_or_else(|| format!("{} {}", req.method, req.url))
                .as_str(),
        );

        let url = swarmo_http::normalize_url(&req.url).map_err(|e| e.to_string())?;
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|_| format!("invalid HTTP method: {}", req.method))?;

        let mut builder = self.client.request(method, url);
        for (k, v) in &req.headers {
            if !k.trim().is_empty() {
                builder = builder.header(k, v);
            }
        }
        let req_bytes = req.body.as_ref().map(|b| b.len() as u64).unwrap_or(0);
        if let Some(body) = req.body.clone() {
            builder = builder.body(body);
        }

        let queued = self.take_queue_delay();
        let t0 = Instant::now();
        // Raced against cancellation: without this, Stop waits out every
        // in-flight request's full timeout before the run can end.
        let result = self.rt.block_on(async {
            tokio::select! {
                r = builder.send() => Some(r),
                _ = self.cancel.cancelled() => None,
            }
        });
        let Some(result) = result else {
            return Err("cancelled".to_string());
        };

        match result {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let status_text = resp
                    .status()
                    .canonical_reason()
                    .unwrap_or_default()
                    .to_string();
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
                let body = self
                    .rt
                    .block_on(async { resp.text().await })
                    .unwrap_or_default();
                let elapsed = t0.elapsed();

                self.sink.sample(Sample {
                    tag,
                    protocol: Protocol::Http,
                    status,
                    ok: (200..400).contains(&status),
                    latency_us: (elapsed + queued).as_micros() as u64,
                    bytes_in: body.len() as u64,
                    bytes_out: req_bytes,
                    error: None,
                });

                Ok(HostResponse {
                    status,
                    status_text,
                    headers,
                    body,
                    duration_ms: elapsed.as_secs_f64() * 1000.0,
                })
            }
            Err(e) => {
                let elapsed = t0.elapsed();
                self.request_failed.store(true, Ordering::Relaxed);
                self.sink.sample(Sample {
                    tag,
                    protocol: Protocol::Http,
                    status: 0,
                    ok: false,
                    latency_us: (elapsed + queued).as_micros() as u64,
                    bytes_in: 0,
                    bytes_out: 0,
                    error: Some(crate::metrics::classify_err(&e)),
                });
                Err(e.to_string())
            }
        }
    }

    fn send_grpc(&self, req: HostGrpcRequest) -> Result<HostGrpcResponse, String> {
        let Some(grpc) = &self.grpc else {
            return Err(
                "This script makes gRPC calls but `options.grpc` is not set. Add \
                 `grpc: { protoFiles: [...] }` or `grpc: { reflection: true }`."
                    .to_string(),
            );
        };
        if self.is_cancelled() {
            return Err("cancelled".to_string());
        }

        let tag: Arc<str> = Arc::from(
            req.tag
                .clone()
                .unwrap_or_else(|| format!("{}/{}", req.service, req.method))
                .as_str(),
        );

        let resolved = swarmo_core::ResolvedGrpcRequest {
            name: req.method.clone(),
            address: swarmo_core::model_grpc::normalize_address(&req.address),
            proto_source: grpc.source.clone(),
            service: req.service.clone(),
            method: req.method.clone(),
            metadata: req.metadata.clone(),
            message_json: req.message_json.clone(),
            settings: swarmo_core::GrpcSettings {
                timeout_ms: grpc.timeout_ms,
                verify_tls: grpc.verify_tls,
                max_response_bytes: grpc.max_response_bytes,
                stream_max_messages: None,
            },
            unresolved: Vec::new(),
        };

        let queued = self.take_queue_delay();
        let t0 = Instant::now();
        let outcome = self.rt.block_on(async {
            tokio::select! {
                r = swarmo_grpc::call_unary(
                    &grpc.channels,
                    &grpc.descriptors,
                    &resolved,
                    self.vu_id,
                ) => Some(r),
                _ = self.cancel.cancelled() => None,
            }
        });
        let Some(outcome) = outcome else {
            return Err("cancelled".to_string());
        };

        match outcome {
            Ok(res) => {
                let elapsed = t0.elapsed();
                self.sink.sample(Sample {
                    tag,
                    protocol: Protocol::Grpc,
                    status: res.code,
                    ok: res.ok(),
                    latency_us: (elapsed + queued).as_micros() as u64,
                    bytes_in: res.response_bytes,
                    bytes_out: res.request_bytes,
                    error: None,
                });
                Ok(HostGrpcResponse {
                    code: res.code,
                    code_name: res.code_name,
                    status_message: res.status_message,
                    body: res.response_raw_json,
                    headers: res.headers,
                    trailers: res.trailers,
                    duration_ms: res.duration_ms,
                })
            }
            Err(e) => {
                let elapsed = t0.elapsed();
                self.request_failed.store(true, Ordering::Relaxed);
                self.sink.sample(Sample {
                    tag,
                    protocol: Protocol::Grpc,
                    status: 14,
                    ok: false,
                    latency_us: (elapsed + queued).as_micros() as u64,
                    bytes_in: 0,
                    bytes_out: 0,
                    error: Some(crate::metrics::classify_err(&e)),
                });
                Err(e.to_string())
            }
        }
    }

    fn sleep(&self, ms: u64) {
        // Sleep in slices so cancellation is felt promptly.
        let deadline = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < deadline {
            if self.is_cancelled() {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(remaining.min(Duration::from_millis(50)));
        }
    }

    fn interpolate(&self, text: &str) -> String {
        swarmo_core::interpolate_str(text, &self.scope)
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled() || self.stop.load(Ordering::Relaxed)
    }
}

/// Deterministic weighted assignment of virtual users to exec functions, so a
/// 3:1 mix really produces a 3:1 split rather than one that only holds on average.
fn weighted_pattern(mix: &[UserMixEntry]) -> Vec<String> {
    let scaled: Vec<u64> = mix
        .iter()
        .map(|m| (m.weight.max(0.0) * 100.0).round() as u64)
        .collect();
    // Saturating: an absurd weight casts to u64::MAX, and a plain sum of two
    // of those would overflow.
    let total: u64 = scaled.iter().fold(0u64, |acc, v| acc.saturating_add(*v));
    if total == 0 {
        return mix.iter().map(|m| m.exec.clone()).collect();
    }

    let divisor = scaled.iter().copied().filter(|v| *v > 0).fold(0u64, gcd);
    let divisor = divisor.max(1);

    let mut pattern = Vec::new();
    for (i, count) in scaled.iter().enumerate() {
        for _ in 0..(count / divisor) {
            pattern.push(mix[i].exec.clone());
        }
    }
    if pattern.is_empty() {
        pattern.push(mix[0].exec.clone());
    }
    pattern
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Run a scripted plan. Returns an error only for failures that make the whole
/// run meaningless (for example the script does not compile).
/// How each virtual-user thread should build its own HTTP client.
#[derive(Debug, Clone, Copy)]
pub struct ClientConfig {
    pub verify_tls: bool,
    pub timeout_ms: u64,
    pub new_connection_per_iteration: bool,
}

/// A `reqwest::Client`'s connection pool is driven by the runtime that created
/// it, so a client shared across the per-VU runtimes would hand thread B a
/// connection only thread A's runtime can poll. Each thread builds its own.
fn build_thread_client(cfg: ClientConfig) -> Result<reqwest::Client, reqwest::Error> {
    let mut b = reqwest::Client::builder()
        .danger_accept_invalid_certs(!cfg.verify_tls)
        .tcp_nodelay(true)
        .pool_idle_timeout(Duration::from_secs(90))
        .user_agent(concat!("Swarmo/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_millis(cfg.timeout_ms.max(1)));
    if cfg.new_connection_per_iteration {
        b = b.pool_max_idle_per_host(0);
    }
    b.build()
}

pub async fn run_scripted(
    plan: &LoadPlan,
    source: String,
    user_mix: Vec<UserMixEntry>,
    client_cfg: ClientConfig,
    sink: MetricsSink,
    cancel: CancellationToken,
    grpc: Option<GrpcContext>,
) -> Result<(), String> {
    // Compile once up front so a broken script fails immediately and clearly,
    // rather than once per virtual user after the run has started.
    {
        let host: swarmo_script::SharedHost = Arc::new(swarmo_script::NullHost);
        VuEngine::load(host, &source, 0, &plan.base_vars).map_err(|e| e.to_string())?;
    }

    let pattern = weighted_pattern(&user_mix);
    let stop = Arc::new(AtomicBool::new(false));
    let target = Arc::new(AtomicU64::new(0));
    let active = Arc::new(AtomicU64::new(0));

    let tickets: Option<TicketQueue> = match plan.mode {
        LoadMode::Open => Some(TicketQueue::new()),
        LoadMode::Closed => None,
    };

    let mut scope = VarScope::new();
    scope.push_layer(plan.base_vars.clone());

    // Spawn every virtual user thread up front; each idles until it is within
    // the current target, which keeps ramping cheap and jitter-free.
    let mut threads: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let startup_errors = Arc::new(Mutex::new(Vec::<String>::new()));
    // Held apart from the per-thread clones below, which move into each
    // closure, so a failed spawn can still reach every thread already running.
    let stop_all = stop.clone();
    let tickets_all = tickets.clone();

    for id in 0..plan.max_vus as u64 {
        let exec = pattern[(id as usize) % pattern.len()].clone();
        let source = source.clone();
        let sink = sink.clone();
        let cancel = cancel.clone();
        let stop = stop.clone();
        let scope = scope.clone();
        let base_vars = plan.base_vars.clone();
        let target = target.clone();
        let active = active.clone();
        let tickets = tickets.clone();
        let errors = startup_errors.clone();
        let grpc = grpc.clone();

        let builder = std::thread::Builder::new()
            .name(format!("swarmo-vu-{id}"))
            .stack_size(THREAD_STACK);

        let spawned = builder.spawn(move || {
            vu_thread(VuThreadArgs {
                id,
                exec,
                source,
                client_cfg,
                sink,
                cancel,
                stop,
                scope,
                base_vars,
                target,
                active,
                tickets,
                errors,
                grpc,
            })
        });
        let handle = match spawned {
            Ok(h) => h,
            Err(e) => {
                // The threads already started would otherwise keep polling
                // until something else happens to cancel the run — which,
                // once this function has returned an error, nothing does.
                stop_all.store(true, Ordering::Relaxed);
                if let Some(q) = &tickets_all {
                    q.close();
                }
                for t in threads {
                    let _ = t.join();
                }
                return Err(format!("could not start virtual-user thread: {e}"));
            }
        };
        threads.push(handle);
    }

    // Scheduler.
    let start = Instant::now();
    let total = plan.total_duration();
    let mut credit = 0.0f64;
    let tick = if tickets.is_some() {
        TICKET_TICK
    } else {
        SCHEDULER_TICK
    };
    let mut ticker = tokio::time::interval(tick);

    loop {
        if cancel.is_cancelled() {
            break;
        }
        let elapsed = start.elapsed();
        if elapsed >= total {
            break;
        }

        let value = plan.target_at(elapsed).max(0.0);
        match &tickets {
            None => {
                let want = (value.round() as u64).min(plan.max_vus as u64);
                target.store(want, Ordering::Relaxed);
                sink.counters().target_vus.store(want, Ordering::Relaxed);
            }
            Some(q) => {
                // Open model: every thread is an executor waiting for tickets.
                target.store(plan.max_vus as u64, Ordering::Relaxed);
                sink.counters()
                    .target_vus
                    .store(value.round() as u64, Ordering::Relaxed);
                credit += value * tick.as_secs_f64();
                while credit >= 1.0 {
                    credit -= 1.0;
                    if !q.push(Instant::now(), plan.max_vus as usize * 4 + 64) {
                        sink.counters().vus_saturated.store(1, Ordering::Relaxed);
                        sink.dropped_iteration();
                    }
                }
            }
        }
        // Both models: in the open one `active` counts executors mid-iteration.
        sink.counters()
            .active_vus
            .store(active.load(Ordering::Relaxed), Ordering::Relaxed);

        tokio::select! {
            _ = ticker.tick() => {}
            _ = cancel.cancelled() => break,
        }
    }

    stop.store(true, Ordering::Relaxed);
    target.store(0, Ordering::Relaxed);
    if let Some(q) = &tickets {
        q.close();
    }

    // Joining blocking threads must not block the async runtime.
    let joined = tokio::task::spawn_blocking(move || {
        for t in threads {
            let _ = t.join();
        }
    })
    .await;
    if joined.is_err() {
        tracing::warn!("virtual-user threads did not shut down cleanly");
    }

    sink.counters().active_vus.store(0, Ordering::Relaxed);

    // Arrivals still queued when the run ended never ran; they are dropped
    // iterations like any other, not something to lose without a trace.
    if let Some(q) = &tickets {
        for _ in 0..q.drain() {
            sink.dropped_iteration();
        }
    }

    let errs = startup_errors.lock().unwrap();
    if !errs.is_empty() && errs.len() as u32 == plan.max_vus {
        return Err(errs[0].clone());
    }
    Ok(())
}

struct VuThreadArgs {
    id: u64,
    exec: String,
    source: String,
    client_cfg: ClientConfig,
    sink: MetricsSink,
    cancel: CancellationToken,
    stop: Arc<AtomicBool>,
    scope: VarScope,
    base_vars: HashMap<String, String>,
    target: Arc<AtomicU64>,
    active: Arc<AtomicU64>,
    tickets: Option<TicketQueue>,
    errors: Arc<Mutex<Vec<String>>>,
    grpc: Option<GrpcContext>,
}

fn vu_thread(args: VuThreadArgs) {
    let VuThreadArgs {
        id,
        exec,
        source,
        client_cfg,
        sink,
        cancel,
        stop,
        scope,
        base_vars,
        target,
        active,
        tickets,
        errors,
        grpc,
    } = args;

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            errors.lock().unwrap().push(format!(
                "virtual user {id} could not start its runtime: {e}"
            ));
            return;
        }
    };

    // Built inside this thread so the client's connection pool belongs to the
    // runtime that will drive it.
    let client = match rt.block_on(async { build_thread_client(client_cfg) }) {
        Ok(c) => c,
        Err(e) => {
            errors.lock().unwrap().push(format!(
                "virtual user {id} could not build an HTTP client: {e}"
            ));
            return;
        }
    };

    let host = Arc::new(LoadHost {
        rt,
        client,
        sink: sink.clone(),
        cancel: cancel.clone(),
        stop: stop.clone(),
        scope,
        grpc,
        vu_id: id as usize,
        request_failed: AtomicBool::new(false),
        queue_delay_us: AtomicU64::new(0),
    });

    let engine = match VuEngine::load(host.clone(), &source, id, &base_vars) {
        Ok((e, _opts)) => e,
        Err(e) => {
            errors
                .lock()
                .unwrap()
                .push(format!("virtual user {id}: {e}"));
            return;
        }
    };

    let mut iteration = 0u64;
    let mut counted_active = false;

    loop {
        if cancel.is_cancelled() || stop.load(Ordering::Relaxed) {
            break;
        }

        match &tickets {
            // Closed model: run continuously while inside the target.
            None => {
                if id >= target.load(Ordering::Relaxed) {
                    if counted_active {
                        active.fetch_sub(1, Ordering::Relaxed);
                        counted_active = false;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                if !counted_active {
                    active.fetch_add(1, Ordering::Relaxed);
                    counted_active = true;
                }
            }
            // Open model: wait for an arrival ticket.
            Some(q) => match q.pop(Duration::from_millis(50)) {
                TicketPop::Closed => break,
                TicketPop::Empty => continue,
                TicketPop::Ticket(scheduled) => {
                    if scheduled.elapsed() > MAX_QUEUE_AGE {
                        sink.dropped_iteration();
                        continue;
                    }
                    host.queue_delay_us
                        .store(scheduled.elapsed().as_micros() as u64, Ordering::Relaxed);
                    active.fetch_add(1, Ordering::Relaxed);
                    counted_active = true;
                }
            },
        }

        host.request_failed.store(false, Ordering::Relaxed);
        match engine.run_iteration(&exec, iteration) {
            Ok(checks) => {
                let list: Vec<(String, u64, u64)> = checks
                    .into_iter()
                    .map(|(name, c)| (name, c.passes, c.fails))
                    .collect();
                sink.checks(list);
            }
            Err(e) => {
                // An iteration that throws is a script-level failure. It is
                // reported once per virtual user to avoid flooding the log.
                //
                // Two kinds of throw are not: the run ending interrupts
                // whatever iteration is under way, and a failed request —
                // already recorded as a failed sample — surfaces in the
                // script as an exception. Counting either would report a run
                // that simply ended, or one against a server that is down, as
                // a broken script.
                let interrupted = cancel.is_cancelled() || stop.load(Ordering::Relaxed);
                let request_failed = host.request_failed.load(Ordering::Relaxed);
                if iteration == 0 && !interrupted && !request_failed {
                    tracing::warn!("virtual user {id} iteration failed: {e}");
                    errors
                        .lock()
                        .unwrap()
                        .push(format!("virtual user {id}: {e}"));
                }
            }
        }

        iteration += 1;

        if tickets.is_some() && counted_active {
            active.fetch_sub(1, Ordering::Relaxed);
            counted_active = false;
        }
    }

    if counted_active {
        active.fetch_sub(1, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// A simple blocking ticket queue shared by every scripted executor thread.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct TicketQueue {
    inner: Arc<(Mutex<TicketState>, std::sync::Condvar)>,
}

struct TicketState {
    queue: std::collections::VecDeque<Instant>,
    closed: bool,
}

enum TicketPop {
    Ticket(Instant),
    Empty,
    Closed,
}

impl TicketQueue {
    fn new() -> Self {
        Self {
            inner: Arc::new((
                Mutex::new(TicketState {
                    queue: std::collections::VecDeque::new(),
                    closed: false,
                }),
                std::sync::Condvar::new(),
            )),
        }
    }

    /// Returns false when the queue is at capacity (the run is saturated).
    fn push(&self, at: Instant, cap: usize) -> bool {
        let (lock, cv) = &*self.inner;
        let mut state = lock.lock().unwrap();
        if state.closed || state.queue.len() >= cap {
            return false;
        }
        state.queue.push_back(at);
        cv.notify_one();
        true
    }

    fn pop(&self, timeout: Duration) -> TicketPop {
        let (lock, cv) = &*self.inner;
        let mut state = lock.lock().unwrap();
        if let Some(t) = state.queue.pop_front() {
            return TicketPop::Ticket(t);
        }
        if state.closed {
            return TicketPop::Closed;
        }
        let (mut state, _) = cv.wait_timeout(state, timeout).unwrap();
        match state.queue.pop_front() {
            Some(t) => TicketPop::Ticket(t),
            None if state.closed => TicketPop::Closed,
            None => TicketPop::Empty,
        }
    }

    fn close(&self) {
        let (lock, cv) = &*self.inner;
        lock.lock().unwrap().closed = true;
        cv.notify_all();
    }

    /// Empty the queue, returning how many arrivals were still waiting.
    fn drain(&self) -> usize {
        let (lock, _) = &*self.inner;
        let mut state = lock.lock().unwrap();
        let n = state.queue.len();
        state.queue.clear();
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mix(pairs: &[(&str, f64)]) -> Vec<UserMixEntry> {
        pairs
            .iter()
            .map(|(e, w)| UserMixEntry {
                exec: e.to_string(),
                weight: *w,
            })
            .collect()
    }

    #[test]
    fn three_to_one_pattern() {
        let p = weighted_pattern(&mix(&[("a", 3.0), ("b", 1.0)]));
        assert_eq!(p.len(), 4);
        assert_eq!(p.iter().filter(|x| *x == "a").count(), 3);
        assert_eq!(p.iter().filter(|x| *x == "b").count(), 1);
    }

    #[test]
    fn equal_weights_alternate() {
        let p = weighted_pattern(&mix(&[("a", 1.0), ("b", 1.0)]));
        assert_eq!(p, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn fractional_weights() {
        let p = weighted_pattern(&mix(&[("a", 0.75), ("b", 0.25)]));
        assert_eq!(p.iter().filter(|x| *x == "a").count(), 3);
        assert_eq!(p.iter().filter(|x| *x == "b").count(), 1);
    }

    #[test]
    fn zero_weight_entry_is_never_assigned() {
        let p = weighted_pattern(&mix(&[("a", 1.0), ("b", 0.0)]));
        assert!(!p.iter().any(|x| x == "b"));
    }

    #[test]
    fn ticket_queue_respects_capacity_and_close() {
        let q = TicketQueue::new();
        assert!(q.push(Instant::now(), 1));
        assert!(!q.push(Instant::now(), 1));
        assert!(matches!(
            q.pop(Duration::from_millis(1)),
            TicketPop::Ticket(_)
        ));
        q.close();
        assert!(matches!(q.pop(Duration::from_millis(1)), TicketPop::Closed));
    }

    #[test]
    fn ticket_queue_drain_counts_what_was_left() {
        let q = TicketQueue::new();
        for _ in 0..3 {
            assert!(q.push(Instant::now(), 10));
        }
        q.close();
        assert_eq!(q.drain(), 3);
        assert_eq!(q.drain(), 0);
    }
}
