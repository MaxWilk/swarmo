//! Serde types for gRPC requests (`*.grpc.json`). See docs/formats.md.
//!
//! Deliberately free of any gRPC dependency: this crate must keep building
//! without tonic so the store and the load planner can handle gRPC requests
//! without linking a gRPC stack.

use serde::{Deserialize, Serialize};

use base64::Engine as _;

use crate::interp::{interpolate, Unresolved, VarScope};
use crate::model::{Auth, ContainerDef, KeyValue, Scripts, FORMAT_VERSION};

fn default_version() -> u32 {
    FORMAT_VERSION
}
fn default_true() -> bool {
    true
}
fn default_timeout_ms() -> u64 {
    30_000
}
fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ---------------------------------------------------------------------------
// Where the schema comes from
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProtoSource {
    /// Point at one directory and compile every `.proto` beneath it, using that
    /// directory as the single import root.
    ///
    /// This is the easy path for schemas that ship as a tree — TensorFlow
    /// Serving, googleapis, anything whose `import` statements are written
    /// relative to a common root. Pick the folder that directly contains the
    /// first segment of those imports.
    #[serde(rename_all = "camelCase")]
    Directory {
        root: String,
        /// Optionally narrow to specific entry points, still resolved against
        /// `root`. Empty means "compile everything found".
        #[serde(default)]
        entry_files: Vec<String>,
    },
    /// Compile named `.proto` files. Paths may be workspace-relative.
    #[serde(rename_all = "camelCase")]
    Files {
        #[serde(default)]
        files: Vec<String>,
        /// Import roots (`-I`). Defaults to each file's parent directory.
        #[serde(default)]
        include_paths: Vec<String>,
    },
    /// Ask the server for its schema over the gRPC reflection service.
    Reflection,
}

impl Default for ProtoSource {
    fn default() -> Self {
        ProtoSource::Files {
            files: Vec::new(),
            include_paths: Vec::new(),
        }
    }
}

impl ProtoSource {
    pub fn is_reflection(&self) -> bool {
        matches!(self, ProtoSource::Reflection)
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrpcSettings {
    /// Call deadline. Sent as `grpc-timeout` and enforced client-side.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_true")]
    pub verify_tls: bool,
    /// Largest response message to accept.
    ///
    /// gRPC libraries default to 4 MB, which is easy to exceed with real
    /// payloads (an image mask, a batch of embeddings), so Swarmo is more
    /// generous by default and lets you set it per request.
    #[serde(default = "default_max_response_bytes")]
    pub max_response_bytes: u64,
    /// For a server- or bidirectional-streaming method: stop after this many
    /// messages have arrived, closing the stream. `None` reads until the
    /// server ends it (or the deadline passes). A load test against an
    /// unbounded feed needs this, or every iteration runs to its timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_max_messages: Option<u32>,
}

fn default_max_response_bytes() -> u64 {
    16 * 1024 * 1024
}

impl Default for GrpcSettings {
    fn default() -> Self {
        Self {
            timeout_ms: default_timeout_ms(),
            verify_tls: true,
            max_response_bytes: default_max_response_bytes(),
            stream_max_messages: None,
        }
    }
}

// ---------------------------------------------------------------------------
// The request definition
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrpcRequestDef {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "new_uuid")]
    pub id: String,
    pub name: String,
    /// `http://host:port` (plaintext h2c) or `https://host:port` (TLS).
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub proto_source: ProtoSource,
    /// Fully-qualified service name, e.g. `orders.v1.OrderService`.
    #[serde(default)]
    pub service: String,
    #[serde(default)]
    pub method: String,
    /// The gRPC equivalent of headers.
    #[serde(default)]
    pub metadata: Vec<KeyValue>,
    /// Turned into a metadata entry at send time. Inherited from the enclosing
    /// folder or collection when set to `inherit`.
    #[serde(default)]
    pub auth: Auth,
    /// The request message as protobuf-JSON text.
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub scripts: Scripts,
    #[serde(default)]
    pub settings: GrpcSettings,
}

