//! Serde types for every on-disk format. Documented in docs/formats.md.
//!
//! Reading is lenient (unknown fields ignored, most fields default); writing
//! always emits the full current schema.

use serde::{Deserialize, Serialize};

use crate::store::Protocol;

pub const FORMAT_VERSION: u32 = 1;

pub(crate) fn default_version() -> u32 {
    FORMAT_VERSION
}
pub(crate) fn default_true() -> bool {
    true
}
fn default_timeout_ms() -> u64 {
    30_000
}

// ---------------------------------------------------------------------------
// Workspace manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceManifest {
    #[serde(default = "default_version")]
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub active_environment: Option<String>,
    // Approvals (load hosts, auth commands) deliberately do not live here.
    // This file is committed and shared, so a workspace could arrive
    // approving its own commands; the app keeps them per machine instead.
    // Older manifests that still carry `approvedLoadHosts` or
    // `approvedAuthCommands` load fine, the keys are ignored, and the next
    // save drops them.
}

impl WorkspaceManifest {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            version: FORMAT_VERSION,
            name: name.into(),
            active_environment: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Environments & variables
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvVariable {
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub secret: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    #[serde(default = "default_version")]
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub variables: Vec<EnvVariable>,
}

impl Environment {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            version: FORMAT_VERSION,
            name: name.into(),
            variables: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Key/value pairs used by params, headers, form fields
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyValue {
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl KeyValue {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            enabled: true,
            description: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Auth {
    /// Inherit from the parent folder / collection. Default for requests.
    #[default]
    Inherit,
    /// Explicitly send no auth, stopping inheritance.
    None,
    #[serde(rename_all = "camelCase")]
    Basic { username: String, password: String },
    #[serde(rename_all = "camelCase")]
    Bearer { token: String },
    #[serde(rename_all = "camelCase")]
    ApiKeyHeader { header_name: String, value: String },
    /// A token produced by running a local command, e.g.
    /// `gcloud auth print-identity-token`.
    ///
    /// Exists because the credentials people test against expire on the hour,
    /// and the script sandbox has no way to run a process. The token is
    /// fetched lazily, cached in memory only, and refreshed once when the
    /// server rejects it.
    #[serde(rename_all = "camelCase")]
    CommandToken {
        command: String,
        /// Header (HTTP) or metadata key (gRPC) to set.
        #[serde(default = "default_auth_header")]
        header_name: String,
        /// Text before the token. Empty means send the token bare.
        #[serde(default = "default_bearer_prefix")]
        prefix: String,
    },
}

fn default_auth_header() -> String {
    "Authorization".to_string()
}

fn default_bearer_prefix() -> String {
    "Bearer ".to_string()
}

// ---------------------------------------------------------------------------
// Body
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultipartPart {
    pub key: String,
    /// "text" | "file"
    pub kind: MultipartKind,
    #[serde(default)]
    pub value: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MultipartKind {
    Text,
    File,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Body {
    #[default]
    None,
    #[serde(rename_all = "camelCase")]
    Json { text: String },
    #[serde(rename_all = "camelCase")]
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Form { fields: Vec<KeyValue> },
    #[serde(rename_all = "camelCase")]
    Multipart { parts: Vec<MultipartPart> },
    #[serde(rename_all = "camelCase")]
    Graphql { query: String, variables: String },
    #[serde(rename_all = "camelCase")]
    Binary { path: String },
}

impl Body {
    pub fn kind_str(&self) -> &'static str {
        match self {
            Body::None => "none",
            Body::Json { .. } => "json",
            Body::Text { .. } => "text",
            Body::Form { .. } => "form",
            Body::Multipart { .. } => "multipart",
            Body::Graphql { .. } => "graphql",
            Body::Binary { .. } => "binary",
        }
    }
}

// ---------------------------------------------------------------------------
// Scripts & per-request settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scripts {
    #[serde(default)]
    pub pre_request: String,
    #[serde(default)]
    pub post_response: String,
}

impl Scripts {
    pub fn is_empty(&self) -> bool {
        self.pre_request.trim().is_empty() && self.post_response.trim().is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestSettings {
    #[serde(default = "default_true")]
    pub follow_redirects: bool,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_true")]
    pub verify_tls: bool,
}

impl Default for RequestSettings {
    fn default() -> Self {
        Self {
            follow_redirects: true,
            timeout_ms: default_timeout_ms(),
            verify_tls: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Request definition (*.req.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestDef {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "new_uuid")]
    pub id: String,
    pub name: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub params: Vec<KeyValue>,
    #[serde(default)]
    pub headers: Vec<KeyValue>,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub body: Body,
    #[serde(default)]
    pub scripts: Scripts,
    #[serde(default)]
    pub settings: RequestSettings,
}

pub(crate) fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn default_method() -> String {
    "GET".to_string()
}

impl RequestDef {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            version: FORMAT_VERSION,
            id: new_uuid(),
            name: name.into(),
            method: "GET".into(),
            url: String::new(),
            params: Vec::new(),
            headers: Vec::new(),
            auth: Auth::Inherit,
            body: Body::None,
            scripts: Scripts::default(),
            settings: RequestSettings::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Collection / folder settings (collection.json, folder.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerDef {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "new_uuid")]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub headers: Vec<KeyValue>,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub scripts: Scripts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl ContainerDef {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            version: FORMAT_VERSION,
            id: new_uuid(),
            name: name.into(),
            headers: Vec::new(),
            auth: Auth::Inherit,
            scripts: Scripts::default(),
            description: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Sidebar tree
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeKind {
    Collection,
    Folder,
    Request,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    /// Workspace-relative path with forward slashes. Stable identity for a node.
    pub node_ref: String,
    pub name: String,
    pub kind: NodeKind,
    /// A request's stable id, so callers can follow it across renames without
    /// re-reading every file. `None` for folders and collections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default)]
    pub children: Vec<TreeNode>,
}

// ---------------------------------------------------------------------------
// Load scenarios (*.load.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LoadMode {
    /// Stages ramp the number of concurrent virtual users.
    #[default]
    Closed,
    /// Stages ramp the arrival rate (iterations started per second).
    Open,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stage {
    pub duration_sec: u64,
    /// VUs in closed mode; arrivals/sec in open mode.
    pub target: f64,
    /// Add `target` to where the previous stage left off, instead of setting
    /// it outright.
    ///
    /// This is what makes a repeated block useful: "another hundred a second"
    /// means something different each time round, where a fixed number does
    /// not. A relative target of zero holds the current rate.
    #[serde(default, skip_serializing_if = "is_false")]
    pub relative: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One entry in a scenario's ramp: either a stage, or a repeated block.
///
/// Untagged so that a scenario written before repeats existed still reads: an
/// object with `durationSec` is a stage, one with `stages` is a block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StageItem {
    Repeat(RepeatBlock),
    Stage(Stage),
}

/// A group of stages run several times over.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepeatBlock {
    /// How many times to run the block. One is the same as not repeating.
    pub times: u32,
    pub stages: Vec<Stage>,
    /// Multiply every duration in the block by this, once per repetition.
    ///
    /// `1.5` makes each pass half again as long as the one before — 30s, 45s,
    /// 68s — which is how you hold each step of a climb for longer as the
    /// system gets closer to its limit. `None` and `1.0` both mean no change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_scale: Option<f64>,
}

/// One round of a fixed-count run: send this many, then pause.
///
/// The counterpart of [`Stage`] for runs measured in requests rather than in
/// time. A stage says "hold this rate for this long"; a round says "send
/// exactly this many, this many at a time, then wait" — which is what a batch
/// benchmark does, and what repeating one lets you see: whether the second
/// blast is faster than the first because a cache is warm, or slower because
/// something is leaking.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Blast {
    /// Iterations to run in this round.
    pub iterations: u64,
    /// How many run at once.
    pub concurrency: u32,
    /// Seconds to wait after the round, before the next one starts.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub gap_sec: u64,
    /// Add `iterations` to the previous round instead of setting it outright.
    ///
    /// Same reason as [`Stage::relative`]: it is what makes a repeated block
    /// worth having, since "another thousand" means something different each
    /// time round and a fixed number does not.
    #[serde(default, skip_serializing_if = "is_false")]
    pub relative: bool,
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

/// One entry in a fixed-count run: either a round, or a repeated block.
///
/// Untagged, on the same reasoning as [`StageItem`]: an object with
/// `iterations` is a round, one with `blasts` is a block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BlastItem {
    Repeat(BlastRepeat),
    Blast(Blast),
}

/// A group of rounds run several times over.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlastRepeat {
    /// How many times to run the block. One is the same as not repeating.
    pub times: u32,
    pub blasts: Vec<Blast>,
    /// Multiply every count in the block by this, once per repetition.
    ///
    /// `2.0` doubles the batch each pass — 1k, 2k, 4k — which is how you find
    /// where throughput stops scaling. `None` and `1.0` both mean no change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_scale: Option<f64>,
}

impl From<Blast> for BlastItem {
    fn from(b: Blast) -> Self {
        BlastItem::Blast(b)
    }
}

impl BlastItem {
    /// The rounds this item contributes, in order.
    fn blasts(&self) -> Vec<Blast> {
        match self {
            BlastItem::Blast(b) => vec![b.clone()],
            BlastItem::Repeat(r) => {
                // Capped *before* allocating: a mistyped count of four
                // billion must be refused, not turned into a with_capacity
                // that aborts the process.
                let times = (r.times.max(1) as usize).min(bounded_passes(r.blasts.len()));
                let scale = r.iteration_scale.filter(|s| *s > 0.0).unwrap_or(1.0);
                let mut out = Vec::with_capacity(times * r.blasts.len());
                for pass in 0..times {
                    // From the original count each pass rather than the
                    // previous one, so rounding cannot compound over a climb.
                    let factor = scale.powi(pass as i32);
                    for blast in &r.blasts {
                        out.push(Blast {
                            iterations: (blast.iterations as f64 * factor).round() as u64,
                            ..blast.clone()
                        });
                    }
                }
                out
            }
        }
    }
}

/// Flatten rounds into the plain list a run executes.
///
/// Repeats are unrolled and relative counts resolved here, once, so the engine
/// only ever sees a list of concrete rounds.
pub fn flatten_blasts(items: &[BlastItem]) -> Vec<Blast> {
    let mut out: Vec<Blast> = Vec::new();
    let mut current = 0.0_f64;

    for item in items {
        for blast in item.blasts() {
            if out.len() >= MAX_EXPANDED_STAGES {
                return out;
            }
            let iterations = if blast.relative {
                (current + blast.iterations as f64).max(0.0)
            } else {
                blast.iterations as f64
            };
            current = iterations;
            out.push(Blast {
                iterations: iterations.round() as u64,
                concurrency: blast.concurrency.max(1),
                gap_sec: blast.gap_sec,
                relative: false,
            });
        }
    }
    out
}

/// End a run early once it has shown what it was asked to find.
///
/// This is not a threshold. A threshold judges a run — breaching one means the
/// run failed. A stop condition is the *goal*: when you ramp up to find where a
/// service breaks, reaching the break is a success, and carrying on past it
/// only wastes time and hammers something already struggling.
///
/// Conditions are measured over the most recent interval, not the whole run,
/// so a service that fails after ten clean minutes stops promptly instead of
/// waiting for a cumulative average to catch up.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopCondition {
    /// `errorRate`, `p95`, `p99` or `rps`.
    pub metric: String,
    /// Stop once the metric goes above this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub above: Option<f64>,
    /// Stop once the metric falls below this — for throughput collapsing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below: Option<f64>,
    /// Consecutive intervals it must hold for, so one blip does not end a run.
    #[serde(default = "default_for_intervals")]
    pub for_intervals: u32,
}

fn default_for_intervals() -> u32 {
    2
}

impl StopCondition {
    /// The value this condition watches, for one interval.
    pub fn value_in(&self, snapshot: &Snapshot) -> Option<f64> {
        match self.metric.as_str() {
            "errorRate" | "error_rate" => Some(snapshot.interval_error_rate),
            "p95" => Some(snapshot.interval_p95),
            "p99" => Some(snapshot.interval_p99),
            "rps" => Some(snapshot.rps),
            _ => None,
        }
    }

    /// Whether this interval breaches the condition.
    pub fn breached_by(&self, snapshot: &Snapshot) -> bool {
        let Some(v) = self.value_in(snapshot) else {
            return false;
        };
        // A condition with neither bound set can never fire, rather than
        // firing immediately.
        self.above.is_some_and(|a| v > a) || self.below.is_some_and(|b| v < b)
    }

    /// How to describe the stop afterwards.
    pub fn describe(&self, value: f64) -> String {
        let unit = if self.metric.starts_with('p') {
            "ms"
        } else {
            ""
        };
        match (self.above, self.below) {
            (Some(a), _) => format!(
                "stopped early: {} reached {value:.3}{unit}, above the limit of {a}{unit}",
                self.metric
            ),
            (_, Some(b)) => format!(
                "stopped early: {} fell to {value:.3}{unit}, below the floor of {b}{unit}",
                self.metric
            ),
            _ => "stopped early".to_string(),
        }
    }
}

/// Most stages a scenario may expand to.
///
/// A ceiling rather than a target: a mistyped repeat count should be refused,
/// not turned into a plan with a million stages in it.
pub const MAX_EXPANDED_STAGES: usize = 10_000;

/// The most passes of a block worth unrolling, given how many entries each
/// pass adds: enough to reach the expansion cap and no more.
fn bounded_passes(per_pass: usize) -> usize {
    MAX_EXPANDED_STAGES / per_pass.max(1) + 1
}

impl From<Stage> for StageItem {
    fn from(s: Stage) -> Self {
        StageItem::Stage(s)
    }
}

impl StageItem {
    /// The stages this item contributes, in order.
    fn stages(&self) -> Vec<Stage> {
        match self {
            StageItem::Stage(s) => vec![s.clone()],
            StageItem::Repeat(r) => {
                // Capped *before* allocating; see `BlastItem::blasts`.
                let times = (r.times.max(1) as usize).min(bounded_passes(r.stages.len()));
                let scale = r.duration_scale.filter(|s| *s > 0.0).unwrap_or(1.0);
                let mut out = Vec::with_capacity(times * r.stages.len());

                for pass in 0..times {
                    // Computed from the original duration each time rather
                    // than from the previous pass, so rounding to whole
                    // seconds does not compound over a long climb.
                    let factor = scale.powi(pass as i32);
                    for stage in &r.stages {
                        out.push(Stage {
                            duration_sec: (stage.duration_sec as f64 * factor).round() as u64,
                            ..stage.clone()
                        });
                    }
                }
                out
            }
        }
    }
}

/// Flatten a ramp into the plain list of stages a run executes.
///
/// Repeats are unrolled and relative targets resolved here, once, so nothing
/// downstream — the scheduler, the preview chart, the duration estimate — has
/// to know that either exists.
impl LoadScenario {
    /// The rounds a fixed-count run will execute, or empty if it is not one.
    ///
    /// One reading for both shapes: an explicit `blasts` list wins, a plain
    /// `iterations` becomes the single round it has always meant, and a
    /// duration-based scenario yields nothing. Callers never have to know
    /// which of the two fields a scenario happens to use.
    pub fn rounds(&self) -> Vec<Blast> {
        if !self.blasts.is_empty() {
            return flatten_blasts(&self.blasts);
        }
        match self.iterations {
            Some(n) => vec![Blast {
                iterations: n,
                concurrency: self.concurrency.unwrap_or(16).max(1),
                gap_sec: 0,
                relative: false,
            }],
            None => Vec::new(),
        }
    }

    /// Whether this scenario is measured in requests rather than in time.
    pub fn is_fixed_count(&self) -> bool {
        self.iterations.is_some() || !self.blasts.is_empty()
    }
}

pub fn flatten_stages(items: &[StageItem]) -> Vec<Stage> {
    let mut out: Vec<Stage> = Vec::new();
    let mut current = 0.0_f64;

    for item in items {
        for stage in item.stages() {
            if out.len() >= MAX_EXPANDED_STAGES {
                return out;
            }
            // A relative target is measured from where the ramp has reached,
            // not from the value written in the previous stage, so a block can
            // be repeated without rewriting its numbers.
            let target = if stage.relative {
                (current + stage.target).max(0.0)
            } else {
                stage.target
            };
            current = target;
            out.push(Stage {
                duration_sec: stage.duration_sec,
                target,
                relative: false,
            });
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capture {
    /// "body" | "header"
    pub from: CaptureSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "as")]
    pub as_var: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CaptureSource {
    Body,
    Header,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadStep {
    /// Where the request was last seen. Readable, and what a human edits.
    pub request_ref: String,
    /// Which request this is, independent of where it lives.
    ///
    /// The ref is a path, so renaming or moving a request would otherwise
    /// break every scenario that used it. The id is what the step is really
    /// about; the ref is a hint that keeps the file readable and is repaired
    /// whenever the scenario is saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// [minMs, maxMs] uniform think time after this step.
    #[serde(default)]
    pub think_time_ms: Option<[u64; 2]>,
    #[serde(default)]
    pub capture: Vec<Capture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Run concurrently with the step above, instead of after it.
    ///
    /// Consecutive parallel steps form one group that is sent together and
    /// joined before the next group starts — the shape of a client fetching a
    /// page and its assets, or fanning a batch out to several endpoints.
    /// Captures from the whole group land before anything after it runs.
    #[serde(default, skip_serializing_if = "is_false")]
    pub parallel: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThresholdOp {
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "<=")]
    Lte,
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = ">=")]
    Gte,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Threshold {
    /// http_req_duration | http_req_failed | http_reqs | checks
    pub metric: String,
    /// p50 | p90 | p95 | p99 | avg | max | rate | count
    pub stat: String,
    pub op: ThresholdOp,
    /// Used for duration metrics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_ms: Option<f64>,
    /// Used for rate/count metrics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(default)]
    pub abort_on_fail: bool,
}

impl Threshold {
    pub fn target(&self) -> f64 {
        self.value_ms.or(self.value).unwrap_or(0.0)
    }
    pub fn describe(&self) -> String {
        let op = match self.op {
            ThresholdOp::Lt => "<",
            ThresholdOp::Lte => "<=",
            ThresholdOp::Gt => ">",
            ThresholdOp::Gte => ">=",
        };
        let unit = if self.value_ms.is_some() { "ms" } else { "" };
        format!(
            "{}.{} {} {}{}",
            self.metric,
            self.stat,
            op,
            self.target(),
            unit
        )
    }
}

fn default_max_vus() -> u32 {
    200
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadScenario {
    #[serde(default = "default_version")]
    pub version: u32,
    /// Stable across renames, so a run recorded months ago can still find the
    /// scenario it came from. Generated for scenarios written before ids
    /// existed the first time they are read, and kept from then on.
    #[serde(default = "new_uuid")]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub mode: LoadMode,
    /// Ramps. Each stage moves linearly from the previous stage's target, so
    /// the first one starts from zero. In open mode a target is a rate
    /// (arrivals per second); in closed mode it is a number of virtual users.
    #[serde(default)]
    pub stages: Vec<StageItem>,
    /// End the run early once this holds — for finding a breaking point,
    /// where reaching it is the point rather than a failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_when: Option<StopCondition>,
    /// Run exactly this many iterations and stop, instead of running for a
    /// duration.
    ///
    /// The batch-benchmark shape: "send 5,000 requests as fast as N workers
    /// can and tell me how long it took". Stages and rates are ignored when
    /// this is set; `concurrency` says how many run at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations: Option<u64>,
    /// Workers running those iterations concurrently. Capped by `maxVus`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<u32>,
    /// Several rounds of a fixed-count run, with pauses between them.
    ///
    /// Supersedes `iterations`/`concurrency` when non-empty; those stay for
    /// the single-round case and for every scenario written before rounds
    /// existed. Use [`Self::rounds`] rather than reading either directly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blasts: Vec<BlastItem>,
    /// Hold one rate for the whole run instead of ramping.
    ///
    /// When set, stage targets are ignored and this rate applies throughout —
    /// the simplest way to ask for "N per second". Pair it with `durationSec`,
    /// or let the stages supply the duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrival_rate_per_sec: Option<f64>,
    /// How long to run when there are no stages. Ignored when stages are given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_sec: Option<u64>,
    #[serde(default = "default_max_vus")]
    pub max_vus: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(default)]
    pub steps: Vec<LoadStep>,
    #[serde(default)]
    pub thresholds: Vec<Threshold>,
    #[serde(default)]
    pub run_scripts: bool,
    #[serde(default)]
    pub new_connection_per_iteration: bool,
    #[serde(default = "default_true")]
    pub verify_tls: bool,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// How many gRPC channels to open per address. One HTTP/2 connection can
    /// become the bottleneck under load, so this is the gRPC analogue of HTTP
    /// connection reuse. `None` picks a sensible default from the CPU count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grpc_channels: Option<u32>,
}

impl LoadScenario {
    /// The ramp as the run will actually execute it.
    pub fn flat_stages(&self) -> Vec<Stage> {
        flatten_stages(&self.stages)
    }

    pub fn new(name: impl Into<String>) -> Self {
        Self {
            version: FORMAT_VERSION,
            id: new_uuid(),
            name: name.into(),
            mode: LoadMode::Closed,
            stop_when: None,
            iterations: None,
            concurrency: None,
            blasts: Vec::new(),
            stages: vec![
                Stage {
                    duration_sec: 15,
                    target: 10.0,
                    relative: false,
                }
                .into(),
                Stage {
                    duration_sec: 30,
                    target: 10.0,
                    relative: false,
                }
                .into(),
                Stage {
                    duration_sec: 5,
                    target: 0.0,
                    relative: false,
                }
                .into(),
            ],
            arrival_rate_per_sec: None,
            duration_sec: None,
            max_vus: default_max_vus(),
            environment: None,
            steps: Vec::new(),
            thresholds: vec![Threshold {
                metric: "http_req_failed".into(),
                stat: "rate".into(),
                op: ThresholdOp::Lt,
                value_ms: None,
                value: Some(0.01),
                abort_on_fail: false,
            }],
            run_scripts: false,
            new_connection_per_iteration: false,
            verify_tls: true,
            timeout_ms: default_timeout_ms(),
            grpc_channels: None,
        }
    }

    pub fn total_duration_sec(&self) -> u64 {
        // Saturating: a geometric repeat scale can push stages to u64::MAX.
        self.flat_stages()
            .iter()
            .fold(0u64, |acc, s| acc.saturating_add(s.duration_sec))
    }
}

// ---------------------------------------------------------------------------
// Run results (.swarmo/runs/<id>/)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagStats {
    pub tag: String,
    pub count: u64,
    pub errors: u64,
    /// The fastest response seen. A p50 far above it says the fast path exists
    /// but most requests are not taking it.
    #[serde(default)]
    pub min: f64,
    pub p50: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
    /// The tail that only shows up at volume; at 10k requests this is the
    /// slowest 10.
    #[serde(default)]
    pub p999: f64,
    pub max: f64,
    pub avg: f64,
    /// Response bytes received for this tag.
    #[serde(default)]
    pub bytes_in: u64,
    /// Request body bytes sent for this tag.
    #[serde(default)]
    pub bytes_out: u64,
}

/// How many responses carried a given status.
///
/// Keyed by protocol as well as code: the two schemes collide at zero, where
/// gRPC means OK and an HTTP request means it never got a response at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusCount {
    pub protocol: Protocol,
    pub code: u16,
    pub count: u64,
}

/// The canonical name for a gRPC status code.
///
/// The single source of truth for the whole workspace: the CLI, the report
/// generator and anything else render from here, so a code cannot be named two
/// different things in two different places.
pub fn grpc_code_name(code: u16) -> &'static str {
    match code {
        0 => "OK",
        1 => "CANCELLED",
        2 => "UNKNOWN",
        3 => "INVALID_ARGUMENT",
        4 => "DEADLINE_EXCEEDED",
        5 => "NOT_FOUND",
        6 => "ALREADY_EXISTS",
        7 => "PERMISSION_DENIED",
        8 => "RESOURCE_EXHAUSTED",
        9 => "FAILED_PRECONDITION",
        10 => "ABORTED",
        11 => "OUT_OF_RANGE",
        12 => "UNIMPLEMENTED",
        13 => "INTERNAL",
        14 => "UNAVAILABLE",
        15 => "DATA_LOSS",
        16 => "UNAUTHENTICATED",
        _ => "UNKNOWN",
    }
}

impl StatusCount {
    /// Name this status the way its own protocol does.
    ///
    /// The protocol has to come from the sample rather than the number: the
    /// two schemes collide at zero, where gRPC means OK and HTTP means the
    /// request never got a response at all.
    pub fn describe(&self) -> String {
        match self.protocol {
            Protocol::Grpc => format!("gRPC {} {}", self.code, grpc_code_name(self.code)),
            Protocol::Http if self.code == 0 => "HTTP (no response)".to_string(),
            Protocol::Http => format!("HTTP {}", self.code),
            // A WebSocket has no status codes; the sample records 101 for a
            // completed handshake, 0 for an answered message, and otherwise
            // the close code the connection ended with.
            Protocol::Ws if self.code == 101 => "WS connected".to_string(),
            Protocol::Ws if self.code == 0 => "WS message".to_string(),
            Protocol::Ws => format!("WS closed {}", self.code),
        }
    }

    /// Whether this status counts as a success for its protocol.
    pub fn is_ok(&self) -> bool {
        match self.protocol {
            Protocol::Grpc => self.code == 0,
            Protocol::Http => (200..400).contains(&self.code),
            Protocol::Ws => matches!(self.code, 0 | 101 | 1000),
        }
    }
}

/// Accepts both the current shape and the one written before status counts
/// carried a protocol.
#[derive(Deserialize)]
#[serde(untagged)]
enum StatusCountCompat {
    Current(StatusCount),
    /// `[code, count]`, as written by earlier versions.
    Legacy(u16, u64),
}

/// Read a status breakdown, upgrading the old `[code, count]` pairs in place.
///
/// Old files cannot say which protocol a code belonged to, so the code itself
/// has to imply it: that is the same inference those runs were displayed with
/// when they were written, so they read exactly as they always did. Only runs
/// recorded from now on can tell an HTTP request that got no response apart
/// from a successful gRPC call, since both are zero.
fn de_status_codes<'de, D>(d: D) -> std::result::Result<Vec<StatusCount>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<StatusCountCompat>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .map(|c| match c {
            StatusCountCompat::Current(sc) => sc,
            StatusCountCompat::Legacy(code, count) => StatusCount {
                protocol: if code >= 100 {
                    Protocol::Http
                } else {
                    Protocol::Grpc
                },
                code,
                count,
            },
        })
        .collect())
}

