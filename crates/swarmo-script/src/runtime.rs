//! The QuickJS engine wrapper.
//!
//! Design note: the Rust<->JS boundary is deliberately tiny. Only a handful of
//! primitive functions are bound natively (all taking and returning strings);
//! the entire user-facing API (`sw`, `pm`, `ctx`) is implemented in JavaScript
//! in `prelude.js`. That keeps the marshalling code small and the API easy to
//! evolve.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use rquickjs::{Context, Ctx, Function, Runtime};

use crate::host::{HostRequest, SharedHost};
use crate::types::{ConsoleLine, LogLevel, ScriptError};

pub const PRELUDE: &str = include_str!("prelude.js");

/// Default limits for request scripts. VU scripts pass `timeout_ms: None`.
#[derive(Debug, Clone)]
pub struct Limits {
    pub memory_bytes: usize,
    pub stack_bytes: usize,
    pub timeout_ms: Option<u64>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            stack_bytes: 1024 * 1024,
            timeout_ms: Some(5_000),
        }
    }
}

impl Limits {
    /// Virtual users are stopped by run cancellation, not by a wall clock.
    pub fn for_vu() -> Self {
        Self {
            timeout_ms: None,
            ..Default::default()
        }
    }
}

struct Deadline {
    at: Mutex<Option<Instant>>,
    tripped: AtomicBool,
}

pub struct Engine {
    // Field order matters: the context must drop before the runtime.
    ctx: Context,
    #[allow(dead_code)]
    rt: Runtime,
    deadline: Arc<Deadline>,
    console: Arc<Mutex<Vec<ConsoleLine>>>,
    host: SharedHost,
    limits: Limits,
}

impl Engine {
    pub fn new(host: SharedHost, limits: Limits) -> Result<Self, ScriptError> {
        let rt = Runtime::new().map_err(|e| ScriptError::Engine(e.to_string()))?;
        rt.set_memory_limit(limits.memory_bytes);
        rt.set_max_stack_size(limits.stack_bytes);

        let deadline = Arc::new(Deadline {
            at: Mutex::new(None),
            tripped: AtomicBool::new(false),
        });

        {
            let d = deadline.clone();
            let h = host.clone();
            rt.set_interrupt_handler(Some(Box::new(move || {
                if h.is_cancelled() {
                    d.tripped.store(true, Ordering::Relaxed);
                    return true;
                }
                let at = *d.at.lock().unwrap();
                match at {
                    Some(t) if Instant::now() >= t => {
                        d.tripped.store(true, Ordering::Relaxed);
                        true
                    }
                    _ => false,
                }
            })));
        }

        let ctx = Context::full(&rt).map_err(|e| ScriptError::Engine(e.to_string()))?;
        let console = Arc::new(Mutex::new(Vec::new()));

        let engine = Engine {
            ctx,
            rt,
            deadline,
            console,
            host,
            limits,
        };
        engine.install()?;
        Ok(engine)
    }

    pub fn console(&self) -> Vec<ConsoleLine> {
        self.console.lock().unwrap().clone()
    }

    pub fn clear_console(&self) {
        self.console.lock().unwrap().clear();
    }

    fn install(&self) -> Result<(), ScriptError> {
        let console = self.console.clone();
        let host_http = self.host.clone();
        let host_grpc = self.host.clone();
        let host_sleep = self.host.clone();
        let host_interp = self.host.clone();

        self.ctx.with(|ctx| -> Result<(), ScriptError> {
            let g = ctx.globals();

            let f = Function::new(ctx.clone(), move |level: String, text: String| {
                let level = match level.as_str() {
                    "info" => LogLevel::Info,
                    "warn" => LogLevel::Warn,
                    "error" => LogLevel::Error,
                    _ => LogLevel::Log,
                };
                let mut c = console.lock().unwrap();
                // Bound the buffer so a runaway loop cannot exhaust memory.
                if c.len() < 2_000 {
                    c.push(ConsoleLine { level, text });
                }
            })
            .map_err(js_err)?;
            g.set("__swarmo_log", f).map_err(js_err)?;

            let f =
                Function::new(ctx.clone(), || uuid::Uuid::new_v4().to_string()).map_err(js_err)?;
            g.set("__swarmo_uuid", f).map_err(js_err)?;

            // `btoa`/`atob` work on "binary strings" (one char per byte,
            // U+0000..U+00FF), as in browsers. `None` means the input is
            // invalid; the prelude turns that into a throw.
            let f = Function::new(ctx.clone(), |s: String| -> Option<String> {
                let bytes = s
                    .chars()
                    .map(|c| u8::try_from(u32::from(c)).ok())
                    .collect::<Option<Vec<u8>>>()?;
                Some(base64::engine::general_purpose::STANDARD.encode(bytes))
            })
            .map_err(js_err)?;
            g.set("__swarmo_b64encode", f).map_err(js_err)?;

            let f = Function::new(ctx.clone(), |s: String| -> Option<String> {
                forgiving_b64_decode(&s).map(|bytes| bytes.into_iter().map(char::from).collect())
            })
            .map_err(js_err)?;
            g.set("__swarmo_b64decode", f).map_err(js_err)?;

            let f = Function::new(ctx.clone(), move |req_json: String| -> String {
                do_http(&host_http, &req_json)
            })
            .map_err(js_err)?;
            g.set("__swarmo_http", f).map_err(js_err)?;

            let f = Function::new(ctx.clone(), move |req_json: String| -> String {
                do_grpc(&host_grpc, &req_json)
            })
            .map_err(js_err)?;
            g.set("__swarmo_grpc", f).map_err(js_err)?;

            let f = Function::new(ctx.clone(), move |ms: f64| {
                let ms = if ms.is_finite() && ms > 0.0 {
                    ms as u64
                } else {
                    0
                };
                host_sleep.sleep(ms);
            })
            .map_err(js_err)?;
            g.set("__swarmo_sleep", f).map_err(js_err)?;

            let f = Function::new(ctx.clone(), move |s: String| host_interp.interpolate(&s))
                .map_err(js_err)?;
            g.set("__swarmo_interp", f).map_err(js_err)?;

            let f = Function::new(ctx.clone(), || now_millis() as f64).map_err(js_err)?;
            g.set("__swarmo_now", f).map_err(js_err)?;

            ctx.eval::<(), _>(PRELUDE).map_err(|_| {
                ScriptError::Engine(format!("prelude failed: {}", fmt_exception(&ctx)))
            })?;

            Ok(())
        })?;

        Ok(())
    }

