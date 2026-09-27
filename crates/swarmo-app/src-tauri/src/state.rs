//! Process-wide application state shared by every Tauri command.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use swarmo_core::model::{HistoryEntry, MAX_HISTORY};
use swarmo_core::WorkspaceStore;
use swarmo_http::{ClientPool, CookieRecord};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// "system" | "light" | "dark"
    pub theme: String,
    pub proxy: Option<String>,
    pub default_timeout_ms: u64,
    pub default_verify_tls: bool,
    pub recent_workspaces: Vec<String>,
}

/// What the user has allowed in one workspace.
///
/// Kept in the app's config directory, keyed by the workspace's path, and
/// never inside the workspace itself. A workspace is committed and shared —
/// and `.gitignore` is no barrier to a file someone commits on purpose — so
/// an approval stored in it would be one the workspace could grant itself:
/// a shared collection arriving with its own shell command pre-approved.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceApprovals {
    /// Hosts confirmed as safe to send load to.
    #[serde(default)]
    pub load_hosts: Vec<String>,
    /// Auth commands allowed to run, by exact command text.
    #[serde(default)]
    pub auth_commands: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            proxy: None,
            default_timeout_ms: 30_000,
            default_verify_tls: true,
            recent_workspaces: Vec::new(),
        }
    }
}

pub struct AppState {
    pub store: Mutex<Option<WorkspaceStore>>,
    pub pool: Arc<ClientPool>,
    /// Variables set by scripts during this session. Never written to disk.
    pub runtime_vars: Mutex<HashMap<String, String>>,
    pub history: Mutex<VecDeque<HistoryEntry>>,
    /// Orders asynchronous history writes and lets newer snapshots supersede
    /// older ones for the same workspace.
    history_persistence: Arc<HistoryPersistence>,
    /// Tokens produced by auth commands. Session-only: never written to disk.
    pub tokens: Arc<swarmo_load::auth::TokenCache>,
    pub cookies: Mutex<Vec<CookieRecord>>,
    pub settings: Mutex<Settings>,
    pub settings_path: Mutex<Option<PathBuf>>,
    /// Approvals per workspace path; see `WorkspaceApprovals`.
    approvals: Mutex<HashMap<String, WorkspaceApprovals>>,
    approvals_path: Mutex<Option<PathBuf>>,
    /// In-flight sends, so the UI can cancel them.
    pub in_flight: Mutex<HashMap<String, CancellationToken>>,
    /// Active load runs.
    pub runs: Mutex<HashMap<String, CancellationToken>>,
    pub temp_dir: PathBuf,
    /// gRPC channels for the client UI. One per address is plenty here; the
    /// load engine builds its own wider pool.
    pub grpc_channels: Arc<swarmo_grpc::ChannelPool>,
    /// Compiled gRPC schemas, keyed by proto source. File-based entries carry
    /// modification times, so editing a `.proto` invalidates them.
    pub descriptors: Mutex<HashMap<String, Arc<swarmo_grpc::DescriptorSource>>>,
}

impl AppState {
    pub fn new(temp_dir: PathBuf) -> Self {
        Self {
            store: Mutex::new(None),
            pool: Arc::new(ClientPool::new()),
            runtime_vars: Mutex::new(HashMap::new()),
            history: Mutex::new(VecDeque::new()),
            history_persistence: Arc::new(HistoryPersistence::default()),
            tokens: Arc::new(swarmo_load::auth::TokenCache::new()),
            cookies: Mutex::new(Vec::new()),
            settings: Mutex::new(Settings::default()),
            settings_path: Mutex::new(None),
            approvals: Mutex::new(HashMap::new()),
            approvals_path: Mutex::new(None),
            in_flight: Mutex::new(HashMap::new()),
            runs: Mutex::new(HashMap::new()),
            temp_dir,
            grpc_channels: Arc::new(swarmo_grpc::ChannelPool::new(1)),
            descriptors: Mutex::new(HashMap::new()),
        }
    }

    /// The open workspace, or a user-facing error explaining that none is open.
    pub fn with_store<T>(
        &self,
        f: impl FnOnce(&WorkspaceStore) -> Result<T, String>,
    ) -> Result<T, String> {
        // The store is a path; the lock only guards the swap. Holding it for
        // the whole operation would make a long import stall every send
        // that needs a clone of the store meanwhile.
        let store = self.store_clone()?;
        f(&store)
    }

