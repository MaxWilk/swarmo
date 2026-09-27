//! Postman Collection Format v2.1 importer.
//!
//! Everything importable is imported; anything we cannot map faithfully is
//! imported anyway with a `// SWARMO-IMPORT-WARNING:` comment and listed in the
//! report, so nothing is silently dropped.

use std::path::Path;

use serde_json::Value;

use crate::curl_import::form_decode;
use crate::error::{io_err, CoreError, Result};
use crate::model::*;
use crate::store::{ImportReport, WorkspaceStore};

const WARN_PREFIX: &str = "// SWARMO-IMPORT-WARNING:";

/// Import a Postman v2.x collection file into the workspace.
pub fn import_collection(store: &WorkspaceStore, file: &Path) -> Result<ImportReport> {
    let text = std::fs::read_to_string(file).map_err(io_err(file.to_path_buf()))?;
    let root: Value = serde_json::from_str(&text).map_err(|source| CoreError::Parse {
        path: file.to_path_buf(),
        source,
    })?;

    let info = root
        .get("info")
        .ok_or_else(|| CoreError::invalid("not a Postman collection (missing \"info\" object)"))?;
    let name = info
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Imported Collection")
        .to_string();

    let mut report = ImportReport {
        collection_name: name.clone(),
        ..Default::default()
    };

    let schema = info.get("schema").and_then(Value::as_str).unwrap_or("");
    if !schema.contains("v2.1") && !schema.contains("v2.0") {
        report.warnings.push(format!(
            "Unrecognized collection schema \"{schema}\"; imported as v2.1 (best effort)."
        ));
    }

    let coll_ref = store.create_collection(&name)?;
    report.collection_ref = coll_ref.clone();

    // Collection-level auth / scripts.
    let mut container = store.get_container(&coll_ref)?;
    container.name = name.clone();
    if let Some(a) = root.get("auth") {
        container.auth = convert_auth(a, &mut report.warnings);
    }
    apply_events(
        root.get("event"),
        &mut container.scripts,
        &mut report.warnings,
    );
    store.save_container(&coll_ref, &container)?;

    // Items.
    if let Some(items) = root.get("item").and_then(Value::as_array) {
        import_items(store, &coll_ref, items, &mut report)?;
    }

    // Collection variables -> a new environment.
    if let Some(vars) = root.get("variable").and_then(Value::as_array) {
        if !vars.is_empty() {
            let env_name = format!("{name}-imported");
            let mut env = Environment::new(&env_name);
            for v in vars {
                let key = v.get("key").and_then(Value::as_str).unwrap_or_default();
                if key.is_empty() {
                    continue;
                }
                env.variables.push(EnvVariable {
                    key: key.to_string(),
                    value: value_to_string(v.get("value")),
                    secret: v.get("type").and_then(Value::as_str) == Some("secret"),
                    enabled: !v.get("disabled").and_then(Value::as_bool).unwrap_or(false),
                });
            }
            store.save_environment(&env)?;
            report.environment_created = Some(env_name);
        }
    }

    Ok(report)
}

/// Import a Postman environment export.
pub fn import_environment(store: &WorkspaceStore, file: &Path) -> Result<String> {
    let text = std::fs::read_to_string(file).map_err(io_err(file.to_path_buf()))?;
    let root: Value = serde_json::from_str(&text).map_err(|source| CoreError::Parse {
        path: file.to_path_buf(),
        source,
    })?;

    let name = root
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Imported Environment")
        .to_string();
    let mut env = Environment::new(&name);

    let values = root
        .get("values")
        .and_then(Value::as_array)
        .ok_or_else(|| CoreError::invalid("not a Postman environment (missing \"values\")"))?;

    for v in values {
        let key = v.get("key").and_then(Value::as_str).unwrap_or_default();
        if key.is_empty() {
            continue;
        }
        env.variables.push(EnvVariable {
            key: key.to_string(),
            value: value_to_string(v.get("value")),
            secret: v.get("type").and_then(Value::as_str) == Some("secret"),
            enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        });
    }

    store.save_environment(&env)?;
    Ok(name)
}

