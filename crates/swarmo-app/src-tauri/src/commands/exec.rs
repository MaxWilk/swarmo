//! Sending a request: pre-request scripts, execution, post-response scripts.
//!
//! Ordering matters. Pre-request scripts run *before* interpolation so a script
//! can set a variable the URL then uses; post-response scripts see the frozen
//! response. This mirrors what Postman users expect from imported collections.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use swarmo_core::model::RequestSettings;
use swarmo_core::{finalize, now_millis, MergedRequest, VarScope};
use swarmo_http::{ExecOpts, ExecResult};
use swarmo_script::{
    ConsoleLine, Limits, ScriptOutcome, ScriptRequest, ScriptResponse, TestResult,
};
use tauri::State;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::scripting::{body_text, serve_script_http, AppScriptHost, ScriptHttpJob};
use crate::state::{lock_err, AppState};
use swarmo_core::model::{Auth, HistoryEntry, HistoryResponse, HistorySentRequest};
use swarmo_core::store::Protocol;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendResult {
    pub exec_id: String,
    pub request_ref: String,
    /// Present unless the request itself failed.
    pub response: Option<ExecResult>,
    /// A transport or validation failure.
    pub error: Option<String>,
    /// A pre-request or post-response script failure.
    pub script_error: Option<String>,
    pub tests: Vec<TestResult>,
    pub console: Vec<ConsoleLine>,
    /// Variables still unresolved after interpolation.
    pub unresolved: Vec<String>,
    /// What was actually put on the wire, after scripts and interpolation.
    pub sent: SentRequest,
    pub variables_set: HashMap<String, String>,
}

#[tauri::command]
pub async fn request_send(
    state: State<'_, AppState>,
    node_ref: String,
    exec_id: String,
) -> Result<SendResult, String> {
    let settings = state.settings_snapshot();

    // Registered before the store is cloned: the workspace-switch guard
    // checks this set, and a switch landing between the clone and the insert
    // would let this send run against, and record into, the old workspace.
    let cancel = CancellationToken::new();
    state
        .in_flight
        .lock()
        .map_err(|_| lock_err())?
        .insert(exec_id.clone(), cancel.clone());

    let result = match state.store_clone() {
        Ok(store) => send_inner(&state, &store, &node_ref, &exec_id, &settings, cancel).await,
        Err(e) => Err(e),
    };

    state
        .in_flight
        .lock()
        .map_err(|_| lock_err())?
        .remove(&exec_id);
    result
}

/// The URL a send would actually hit, without sending.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedUrl {
    pub url: String,
    /// Variables that had no value, left in place as `{{name}}` in `url`.
    pub unresolved: Vec<String>,
}

/// Resolve a request's URL the way a send would — collection and folder
/// settings merged in, `{{variables}}` filled from the active environment and
/// anything scripts have set this session, enabled query params appended —
/// so what lands on the clipboard is what the server would see, not the
/// template in the address bar.
#[tauri::command]
pub fn request_resolve_url(
    state: State<'_, AppState>,
    node_ref: String,
) -> Result<ResolvedUrl, String> {
    let store = state.store_clone()?;
    let merged: MergedRequest = store.merged_request(&node_ref).map_err(err)?;
    let mut scope = store.var_scope(None).map_err(err)?;
    for (k, v) in state.runtime_vars.lock().map_err(|_| lock_err())?.iter() {
        scope.set(k.clone(), v.clone());
    }
    let resolved = finalize(&merged, &scope);
    Ok(ResolvedUrl {
        url: resolved.url,
        unresolved: resolved.unresolved,
    })
}

#[tauri::command]
pub fn request_cancel(state: State<'_, AppState>, exec_id: String) -> Result<bool, String> {
    let guard = state.in_flight.lock().map_err(|_| lock_err())?;
    match guard.get(&exec_id) {
        Some(token) => {
            token.cancel();
            Ok(true)
        }
        None => Ok(false),
    }
}

