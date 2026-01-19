//! Telemetry error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TelemetryError {
    #[error("Logging initialization failed: {0}")]
    LoggingInit(String),

    #[error("Metrics error: {0}")]
    Metrics(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type TelemetryResult<T> = Result<T, TelemetryError>;
