//! Error types for hip3-core.

use thiserror::Error;

/// Core error types.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("Invalid price: {0}")]
    InvalidPrice(String),

    #[error("Invalid size: {0}")]
    InvalidSize(String),

    #[error("Invalid market key: {0}")]
    InvalidMarketKey(String),

    #[error("Decimal parse error: {0}")]
    DecimalParse(#[from] rust_decimal::Error),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
}

/// Result type alias for core operations.
pub type Result<T> = std::result::Result<T, CoreError>;
