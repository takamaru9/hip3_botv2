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

pub mod error;
pub mod gates;

pub use error::{RiskError, RiskResult};
pub use gates::{GateResult, RiskGate, RiskGateConfig};
