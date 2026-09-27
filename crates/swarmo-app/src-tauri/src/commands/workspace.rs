//! Workspace, environment, collection, folder and request commands.

use serde::{Deserialize, Serialize};
use swarmo_core::model::*;
use swarmo_core::store::LoadTestEntry;
use swarmo_core::{ImportReport, WorkspaceStore};
use tauri::State;

use crate::state::{lock_err, AppState, Settings};

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInfo {
    pub path: String,
    pub name: String,
    pub active_environment: Option<String>,
    pub environments: Vec<String>,
}

fn info_of(store: &WorkspaceStore) -> Result<WorkspaceInfo, String> {
    let m = store.manifest().map_err(err)?;
    Ok(WorkspaceInfo {
        path: store.root().to_string_lossy().to_string(),
        name: m.name,
        active_environment: m.active_environment,
        environments: store.list_environments().map_err(err)?,
    })
}

// Runs on the async runtime, not the WebView UI thread: this does
// unbounded file I/O and would otherwise freeze the window for its duration.
#[tauri::command(async)]
pub fn workspace_open(state: State<'_, AppState>, path: String) -> Result<WorkspaceInfo, String> {
    state.ensure_no_active_runs()?;
    state.ensure_no_in_flight_sends()?;
    let store = WorkspaceStore::open(&path).map_err(err)?;
    let info = info_of(&store)?;
    // History belongs to the workspace, so it is swapped along with it.
    state.load_history_for(&store);
    *state.store.lock().map_err(|_| lock_err())? = Some(store);
    state.runtime_vars.lock().map_err(|_| lock_err())?.clear();
    state.remember_workspace(&info.path);
    Ok(info)
}

// Runs on the async runtime, not the WebView UI thread: this does
// unbounded file I/O and would otherwise freeze the window for its duration.
#[tauri::command(async)]
pub fn workspace_create(
    state: State<'_, AppState>,
    path: String,
    name: String,
) -> Result<WorkspaceInfo, String> {
    state.ensure_no_active_runs()?;
    state.ensure_no_in_flight_sends()?;
    let store = WorkspaceStore::create(&path, &name).map_err(err)?;
    let info = info_of(&store)?;
    // History belongs to the workspace, so it is swapped along with it.
    state.load_history_for(&store);
    *state.store.lock().map_err(|_| lock_err())? = Some(store);
    state.runtime_vars.lock().map_err(|_| lock_err())?.clear();
    state.remember_workspace(&info.path);
    Ok(info)
}

#[tauri::command]
pub fn workspace_info(state: State<'_, AppState>) -> Result<Option<WorkspaceInfo>, String> {
    let guard = state.store.lock().map_err(|_| lock_err())?;
    match guard.as_ref() {
        Some(store) => Ok(Some(info_of(store)?)),
        None => Ok(None),
    }
}

#[tauri::command]
pub fn workspace_recent(state: State<'_, AppState>) -> Vec<String> {
    state.settings_snapshot().recent_workspaces
}

#[tauri::command]
pub fn workspace_close(state: State<'_, AppState>) -> Result<(), String> {
    state.ensure_no_active_runs()?;
    state.ensure_no_in_flight_sends()?;
    *state.store.lock().map_err(|_| lock_err())? = None;
    state.runtime_vars.lock().map_err(|_| lock_err())?.clear();
    Ok(())
}

// ---------------------------------------------------------------------------
// Tree
// ---------------------------------------------------------------------------

// Runs on the async runtime, not the WebView UI thread: this does
// unbounded file I/O and would otherwise freeze the window for its duration.
#[tauri::command(async)]
pub fn tree_get(state: State<'_, AppState>) -> Result<Vec<TreeNode>, String> {
    state.with_store(|s| s.tree().map_err(err))
}

// ---------------------------------------------------------------------------
// Environments
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn env_list(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    state.with_store(|s| s.list_environments().map_err(err))
}

#[tauri::command]
pub fn env_get(state: State<'_, AppState>, name: String) -> Result<Environment, String> {
    state.with_store(|s| s.get_environment_with_secrets(&name).map_err(err))
}

#[tauri::command]
pub fn env_save(state: State<'_, AppState>, env: Environment) -> Result<(), String> {
    state.with_store(|s| s.save_environment(&env).map_err(err))
}

#[tauri::command]
pub fn env_create(state: State<'_, AppState>, name: String) -> Result<String, String> {
    state.with_store(|s| {
        if s.list_environments().map_err(err)?.contains(&name) {
            return Err(format!("An environment named \"{name}\" already exists."));
        }
        s.save_environment(&Environment::new(&name)).map_err(err)?;
        Ok(name.clone())
    })
}

