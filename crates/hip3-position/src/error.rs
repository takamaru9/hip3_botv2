//! Position error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PositionError {
    #[error("Position not found: {0}")]
    NotFound(String),

    #[error("Position limit exceeded: {0}")]
    LimitExceeded(String),

    #[error("Invalid position state: {0}")]
    InvalidState(String),
}

pub type PositionResult<T> = Result<T, PositionError>;
