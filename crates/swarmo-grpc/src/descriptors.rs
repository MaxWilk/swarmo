//! Loading service schemas, either by compiling `.proto` files with protox or
//! by asking a server over the reflection service.

use std::path::{Path, PathBuf};

use prost_reflect::{DescriptorPool, Kind, MessageDescriptor, MethodDescriptor, ServiceDescriptor};
use serde::{Deserialize, Serialize};

use crate::error::{GrpcError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MethodInfo {
    pub name: String,
    /// Fully-qualified input message type.
    pub input: String,
    pub output: String,
    pub client_streaming: bool,
    pub server_streaming: bool,
}

impl MethodInfo {
    pub fn is_streaming(&self) -> bool {
        self.client_streaming || self.server_streaming
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInfo {
    /// Fully-qualified service name.
    pub name: String,
    pub methods: Vec<MethodInfo>,
}

/// A compiled schema plus a note of where it came from, for error messages.
#[derive(Debug, Clone)]
pub struct DescriptorSource {
    pool: DescriptorPool,
    origin: String,
    /// Things that were skipped while loading, so a service that is absent can
    /// explain why rather than just not being in the list.
    warnings: Vec<String>,
}

impl DescriptorSource {
    pub fn from_pool(pool: DescriptorPool, origin: impl Into<String>) -> Self {
        Self {
            pool,
            origin: origin.into(),
            warnings: Vec::new(),
        }
    }

    pub fn with_warnings(mut self, warnings: Vec<String>) -> Self {
        self.warnings = warnings;
        self
    }

    pub fn pool(&self) -> &DescriptorPool {
        &self.pool
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Every service in the schema, excluding the reflection service itself.
    pub fn services(&self) -> Vec<ServiceInfo> {
        let mut out: Vec<ServiceInfo> = self
            .pool
            .services()
            .filter(|s| !is_reflection_service(s.full_name()))
            .map(|s| ServiceInfo {
                name: s.full_name().to_string(),
                methods: s
                    .methods()
                    .map(|m| MethodInfo {
                        name: m.name().to_string(),
                        input: m.input().full_name().to_string(),
                        output: m.output().full_name().to_string(),
                        client_streaming: m.is_client_streaming(),
                        server_streaming: m.is_server_streaming(),
                    })
                    .collect(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn service_names(&self) -> Vec<String> {
        self.services().into_iter().map(|s| s.name).collect()
    }

    fn service(&self, name: &str) -> Result<ServiceDescriptor> {
        self.pool
            .get_service_by_name(name)
            .ok_or_else(|| GrpcError::ServiceNotFound {
                wanted: name.to_string(),
                available: self.service_names(),
                // If the service is missing because its schema was incomplete,
                // say so here — otherwise it just silently is not in the list.
                notes: self.warnings.clone(),
            })
    }

    /// Look up a method, refusing streaming ones.
    /// Look a method up, whatever its streaming shape.
    pub fn method(&self, service: &str, method: &str) -> Result<MethodDescriptor> {
        let svc = self.service(service)?;
        // Bound to a local first: as a tail expression the iterator's borrow
        // of `svc` would outlive `svc` itself.
        let found = svc.methods().find(|m| m.name() == method).ok_or_else(|| {
            GrpcError::MethodNotFound {
                service: service.to_string(),
                method: method.to_string(),
                available: svc.methods().map(|m| m.name().to_string()).collect(),
            }
        })?;
        Ok(found)
    }

    /// Look a unary method up, refusing streaming ones. Kept for callers that
    /// genuinely need one request and one reply; [`Self::method`] is the
    /// general lookup now that streaming calls are supported.
    pub fn unary_method(&self, service: &str, method: &str) -> Result<MethodDescriptor> {
        let m = self.method(service, method)?;
        if m.is_client_streaming() || m.is_server_streaming() {
            return Err(GrpcError::Streaming {
                service: service.to_string(),
                method: method.to_string(),
            });
        }
        Ok(m)
    }

    /// A skeleton request message, so the editor can pre-fill something valid.
    pub fn message_template(&self, service: &str, method: &str) -> Result<String> {
        let svc = self.service(service)?;
        let m = svc.methods().find(|m| m.name() == method).ok_or_else(|| {
            GrpcError::MethodNotFound {
                service: service.to_string(),
                method: method.to_string(),
                available: svc.methods().map(|m| m.name().to_string()).collect(),
            }
        })?;
        let value = skeleton(&m.input(), 0);
        serde_json::to_string_pretty(&value)
            .map_err(|e| GrpcError::descriptor(format!("could not render a template: {e}")))
    }
}

fn is_reflection_service(name: &str) -> bool {
    name.starts_with("grpc.reflection.")
}

/// Build an example JSON value for a message. Recurses one level into nested
/// messages (`depth < 1`) so the skeleton is useful without being enormous or
/// looping on self-referential types.
fn skeleton(msg: &MessageDescriptor, depth: usize) -> serde_json::Value {
    use serde_json::{Map, Value};
    let mut obj = Map::new();
    // A oneof may carry only one of its fields; naming them all makes a
    // template the parser rejects. The first stands in for the rest.
    let mut oneofs_used: Vec<String> = Vec::new();

    for field in msg.fields() {
        if let Some(oneof) = field.containing_oneof() {
            if !oneof.is_synthetic() {
                if oneofs_used.iter().any(|n| n == oneof.full_name()) {
                    continue;
                }
                oneofs_used.push(oneof.full_name().to_string());
            }
        }
        // proto3 JSON accepts either; lowerCamelCase is the canonical form and
        // what servers emit, so match it.
        let key = field.json_name().to_string();

        let value = if field.is_map() {
            Value::Object(Map::new())
        } else if field.is_list() {
            Value::Array(Vec::new())
        } else {
            match field.kind() {
                Kind::Double | Kind::Float => Value::from(0.0),
                Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 | Kind::Uint32 | Kind::Fixed32 => {
                    Value::from(0)
                }
                // 64-bit ints are strings in proto3 JSON.
                Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 | Kind::Uint64 | Kind::Fixed64 => {
                    Value::from("0")
                }
                Kind::Bool => Value::from(false),
                Kind::String => Value::from(""),
                Kind::Bytes => Value::from(""),
                Kind::Enum(e) => e
                    .values()
                    .next()
                    .map(|v| Value::from(v.name().to_string()))
                    .unwrap_or(Value::Null),
                Kind::Message(inner) => {
                    if let Some(wk) = well_known_example(inner.full_name()) {
                        wk
                    } else if depth < 1 {
                        skeleton(&inner, depth + 1)
                    } else {
                        Value::Object(Map::new())
                    }
                }
            }
        };
        obj.insert(key, value);
    }

    Value::Object(obj)
}

/// Well-known types have a scalar JSON form, so a nested skeleton would be wrong.
fn well_known_example(full_name: &str) -> Option<serde_json::Value> {
    use serde_json::Value;
    Some(match full_name {
        "google.protobuf.Timestamp" => Value::from("1970-01-01T00:00:00Z"),
        "google.protobuf.Duration" => Value::from("0s"),
        "google.protobuf.StringValue" => Value::from(""),
        "google.protobuf.BoolValue" => Value::from(false),
        "google.protobuf.Int32Value" | "google.protobuf.UInt32Value" => Value::from(0),
        "google.protobuf.Int64Value" | "google.protobuf.UInt64Value" => Value::from("0"),
        "google.protobuf.DoubleValue" | "google.protobuf.FloatValue" => Value::from(0.0),
        "google.protobuf.BytesValue" => Value::from(""),
        "google.protobuf.Empty" => Value::Object(serde_json::Map::new()),
        "google.protobuf.Value" => Value::Null,
        "google.protobuf.Struct" => Value::Object(serde_json::Map::new()),
        "google.protobuf.ListValue" => Value::Array(Vec::new()),
        "google.protobuf.FieldMask" => Value::from(""),
        // An Any needs an "@type" naming a real message; null leaves it unset
        // rather than inventing one the parser would reject.
        "google.protobuf.Any" => Value::Null,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Loading from .proto files
// ---------------------------------------------------------------------------

/// Compile `.proto` files with protox. No `protoc` binary is involved.
///
/// `include_paths` are the import roots. When empty, each file's parent
/// directory is used, which is what a single-file schema almost always wants.
pub fn load_from_files(files: &[PathBuf], include_paths: &[PathBuf]) -> Result<DescriptorSource> {
    if files.is_empty() {
        return Err(GrpcError::descriptor(
            "No .proto files are configured. Add one, or switch this request to server reflection.",
        ));
    }

    for f in files {
        if !f.is_file() {
            return Err(GrpcError::descriptor(format!(
                "The .proto file was not found: {}",
                f.display()
            )));
        }
    }

    let mut includes: Vec<PathBuf> = include_paths.to_vec();
    if includes.is_empty() {
        for f in files {
            if let Some(parent) = f.parent() {
                if !includes.contains(&parent.to_path_buf()) {
                    includes.push(parent.to_path_buf());
                }
            }
        }
    }
    for inc in &includes {
        if !inc.is_dir() {
            return Err(GrpcError::descriptor(format!(
                "The import path is not a directory: {}",
                inc.display()
            )));
        }
    }

    // protox resolves each file against the include paths, so pass whichever
    // form it can actually find: a path relative to an include root when
    // possible, otherwise the absolute path with its own parent as a root.
    let mut resolved_files: Vec<PathBuf> = Vec::new();
    for f in files {
        let rel = includes
            .iter()
            .find_map(|inc| f.strip_prefix(inc).ok().map(|r| r.to_path_buf()));
        match rel {
            Some(r) => resolved_files.push(r),
            None => {
                if let Some(parent) = f.parent() {
                    if !includes.contains(&parent.to_path_buf()) {
                        includes.push(parent.to_path_buf());
                    }
                }
                let name = f
                    .file_name()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| f.clone());
                resolved_files.push(name);
            }
        }
    }

    let fds = protox::compile(&resolved_files, &includes).map_err(|e| {
        // protox renders good diagnostics (file, line, column); keep them.
        GrpcError::descriptor(format!("Could not compile the .proto files.\n{e}"))
    })?;

    let pool = DescriptorPool::from_file_descriptor_set(fds)
        .map_err(|e| GrpcError::descriptor(format!("Could not build a descriptor pool: {e}")))?;

    let origin = files
        .iter()
        .map(|f| f.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(DescriptorSource::from_pool(pool, origin))
}

// ---------------------------------------------------------------------------
// Loading a whole directory
// ---------------------------------------------------------------------------

/// Cap on how many `.proto` files one directory source will compile, so
/// pointing at a huge tree by mistake fails fast with a clear message rather
/// than appearing to hang.
pub const MAX_PROTOS_IN_DIRECTORY: usize = 4_000;
const MAX_SCAN_DEPTH: usize = 24;

/// Every `.proto` beneath `root`, as paths relative to `root`.
///
/// Import statements in a proto tree are written relative to a common root, so
/// scanning gives us both the file list and the single include path we need.
pub fn scan_protos(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.is_dir() {
        return Err(GrpcError::descriptor(format!(
            "The proto folder was not found: {}",
            root.display()
        )));
    }

    let mut found = Vec::new();
    walk(root, root, 0, &mut found)?;

    if found.is_empty() {
        return Err(GrpcError::descriptor(format!(
            "No .proto files were found under {}. Pick the folder that directly \
             contains the first segment of your import paths — for a schema \
             importing \"tensorflow_serving/apis/predict.proto\", that is the \
             folder holding the \"tensorflow_serving\" directory.",
            root.display()
        )));
    }

    found.sort();
    Ok(found)
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<()> {
    if depth > MAX_SCAN_DEPTH || out.len() >= MAX_PROTOS_IN_DIRECTORY {
        return Ok(());
    }

    let entries = std::fs::read_dir(dir)
        .map_err(|e| GrpcError::descriptor(format!("Could not read {}: {e}", dir.display())))?;

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip the usual noise; a vendored proto tree often sits next to them.
        if name.starts_with('.') || name == "node_modules" || name == "target" {
            continue;
        }

        match entry.file_type() {
            // Do not follow symlinks: a self-referential link would loop.
            Ok(t) if t.is_symlink() => continue,
            Ok(t) if t.is_dir() => walk(root, &path, depth + 1, out)?,
            Ok(t) if t.is_file() && path.extension().is_some_and(|e| e == "proto") => {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_path_buf());
                }
            }
            _ => {}
        }

        if out.len() >= MAX_PROTOS_IN_DIRECTORY {
            return Err(GrpcError::descriptor(format!(
                "More than {MAX_PROTOS_IN_DIRECTORY} .proto files were found under {}. \
                 Point at a narrower folder, or list specific entry files.",
                root.display()
            )));
        }
    }
    Ok(())
}

/// How many directory levels above the chosen folder we are willing to look
/// when an import resolves to a sibling tree.
const MAX_ANCESTOR_LOOKUP: usize = 5;
/// Bound on the dependency-closure loop.
const MAX_RESOLUTION_ROUNDS: usize = 12;

/// What a folder scan worked out about a proto tree.
#[derive(Debug, Clone)]
pub struct DiscoveredTree {
    /// Import roots, outermost first. These are what get passed as `-I`.
    pub roots: Vec<PathBuf>,
    /// File names relative to one of `roots`, in the form imports refer to them.
    pub files: Vec<PathBuf>,
    /// Imports we could not find anywhere.
    pub missing: Vec<String>,
}

impl DiscoveredTree {
    /// Where an import name lands on disk, or `None` if nothing provides it.
    ///
    /// Resolution goes through the import roots rather than the scanned file
    /// list, because a dependency can be satisfied by a root without having
    /// been scanned — that is exactly what happens when the root sits above the
    /// folder that was picked.
    pub fn resolve(&self, import_name: &str) -> Option<PathBuf> {
        self.roots
            .iter()
            .map(|r| r.join(import_name))
            .find(|p| p.is_file())
    }
}

/// Work out how to compile a proto tree from nothing but a folder.
///
/// Pointing at a folder is only useful if we can figure out what the tree's
/// import root actually is, because `import "a/b/c.proto"` has to resolve to a
/// real path. Rather than making people reason about that, this reads the
/// import statements and derives the roots from them: if some discovered file's
/// path ends with an imported path, the part before it is a root. That
/// correctly handles the common case where the folder you would naturally pick
/// sits *below* the root the imports are written against, and it will reach a
/// sibling directory when a dependency lives there.
pub fn discover_tree(root: &Path) -> Result<DiscoveredTree> {
    let scanned = scan_protos(root)?;
    let mut absolute: Vec<PathBuf> = scanned.iter().map(|rel| root.join(rel)).collect();

    // Candidate import roots, starting with the folder itself.
    let mut roots: Vec<PathBuf> = vec![root.to_path_buf()];

    // Ancestors are only consulted when an import cannot be satisfied inside
    // the folder, so a wide repository is never scanned speculatively.
    let ancestors: Vec<PathBuf> = root
        .ancestors()
        .skip(1)
        .take(MAX_ANCESTOR_LOOKUP)
        .map(Path::to_path_buf)
        .collect();

    let mut missing: Vec<String> = Vec::new();

    for _ in 0..MAX_RESOLUTION_ROUNDS {
        // Derive roots from where imported paths actually land on disk.
        let imports = collect_imports(&absolute);
        for import in &imports {
            for file in &absolute {
                if let Some(r) = strip_path_suffix(file, import) {
                    if !roots.contains(&r) {
                        roots.push(r);
                    }
                }
            }
        }

        // Outermost first, so the name an import uses wins over a shorter one.
        roots.sort_by_key(|r| r.components().count());
        roots.dedup();

        // Anything still unresolved: look for it under a root or an ancestor.
        let mut added = false;
        missing.clear();
        for import in &imports {
            if is_well_known(import) || resolves_under(&roots, import) {
                continue;
            }
            match roots.iter().chain(ancestors.iter()).find_map(|dir| {
                let candidate = dir.join(import);
                candidate.is_file().then_some((dir.clone(), candidate))
            }) {
                Some((dir, file)) => {
                    if !roots.contains(&dir) {
                        roots.push(dir);
                    }
                    if !absolute.contains(&file) {
                        absolute.push(file);
                    }
                    added = true;
                }
                None => missing.push(import.clone()),
            }
        }

        if !added {
            break;
        }
    }

    // Name each file the way the tree's own imports name it.
    let imports = collect_imports(&absolute);
    let mut files: Vec<PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for file in &absolute {
        let candidates: Vec<PathBuf> = roots
            .iter()
            .filter_map(|r| file.strip_prefix(r).ok().map(Path::to_path_buf))
            .collect();
        if candidates.is_empty() {
            continue;
        }
        // Prefer the form some import actually uses; otherwise the outermost
        // root, which is the one imports are written against.
        let chosen = candidates
            .iter()
            .find(|c| imports.contains(&slashed(c)))
            .cloned()
            .unwrap_or_else(|| candidates[0].clone());

        // The same file reachable under two roots would otherwise be compiled
        // twice under different names, which protobuf rejects as a redefinition.
        if seen.insert(slashed(&chosen)) {
            files.push(chosen);
        }
    }

    files.sort();
    Ok(DiscoveredTree {
        roots,
        files,
        missing,
    })
}

/// Compile a proto tree from a folder, working out the import roots itself.
///
/// `entry_files` optionally narrows what gets compiled (their imports are still
/// pulled in); empty means compile everything found.
pub fn load_from_directory(root: &Path, entry_files: &[String]) -> Result<DescriptorSource> {
    let tree = discover_tree(root)?;

    // Nothing here defines a service, so this is the wrong folder — say that
    // plainly instead of reporting whatever import happened to be dangling.
    if !tree.files.iter().any(|rel| {
        tree.resolve(&slashed(rel))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .is_some_and(|t| declares_a_service(&t))
    }) {
        return Err(GrpcError::descriptor(format!(
            "No .proto file under {} declares a service, so there is nothing to call. \
             Pick the folder that holds your service definitions — often the parent \
             of this one, or a sibling.",
            root.display()
        )));
    }

    let targets: Vec<PathBuf> = if entry_files.is_empty() {
        tree.files.clone()
    } else {
        let mut out = Vec::new();
        for e in entry_files {
            let wanted = e.replace('\\', "/");
            let found = tree
                .files
                .iter()
                .find(|f| slashed(f) == wanted || slashed(f).ends_with(&format!("/{wanted}")));
            match found {
                Some(f) => out.push(f.clone()),
                None => {
                    return Err(GrpcError::descriptor(format!(
                        "The entry file {wanted} was not found under {}",
                        root.display()
                    )))
                }
            }
        }

        // Named entry points are a deliberate choice, so an unsatisfiable one is
        // an error rather than something to quietly skip.
        for f in &out {
            let missing = import_closure(&slashed(f), &tree).1;
            if !missing.is_empty() {
                return Err(GrpcError::descriptor(format!(
                    "{} cannot be compiled: it needs {}, which {} not in {}. Copy \
                     the missing file(s) in, keeping the directory structure their \
                     import paths describe.",
                    slashed(f),
                    list(&missing),
                    if missing.len() == 1 { "is" } else { "are" },
                    root.display()
                )));
            }
        }
        out
    };

    let compile = |files: &[PathBuf]| protox::compile(files, &tree.roots);
    let mut warnings: Vec<String> = Vec::new();

    // Ideal case: the whole tree compiles, giving the richest descriptor pool.
    let fds = match compile(&targets) {
        Ok(fds) => fds,
        Err(all_err) => {
            // Vendored trees are routinely incomplete in corners nobody uses —
            // an orphaned file importing something that was never copied, or a
            // proto that only compiles in some other build context. Neither
            // should stop the services that *are* complete from working, so
            // narrow down rather than giving up.
            let (usable, skipped) = partition_by_satisfiability(&targets, &tree);
            for (file, missing) in &skipped {
                warnings.push(format!("{file} was skipped: it needs {}", list(missing)));
            }

            let attempt = if usable.len() < targets.len() {
                compile(&usable)
            } else {
                Err(all_err)
            };

            match attempt {
                Ok(fds) => fds,
                Err(narrowed_err) => {
                    // Last resort: just the files that declare a service, which
                    // is all a gRPC client actually needs, plus their imports.
                    let services: Vec<PathBuf> = usable
                        .iter()
                        .filter(|rel| {
                            tree.resolve(&slashed(rel))
                                .and_then(|p| std::fs::read_to_string(p).ok())
                                .is_some_and(|t| declares_a_service(&t))
                        })
                        .cloned()
                        .collect();

                    if services.is_empty() {
                        return Err(no_services_error(root, &tree, &skipped, &narrowed_err));
                    }
                    compile(&services).map_err(|e| {
                        GrpcError::descriptor(format!(
                            "Could not compile the .proto files under {}.\n{e}",
                            root.display()
                        ))
                    })?
                }
            }
        }
    };

    let pool = DescriptorPool::from_file_descriptor_set(fds)
        .map_err(|e| GrpcError::descriptor(format!("Could not build a descriptor pool: {e}")))?;

    let source = DescriptorSource::from_pool(
        pool,
        format!(
            "{} ({} .proto files, {} import root{})",
            root.display(),
            targets.len(),
            tree.roots.len(),
            if tree.roots.len() == 1 { "" } else { "s" }
        ),
    );

    if source.services().is_empty() {
        return Err(no_services_error(
            root,
            &tree,
            &partition_by_satisfiability(&targets, &tree).1,
            &"no service definitions were compiled",
        ));
    }

    Ok(source.with_warnings(warnings))
}

/// Split files into those whose imports all resolve and those that are missing
/// something, along with what each one is missing.
fn partition_by_satisfiability(
    targets: &[PathBuf],
    tree: &DiscoveredTree,
) -> (Vec<PathBuf>, Vec<(String, Vec<String>)>) {
    let mut usable = Vec::new();
    let mut skipped = Vec::new();
    for f in targets {
        let name = slashed(f);
        let (_, missing) = import_closure(&name, tree);
        if missing.is_empty() {
            usable.push(f.clone());
        } else {
            skipped.push((name, missing));
        }
    }
    (usable, skipped)
}

fn no_services_error(
    root: &Path,
    tree: &DiscoveredTree,
    skipped: &[(String, Vec<String>)],
    cause: &dyn std::fmt::Display,
) -> GrpcError {
    // Lead with what the person can act on: which files are missing.
    let mut all_missing: Vec<String> = skipped
        .iter()
        .flat_map(|(_, m)| m.iter().cloned())
        .chain(tree.missing.iter().cloned())
        .collect();
    all_missing.sort();
    all_missing.dedup();

    if all_missing.is_empty() {
        return GrpcError::descriptor(format!(
            "No services could be compiled from {}.\n{cause}",
            root.display()
        ));
    }

    GrpcError::descriptor(format!(
        "No services could be compiled from {}. These imported files are missing: {}. \
         Copy them in, keeping the directory structure their import paths describe.",
        root.display(),
        list(&all_missing)
    ))
}

/// Everything a file imports, transitively, and whatever could not be found.
fn import_closure(
    start: &str,
    tree: &DiscoveredTree,
) -> (std::collections::HashSet<String>, Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    let mut missing = std::collections::BTreeSet::new();
    let mut stack = vec![start.to_string()];

    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        // protox bundles the well-known types, so they never need a file.
        if is_well_known(&name) {
            continue;
        }
        match tree.resolve(&name) {
            None => {
                missing.insert(name);
            }
            Some(path) => {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    stack.extend(parse_imports(&text));
                }
            }
        }
    }

    seen.remove(start);
    (seen, missing.into_iter().collect())
}

fn list(items: &[String]) -> String {
    const SHOWN: usize = 6;
    let head = items
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    match items.len().saturating_sub(SHOWN) {
        0 => head,
        more => format!("{head}, and {more} more"),
    }
}

/// Every path named by an `import` statement across these files.
fn collect_imports(files: &[PathBuf]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for f in files {
        if let Ok(text) = std::fs::read_to_string(f) {
            for i in parse_imports(&text) {
                out.insert(i);
            }
        }
    }
    out
}

/// Pull the paths out of `import "...";` lines.
///
/// Deliberately a light scan rather than a parse: protox does the real parsing,
/// and all we need here is enough to work out the import roots.
fn parse_imports(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with("import") {
            continue;
        }
        // Covers `import "x";`, `import public "x";` and `import weak "x";`.
        let Some(start) = line.find('"') else {
            continue;
        };
        let rest = &line[start + 1..];
        let Some(end) = rest.find('"') else { continue };
        let path = rest[..end].trim();
        if !path.is_empty() {
            out.push(path.replace('\\', "/"));
        }
    }
    out
}

fn declares_a_service(text: &str) -> bool {
    text.lines().any(|l| l.trim_start().starts_with("service "))
}

/// If `file` ends with `import_path`, the part before it is an import root.
fn strip_path_suffix(file: &Path, import_path: &str) -> Option<PathBuf> {
    let file_s = slashed(file);
    let suffix = format!("/{import_path}");
    let head = file_s.strip_suffix(&suffix)?;
    Some(PathBuf::from(head))
}

fn resolves_under(roots: &[PathBuf], import_path: &str) -> bool {
    roots.iter().any(|r| r.join(import_path).is_file())
}

/// protox bundles the well-known types, so those imports never need a file.
fn is_well_known(import_path: &str) -> bool {
    import_path.starts_with("google/protobuf/")
}

fn slashed(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Resolve a possibly-workspace-relative proto path.
pub fn resolve_path(raw: &str, workspace_root: &Path) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else {
        workspace_root.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> DescriptorSource {
        load_from_files(
            &[grpc_test_server::proto_file()],
            &[grpc_test_server::proto_dir()],
        )
        .expect("test proto should compile")
    }

    #[test]
    fn compiles_a_proto_and_lists_services() {
        let s = source();
        let services = s.services();
        assert_eq!(services.len(), 1, "{:?}", s.service_names());
        assert_eq!(services[0].name, "swarmo.testing.TestService");

        let names: Vec<&str> = services[0]
            .methods
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        for want in ["Echo", "Delay", "Fail", "Login", "StreamNumbers"] {
            assert!(names.contains(&want), "missing {want} in {names:?}");
        }
    }

    #[test]
    fn streaming_methods_are_flagged() {
        let s = source();
        let svc = &s.services()[0];
        let echo = svc.methods.iter().find(|m| m.name == "Echo").unwrap();
        assert!(!echo.is_streaming());
        let stream = svc
            .methods
            .iter()
            .find(|m| m.name == "StreamNumbers")
            .unwrap();
        assert!(stream.server_streaming);
        assert!(stream.is_streaming());
    }

    #[test]
    fn unary_lookup_refuses_streaming() {
        let s = source();
        assert!(s.unary_method("swarmo.testing.TestService", "Echo").is_ok());
        let err = s
            .unary_method("swarmo.testing.TestService", "StreamNumbers")
            .unwrap_err();
        assert!(matches!(err, GrpcError::Streaming { .. }));
        assert!(err.to_string().contains("unary calls only"));
    }

    #[test]
    fn unknown_service_and_method_list_what_exists() {
        let s = source();
        let err = s.unary_method("nope.Service", "X").unwrap_err();
        assert!(err.to_string().contains("swarmo.testing.TestService"));

        let err = s
            .unary_method("swarmo.testing.TestService", "Nope")
            .unwrap_err();
        assert!(err.to_string().contains("Echo"), "{err}");
        assert!(err.is_schema_stale());
    }

    #[test]
    fn message_template_covers_scalars_nesting_and_well_known_types() {
        let s = source();
        let text = s
            .message_template("swarmo.testing.TestService", "Echo")
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert_eq!(v["message"], "");
        assert_eq!(v["number"], 0);
        assert_eq!(v["flag"], false);
        // A nested message is expanded one level.
        assert!(v["nested"].is_object());
        assert_eq!(v["nested"]["label"], "");
        assert!(v["nested"]["values"].is_array());
        // Timestamp renders as its JSON scalar form, not a nested object.
        assert_eq!(v["at"], "1970-01-01T00:00:00Z");
        // Maps are objects.
        assert!(v["tags"].is_object());
    }

    #[test]
    fn templates_with_oneofs_and_any_parse_as_they_are() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("t.proto"),
            r#"syntax = "proto3";
               package t;
               import "google/protobuf/any.proto";
               message Inner { oneof pick { string x = 1; int32 y = 2; } }
               message Req {
                 oneof choice { string a = 1; string b = 2; Inner c = 3; }
                 optional string note = 4;
                 google.protobuf.Any extra = 5;
                 Inner inner = 6;
               }
               service S { rpc Go(Req) returns (Req); }"#,
        );
        let s =
            load_from_files(&[dir.path().join("t.proto")], &[dir.path().to_path_buf()]).unwrap();
        let text = s.message_template("t.S", "Go").unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        // One member per oneof; a proto3 `optional` is still offered.
        assert!(v.get("a").is_some() && v.get("b").is_none() && v.get("c").is_none());
        assert!(v.get("note").is_some(), "{text}");
        assert!(v["inner"].get("x").is_some() && v["inner"].get("y").is_none());

        let input = s.method("t.S", "Go").unwrap().input();
        let mut de = serde_json::Deserializer::from_str(&text);
        prost_reflect::DynamicMessage::deserialize(input, &mut de)
            .unwrap_or_else(|e| panic!("the template must parse: {e}\n{text}"));
    }

    #[test]
    fn missing_proto_file_names_the_path() {
        let err = load_from_files(&[PathBuf::from("does/not/exist.proto")], &[]).unwrap_err();
        assert!(err.to_string().contains("exist.proto"), "{err}");
    }

    #[test]
    fn no_files_configured_is_actionable() {
        let err = load_from_files(&[], &[]).unwrap_err();
        assert!(err.to_string().contains("reflection"), "{err}");
    }

    /// A tree whose imports are written relative to a common root, the way
    /// TensorFlow Serving's and googleapis' schemas are laid out.
    fn nested_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::create_dir_all(root.join("common/v1")).unwrap();
        std::fs::write(
            root.join("common/v1/types.proto"),
            r#"syntax = "proto3";
               package common.v1;
               message Id { string value = 1; }"#,
        )
        .unwrap();

        std::fs::create_dir_all(root.join("shop/apis")).unwrap();
        std::fs::write(
            root.join("shop/apis/order.proto"),
            r#"syntax = "proto3";
               package shop.apis;
               import "common/v1/types.proto";
               message GetOrder { common.v1.Id id = 1; }
               message Order { common.v1.Id id = 1; string sku = 2; }"#,
        )
        .unwrap();
        std::fs::write(
            root.join("shop/apis/service.proto"),
            r#"syntax = "proto3";
               package shop.apis;
               import "shop/apis/order.proto";
               service OrderService { rpc Get(GetOrder) returns (Order); }"#,
        )
        .unwrap();

        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A tree laid out the way TensorFlow Serving's is: two sibling package
    /// directories, with imports written relative to the directory that holds
    /// them both.
    fn serving_like_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();

        write(
            &r.join("protos/tensorflow/core/framework/tensor.proto"),
            r#"syntax = "proto3";
               package tensorflow;
               message TensorProto { repeated float float_val = 1; }"#,
        );
        write(
            &r.join("protos/tensorflow_serving/apis/model.proto"),
            r#"syntax = "proto3";
               package tensorflow.serving;
               message ModelSpec { string name = 1; }"#,
        );
        write(
            &r.join("protos/tensorflow_serving/apis/predict.proto"),
            r#"syntax = "proto3";
               package tensorflow.serving;
               import "tensorflow/core/framework/tensor.proto";
               import "tensorflow_serving/apis/model.proto";
               message PredictRequest {
                 ModelSpec model_spec = 1;
                 map<string, tensorflow.TensorProto> inputs = 2;
               }
               message PredictResponse {
                 map<string, tensorflow.TensorProto> outputs = 1;
               }"#,
        );
        write(
            &r.join("protos/tensorflow_serving/apis/prediction_service.proto"),
            r#"syntax = "proto3";
               package tensorflow.serving;
               import "tensorflow_serving/apis/predict.proto";
               service PredictionService {
                 rpc Predict(PredictRequest) returns (PredictResponse);
               }"#,
        );

        dir
    }

    #[test]
    fn pointing_at_the_folder_holding_both_package_trees_works() {
        let dir = serving_like_tree();
        let source = load_from_directory(&dir.path().join("protos"), &[]).unwrap();
        assert_eq!(
            source.service_names(),
            vec!["tensorflow.serving.PredictionService".to_string()]
        );
    }

    #[test]
    fn pointing_one_level_too_deep_still_works() {
        // Someone naturally picks the folder named after the service. Its
        // imports are written against the level above, and a dependency lives
        // in a sibling directory. Both must be found.
        let dir = serving_like_tree();
        let picked = dir.path().join("protos/tensorflow_serving");

        let source = load_from_directory(&picked, &[])
            .unwrap_or_else(|e| panic!("pointing at {} should work: {e}", picked.display()));
        assert_eq!(
            source.service_names(),
            vec!["tensorflow.serving.PredictionService".to_string()]
        );
        // The sibling tree's type came along.
        assert!(source
            .pool()
            .get_message_by_name("tensorflow.TensorProto")
            .is_some());
    }

    #[test]
    fn pointing_at_the_repository_root_works_too() {
        let dir = serving_like_tree();
        let source = load_from_directory(dir.path(), &[]).unwrap();
        assert_eq!(source.services().len(), 1);
    }

    #[test]
    fn import_roots_are_derived_from_the_import_statements() {
        let dir = serving_like_tree();
        let tree = discover_tree(&dir.path().join("protos/tensorflow_serving")).unwrap();

        assert!(tree.missing.is_empty(), "{:?}", tree.missing);
        // The root the imports are written against was inferred even though it
        // sits above the folder that was chosen.
        assert!(
            tree.roots.iter().any(|r| r.ends_with("protos")),
            "{:?}",
            tree.roots
        );
        // Files are named the way imports refer to them.
        let names: Vec<String> = tree.files.iter().map(|f| slashed(f)).collect();
        assert!(
            names.contains(&"tensorflow_serving/apis/predict.proto".to_string()),
            "{names:?}"
        );
    }

    /// The shape a real vendored TensorFlow Serving tree turns up in: the
    /// service you want is complete, but the tree also holds an orphaned file
    /// with a dangling import, and a second service whose own imports were
    /// never copied. Neither should stop the first service from working.
    fn partially_vendored_tree() -> tempfile::TempDir {
        let dir = serving_like_tree();
        let r = dir.path().join("protos");

        // Orphaned: imported by nothing that matters, and itself incomplete.
        write(
            &r.join("tensorflow/core/protobuf/queue_runner.proto"),
            r#"syntax = "proto3";
               package tensorflow;
               import "tensorflow/core/protobuf/error_codes.proto";
               message QueueRunnerDef { string queue_name = 1; }"#,
        );

        // A second service whose dependencies were not copied.
        write(
            &r.join("tensorflow_serving/apis/model_service.proto"),
            r#"syntax = "proto3";
               package tensorflow.serving;
               import "tensorflow_serving/apis/get_model_status.proto";
               service ModelService {
                 rpc GetModelStatus(GetModelStatusRequest) returns (GetModelStatusResponse);
               }"#,
        );

        dir
    }

    #[test]
    fn an_incomplete_corner_does_not_block_a_complete_service() {
        let dir = partially_vendored_tree();
        let source = load_from_directory(&dir.path().join("protos"), &[]).unwrap_or_else(|e| {
            panic!("PredictionService is complete and should have compiled: {e}")
        });

        assert_eq!(
            source.service_names(),
            vec!["tensorflow.serving.PredictionService".to_string()],
            "the complete service should be usable"
        );
        // And it is genuinely usable, not just listed.
        assert!(source
            .unary_method("tensorflow.serving.PredictionService", "Predict")
            .is_ok());
    }

    #[test]
    fn a_skipped_service_explains_itself_when_asked_for() {
        let dir = partially_vendored_tree();
        let source = load_from_directory(&dir.path().join("protos"), &[]).unwrap();

        // The dropped files are recorded rather than silently forgotten.
        let warnings = source.warnings().join("\n");
        assert!(warnings.contains("model_service.proto"), "{warnings}");
        assert!(warnings.contains("get_model_status.proto"), "{warnings}");

        // Asking for the missing service says why it is absent.
        let err = source
            .unary_method("tensorflow.serving.ModelService", "GetModelStatus")
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("PredictionService"), "{msg}");
        assert!(msg.contains("get_model_status.proto"), "{msg}");
    }

    #[test]
    fn an_orphaned_dangling_import_is_ignored_entirely() {
        let dir = serving_like_tree();
        write(
            &dir.path()
                .join("protos/tensorflow/core/protobuf/queue_runner.proto"),
            r#"syntax = "proto3";
               package tensorflow;
               import "tensorflow/core/protobuf/error_codes.proto";
               message QueueRunnerDef { string queue_name = 1; }"#,
        );

        let source = load_from_directory(&dir.path().join("protos"), &[]).unwrap();
        assert_eq!(source.services().len(), 1);
    }

    #[test]
    fn naming_an_incomplete_entry_file_is_an_error_not_a_silent_skip() {
        let dir = partially_vendored_tree();
        let err = load_from_directory(
            &dir.path().join("protos"),
            &["tensorflow_serving/apis/model_service.proto".to_string()],
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("get_model_status.proto"), "{msg}");
    }

    #[test]
    fn import_closure_reports_reach_and_gaps() {
        let dir = partially_vendored_tree();
        let tree = discover_tree(&dir.path().join("protos")).unwrap();

        let (reached, missing) =
            import_closure("tensorflow_serving/apis/prediction_service.proto", &tree);
        assert!(missing.is_empty(), "{missing:?}");
        assert!(reached.contains("tensorflow/core/framework/tensor.proto"));
        // The orphan is not on the path.
        assert!(!reached.contains("tensorflow/core/protobuf/queue_runner.proto"));

        let (_, missing) = import_closure("tensorflow_serving/apis/model_service.proto", &tree);
        assert_eq!(
            missing,
            vec!["tensorflow_serving/apis/get_model_status.proto".to_string()]
        );
    }

    #[test]
    fn a_genuinely_missing_import_says_what_to_do() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("api/service.proto"),
            r#"syntax = "proto3";
               package api;
               import "third_party/absent.proto";
               service S { rpc Go(M) returns (M); }
               message M { string a = 1; }"#,
        );

        // Here the incomplete file *is* the only service, so there is nothing
        // left to fall back to and this must fail rather than succeed emptily.
        let err = load_from_directory(&dir.path().join("api"), &[]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("third_party/absent.proto"), "{msg}");
        assert!(msg.contains("Copy them in"), "{msg}");
    }

    #[test]
    fn well_known_imports_never_count_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("api/service.proto"),
            r#"syntax = "proto3";
               package api;
               import "google/protobuf/timestamp.proto";
               message M { google.protobuf.Timestamp at = 1; }
               service S { rpc Go(M) returns (M); }"#,
        );

        let source = load_from_directory(&dir.path().join("api"), &[]).unwrap();
        assert_eq!(source.services().len(), 1);
    }

    #[test]
    fn a_stray_uncompilable_proto_does_not_sink_the_whole_folder() {
        let dir = serving_like_tree();
        // A file that cannot compile on its own, of the sort that turns up in
        // vendored trees.
        write(
            &dir.path().join("protos/scratch/broken.proto"),
            "syntax = \"proto3\"; message { oops",
        );

        let source = load_from_directory(&dir.path().join("protos"), &[])
            .unwrap_or_else(|e| panic!("service files should still compile: {e}"));
        assert_eq!(
            source.service_names(),
            vec!["tensorflow.serving.PredictionService".to_string()]
        );
    }

    #[test]
    fn entry_files_may_be_named_by_their_tail() {
        let dir = serving_like_tree();
        let source = load_from_directory(
            &dir.path().join("protos"),
            &["tensorflow_serving/apis/prediction_service.proto".to_string()],
        )
        .unwrap();
        assert_eq!(source.services().len(), 1);
    }

    #[test]
    fn import_statement_forms_are_all_recognised() {
        let text = r#"
            import "a/b.proto";
            import public "c/d.proto";
            import weak "e/f.proto";
            // import "commented/out.proto";
            message M {}
        "#;
        let found = parse_imports(text);
        assert!(found.contains(&"a/b.proto".to_string()));
        assert!(found.contains(&"c/d.proto".to_string()));
        assert!(found.contains(&"e/f.proto".to_string()));
        assert!(!found.iter().any(|i| i.contains("commented")));
    }

    #[test]
    fn a_directory_compiles_a_whole_tree_with_one_include_root() {
        let dir = nested_tree();
        let source = load_from_directory(dir.path(), &[]).expect("the tree should compile");

        assert_eq!(
            source.service_names(),
            vec!["shop.apis.OrderService".to_string()]
        );
        let m = source
            .unary_method("shop.apis.OrderService", "Get")
            .unwrap();
        assert_eq!(m.input().full_name(), "shop.apis.GetOrder");
        // The transitively imported type came along.
        assert!(source.pool().get_message_by_name("common.v1.Id").is_some());
    }

    #[test]
    fn entry_files_narrow_what_is_compiled_but_still_pull_imports() {
        let dir = nested_tree();
        let source =
            load_from_directory(dir.path(), &["shop/apis/service.proto".to_string()]).unwrap();
        assert_eq!(source.service_names().len(), 1);
        assert!(source.pool().get_message_by_name("common.v1.Id").is_some());
    }

    #[test]
    fn a_missing_entry_file_names_itself() {
        let dir = nested_tree();
        let err =
            load_from_directory(dir.path(), &["shop/apis/nope.proto".to_string()]).unwrap_err();
        assert!(err.to_string().contains("nope.proto"), "{err}");
    }

    #[test]
    fn scanning_finds_every_proto_relative_to_the_root() {
        let dir = nested_tree();
        let found = scan_protos(dir.path()).unwrap();
        let names: Vec<String> = found
            .iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(
            names,
            vec![
                "common/v1/types.proto".to_string(),
                "shop/apis/order.proto".to_string(),
                "shop/apis/service.proto".to_string(),
            ]
        );
    }

    #[test]
    fn scanning_skips_noise_directories() {
        let dir = nested_tree();
        std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        std::fs::write(
            dir.path().join("node_modules/pkg/junk.proto"),
            "syntax=\"proto3\";",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/hidden.proto"), "syntax=\"proto3\";").unwrap();

        let found = scan_protos(dir.path()).unwrap();
        assert_eq!(found.len(), 3, "{found:?}");
    }

    #[test]
    fn an_empty_folder_explains_which_folder_to_pick() {
        let dir = tempfile::tempdir().unwrap();
        let err = scan_protos(dir.path()).unwrap_err();
        assert!(err.to_string().contains("import paths"), "{err}");
    }

    #[test]
    fn a_missing_folder_names_the_path() {
        let err = scan_protos(Path::new("no/such/folder")).unwrap_err();
        assert!(err.to_string().contains("no"), "{err}");
    }

    #[test]
    fn broken_proto_reports_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.proto");
        std::fs::write(&p, "syntax = \"proto3\"; message { oops").unwrap();
        let err = load_from_files(&[p], &[dir.path().to_path_buf()]).unwrap_err();
        assert!(matches!(err, GrpcError::Descriptor(_)));
        assert!(err.to_string().contains("Could not compile"));
    }
}
