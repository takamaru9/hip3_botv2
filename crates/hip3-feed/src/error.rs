//! Feed error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum FeedError {
    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Invalid data: {0}")]
    InvalidData(String),

    #[error("Market not found: {0}")]
    MarketNotFound(String),

    #[error("Data stale: {0}")]
    DataStale(String),

    #[error("Spot market rejected (P0-30): {0}")]
    SpotRejected(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type FeedResult<T> = Result<T, FeedError>;
