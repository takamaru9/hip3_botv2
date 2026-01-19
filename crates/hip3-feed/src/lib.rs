//! Market data feed aggregation for HIP-3.
//!
//! Aggregates BBO, AssetCtx, and order book data from WebSocket feeds
//! into a unified MarketState per market.

pub mod error;
pub mod market_state;
pub mod parser;

pub use error::{FeedError, FeedResult};
pub use market_state::MarketState;
pub use parser::{MarketEvent, MessageParser};
