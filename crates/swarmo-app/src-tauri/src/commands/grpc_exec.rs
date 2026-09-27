//! Sending a gRPC call from the client UI, including its scripts.
//!
//! Request scripts are protocol-neutral JavaScript, so rather than growing a
//! second script API this maps gRPC onto the same `ScriptRequest` /
//! `ScriptResponse` shapes the HTTP path uses. That means `sw.*` and every
//! imported `pm.*` script keeps working, with `sw.response.status` carrying the
//! gRPC status code.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use swarmo_core::model_grpc::{MergedGrpcRequest, ResolvedGrpcRequest};
use swarmo_core::{now_millis, KeyValue};
use swarmo_grpc::{DescriptorSource, GrpcResult};
use swarmo_script::{ConsoleLine, ScriptRequest, ScriptResponse, TestResult};
use tauri::State;
use tokio_util::sync::CancellationToken;

use crate::state::{lock_err, AppState};
use swarmo_core::model::{Auth, HistoryEntry, HistoryResponse, HistorySentRequest};
use swarmo_core::store::Protocol;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------
// Mapping gRPC onto the shared script shapes
// ---------------------------------------------------------------------------

/// Replace a `CommandToken` auth with the metadata entry it resolves to.
async fn materialize_command_auth_grpc(
    state: &AppState,
    merged: &mut swarmo_core::MergedGrpcRequest,
) -> Result<Option<(String, u64)>, String> {
    let Auth::CommandToken { command, .. } = &merged.auth else {
        return Ok(None);
    };
    let command = command.clone();
    let token = state.auth_token(&command, None).await?;
    apply_command_token_grpc(merged, &token.value);
    Ok(Some((command, token.epoch)))
}

fn apply_command_token_grpc(merged: &mut swarmo_core::MergedGrpcRequest, token: &str) {
    if let Auth::CommandToken {
        header_name,
        prefix,
        ..
    } = &merged.auth
    {
        let (header_name, prefix) = (header_name.clone(), prefix.clone());
        merged.auth = Auth::ApiKeyHeader {
            header_name,
            value: format!("{prefix}{token}"),
        };
    }
}

/// `address/service/method`, the URL-shaped view scripts see.
pub fn script_url(address: &str, service: &str, method: &str) -> String {
    format!("{}/{}/{}", address.trim_end_matches('/'), service, method)
}

/// Split a script-mutated URL back into its parts.
///
/// Scripts are allowed to rewrite `sw.request.url`; anything that no longer
/// has an address, a service and a method is rejected rather than guessed at.
pub fn parse_script_url(url: &str) -> Result<(String, String, String), String> {
    let bad = || {
        format!(
            "A script rewrote the request URL to \"{url}\", which is not \
             \"address/package.Service/Method\"."
        )
    };

    let trimmed = url.trim().trim_end_matches('/');

    // Split the scheme off first: its "//" must not be mistaken for a path
    // separator, or "http://host/Method" would parse as a valid service.
    let (scheme, rest) = match trimmed.find("://") {
        Some(i) => (&trimmed[..i + 3], &trimmed[i + 3..]),
        None => ("", trimmed),
    };

    let mut segments = rest.split('/');
    let authority = segments.next().unwrap_or_default();
    let path: Vec<&str> = segments.filter(|s| !s.is_empty()).collect();

    if authority.is_empty() || path.len() != 2 {
        return Err(bad());
    }
    let (service, method) = (path[0], path[1]);
    if service.is_empty() || method.is_empty() {
        return Err(bad());
    }

    Ok((
        format!("{scheme}{authority}"),
        service.to_string(),
        method.to_string(),
    ))
}

fn to_script_request(r: &ResolvedGrpcRequest) -> ScriptRequest {
    ScriptRequest {
        name: r.name.clone(),
        method: "GRPC".to_string(),
        url: script_url(&r.address, &r.service, &r.method),
        headers: r.metadata.clone(),
        body: r.message_json.clone(),
    }
}