    /// Evaluate JavaScript in the engine's context, returning its value as a
    /// JSON string (via `JSON.stringify`). Used to read results back out.
    pub fn eval_json(&self, source: &str) -> Result<String, ScriptError> {
        self.arm();
        let r = self
            .ctx
            .with(|ctx| match ctx.eval::<rquickjs::Value, _>(source) {
                Ok(v) => {
                    let json: Result<String, _> = ctx
                        .json_stringify(v)
                        .map(|s| s.and_then(|s| s.to_string().ok()).unwrap_or_default());
                    json.map_err(|_| ScriptError::Js(fmt_exception(&ctx)))
                }
                Err(_) => Err(ScriptError::Js(fmt_exception(&ctx))),
            });
        self.disarm();
        r
    }

    /// Run user script source to completion, including its microtask queue.
    ///
    /// The source is wrapped in an async IIFE so `await` works at top level.
    /// None of our host functions return real promises, so every `await`
    /// settles as soon as the job queue is drained.
    pub fn run_user_script(&self, source: &str) -> Result<(), ScriptError> {
        let mut wrapped = String::with_capacity(source.len() + 256);
        wrapped.push_str("__swarmo.__done = false; __swarmo.__err = null; (async () => {\n");
        wrapped.push_str(source);
        wrapped.push_str(
            "\n})().then(function(){ __swarmo.__done = true; }, function(e){ __swarmo.__done = true; __swarmo.__err = __swarmo.errText(e); });",
        );

        self.arm();
        let result = self.drive(&wrapped);
        self.disarm();
        result
    }

    fn drive(&self, wrapped: &str) -> Result<(), ScriptError> {
        // 1. Evaluate. A syntax error or synchronous throw surfaces here.
        let eval_result = self.ctx.with(|ctx| match ctx.eval::<(), _>(wrapped) {
            Ok(()) => Ok(()),
            Err(_) => Err(ScriptError::Js(fmt_exception(&ctx))),
        });
        if let Err(e) = eval_result {
            return Err(self.classify(e));
        }

        // 2. Drain the microtask queue until the IIFE settles. Once the
        //    deadline trips, even these small bookkeeping evals can be
        //    interrupted, so a failure here is an error, never "done".
        loop {
            let done = self.ctx.with(|ctx| {
                ctx.eval::<bool, _>("!!__swarmo.__done")
                    .map_err(|_| ScriptError::Js(fmt_exception(&ctx)))
            });
            match done {
                Ok(true) => break,
                Ok(false) => {}
                Err(e) => return Err(self.classify(e)),
            }
            match self.run_job() {
                Ok(true) => continue,
                Ok(false) => break, // No jobs left but not settled: a dangling promise.
                Err(e) => return Err(self.classify(e)),
            }
        }
        // Flush any remaining jobs so post-settle microtasks still run.
        for _ in 0..10_000 {
            match self.run_job() {
                Ok(true) => continue,
                Ok(false) => break,
                Err(e) => return Err(self.classify(e)),
            }
        }

        // 3. Surface an async throw.
        let err = self.ctx.with(|ctx| {
            ctx.eval::<Option<String>, _>("__swarmo.__err === null ? undefined : __swarmo.__err")
                .map_err(|_| ScriptError::Js(fmt_exception(&ctx)))
        });
        match err {
            Ok(Some(e)) => Err(self.classify(ScriptError::Js(e))),
            // Anything interrupted, even work after the IIFE settled, means
            // the script did not run to completion.
            Ok(None) if self.deadline.tripped.load(Ordering::Relaxed) => {
                Err(self.classify(ScriptError::Js("interrupted".to_string())))
            }
            Ok(None) => Ok(()),
            Err(e) => Err(self.classify(e)),
        }
    }