// ---------------------------------------------------------------------------

fn import_items(
    store: &WorkspaceStore,
    parent_ref: &str,
    items: &[Value],
    report: &mut ImportReport,
) -> Result<()> {
    for item in items {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Untitled")
            .to_string();

        if let Some(children) = item.get("item").and_then(Value::as_array) {
            // A folder.
            let folder_ref = store.create_folder(parent_ref, &name)?;
            report.folders_imported += 1;

            let mut container = store.get_container(&folder_ref)?;
            container.name = name.clone();
            if let Some(a) = item.get("auth") {
                container.auth = convert_auth(a, &mut report.warnings);
            }
            apply_events(
                item.get("event"),
                &mut container.scripts,
                &mut report.warnings,
            );
            store.save_container(&folder_ref, &container)?;

            import_items(store, &folder_ref, children, report)?;
        } else if item.get("request").is_some() {
            let req = convert_request(&name, item, report);
            let node_ref = store.create_request(parent_ref, &name)?;
            let mut def = req;
            // Keep the id the store generated for consistency.
            def.id = store.get_request(&node_ref)?.id;
            store.save_request(&node_ref, &def)?;
            report.requests_imported += 1;
        }
    }
    Ok(())
}

fn convert_request(name: &str, item: &Value, report: &mut ImportReport) -> RequestDef {
    let mut def = RequestDef::new(name);
    let req = match item.get("request") {
        Some(r) => r,
        None => return def,
    };

    // A request can be a bare URL string.
    if let Some(url_str) = req.as_str() {
        def.url = url_str.to_string();
        return def;
    }

    def.method = req
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_uppercase();

    // URL: either a string or an object with raw/query.
    match req.get("url") {
        Some(Value::String(s)) => def.url = s.clone(),
        Some(Value::Object(_)) => {
            let url = req.get("url").unwrap();
            let raw = url.get("raw").and_then(Value::as_str).unwrap_or_default();
            // The query moves into `params` only when Postman has itemised
            // it; a hand-written entry with just `raw` keeps its query,
            // rather than losing it with nothing to say so.
            let query = url.get("query").and_then(Value::as_array);
            def.url = if query.is_some() {
                raw.split('?').next().unwrap_or(raw).to_string()
            } else {
                raw.to_string()
            };
            if let Some(q) = query {
                for p in q {
                    let key = p.get("key").and_then(Value::as_str).unwrap_or_default();
                    if key.is_empty() {
                        continue;
                    }
                    // Postman keeps these as written in the URL, already
                    // encoded; params are encoded at send, so store them
                    // decoded or `%20` goes out as `%2520`.
                    def.params.push(KeyValue {
                        key: form_decode(key),
                        value: form_decode(&value_to_string(p.get("value"))),
                        enabled: !p.get("disabled").and_then(Value::as_bool).unwrap_or(false),
                        description: None,
                    });
                }
            }
            if let Some(vars) = url.get("variable").and_then(Value::as_array) {
                def.url = substitute_path_variables(name, &def.url, vars, &mut report.warnings);
            }
        }
        _ => {}
    }

    if let Some(headers) = req.get("header").and_then(Value::as_array) {
        for h in headers {
            let key = h.get("key").and_then(Value::as_str).unwrap_or_default();
            if key.is_empty() {
                continue;
            }
            def.headers.push(KeyValue {
                key: key.to_string(),
                value: value_to_string(h.get("value")),
                enabled: !h.get("disabled").and_then(Value::as_bool).unwrap_or(false),
                description: None,
            });
        }
    }

    def.auth = match req.get("auth") {
        Some(a) => convert_auth(a, &mut report.warnings),
        None => Auth::Inherit,
    };

    def.body = convert_body(req.get("body"), &def.headers, &mut report.warnings);

    apply_events(item.get("event"), &mut def.scripts, &mut report.warnings);

    def
}

