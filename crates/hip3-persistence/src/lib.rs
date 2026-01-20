//! Data persistence (JSON Lines) for HIP-3.
//!
//! Records trading signals, market data, and events to JSON Lines files
//! for post-analysis in Python/Pandas/Polars.
//!
//! JSON Lines format is more robust than Parquet for streaming writes:
//! - Each line is a complete JSON object
//! - Partial file corruption only affects individual lines
//! - Can be read even if write was interrupted

pub mod error;
pub mod writer;

pub use error::{PersistenceError, PersistenceResult};
pub use writer::{JsonLinesWriter, ParquetWriter, SignalRecord};