    /// Refuse to switch workspaces while a send is still in flight.
    ///
    /// A send finishing after the switch would write its history entry and
    /// its script-set variables into whichever workspace is open *then* —
    /// request A's URL and headers landing in workspace B's history file.
    /// The same guard already exists for load runs.
    pub fn ensure_no_in_flight_sends(&self) -> Result<(), String> {
        let n = self.in_flight.lock().map_err(|_| lock_err())?.len();
        if n > 0 {
            return Err(format!(
                "{n} request{} still in flight. Wait for {} to finish, or cancel {}, before                  switching workspaces.",
                if n == 1 { " is" } else { "s are" },
                if n == 1 { "it" } else { "them" },
                if n == 1 { "it" } else { "them" },
            ));
        }
        Ok(())
    }

    pub fn store_clone(&self) -> Result<WorkspaceStore, String> {
        let guard = self.store.lock().map_err(|_| lock_err())?;
        guard
            .clone()
            .ok_or_else(|| "No workspace is open. Open or create one first.".to_string())
    }

    /// Record a send and write history through to the workspace.
    ///
    /// Persisting here rather than on shutdown is what makes history survive a
    /// crash. The in-memory push is synchronous so a `history_list` right after
    /// a send already sees the entry; only the disk write is handed to a
    /// blocking thread, so it never stalls an async command worker. A failed
    /// write is ignored: history is a convenience, and losing it must never
    /// fail the send that produced it.
    pub fn push_history(&self, entry: HistoryEntry) {
        // Cloned out from under the lock: the guard must not be held across
        // the hop onto the blocking pool.
        let store = match self.store.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => return,
        };
        let (snapshot, revision) = match self.history.lock() {
            Ok(mut h) => {
                h.push_front(entry);
                while h.len() > MAX_HISTORY {
                    h.pop_back();
                }
                let Some(store) = &store else { return };
                // Reserved while the history lock is still held, so revision
                // order is snapshot order. Reserved after it, two sends could
                // swap, and the older snapshot would win on disk.
                let Ok(revision) = self.history_persistence.reserve(store.root()) else {
                    return;
                };
                (h.iter().cloned().collect::<Vec<_>>(), revision)
            }
            Err(_) => return,
        };
        let Some(store) = store else { return };
        let persistence = self.history_persistence.clone();
        spawn_write(move || {
            let _ = persistence.save_if_current(revision, &store, &snapshot);
        });
    }

    /// The open workspace's approvals, read or changed under one lock.
    /// A change is written through, so it survives a restart.
    fn with_approvals<T>(
        &self,
        change: bool,
        f: impl FnOnce(&mut WorkspaceApprovals) -> T,
    ) -> Result<T, String> {
        let store = self.store_clone()?;
        // Canonical, so one folder reached by two spellings is one workspace.
        let root = store.root();
        let key = std::fs::canonicalize(root)
            .unwrap_or_else(|_| root.to_path_buf())
            .to_string_lossy()
            .into_owned();

        let mut all = self.approvals.lock().map_err(|_| lock_err())?;
        let out = f(all.entry(key.clone()).or_default());
        if !change {
            return Ok(out);
        }
        if all
            .get(&key)
            .is_some_and(|a| a.load_hosts.is_empty() && a.auth_commands.is_empty())
        {
            all.remove(&key);
        }
        // Written under the lock, so two approvals cannot land out of order.
        let path = self.approvals_path.lock().map_err(|_| lock_err())?.clone();
        if let Some(path) = path {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let text = serde_json::to_string_pretty(&*all).map_err(|e| e.to_string())?;
            std::fs::write(&path, text).map_err(|e| e.to_string())?;
        }
        Ok(out)
    }

    /// Read the approvals file. A missing or unreadable one approves nothing.
    pub fn load_approvals(&self, path: PathBuf) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(all) = serde_json::from_str(&text) {
                if let Ok(mut guard) = self.approvals.lock() {
                    *guard = all;
                }
            }
        }
        if let Ok(mut p) = self.approvals_path.lock() {
            *p = Some(path);
        }
    }

    /// Hosts approved for load in the open workspace.
    pub fn approved_load_hosts(&self) -> Result<Vec<String>, String> {
        self.with_approvals(false, |a| a.load_hosts.clone())
    }

    pub fn approve_load_hosts(&self, hosts: &[String]) -> Result<(), String> {
        self.with_approvals(true, |a| {
            for h in hosts {
                if !a.load_hosts.contains(h) {
                    a.load_hosts.push(h.clone());
                }
            }
        })
    }

    pub fn clear_approved_hosts(&self) -> Result<(), String> {
        self.with_approvals(true, |a| a.load_hosts.clear())
    }

    /// Commands approved for auth in the open workspace.
    pub fn approved_auth_commands(&self) -> Result<Vec<String>, String> {
        self.with_approvals(false, |a| a.auth_commands.clone())
    }

    /// Whether the user has allowed this exact command in this workspace.
    pub fn auth_command_approved(&self, command: &str) -> bool {
        self.with_approvals(false, |a| a.auth_commands.iter().any(|c| c == command))
            .unwrap_or(false)
    }

    /// Record that the user allowed a command to run in this workspace.
    pub fn approve_auth_command(&self, command: &str) -> Result<(), String> {
        self.with_approvals(true, |a| {
            if !a.auth_commands.iter().any(|c| c == command) {
                a.auth_commands.push(command.to_string());
            }
        })
    }

    /// The token for an auth command, refusing to run an unapproved one.
    ///
    /// The approval check has to be here rather than in the token cache: a
    /// workspace is a shared, committed artefact, so "run this command" in a
    /// request file is remote code execution until the user has said yes to
    /// that exact command.
    pub async fn auth_token(
        &self,
        command: &str,
        stale_epoch: Option<u64>,
    ) -> Result<swarmo_load::auth::Token, String> {
        if !self.auth_command_approved(command) {
            return Err(unapproved_command_error(command));
        }
        match stale_epoch {
            Some(epoch) => self.tokens.refresh(command, epoch).await,
            None => self.tokens.get(command).await,
        }
    }

    /// Refuse an operation that would trample a run still in progress.
    ///
    /// Deleting a live run only clears the directory the engine is about to
    /// write its summary into, so the run would reappear moments later.
    pub fn ensure_run_not_active(&self, run_id: &str) -> Result<(), String> {
        if self
            .runs
            .lock()
            .map_err(|_| lock_err())?
            .contains_key(run_id)
        {
            return Err("This run is still in progress. Stop it first.".to_string());
        }
        Ok(())
    }

    /// A running load test owns the workspace it was planned from and writes
    /// its result there. Do not let the UI detach that run by switching the
    /// process-wide workspace underneath it.
    pub fn ensure_no_active_runs(&self) -> Result<(), String> {
        if self.runs.lock().map_err(|_| lock_err())?.is_empty() {
            Ok(())
        } else {
            Err("A load run is still in progress. Stop it before changing workspaces.".into())
        }
    }

    /// Swap in the history belonging to a newly opened workspace.
    pub fn load_history_for(&self, store: &WorkspaceStore) {
        let entries = store.load_history();
        if let Ok(mut h) = self.history.lock() {
            *h = entries.into_iter().take(MAX_HISTORY).collect();
        }
    }

    /// Forget one recorded send.
    pub fn delete_history(&self, id: &str) -> Result<(), String> {
        let store = self.store.lock().map_err(|_| lock_err())?.clone();
        // The revision is reserved under the history lock, as in
        // `push_history`, so a concurrent send cannot slip in between.
        let (snapshot, revision) = {
            let mut h = self.history.lock().map_err(|_| lock_err())?;
            h.retain(|e| e.id != id);
            let revision = match &store {
                Some(store) => Some(self.history_persistence.reserve(store.root())?),
                None => None,
            };
            (h.iter().cloned().collect::<Vec<_>>(), revision)
        };
        if let (Some(store), Some(revision)) = (store, revision) {
            self.history_persistence
                .save_if_current(revision, &store, &snapshot)?;
        }
        Ok(())
    }

    /// Forget every recorded send, on disk as well as in memory.
    pub fn clear_history(&self) -> Result<(), String> {
        let store = self.store.lock().map_err(|_| lock_err())?.clone();
        // Held until the revision is reserved, as in `push_history`. A
        // poisoned lock still clears the file: that is what was asked for.
        let mut history = self.history.lock().ok();
        if let Some(h) = history.as_mut() {
            h.clear();
        }
        let revision = match &store {
            Some(store) => Some(self.history_persistence.reserve(store.root())?),
            None => None,
        };
        drop(history);
        if let (Some(store), Some(revision)) = (store, revision) {
            self.history_persistence
                .clear_if_current(revision, &store)?;
        }
        Ok(())
    }

    pub fn record_cookies(&self, new: &[CookieRecord]) {
        if new.is_empty() {
            return;
        }
        if let Ok(mut c) = self.cookies.lock() {
            for record in new {
                let name = cookie_name(&record.raw);
                c.retain(|e| !(e.domain == record.domain && cookie_name(&e.raw) == name));
                c.push(record.clone());
            }
        }
    }

    pub fn settings_snapshot(&self) -> Settings {
        self.settings.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn save_settings(&self) -> Result<(), String> {
        let path = {
            let p = self.settings_path.lock().map_err(|_| lock_err())?;
            match p.as_ref() {
                Some(p) => p.clone(),
                None => return Ok(()),
            }
        };
        let settings = self.settings_snapshot();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| e.to_string())
    }

    pub fn load_settings(&self, path: PathBuf) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(s) = serde_json::from_str::<Settings>(&text) {
                if let Ok(mut guard) = self.settings.lock() {
                    *guard = s;
                }
            }
        }
        if let Ok(mut p) = self.settings_path.lock() {
            *p = Some(path);
        }
    }

    pub fn remember_workspace(&self, path: &str) {
        if let Ok(mut s) = self.settings.lock() {
            s.recent_workspaces.retain(|p| p != path);
            s.recent_workspaces.insert(0, path.to_string());
            s.recent_workspaces.truncate(10);
        }
        let _ = self.save_settings();
    }
}