fn to_script_response(res: &GrpcResult) -> ScriptResponse {
    // Trailers are namespaced so they cannot silently shadow initial metadata.
    let mut headers = res.headers.clone();
    for (k, v) in &res.trailers {
        headers.push((format!("trailer-{k}"), v.clone()));
    }
    ScriptResponse {
        status: res.code,
        status_text: res.code_name.clone(),
        headers,
        body: res.response_raw_json.clone(),
        duration_ms: res.duration_ms,
    }
}

/// Fold a pre-request script's edits back into the request it is about to send.
///
/// Applied to the *merged* request rather than the resolved one, so edits land
/// before interpolation and a script can set a variable the address or message
/// then uses — the same ordering the HTTP path has.
fn apply_script_request(
    merged: &mut MergedGrpcRequest,
    edited: ScriptRequest,
) -> Result<(), String> {
    let (address, service, method) = parse_script_url(&edited.url)?;
    merged.address = address;
    merged.service = service;
    merged.method = method;
    merged.metadata = edited
        .headers
        .into_iter()
        .map(|(k, v)| KeyValue::new(k, v))
        .collect();
    merged.message = edited.body;
    Ok(())
}

// ---------------------------------------------------------------------------
// Command surface
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentGrpcRequest {
    pub address: String,
    pub service: String,
    pub method: String,
    pub metadata: Vec<(String, String)>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrpcSendResult {
    pub exec_id: String,
    pub request_ref: String,
    pub response: Option<GrpcResult>,
    pub error: Option<String>,
    pub script_error: Option<String>,
    pub tests: Vec<TestResult>,
    pub console: Vec<ConsoleLine>,
    pub unresolved: Vec<String>,
    pub sent: SentGrpcRequest,
    pub variables_set: HashMap<String, String>,
}

#[tauri::command]
pub async fn grpc_request_get(
    state: State<'_, AppState>,
    node_ref: String,
) -> Result<swarmo_core::GrpcRequestDef, String> {
    let store = state.store_clone()?;
    store.get_grpc_request(&node_ref).map_err(err)
}

#[tauri::command]
pub async fn grpc_request_save(
    state: State<'_, AppState>,
    node_ref: String,
    def: swarmo_core::GrpcRequestDef,
) -> Result<(), String> {
    let store = state.store_clone()?;
    store.save_grpc_request(&node_ref, &def).map_err(err)
}

#[tauri::command]
pub async fn grpc_request_create(
    state: State<'_, AppState>,
    parent_ref: String,
    name: String,
) -> Result<String, String> {
    let store = state.store_clone()?;
    store.create_grpc_request(&parent_ref, &name).map_err(err)
}

/// Load (or reuse) the schema for a request and list what it offers.
///
/// `def` is the editor's current state rather than what is on disk, so
/// switching the proto source or fixing an address takes effect without having
/// to save first.
#[tauri::command]
pub async fn grpc_list_services(
    state: State<'_, AppState>,
    node_ref: String,
    def: swarmo_core::GrpcRequestDef,
    refresh: bool,
) -> Result<Vec<swarmo_grpc::ServiceInfo>, String> {
    let source = descriptors_for(&state, &node_ref, &def, refresh).await?;
    Ok(source.services())
}

/// A skeleton request message for the selected method.
#[tauri::command]
pub async fn grpc_message_template(
    state: State<'_, AppState>,
    node_ref: String,
    def: swarmo_core::GrpcRequestDef,
    service: String,
    method: String,
) -> Result<String, String> {
    let source = descriptors_for(&state, &node_ref, &def, false).await?;
    source.message_template(&service, &method).map_err(err)
}

/// Resolve a request's schema, using the cache unless `refresh` is set.
async fn descriptors_for(
    state: &State<'_, AppState>,
    node_ref: &str,
    def: &swarmo_core::GrpcRequestDef,
    refresh: bool,
) -> Result<Arc<DescriptorSource>, String> {
    let store = state.store_clone()?;

    // Inheritance still comes from the saved tree (auth on the enclosing
    // collection), but the request itself is whatever the editor currently has.
    let ancestors = store.ancestors_of(node_ref).unwrap_or_default();
    let mut merged = swarmo_core::merge_grpc_chain(&ancestors, def);

    let mut scope = store.var_scope(None).map_err(err)?;
    {
        let runtime = state.runtime_vars.lock().map_err(|_| lock_err())?;
        for (k, v) in runtime.iter() {
            scope.set(k.clone(), v.clone());
        }
    }
    let mut resolved = swarmo_core::finalize_grpc(&merged, &scope);

    if !refresh {
        if let Some(hit) = cached_schema(state, store.root(), &resolved)? {
            return Ok(hit);
        }
    }

    // A command-sourced token only exists once its command has run, and only
    // a reflection fetch needs it — a `.proto` on disk needs no credentials,
    // so an unapproved command must not stop one from loading.
    if matches!(resolved.proto_source, swarmo_core::ProtoSource::Reflection)
        && materialize_command_auth_grpc(state, &mut merged)
            .await?
            .is_some()
    {
        resolved = swarmo_core::finalize_grpc(&merged, &scope);
    }

    fetch_schema(state, store.root(), &resolved).await
}

/// A previously loaded schema for this request's proto source, if any.
fn cached_schema(
    state: &AppState,
    root: &std::path::Path,
    resolved: &ResolvedGrpcRequest,
) -> Result<Option<Arc<DescriptorSource>>, String> {
    let key = swarmo_grpc::cache_key(&resolved.proto_source, &resolved.address, root);
    Ok(state
        .descriptors
        .lock()
        .map_err(|_| lock_err())?
        .get(&key)
        .cloned())
}

/// Load a request's schema and cache it.
///
/// `resolved` must be fully resolved, command-sourced auth included.
async fn fetch_schema(
    state: &AppState,
    root: &std::path::Path,
    resolved: &ResolvedGrpcRequest,
) -> Result<Arc<DescriptorSource>, String> {
    let loaded = swarmo_grpc::load_descriptors(
        &resolved.proto_source,
        &resolved.address,
        resolved.settings.verify_tls,
        root,
        // Reflection is itself an RPC, so an authenticated server needs the
        // request's metadata — including whatever its auth produced — or the
        // schema fetch comes back denied.
        &resolved.metadata,
    )
    .await
    .map_err(err)?;

    let loaded = Arc::new(loaded);
    let key = swarmo_grpc::cache_key(&resolved.proto_source, &resolved.address, root);
    state
        .descriptors
        .lock()
        .map_err(|_| lock_err())?
        .insert(key, loaded.clone());
    Ok(loaded)
}

#[tauri::command]
pub async fn grpc_send(
    state: State<'_, AppState>,
    node_ref: String,
    exec_id: String,
) -> Result<GrpcSendResult, String> {
    let cancel = CancellationToken::new();
    state
        .in_flight
        .lock()
        .map_err(|_| lock_err())?
        .insert(exec_id.clone(), cancel.clone());

    let result = send_inner(&state, &node_ref, &exec_id, cancel).await;

    state
        .in_flight
        .lock()
        .map_err(|_| lock_err())?
        .remove(&exec_id);
    result
}

async fn send_inner(
    state: &State<'_, AppState>,
    node_ref: &str,
    exec_id: &str,
    cancel: CancellationToken,
) -> Result<GrpcSendResult, String> {
    let store = state.store_clone()?;
    let settings = state.settings_snapshot();
    let merged: MergedGrpcRequest = store.merged_grpc_request(node_ref).map_err(err)?;

    let mut scope = store.var_scope(None).map_err(err)?;
    {
        let runtime = state.runtime_vars.lock().map_err(|_| lock_err())?;
        for (k, v) in runtime.iter() {
            scope.set(k.clone(), v.clone());
        }
    }
    let mut base_vars: HashMap<String, String> = scope.flatten();

    let mut console: Vec<ConsoleLine> = Vec::new();
    let mut tests: Vec<TestResult> = Vec::new();
    let mut script_error: Option<String> = None;
    let mut variables_set: HashMap<String, String> = HashMap::new();

    // -- pre-request scripts ------------------------------------------------
    // Run before interpolation, like the HTTP path, so a script can set a
    // variable the address or message then uses.
    let mut working = merged.clone();
    if !merged.pre_scripts.is_empty() {
        let preview = ResolvedGrpcRequest {
            name: merged.name.clone(),
            address: merged.address.clone(),
            proto_source: merged.proto_source.clone(),
            service: merged.service.clone(),
            method: merged.method.clone(),
            metadata: merged
                .metadata
                .iter()
                .filter(|m| m.enabled)
                .map(|m| (m.key.clone(), m.value.clone()))
                .collect(),
            message_json: merged.message.clone(),
            settings: merged.settings.clone(),
            unresolved: Vec::new(),
        };

        let outcome = crate::commands::exec::run_scripts_for(
            state,
            &settings,
            scope.clone(),
            merged.pre_scripts.clone(),
            to_script_request(&preview),
            None,
            &base_vars,
            cancel.clone(),
        )
        .await?;

        console.extend(outcome.console.iter().cloned());
        tests.extend(outcome.tests.iter().cloned());
        if let Some(e) = &outcome.error {
            script_error = Some(format!("Pre-request script: {e}"));
        }
        for (k, v) in &outcome.vars {
            base_vars.insert(k.clone(), v.clone());
            scope.set(k.clone(), v.clone());
            variables_set.insert(k.clone(), v.clone());
        }

        if let Some(edited) = outcome.request {
            apply_script_request(&mut working, edited)?;
        }
    }

    // -- resolve and call ---------------------------------------------------
    // Same as HTTP: a command-sourced token is resolved before finalizing, so
    // the core resolver never has to run a process.
    let mut token = materialize_command_auth_grpc(state, &mut working).await?;

    let resolved = swarmo_core::finalize_grpc(&working, &scope);
    let sent = SentGrpcRequest {
        address: resolved.address.clone(),
        service: resolved.service.clone(),
        method: resolved.method.clone(),
        metadata: resolved.metadata.clone(),
        message: resolved.message_json.clone(),
    };
    let unresolved = resolved.unresolved.clone();

    // The schema comes from the request as it will be sent — after scripts,
    // variables and command auth — not from the file: a script may have
    // pointed it at another address, and reflection needs the same
    // credentials the call does.
    let descriptors = async {
        match cached_schema(state, store.root(), &resolved)? {
            Some(hit) => Ok(hit),
            None => fetch_schema(state, store.root(), &resolved).await,
        }
    };
    let descriptors = tokio::select! {
        r = descriptors => r,
        _ = cancel.cancelled() => Err("cancelled".to_string()),
    };

    // Recorded whatever happens, so a call that never reached the server —
    // a schema that would not load, say — still leaves a trace of what was
    // attempted.
    let mut entry = HistoryEntry {
        id: exec_id.to_string(),
        request_ref: node_ref.to_string(),
        request_id: Some(working.id.clone()),
        name: resolved.name.clone(),
        protocol: Protocol::Grpc,
        method: "GRPC".into(),
        url: script_url(&resolved.address, &resolved.service, &resolved.method),
        status: 0,
        status_text: None,
        duration_ms: 0.0,
        response_bytes: 0,
        at: now_millis(),
        ok: false,
        error: None,
        response: HistoryResponse::default(),
        request: HistorySentRequest::new(
            resolved.metadata.clone(),
            Some(resolved.message_json.clone()),
        ),
    };

    let (response, error) = match descriptors {
        Err(e) => {
            entry.error = Some(e.clone());
            state.push_history(entry);
            (None, Some(e))
        }
        Ok(source) => {
            let call = swarmo_grpc::call_unary(&state.grpc_channels, &source, &resolved, 0);
            let outcome = tokio::select! {
                r = call => r,
                _ = cancel.cancelled() => Err(swarmo_grpc::GrpcError::invalid("cancelled")),
            };

            // UNAUTHENTICATED and PERMISSION_DENIED are gRPC's 401 and 403.
            // Refresh once and try again; only once, so a genuine permission
            // failure cannot loop.
            let outcome = match (&outcome, token.take()) {
                (Ok(res), Some((command, epoch)))
                    if (res.code == 16 || res.code == 7) && !cancel.is_cancelled() =>
                {
                    match state.auth_token(&command, Some(epoch)).await {
                        Ok(fresh) => {
                            apply_command_token_grpc(&mut working, &fresh.value);
                            let retried = swarmo_core::finalize_grpc(&working, &scope);
                            let call =
                                swarmo_grpc::call_unary(&state.grpc_channels, &source, &retried, 0);
                            tokio::select! {
                                r = call => r,
                                _ = cancel.cancelled() => {
                                    Err(swarmo_grpc::GrpcError::invalid("cancelled"))
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!("auth token refresh failed: {e}");
                            outcome
                        }
                    }
                }
                _ => outcome,
            };

            match outcome {
                Ok(res) => {
                    entry.status = res.code;
                    entry.status_text = Some(swarmo_grpc::code_name(res.code).to_string());
                    entry.duration_ms = res.duration_ms;
                    entry.ok = res.ok();
                    // Trailers are namespaced the same way the script runtime
                    // does it, so a trailer can never shadow a header.
                    let mut headers = res.headers.clone();
                    headers.extend(
                        res.trailers
                            .iter()
                            .map(|(k, v)| (format!("trailer-{k}"), v.clone())),
                    );
                    entry.response =
                        HistoryResponse::new(headers, Some(res.response_raw_json.clone()));
                    state.push_history(entry);
                    (Some(res), None)
                }
                Err(e) => {
                    // A stale cached schema is the most common cause; drop it so
                    // the next attempt refetches.
                    if e.is_schema_stale() {
                        if let Ok(mut cache) = state.descriptors.lock() {
                            cache.clear();
                        }
                    }
                    let msg = e.to_string();
                    entry.error = Some(msg.clone());
                    state.push_history(entry);
                    (None, Some(msg))
                }
            }
        }
    };

    // -- post-response scripts ----------------------------------------------
    if let (Some(res), false) = (&response, working.post_scripts.is_empty()) {
        let outcome = crate::commands::exec::run_scripts_for(
            state,
            &settings,
            scope.clone(),
            working.post_scripts.clone(),
            to_script_request(&resolved),
            Some(to_script_response(res)),
            &base_vars,
            cancel.clone(),
        )
        .await?;

        console.extend(outcome.console.iter().cloned());
        tests.extend(outcome.tests.iter().cloned());
        if let Some(e) = &outcome.error {
            let msg = format!("Post-response script: {e}");
            script_error = Some(match script_error {
                Some(prev) => format!("{prev}\n{msg}"),
                None => msg,
            });
        }
        for (k, v) in &outcome.vars {
            variables_set.insert(k.clone(), v.clone());
        }
    }

    if !variables_set.is_empty() {
        let mut runtime = state.runtime_vars.lock().map_err(|_| lock_err())?;
        for (k, v) in &variables_set {
            runtime.insert(k.clone(), v.clone());
        }
    }

    Ok(GrpcSendResult {
        exec_id: exec_id.to_string(),
        request_ref: node_ref.to_string(),
        response,
        error,
        script_error,
        tests,
        console,
        unresolved,
        sent,
        variables_set,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_round_trips() {
        let url = script_url("http://localhost:50051", "pkg.Svc", "Method");
        assert_eq!(url, "http://localhost:50051/pkg.Svc/Method");
        let (a, s, m) = parse_script_url(&url).unwrap();
        assert_eq!(a, "http://localhost:50051");
        assert_eq!(s, "pkg.Svc");
        assert_eq!(m, "Method");
    }

    #[test]
    fn a_script_may_switch_the_method() {
        let (a, s, m) =
            parse_script_url("https://api.test:443/orders.v1.OrderService/CancelOrder").unwrap();
        assert_eq!(a, "https://api.test:443");
        assert_eq!(s, "orders.v1.OrderService");
        assert_eq!(m, "CancelOrder");
    }

    #[test]
    fn a_mangled_url_is_rejected_rather_than_guessed() {
        assert!(parse_script_url("nonsense").is_err());
        // The scheme's "//" must not be read as a path separator.
        assert!(parse_script_url("http://x/OnlyOnePart").is_err());
        assert!(parse_script_url("http://x").is_err());
        assert!(parse_script_url("http://x/a/b/c").is_err());
        assert!(parse_script_url("").is_err());
    }

    #[test]
    fn a_scheme_less_address_still_parses() {
        let (a, s, m) = parse_script_url("localhost:50051/pkg.Svc/M").unwrap();
        assert_eq!(
            (a.as_str(), s.as_str(), m.as_str()),
            ("localhost:50051", "pkg.Svc", "M")
        );
    }

    #[test]
    fn a_trailing_slash_is_tolerated() {
        let (a, s, m) = parse_script_url("http://x:1/pkg.Svc/M/").unwrap();
        assert_eq!(
            (a.as_str(), s.as_str(), m.as_str()),
            ("http://x:1", "pkg.Svc", "M")
        );
    }

    fn result(code: u16, body: &str) -> GrpcResult {
        GrpcResult {
            kind: "unary".into(),
            messages: vec![],
            message_count: 0,
            first_message_ms: None,
            code,
            code_name: swarmo_grpc::code_name(code).to_string(),
            status_message: "why".into(),
            response_json: String::new(),
            response_raw_json: body.to_string(),
            headers: vec![("x-a".into(), "1".into())],
            trailers: vec![("x-b".into(), "2".into())],
            duration_ms: 12.0,
            response_bytes: body.len() as u64,
            request_bytes: 0,
        }
    }

    #[test]
    fn the_script_response_carries_the_grpc_code() {
        let s = to_script_response(&result(5, r#"{"a":1}"#));
        assert_eq!(s.status, 5);
        assert_eq!(s.status_text, "NOT_FOUND");
        assert_eq!(s.body, r#"{"a":1}"#);
    }

    #[test]
    fn trailers_are_namespaced_so_they_cannot_shadow_headers() {
        let s = to_script_response(&result(0, "{}"));
        assert!(s.headers.iter().any(|(k, _)| k == "x-a"));
        assert!(s.headers.iter().any(|(k, _)| k == "trailer-x-b"));
    }

    fn merged() -> MergedGrpcRequest {
        MergedGrpcRequest {
            id: String::new(),
            name: "n".into(),
            address: "http://a:1".into(),
            proto_source: swarmo_core::ProtoSource::Reflection,
            service: "pkg.Svc".into(),
            method: "M".into(),
            metadata: vec![],
            auth: swarmo_core::Auth::None,
            message: "{}".into(),
            settings: Default::default(),
            pre_scripts: vec![],
            post_scripts: vec![],
        }
    }

    #[test]
    fn script_edits_fold_back_into_the_request() {
        let mut m = merged();
        apply_script_request(
            &mut m,
            ScriptRequest {
                name: "n".into(),
                method: "GRPC".into(),
                url: "http://b:2/pkg.Svc/Other".into(),
                headers: vec![("Authorization".into(), "Bearer t".into())],
                body: r#"{"x":1}"#.into(),
            },
        )
        .unwrap();

        assert_eq!(m.address, "http://b:2");
        assert_eq!(m.method, "Other");
        assert_eq!(m.metadata[0].key, "Authorization");
        assert_eq!(m.message, r#"{"x":1}"#);

        // Metadata keys are lowercased when the request is finalized, as gRPC
        // requires — not when the script writes them.
        let resolved = swarmo_core::finalize_grpc(&m, &swarmo_core::VarScope::new());
        assert_eq!(resolved.metadata[0].0, "authorization");
    }

    #[test]
    fn a_script_cannot_mangle_the_request_into_something_unsendable() {
        let mut m = merged();
        let err = apply_script_request(
            &mut m,
            ScriptRequest {
                name: "n".into(),
                method: "GRPC".into(),
                url: "not-a-grpc-target".into(),
                headers: vec![],
                body: "{}".into(),
            },
        )
        .unwrap_err();
        assert!(err.contains("package.Service/Method"), "{err}");
        // The request is left untouched rather than half-applied.
        assert_eq!(m.address, "http://a:1");
        assert_eq!(m.method, "M");
    }
}
