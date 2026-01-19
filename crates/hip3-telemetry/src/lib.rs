//! Prometheus metrics and structured logging for HIP-3.
//!
//! Provides observability from Day 1:
//! - Prometheus metrics for trading signals, latency, risk gates
//! - Structured JSON logging with tracing
//! - Health check endpoints
//! - Daily statistics output (P0-31)

pub mod daily_stats;
pub mod error;
pub mod logging;
pub mod metrics;

pub use daily_stats::{DailyStatsReporter, MarketDailyStats};
pub use error::{TelemetryError, TelemetryResult};
pub use logging::init_logging;
pub use metrics::Metrics;
