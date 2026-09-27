//! The Swarmo load engine.
//!
//! Memory is bounded by the number of distinct metric tags, never by the number
//! of requests: samples are folded into HDR histograms on arrival.

pub mod auth;
pub mod capture;
pub mod engine;
pub mod metrics;
pub mod plan;

pub use engine::{run, RunControl, RunOutput};
pub use metrics::{MetricMsg, MetricsSink, RunCounters, Sample};
pub use plan::{
    LoadPlan, PlanError, PlanKind, PreparedAction, PreparedStep, ScriptedGrpc, MAX_RATE_HARD_CAP,
    MAX_SCRIPTED_VUS, MAX_VUS_HARD_CAP,
};
