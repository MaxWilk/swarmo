//! Wiring the sandboxed script runtime to the app's HTTP stack.
//!
//! Scripts run on a plain OS thread (QuickJS values are not `Send`) and reach
//! the network by handing a job to the tokio runtime and blocking on the reply.
//! That keeps every request on the shared client pool, so cookies and the proxy
//! setting behave the same whether a request came from the UI or from a script.

use std::sync::mpsc as std_mpsc;
use std::sync::Arc;

use swarmo_core::model::RequestSettings;
use swarmo_core::{ResolvedBody, ResolvedRequest, VarScope};
use swarmo_http::{ClientPool, ExecOpts};
use swarmo_script::{HostRequest, HostResponse, ScriptHost};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub struct ScriptHttpJob {
    pub req: HostRequest,
    pub reply: std_mpsc::Sender<Result<HostResponse, String>>,
}

/// The `ScriptHost` implementation used by request scripts.
pub struct AppScriptHost {
    pub tx: mpsc::UnboundedSender<ScriptHttpJob>,
    pub scope: VarScope,
    /// The send's cancellation token. Without it, Cancel stopped only the
    /// network hop: a pre-request script sleeping or looping kept the Send
    /// button in its "sending" state until the script timeout.
    pub cancel: CancellationToken,
}

impl ScriptHost for AppScriptHost {
    fn send_request(&self, req: HostRequest) -> Result<HostResponse, String> {
        if self.cancel.is_cancelled() {
            return Err("cancelled".to_string());
        }
        let (reply_tx, reply_rx) = std_mpsc::channel();
        self.tx
            .send(ScriptHttpJob {
                req,
                reply: reply_tx,
            })
            .map_err(|_| "the application is shutting down".to_string())?;
        reply_rx
            .recv()
            .map_err(|_| "the request was dropped before it completed".to_string())?
    }

    fn sleep(&self, ms: u64) {
        // Capped so a script cannot wedge the UI's send button indefinitely;
        // the script timeout is the real backstop. Slept in short slices so
        // a cancel lands within one slice rather than after the whole sleep.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms.min(60_000));
        while std::time::Instant::now() < deadline {
            if self.cancel.is_cancelled() {
                return;
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            std::thread::sleep(left.min(std::time::Duration::from_millis(50)));
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    fn interpolate(&self, text: &str) -> String {
        swarmo_core::interpolate_str(text, &self.scope)
    }
}

/// Serve script HTTP jobs until the sender is dropped.
///
/// Each request is raced against the send's cancellation: the script thread
/// blocks on the reply, so an uncancellable request here would hold Cancel
/// off until the server answered or the request timed out.
pub async fn serve_script_http(
    mut rx: mpsc::UnboundedReceiver<ScriptHttpJob>,
    pool: Arc<ClientPool>,
    opts: ExecOpts,
    settings: RequestSettings,
    cancel: CancellationToken,
) {
    while let Some(job) = rx.recv().await {
        let resolved = ResolvedRequest {
            name: "script request".into(),
            method: job.req.method.clone(),
            url: job.req.url.clone(),
            headers: job.req.headers.clone(),
            body: match &job.req.body {
                Some(b) => ResolvedBody::Bytes {
                    content_type: None,
                    text: b.clone(),
                },
                None => ResolvedBody::None,
            },
            settings: settings.clone(),
            unresolved: Vec::new(),
        };

        let executed = tokio::select! {
            r = swarmo_http::execute(&pool, &resolved, &opts) => r,
            _ = cancel.cancelled() => Err(swarmo_http::ExecError::Cancelled),
        };
        let result = match executed {
            Ok(res) => Ok(HostResponse {
                status: res.status,
                status_text: res.status_text,
                headers: res
                    .headers
                    .iter()
                    .map(|h| (h.key.clone(), h.value.clone()))
                    .collect(),
                body: body_text(&res.body),
                duration_ms: res.timings.total_ms,
            }),
            Err(e) => Err(e.to_string()),
        };

        let _ = job.reply.send(result);
    }
}

pub fn body_text(preview: &swarmo_http::BodyPreview) -> String {
    match preview {
        // Scripts get the untouched bytes, never the pretty-printed view.
        swarmo_http::BodyPreview::Text { raw, .. } => raw.clone(),
        swarmo_http::BodyPreview::Empty => String::new(),
        swarmo_http::BodyPreview::Image { .. } => String::new(),
        swarmo_http::BodyPreview::Binary { .. } => String::new(),
        swarmo_http::BodyPreview::File { path, .. } => {
            format!("<body written to {path}>")
        }
    }
}
