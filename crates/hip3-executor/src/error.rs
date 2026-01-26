//! Executor error types.

use hip3_core::MarketKey;
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

    #[error("MarketSpec not found for market: {0}")]
    MarketSpecNotFound(MarketKey),
}

pub type ExecutorResult<T> = Result<T, ExecutorError>;
