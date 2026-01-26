//! Application error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("WebSocket error: {0}")]
    WebSocket(#[from] Box<hip3_ws::WsError>),

    #[error("Feed error: {0}")]
    Feed(#[from] hip3_feed::FeedError),

    #[error("Registry error: {0}")]
    Registry(#[from] hip3_registry::RegistryError),

    #[error("Risk error: {0}")]
    Risk(#[from] hip3_risk::RiskError),

    #[error("Detector error: {0}")]
    Detector(#[from] hip3_detector::DetectorError),

    #[error("Telemetry error: {0}")]
    Telemetry(#[from] hip3_telemetry::TelemetryError),

    #[error("Persistence error: {0}")]
    Persistence(#[from] hip3_persistence::PersistenceError),

    #[error("Preflight error: {0}")]
    Preflight(String),

    #[error("Executor error: {0}")]
    Executor(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Shutdown requested")]
    Shutdown,
}

pub type AppResult<T> = Result<T, AppError>;