impl GrpcRequestDef {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            version: FORMAT_VERSION,
            id: new_uuid(),
            name: name.into(),
            address: String::new(),
            proto_source: ProtoSource::default(),
            service: String::new(),
            method: String::new(),
            metadata: Vec::new(),
            auth: Auth::Inherit,
            message: "{}".to_string(),
            scripts: Scripts::default(),
            settings: GrpcSettings::default(),
        }
    }

    /// `service/method`, the short form used for metric tags and display.
    pub fn full_method(&self) -> String {
        format!("{}/{}", self.service, self.method)
    }
}

// ---------------------------------------------------------------------------
// Two-phase resolution, mirroring the HTTP path so scripts can run in between
// ---------------------------------------------------------------------------

/// Post-inheritance, pre-interpolation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergedGrpcRequest {
    /// The request's stable id; see [`MergedRequest::id`].
    pub id: String,
    pub name: String,
    pub address: String,
    pub proto_source: ProtoSource,
    pub service: String,
    pub method: String,
    pub metadata: Vec<KeyValue>,
    pub auth: Auth,
    pub message: String,
    pub settings: GrpcSettings,
    /// Outermost first: collection, then folders top-down, then the request.
    pub pre_scripts: Vec<String>,
    /// Innermost first: the request, then folders bottom-up, then collection.
    pub post_scripts: Vec<String>,
}

/// Post-interpolation, ready to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedGrpcRequest {
    pub name: String,
    pub address: String,
    pub proto_source: ProtoSource,
    pub service: String,
    pub method: String,
    pub metadata: Vec<(String, String)>,
    pub message_json: String,
    pub settings: GrpcSettings,
    pub unresolved: Vec<String>,
}

impl ResolvedGrpcRequest {
    pub fn full_method(&self) -> String {
        format!("{}/{}", self.service, self.method)
    }

    /// The HTTP/2 path a gRPC call is addressed to.
    pub fn path(&self) -> String {
        format!("/{}/{}", self.service, self.method)
    }
}

/// Merge the container chain into a gRPC request.
///
/// Scripts and **auth** are inherited, using the same nearest-explicit-wins
/// rule as HTTP: one bearer token on a collection covers every call inside it.
///
/// Plain collection and folder *headers* are deliberately not inherited.
/// Metadata is not headers — it is lowercase-only, has binary-key conventions,
/// and reserves the `grpc-` prefix — so quietly reinterpreting an HTTP header
/// as metadata would be a surprise rather than a convenience.
pub fn merge_grpc_chain(ancestors: &[ContainerDef], req: &GrpcRequestDef) -> MergedGrpcRequest {
    let auth = if !matches!(req.auth, Auth::Inherit) {
        req.auth.clone()
    } else {
        ancestors
            .iter()
            .rev()
            .find(|a| !matches!(a.auth, Auth::Inherit))
            .map(|a| a.auth.clone())
            .unwrap_or(Auth::None)
    };

    let mut pre_scripts: Vec<String> = ancestors
        .iter()
        .filter(|a| !a.scripts.pre_request.trim().is_empty())
        .map(|a| a.scripts.pre_request.clone())
        .collect();
    if !req.scripts.pre_request.trim().is_empty() {
        pre_scripts.push(req.scripts.pre_request.clone());
    }

    let mut post_scripts: Vec<String> = Vec::new();
    if !req.scripts.post_response.trim().is_empty() {
        post_scripts.push(req.scripts.post_response.clone());
    }
    for a in ancestors.iter().rev() {
        if !a.scripts.post_response.trim().is_empty() {
            post_scripts.push(a.scripts.post_response.clone());
        }
    }

    MergedGrpcRequest {
        id: req.id.clone(),
        name: req.name.clone(),
        address: req.address.clone(),
        proto_source: req.proto_source.clone(),
        service: req.service.clone(),
        method: req.method.clone(),
        metadata: req.metadata.clone(),
        auth,
        message: req.message.clone(),
        settings: req.settings.clone(),
        pre_scripts,
        post_scripts,
    }
}

