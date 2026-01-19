//! Executor error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("Order submission failed: {0}")]
    SubmissionFailed(String),

    #[error("Order rejected: {0}")]
    OrderRejected(String),

    #[error("Connection error: {0}")]
    ConnectionError(String),

    #[error("Rate limited")]
    RateLimited,
}

pub type ExecutorResult<T> = Result<T, ExecutorError>;
