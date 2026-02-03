//! Position management for HIP-3 (Phase B).
//!
//! Tracks open positions, calculates PnL, manages position limits.
//! Includes TimeStop (time-based exit), MarkRegression (profit-taking), ExitWatcher (WS-driven exit),
//! OracleExitWatcher (oracle-driven exit), and Flattener (position closing).
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
//! - [`MarkRegressionMonitor`]: Background task for profit-taking when BBO returns to Oracle (polling)
//! - [`ExitWatcher`]: WS-driven exit for immediate mark regression detection (< 1ms latency)
//! - [`OracleExitWatcher`]: Oracle-driven exit based on consecutive price movements

pub mod error;
pub mod exit_watcher;
pub mod flatten;
pub mod mark_regression;
pub mod oracle_exit;
pub mod time_stop;
pub mod tracker;

pub use error::{PositionError, PositionResult};
pub use exit_watcher::{new_exit_watcher, ExitWatcher, ExitWatcherHandle};
pub use flatten::{
    flatten_all_positions, FlattenReason, FlattenRequest, FlattenState, Flattener,
    REDUCE_ONLY_TIMEOUT_MS,
};
pub use mark_regression::{MarkRegressionConfig, MarkRegressionMonitor};
pub use oracle_exit::{
    new_oracle_exit_watcher, OracleExitConfig, OracleExitMetrics, OracleExitReason,
    OracleExitWatcher, OracleExitWatcherHandle,
};
pub use time_stop::{
    FlattenOrderBuilder, PriceProvider, TimeStop, TimeStopConfig, TimeStopManager, TimeStopMonitor,
    TIME_STOP_MS,
};
pub use tracker::{
    spawn_position_tracker, Position, PositionTrackerHandle, PositionTrackerMsg,
    PositionTrackerTask,
};
