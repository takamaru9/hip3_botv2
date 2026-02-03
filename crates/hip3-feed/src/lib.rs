//! Market data feed aggregation for HIP-3.
//!
//! Aggregates BBO, AssetCtx, and order book data from WebSocket feeds
//! into a unified MarketState per market.
//!
//! # Key Components
//!
//! - [`MarketState`]: Aggregates BBO and AssetCtx per market
//! - [`MessageParser`]: Parses WebSocket messages into market events
//! - [`OracleMovementTracker`]: Tracks consecutive oracle price movements

pub mod error;
pub mod market_state;
pub mod oracle_tracker;
pub mod parser;

pub use error::{FeedError, FeedResult};
pub use market_state::MarketState;
pub use oracle_tracker::{
    MoveDirection, OracleMovementTracker, OracleTrackerConfig, OracleTrackerHandle,
};
pub use parser::{MarketEvent, MessageParser};
