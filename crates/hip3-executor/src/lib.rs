//! IOC order execution for HIP-3 (Phase B).
//!
//! Handles order submission with idempotency guarantees via client order IDs.
//! Phase B implementation - not active in Phase A observation mode.

pub mod error;

pub use error::{ExecutorError, ExecutorResult};

// Phase B: Executor implementation will go here