/// One bar of a latency distribution.
///
/// Percentiles say where the mass sits but not what shape it is: a run whose
/// requests are half fast and half slow has the same p50 as one where every
/// request is mediocre. This is what tells them apart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DistBucket {
    /// Upper bound of the bucket, in milliseconds.
    pub upper_ms: f64,
    pub count: u64,
}

/// How many requests failed with a given error.
///
/// A status breakdown cannot explain a request that never reached the server;
/// How one round of a fixed-count run went.
///
/// A repeated blast is only worth running twice if you can compare the passes,
/// and a whole-run average hides exactly what you were looking for: the first
/// round paying for cold connections and an empty cache, the fourth one
/// slowing down because something is not recovering between them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoundStats {
    /// 1-based, in execution order.
    pub index: u32,
    /// Requests the round was asked to send, which is the budget rather than
    /// the number that finished — they differ when a run is cancelled.
    pub planned: u64,
    pub concurrency: u32,
    /// Seconds the round itself took, excluding the pause after it.
    pub wall_sec: f64,
    /// Requests per second *within* the round, so the gaps do not depress it.
    pub rps: f64,
    /// The pause that followed this round, if any.
    #[serde(default)]
    pub gap_sec: u64,
    /// Counts, latency and bytes for this round alone.
    pub stats: TagStats,
}

