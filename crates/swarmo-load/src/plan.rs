//! Turning a `*.load.json` scenario or a `*.user.js` script into an executable
//! plan. The file formats are in docs/formats.md.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use swarmo_core::model::*;
use swarmo_core::model_grpc::MergedGrpcRequest;
use swarmo_core::{MergedRequest, Protocol, WorkspaceStore};
use swarmo_grpc::DescriptorSource;
use swarmo_script::{UserMixEntry, VuOptions};

use crate::auth::{AuthTokenState, Token, TokenCache};

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Core(#[from] swarmo_core::CoreError),
    #[error("script error: {0}")]
    Script(String),
}

/// What a step actually does. HTTP and gRPC steps mix freely in one scenario;
/// everything above this point — scheduling, metrics, thresholds, captures —
/// is protocol-neutral.
#[derive(Clone)]
pub enum PreparedAction {
    Http(Box<MergedRequest>),
    /// One WebSocket session per iteration: connect, run the message list,
    /// close. Measured as one connect sample plus one sample per message
    /// that waited for a reply.
    Ws(Box<swarmo_core::MergedWsRequest>),
    Grpc {
        merged: Box<MergedGrpcRequest>,
        descriptors: Arc<DescriptorSource>,
        /// The request message, parsed once at planning time.
        ///
        /// Only set when the message has no `{{variables}}`, so it is identical
        /// every iteration. Turning a large tensor's JSON into protobuf on
        /// every iteration would make the load generator measure its own parser
        /// rather than the server.
        prepared_message: Option<Arc<prost_reflect::DynamicMessage>>,
    },
}

impl std::fmt::Debug for PreparedAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PreparedAction::Http(m) => f.debug_tuple("Http").field(&m.name).finish(),
            PreparedAction::Ws(m) => f.debug_tuple("Ws").field(&m.name).finish(),
            PreparedAction::Grpc { merged, .. } => f
                .debug_struct("Grpc")
                .field("name", &merged.name)
                .field("method", &format!("{}/{}", merged.service, merged.method))
                .finish(),
        }
    }
}

/// The schema and settings backing `ctx.grpc` in a scripted run.
#[derive(Clone)]
pub struct ScriptedGrpc {
    pub descriptors: Arc<DescriptorSource>,
    pub source: swarmo_core::ProtoSource,
    pub verify_tls: bool,
    pub timeout_ms: u64,
    pub max_response_bytes: u64,
}

impl std::fmt::Debug for ScriptedGrpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedGrpc")
            .field("origin", &self.descriptors.origin())
            .finish()
    }
}

#[derive(Clone)]
pub struct PreparedStep {
    pub action: PreparedAction,
    pub tag: String,
    pub think_time_ms: Option<[u64; 2]>,
    pub capture: Vec<Capture>,
    /// The live auth token this step sends, when its auth comes from a
    /// command. Shared across every virtual user so one refresh serves all.
    pub auth_token: Option<Arc<AuthTokenState>>,
    /// Runs concurrently with the step above it; see [`LoadStep::parallel`].
    pub parallel: bool,
}

impl std::fmt::Debug for PreparedStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedStep")
            .field("action", &self.action)
            .field("tag", &self.tag)
            .field("think_time_ms", &self.think_time_ms)
            .field("capture", &self.capture)
            .field("auth_token", &self.auth_token.is_some())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub enum PlanKind {
    /// Engine A: pure Rust, no JS in the hot path.
    Native { steps: Vec<PreparedStep> },
    /// Engine B: one QuickJS context per virtual user.
    Scripted {
        source: String,
        user_mix: Vec<UserMixEntry>,
        /// Present when the script declared `options.grpc`.
        grpc: Option<ScriptedGrpc>,
    },
}

#[derive(Debug, Clone)]
pub struct LoadPlan {
    pub name: String,
    pub scenario_ref: String,
    /// The scenario's stable id, recorded on the run so "run again" survives a
    /// rename. `None` for user scripts, which have no id of their own.
    pub scenario_id: Option<String>,
    pub mode: LoadMode,
    pub stages: Vec<Stage>,
    /// A flat rate held for the whole run, overriding stage targets.
    pub constant_rate: Option<f64>,
    /// Run length when there are no stages.
    pub flat_duration_sec: u64,
    pub max_vus: u32,
    pub thresholds: Vec<Threshold>,
    /// End the run early once this holds. Unlike a threshold, reaching it is
    /// the goal rather than a failure.
    pub stop_when: Option<StopCondition>,
    /// The rounds of a fixed-count run, already flattened; empty for a run
    /// measured in time. See [`LoadScenario::rounds`].
    pub rounds: Vec<Blast>,
    pub base_vars: HashMap<String, String>,
    pub verify_tls: bool,
    pub timeout_ms: u64,
    pub new_connection_per_iteration: bool,
    /// Channels to open per gRPC address; `None` means pick a default.
    pub grpc_channel_width: Option<u32>,
    pub kind: PlanKind,
}