/// Replace each `:key` path segment with its value from `url.variable`.
///
/// The values go in directly rather than as `{{key}}` plus a variable: they
/// belong to one request, and two requests each with an `:id` would fight
/// over a shared one. A variable with no value becomes `{{key}}`, so sending
/// it flags an unresolved variable rather than a literal `:key` going out.
fn substitute_path_variables(
    request: &str,
    url: &str,
    vars: &[Value],
    warnings: &mut Vec<String>,
) -> String {
    let split = url.find(['?', '#']).unwrap_or(url.len());
    let (path, rest) = url.split_at(split);
    let mut segments: Vec<String> = path.split('/').map(str::to_string).collect();
    for v in vars {
        let key = v.get("key").and_then(Value::as_str).unwrap_or_default();
        if key.is_empty() {
            continue;
        }
        let placeholder = format!(":{key}");
        if !segments.contains(&placeholder) {
            continue;
        }
        let value = value_to_string(v.get("value"));
        let replacement = if value.is_empty() {
            warnings.push(format!(
                "Path variable \":{key}\" in \"{request}\" has no value; it was imported as {{{{{key}}}}}."
            ));
            format!("{{{{{key}}}}}")
        } else {
            value
        };
        for s in segments.iter_mut().filter(|s| **s == placeholder) {
            *s = replacement.clone();
        }
    }
    segments.join("/") + rest
}

fn convert_auth(a: &Value, warnings: &mut Vec<String>) -> Auth {
    // No type — `"auth": null` included, which the schema allows — means
    // inherit, not "no auth".
    let Some(kind) = a.get("type").and_then(Value::as_str) else {
        return Auth::Inherit;
    };

    // Postman stores params as either an array of {key,value} or an object.
    let get = |field: &str| -> String {
        if let Some(arr) = a.get(kind).and_then(Value::as_array) {
            for e in arr {
                if e.get("key").and_then(Value::as_str) == Some(field) {
                    return value_to_string(e.get("value"));
                }
            }
            String::new()
        } else if let Some(obj) = a.get(kind) {
            value_to_string(obj.get(field))
        } else {
            String::new()
        }
    };

    match kind {
        "inherit" => Auth::Inherit,
        "noauth" => Auth::None,
        "basic" => Auth::Basic {
            username: get("username"),
            password: get("password"),
        },
        "bearer" => Auth::Bearer {
            token: get("token"),
        },
        "apikey" => {
            let in_ = get("in");
            let key = get("key");
            let value = get("value");
            if in_ == "query" {
                warnings.push(format!(
                    "API-key auth in the query string is not supported; add \"{key}\" as a query param manually."
                ));
                Auth::None
            } else {
                Auth::ApiKeyHeader {
                    header_name: if key.is_empty() {
                        "X-API-Key".to_string()
                    } else {
                        key
                    },
                    value,
                }
            }
        }
        other => {
            warnings.push(format!(
                "Auth type \"{other}\" is not supported; that request was imported with no auth."
            ));
            Auth::None
        }
    }
}

