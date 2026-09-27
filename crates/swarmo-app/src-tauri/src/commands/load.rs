//! Load-test scenario editing and run control.

use serde::{Deserialize, Serialize};
use swarmo_core::model::*;
use swarmo_load::plan::LoadPlan;
use swarmo_load::{run as run_load, RunControl};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::state::{lock_err, AppState};

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub const EVENT_SNAPSHOT: &str = "load://snapshot";
pub const EVENT_DONE: &str = "load://done";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotEvent {
    pub run_id: String,
    pub snapshot: Snapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoneEvent {
    pub run_id: String,
    pub summary: RunSummary,
}

// ---------------------------------------------------------------------------
// Scenario CRUD
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn scenario_get(state: State<'_, AppState>, node_ref: String) -> Result<LoadScenario, String> {
    state.with_store(|s| s.get_scenario(&node_ref).map_err(err))
}

#[tauri::command]
pub fn scenario_save(
    state: State<'_, AppState>,
    node_ref: String,
    scenario: LoadScenario,
) -> Result<(), String> {
    state.with_store(|s| s.save_scenario(&node_ref, &scenario).map_err(err))
}

#[tauri::command]
pub fn scenario_create(
    state: State<'_, AppState>,
    name: String,
    parent_ref: Option<String>,
) -> Result<String, String> {
    let parent = parent_ref.unwrap_or_default();
    state.with_store(|s| s.create_scenario_in(&parent, &name).map_err(err))
}

#[tauri::command]
pub fn user_script_create(
    state: State<'_, AppState>,
    name: String,
    parent_ref: Option<String>,
) -> Result<String, String> {
    let parent = parent_ref.unwrap_or_default();
    state.with_store(|s| {
        s.create_user_script_in(&parent, &name, swarmo_script::USER_SCRIPT_TEMPLATE)
            .map_err(err)
    })
}

#[tauri::command]
pub fn text_get(state: State<'_, AppState>, node_ref: String) -> Result<String, String> {
    state.with_store(|s| s.read_text(&node_ref).map_err(err))
}

#[tauri::command]
pub fn text_save(state: State<'_, AppState>, node_ref: String, text: String) -> Result<(), String> {
    state.with_store(|s| s.write_text(&node_ref, &text).map_err(err))
}

/// Build a scenario from a set of requests ("promote to load test").
#[tauri::command]
pub fn load_promote(
    state: State<'_, AppState>,
    request_refs: Vec<String>,
    name: String,
) -> Result<String, String> {
    if request_refs.is_empty() {
        return Err("Select at least one request to promote.".into());
    }
    state.with_store(|s| {
        let scenario_ref = s.create_scenario(&name).map_err(err)?;
        let mut scenario = s.get_scenario(&scenario_ref).map_err(err)?;
        scenario.steps = request_refs
            .iter()
            .map(|r| LoadStep {
                request_id: None,
                request_ref: r.clone(),
                think_time_ms: Some([200, 800]),
                capture: Vec::new(),
                tag: None,
                parallel: false,
            })
            .collect();
        scenario.environment = s.manifest().map_err(err)?.active_environment;
        s.save_scenario(&scenario_ref, &scenario).map_err(err)?;
        Ok(scenario_ref)
    })
}

// ---------------------------------------------------------------------------
// Preflight & running
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preflight {
    pub name: String,
    pub mode: LoadMode,
    pub duration_sec: u64,
    pub peak_target: f64,
    pub max_vus: u32,
    /// Every distinct host this run would send traffic to.
    pub hosts: Vec<String>,
    /// Hosts the user has not yet approved for this workspace.
    pub unapproved_hosts: Vec<String>,
    /// Commands this run would execute for auth tokens.
    #[serde(default)]
    pub auth_commands: Vec<String>,
    /// Of those, the ones not yet approved in this workspace.
    #[serde(default)]
    pub unapproved_auth_commands: Vec<String>,
    pub scripted: bool,
}

/// Planning compiles `.proto` files and may fetch a schema over reflection, so
/// it is async even though HTTP-only scenarios never await anything.
async fn plan_for(store: &swarmo_core::WorkspaceStore, node_ref: &str) -> Result<LoadPlan, String> {
    if node_ref.ends_with(".user.js") {
        LoadPlan::from_user_script(store, node_ref)
            .await
            .map_err(err)
    } else {
        LoadPlan::from_scenario(store, node_ref).await.map_err(err)
    }
}

/// Validate a scenario and report what it would hit, for the confirmation dialog.
#[tauri::command]
pub async fn load_preflight(
    state: State<'_, AppState>,
    node_ref: String,
) -> Result<Preflight, String> {
    let store = state.store_clone()?;
    let plan = plan_for(&store, &node_ref).await?;

    let approved = state.approved_load_hosts()?;
    let hosts = plan.target_hosts();
    let unapproved: Vec<String> = hosts
        .iter()
        .filter(|h| !approved.contains(h))
        .cloned()
        .collect();

    // Auth commands are surfaced here for the same reason hosts are: the
    // decision to let something run belongs before the run, not thirty
    // seconds into it.
    let auth_commands = plan.auth_commands();
    let unapproved_auth_commands: Vec<String> = auth_commands
        .iter()
        .filter(|c| !state.auth_command_approved(c))
        .cloned()
        .collect();

    Ok(Preflight {
        name: plan.name.clone(),
        mode: plan.mode,
        duration_sec: plan.total_duration_sec(),
        peak_target: plan.peak_target(),
        max_vus: plan.max_vus,
        hosts,
        unapproved_hosts: unapproved,
        auth_commands,
        unapproved_auth_commands,
        scripted: matches!(plan.kind, swarmo_load::PlanKind::Scripted { .. }),
    })
}

#[tauri::command]
pub fn load_approve_hosts(state: State<'_, AppState>, hosts: Vec<String>) -> Result<(), String> {
    state.approve_load_hosts(&hosts)
}

/// Start a run. Returns immediately with the run id; progress arrives as events.
#[tauri::command]
pub async fn load_run(
    app: AppHandle,
    state: State<'_, AppState>,
    node_ref: String,
) -> Result<String, String> {
    // Registered before anything else, planning included: planning can take
    // a while (a reflection fetch, an auth command), and a workspace switch
    // in that window would pass the active-runs check and leave this run
    // started against the old workspace.
    let run_id = uuid::Uuid::new_v4().to_string();
    let cancel = CancellationToken::new();
    state
        .runs
        .lock()
        .map_err(|_| lock_err())?
        .insert(run_id.clone(), cancel.clone());
    // Removes the run again on every early return below, and later however
    // the run task ends.
    let deregister = Deregister {
        app: app.clone(),
        run_id: run_id.clone(),
    };

    let store = state.store_clone()?;
    let mut plan = plan_for(&store, &node_ref).await?;

    // Resolve auth tokens once, before the run starts: one process spawned
    // instead of hundreds, and a broken command fails the run immediately
    // rather than turning into a wall of 401s. The token is *installed* rather
    // than baked in, so a run that outlives its credentials can refresh them
    // without stopping.
    for command in plan.auth_commands() {
        let token = state.auth_token(&command, None).await?;
        plan.install_auth_token(&command, state.tokens.clone(), token);
    }

    // Refuse to start against a host the user has not approved.
    let approved = state.approved_load_hosts()?;
    let unapproved: Vec<String> = plan
        .target_hosts()
        .into_iter()
        .filter(|h| !approved.contains(h))
        .collect();
    if !unapproved.is_empty() {
        return Err(format!(
            "These hosts have not been approved for load testing in this workspace: {}. \
             Confirm them first.",
            unapproved.join(", ")
        ));
    }

    let (tx, mut rx) = broadcast::channel::<Snapshot>(1024);
    let ctrl = RunControl {
        run_id: run_id.clone(),
        cancel,
        snapshots: tx,
    };

    // Forward live snapshots to the UI.
    {
        let app = app.clone();
        let run_id = run_id.clone();
        tokio::spawn(async move {
            while let Ok(snapshot) = rx.recv().await {
                let _ = app.emit(
                    EVENT_SNAPSHOT,
                    SnapshotEvent {
                        run_id: run_id.clone(),
                        snapshot,
                    },
                );
            }
        });
    }

    let app_for_run = app.clone();
    let run_id_for_run = run_id.clone();
    tokio::spawn(async move {
        let out = run_load(plan, ctrl).await;

        if let Err(e) = store.save_run_summary(&out.summary) {
            tracing::warn!("could not save the run summary: {e}");
        }
        if let Err(e) = store.save_run_timeline(&run_id_for_run, &out.timeline) {
            tracing::warn!("could not save the run timeline: {e}");
        }

        // Deregistered *before* `done` goes out: the UI's handler queries
        // the active set on that event, and would otherwise see the finished
        // run still listed as running.
        drop(deregister);

        let _ = app_for_run.emit(
            EVENT_DONE,
            DoneEvent {
                run_id: run_id_for_run.clone(),
                summary: out.summary,
            },
        );
    });

    Ok(run_id)
}

/// Removes a run from the active set when dropped — a panic inside the
/// engine included. Left registered, a dead run would keep `load_stop`
/// answering true and block every workspace switch until the app restarted.
struct Deregister {
    app: AppHandle,
    run_id: String,
}

impl Drop for Deregister {
    fn drop(&mut self) {
        if let Some(state) = tauri_state(&self.app) {
            if let Ok(mut runs) = state.runs.lock() {
                runs.remove(&self.run_id);
            }
        }
    }
}

fn tauri_state(app: &AppHandle) -> Option<tauri::State<'_, AppState>> {
    Some(tauri::Manager::state::<AppState>(app))
}

