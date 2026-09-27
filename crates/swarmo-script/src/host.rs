//! The capability boundary between scripts and the rest of the app.
//!
//! Scripts get no filesystem, process, or environment access. The only host
//! capabilities are the ones on this trait, which the embedder supplies.
//!
//! Every method is synchronous and may block the calling (script) thread. The
//! script thread is never a tokio worker, so implementers are free to
//! `block_on` their runtime.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    /// Metric tag; used by the load engine, ignored by request scripts.
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub duration_ms: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostGrpcRequest {
    pub address: String,
    pub service: String,
    pub method: String,
    pub metadata: Vec<(String, String)>,
    /// The request message as protobuf-JSON text.
    pub message_json: String,
    /// Metric tag; defaults to `service/method` when absent.
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostGrpcResponse {
    pub code: u16,
    pub code_name: String,
    pub status_message: String,
    pub body: String,
    pub headers: Vec<(String, String)>,
    pub trailers: Vec<(String, String)>,
    pub duration_ms: f64,
}

pub trait ScriptHost: Send + Sync {
    /// Perform an HTTP request, blocking until it completes.
    fn send_request(&self, req: HostRequest) -> Result<HostResponse, String>;

    /// Perform a unary gRPC call, blocking until it completes.
    ///
    /// Defaulted so hosts that do not speak gRPC (request scripts, tests) keep
    /// compiling and fail with a clear message rather than silently doing
    /// nothing.
    fn send_grpc(&self, _req: HostGrpcRequest) -> Result<HostGrpcResponse, String> {
        Err("gRPC is not available in this script context".to_string())
    }

    /// Block the script thread for `ms` milliseconds.
    fn sleep(&self, ms: u64);

    /// Interpolate `{{vars}}` using the host's current variable scope.
    fn interpolate(&self, text: &str) -> String {
        text.to_string()
    }

    /// True once the run is cancelled; scripts are torn down promptly after.
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// A host with no capabilities, for tests and for scripts that must not do I/O.
pub struct NullHost;

impl ScriptHost for NullHost {
    fn send_request(&self, _req: HostRequest) -> Result<HostResponse, String> {
        Err("HTTP is not available in this script context".to_string())
    }
    fn sleep(&self, ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms.min(5_000)));
    }
}

pub type SharedHost = Arc<dyn ScriptHost>;
