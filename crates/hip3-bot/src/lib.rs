//! HIP-3 Oracle/Mark Dislocation Taker Bot.
//!
//! Main application that orchestrates all components:
//! - WebSocket connection to exchange
//! - Market data feed aggregation
//! - Risk gate checks
//! - Dislocation detection
//! - Signal recording (Phase A) / Execution (Phase B)

pub mod app;
pub mod config;
pub mod edge_tracker;
pub mod error;

pub use app::Application;
pub use config::AppConfig;
pub use error::{AppError, AppResult};
