//! WebSocket requests: files, and running one session from the client.
//!
//! The session the client runs is the same artifact the load engine runs —
//! same file, same resolution, same executor — with one difference: here the
//! transcript is kept, because a person is going to read it.

use std::collections::HashMap;

use serde::Serialize;
use tauri::State;
use tokio_util::sync::CancellationToken;

use swarmo_core::model::{Auth, HistoryEntry, HistoryResponse, HistorySentRequest};
use swarmo_core::model_ws::{finalize_ws, MergedWsRequest, ResolvedWsMessage, WsRequestDef};
use swarmo_core::{now_millis, store::Protocol};

use crate::state::{lock_err, AppState};

// Each command module keeps its own, as exec.rs and grpc_exec.rs do.
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub async fn ws_request_get(
    state: State<'_, AppState>,
    node_ref: String,
) -> Result<WsRequestDef, String> {
    state.with_store(|s| s.get_ws_request(&node_ref).map_err(err))
}

#[tauri::command]
pub async fn ws_request_save(
    state: State<'_, AppState>,
    node_ref: String,
    def: WsRequestDef,
) -> Result<(), String> {
    state.with_store(|s| s.save_ws_request(&node_ref, &def).map_err(err))
}

#[tauri::command]
pub async fn ws_request_create(
    state: State<'_, AppState>,
    parent_ref: String,
    name: String,
) -> Result<String, String> {
    state.with_store(|s| s.create_ws_request(&parent_ref, &name).map_err(err))
}

/// What was actually sent, for the request tab of the response pane.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SentWsRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub subprotocols: Vec<String>,
    pub messages: Vec<ResolvedWsMessage>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WsSendResult {
    pub exec_id: String,
    pub request_ref: String,
    /// The measured session, when the handshake got that far.
    pub result: Option<swarmo_ws::WsResult>,
    /// Why there is no session: a bad URL, an unreachable host.
    pub error: Option<String>,
    pub unresolved: Vec<String>,
    pub sent: SentWsRequest,
    pub variables_set: HashMap<String, String>,
}

/// A `CommandToken` auth becomes a concrete header before resolution, the
/// same way it does for HTTP and gRPC; the command has to be approved.
async fn materialize_command_auth_ws(
    state: &AppState,
    merged: &mut MergedWsRequest,
) -> Result<(), String> {
    let Auth::CommandToken {
        command,
        header_name,
        prefix,
    } = &merged.auth
    else {
        return Ok(());
    };
    let (command, header_name, prefix) = (command.clone(), header_name.clone(), prefix.clone());
    let token = state.auth_token(&command, None).await?;
    merged.auth = Auth::ApiKeyHeader {
        header_name,
        value: format!("{prefix}{}", token.value),
    };
    Ok(())
}

#[tauri::command]
pub async fn ws_send(
    state: State<'_, AppState>,
    node_ref: String,
    exec_id: String,
) -> Result<WsSendResult, String> {
    // Registered under the same exec id scheme as HTTP and gRPC, so the one
    // `request_cancel` command stops any of them.
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
    state: &AppState,
    node_ref: &str,
    exec_id: &str,
    cancel: CancellationToken,
) -> Result<WsSendResult, String> {
    let store = state.store_clone()?;
    let mut merged = store.merged_ws_request(node_ref).map_err(err)?;

    let mut scope = store.var_scope(None).map_err(err)?;
    {
        let runtime = state.runtime_vars.lock().map_err(|_| lock_err())?;
        for (k, v) in runtime.iter() {
            scope.set(k.clone(), v.clone());
        }
    }

    materialize_command_auth_ws(state, &mut merged).await?;
    let resolved = finalize_ws(&merged, &scope);

    let sent = SentWsRequest {
        url: resolved.url.clone(),
        headers: resolved.headers.clone(),
        subprotocols: resolved.subprotocols.clone(),
        messages: resolved.messages.clone(),
    };
    let unresolved = resolved.unresolved.clone();

    // Recorded whatever happens: a session that never connected is exactly
    // the one worth finding in the history later.
    let mut entry = HistoryEntry {
        id: exec_id.to_string(),
        request_ref: node_ref.to_string(),
        request_id: Some(merged.id.clone()),
        name: merged.name.clone(),
        protocol: Protocol::Ws,
        method: "WS".into(),
        url: resolved.url.clone(),
        status: 0,
        status_text: None,
        duration_ms: 0.0,
        response_bytes: 0,
        at: now_millis(),
        ok: false,
        error: None,
        response: HistoryResponse::default(),
        request: HistorySentRequest::new(
            resolved.headers.clone(),
            Some(
                resolved
                    .messages
                    .iter()
                    .map(|m| m.body.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        ),
    };

    let outcome = swarmo_ws::run(&resolved, &cancel, true).await;

    let (result, error) = match outcome {
        Ok(res) => {
            // 101 is the handshake; a session that then broke reports the
            // close code it ended on, matching what the load engine records.
            entry.status = if res.connected && res.error.is_none() {
                101
            } else if !res.connected {
                // Cancelled during the handshake: nothing closed, so no code.
                0
            } else {
                res.close_code.unwrap_or(1006)
            };
            entry.status_text = Some(if res.ok() {
                "connected".to_string()
            } else {
                res.error
                    .clone()
                    .unwrap_or_else(|| "a wait went unanswered".into())
            });
            entry.duration_ms = res.duration_ms;
            entry.response_bytes = res.bytes_in;
            entry.ok = res.ok();
            entry.error = res.error.clone();
            // The transcript, one frame per line, is the response worth
            // keeping for a socket.
            let transcript = res
                .frames
                .iter()
                .map(|f| {
                    format!(
                        "{} {}",
                        match f.direction {
                            swarmo_ws::Direction::Out => "→",
                            swarmo_ws::Direction::In => "←",
                        },
                        f.body
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            entry.response = HistoryResponse::new(Vec::new(), Some(transcript));
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

    Ok(WsSendResult {
        exec_id: exec_id.to_string(),
        request_ref: node_ref.to_string(),
        result,
        error,
        unresolved,
        sent,
        variables_set: HashMap::new(),
    })
}