/// The comparison a repeated blast exists to make.
///
/// Derived rather than stored: everything here is a fact about the list of
/// rounds, and two copies of a fact drift apart.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoundsSummary {
    pub rounds: usize,
    /// Mean, fastest and slowest round wall time, in seconds.
    pub mean_wall_sec: f64,
    pub fastest_wall_sec: f64,
    pub slowest_wall_sec: f64,
    /// The first and last round's wall time — the cold/warm comparison.
    pub first_wall_sec: f64,
    pub last_wall_sec: f64,
    /// Last minus first, in seconds. Negative means it sped up.
    pub change_sec: f64,
    /// Total seconds spent waiting between rounds.
    pub total_gap_sec: u64,
}

impl RoundsSummary {
    /// Summarise a list of rounds, or `None` when there are none to compare.
    pub fn of(rounds: &[RoundStats]) -> Option<Self> {
        let first = rounds.first()?;
        let last = rounds.last()?;
        let walls: Vec<f64> = rounds.iter().map(|r| r.wall_sec).collect();
        let total: f64 = walls.iter().sum();
        Some(Self {
            rounds: rounds.len(),
            mean_wall_sec: total / rounds.len() as f64,
            fastest_wall_sec: walls.iter().cloned().fold(f64::INFINITY, f64::min),
            slowest_wall_sec: walls.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            first_wall_sec: first.wall_sec,
            last_wall_sec: last.wall_sec,
            change_sec: last.wall_sec - first.wall_sec,
            // The pause after the final round is not waited out, so it is not
            // counted here either.
            total_gap_sec: rounds
                .iter()
                .take(rounds.len() - 1)
                .map(|r| r.gap_sec)
                .sum(),
        })
    }
}