#[tauri::command]
pub fn load_stop(state: State<'_, AppState>, run_id: String) -> Result<bool, String> {
    let runs = state.runs.lock().map_err(|_| lock_err())?;
    match runs.get(&run_id) {
        Some(token) => {
            token.cancel();
            Ok(true)
        }
        None => Ok(false),
    }
}

#[tauri::command]
pub fn load_active_runs(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    Ok(state
        .runs
        .lock()
        .map_err(|_| lock_err())?
        .keys()
        .cloned()
        .collect())
}

/// Run a blocking file operation off the async worker threads.
async fn blocking<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("The operation could not be completed: {e}"))?
}

// ---------------------------------------------------------------------------
// Run history
// ---------------------------------------------------------------------------

// Runs on the async runtime, not the WebView UI thread: this does
// unbounded file I/O and would otherwise freeze the window for its duration.
#[tauri::command(async)]
pub fn runs_list(state: State<'_, AppState>) -> Result<Vec<RunListEntry>, String> {
    state.with_store(|s| s.list_runs().map_err(err))
}

#[tauri::command]
pub fn run_get(state: State<'_, AppState>, run_id: String) -> Result<RunSummary, String> {
    state.with_store(|s| s.get_run(&run_id).map_err(err))
}

// Runs on the async runtime, not the WebView UI thread: this does
// unbounded file I/O and would otherwise freeze the window for its duration.
#[tauri::command(async)]
pub fn run_timeline(state: State<'_, AppState>, run_id: String) -> Result<Vec<Snapshot>, String> {
    state.with_store(|s| s.get_run_timeline(&run_id).map_err(err))
}

