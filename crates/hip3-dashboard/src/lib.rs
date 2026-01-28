//! hip3-dashboard - Real-time monitoring dashboard for hip3-bot.
//!
//! This crate provides a web-based dashboard for monitoring the trading bot's
//! state in real-time. It includes:
//!
//! - REST API for fetching current state
//! - WebSocket for real-time updates (100ms interval)
//! - Static HTML dashboard UI
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                        hip3-bot process                          │
//! │                                                                 │
//! │  ┌────────────────┐  ┌───────────────┐  ┌─────────────────┐    │
//! │  │  MarketState   │  │PositionTracker│  │  HardStopLatch  │    │
//! │  │  (Arc<>)       │  │   Handle      │  │    (Arc<>)      │    │
//! │  └───────┬────────┘  └───────┬───────┘  └────────┬────────┘    │
//! │          │                   │                    │             │
//! │          └───────────────────┼────────────────────┘             │
//! │                              ▼                                  │
//! │  ┌───────────────────────────────────────────────────────────┐  │
//! │  │              DashboardState (aggregates data)              │  │
//! │  └──────────────────────────┬────────────────────────────────┘  │
//! │                             │                                   │
//! │  ┌──────────────────────────┼────────────────────────────────┐  │
//! │  │       axum HTTP Server (port 8080)                        │  │
//! │  │  GET /          → Static HTML/JS                          │  │
//! │  │  GET /api/snapshot → JSON state                           │  │
//! │  │  GET /ws        → WebSocket upgrade                       │  │
//! │  └───────────────────────────────────────────────────────────┘  │
//! └───────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use hip3_dashboard::{DashboardConfig, DashboardState, run_server};
//!
//! // Create dashboard state from bot components
//! let dashboard_state = DashboardState::new(
//!     market_state.clone(),
//!     position_tracker.clone(),
//!     hard_stop_latch.clone(),
//!     recent_signals.clone(),
//! );
//!
//! // Run server in a separate task
//! let config = DashboardConfig::default();
//! tokio::spawn(async move {
//!     if let Err(e) = run_server(dashboard_state, config).await {
//!         tracing::error!(error = %e, "Dashboard server failed");
//!     }
//! });
//! ```

mod broadcast;
mod config;
mod server;
mod state;
mod types;

pub use config::DashboardConfig;
pub use server::run_server;
pub use state::{DashboardState, SignalSender};
pub use types::{
    DashboardMessage, DashboardSnapshot, MarketDataSnapshot, PositionSnapshot, RiskAlertType,
    RiskStatus, SignalSnapshot,
};
