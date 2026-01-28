//! Position management for HIP-3 (Phase B).
//!
//! Tracks open positions, calculates PnL, manages position limits.
//! Includes TimeStop (time-based exit), MarkRegression (profit-taking), and Flattener (position closing).
//!
//! # Key Components
//!
//! - [`Position`]: Represents an open position
//! - [`PositionTrackerHandle`]: Handle for interacting with position tracker
//! - [`TimeStop`]: Monitors position holding time for timeout detection
//! - [`TimeStopConfig`]: Configuration for time-based exit parameters
//! - [`TimeStopManager`]: Batch checking of multiple positions (legacy)
//! - [`FlattenOrderBuilder`]: Creates reduce-only orders to close positions
//! - [`Flattener`]: Manages the flatten (close) process state machine
//! - [`FlattenRequest`]: Request to close a position
//! - [`FlattenReason`]: Why a position is being flattened
//! - [`PriceProvider`]: Trait for providing current market prices
//! - [`TimeStopMonitor`]: Background task for monitoring and triggering time-based flattens
//! - [`MarkRegressionMonitor`]: Background task for profit-taking when BBO returns to Oracle

pub mod error;
pub mod flatten;
pub mod mark_regression;
pub mod time_stop;
pub mod tracker;

pub use error::{PositionError, PositionResult};
pub use flatten::{
    flatten_all_positions, FlattenReason, FlattenRequest, FlattenState, Flattener,
    REDUCE_ONLY_TIMEOUT_MS,
};
pub use mark_regression::{MarkRegressionConfig, MarkRegressionMonitor};
pub use time_stop::{
    FlattenOrderBuilder, PriceProvider, TimeStop, TimeStopConfig, TimeStopManager, TimeStopMonitor,
    TIME_STOP_MS,
};
pub use tracker::{
    spawn_position_tracker, Position, PositionTrackerHandle, PositionTrackerMsg,
    PositionTrackerTask,
};