#[tauri::command]
pub fn env_delete(state: State<'_, AppState>, name: String) -> Result<(), String> {
    state.with_store(|s| s.delete_environment(&name).map_err(err))
}

#[tauri::command]
pub fn env_set_active(state: State<'_, AppState>, name: Option<String>) -> Result<(), String> {
    state.with_store(|s| s.set_active_environment(name.clone()).map_err(err))
}

/// The variables that would apply right now, including script-set overrides.
#[tauri::command]
pub fn env_effective(
    state: State<'_, AppState>,
) -> Result<std::collections::HashMap<String, String>, String> {
    let store = state.store_clone()?;
    let mut scope = store.var_scope(None).map_err(err)?;
    for (k, v) in state.runtime_vars.lock().map_err(|_| lock_err())?.iter() {
        scope.set(k.clone(), v.clone());
    }
    Ok(scope.flatten())
}

/// One expansion of `{{...}}` text, for the variable picker's live preview.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenPreview {
    /// The expanded text, truncated so a million-value repeat cannot be
    /// serialised into the UI on every keystroke.
    pub value: String,
    /// True when `value` was cut short.
    pub truncated: bool,
    /// Full length before truncation, so the preview can say how big it is.
    pub length: usize,
    /// Tokens that could not be understood, left in place in `value`.
    pub unresolved: Vec<String>,
}

/// Expand generator and variable tokens exactly as a real send would.
///
/// The picker previews through this rather than reimplementing the generators
/// in TypeScript: a second implementation would drift, and a preview that
/// disagrees with what is actually sent is worse than no preview.
#[tauri::command]
pub fn token_preview(state: State<'_, AppState>, text: String) -> Result<TokenPreview, String> {
    const MAX_PREVIEW: usize = 2000;

    let store = state.store_clone()?;
    let mut scope = store.var_scope(None).map_err(err)?;
    for (k, v) in state.runtime_vars.lock().map_err(|_| lock_err())?.iter() {
        scope.set(k.clone(), v.clone());
    }

    let (value, unresolved) = swarmo_core::interp::interpolate(&text, &scope);
    let length = value.chars().count();
    let truncated = length > MAX_PREVIEW;
    let value = if truncated {
        value.chars().take(MAX_PREVIEW).collect()
    } else {
        value
    };
    Ok(TokenPreview {
        value,
        truncated,
        length,
        unresolved: unresolved.into_iter().map(|u| u.name).collect(),
    })
}