/// Serializes history persistence without making request completion wait for
/// disk. Revisions are tracked per workspace so a late, older snapshot cannot
/// overwrite a newer send, delete, or clear operation.
#[derive(Default)]
struct HistoryPersistence {
    revisions: Mutex<HashMap<PathBuf, u64>>,
    write_lock: Mutex<()>,
}

impl HistoryPersistence {
    fn reserve(&self, root: &Path) -> Result<u64, String> {
        let mut revisions = self.revisions.lock().map_err(|_| lock_err())?;
        let revision = revisions.entry(root.to_path_buf()).or_default();
        *revision = revision.saturating_add(1);
        Ok(*revision)
    }

    fn is_current(&self, root: &Path, revision: u64) -> Result<bool, String> {
        Ok(self
            .revisions
            .lock()
            .map_err(|_| lock_err())?
            .get(root)
            .copied()
            == Some(revision))
    }

    fn save_if_current(
        &self,
        revision: u64,
        store: &WorkspaceStore,
        entries: &[HistoryEntry],
    ) -> Result<(), String> {
        let _write = self.write_lock.lock().map_err(|_| lock_err())?;
        if self.is_current(store.root(), revision)? {
            store.save_history(entries).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn clear_if_current(&self, revision: u64, store: &WorkspaceStore) -> Result<(), String> {
        let _write = self.write_lock.lock().map_err(|_| lock_err())?;
        if self.is_current(store.root(), revision)? {
            store.clear_history().map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

fn cookie_name(raw: &str) -> String {
    raw.split('=').next().unwrap_or("").trim().to_string()
}

pub fn lock_err() -> String {
    "Internal error: application state was poisoned by an earlier panic. Restart Swarmo."
        .to_string()
}

/// The marker the UI looks for to offer the approval dialog.
///
/// Carried in the error text rather than a typed error because every command
/// returns `Result<_, String>` to the frontend; the prefix is what lets the UI
/// tell "you must approve this" apart from "the command failed".
pub const UNAPPROVED_COMMAND_PREFIX: &str = "SWARMO_UNAPPROVED_COMMAND:";

pub fn unapproved_command_error(command: &str) -> String {
    format!("{UNAPPROVED_COMMAND_PREFIX}{command}")
}

/// Run a small file write off the async worker threads.
///
/// Falls back to running inline when there is no Tokio runtime — which is the
/// case in unit tests, and means a test can assert on the file immediately
/// after the call.
fn spawn_write<F: FnOnce() + Send + 'static>(f: F) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(f);
        }
        Err(_) => f(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swarmo_core::model::HistorySentRequest;
    use tempfile::TempDir;

    fn entry(id: &str) -> HistoryEntry {
        HistoryEntry {
            id: id.into(),
            request_ref: "collections/api/get.req.json".into(),
            request_id: None,
            name: "Get user".into(),
            protocol: swarmo_core::store::Protocol::Http,
            method: "GET".into(),
            url: "https://example.test/1".into(),
            status: 200,
            status_text: Some("OK".into()),
            duration_ms: 3.0,
            response_bytes: 10,
            at: 1,
            ok: true,
            error: None,
            request: HistorySentRequest::new(Vec::new(), None),
            response: swarmo_core::model::HistoryResponse::default(),
        }
    }

    fn stub_summary(run_id: &str) -> swarmo_core::model::RunSummary {
        let stats = swarmo_core::model::TagStats {
            tag: "overall".into(),
            count: 1,
            errors: 0,
            min: 1.0,
            p50: 1.0,
            p90: 1.0,
            p95: 1.0,
            p99: 1.0,
            p999: 1.0,
            max: 1.0,
            avg: 1.0,
            bytes_in: 0,
            bytes_out: 0,
        };
        swarmo_core::model::RunSummary {
            version: 1,
            run_id: run_id.into(),
            scenario_name: "s".into(),
            scenario_ref: "s.load.json".into(),
            scenario_id: None,
            started_at: 1,
            ended_at: 2,
            duration_sec: 1.0,
            state: swarmo_core::model::RunState::Running,
            error: None,
            total_requests: 1,
            total_errors: 0,
            error_rate: 0.0,
            rps: 1.0,
            overall: stats.clone(),
            per_tag: vec![stats],
            checks: Vec::new(),
            thresholds: Vec::new(),
            samples_dropped: 0,
            dropped_iterations: 0,
            status_codes: Vec::new(),
            errors_by_message: Vec::new(),
            bytes_in: 0,
            bytes_out: 0,
            bytes_per_sec: 0.0,
            bytes_out_per_sec: 0.0,
            peak_rps: 0.0,
            latency_distribution: Vec::new(),
            token_refreshes: 0,
            rounds: Vec::new(),
            stopped_because: None,
        }
    }

    fn state_with_workspace(tmp: &TempDir) -> (AppState, WorkspaceStore) {
        let store = WorkspaceStore::create(tmp.path().join("ws"), "test").unwrap();
        let state = AppState::new(tmp.path().join("tmp"));
        *state.store.lock().unwrap() = Some(store.clone());
        (state, store)
    }

    #[test]
    fn sends_are_written_through_to_the_workspace() {
        let tmp = TempDir::new().unwrap();
        let (state, store) = state_with_workspace(&tmp);

        state.push_history(entry("a"));
        state.push_history(entry("b"));

        // Written on each send, so a crash cannot lose the log.
        let on_disk = store.load_history();
        assert_eq!(on_disk.len(), 2);
        assert_eq!(on_disk[0].id, "b", "newest first");
    }

    #[test]
    fn history_reloads_when_a_workspace_is_opened() {
        let tmp = TempDir::new().unwrap();
        let (state, store) = state_with_workspace(&tmp);
        state.push_history(entry("a"));

        // A fresh process opening the same workspace sees the same history.
        let reopened = AppState::new(tmp.path().join("tmp"));
        reopened.load_history_for(&store);
        assert_eq!(reopened.history.lock().unwrap().len(), 1);
    }

    #[test]
    fn opening_another_workspace_replaces_the_history() {
        let tmp = TempDir::new().unwrap();
        let (state, _store) = state_with_workspace(&tmp);
        state.push_history(entry("a"));

        // History belongs to a workspace, so switching must not carry entries
        // from the previous one across.
        let other = WorkspaceStore::create(tmp.path().join("other"), "other").unwrap();
        state.load_history_for(&other);
        assert!(state.history.lock().unwrap().is_empty());
    }

    #[test]
    fn clearing_and_deleting_reach_the_file() {
        let tmp = TempDir::new().unwrap();
        let (state, store) = state_with_workspace(&tmp);
        state.push_history(entry("a"));
        state.push_history(entry("b"));

        state.delete_history("a").unwrap();
        assert_eq!(store.load_history().len(), 1);

        state.clear_history().unwrap();
        assert!(store.load_history().is_empty());
    }

    #[test]
    fn an_older_async_history_snapshot_cannot_restore_deleted_entries() {
        let tmp = TempDir::new().unwrap();
        let (state, store) = state_with_workspace(&tmp);
        let stale = vec![entry("stale")];

        // Model an async push that reserved its place before Clear History,
        // but did not reach the disk until afterwards.
        let old_revision = state.history_persistence.reserve(store.root()).unwrap();
        let clear_revision = state.history_persistence.reserve(store.root()).unwrap();
        state
            .history_persistence
            .clear_if_current(clear_revision, &store)
            .unwrap();
        state
            .history_persistence
            .save_if_current(old_revision, &store, &stale)
            .unwrap();

        assert!(store.load_history().is_empty());
    }

    #[tokio::test]
    async fn a_send_is_listable_before_its_write_completes() {
        let tmp = TempDir::new().unwrap();
        let (state, _store) = state_with_workspace(&tmp);

        // Inside a runtime the disk write is handed to a blocking thread, so
        // this asserts the in-memory push stayed synchronous: a history_list
        // straight after a send must already show it.
        state.push_history(entry("a"));
        assert_eq!(state.history.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_run_still_in_progress_cannot_be_deleted() {
        let tmp = TempDir::new().unwrap();
        let (state, store) = state_with_workspace(&tmp);
        store.save_run_summary(&stub_summary("live")).unwrap();
        state
            .runs
            .lock()
            .unwrap()
            .insert("live".into(), tokio_util::sync::CancellationToken::new());

        assert!(state.ensure_run_not_active("live").is_err());
        // Nothing was removed on the way to refusing.
        assert!(store.get_run("live").is_ok());

        // Once it is no longer registered, deleting is allowed again.
        state.runs.lock().unwrap().remove("live");
        assert!(state.ensure_run_not_active("live").is_ok());
    }

    #[test]
    fn a_workspace_cannot_be_detached_from_an_active_run() {
        let tmp = TempDir::new().unwrap();
        let state = AppState::new(tmp.path().join("tmp"));
        state
            .runs
            .lock()
            .unwrap()
            .insert("live".into(), CancellationToken::new());

        assert!(state.ensure_no_active_runs().is_err());
        state.runs.lock().unwrap().clear();
        assert!(state.ensure_no_active_runs().is_ok());
    }

    /// A command that leaves evidence, so a test can prove it never ran.
    fn marker_command(path: &std::path::Path) -> String {
        let p = path.display();
        if cfg!(windows) {
            format!("echo ran > \"{p}\" && echo tok")
        } else {
            format!("echo ran > '{p}'; echo tok")
        }
    }

    #[tokio::test]
    async fn an_unapproved_command_is_never_executed() {
        let tmp = TempDir::new().unwrap();
        let (state, _store) = state_with_workspace(&tmp);
        let marker = tmp.path().join("evidence.txt");
        let command = marker_command(&marker);

        let err = state.auth_token(&command, None).await.unwrap_err();
        assert!(err.starts_with(UNAPPROVED_COMMAND_PREFIX), "{err}");
        // The gate is not advisory: the process must not have started.
        assert!(
            !marker.exists(),
            "an unapproved command was executed anyway"
        );
    }

    #[tokio::test]
    async fn approval_persists_and_is_per_exact_command() {
        let tmp = TempDir::new().unwrap();
        let (state, store) = state_with_workspace(&tmp);

        state.approve_auth_command("echo one").unwrap();
        assert!(state.auth_command_approved("echo one"));
        assert_eq!(state.approved_auth_commands().unwrap(), ["echo one"]);
        // Kept out of the workspace, which is shared: nothing a workspace
        // contains can grant its own approval.
        let manifest = std::fs::read_to_string(store.root().join("swarmo.json")).unwrap();
        assert!(!manifest.contains("echo one"), "{manifest}");

        // Editing the command means it must be approved again — otherwise
        // approving once would approve anything the file later said.
        assert!(!state.auth_command_approved("echo one && curl evil.test"));
        assert!(!state.auth_command_approved("echo ONE"));
    }

    #[tokio::test]
    async fn approving_twice_does_not_duplicate_the_entry() {
        let tmp = TempDir::new().unwrap();
        let (state, store) = state_with_workspace(&tmp);
        state.approve_auth_command("echo one").unwrap();
        state.approve_auth_command("echo one").unwrap();
        assert_eq!(state.approved_auth_commands().unwrap().len(), 1);
        drop(store);
    }

    #[test]
    fn a_workspace_cannot_approve_its_own_command() {
        let tmp = TempDir::new().unwrap();
        let (state, store) = state_with_workspace(&tmp);
        // What a hostile shared workspace would ship.
        std::fs::write(
            store.root().join("swarmo.json"),
            r#"{"version":1,"name":"test","approvedAuthCommands":["echo pwned"],
                "approvedLoadHosts":["prod.example.com"]}"#,
        )
        .unwrap();

        assert!(!state.auth_command_approved("echo pwned"));
        assert!(state.approved_load_hosts().unwrap().is_empty());
    }

    #[test]
    fn approvals_survive_a_restart_and_stay_with_their_workspace() {
        let tmp = TempDir::new().unwrap();
        let approvals = tmp.path().join("config").join("approvals.json");
        let (state, store) = state_with_workspace(&tmp);
        state.load_approvals(approvals.clone());
        state.approve_auth_command("echo one").unwrap();
        state.approve_load_hosts(&["api.test".into()]).unwrap();

        // A fresh process reading the same file.
        let again = AppState::new(tmp.path().join("tmp2"));
        again.load_approvals(approvals);
        *again.store.lock().unwrap() = Some(store);
        assert!(again.auth_command_approved("echo one"));
        assert_eq!(again.approved_load_hosts().unwrap(), ["api.test"]);

        // Another workspace starts with nothing approved.
        let other = WorkspaceStore::create(tmp.path().join("other"), "other").unwrap();
        *again.store.lock().unwrap() = Some(other);
        assert!(!again.auth_command_approved("echo one"));
        assert!(again.approved_load_hosts().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_expired_token_is_refreshed_and_the_request_retried() {
        // The whole point of the feature, end to end against a real server:
        // the first credential is rejected, the second is accepted, and the
        // send succeeds without the user doing anything.
        let (addr, _srv) = echo_server::spawn().await;
        let tmp = TempDir::new().unwrap();
        let (state, _store) = state_with_workspace(&tmp);

        // A command whose output differs each time it runs, standing in for a
        // credential that has been reissued.
        let command = if cfg!(windows) {
            "echo %TIME%".to_string()
        } else {
            "date +%s%N".to_string()
        };
        state.approve_auth_command(&command).unwrap();

        let url = format!("http://{addr}/expiring-auth");
        let client = reqwest::Client::new();

        // First attempt with the cached token: rejected.
        let first = state.auth_token(&command, None).await.unwrap();
        let res = client
            .get(&url)
            .header("authorization", format!("Bearer {}", first.value))
            .send()
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            401,
            "the server should reject the stale token"
        );

        // Refresh once and retry, which is exactly what the send path does.
        let fresh = state.auth_token(&command, Some(first.epoch)).await.unwrap();
        assert_ne!(
            fresh.value, first.value,
            "the token did not actually change"
        );
        let res = client
            .get(&url)
            .header("authorization", format!("Bearer {}", fresh.value))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200, "the refreshed token should be accepted");
    }

    #[test]
    fn recording_without_a_workspace_does_not_panic() {
        let tmp = TempDir::new().unwrap();
        let state = AppState::new(tmp.path().join("tmp"));
        state.push_history(entry("a"));
        assert_eq!(state.history.lock().unwrap().len(), 1);
    }
}
