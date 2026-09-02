//! 服务层错误类型。

use aion_adapter::AdapterError;

/// AION 服务层错误。
#[derive(Debug, thiserror::Error)]
pub enum AionError {
    #[error("permission denied: requires capability `{0}`")]
    PermissionDenied(String),
    #[error("path `{0}` is outside allowed roots")]
    PathDenied(std::path::PathBuf),
    #[error("network target `{0}` is not allowed")]
    NetDenied(String),
    #[error("resource limit exceeded: {0}")]
    Limit(String),
    #[error("adapter error: {0}")]
    Adapter(String),
    #[error("service `{0}` unavailable: {1}")]
    Unavailable(String, String),
    #[error("model error: {0}")]
    Model(String),
    #[error("{0}")]
    Other(String),
}

pub type AionResult<T> = Result<T, AionError>;

impl From<AdapterError> for AionError {
    fn from(e: AdapterError) -> Self {
        AionError::Adapter(e.to_string())
    }
}

impl From<cordis::CordisError> for AionError {
    fn from(e: cordis::CordisError) -> Self {
        AionError::Other(e.to_string())
    }
}

impl From<std::io::Error> for AionError {
    fn from(e: std::io::Error) -> Self {
        AionError::Other(e.to_string())
    }
}

impl From<serde_json::Error> for AionError {
    fn from(e: serde_json::Error) -> Self {
        AionError::Other(format!("json: {e}"))
    }
}
