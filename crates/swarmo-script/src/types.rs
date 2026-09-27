use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LogLevel {
    Log,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleLine {
    pub level: LogLevel,
    pub text: String,
}

/// The mutable view of an outgoing request that pre-request scripts see.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptRequest {
    pub name: String,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub duration_ms: f64,
}

/// Everything a script run produced.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptOutcome {
    /// Variables the script set (applied over the caller's scope).
    pub vars: HashMap<String, String>,
    /// Present for pre-request runs: the (possibly mutated) request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<ScriptRequest>,
    pub tests: Vec<TestResult>,
    pub console: Vec<ConsoleLine>,
    /// A script-level failure (throw, syntax error, timeout, OOM).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ScriptOutcome {
    pub fn failed(&self) -> bool {
        self.error.is_some()
    }
    pub fn tests_passed(&self) -> usize {
        self.tests.iter().filter(|t| t.passed).count()
    }
    pub fn tests_failed(&self) -> usize {
        self.tests.iter().filter(|t| !t.passed).count()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error("script error: {0}")]
    Js(String),
    #[error("script timed out after {0}ms")]
    Timeout(u64),
    #[error("engine error: {0}")]
    Engine(String),
}
