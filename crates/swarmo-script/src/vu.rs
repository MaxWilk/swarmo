//! Loading and driving `*.user.js` virtual-user scripts. See docs/scripting.md.
//!
//! ES module syntax (`export const options`, `export async function shopper`) is
//! accepted by stripping the `export` keywords and evaluating in global scope,
//! which avoids pulling in a module loader for what is always a single file.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::host::SharedHost;
use crate::runtime::{Engine, Limits};
use crate::types::{ConsoleLine, ScriptError};

// ---------------------------------------------------------------------------
// The `options` block
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMixEntry {
    pub exec: String,
    #[serde(default = "one")]
    pub weight: f64,
}

fn one() -> f64 {
    1.0
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VuOptions {
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub stages: Vec<StageOpt>,
    #[serde(default)]
    pub max_vus: Option<u32>,
    #[serde(default)]
    pub user_mix: Vec<UserMixEntry>,
    /// Free-form; validated by the load engine.
    #[serde(default)]
    pub thresholds: Vec<serde_json::Value>,
    #[serde(default)]
    pub verify_tls: Option<bool>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Where `ctx.grpc` calls get their schema. Absent means the script makes
    /// no gRPC calls.
    #[serde(default)]
    pub grpc: Option<GrpcOptions>,
}

/// `options.grpc` — either a list of `.proto` files or server reflection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrpcOptions {
    /// Compile every `.proto` under this folder, using it as the import root.
    /// The easy path for schemas that ship as a tree.
    #[serde(default)]
    pub proto_dir: Option<String>,
    #[serde(default)]
    pub proto_files: Vec<String>,
    #[serde(default)]
    pub include_paths: Vec<String>,
    /// Fetch the schema from the server instead of compiling `.proto` files.
    #[serde(default)]
    pub reflection: bool,
    /// Address to fetch the schema from when using reflection. Defaults to the
    /// address of the first call the script makes.
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub verify_tls: Option<bool>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Largest response message to accept. Defaults to 16 MB.
    #[serde(default)]
    pub max_response_bytes: Option<u64>,
    /// Metadata sent with the reflection request that fetches the schema.
    /// Needed when the server requires auth even for reflection.
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageOpt {
    pub duration_sec: u64,
    pub target: f64,
}

impl VuOptions {
    /// The exec functions to run, defaulting to a single `default` export.
    pub fn exec_names(&self) -> Vec<String> {
        if self.user_mix.is_empty() {
            vec!["default".to_string()]
        } else {
            self.user_mix.iter().map(|u| u.exec.clone()).collect()
        }
    }
}

// ---------------------------------------------------------------------------
// Source transformation
// ---------------------------------------------------------------------------

fn export_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^\s*export\s+(default\s+)?").unwrap())
}

fn import_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^\s*import\s+.*$").unwrap())
}

/// Strip module syntax so the script can be evaluated in global scope.
pub fn strip_module_syntax(source: &str) -> String {
    let no_imports = import_re().replace_all(source, |caps: &regex::Captures| {
        format!("// [swarmo] import is not supported: {}", caps[0].trim())
    });
    export_re()
        .replace_all(&no_imports, |caps: &regex::Captures| {
            if caps.get(1).is_some() {
                "var __swarmo_default = ".to_string()
            } else {
                String::new()
            }
        })
        .into_owned()
}

/// JS identifiers we are willing to call by name (guards against injection).
fn is_safe_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

// ---------------------------------------------------------------------------
// The VU engine
// ---------------------------------------------------------------------------

pub struct VuEngine {
    engine: Engine,
    vu_id: u64,
}

impl VuEngine {
    /// Load a user script into a fresh context and return the parsed options.
    pub fn load(
        host: SharedHost,
        source: &str,
        vu_id: u64,
        base_vars: &HashMap<String, String>,
    ) -> Result<(Self, VuOptions), ScriptError> {
        let engine = Engine::new(host, Limits::for_vu())?;

        let vars_json = serde_json::to_string(base_vars)
            .map_err(|e| ScriptError::Engine(format!("cannot serialize variables: {e}")))?;
        engine.eval_json(&format!("(__swarmo.setScope({vars_json}), null)"))?;

        let transformed = strip_module_syntax(source);
        // Evaluated directly (not inside an async IIFE) so its declarations
        // land in global scope, where later iterations can call them by name.
        engine.eval_json(&format!("{transformed}\nnull;"))?;

        let opts_json =
            engine.eval_json("(typeof options !== 'undefined' && options) ? options : {}")?;
        let options: VuOptions = serde_json::from_str(&opts_json)
            .map_err(|e| ScriptError::Js(format!("invalid `options` export: {e}")))?;

        let mut vu = VuEngine { engine, vu_id };
        vu.make_ctx()?;

        // Fail fast if an exec target is missing or unsafe.
        for name in options.exec_names() {
            if !is_safe_identifier(&name) {
                return Err(ScriptError::Js(format!(
                    "`{name}` is not a valid function name"
                )));
            }
            // `default` is a reserved word, so it can never be probed by name.
            let probe = if name == "default" {
                "typeof __swarmo_default === 'function' || typeof globalThis['default'] === 'function'".to_string()
            } else {
                format!("typeof {name} === 'function'")
            };
            let exists = vu.engine.eval_json(&probe)?;
            if exists.trim() != "true" {
                // `default` is the implicit fallback; give a clearer message.
                let hint = if name == "default" {
                    " (define `export default async function (ctx) {...}` or an `options.userMix`)"
                } else {
                    ""
                };
                return Err(ScriptError::Js(format!(
                    "user script does not export a function named `{name}`{hint}"
                )));
            }
        }

        Ok((vu, options))
    }