/// Write a run to a shareable file.
///
/// `format` is "html" for a self-contained report, or "json" for the raw
/// summary and timeline. The path comes from the user through a save dialog,
/// so this only ever writes where they pointed it.
#[tauri::command]
pub async fn run_export(
    state: State<'_, AppState>,
    run_id: String,
    path: String,
    format: String,
) -> Result<(), String> {
    let store = state.store_clone()?;
    // Reading a timeline and rendering a report is real work, and the file can
    // be large; neither belongs on an async worker thread.
    blocking(move || {
        let summary = store.get_run(&run_id).map_err(err)?;
        let timeline = store.get_run_timeline(&run_id).map_err(err)?;
        let annotation = store.get_run_annotation(&run_id);

        let bytes = match format.as_str() {
            "html" => {
                swarmo_core::report::html_report(&summary, &timeline, &annotation).into_bytes()
            }
            "json" => serde_json::to_vec_pretty(&swarmo_core::report::json_report(
                &summary,
                &timeline,
                &annotation,
            ))
            .map_err(|e| e.to_string())?,
            other => return Err(format!("Unknown report format: {other}")),
        };
        std::fs::write(&path, bytes).map_err(|e| format!("Could not write {path}: {e}"))
    })
    .await
}

#[tauri::command]
pub fn run_annotation_get(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<RunAnnotation, String> {
    state.with_store(|s| Ok(s.get_run_annotation(&run_id)))
}

/// Set a run's name and notes. Blank values clear them; the saved result is
/// returned so the caller reflects exactly what landed on disk.
#[tauri::command]
pub async fn run_annotation_set(
    state: State<'_, AppState>,
    run_id: String,
    label: Option<String>,
    notes: Option<String>,
) -> Result<RunAnnotation, String> {
    let store = state.store_clone()?;
    blocking(move || {
        store
            .save_run_annotation(&run_id, RunAnnotation { label, notes })
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub fn run_delete(state: State<'_, AppState>, run_id: String) -> Result<(), String> {
    state.ensure_run_not_active(&run_id)?;
    state.with_store(|s| s.delete_run(&run_id).map_err(err))
}

#[tauri::command]
pub fn user_script_template() -> String {
    swarmo_script::USER_SCRIPT_TEMPLATE.to_string()
}
