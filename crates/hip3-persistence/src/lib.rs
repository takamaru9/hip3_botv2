//! Data persistence (Parquet) for HIP-3.
//!
//! Records trading signals, market data, and events to Parquet files
//! for post-analysis in Python/Polars.

pub mod error;
pub mod writer;

pub use error::{PersistenceError, PersistenceResult};
pub use writer::{ParquetWriter, SignalRecord};