    fn make_ctx(&mut self) -> Result<(), ScriptError> {
        self.engine.eval_json(&format!(
            "(__swarmo.__ctx = __swarmo.makeCtx({}), null)",
            self.vu_id
        ))?;
        Ok(())
    }

    /// Run one iteration of `exec_name`. Returns the checks it recorded.
    pub fn run_iteration(
        &self,
        exec_name: &str,
        iteration: u64,
    ) -> Result<HashMap<String, CheckCount>, ScriptError> {
        if !is_safe_identifier(exec_name) {
            return Err(ScriptError::Js(format!(
                "`{exec_name}` is not a valid function name"
            )));
        }
        self.engine.eval_json(&format!(
            "(__swarmo.__ctx.vu.iteration = {iteration}, null)"
        ))?;

        let call = if exec_name == "default" {
            "await (typeof __swarmo_default === 'function' ? __swarmo_default : globalThis['default'])(__swarmo.__ctx);".to_string()
        } else {
            format!("await {exec_name}(__swarmo.__ctx);")
        };

        let run = self.engine.run_user_script(&call);
        // Read checks even when the iteration threw, so partial work counts.
        let checks_json = self.engine.eval_json("__swarmo.takeChecks()")?;
        let checks: HashMap<String, CheckCount> =
            serde_json::from_str(&checks_json).unwrap_or_default();
        run?;
        Ok(checks)
    }

    pub fn take_console(&self) -> Vec<ConsoleLine> {
        let lines = self.engine.console();
        self.engine.clear_console();
        lines
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct CheckCount {
    #[serde(default)]
    pub passes: u64,
    #[serde(default)]
    pub fails: u64,
}

/// The starter template offered when creating a new user script.
pub const USER_SCRIPT_TEMPLATE: &str = r#"// Swarmo virtual-user script.
//
// `options` configures the run. Each exported function is one kind of user;
// `userMix` weights how many virtual users run each one.

export const options = {
  mode: "closed",
  stages: [
    { durationSec: 15, target: 10 },
    { durationSec: 30, target: 10 },
    { durationSec: 5,  target: 0 }
  ],
  maxVus: 100,
  userMix: [
    { exec: "browse", weight: 3 },
    { exec: "checkout", weight: 1 }
  ],
  thresholds: [
    { metric: "http_req_duration", stat: "p95", op: "<", valueMs: 800 },
    { metric: "http_req_failed", stat: "rate", op: "<", value: 0.01 }
  ]
};

// ctx.vars persists across iterations for the same virtual user.
export async function browse(ctx) {
  const res = await ctx.http.get("{{baseUrl}}/json", { tag: "list items" });
  ctx.check(res, {
    "status is 200": r => r.status === 200,
    "has items": r => Array.isArray(r.json().items)
  });
  await ctx.sleep(1, 3);
}

export async function checkout(ctx) {
  if (!ctx.vars.token) {
    const login = await ctx.http.post("{{baseUrl}}/token", { json: { user: "demo" }, tag: "login" });
    ctx.vars.token = login.json().token;
  }
  const res = await ctx.http.post("{{baseUrl}}/echo", {
    headers: { Authorization: `Bearer ${ctx.vars.token}` },
    json: { sku: "A-1", qty: 1 },
    tag: "create order"
  });
  ctx.check(res, { "order accepted": r => r.status === 200 });
  await ctx.sleep(0.5, 1.5);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_exports() {
        let src = "export const options = {};\nexport async function a(ctx) {}\n";
        let out = strip_module_syntax(src);
        assert!(out.contains("const options"));
        assert!(!out.contains("export"));
    }

    #[test]
    fn rewrites_default_export() {
        let out = strip_module_syntax("export default async function (ctx) {}");
        assert!(out.contains("var __swarmo_default ="));
    }

    #[test]
    fn comments_out_imports() {
        let out = strip_module_syntax("import x from 'y';\nconst a = 1;");
        assert!(out.contains("// [swarmo] import is not supported"));
        assert!(out.contains("const a = 1"));
    }

    #[test]
    fn identifier_guard() {
        assert!(is_safe_identifier("shopper"));
        assert!(is_safe_identifier("_a$1"));
        assert!(!is_safe_identifier("a; evil()"));
        assert!(!is_safe_identifier(""));
        assert!(!is_safe_identifier("1abc"));
    }
}