async fn send_inner(
    state: &State<'_, AppState>,
    store: &swarmo_core::WorkspaceStore,
    node_ref: &str,
    exec_id: &str,
    settings: &crate::state::Settings,
    cancel: CancellationToken,
) -> Result<SendResult, String> {
    let mut merged: MergedRequest = store.merged_request(node_ref).map_err(err)?;

    // Base scope: environment + this session's script-set variables.
    let mut base_scope = store.var_scope(None).map_err(err)?;
    {
        let runtime = state.runtime_vars.lock().map_err(|_| lock_err())?;
        for (k, v) in runtime.iter() {
            base_scope.set(k.clone(), v.clone());
        }
    }
    let mut base_vars: HashMap<String, String> = base_scope.flatten();

    let opts = ExecOpts {
        proxy: settings.proxy.clone(),
        temp_dir: state.temp_dir.clone(),
    };

    let mut console: Vec<ConsoleLine> = Vec::new();
    let mut tests: Vec<TestResult> = Vec::new();
    let mut script_error: Option<String> = None;
    let mut variables_set: HashMap<String, String> = HashMap::new();

    // -- pre-request scripts ------------------------------------------------
    if !merged.pre_scripts.is_empty() {
        let script_req = ScriptRequest {
            name: merged.name.clone(),
            method: merged.method.clone(),
            url: merged.url.clone(),
            headers: merged
                .headers
                .iter()
                .filter(|h| h.enabled)
                .map(|h| (h.key.clone(), h.value.clone()))
                .collect(),
            body: body_of(&merged),
        };

        let outcome = run_scripts(
            state.pool.clone(),
            opts.clone(),
            merged.settings.clone(),
            base_scope.clone(),
            merged.pre_scripts.clone(),
            script_req,
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
            base_scope.set(k.clone(), v.clone());
            variables_set.insert(k.clone(), v.clone());
        }
        if let Some(r) = outcome.request {
            apply_script_request(&mut merged, r);
        }
    }

    // -- execute ------------------------------------------------------------
    // A command-sourced token is fetched and substituted before resolving, so
    // the core resolver never needs to know about running processes.
    let mut token = materialize_command_auth(state, &mut merged).await?;

    let mut resolved = finalize(&merged, &base_scope);
    let mut exec_result = tokio::select! {
        r = swarmo_http::execute(&state.pool, &resolved, &opts) => r,
        _ = cancel.cancelled() => Err(swarmo_http::ExecError::Cancelled),
    };

    // An expired token is the ordinary case, not an error: refresh once and
    // send again. Only once — a genuine permission failure must not loop.
    if let Some((command, epoch)) = token.take() {
        let rejected = matches!(&exec_result, Ok(r) if r.status == 401 || r.status == 403);
        if rejected && !cancel.is_cancelled() {
            match state.auth_token(&command, Some(epoch)).await {
                Ok(fresh) => {
                    apply_command_token(&mut merged, &fresh.value);
                    resolved = finalize(&merged, &base_scope);
                    exec_result = tokio::select! {
                        r = swarmo_http::execute(&state.pool, &resolved, &opts) => r,
                        _ = cancel.cancelled() => Err(swarmo_http::ExecError::Cancelled),
                    };
                }
                // Keep the original rejection: it says more about what went
                // wrong than "the refresh failed too" would.
                Err(e) => tracing::debug!("auth token refresh failed: {e}"),
            }
        }
    }

    let sent = SentRequest {
        method: resolved.method.clone(),
        url: resolved.url.clone(),
        headers: resolved.headers.clone(),
        body: preview_body(&resolved.body),
    };
    let unresolved = resolved.unresolved.clone();

    // Every send is recorded, including the ones that never got a response:
    // "it failed at 14:02 and here is what I sent" is the case history is
    // most needed for.
    let mut entry = HistoryEntry {
        id: exec_id.to_string(),
        request_ref: node_ref.to_string(),
        request_id: Some(merged.id.clone()),
        name: merged.name.clone(),
        protocol: Protocol::Http,
        method: resolved.method.clone(),
        url: resolved.url.clone(),
        status: 0,
        status_text: None,
        duration_ms: 0.0,
        response_bytes: 0,
        at: now_millis(),
        ok: false,
        error: None,
        response: HistoryResponse::default(),
        request: HistorySentRequest::new(sent.headers.clone(), sent.body.clone()),
    };

    let (response, error) = match exec_result {
        Ok(res) => {
            state.record_cookies(&res.set_cookies);
            entry.status = res.status;
            entry.status_text = Some(res.status_text.clone());
            entry.duration_ms = res.timings.total_ms;
            entry.response_bytes = res.body_size;
            entry.ok = (200..400).contains(&res.status);
            entry.response = HistoryResponse::new(
                res.headers
                    .iter()
                    .map(|h| (h.key.clone(), h.value.clone()))
                    .collect(),
                // Only text is worth keeping verbatim. An image, a temp-file
                // body or raw binary is stated by its size instead of being
                // described in a made-up sentence.
                match &res.body {
                    swarmo_http::BodyPreview::Text { raw, .. } => Some(raw.clone()),
                    _ => None,
                },
            );
            state.push_history(entry);
            (Some(res), None)
        }
        Err(e) => {
            let msg = e.to_string();
            entry.error = Some(msg.clone());
            state.push_history(entry);
            (None, Some(msg))
        }
    };

    // -- post-response scripts ----------------------------------------------
    if let (Some(res), false) = (&response, merged.post_scripts.is_empty()) {
        let script_req = ScriptRequest {
            name: merged.name.clone(),
            method: resolved.method.clone(),
            url: resolved.url.clone(),
            headers: resolved.headers.clone(),
            body: preview_body(&resolved.body).unwrap_or_default(),
        };
        let script_res = ScriptResponse {
            status: res.status,
            status_text: res.status_text.clone(),
            headers: res
                .headers
                .iter()
                .map(|h| (h.key.clone(), h.value.clone()))
                .collect(),
            body: body_text(&res.body),
            duration_ms: res.timings.total_ms,
        };

        let outcome = run_scripts(
            state.pool.clone(),
            opts.clone(),
            merged.settings.clone(),
            base_scope.clone(),
            merged.post_scripts.clone(),
            script_req,
            Some(script_res),
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

    // Persist script-set variables for the rest of the session.
    if !variables_set.is_empty() {
        let mut runtime = state.runtime_vars.lock().map_err(|_| lock_err())?;
        for (k, v) in &variables_set {
            runtime.insert(k.clone(), v.clone());
        }
    }

    Ok(SendResult {
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

/// Run scripts using the app's shared client pool and settings.
///
/// Shared with the gRPC path so both protocols get the same script runtime,
/// the same variable scope and the same ad-hoc HTTP capability. `cancel` is
/// the send's own token, so Cancel reaches a sleeping or requesting script.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_scripts_for(
    state: &State<'_, AppState>,
    settings: &crate::state::Settings,
    scope: VarScope,
    scripts: Vec<String>,
    request: ScriptRequest,
    response: Option<ScriptResponse>,
    base_vars: &HashMap<String, String>,
    cancel: CancellationToken,
) -> Result<ScriptOutcome, String> {
    let opts = ExecOpts {
        proxy: settings.proxy.clone(),
        temp_dir: state.temp_dir.clone(),
    };
    let req_settings = RequestSettings {
        follow_redirects: true,
        timeout_ms: settings.default_timeout_ms,
        verify_tls: settings.default_verify_tls,
    };
    run_scripts(
        state.pool.clone(),
        opts,
        req_settings,
        scope,
        scripts,
        request,
        response,
        base_vars,
        cancel,
    )
    .await
}

/// Run a batch of scripts on a dedicated thread and await the outcome.
#[allow(clippy::too_many_arguments)]
async fn run_scripts(
    pool: Arc<swarmo_http::ClientPool>,
    opts: ExecOpts,
    settings: RequestSettings,
    scope: VarScope,
    scripts: Vec<String>,
    request: ScriptRequest,
    response: Option<ScriptResponse>,
    base_vars: &HashMap<String, String>,
    cancel: CancellationToken,
) -> Result<ScriptOutcome, String> {
    let (job_tx, job_rx) = mpsc::unbounded_channel::<ScriptHttpJob>();
    let server = tokio::spawn(serve_script_http(
        job_rx,
        pool,
        opts,
        settings,
        cancel.clone(),
    ));

    let host: swarmo_script::SharedHost = Arc::new(AppScriptHost {
        tx: job_tx,
        scope,
        cancel,
    });
    let base_vars = base_vars.clone();
    let (done_tx, done_rx) = oneshot::channel::<ScriptOutcome>();

    // QuickJS values are not Send, so the engine lives entirely on this thread.
    std::thread::Builder::new()
        .name("swarmo-script".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let outcome = match &response {
                None => swarmo_script::run_pre_request(
                    host,
                    &scripts,
                    &request,
                    &base_vars,
                    Limits::default(),
                ),
                Some(res) => swarmo_script::run_post_response(
                    host,
                    &scripts,
                    &request,
                    res,
                    &base_vars,
                    Limits::default(),
                ),
            };
            let _ = done_tx.send(outcome);
        })
        .map_err(|e| format!("could not start the script thread: {e}"))?;

    let outcome = done_rx
        .await
        .map_err(|_| "the script thread stopped unexpectedly".to_string())?;

    // Dropping the host closed the job channel, so the server task ends.
    let _ = server.await;
    Ok(outcome)
}

fn apply_script_request(merged: &mut MergedRequest, r: ScriptRequest) {
    merged.method = r.method;
    merged.url = r.url;
    merged.headers = r
        .headers
        .into_iter()
        .map(|(k, v)| swarmo_core::model::KeyValue::new(k, v))
        .collect();

    // Only replace a text body; form and multipart bodies keep their structure.
    match &mut merged.body {
        swarmo_core::model::Body::Json { text } => *text = r.body,
        swarmo_core::model::Body::Text { text, .. } => *text = r.body,
        _ => {}
    }
}

fn body_of(merged: &MergedRequest) -> String {
    match &merged.body {
        swarmo_core::model::Body::Json { text } => text.clone(),
        swarmo_core::model::Body::Text { text, .. } => text.clone(),
        _ => String::new(),
    }
}

/// Replace a `CommandToken` auth with the header it resolves to.
///
/// Returns the command and the token epoch used, so the caller can refresh
/// exactly that token if the server rejects it. `None` when this request does
/// not use a command token.
pub async fn materialize_command_auth(
    state: &AppState,
    merged: &mut swarmo_core::MergedRequest,
) -> Result<Option<(String, u64)>, String> {
    let Auth::CommandToken { command, .. } = &merged.auth else {
        return Ok(None);
    };
    let command = command.clone();
    let token = state.auth_token(&command, None).await?;
    apply_command_token(merged, &token.value);
    Ok(Some((command, token.epoch)))
}

/// Swap a resolved token into the auth slot as a plain header.
fn apply_command_token(merged: &mut swarmo_core::MergedRequest, token: &str) {
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

fn preview_body(body: &swarmo_core::ResolvedBody) -> Option<String> {
    match body {
        swarmo_core::ResolvedBody::None => None,
        swarmo_core::ResolvedBody::Bytes { text, .. } => Some(text.clone()),
        swarmo_core::ResolvedBody::Form { fields } => Some(
            fields
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&"),
        ),
        swarmo_core::ResolvedBody::Multipart { parts } => Some(
            parts
                .iter()
                .map(|p| format!("{}: {}", p.key, p.value))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        swarmo_core::ResolvedBody::File { path } => Some(format!("<file: {path}>")),
    }
}

// ---------------------------------------------------------------------------
// History & cookies
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn history_list(state: State<'_, AppState>) -> Result<Vec<HistoryEntry>, String> {
    Ok(state
        .history
        .lock()
        .map_err(|_| lock_err())?
        .iter()
        .cloned()
        .collect())
}

// Runs on the async runtime, not the WebView UI thread: this does
// unbounded file I/O and would otherwise freeze the window for its duration.
#[tauri::command(async)]
pub fn history_clear(state: State<'_, AppState>) -> Result<(), String> {
    state.clear_history()
}

/// Drop a single entry — for the one send whose URL you would rather not keep.
// Runs on the async runtime, not the WebView UI thread: this does
// unbounded file I/O and would otherwise freeze the window for its duration.
#[tauri::command(async)]
pub fn history_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.delete_history(&id)
}

#[tauri::command]
pub fn cookies_list(state: State<'_, AppState>) -> Result<Vec<swarmo_http::CookieRecord>, String> {
    Ok(state.cookies.lock().map_err(|_| lock_err())?.clone())
}

#[tauri::command]
pub fn cookies_clear(state: State<'_, AppState>) -> Result<(), String> {
    state.cookies.lock().map_err(|_| lock_err())?.clear();
    // The list above is only a record of Set-Cookie headers seen; the jar the
    // clients actually send from is the pool's, and clearing the clients
    // would have rebuilt them around it unchanged.
    state.pool.clear_cookies();
    Ok(())
}
