//! Errors that stop a call from happening.
//!
//! A non-OK gRPC *status* is never an error here: it is data, exactly as an
//! HTTP 500 is a successful `ExecResult` on the HTTP side. Only failures that
//! prevent a call from being made or decoded appear as `GrpcError`.

#[derive(Debug, thiserror::Error)]
pub enum GrpcError {
    /// A `.proto` failed to compile, or reflection could not supply a schema.
    #[error("{0}")]
    Descriptor(String),

    #[error(
        "service \"{wanted}\" was not found. Available services: {}{}",
        if available.is_empty() { "(none)".to_string() } else { available.join(", ") },
        if notes.is_empty() { String::new() } else { format!("\n{}", notes.join("\n")) }
    )]
    ServiceNotFound {
        wanted: String,
        available: Vec<String>,
        /// Why a service might be absent — for example its schema was
        /// incomplete and it had to be skipped.
        notes: Vec<String>,
    },

    #[error("method \"{method}\" was not found on {service}. Available methods: {}", available.join(", "))]
    MethodNotFound {
        service: String,
        method: String,
        available: Vec<String>,
    },

    /// The selected method streams; Swarmo supports unary calls only.
    #[error(
        "{service}/{method} is a streaming method, and Swarmo supports unary calls only for now"
    )]
    Streaming { service: String, method: String },

    /// The request message JSON did not fit the schema.
    #[error("the request message does not match {message_type}: {detail}")]
    BadMessage {
        message_type: String,
        detail: String,
    },

    #[error("could not decode the response: {0}")]
    BadResponse(String),

    #[error("{0}")]
    Transport(String),

    #[error("the call timed out after {0}ms")]
    Timeout(u64),

    #[error("{0}")]
    Invalid(String),
}

impl GrpcError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        GrpcError::Invalid(msg.into())
    }
    pub fn descriptor(msg: impl Into<String>) -> Self {
        GrpcError::Descriptor(msg.into())
    }

    /// A short category, mirroring `ExecError::kind` on the HTTP side.
    pub fn kind(&self) -> &'static str {
        match self {
            GrpcError::Descriptor(_) => "descriptor",
            GrpcError::ServiceNotFound { .. } | GrpcError::MethodNotFound { .. } => "not-found",
            GrpcError::Streaming { .. } => "unsupported",
            GrpcError::BadMessage { .. } => "invalid-message",
            GrpcError::BadResponse(_) => "invalid-response",
            GrpcError::Transport(_) => "transport",
            GrpcError::Timeout(_) => "timeout",
            GrpcError::Invalid(_) => "invalid",
        }
    }

    /// True when refreshing a cached schema might fix this.
    pub fn is_schema_stale(&self) -> bool {
        matches!(
            self,
            GrpcError::ServiceNotFound { .. } | GrpcError::MethodNotFound { .. }
        )
    }
}

pub type Result<T> = std::result::Result<T, GrpcError>;