#[tauri::command]
pub fn runtime_vars_clear(state: State<'_, AppState>) -> Result<(), String> {
    state.runtime_vars.lock().map_err(|_| lock_err())?.clear();
    Ok(())
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn request_get(state: State<'_, AppState>, node_ref: String) -> Result<RequestDef, String> {
    state.with_store(|s| s.get_request(&node_ref).map_err(err))
}

#[tauri::command]
pub fn request_save(
    state: State<'_, AppState>,
    node_ref: String,
    def: RequestDef,
) -> Result<(), String> {
    state.with_store(|s| s.save_request(&node_ref, &def).map_err(err))
}

#[tauri::command]
pub fn request_create(
    state: State<'_, AppState>,
    parent_ref: String,
    name: String,
) -> Result<String, String> {
    let settings = state.settings_snapshot();
    state.with_store(|s| {
        let node_ref = s.create_request(&parent_ref, &name).map_err(err)?;
        let mut def = s.get_request(&node_ref).map_err(err)?;
        apply_new_request_defaults(&mut def.settings, &settings);
        s.save_request(&node_ref, &def).map_err(err)?;
        Ok(node_ref)
    })
}

/// Stamp the "Default timeout" and "Verify TLS by default" settings onto a
/// request being created. Only new requests take them: existing ones keep
/// their own values, as the settings screen says.
fn apply_new_request_defaults(req: &mut RequestSettings, settings: &Settings) {
    req.timeout_ms = settings.default_timeout_ms;
    req.verify_tls = settings.default_verify_tls;
}

#[tauri::command]
pub fn request_rename(
    state: State<'_, AppState>,
    node_ref: String,
    name: String,
) -> Result<String, String> {
    state.with_store(|s| s.rename_request(&node_ref, &name).map_err(err))
}

#[tauri::command]
pub fn request_duplicate(state: State<'_, AppState>, node_ref: String) -> Result<String, String> {
    state.with_store(|s| s.duplicate_request(&node_ref).map_err(err))
}

#[tauri::command]
pub fn request_move(
    state: State<'_, AppState>,
    node_ref: String,
    new_parent_ref: String,
) -> Result<String, String> {
    state.with_store(|s| s.move_request(&node_ref, &new_parent_ref).map_err(err))
}

// ---------------------------------------------------------------------------
// Collections & folders
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn collection_create(state: State<'_, AppState>, name: String) -> Result<String, String> {
    state.with_store(|s| s.create_collection(&name).map_err(err))
}

#[tauri::command]
pub fn folder_create(
    state: State<'_, AppState>,
    parent_ref: String,
    name: String,
) -> Result<String, String> {
    state.with_store(|s| s.create_folder(&parent_ref, &name).map_err(err))
}

#[tauri::command]
pub fn container_get(state: State<'_, AppState>, node_ref: String) -> Result<ContainerDef, String> {
    state.with_store(|s| s.get_container(&node_ref).map_err(err))
}

#[tauri::command]
pub fn container_save(
    state: State<'_, AppState>,
    node_ref: String,
    def: ContainerDef,
) -> Result<(), String> {
    state.with_store(|s| s.save_container(&node_ref, &def).map_err(err))
}

#[tauri::command]
pub fn container_rename(
    state: State<'_, AppState>,
    node_ref: String,
    name: String,
) -> Result<String, String> {
    state.with_store(|s| s.rename_container(&node_ref, &name).map_err(err))
}

#[tauri::command]
pub fn node_delete(state: State<'_, AppState>, node_ref: String) -> Result<(), String> {
    state.with_store(|s| s.delete_node(&node_ref).map_err(err))
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

// Runs on the async runtime, not the WebView UI thread: this does
// unbounded file I/O and would otherwise freeze the window for its duration.
#[tauri::command(async)]
pub fn import_postman_collection(
    state: State<'_, AppState>,
    file_path: String,
) -> Result<ImportReport, String> {
    state.with_store(|s| {
        swarmo_core::postman::import_collection(s, std::path::Path::new(&file_path)).map_err(err)
    })
}

/// Where a request is now, given where it was and which one it is.
///
/// Returns `None` when it is genuinely gone, which is what lets the UI say so
/// rather than offering a button that cannot work.
#[tauri::command]
pub fn request_locate(
    state: State<'_, AppState>,
    node_ref: String,
    id: Option<String>,
) -> Result<Option<String>, String> {
    state.with_store(|s| {
        Ok(s.locate_request(&node_ref, id.as_deref())
            .ok()
            .map(|(current, _)| current))
    })
}

/// Where a load test is now, given where it was and which one it is.
#[tauri::command]
pub fn load_test_locate(
    state: State<'_, AppState>,
    node_ref: String,
    id: Option<String>,
) -> Result<Option<String>, String> {
    state.with_store(|s| Ok(s.locate_load_test(&node_ref, id.as_deref())))
}

/// Copy a load test, giving the copy an identity of its own.
#[tauri::command]
pub fn load_test_duplicate(state: State<'_, AppState>, node_ref: String) -> Result<String, String> {
    state.with_store(|s| s.duplicate_load_test(&node_ref).map_err(err))
}

/// Rename a load scenario or user script. Returns its new ref.
#[tauri::command]
pub fn load_test_rename(
    state: State<'_, AppState>,
    node_ref: String,
    new_name: String,
) -> Result<String, String> {
    state.with_store(|s| s.rename_load_test(&node_ref, &new_name).map_err(err))
}

/// Rename an environment, updating everything that refers to it by name.
#[tauri::command]
pub fn environment_rename(
    state: State<'_, AppState>,
    old_name: String,
    new_name: String,
) -> Result<(), String> {
    state.with_store(|s| s.rename_environment(&old_name, &new_name).map_err(err))
}

/// What running an auth command produced, with the token never shown in full.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthCommandTest {
    /// e.g. "eyJh…f2Qw" — enough to tell two tokens apart, not enough to use.
    pub masked: String,
    pub length: usize,
    /// Seconds until the token expires, when it says so itself.
    pub expires_in_sec: Option<u64>,
}

/// Run an auth command once and report what it produced.
///
/// Approval-gated exactly like a real send: a "Test" button must not be a way
/// to run an unapproved command.
#[tauri::command]
pub async fn auth_command_test(
    state: State<'_, AppState>,
    command: String,
) -> Result<AuthCommandTest, String> {
    if !state.auth_command_approved(&command) {
        return Err(crate::state::unapproved_command_error(&command));
    }
    let token = swarmo_load::auth::run_command(&command).await?;
    let expires_in_sec = swarmo_load::auth::jwt_expiry(&token).and_then(|at| {
        at.duration_since(std::time::SystemTime::now())
            .ok()
            .map(|d| d.as_secs())
    });
    Ok(AuthCommandTest {
        masked: swarmo_load::auth::mask(&token),
        length: token.chars().count(),
        expires_in_sec,
    })
}

/// Allow a command to be run for auth tokens in this workspace.
#[tauri::command]
pub fn auth_command_approve(state: State<'_, AppState>, command: String) -> Result<(), String> {
    state.approve_auth_command(&command)
}

/// Every auth command this workspace has approved.
#[tauri::command]
pub fn auth_commands_approved(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    state.approved_auth_commands()
}

/// What a pasted cURL command would import as, before anything is saved.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurlPreview {
    pub def: swarmo_core::model::RequestDef,
    pub warnings: Vec<String>,
}

