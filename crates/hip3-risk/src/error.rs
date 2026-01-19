//! Risk error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RiskError {
    #[error("Risk gate blocked: {gate} - {reason}")]
    GateBlocked { gate: String, reason: String },

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Data unavailable: {0}")]
    DataUnavailable(String),
}

pub type RiskResult<T> = Result<T, RiskError>;