/// Hard safety caps.
pub const MAX_VUS_HARD_CAP: u32 = 10_000;
pub const MAX_RATE_HARD_CAP: f64 = 50_000.0;
/// Scripted VUs get an OS thread each, so they are capped much lower.
pub const MAX_SCRIPTED_VUS: u32 = 500;

impl LoadPlan {
    pub async fn from_scenario(
        store: &WorkspaceStore,
        scenario_ref: &str,
    ) -> Result<Self, PlanError> {
        let s = store.get_scenario(scenario_ref)?;

        if s.steps.is_empty() {
            return Err(PlanError::Invalid(
                "this scenario has no steps — add at least one request".into(),
            ));
        }
        // Either ramps, or a flat rate with a duration. Both are valid shapes.
        if !s.is_fixed_count() && s.stages.is_empty() && s.duration_sec.unwrap_or(0) == 0 {
            return Err(PlanError::Invalid(
                "this scenario has no stages — add at least one stage, or set \
                 arrivalRatePerSec together with durationSec for a flat run"
                    .into(),
            ));
        }
        if s.arrival_rate_per_sec.is_some() && s.mode != LoadMode::Open {
            return Err(PlanError::Invalid(
                "arrivalRatePerSec only applies in open mode, where the target \
                 is a request rate. Switch the mode, or use stages to ramp \
                 virtual users."
                    .into(),
            ));
        }
        if s.run_scripts {
            return Err(PlanError::Invalid(
                "`runScripts` requires the scripted engine; use a *.user.js script instead".into(),
            ));
        }

        let base_vars = store.var_scope(s.environment.as_deref())?.flatten();
        let mut var_scope = swarmo_core::VarScope::new();
        var_scope.push_layer(base_vars.clone());

        // Descriptors are compiled once per distinct proto source and shared by
        // every step that uses it.
        let mut descriptor_cache: HashMap<String, Arc<DescriptorSource>> = HashMap::new();

        let mut steps = Vec::with_capacity(s.steps.len());
        for step in &s.steps {
            // The stored path is only a hint. A request that has been renamed
            // or moved is found by its id, so a scenario does not break the
            // moment someone tidies up their collection.
            let request_ref = store
                .locate_request(&step.request_ref, step.request_id.as_deref())
                .map(|(current, _)| current)
                .unwrap_or_else(|_| step.request_ref.clone());
            let step = &LoadStep {
                request_ref,
                ..step.clone()
            };

            let (action, default_tag) = match WorkspaceStore::protocol_of(&step.request_ref) {
                Protocol::Http => {
                    let merged = store.merged_request(&step.request_ref).map_err(|e| {
                        PlanError::Invalid(format!(
                            "step references a request that could not be loaded ({}): {e}",
                            step.request_ref
                        ))
                    })?;
                    let tag = if merged.name.trim().is_empty() {
                        format!("{} {}", merged.method, merged.url)
                    } else {
                        merged.name.clone()
                    };
                    (PreparedAction::Http(Box::new(merged)), tag)
                }
                Protocol::Ws => {
                    let merged = store.merged_ws_request(&step.request_ref).map_err(|e| {
                        PlanError::Invalid(format!(
                            "step references a WebSocket request that could not be loaded \
                                     ({}): {e}",
                            step.request_ref
                        ))
                    })?;
                    if merged.url.trim().is_empty() {
                        return Err(PlanError::Invalid(format!(
                            "{}: the WebSocket request has no URL",
                            step.request_ref
                        )));
                    }
                    let tag = if merged.name.trim().is_empty() {
                        format!("WS {}", merged.url)
                    } else {
                        merged.name.clone()
                    };
                    (PreparedAction::Ws(Box::new(merged)), tag)
                }
                Protocol::Grpc => {
                    let merged = store.merged_grpc_request(&step.request_ref).map_err(|e| {
                        PlanError::Invalid(format!(
                            "step references a gRPC request that could not be loaded \
                                     ({}): {e}",
                            step.request_ref
                        ))
                    })?;

                    // Resolve now so proto paths and the address are concrete.
                    let resolved = swarmo_core::finalize_grpc(&merged, &var_scope);

                    let key = swarmo_grpc::cache_key(
                        &resolved.proto_source,
                        &resolved.address,
                        store.root(),
                    );
                    let descriptors = match descriptor_cache.get(&key) {
                        Some(d) => d.clone(),
                        None => {
                            // Fail here rather than mid-run: a bad .proto or an
                            // unreachable reflection endpoint should stop the
                            // run before a single request is sent.
                            let d = swarmo_grpc::load_descriptors(
                                &resolved.proto_source,
                                &resolved.address,
                                resolved.settings.verify_tls,
                                store.root(),
                                &resolved.metadata,
                            )
                            .await
                            .map_err(|e| {
                                PlanError::Invalid(format!(
                                    "the schema for {} could not be loaded: {e}",
                                    step.request_ref
                                ))
                            })?;
                            let d = Arc::new(d);
                            descriptor_cache.insert(key, d.clone());
                            d
                        }
                    };

                    // Surface an unknown method up front, before any traffic.
                    let method = descriptors
                        .method(&resolved.service, &resolved.method)
                        .map_err(|e| PlanError::Invalid(format!("{}: {e}", step.request_ref)))?;

                    // A message with no variables never changes between
                    // iterations, so parse it once here rather than on every
                    // one. This also surfaces a malformed message before any
                    // traffic is sent. The request side of a client stream is
                    // a list, parsed per call instead.
                    let prepared_message =
                        if merged.message.contains("{{") || method.is_client_streaming() {
                            None
                        } else {
                            let msg = swarmo_grpc::prepare_message(&descriptors, &resolved)
                                .map_err(|e| {
                                    PlanError::Invalid(format!("{}: {e}", step.request_ref))
                                })?;
                            Some(Arc::new(msg))
                        };

                    let tag = if merged.name.trim().is_empty() {
                        short_method(&resolved.service, &resolved.method)
                    } else {
                        merged.name.clone()
                    };
                    (
                        PreparedAction::Grpc {
                            merged: Box::new(merged),
                            descriptors,
                            prepared_message,
                        },
                        tag,
                    )
                }
            };

            steps.push(PreparedStep {
                action,
                tag: step.tag.clone().unwrap_or(default_tag),
                think_time_ms: step.think_time_ms,
                capture: step.capture.clone(),
                // Filled in by `install_auth_token` once the caller has
                // approved the command and fetched the first token.
                auth_token: None,
                parallel: step.parallel,
            });
        }

        let plan = LoadPlan {
            name: s.name.clone(),
            scenario_ref: scenario_ref.to_string(),
            scenario_id: Some(s.id.clone()),
            mode: s.mode,
            stages: s.flat_stages(),
            stop_when: s.stop_when.clone(),
            rounds: s
                .rounds()
                .into_iter()
                .map(|b| Blast {
                    concurrency: b.concurrency.clamp(1, MAX_VUS_HARD_CAP),
                    ..b
                })
                .collect(),
            constant_rate: s.arrival_rate_per_sec,
            flat_duration_sec: s.duration_sec.unwrap_or(0),
            max_vus: s.max_vus,
            thresholds: s.thresholds.clone(),
            base_vars,
            verify_tls: s.verify_tls,
            timeout_ms: s.timeout_ms,
            new_connection_per_iteration: s.new_connection_per_iteration,
            grpc_channel_width: s.grpc_channels,
            kind: PlanKind::Native { steps },
        };
        plan.validate()?;
        Ok(plan)
    }

