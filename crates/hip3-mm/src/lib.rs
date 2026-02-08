//! Market making strategy for HIP-3 bot.
//!
//! Provides weekend market making capabilities:
//! - Quote calculation with inventory skew
//! - Quote lifecycle management (place/cancel/replace)
//! - Inventory tracking with PnL calculation
//!
//! # Architecture
//!
//! ```text
//! Oracle update → QuoteManager.on_market_update()
//!                  ├─ QuoteEngine: compute bid/ask prices
//!                  ├─ InventoryManager: get skew ratio
//!                  └─ MakerAction: place/cancel/replace orders
//!                       ↓
//!                  Executor.on_mm_quote() (bypasses taker gates)
//! ```

pub mod config;
pub mod inventory;
pub mod quote_engine;
pub mod quote_manager;
pub mod volatility;

pub use config::MakerConfig;
pub use inventory::InventoryManager;
pub use quote_engine::{compute_quotes, QuoteLevel, QuotePair};
pub use quote_manager::{ActiveQuote, MakerAction, QuoteManager};
pub use volatility::{VolatilityStats, WickTracker};
