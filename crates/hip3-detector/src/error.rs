//! Detector error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DetectorError {
    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Data unavailable: {0}")]
    DataUnavailable(String),

    #[error("Invalid state: {0}")]
    InvalidState(String),
}

pub type DetectorResult<T> = Result<T, DetectorError>;