    pub async fn from_user_script(
        store: &WorkspaceStore,
        script_ref: &str,
    ) -> Result<Self, PlanError> {
        let source = store.read_text(script_ref)?;

        // Load the script once, purely to read and validate its `options`.
        //
        // Scoped so the QuickJS engine is dropped before this function awaits
        // anything: it is not `Send`, and holding it across an await would make
        // the whole future unusable from a Tauri command.
        let options = {
            let host: swarmo_script::SharedHost = std::sync::Arc::new(swarmo_script::NullHost);
            let (engine, options) =
                swarmo_script::VuEngine::load(host, &source, 0, &HashMap::new())
                    .map_err(|e| PlanError::Script(e.to_string()))?;
            drop(engine);
            options
        };

        let name = script_ref
            .rsplit('/')
            .next()
            .unwrap_or(script_ref)
            .trim_end_matches(".user.js")
            .to_string();

        let mode = match options.mode.as_deref() {
            Some("open") => LoadMode::Open,
            _ => LoadMode::Closed,
        };

        let stages: Vec<Stage> = options
            .stages
            .iter()
            .map(|s| Stage {
                relative: false,
                duration_sec: s.duration_sec,
                target: s.target,
            })
            .collect();
        if stages.is_empty() {
            return Err(PlanError::Invalid(
                "the script's `options.stages` is empty — add at least one stage".into(),
            ));
        }

        let thresholds = parse_thresholds(&options)?;
        let base_vars = store.var_scope(options.environment.as_deref())?.flatten();

        let user_mix = if options.user_mix.is_empty() {
            vec![UserMixEntry {
                exec: "default".into(),
                weight: 1.0,
            }]
        } else {
            options.user_mix.clone()
        };
        if user_mix.iter().all(|u| u.weight <= 0.0) {
            return Err(PlanError::Invalid(
                "every entry in `options.userMix` has a weight of zero".into(),
            ));
        }

        // Compile the schema up front, so a bad .proto or an unreachable
        // reflection endpoint stops the run before any traffic is sent.
        let grpc = match &options.grpc {
            None => None,
            Some(g) => {
                let mut scope = swarmo_core::VarScope::new();
                scope.push_layer(base_vars.clone());

                let proto_source = if g.reflection {
                    swarmo_core::ProtoSource::Reflection
                } else if let Some(dir) = g.proto_dir.as_deref().filter(|d| !d.trim().is_empty()) {
                    swarmo_core::ProtoSource::Directory {
                        root: dir.to_string(),
                        entry_files: g.proto_files.clone(),
                    }
                } else {
                    if g.proto_files.is_empty() {
                        return Err(PlanError::Invalid(
                            "`options.grpc` needs one of `protoDir`, `protoFiles: [...]` \
                             or `reflection: true`"
                                .into(),
                        ));
                    }
                    swarmo_core::ProtoSource::Files {
                        files: g.proto_files.clone(),
                        include_paths: g.include_paths.clone(),
                    }
                };

                let address = g
                    .address
                    .as_deref()
                    .map(|a| swarmo_core::interpolate_str(a, &scope))
                    .map(|a| swarmo_core::model_grpc::normalize_address(&a))
                    .unwrap_or_default();

                if proto_source.is_reflection() && address.is_empty() {
                    return Err(PlanError::Invalid(
                        "`options.grpc.reflection` needs an `address` to fetch the schema from"
                            .into(),
                    ));
                }

                // Like `timeoutMs` below, the gRPC setting falls back to the
                // script-wide one rather than straight to the default.
                let verify_tls = g.verify_tls.or(options.verify_tls).unwrap_or(true);

                // Reflection is an RPC too, so a server that demands auth needs
                // it here as well as on the calls themselves.
                let mut script_metadata: Vec<(String, String)> = g
                    .metadata
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.trim().to_lowercase(),
                            swarmo_core::interpolate_str(v, &scope),
                        )
                    })
                    .filter(|(k, _)| !k.is_empty())
                    .collect();
                script_metadata.sort();
                let descriptors = swarmo_grpc::load_descriptors(
                    &proto_source,
                    &address,
                    verify_tls,
                    store.root(),
                    &script_metadata,
                )
                .await
                .map_err(|e| {
                    PlanError::Invalid(format!("the schema in `options.grpc` failed to load: {e}"))
                })?;

