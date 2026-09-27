use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("{0}")]
    Invalid(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("already exists: {0}")]
    Exists(String),
}

impl CoreError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        CoreError::Invalid(msg.into())
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        CoreError::NotFound(msg.into())
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;

pub(crate) fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> CoreError {
    let path = path.into();
    move |source| CoreError::Io { path, source }
}