/// this says whether that was a refused connection, a timeout, or TLS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorCount {
    pub message: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckStats {
    pub name: String,
    pub passes: u64,
    pub fails: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub elapsed_sec: u64,
    pub active_vus: u32,
    pub target_vus: f64,
    pub rps: f64,
    pub error_rate: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub per_tag: Vec<TagStats>,
    pub checks: Vec<CheckStats>,
    pub total_requests: u64,
    pub total_errors: u64,
    pub samples_dropped: u64,
    pub dropped_iterations: u64,
    pub vus_saturated: bool,
    pub threshold_results: Vec<ThresholdResult>,
    /// Response bytes received so far.
    #[serde(default)]
    pub bytes_in: u64,
    /// Bytes received per second over the last interval.
    #[serde(default)]
    pub bytes_per_sec: f64,
    /// Request body bytes sent so far, and the recent send rate — the
    /// headline numbers when the payload is the thing under test.
    #[serde(default)]
    pub bytes_out: u64,
    #[serde(default)]
    pub bytes_out_per_sec: f64,
    /// Failures as a share of the last interval only.
    ///
    /// The cumulative rate is what a threshold judges a whole run by; this is
    /// what says whether it is failing right now.
    #[serde(default)]
    pub interval_error_rate: f64,
    /// Latency percentiles over the last interval only.
    #[serde(default)]
    pub interval_p95: f64,
    #[serde(default)]
    pub interval_p99: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThresholdResult {
    pub description: String,
    pub metric: String,
    pub stat: String,
    pub target: f64,
    pub actual: f64,
    pub passed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunState {
    Running,
    Passed,
    Failed,
    Stopped,
    Errored,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    #[serde(default = "default_version")]
    pub version: u32,
    pub run_id: String,
    pub scenario_name: String,
    pub scenario_ref: String,
    /// The scenario's stable id, so "run again" survives a rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    /// Unix millis.
    pub started_at: u64,
    pub ended_at: u64,
    pub duration_sec: f64,
    pub state: RunState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub total_requests: u64,
    pub total_errors: u64,
    pub error_rate: f64,
    pub rps: f64,
    pub overall: TagStats,
    pub per_tag: Vec<TagStats>,
    pub checks: Vec<CheckStats>,
    pub thresholds: Vec<ThresholdResult>,
    pub samples_dropped: u64,
    pub dropped_iterations: u64,
    #[serde(default, deserialize_with = "de_status_codes")]
    pub status_codes: Vec<StatusCount>,
    /// Failures grouped by message, most frequent first.
    #[serde(default)]
    pub errors_by_message: Vec<ErrorCount>,
    /// Total response bytes received.
    #[serde(default)]
    pub bytes_in: u64,
    /// Mean bytes received per second across the run.
    #[serde(default)]
    pub bytes_per_sec: f64,
    /// Total request body bytes sent, and the mean send rate.
    #[serde(default)]
    pub bytes_out: u64,
    #[serde(default)]
    pub bytes_out_per_sec: f64,
    /// The highest one-second rate the run reached, as opposed to its mean.
    #[serde(default)]
    pub peak_rps: f64,
    /// The shape of the latency distribution. Empty for runs recorded before
    /// it was captured, in which case the section is simply not shown.
    #[serde(default)]
    pub latency_distribution: Vec<DistBucket>,
    /// Per-round results, for a fixed-count run. Empty for every other shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rounds: Vec<RoundStats>,
    /// Why the run ended before its ramp finished, if it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_because: Option<String>,
    /// How many times an auth token had to be refreshed mid-run. Worth stating
    /// because a refresh costs a retried request, not because it is a problem.
    #[serde(default)]
    pub token_refreshes: u64,
}

// ---------------------------------------------------------------------------
// Request history (.swarmo/history.json)
// ---------------------------------------------------------------------------

/// How many sends to keep. Old entries fall off the end.
pub const MAX_HISTORY: usize = 200;

/// Longest request body kept in history.
///
/// History exists to answer "what did I actually send", which the first few KB
/// almost always answers. Without a cap one file upload would put megabytes
/// into the workspace and keep them there.
pub const MAX_HISTORY_BODY: usize = 16 * 1024;

/// What was actually put on the wire, after variables were resolved.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySentRequest {
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// True when `body` was cut to [`MAX_HISTORY_BODY`], so the UI can say so
    /// rather than presenting a truncated body as the whole thing.
    #[serde(default)]
    pub body_truncated: bool,
}

/// Trim a body to [`MAX_HISTORY_BODY`], reporting whether anything was cut.
///
/// Requests and responses share this so their caps cannot drift apart, and it
/// cuts back to a character boundary so a multi-byte character straddling the
/// limit never produces invalid UTF-8.
pub fn truncate_for_history(body: String) -> (String, bool) {
    if body.len() <= MAX_HISTORY_BODY {
        return (body, false);
    }
    let mut cut = MAX_HISTORY_BODY;
    while cut > 0 && !body.is_char_boundary(cut) {
        cut -= 1;
    }
    (body[..cut].to_string(), true)
}

/// Header names whose values are credentials and must not be logged in full.
const SECRET_HEADERS: [&str; 4] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "x-api-key",
];