/// Interpolate everything and produce a directly callable request.
pub fn finalize_grpc(merged: &MergedGrpcRequest, scope: &VarScope) -> ResolvedGrpcRequest {
    let mut unresolved: Vec<String> = Vec::new();
    let mut interp = |s: &str| -> String {
        let (out, un) = interpolate(s, scope);
        collect(&mut unresolved, un);
        out
    };

    let mut metadata: Vec<(String, String)> = merged
        .metadata
        .iter()
        .filter(|m| m.enabled && !m.key.trim().is_empty())
        // Trimmed here as well as at send time, so the override check below
        // compares the key that will actually go on the wire.
        .map(|m| (interp(&m.key).trim().to_lowercase(), interp(&m.value)))
        .collect();

    // Auth becomes a metadata entry. An explicit metadata row of the same key
    // wins, so a request can always override what it inherited.
    if let Some((key, value)) = auth_metadata(&merged.auth, &mut interp) {
        if !metadata.iter().any(|(k, _)| *k == key) {
            metadata.push((key, value));
        }
    }

    let proto_source = match &merged.proto_source {
        ProtoSource::Reflection => ProtoSource::Reflection,
        ProtoSource::Directory { root, entry_files } => ProtoSource::Directory {
            root: interp(root),
            entry_files: entry_files.iter().map(|f| interp(f)).collect(),
        },
        ProtoSource::Files {
            files,
            include_paths,
        } => ProtoSource::Files {
            files: files.iter().map(|f| interp(f)).collect(),
            include_paths: include_paths.iter().map(|p| interp(p)).collect(),
        },
    };

    ResolvedGrpcRequest {
        name: merged.name.clone(),
        address: normalize_address(&interp(&merged.address)),
        proto_source,
        service: interp(&merged.service),
        method: interp(&merged.method),
        metadata,
        message_json: interp(&merged.message),
        settings: merged.settings.clone(),
        unresolved,
    }
}

/// Render an auth setting as the metadata entry it becomes on the wire.
///
/// Metadata keys are always lowercase, so `Authorization` and `authorization`
/// are the same entry as far as gRPC is concerned.
fn auth_metadata(auth: &Auth, interp: &mut impl FnMut(&str) -> String) -> Option<(String, String)> {
    match auth {
        Auth::Inherit | Auth::None => None,
        Auth::Bearer { token } => {
            let t = interp(token);
            Some(("authorization".to_string(), format!("Bearer {t}")))
        }
        Auth::Basic { username, password } => {
            let raw = format!("{}:{}", interp(username), interp(password));
            let b64 = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
            Some(("authorization".to_string(), format!("Basic {b64}")))
        }
        Auth::ApiKeyHeader { header_name, value } => {
            let key = interp(header_name).trim().to_lowercase();
            if key.is_empty() {
                return None;
            }
            Some((key, interp(value)))
        }
        // Resolved by the caller before this runs; see the note in resolve.rs.
        Auth::CommandToken { .. } => None,
    }
}

/// A scheme-less address defaults to TLS, matching how the HTTP side treats
/// a bare host.
pub fn normalize_address(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() || t.starts_with("http://") || t.starts_with("https://") {
        t.to_string()
    } else if t.contains("://") {
        // Leave it alone; execution will reject it with a clear message.
        t.to_string()
    } else {
        format!("https://{t}")
    }
}