                Some(ScriptedGrpc {
                    descriptors: Arc::new(descriptors),
                    source: proto_source,
                    verify_tls,
                    timeout_ms: g.timeout_ms.or(options.timeout_ms).unwrap_or(30_000),
                    max_response_bytes: g.max_response_bytes.unwrap_or(16 * 1024 * 1024),
                })
            }
        };

        let plan = LoadPlan {
            name,
            scenario_ref: script_ref.to_string(),
            scenario_id: None,
            // A user script defines its own ramp in `options`; a stop
            // condition belongs to the declarative form.
            stop_when: None,
            rounds: Vec::new(),
            mode,
            stages,
            constant_rate: None,
            flat_duration_sec: 0,
            max_vus: options.max_vus.unwrap_or(100),
            thresholds,
            base_vars,
            verify_tls: options.verify_tls.unwrap_or(true),
            timeout_ms: options.timeout_ms.unwrap_or(30_000),
            new_connection_per_iteration: false,
            grpc_channel_width: None,
            kind: PlanKind::Scripted {
                source,
                user_mix,
                grpc,
            },
        };
        plan.validate()?;
        Ok(plan)
    }

    fn validate(&self) -> Result<(), PlanError> {
        // A fixed-count run ignores stages, mode and maxVus entirely —
        // leftover stages from a mode switch must not block it.
        if !self.rounds.is_empty() {
            if self.rounds.iter().all(|r| r.iterations == 0) {
                return Err(PlanError::Invalid(
                    "every round sends zero requests — give at least one a count".into(),
                ));
            }
            return Ok(());
        }

        let peak = self.peak_target();
        // Without this a run with a duration but nothing to drive it — no
        // stages and no rate, or only zero targets — sends nothing at all and
        // then reports that as a pass.
        if peak <= 0.0 || peak.is_nan() {
            return Err(PlanError::Invalid(match self.mode {
                LoadMode::Closed => "no stage targets any virtual users — give a stage a                                      target above zero"
                    .into(),
                LoadMode::Open => "the arrival rate is zero throughout — set                                    arrivalRatePerSec, or give a stage a target above zero"
                    .into(),
            }));
        }

        match self.mode {
            LoadMode::Closed => {
                if peak > self.max_vus as f64 {
                    return Err(PlanError::Invalid(format!(
                        "a stage targets {peak:.0} virtual users but maxVus is {}",
                        self.max_vus
                    )));
                }
            }
            LoadMode::Open => {
                if peak > MAX_RATE_HARD_CAP {
                    return Err(PlanError::Invalid(format!(
                        "arrival rate {peak:.0}/s exceeds the {MAX_RATE_HARD_CAP:.0}/s safety cap"
                    )));
                }
            }
        }

        let cap = match self.kind {
            PlanKind::Scripted { .. } => MAX_SCRIPTED_VUS,
            PlanKind::Native { .. } => MAX_VUS_HARD_CAP,
        };
        if self.max_vus > cap {
            return Err(PlanError::Invalid(format!(
                "maxVus {} exceeds the {} cap for this engine",
                self.max_vus, cap
            )));
        }
        if self.max_vus == 0 {
            return Err(PlanError::Invalid("maxVus must be at least 1".into()));
        }
        // A fixed-iteration run has no duration at all; it ends when the
        // budget is spent.
        if self.rounds.is_empty() && self.total_duration_sec() == 0 {
            return Err(PlanError::Invalid(
                "the stages add up to zero seconds".into(),
            ));
        }
        Ok(())
    }

    /// How many gRPC channels to open per address.
    ///
    /// Defaults to a few, capped by the CPU count: enough that one HTTP/2
    /// connection's flow-control window is not the limiting factor, without
    /// opening so many that connection setup dominates a short run.
    pub fn grpc_channels(&self) -> usize {
        match self.grpc_channel_width {
            Some(n) => (n as usize).clamp(1, 64),
            None => {
                let cpus = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4);
                cpus.clamp(1, 8)
            }
        }
    }

    pub fn peak_target(&self) -> f64 {
        match self.constant_rate {
            Some(r) => r,
            None => self.stages.iter().map(|s| s.target).fold(0.0, f64::max),
        }
    }

    pub fn stage_duration_sec(&self) -> u64 {
        // Saturating: a repeat block with a growing durationScale can pin
        // stage lengths at u64::MAX, and a plain sum would then overflow.
        self.stages
            .iter()
            .fold(0u64, |acc, s| acc.saturating_add(s.duration_sec))
    }

    pub fn total_duration_sec(&self) -> u64 {
        // A constant rate ignores stages completely — durations as well as
        // targets. Otherwise a scenario that still had its default stages
        // lying around would quietly run for their length instead of the
        // `durationSec` actually being edited.
        if self.constant_rate.is_some() && self.flat_duration_sec > 0 {
            return self.flat_duration_sec;
        }

        match self.stage_duration_sec() {
            0 => self.flat_duration_sec,
            n => n,
        }
    }

    pub fn total_duration(&self) -> Duration {
        Duration::from_secs(self.total_duration_sec())
    }

    /// The rate (open mode) or virtual-user count (closed mode) wanted right now.
    ///
    /// A constant rate wins over stages, so `arrivalRatePerSec` really does mean
    /// "hold this"; otherwise stages ramp linearly from the previous target.
    pub fn target_at(&self, elapsed: Duration) -> f64 {
        if let Some(rate) = self.constant_rate {
            return if elapsed < self.total_duration() {
                rate
            } else {
                0.0
            };
        }

        let t = elapsed.as_secs_f64();
        let mut prev = 0.0f64;
        let mut start = 0.0f64;
        for stage in &self.stages {
            let dur = stage.duration_sec as f64;
            let end = start + dur;
            if t < end {
                if dur <= 0.0 {
                    return stage.target;
                }
                let frac = ((t - start) / dur).clamp(0.0, 1.0);
                return prev + (stage.target - prev) * frac;
            }
            prev = stage.target;
            start = end;
        }
        0.0
    }

    /// Distinct hosts this plan will send traffic to, for the confirmation
    /// dialog. Scripted plans report what can be read statically.
    /// Every distinct auth command this plan would run, in step order.
    ///
    /// Reported before a run starts so the command can be approved then,
    /// rather than failing thirty seconds into a run.
    pub fn auth_commands(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut add = |auth: &Auth| {
            if let Auth::CommandToken { command, .. } = auth {
                if !command.trim().is_empty() && !out.contains(command) {
                    out.push(command.clone());
                }
            }
        };
        if let PlanKind::Native { steps } = &self.kind {
            for step in steps {
                match &step.action {
                    PreparedAction::Http(m) => add(&m.auth),
                    PreparedAction::Ws(m) => add(&m.auth),
                    PreparedAction::Grpc { merged, .. } => add(&merged.auth),
                }
            }
        }
        out
    }

    /// Give every step that uses `command` its live token.
    ///
    /// The token is shared rather than baked into each step, so that when it
    /// expires mid-run one refresh reaches every virtual user at once. The
    /// caller has already approved the command and fetched the first token.
    pub fn install_auth_token(&mut self, command: &str, cache: Arc<TokenCache>, initial: Token) {
        let mut state: Option<Arc<AuthTokenState>> = None;

        if let PlanKind::Native { steps } = &mut self.kind {
            for step in steps.iter_mut() {
                let auth = match &step.action {
                    PreparedAction::Http(m) => &m.auth,
                    PreparedAction::Ws(m) => &m.auth,
                    PreparedAction::Grpc { merged, .. } => &merged.auth,
                };
                let Auth::CommandToken {
                    command: c,
                    header_name,
                    prefix,
                } = auth
                else {
                    continue;
                };
                if c != command {
                    continue;
                }
                // One live token per command, built from the first step that
                // wants it; later steps share it. How it is sent stays each
                // step's own: two requests may run one command yet put its
                // output in different headers.
                let shared = state
                    .get_or_insert_with(|| {
                        Arc::new(AuthTokenState::new(
                            command.to_string(),
                            header_name.clone(),
                            prefix.clone(),
                            cache.clone(),
                            initial.clone(),
                        ))
                    })
                    .clone();
                step.auth_token = Some(
                    if shared.header_name == *header_name && shared.prefix == *prefix {
                        shared
                    } else {
                        Arc::new(shared.with_header(header_name.clone(), prefix.clone()))
                    },
                );
            }
        }
    }

    /// How many times a token was refreshed while the run was going.
    pub fn token_refreshes(&self) -> u64 {
        let mut seen: Vec<&AuthTokenState> = Vec::new();
        let mut total = 0;
        if let PlanKind::Native { steps } = &self.kind {
            for step in steps {
                if let Some(state) = &step.auth_token {
                    // Steps share one token per command; count it once.
                    if !seen.iter().any(|s| s.shares_token_with(state)) {
                        seen.push(state);
                        total += state.refresh_count();
                    }
                }
            }
        }
        total
    }

    pub fn target_hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = Vec::new();
        let mut add = |raw: &str| {
            if let Some(h) = host_of(raw) {
                if !hosts.contains(&h) {
                    hosts.push(h);
                }
            }
        };

        match &self.kind {
            PlanKind::Native { steps } => {
                let scope = self.scope();
                for s in steps {
                    match &s.action {
                        PreparedAction::Http(merged) => {
                            let (url, _) = swarmo_core::interpolate(&merged.url, &scope);
                            add(&url);
                        }
                        // A ws:// host is a host to approve like any other.
                        PreparedAction::Ws(merged) => {
                            let (url, _) = swarmo_core::interpolate(&merged.url, &scope);
                            add(&url);
                        }
                        PreparedAction::Grpc { merged, .. } => {
                            let (addr, _) = swarmo_core::interpolate(&merged.address, &scope);
                            add(&addr);
                        }
                    }
                }
            }
            PlanKind::Scripted { source, .. } => {
                let scope = self.scope();
                for raw in extract_urls(source) {
                    let (url, _) = swarmo_core::interpolate(&raw, &scope);
                    add(&url);
                }
            }
        }
        hosts
    }

    pub fn scope(&self) -> swarmo_core::VarScope {
        let mut scope = swarmo_core::VarScope::new();
        scope.push_layer(self.base_vars.clone());
        scope
    }
}

