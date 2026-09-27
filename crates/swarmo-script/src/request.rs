//! Running pre-request and post-response scripts. See docs/scripting.md.

use std::collections::HashMap;

use serde::Deserialize;

use crate::host::SharedHost;
use crate::runtime::{Engine, Limits};
use crate::types::*;

#[derive(Debug, Deserialize)]
struct RawResult {
    #[serde(default)]
    vars: HashMap<String, String>,
    #[serde(default)]
    request: Option<ScriptRequest>,
    #[serde(default)]
    tests: Vec<TestResult>,
}

/// Run every pre-request script in order against a mutable request.
pub fn run_pre_request(
    host: SharedHost,
    scripts: &[String],
    request: &ScriptRequest,
    base_vars: &HashMap<String, String>,
    limits: Limits,
) -> ScriptOutcome {
    run(host, scripts, Some(request), None, base_vars, limits)
}

/// Run every post-response script in order against a frozen response.
pub fn run_post_response(
    host: SharedHost,
    scripts: &[String],
    request: &ScriptRequest,
    response: &ScriptResponse,
    base_vars: &HashMap<String, String>,
    limits: Limits,
) -> ScriptOutcome {
    run(
        host,
        scripts,
        Some(request),
        Some(response),
        base_vars,
        limits,
    )
}

fn run(
    host: SharedHost,
    scripts: &[String],
    request: Option<&ScriptRequest>,
    response: Option<&ScriptResponse>,
    base_vars: &HashMap<String, String>,
    limits: Limits,
) -> ScriptOutcome {
    let mut outcome = ScriptOutcome::default();
    if scripts.iter().all(|s| s.trim().is_empty()) {
        outcome.request = request.cloned();
        return outcome;
    }

    let engine = match Engine::new(host, limits) {
        Ok(e) => e,
        Err(e) => {
            outcome.error = Some(e.to_string());
            return outcome;
        }
    };

    if let Err(e) = seed(&engine, request, response, base_vars) {
        outcome.error = Some(e.to_string());
        return outcome;
    }

    for src in scripts {
        if src.trim().is_empty() {
            continue;
        }
        if let Err(e) = engine.run_user_script(src) {
            outcome.error = Some(e.to_string());
            break;
        }
    }

    outcome.console = engine.console();

    match engine.eval_json("__swarmo.result()") {
        Ok(json) => match serde_json::from_str::<RawResult>(&json) {
            Ok(raw) => {
                outcome.vars = raw.vars;
                outcome.request = raw.request.or_else(|| request.cloned());
                outcome.tests = raw.tests;
            }
            Err(e) => {
                if outcome.error.is_none() {
                    outcome.error = Some(format!("could not read script result: {e}"));
                }
                outcome.request = request.cloned();
            }
        },
        Err(e) => {
            if outcome.error.is_none() {
                outcome.error = Some(e.to_string());
            }
            outcome.request = request.cloned();
        }
    }

    outcome
}

fn seed(
    engine: &Engine,
    request: Option<&ScriptRequest>,
    response: Option<&ScriptResponse>,
    base_vars: &HashMap<String, String>,
) -> Result<(), ScriptError> {
    let vars_json = serde_json::to_string(base_vars)
        .map_err(|e| ScriptError::Engine(format!("cannot serialize variables: {e}")))?;
    engine.eval_json(&format!("(__swarmo.setScope({vars_json}), null)"))?;

    let req_json = match request {
        Some(r) => serde_json::to_string(r)
            .map_err(|e| ScriptError::Engine(format!("cannot serialize request: {e}")))?,
        None => "null".to_string(),
    };
    engine.eval_json(&format!("(__swarmo.setRequest({req_json}), null)"))?;

    let res_json = match response {
        Some(r) => serde_json::to_string(r)
            .map_err(|e| ScriptError::Engine(format!("cannot serialize response: {e}")))?,
        None => "null".to_string(),
    };
    engine.eval_json(&format!("(__swarmo.setResponse({res_json}), null)"))?;

    Ok(())
}
