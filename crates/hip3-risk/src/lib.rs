//! Hard risk gates for HIP-3 trading.
//!
//! Implements critical risk checks that must pass before any trade:
//! - OracleFresh: Oracle data not stale
//! - MarkMidDivergence: Mark-Mid gap within threshold
//! - SpreadShock: Spread not abnormally wide
//! - OiCap: Open interest below limit
//! - ParamChange: No tick/lot/fee changes
//! - Halt: Market not halted
//! - BufferLow: Liquidation buffer adequate
//! - MaxPositionPerMarket: Position per market within limit
//! - MaxPositionTotal: Total portfolio position within limit
//!
//! Also provides:
//! - HardStopLatch: Emergency stop mechanism
//! - RiskMonitor: Execution event monitoring for risk violations

pub mod error;
pub mod gates;
pub mod hard_stop;

pub use error::{RiskError, RiskResult};
pub use gates::{
    GateResult, MaxPositionPerMarketGate, MaxPositionTotalGate, RiskGate, RiskGateConfig,
};
pub use hard_stop::{
    ExecutionEvent, HardStopLatch, HardStopReason, RiskMonitor, RiskMonitorConfig,
};