fn parse_thresholds(options: &VuOptions) -> Result<Vec<Threshold>, PlanError> {
    let mut out = Vec::new();
    for (i, raw) in options.thresholds.iter().enumerate() {
        let t: Threshold = serde_json::from_value(raw.clone()).map_err(|e| {
            PlanError::Invalid(format!("options.thresholds[{i}] is not valid: {e}"))
        })?;
        out.push(t);
    }
    Ok(out)
}

/// `OrderService/GetOrder` — the readable short form used for metric tags.
fn short_method(service: &str, method: &str) -> String {
    let short = service.rsplit('.').next().unwrap_or(service);
    format!("{short}/{method}")
}

fn host_of(url: &str) -> Option<String> {
    // A host that still has `{{vars}}` in it did not resolve; naming a
    // half-substituted host in the confirmation dialog would be misleading.
    // Only the scheme and authority matter: `/orders/{{orderId}}` is still
    // plainly traffic to its host, and dropping it would let that host skip
    // the approval gate.
    let (scheme, rest) = match url.find("://") {
        Some(i) => (&url[..i], &url[i + 3..]),
        None => ("", url),
    };
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    if scheme.contains("{{") || authority.contains("{{") {
        return None;
    }
    let normalized = if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    };
    url::Url::parse(&normalized)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
}

