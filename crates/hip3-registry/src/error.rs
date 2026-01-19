//! Registry error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("Market not found: {0}")]
    MarketNotFound(String),

    #[error("Spec parse error: {0}")]
    ParseError(String),

    #[error("Parameter change detected: {0}")]
    ParamChange(String),

    #[error("Preflight validation failed: {0}")]
    PreflightFailed(String),

    #[error("HTTP client error: {0}")]
    HttpClient(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type RegistryResult<T> = Result<T, RegistryError>;
