//! Dislocation detection for HIP-3 oracle/mark strategy.
//!
//! Detects when best bid/ask crosses oracle price with sufficient edge
//! to cover fees and slippage.
//!
//! Implements P0-24: HIP-3 2x fee calculation with audit trail.
//! Implements P0-31: Cross duration tracking for Phase A DoD.

pub mod config;
pub mod cross_tracker;
pub mod detector;
pub mod error;
pub mod fee;
pub mod signal;

pub use config::DetectorConfig;
pub use cross_tracker::CrossDurationTracker;
pub use detector::DislocationDetector;
pub use error::{DetectorError, DetectorResult};
pub use fee::{FeeCalculator, FeeMetadata, UserFees, HIP3_FEE_MULTIPLIER};
pub use signal::{DislocationSignal, SignalStrength};