/// Pull quoted URL-ish literals out of a script for the host confirmation.
fn extract_urls(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for quote in ['"', '\'', '`'] {
        let mut rest = source;
        while let Some(start) = rest.find(quote) {
            let after = &rest[start + 1..];
            let Some(end) = after.find(quote) else { break };
            let literal = &after[..end];
            let interesting = literal.contains("://") || literal.starts_with("{{");
            if interesting && !out.contains(&literal.to_string()) {
                out.push(literal.to_string());
            }
            rest = &after[end + 1..];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_with(stages: Vec<(u64, f64)>) -> LoadPlan {
        LoadPlan {
            name: "t".into(),
            scenario_ref: "x".into(),
            scenario_id: None,
            stop_when: None,
            rounds: Vec::new(),
            mode: LoadMode::Closed,
            stages: stages
                .into_iter()
                .map(|(d, t)| Stage {
                    duration_sec: d,
                    target: t,
                    relative: false,
                })
                .collect(),
            constant_rate: None,
            flat_duration_sec: 0,
            max_vus: 1000,
            thresholds: vec![],
            base_vars: HashMap::new(),
            verify_tls: true,
            timeout_ms: 30_000,
            new_connection_per_iteration: false,
            grpc_channel_width: None,
            kind: PlanKind::Native { steps: vec![] },
        }
    }

    #[test]
    fn ramps_linearly_from_zero() {
        let p = plan_with(vec![(10, 100.0)]);
        assert_eq!(p.target_at(Duration::from_secs(0)), 0.0);
        assert_eq!(p.target_at(Duration::from_secs(5)), 50.0);
        assert!((p.target_at(Duration::from_secs_f64(9.9)) - 99.0).abs() < 1.0);
    }

    #[test]
    fn holds_then_ramps_down() {
        let p = plan_with(vec![(10, 100.0), (10, 100.0), (10, 0.0)]);
        assert_eq!(p.target_at(Duration::from_secs(15)), 100.0);
        assert_eq!(p.target_at(Duration::from_secs(25)), 50.0);
        assert_eq!(p.target_at(Duration::from_secs(31)), 0.0);
    }

    #[test]
    fn zero_length_stage_jumps_to_target() {
        let p = plan_with(vec![(0, 50.0), (10, 50.0)]);
        assert_eq!(p.target_at(Duration::from_secs(1)), 50.0);
    }

    #[test]
    fn a_constant_rate_holds_flat_and_overrides_stage_targets() {
        let mut p = plan_with(vec![(10, 999.0)]);
        p.constant_rate = Some(100.0);

        for t in [0.0, 0.1, 5.0, 9.9] {
            assert_eq!(
                p.target_at(Duration::from_secs_f64(t)),
                100.0,
                "flat at {t}s"
            );
        }
        // And stops when the run is over.
        assert_eq!(p.target_at(Duration::from_secs(10)), 0.0);
        assert_eq!(p.peak_target(), 100.0);
    }

    #[test]
    fn a_constant_rate_can_supply_its_own_duration_without_stages() {
        let mut p = plan_with(vec![]);
        p.constant_rate = Some(42.0);
        p.flat_duration_sec = 60;

        assert_eq!(p.total_duration_sec(), 60);
        assert_eq!(p.target_at(Duration::from_secs(30)), 42.0);
        assert_eq!(p.target_at(Duration::from_secs(60)), 0.0);
    }

    #[test]
    fn a_constant_rate_ignores_leftover_stages_entirely() {
        // A scenario created from the default template arrives with stages
        // adding up to 50s. Switching it to a constant rate and setting a
        // 10s duration must run for 10s, not 50.
        let mut p = plan_with(vec![(15, 10.0), (30, 10.0), (5, 0.0)]);
        assert_eq!(p.total_duration_sec(), 50, "the default stage total");

        p.constant_rate = Some(150.0);
        p.flat_duration_sec = 10;

        assert_eq!(p.total_duration_sec(), 10, "durationSec must win");
        assert_eq!(p.target_at(Duration::from_secs(9)), 150.0);
        assert_eq!(p.target_at(Duration::from_secs(10)), 0.0);
    }

    #[test]
    fn stages_supply_the_duration_when_no_flat_duration_is_set() {
        let mut p = plan_with(vec![(25, 5.0)]);
        p.constant_rate = Some(7.0);
        p.flat_duration_sec = 0;
        assert_eq!(p.total_duration_sec(), 25, "fall back to the stages");
    }

    #[test]
    fn peak_and_duration() {
        let p = plan_with(vec![(10, 20.0), (5, 80.0)]);
        assert_eq!(p.peak_target(), 80.0);
        assert_eq!(p.total_duration_sec(), 15);
    }

    #[test]
    fn validate_rejects_target_above_max_vus() {
        let mut p = plan_with(vec![(10, 500.0)]);
        p.max_vus = 100;
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_rejects_a_run_that_would_send_nothing() {
        // A duration with no stages and no rate, or only zero targets, used
        // to pass, send nothing, and report the empty run as passed.
        let mut p = plan_with(vec![]);
        p.flat_duration_sec = 30;
        assert!(p.validate().is_err(), "no stages, no rate");
        p.mode = LoadMode::Open;
        assert!(p.validate().is_err(), "open, no rate");
        p.constant_rate = Some(10.0);
        assert!(p.validate().is_ok(), "a flat rate is a real run");

        assert!(plan_with(vec![(10, 0.0)]).validate().is_err());
        assert!(plan_with(vec![(10, 5.0), (10, 0.0)]).validate().is_ok());
    }

    #[test]
    fn saturated_stage_lengths_do_not_overflow() {
        let p = plan_with(vec![(u64::MAX, 1.0), (u64::MAX, 1.0)]);
        assert_eq!(p.total_duration_sec(), u64::MAX);
    }

    #[test]
    fn validate_rejects_zero_duration() {
        let p = plan_with(vec![(0, 10.0)]);
        assert!(p.validate().is_err());
    }

    fn command_step(header_name: &str, prefix: &str) -> PreparedStep {
        PreparedStep {
            action: PreparedAction::Http(Box::new(MergedRequest {
                id: String::new(),
                name: "s".into(),
                method: "GET".into(),
                url: "http://x/".into(),
                params: vec![],
                headers: vec![],
                auth: Auth::CommandToken {
                    command: "get-token".into(),
                    header_name: header_name.into(),
                    prefix: prefix.into(),
                },
                body: Body::None,
                settings: RequestSettings::default(),
                pre_scripts: vec![],
                post_scripts: vec![],
            })),
            tag: "t".into(),
            think_time_ms: None,
            capture: vec![],
            auth_token: None,
            parallel: false,
        }
    }

    #[test]
    fn steps_sharing_a_command_keep_their_own_header() {
        // Both steps used to send the first step's header and prefix.
        let mut p = plan_with(vec![(10, 1.0)]);
        p.kind = PlanKind::Native {
            steps: vec![
                command_step("Authorization", "Bearer "),
                command_step("X-Api-Key", ""),
                command_step("Authorization", "Bearer "),
            ],
        };
        let initial = Token {
            value: "tok".into(),
            epoch: 1,
        };
        p.install_auth_token("get-token", Arc::new(TokenCache::new()), initial);

        let PlanKind::Native { steps } = &p.kind else {
            unreachable!()
        };
        let sent: Vec<(String, String)> = steps
            .iter()
            .map(|s| {
                let state = s.auth_token.as_deref().unwrap();
                (state.header_name.clone(), state.header_value().0)
            })
            .collect();
        assert_eq!(
            sent,
            vec![
                ("Authorization".to_string(), "Bearer tok".to_string()),
                ("X-Api-Key".to_string(), "tok".to_string()),
                ("Authorization".to_string(), "Bearer tok".to_string()),
            ]
        );
        // Still one token between them, so one refresh reaches every step.
        let first = steps[0].auth_token.as_deref().unwrap();
        assert!(steps
            .iter()
            .all(|s| s.auth_token.as_deref().unwrap().shares_token_with(first)));
    }

    #[test]
    fn host_extraction() {
        assert_eq!(
            host_of("https://api.example.com/x").unwrap(),
            "api.example.com"
        );
        assert_eq!(host_of("localhost:3000/x").unwrap(), "localhost");
        assert!(host_of("{{unresolved}}/x").is_none());
        assert!(host_of("https://{{tenant}}.example.com/x").is_none());
        assert!(host_of("{{scheme}}://api.example.com/x").is_none());
    }

    #[test]
    fn a_variable_in_the_path_does_not_hide_the_host() {
        // Dropping these would let the host skip the approval dialog.
        assert_eq!(
            host_of("https://api.example.com/orders/{{orderId}}").as_deref(),
            Some("api.example.com")
        );
        assert_eq!(
            host_of("api.example.com?id={{id}}").as_deref(),
            Some("api.example.com")
        );
        assert_eq!(
            host_of("grpc.example.com:443").as_deref(),
            Some("grpc.example.com")
        );
    }

    #[test]
    fn urls_pulled_from_script_source() {
        let src = r#"
            const a = ctx.http.get("https://api.example.com/one");
            const b = ctx.http.post('{{baseUrl}}/two');
        "#;
        let urls = extract_urls(src);
        assert!(urls.iter().any(|u| u.contains("api.example.com")));
        assert!(urls.iter().any(|u| u.starts_with("{{baseUrl}}")));
    }
}
