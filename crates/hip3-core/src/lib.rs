//! Core domain types for HIP-3 trading bot.
//!
//! This crate provides fundamental types used throughout the trading system:
//! - `MarketKey`: Unique identifier for HIP-3 markets (DEX + Asset)
//! - `Price`, `Size`: Precision-safe numeric types
//! - `MarketSpec`: Market specifications (tick size, lot size, fees)
//! - `Side`, `OrderType`: Trading enums

pub mod decimal;
pub mod error;
pub mod execution;
pub mod market;
pub mod order;
pub mod types;

pub use decimal::{Price, Size};
pub use error::{CoreError, Result};
pub use market::{AssetId, DexId, MarketKey, MarketSpec, HIP3_MAX_SIG_FIGS};
pub use order::{ClientOrderId, ExitProfile, OrderSide, OrderType, TimeInForce};
pub use types::{AssetCtx, Bbo, BboState, MarketSnapshot, OracleData};

// Execution types
pub use execution::{
    ActionBatch, EnqueueResult, ExecutionResult, OrderState, PendingCancel, PendingOrder,
    RejectReason, SkipReason, TrackedOrder,
};