    /// Run one pending job. `Ok(false)` means the queue was empty.
    fn run_job(&self) -> Result<bool, ScriptError> {
        match self.rt.execute_pending_job() {
            Ok(ran) => Ok(ran),
            Err(e) => {
                // rquickjs 0.6.2 wraps the job's `JSContext` in a `Context`
                // without `JS_DupContext`, so dropping this exception frees a
                // reference it never owned: the context's refcount underflows
                // and QuickJS aborts (or corrupts the heap in release builds).
                // Take the missing reference first so the drop is balanced.
                // (`mem::forget` would also stop the underflow, but it would
                // leak the `Runtime` clone inside, and with it the whole JS
                // heap.) Remove once rquickjs is upgraded past this bug.
                //
                // SAFETY: the pointer is our own live context; the dup is
                // released by the `Context` drop just below.
                unsafe { rquickjs::qjs::JS_DupContext(e.0.as_raw().as_ptr()) };
                drop(e);
                // Clear the exception the job left pending on our context.
                let msg = self.ctx.with(|ctx| fmt_exception(&ctx));
                Err(ScriptError::Js(format!("error in a pending job: {msg}")))
            }
        }
    }

    /// Turn a generic JS failure into a timeout error when the deadline tripped.
    fn classify(&self, e: ScriptError) -> ScriptError {
        if self.deadline.tripped.load(Ordering::Relaxed) {
            if self.host.is_cancelled() {
                return ScriptError::Js("script cancelled".into());
            }
            return ScriptError::Timeout(self.limits.timeout_ms.unwrap_or(0));
        }
        e
    }

    fn arm(&self) {
        self.deadline.tripped.store(false, Ordering::Relaxed);
        let at = self
            .limits
            .timeout_ms
            .map(|ms| Instant::now() + Duration::from_millis(ms));
        *self.deadline.at.lock().unwrap() = at;
    }

    fn disarm(&self) {
        *self.deadline.at.lock().unwrap() = None;
    }
}

fn do_http(host: &SharedHost, req_json: &str) -> String {
    let parsed: Result<HostRequest, _> = serde_json::from_str(req_json);
    let req = match parsed {
        Ok(r) => r,
        Err(e) => {
            return serde_json::json!({ "ok": false, "error": format!("bad request object: {e}") })
                .to_string()
        }
    };
    match host.send_request(req) {
        Ok(res) => serde_json::json!({ "ok": true, "res": res }).to_string(),
        Err(e) => serde_json::json!({ "ok": false, "error": e }).to_string(),
    }
}

fn do_grpc(host: &SharedHost, req_json: &str) -> String {
    let parsed: Result<crate::host::HostGrpcRequest, _> = serde_json::from_str(req_json);
    let req = match parsed {
        Ok(r) => r,
        Err(e) => {
            return serde_json::json!({ "ok": false, "error": format!("bad gRPC request: {e}") })
                .to_string()
        }
    };
    match host.send_grpc(req) {
        Ok(res) => serde_json::json!({ "ok": true, "res": res }).to_string(),
        Err(e) => serde_json::json!({ "ok": false, "error": e }).to_string(),
    }
}

/// The WHATWG "forgiving-base64 decode" that `atob` uses: ASCII whitespace is
/// ignored and padding is optional, but anything else invalid is rejected.
fn forgiving_b64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::engine::{general_purpose, DecodePaddingMode, GeneralPurpose};

    const FORGIVING: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        general_purpose::GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );

    let compact: String = s
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\x0C' | '\r' | ' '))
        .collect();
    FORGIVING.decode(compact.as_bytes()).ok()
}

fn js_err(e: rquickjs::Error) -> ScriptError {
    ScriptError::Engine(e.to_string())
}

/// Format the pending exception, including its stack when there is one.
pub fn fmt_exception(ctx: &Ctx<'_>) -> String {
    let v = ctx.catch();
    if let Some(exc) = v.as_exception() {
        let msg = exc.message().unwrap_or_else(|| "error".to_string());
        match exc.stack() {
            Some(stack) if !stack.trim().is_empty() => format!("{msg}\n{stack}"),
            _ => msg,
        }
    } else if let Some(s) = v.as_string() {
        s.to_string().unwrap_or_else(|_| "error".to_string())
    } else {
        format!("{v:?}")
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
