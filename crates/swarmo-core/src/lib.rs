//! Swarmo core: data model, workspace file store, variable interpolation,
//! request resolution, and Postman import.
//!
//! This crate has no Tauri, HTTP, or UI dependencies — it must build and test
//! standalone.

pub mod curl_import;
pub mod error;
pub mod generators;
pub mod interp;
pub mod model;
pub mod model_grpc;
pub mod model_ws;
pub mod postman;
pub mod report;
pub mod resolve;
pub mod store;

pub use error::{CoreError, Result};
pub use interp::{interpolate, interpolate_str, referenced_vars, VarScope};
pub use model::*;
pub use model_grpc::{
    finalize_grpc, merge_grpc_chain, GrpcRequestDef, GrpcSettings, MergedGrpcRequest, ProtoSource,
    ResolvedGrpcRequest,
};
pub use model_ws::{
    finalize_ws, merge_ws_chain, MergedWsRequest, ResolvedWsMessage, ResolvedWsRequest,
    WsMessageDef, WsPayloadKind, WsRequestDef, WsSettings, WsWait,
};
pub use resolve::{
    finalize, merge_chain, MergedRequest, ResolvedBody, ResolvedPart, ResolvedRequest,
};
pub use store::{ImportReport, LoadTestEntry, LoadTestKind, Protocol, WorkspaceStore};

/// Milliseconds since the Unix epoch.
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
