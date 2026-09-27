//! gRPC execution for Swarmo.
//!
//! Unary and streaming calls, against schemas discovered at runtime — either by
//! compiling `.proto` files with protox or by asking the server over the
//! reflection service. Nothing here requires a `protoc` binary, and no code is
//! generated for the services being called.

pub mod call;
pub mod channel;
pub mod codec;
pub mod descriptors;
pub mod error;
pub mod reflection;

pub use call::{
    call, call_unary, call_unary_with, call_with, code_name, prepare_message, CallKind, GrpcResult,
};
pub use channel::ChannelPool;
pub use descriptors::{
    load_from_directory, load_from_files, resolve_path, scan_protos, DescriptorSource, MethodInfo,
    ServiceInfo,
};
pub use error::{GrpcError, Result};
pub use reflection::load_via_reflection;

use std::path::{Path, PathBuf};

use swarmo_core::model_grpc::ProtoSource;

/// Load a schema for a request's configured source.
///
/// `workspace_root` resolves relative `.proto` paths; reflection ignores it.
/// `metadata` is only used by the reflection path, where the schema fetch is
/// itself an RPC the server may require auth for.
pub async fn load_descriptors(
    source: &ProtoSource,
    address: &str,
    verify_tls: bool,
    workspace_root: &Path,
    metadata: &[(String, String)],
) -> Result<DescriptorSource> {
    match source {
        ProtoSource::Reflection => load_via_reflection(address, verify_tls, metadata).await,
        ProtoSource::Directory { root, entry_files } => {
            let root = resolve_path(root, workspace_root);
            descriptors::load_from_directory(&root, entry_files)
        }
        ProtoSource::Files {
            files,
            include_paths,
        } => {
            let files: Vec<PathBuf> = files
                .iter()
                .filter(|f| !f.trim().is_empty())
                .map(|f| resolve_path(f, workspace_root))
                .collect();
            let includes: Vec<PathBuf> = include_paths
                .iter()
                .filter(|p| !p.trim().is_empty())
                .map(|p| resolve_path(p, workspace_root))
                .collect();
            load_from_files(&files, &includes)
        }
    }
}

/// A stable cache key for a proto source, so descriptors are compiled once.
///
/// File sources include modification times, so editing a `.proto` invalidates
/// the cache without the user having to think about it.
pub fn cache_key(source: &ProtoSource, address: &str, workspace_root: &Path) -> String {
    match source {
        ProtoSource::Reflection => format!("reflection:{address}"),
        ProtoSource::Directory { root, entry_files } => {
            // Scanning the tree is what makes editing any file in it invalidate
            // the cache; a failed scan still yields a stable key so the error
            // is reported once rather than on a loop.
            let dir = resolve_path(root, workspace_root);
            let stamp = descriptors::scan_protos(&dir)
                .map(|files| {
                    let newest = files
                        .iter()
                        .filter_map(|f| std::fs::metadata(dir.join(f)).ok())
                        .filter_map(|m| m.modified().ok())
                        .filter_map(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis())
                        .max()
                        .unwrap_or(0);
                    format!("{}@{newest}", files.len())
                })
                .unwrap_or_else(|_| "unreadable".to_string());
            format!("dir:{}|{stamp}|{}", dir.display(), entry_files.join(","))
        }
        ProtoSource::Files {
            files,
            include_paths,
        } => {
            let mut parts: Vec<String> = Vec::new();
            for f in files {
                let p = resolve_path(f, workspace_root);
                let stamp = std::fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis().to_string())
                    .unwrap_or_else(|| "missing".into());
                parts.push(format!("{}@{stamp}", p.display()));
            }
            for i in include_paths {
                parts.push(format!("-I{}", resolve_path(i, workspace_root).display()));
            }
            format!("files:{}", parts.join("|"))
        }
    }
}