fn collect(into: &mut Vec<String>, un: Vec<Unresolved>) {
    for u in un {
        if !into.contains(&u.name) {
            into.push(u.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn scope(pairs: &[(&str, &str)]) -> VarScope {
        let mut s = VarScope::new();
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        s.push_layer(m);
        s
    }

    fn def() -> GrpcRequestDef {
        let mut d = GrpcRequestDef::new("Get Order");
        d.address = "{{grpcHost}}".into();
        d.service = "orders.v1.OrderService".into();
        d.method = "GetOrder".into();
        d.metadata = vec![KeyValue::new("Authorization", "Bearer {{token}}")];
        d.message = "{\"order_id\": \"{{orderId}}\"}".into();
        d
    }

    #[test]
    fn round_trips_through_json() {
        let d = def();
        let text = serde_json::to_string_pretty(&d).unwrap();
        let back: GrpcRequestDef = serde_json::from_str(&text).unwrap();
        assert_eq!(back.service, d.service);
        assert_eq!(back.metadata.len(), 1);
        assert!(matches!(back.proto_source, ProtoSource::Files { .. }));
    }

    #[test]
    fn reads_a_minimal_file_leniently() {
        let d: GrpcRequestDef = serde_json::from_str(r#"{"name":"X"}"#).unwrap();
        assert_eq!(d.name, "X");
        assert_eq!(d.settings.timeout_ms, 30_000);
        assert!(d.settings.verify_tls);
        assert_eq!(d.version, FORMAT_VERSION);
    }

    #[test]
    fn proto_source_tagged_both_ways() {
        let r: ProtoSource = serde_json::from_str(r#"{"kind":"reflection"}"#).unwrap();
        assert!(r.is_reflection());
        let f: ProtoSource =
            serde_json::from_str(r#"{"kind":"files","files":["a.proto"]}"#).unwrap();
        match f {
            ProtoSource::Files { files, .. } => assert_eq!(files, vec!["a.proto".to_string()]),
            _ => panic!("expected files"),
        }
    }

    #[test]
    fn interpolates_every_field() {
        let merged = merge_grpc_chain(&[], &def());
        let r = finalize_grpc(
            &merged,
            &scope(&[
                ("grpcHost", "http://127.0.0.1:50051"),
                ("token", "tok"),
                ("orderId", "o-7"),
            ]),
        );
        assert_eq!(r.address, "http://127.0.0.1:50051");
        assert_eq!(r.metadata[0], ("authorization".into(), "Bearer tok".into()));
        assert!(r.message_json.contains("o-7"));
        assert!(r.unresolved.is_empty());
        assert_eq!(r.path(), "/orders.v1.OrderService/GetOrder");
    }

    #[test]
    fn reports_unresolved_variables() {
        let merged = merge_grpc_chain(&[], &def());
        let r = finalize_grpc(&merged, &scope(&[]));
        assert!(r.unresolved.contains(&"grpcHost".to_string()));
        assert!(r.unresolved.contains(&"token".to_string()));
        assert!(r.unresolved.contains(&"orderId".to_string()));
    }

    #[test]
    fn metadata_keys_are_lowercased_and_disabled_rows_dropped() {
        let mut d = def();
        let mut off = KeyValue::new("X-Off", "no");
        off.enabled = false;
        d.metadata = vec![KeyValue::new("X-Trace-Id", "abc"), off];
        let r = finalize_grpc(&merge_grpc_chain(&[], &d), &scope(&[]));
        assert_eq!(r.metadata.len(), 1);
        assert_eq!(r.metadata[0].0, "x-trace-id");
    }

    #[test]
    fn scheme_less_address_defaults_to_tls() {
        assert_eq!(
            normalize_address("api.example.com:443"),
            "https://api.example.com:443"
        );
        assert_eq!(normalize_address("http://x:1"), "http://x:1");
        assert_eq!(normalize_address("https://x:1"), "https://x:1");
        assert_eq!(normalize_address(""), "");
    }

    #[test]
    fn scripts_and_auth_inherit_but_plain_headers_do_not() {
        let mut coll = ContainerDef::new("C");
        coll.scripts = Scripts {
            pre_request: "COLL".into(),
            post_response: "COLL".into(),
        };
        coll.headers = vec![KeyValue::new("X-Http-Only", "1")];
        coll.auth = Auth::Bearer {
            token: "inherited".into(),
        };

        let mut d = def();
        d.scripts = Scripts {
            pre_request: "REQ".into(),
            post_response: "REQ".into(),
        };
        d.metadata.clear();

        let merged = merge_grpc_chain(&[coll], &d);
        assert_eq!(merged.pre_scripts, vec!["COLL", "REQ"]);
        assert_eq!(merged.post_scripts, vec!["REQ", "COLL"]);

        let r = finalize_grpc(&merged, &scope(&[]));
        // Auth is inherited and rendered as metadata...
        assert_eq!(
            r.metadata,
            vec![("authorization".to_string(), "Bearer inherited".to_string())]
        );
        // ...but a plain HTTP header is not.
        assert!(
            !r.metadata.iter().any(|(k, _)| k == "x-http-only"),
            "plain headers must not leak into gRPC metadata: {:?}",
            r.metadata
        );
    }

    #[test]
    fn every_auth_type_renders_as_metadata() {
        let mut d = def();
        d.metadata.clear();

        d.auth = Auth::Bearer {
            token: "{{tok}}".into(),
        };
        let r = finalize_grpc(&merge_grpc_chain(&[], &d), &scope(&[("tok", "abc")]));
        assert_eq!(r.metadata[0], ("authorization".into(), "Bearer abc".into()));

        d.auth = Auth::Basic {
            username: "u".into(),
            password: "p".into(),
        };
        let r = finalize_grpc(&merge_grpc_chain(&[], &d), &scope(&[]));
        assert_eq!(r.metadata[0], ("authorization".into(), "Basic dTpw".into()));

        d.auth = Auth::ApiKeyHeader {
            header_name: "X-Api-Key".into(),
            value: "k1".into(),
        };
        let r = finalize_grpc(&merge_grpc_chain(&[], &d), &scope(&[]));
        // Metadata keys are lowercase on the wire.
        assert_eq!(r.metadata[0], ("x-api-key".into(), "k1".into()));

        d.auth = Auth::None;
        let r = finalize_grpc(&merge_grpc_chain(&[], &d), &scope(&[]));
        assert!(r.metadata.is_empty());
    }

    #[test]
    fn an_explicit_metadata_row_overrides_inherited_auth() {
        let mut coll = ContainerDef::new("C");
        coll.auth = Auth::Bearer {
            token: "inherited".into(),
        };
        let mut d = def();
        d.metadata = vec![KeyValue::new("Authorization", "Bearer explicit")];

        let r = finalize_grpc(&merge_grpc_chain(&[coll], &d), &scope(&[]));
        assert_eq!(r.metadata.len(), 1);
        assert_eq!(r.metadata[0].1, "Bearer explicit");
    }

    #[test]
    fn a_padded_metadata_key_still_overrides_inherited_auth() {
        let mut coll = ContainerDef::new("C");
        coll.auth = Auth::Bearer {
            token: "inherited".into(),
        };
        let mut d = def();
        d.metadata = vec![KeyValue::new(" Authorization ", "Bearer explicit")];

        let r = finalize_grpc(&merge_grpc_chain(&[coll], &d), &scope(&[]));
        assert_eq!(
            r.metadata,
            vec![("authorization".to_string(), "Bearer explicit".to_string())]
        );
    }

    #[test]
    fn explicit_none_stops_auth_inheritance() {
        let mut coll = ContainerDef::new("C");
        coll.auth = Auth::Bearer {
            token: "inherited".into(),
        };
        let mut d = def();
        d.metadata.clear();
        d.auth = Auth::None;

        let r = finalize_grpc(&merge_grpc_chain(&[coll], &d), &scope(&[]));
        assert!(r.metadata.is_empty(), "{:?}", r.metadata);
    }

    #[test]
    fn a_directory_proto_source_round_trips_and_interpolates() {
        let mut d = def();
        d.proto_source = ProtoSource::Directory {
            root: "{{protoRoot}}".into(),
            entry_files: vec!["tensorflow_serving/apis/prediction_service.proto".into()],
        };

        let text = serde_json::to_string(&d).unwrap();
        let back: GrpcRequestDef = serde_json::from_str(&text).unwrap();
        assert!(matches!(back.proto_source, ProtoSource::Directory { .. }));

        let r = finalize_grpc(
            &merge_grpc_chain(&[], &d),
            &scope(&[("protoRoot", "protos")]),
        );
        match r.proto_source {
            ProtoSource::Directory { root, entry_files } => {
                assert_eq!(root, "protos");
                assert_eq!(entry_files.len(), 1);
            }
            other => panic!("expected a directory source, got {other:?}"),
        }
    }

    #[test]
    fn the_response_size_cap_has_a_generous_default() {
        let d: GrpcRequestDef = serde_json::from_str(r#"{"name":"X"}"#).unwrap();
        assert_eq!(d.settings.max_response_bytes, 16 * 1024 * 1024);
    }

    #[test]
    fn full_method_shape() {
        assert_eq!(def().full_method(), "orders.v1.OrderService/GetOrder");
    }
}