/// Replace a credential with enough of itself to be recognised, and no more.
///
/// History is written to the workspace, which people commit and share. A token
/// that rotates hourly ending up in a file that lives forever is exactly the
/// kind of leak nobody notices until it matters.
pub fn redact_secret(value: &str) -> String {
    // Keep any scheme word ("Bearer", "Basic") — it says what kind of
    // credential this was, and is not itself secret.
    // A word holding '=' or ';' is not a scheme: in "session=abc; theme=dark"
    // it is the first cookie, and keeping it would keep the secret.
    let (scheme, secret) = match value.split_once(' ') {
        Some((s, rest)) if !rest.trim().is_empty() && !s.contains(['=', ';']) => {
            (format!("{s} "), rest.trim())
        }
        _ => (String::new(), value),
    };
    let chars: Vec<char> = secret.chars().collect();
    // Four characters each end are only a small part of a long token; of a
    // short one they would be most of it.
    if chars.len() < 16 {
        return format!("{scheme}… (redacted)");
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{scheme}{head}…{tail} (redacted)")
}

/// Redact any header carrying a credential.
pub fn redact_headers(headers: Vec<(String, String)>) -> Vec<(String, String)> {
    headers
        .into_iter()
        .map(|(k, v)| {
            let lower = k.to_ascii_lowercase();
            let lower = lower.strip_prefix("trailer:").unwrap_or(&lower);
            if SECRET_HEADERS.contains(&lower) {
                (k, redact_secret(&v))
            } else {
                (k, v)
            }
        })
        .collect()
}

impl HistorySentRequest {
    /// Build an entry, redacting credentials and trimming an oversized body.
    ///
    /// Redaction happens here rather than at each call site so that adding a
    /// new kind of send cannot accidentally log a token in full.
    pub fn new(headers: Vec<(String, String)>, body: Option<String>) -> Self {
        let mut truncated = false;
        let body = body.map(|b| {
            let (trimmed, cut) = truncate_for_history(b);
            truncated = cut;
            trimmed
        });
        Self {
            headers: redact_headers(headers),
            body,
            body_truncated: truncated,
        }
    }
}

/// What came back, kept so a send can be reviewed after the fact.
///
/// A status and a duration say a request failed; this says what the server
/// actually replied, which is the other half of the question.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryResponse {
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// True when `body` was cut to [`MAX_HISTORY_BODY`].
    #[serde(default)]
    pub body_truncated: bool,
}

