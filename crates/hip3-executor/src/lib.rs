//! IOC order execution for HIP-3 (Phase B).
//!
//! Handles order submission with idempotency guarantees via client order IDs.
//! Phase B implementation - not active in Phase A observation mode.
//!
//! # Key Components
//!
//! - [`Executor`]: Core executor for trading signal processing with gate checks
//! - [`ExecutorLoop`]: 100ms tick loop for batch processing
//! - [`TradingReadyChecker`]: READY-TRADING condition manager
//! - [`BatchScheduler`]: Three-tier priority queue for orders and cancels
//! - [`Signer`]: Request signing for exchange authentication
//! - [`ActionBudget`]: Rate limiting for new order submissions
//! - [`PostIdGenerator`]: Unique post_id generation for WS correlation
//! - [`HardStopLatch`]: Circuit breaker for emergency trading halt
//! - [`RiskMonitor`]: Real-time risk monitoring and threshold checking
//!
//! # Gate Checks (in `Executor::on_signal`)
//!
//! 1. HardStop -> Rejected::HardStop
//! 2. READY-TRADING -> Rejected::NotReady
//! 3. MaxPositionPerMarket -> Rejected::MaxPositionPerMarket
//! 4. MaxPositionTotal -> Rejected::MaxPositionTotal
//! 5. has_position -> Skipped::AlreadyHasPosition
//! 6. PendingOrder -> Skipped::PendingOrderExists
//! 7. ActionBudget -> Skipped::BudgetExhausted
//! 8. (all passed) -> try_mark_pending_market + enqueue

pub mod batch;
pub mod error;
pub mod executor;
pub mod executor_loop;
pub mod nonce;
pub mod price_provider;
pub mod ready;
pub mod real_ws_sender;
pub mod risk;
pub mod signer;
pub mod ws_sender;

// Batch scheduling
pub use batch::{BatchConfig, BatchScheduler, InflightTracker};

// Risk management
pub use risk::{ExecutionEvent, ExecutorHandle, HardStopLatch, RiskMonitor, RiskMonitorConfig};

// Error types
pub use error::{ExecutorError, ExecutorResult};

// Executor and related types
pub use executor::{
    ActionBudget, Executor, ExecutorConfig, MarketState, MarketStateCache, MmQuoteResult,
    PostIdGenerator,
};

// Price provider for TimeStopMonitor
pub use price_provider::MarkPriceProvider;

// Executor loop
pub use executor_loop::{
    DroppedOrder, ExecutorLoop, PendingRequest, PostRequestManager, PostResult,
};

// Nonce management
pub use nonce::{Clock, NonceError, NonceManager, SystemClock};

// Ready checker
pub use ready::TradingReadyChecker;

// Signing
pub use signer::{
    Action, BuilderInfo, CancelWire, KeyError, KeyManager, KeySource, LimitOrderType,
    OrderTypeWire, OrderWire, PhantomAgent, Signer, SignerError, SigningInput, TriggerOrderType,
};

// WebSocket sender
pub use real_ws_sender::RealWsSender;
pub use ws_sender::{
    ActionSignature, DynWsSender, MockWsSender, SendResult, SignedAction, SignedActionBuilder,
    WsSender,
};