/// Parse a cURL command without saving anything, so the UI can preview it.
#[tauri::command]
pub fn curl_parse(text: String) -> Result<CurlPreview, String> {
    let out = swarmo_core::curl_import::parse_curl(&text)?;
    Ok(CurlPreview {
        def: out.def,
        warnings: out.warnings,
    })
}

/// Save a parsed cURL command as a new request in `parent_ref`.
#[tauri::command]
pub fn curl_import(
    state: State<'_, AppState>,
    parent_ref: String,
    text: String,
) -> Result<String, String> {
    let parsed = swarmo_core::curl_import::parse_curl(&text)?;
    let settings = state.settings_snapshot();
    state.with_store(|s| {
        // Reuse the normal creation path so the file lands with the same
        // naming and collision rules as any other new request.
        let node_ref = s
            .create_request(&parent_ref, &parsed.def.name)
            .map_err(err)?;
        let mut def = s.get_request(&node_ref).map_err(err)?;
        let id = def.id.clone();
        def = parsed.def.clone();
        def.id = id;
        // cURL has no say in the timeout (--max-time is skipped), so the
        // default applies. It does in TLS: -k turns verification off, and
        // that must survive a default of "on".
        let insecure = !def.settings.verify_tls;
        apply_new_request_defaults(&mut def.settings, &settings);
        if insecure {
            def.settings.verify_tls = false;
        }
        s.save_request(&node_ref, &def).map_err(err)?;
        Ok(node_ref)
    })
}

// Runs on the async runtime, not the WebView UI thread: this does
// unbounded file I/O and would otherwise freeze the window for its duration.
#[tauri::command(async)]
pub fn import_postman_environment(
    state: State<'_, AppState>,
    file_path: String,
) -> Result<String, String> {
    state.with_store(|s| {
        swarmo_core::postman::import_environment(s, std::path::Path::new(&file_path)).map_err(err)
    })
}

// ---------------------------------------------------------------------------
// Load-test files (listing lives here; running lives in commands::load)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn load_list(state: State<'_, AppState>) -> Result<Vec<LoadTestEntry>, String> {
    state.with_store(|s| s.list_load_tests().map_err(err))
}

/// The same tests, nested by folder, for the sidebar.
#[tauri::command(async)]
pub fn load_tree(state: State<'_, AppState>) -> Result<Vec<LoadTestEntry>, String> {
    state.with_store(|s| s.load_tree().map_err(err))
}

#[tauri::command]
pub fn load_folder_create(
    state: State<'_, AppState>,
    parent_ref: String,
    name: String,
) -> Result<String, String> {
    state.with_store(|s| s.create_load_folder(&parent_ref, &name).map_err(err))
}

#[tauri::command]
pub fn load_folder_rename(
    state: State<'_, AppState>,
    node_ref: String,
    name: String,
) -> Result<String, String> {
    state.with_store(|s| s.rename_load_folder(&node_ref, &name).map_err(err))
}

#[tauri::command]
pub fn load_test_move(
    state: State<'_, AppState>,
    node_ref: String,
    new_parent_ref: String,
) -> Result<String, String> {
    state.with_store(|s| s.move_load_test(&node_ref, &new_parent_ref).map_err(err))
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn settings_get(state: State<'_, AppState>) -> Settings {
    state.settings_snapshot()
}

#[tauri::command]
pub fn settings_save(state: State<'_, AppState>, settings: Settings) -> Result<(), String> {
    {
        let mut guard = state.settings.lock().map_err(|_| lock_err())?;
        // Recent workspaces are managed by the app, not the settings form.
        let recent = guard.recent_workspaces.clone();
        *guard = Settings {
            recent_workspaces: recent,
            ..settings
        };
    }
    // Proxy or TLS changes invalidate the cached clients.
    state.pool.clear();
    state.save_settings()
}

#[tauri::command]
pub fn approved_hosts_clear(state: State<'_, AppState>) -> Result<(), String> {
    state.clear_approved_hosts()
}
