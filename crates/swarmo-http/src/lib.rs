//! Single-shot HTTP execution for the API client UI.
//!
//! The load engine (`swarmo-load`) has its own hot path; this crate optimizes
//! for rich diagnostics (timings, body previews, cookies) rather than throughput.

pub mod client;
pub mod exec;

pub use client::{ClientPool, ClientSettings};
pub use exec::{
    build_request, execute, normalize_url, BodyPreview, CookieRecord, ExecError, ExecOpts,
    ExecResult, ResponseHeader, Timings,
};

/// Bodies larger than this are streamed to a temp file instead of held in memory.
pub const MAX_INLINE_BODY: usize = 20 * 1024 * 1024;

/// JSON bodies larger than this are not pretty-printed.
pub const MAX_PRETTY_BODY: usize = 5 * 1024 * 1024;