fn convert_body(body: Option<&Value>, headers: &[KeyValue], warnings: &mut Vec<String>) -> Body {
    let body = match body {
        Some(b) if !b.is_null() => b,
        _ => return Body::None,
    };
    let mode = body.get("mode").and_then(Value::as_str).unwrap_or("");

    match mode {
        "raw" => {
            let text = body.get("raw").and_then(Value::as_str).unwrap_or_default();
            let lang = body
                .get("options")
                .and_then(|o| o.get("raw"))
                .and_then(|r| r.get("language"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let header_ct = headers
                .iter()
                .find(|h| h.key.eq_ignore_ascii_case("content-type"))
                .map(|h| h.value.clone());
            if lang == "json" || header_ct.as_deref().is_some_and(|c| c.contains("json")) {
                Body::Json {
                    text: text.to_string(),
                }
            } else {
                // An explicit header wins; otherwise the editor's language
                // picker is the only record of what the text is.
                let lang_ct = match lang {
                    "xml" => Some("application/xml"),
                    "html" => Some("text/html"),
                    "javascript" => Some("application/javascript"),
                    "text" => Some("text/plain"),
                    _ => None,
                };
                Body::Text {
                    text: text.to_string(),
                    content_type: header_ct.or(lang_ct.map(str::to_string)),
                }
            }
        }
        "urlencoded" => Body::Form {
            fields: body
                .get("urlencoded")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|f| KeyValue {
                            key: f
                                .get("key")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            value: value_to_string(f.get("value")),
                            enabled: !f.get("disabled").and_then(Value::as_bool).unwrap_or(false),
                            description: None,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        "formdata" => Body::Multipart {
            parts: body
                .get("formdata")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|f| {
                            let is_file = f.get("type").and_then(Value::as_str) == Some("file");
                            let value = if is_file {
                                match f.get("src") {
                                    Some(Value::String(s)) => s.clone(),
                                    Some(Value::Array(a)) => a
                                        .first()
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string(),
                                    _ => String::new(),
                                }
                            } else {
                                value_to_string(f.get("value"))
                            };
                            MultipartPart {
                                key: f
                                    .get("key")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                                kind: if is_file {
                                    MultipartKind::File
                                } else {
                                    MultipartKind::Text
                                },
                                value,
                                enabled: !f
                                    .get("disabled")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                                content_type: f
                                    .get("contentType")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        "graphql" => {
            let g = body.get("graphql");
            Body::Graphql {
                query: g
                    .and_then(|g| g.get("query"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                variables: g
                    .and_then(|g| g.get("variables"))
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default(),
            }
        }
        "file" => Body::Binary {
            path: body
                .get("file")
                .and_then(|f| f.get("src"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
        "" => Body::None,
        other => {
            warnings.push(format!(
                "Body mode \"{other}\" is not supported; that request was imported with no body."
            ));
            Body::None
        }
    }
}

/// Copy Postman `event` scripts into our script slots, flagging risky APIs.
fn apply_events(events: Option<&Value>, scripts: &mut Scripts, warnings: &mut Vec<String>) {
    let events = match events.and_then(Value::as_array) {
        Some(e) => e,
        None => return,
    };

    for ev in events {
        let listen = ev.get("listen").and_then(Value::as_str).unwrap_or("");
        let exec = ev
            .get("script")
            .and_then(|s| s.get("exec"))
            .map(join_exec)
            .unwrap_or_default();
        if exec.trim().is_empty() {
            continue;
        }

        let unsupported = detect_unsupported(&exec);
        let text = if unsupported.is_empty() {
            exec
        } else {
            warnings.push(format!(
                "Script uses unsupported API(s): {}. It was imported unchanged and will throw at runtime.",
                unsupported.join(", ")
            ));
            format!(
                "{WARN_PREFIX} uses unsupported API(s): {}\n{exec}",
                unsupported.join(", ")
            )
        };

        match listen {
            "prerequest" => scripts.pre_request = text,
            "test" => scripts.post_response = text,
            _ => {}
        }
    }
}

fn join_exec(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(lines) => lines
            .iter()
            .map(|l| l.as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// `pm.*` members our shim does not implement (see docs/scripting.md).
const UNSUPPORTED_APIS: &[&str] = &[
    "pm.cookies",
    "pm.iterationData",
    "pm.execution",
    "pm.visualizer",
    "pm.vault",
    "postman.setNextRequest",
    "require(",
];

fn detect_unsupported(script: &str) -> Vec<String> {
    UNSUPPORTED_APIS
        .iter()
        .filter(|api| script.contains(**api))
        .map(|api| api.trim_end_matches('(').to_string())
        .collect()
}

fn value_to_string(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}