impl HistoryResponse {
    /// Build an entry, redacting credentials and trimming an oversized body.
    pub fn new(headers: Vec<(String, String)>, body: Option<String>) -> Self {
        let mut truncated = false;
        let body = body.map(|b| {
            let (trimmed, cut) = truncate_for_history(b);
            truncated = cut;
            trimmed
        });
        Self {
            headers: redact_headers(headers),
            body,
            body_truncated: truncated,
        }
    }
}

/// One send, kept so it can be reviewed after the fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    /// Where the request was when this was sent.
    pub request_ref: String,
    /// Which request it was, so a later rename does not orphan the entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub protocol: Protocol,
    /// The HTTP verb, or `GRPC`.
    pub method: String,
    /// The HTTP URL, or `grpc://host/package.Service/Method`.
    pub url: String,
    /// The HTTP status, or the gRPC code. Zero when the send never completed.
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    pub duration_ms: f64,
    #[serde(default)]
    pub response_bytes: u64,
    /// Unix millis.
    pub at: u64,
    pub ok: bool,
    /// Why the send failed, when it did not produce a response at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub request: HistorySentRequest,
    #[serde(default)]
    pub response: HistoryResponse,
}

/// A user's own name and notes for a run.
///
/// Kept apart from [`RunSummary`] and stored in its own file: the engine
/// rewrites the summary when a run ends, so anything annotated mid-run would
/// otherwise be overwritten by the final save.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAnnotation {
    /// Shown instead of the scenario name when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl RunAnnotation {
    /// True when there is nothing worth keeping on disk.
    pub fn is_empty(&self) -> bool {
        self.label.is_none() && self.notes.is_none()
    }

    /// Drop fields that are blank, so whitespace never counts as a name.
    pub fn normalized(self) -> Self {
        fn clean(v: Option<String>) -> Option<String> {
            v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        }
        Self {
            label: clean(self.label),
            notes: clean(self.notes),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunListEntry {
    pub run_id: String,
    pub scenario_name: String,
    /// The scenario's stable id, so the list can be scoped to one test even
    /// after it has been renamed or moved. Absent for runs recorded before
    /// ids existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    /// Where the scenario lived when the run happened. A display hint and a
    /// fallback for those older runs — never the thing scoping matches on,
    /// since a path is exactly what a move invalidates.
    #[serde(default)]
    pub scenario_ref: String,
    pub started_at: u64,
    pub state: RunState,
    pub total_requests: u64,
    pub error_rate: f64,
    pub p95: f64,
    /// The user's name for this run, if they gave it one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Whether notes exist, so the list can hint at them without loading them.
    #[serde(default)]
    pub has_notes: bool,
}

#[cfg(test)]
mod stage_tests {
    use super::*;

    fn stage(duration_sec: u64, target: f64) -> Stage {
        Stage {
            duration_sec,
            target,
            relative: false,
        }
    }

    fn relative(duration_sec: u64, target: f64) -> Stage {
        Stage {
            duration_sec,
            target,
            relative: true,
        }
    }

    fn blast(iterations: u64, gap_sec: u64) -> Blast {
        Blast {
            iterations,
            concurrency: 8,
            gap_sec,
            relative: false,
        }
    }

    #[test]
    fn plain_rounds_flatten_to_themselves() {
        let items: Vec<BlastItem> = vec![blast(5000, 5).into(), blast(5000, 0).into()];
        let out = flatten_blasts(&items);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].iterations, 5000);
        assert_eq!(out[0].gap_sec, 5);
        assert_eq!(out[1].iterations, 5000);
    }

    #[test]
    fn a_repeated_block_unrolls() {
        // "5,000 requests then wait 5s" three times over — the shape the
        // feature exists for.
        let items = vec![BlastItem::Repeat(BlastRepeat {
            times: 3,
            blasts: vec![blast(5000, 5)],
            iteration_scale: None,
        })];
        let out = flatten_blasts(&items);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|b| b.iterations == 5000 && b.gap_sec == 5));
    }

    #[test]
    fn a_scaled_block_grows_each_pass() {
        // Doubling each pass is how you find where throughput stops scaling.
        let items = vec![BlastItem::Repeat(BlastRepeat {
            times: 4,
            blasts: vec![blast(1000, 2)],
            iteration_scale: Some(2.0),
        })];
        let counts: Vec<u64> = flatten_blasts(&items)
            .iter()
            .map(|b| b.iterations)
            .collect();
        assert_eq!(counts, vec![1000, 2000, 4000, 8000]);
    }

    #[test]
    fn a_relative_round_counts_from_the_one_before() {
        let mut plus = blast(1000, 0);
        plus.relative = true;
        let items = vec![
            blast(2000, 0).into(),
            BlastItem::Repeat(BlastRepeat {
                times: 3,
                blasts: vec![plus],
                iteration_scale: None,
            }),
        ];
        let counts: Vec<u64> = flatten_blasts(&items)
            .iter()
            .map(|b| b.iterations)
            .collect();
        // 2000, then "another thousand" three times over.
        assert_eq!(counts, vec![2000, 3000, 4000, 5000]);
    }

    #[test]
    fn a_scenario_reads_its_rounds_the_same_either_way() {
        // The old single-round shape still means one round...
        let mut s = LoadScenario::new("x");
        s.iterations = Some(500);
        s.concurrency = Some(32);
        let rounds = s.rounds();
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].iterations, 500);
        assert_eq!(rounds[0].concurrency, 32);
        assert!(s.is_fixed_count());

        // ...and an explicit list wins over it.
        s.blasts = vec![blast(10, 1).into(), blast(20, 0).into()];
        let rounds = s.rounds();
        assert_eq!(rounds.len(), 2);
        assert_eq!(rounds[1].iterations, 20);

        // A duration-based scenario has no rounds at all.
        let plain = LoadScenario::new("y");
        assert!(plain.rounds().is_empty());
        assert!(!plain.is_fixed_count());
    }

    #[test]
    fn an_absurd_repeat_count_is_capped_before_anything_is_allocated() {
        // Four billion passes would previously be handed to with_capacity;
        // now the cap is applied first and the result is merely large.
        let items = vec![StageItem::Repeat(RepeatBlock {
            times: u32::MAX,
            stages: vec![stage(30, 10.0)],
            duration_scale: None,
        })];
        let out = flatten_stages(&items);
        assert_eq!(out.len(), MAX_EXPANDED_STAGES);

        let blasts = vec![BlastItem::Repeat(BlastRepeat {
            times: u32::MAX,
            blasts: vec![blast(1, 0)],
            iteration_scale: None,
        })];
        assert_eq!(flatten_blasts(&blasts).len(), MAX_EXPANDED_STAGES);
    }

    #[test]
    fn a_ramp_without_repeats_is_unchanged() {
        let items: Vec<StageItem> = vec![stage(15, 10.0).into(), stage(30, 50.0).into()];
        let flat = flatten_stages(&items);
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[0].target, 10.0);
        assert_eq!(flat[1].target, 50.0);
    }

    #[test]
    fn a_relative_target_builds_on_where_the_ramp_reached() {
        let items: Vec<StageItem> = vec![
            stage(30, 100.0).into(),
            relative(30, 100.0).into(),
            relative(60, 0.0).into(),
        ];
        let flat = flatten_stages(&items);
        assert_eq!(flat[0].target, 100.0);
        assert_eq!(flat[1].target, 200.0, "+100 should be 200, not 100");
        // Zero relative holds the current rate rather than dropping to nothing.
        assert_eq!(flat[2].target, 200.0);
        // Everything downstream sees plain absolute stages.
        assert!(flat.iter().all(|s| !s.relative));
    }

    #[test]
    fn a_repeat_climbs_a_step_at_a_time() {
        // "ramp to 100 over 30s, then repeat: +100 over 30s, hold 60s"
        let items = vec![
            StageItem::Stage(stage(30, 100.0)),
            StageItem::Repeat(RepeatBlock {
                times: 4,
                stages: vec![relative(30, 100.0), relative(60, 0.0)],
                duration_scale: None,
            }),
        ];
        let flat = flatten_stages(&items);

        assert_eq!(flat.len(), 1 + 4 * 2);
        assert_eq!(
            flat.iter().map(|s| s.target).collect::<Vec<_>>(),
            vec![100.0, 200.0, 200.0, 300.0, 300.0, 400.0, 400.0, 500.0, 500.0],
            "each repetition should climb, not restart"
        );
        assert_eq!(flat.iter().map(|s| s.duration_sec).sum::<u64>(), 390);
    }

    #[test]
    fn a_repeated_block_with_absolute_targets_simply_repeats() {
        // Not every block wants to climb: a fixed block is a soak cycle.
        let items = vec![StageItem::Repeat(RepeatBlock {
            times: 3,
            stages: vec![stage(10, 50.0), stage(10, 0.0)],
            duration_scale: None,
        })];
        let flat = flatten_stages(&items);
        assert_eq!(
            flat.iter().map(|s| s.target).collect::<Vec<_>>(),
            vec![50.0, 0.0, 50.0, 0.0, 50.0, 0.0]
        );
    }

    #[test]
    fn each_pass_can_be_longer_than_the_one_before() {
        let items = vec![StageItem::Repeat(RepeatBlock {
            times: 4,
            stages: vec![stage(30, 100.0)],
            duration_scale: Some(1.5),
        })];
        let flat = flatten_stages(&items);
        // 30, 45, 67.5 -> 68, 101.25 -> 101. Each computed from the original
        // 30 rather than the previous pass, so rounding does not compound.
        assert_eq!(
            flat.iter().map(|s| s.duration_sec).collect::<Vec<_>>(),
            vec![30, 45, 68, 101]
        );
    }

    #[test]
    fn a_scale_can_shorten_as_well_as_lengthen() {
        let items = vec![StageItem::Repeat(RepeatBlock {
            times: 3,
            stages: vec![stage(80, 10.0)],
            duration_scale: Some(0.5),
        })];
        assert_eq!(
            flatten_stages(&items)
                .iter()
                .map(|s| s.duration_sec)
                .collect::<Vec<_>>(),
            vec![80, 40, 20]
        );
    }

    #[test]
    fn no_scale_leaves_durations_alone() {
        for scale in [None, Some(1.0), Some(0.0), Some(-2.0)] {
            let items = vec![StageItem::Repeat(RepeatBlock {
                times: 3,
                stages: vec![stage(30, 10.0)],
                duration_scale: scale,
            })];
            // Zero and negative are meaningless here; they must not collapse
            // every stage to nothing.
            assert_eq!(
                flatten_stages(&items)
                    .iter()
                    .map(|s| s.duration_sec)
                    .collect::<Vec<_>>(),
                vec![30, 30, 30],
                "scale {scale:?}"
            );
        }
    }

    #[test]
    fn scaling_and_climbing_compose() {
        // "+100 over 30s, hold 60s — each pass 50% longer"
        let items = vec![StageItem::Repeat(RepeatBlock {
            times: 3,
            stages: vec![relative(30, 100.0), relative(60, 0.0)],
            duration_scale: Some(1.5),
        })];
        let flat = flatten_stages(&items);
        assert_eq!(
            flat.iter().map(|s| s.duration_sec).collect::<Vec<_>>(),
            vec![30, 60, 45, 90, 68, 135]
        );
        assert_eq!(
            flat.iter().map(|s| s.target).collect::<Vec<_>>(),
            vec![100.0, 100.0, 200.0, 200.0, 300.0, 300.0]
        );
    }

    #[test]
    fn a_stop_condition_watches_the_recent_window() {
        let condition = StopCondition {
            metric: "errorRate".into(),
            above: Some(0.1),
            below: None,
            for_intervals: 2,
        };
        let mut snap = Snapshot {
            elapsed_sec: 1,
            active_vus: 1,
            target_vus: 1.0,
            rps: 100.0,
            error_rate: 0.001,
            p50: 1.0,
            p95: 2.0,
            p99: 3.0,
            per_tag: Vec::new(),
            checks: Vec::new(),
            total_requests: 100_000,
            total_errors: 100,
            samples_dropped: 0,
            dropped_iterations: 0,
            vus_saturated: false,
            threshold_results: Vec::new(),
            bytes_in: 0,
            bytes_out: 0,
            bytes_per_sec: 0.0,
            bytes_out_per_sec: 0.0,
            interval_error_rate: 0.5,
            interval_p95: 2.0,
            interval_p99: 3.0,
        };

        // The run is failing badly right now, even though its cumulative rate
        // is still 0.1% after a long clean stretch. That is the whole point.
        assert!(condition.breached_by(&snap));
        snap.interval_error_rate = 0.01;
        assert!(!condition.breached_by(&snap));
    }

    #[test]
    fn a_condition_with_no_bound_never_fires() {
        let condition = StopCondition {
            metric: "errorRate".into(),
            above: None,
            below: None,
            for_intervals: 1,
        };
        let snap = Snapshot {
            elapsed_sec: 1,
            active_vus: 0,
            target_vus: 0.0,
            rps: 0.0,
            error_rate: 1.0,
            p50: 0.0,
            p95: 0.0,
            p99: 0.0,
            per_tag: Vec::new(),
            checks: Vec::new(),
            total_requests: 0,
            total_errors: 0,
            samples_dropped: 0,
            dropped_iterations: 0,
            vus_saturated: false,
            threshold_results: Vec::new(),
            bytes_in: 0,
            bytes_out: 0,
            bytes_per_sec: 0.0,
            bytes_out_per_sec: 0.0,
            interval_error_rate: 1.0,
            interval_p95: 0.0,
            interval_p99: 0.0,
        };
        // An empty condition ending every run immediately would be worse than
        // it doing nothing.
        assert!(!condition.breached_by(&snap));
        // And an unknown metric is not a silent always-true either.
        let unknown = StopCondition {
            metric: "made_up".into(),
            above: Some(0.0),
            below: None,
            for_intervals: 1,
        };
        assert!(!unknown.breached_by(&snap));
    }

    #[test]
    fn a_relative_target_never_goes_below_zero() {
        // Ramping down further than the current rate is a stop, not a
        // negative arrival rate.
        let items: Vec<StageItem> = vec![stage(10, 50.0).into(), relative(10, -200.0).into()];
        assert_eq!(flatten_stages(&items)[1].target, 0.0);
    }

    #[test]
    fn repeating_zero_or_one_time_still_runs_the_block_once() {
        for times in [0, 1] {
            let items = vec![StageItem::Repeat(RepeatBlock {
                times,
                stages: vec![stage(10, 5.0)],
                duration_scale: None,
            })];
            assert_eq!(flatten_stages(&items).len(), 1, "times = {times}");
        }
    }

    #[test]
    fn an_absurd_repeat_count_is_capped_rather_than_expanded() {
        // A mistyped count must not turn into a plan with millions of stages.
        let items = vec![StageItem::Repeat(RepeatBlock {
            times: 1_000_000,
            stages: vec![stage(1, 1.0)],
            duration_scale: None,
        })];
        assert_eq!(flatten_stages(&items).len(), MAX_EXPANDED_STAGES);
    }

    #[test]
    fn scenarios_written_before_repeats_existed_still_read() {
        // The on-disk shape has not changed for a plain ramp, so an untagged
        // stage must still deserialize as one.
        let json = r#"{"version":1,"name":"old","mode":"closed",
            "stages":[{"durationSec":15,"target":10},{"durationSec":5,"target":0}],
            "maxVus":10,"steps":[],"thresholds":[],"checks":[]}"#;
        let scenario: LoadScenario = serde_json::from_str(json).expect("should read");
        assert_eq!(scenario.stages.len(), 2);
        let flat = scenario.flat_stages();
        assert_eq!(flat[0].target, 10.0);
        assert_eq!(flat[1].target, 0.0);
        assert_eq!(scenario.total_duration_sec(), 20);
    }

    #[test]
    fn a_repeat_block_round_trips_through_json() {
        let scenario_json = r#"{"version":1,"name":"climb","mode":"open",
            "stages":[
              {"durationSec":30,"target":100},
              {"times":3,"stages":[
                 {"durationSec":30,"target":100,"relative":true},
                 {"durationSec":60,"target":0,"relative":true}]}
            ],
            "maxVus":500,"steps":[],"thresholds":[],"checks":[]}"#;
        let scenario: LoadScenario = serde_json::from_str(scenario_json).expect("should read");
        assert_eq!(scenario.stages.len(), 2);
        assert!(matches!(scenario.stages[1], StageItem::Repeat(_)));
        assert_eq!(scenario.flat_stages().len(), 7);

        // And survives being written back out.
        let written = serde_json::to_string(&scenario).unwrap();
        let again: LoadScenario = serde_json::from_str(&written).unwrap();
        assert_eq!(again.flat_stages().len(), 7);
        assert_eq!(again.flat_stages().last().unwrap().target, 400.0);
    }
}
