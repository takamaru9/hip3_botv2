//! Execution-related types for order lifecycle management.
//!
//! This module provides types for:
//! - Pending order/cancel queues
//! - Order lifecycle tracking
//! - Action batching for SDK compliance
//! - Execution results and error handling

use serde::{Deserialize, Serialize};

use crate::market::MarketKey;
use crate::order::{ClientOrderId, OrderSide, TimeInForce};
use crate::{Price, Size};

// ============================================================================
// Pending Order Types
// ============================================================================

/// Pending order waiting to be submitted to the exchange.
///
/// Used in the new_order queue before the order is posted via SDK.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOrder {
    /// Client order ID for idempotency.
    pub cloid: ClientOrderId,
    /// Target market.
    pub market: MarketKey,
    /// Order side (buy/sell).
    pub side: OrderSide,
    /// Limit price.
    pub price: Price,
    /// Order size.
    pub size: Size,
    /// Whether this is a reduce-only order.
    pub reduce_only: bool,
    /// Creation timestamp (Unix milliseconds).
    pub created_at: u64,
    /// Time-in-force. Defaults to IOC for backward compatibility with taker strategy.
    #[serde(default)]
    pub tif: TimeInForce,
}

impl PendingOrder {
    /// Create a new pending order with default TIF (IOC).
    #[must_use]
    pub fn new(
        cloid: ClientOrderId,
        market: MarketKey,
        side: OrderSide,
        price: Price,
        size: Size,
        reduce_only: bool,
        created_at: u64,
    ) -> Self {
        Self {
            cloid,
            market,
            side,
            price,
            size,
            reduce_only,
            created_at,
            tif: TimeInForce::default(), // IOC
        }
    }

    /// Create a new pending order with explicit TIF.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_tif(
        cloid: ClientOrderId,
        market: MarketKey,
        side: OrderSide,
        price: Price,
        size: Size,
        reduce_only: bool,
        created_at: u64,
        tif: TimeInForce,
    ) -> Self {
        Self {
            cloid,
            market,
            side,
            price,
            size,
            reduce_only,
            created_at,
            tif,
        }
    }
}

/// Pending cancel request waiting to be submitted to the exchange.
///
/// Used in the cancel queue before the cancel is posted via SDK.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCancel {
    /// Target market.
    pub market: MarketKey,
    /// Exchange order ID to cancel.
    pub oid: u64,
    /// Creation timestamp (Unix milliseconds).
    pub created_at: u64,
}

impl PendingCancel {
    /// Create a new pending cancel request.
    #[must_use]
    pub fn new(market: MarketKey, oid: u64, created_at: u64) -> Self {
        Self {
            market,
            oid,
            created_at,
        }
    }
}

// ============================================================================
// Order Tracking Types
// ============================================================================

/// State of an order in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum OrderState {
    /// Order enqueued but not yet posted to exchange.
    #[default]
    Pending,
    /// Order successfully posted, confirmed open by orderUpdates.
    Open,
    /// Order partially filled.
    PartialFilled,
    /// Order completely filled.
    Filled,
    /// Order cancelled.
    Cancelled,
    /// Order rejected by exchange.
    Rejected,
}

impl OrderState {
    /// Returns true if the order is in a terminal state.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Filled | Self::Cancelled | Self::Rejected)
    }

    /// Returns true if the order is still active (can be cancelled).
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Pending | Self::Open | Self::PartialFilled)
    }
}

/// Tracked order for lifecycle management.
///
/// Represents an order from creation through completion, tracking
/// all state transitions and fill updates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackedOrder {
    /// Client order ID for idempotency.
    pub cloid: ClientOrderId,
    /// Target market.
    pub market: MarketKey,
    /// Order side (buy/sell).
    pub side: OrderSide,
    /// Limit price.
    pub price: Price,
    /// Original order size.
    pub size: Size,
    /// Amount filled so far.
    pub filled_size: Size,
    /// Whether this is a reduce-only order.
    pub reduce_only: bool,
    /// Current order state.
    pub state: OrderState,
    /// Creation timestamp (Unix milliseconds).
    pub created_at: u64,
    /// Last update timestamp (Unix milliseconds).
    pub updated_at: u64,
    /// Time-in-force.
    #[serde(default)]
    pub tif: TimeInForce,
}

impl TrackedOrder {
    /// Create a tracked order from a pending order.
    ///
    /// Initializes with `Pending` state and zero filled size.
    #[must_use]
    pub fn from_pending(pending: PendingOrder) -> Self {
        Self {
            cloid: pending.cloid,
            market: pending.market,
            side: pending.side,
            price: pending.price,
            size: pending.size,
            filled_size: Size::ZERO,
            reduce_only: pending.reduce_only,
            state: OrderState::Pending,
            created_at: pending.created_at,
            updated_at: pending.created_at,
            tif: pending.tif,
        }
    }

    /// Returns the remaining unfilled size.
    #[must_use]
    pub fn remaining_size(&self) -> Size {
        self.size - self.filled_size
    }

    /// Returns true if the order is completely filled.
    #[must_use]
    pub fn is_filled(&self) -> bool {
        self.state == OrderState::Filled || self.filled_size >= self.size
    }
}

// ============================================================================
// Action Batching
// ============================================================================

/// Batch of actions to submit to the exchange.
///
/// SDK specification requires one action type per tick, so we separate
/// orders and cancels into distinct batches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionBatch {
    /// Batch of new orders to submit.
    Orders(Vec<PendingOrder>),
    /// Batch of cancel requests to submit.
    Cancels(Vec<PendingCancel>),
}

impl ActionBatch {
    /// Returns true if the batch is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Orders(orders) => orders.is_empty(),
            Self::Cancels(cancels) => cancels.is_empty(),
        }
    }

    /// Returns the number of items in the batch.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Orders(orders) => orders.len(),
            Self::Cancels(cancels) => cancels.len(),
        }
    }
}

// ============================================================================
// Result Types
// ============================================================================

/// Result of attempting to enqueue an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EnqueueResult {
    /// Order successfully queued.
    Queued,
    /// Order queued but system is in degraded mode.
    QueuedDegraded,
    /// Queue is full, order rejected.
    QueueFull,
    /// Too many in-flight orders, order rejected.
    InflightFull,
}

impl EnqueueResult {
    /// Returns true if the order was successfully queued.
    #[must_use]
    pub fn is_queued(&self) -> bool {
        matches!(self, Self::Queued | Self::QueuedDegraded)
    }
}

/// Reason for rejecting an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RejectReason {
    /// System not ready for trading.
    NotReady,
    /// Would exceed maximum position for this market.
    MaxPositionPerMarket,
    /// Would exceed total portfolio position limit.
    MaxPositionTotal,
    /// Would exceed maximum concurrent positions limit.
    MaxConcurrentPositions,
    /// Hard stop triggered (circuit breaker).
    HardStop,
    /// Order queue is full.
    QueueFull,
    /// Too many in-flight orders.
    InflightFull,
    /// Required market data (mark price) is unavailable.
    MarketDataUnavailable,
    /// Hourly drawdown limit exceeded (P2-3).
    MaxDrawdown,
    /// Correlation cooldown active (P2-4).
    CorrelationCooldown,
    /// Correlation-weighted position limit exceeded (P3-3).
    CorrelationPositionLimit,
    /// Burst signal rate limit exceeded (too many signals per market in window).
    BurstSignal,
    /// Tilt guard: consecutive loss cooldown active.
    TiltGuard,
    /// Same-market re-entry delay active.
    ReEntryDelay,
}

/// Reason for skipping signal processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SkipReason {
    /// Already have a position in this market.
    AlreadyHasPosition,
    /// Already have a pending order for this market.
    PendingOrderExists,
    /// Risk budget exhausted.
    BudgetExhausted,
    /// Market is currently being flattened (reduce-only order pending).
    FlattenInProgress,
}

/// Result of processing a trading signal via `on_signal()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionResult {
    /// Order successfully queued for execution.
    Queued {
        /// Client order ID of the queued order.
        cloid: ClientOrderId,
    },
    /// Order queued but system is in degraded mode (high inflight count).
    QueuedDegraded {
        /// Client order ID of the queued order.
        cloid: ClientOrderId,
    },
    /// Order rejected by risk checks or system constraints.
    Rejected {
        /// Reason for rejection.
        reason: RejectReason,
    },
    /// Signal intentionally skipped (not an error).
    Skipped {
        /// Reason for skipping.
        reason: SkipReason,
    },
}

impl ExecutionResult {
    /// Create a queued result with the given client order ID.
    #[must_use]
    pub fn queued(cloid: ClientOrderId) -> Self {
        Self::Queued { cloid }
    }

    /// Create a queued-degraded result with the given client order ID.
    #[must_use]
    pub fn queued_degraded(cloid: ClientOrderId) -> Self {
        Self::QueuedDegraded { cloid }
    }

    /// Create a rejected result with the given reason.
    #[must_use]
    pub fn rejected(reason: RejectReason) -> Self {
        Self::Rejected { reason }
    }

    /// Create a skipped result with the given reason.
    #[must_use]
    pub fn skipped(reason: SkipReason) -> Self {
        Self::Skipped { reason }
    }

    /// Returns true if the signal resulted in a queued order (normal or degraded).
    #[must_use]
    pub fn is_queued(&self) -> bool {
        matches!(self, Self::Queued { .. } | Self::QueuedDegraded { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::{AssetId, DexId};
    use rust_decimal_macros::dec;

    fn sample_market() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    #[test]
    fn test_pending_order_creation() {
        let cloid = ClientOrderId::new();
        let order = PendingOrder::new(
            cloid.clone(),
            sample_market(),
            OrderSide::Buy,
            Price::new(dec!(50000)),
            Size::new(dec!(0.1)),
            false,
            1234567890,
        );

        assert_eq!(order.cloid, cloid);
        assert_eq!(order.side, OrderSide::Buy);
        assert!(!order.reduce_only);
    }

    #[test]
    fn test_tracked_order_from_pending() {
        let pending = PendingOrder::new(
            ClientOrderId::new(),
            sample_market(),
            OrderSide::Sell,
            Price::new(dec!(51000)),
            Size::new(dec!(0.5)),
            true,
            1234567890,
        );

        let tracked = TrackedOrder::from_pending(pending.clone());

        assert_eq!(tracked.cloid, pending.cloid);
        assert_eq!(tracked.market, pending.market);
        assert_eq!(tracked.side, pending.side);
        assert_eq!(tracked.price, pending.price);
        assert_eq!(tracked.size, pending.size);
        assert_eq!(tracked.filled_size, Size::ZERO);
        assert_eq!(tracked.state, OrderState::Pending);
        assert!(tracked.reduce_only);
    }

    #[test]
    fn test_order_state_transitions() {
        assert!(OrderState::Pending.is_active());
        assert!(OrderState::Open.is_active());
        assert!(OrderState::PartialFilled.is_active());

        assert!(OrderState::Filled.is_terminal());
        assert!(OrderState::Cancelled.is_terminal());
        assert!(OrderState::Rejected.is_terminal());

        assert!(!OrderState::Pending.is_terminal());
        assert!(!OrderState::Filled.is_active());
    }

    #[test]
    fn test_action_batch_len() {
        let orders = ActionBatch::Orders(vec![]);
        assert!(orders.is_empty());
        assert_eq!(orders.len(), 0);

        let cancels =
            ActionBatch::Cancels(vec![PendingCancel::new(sample_market(), 123, 1234567890)]);
        assert!(!cancels.is_empty());
        assert_eq!(cancels.len(), 1);
    }

    #[test]
    fn test_enqueue_result() {
        assert!(EnqueueResult::Queued.is_queued());
        assert!(EnqueueResult::QueuedDegraded.is_queued());
        assert!(!EnqueueResult::QueueFull.is_queued());
        assert!(!EnqueueResult::InflightFull.is_queued());
    }

    #[test]
    fn test_execution_result_constructors() {
        let cloid = ClientOrderId::new();
        let queued = ExecutionResult::queued(cloid.clone());
        assert!(queued.is_queued());

        let rejected = ExecutionResult::rejected(RejectReason::HardStop);
        assert!(!rejected.is_queued());

        let skipped = ExecutionResult::skipped(SkipReason::AlreadyHasPosition);
        assert!(!skipped.is_queued());

        // Test QueuedDegraded
        let degraded = ExecutionResult::queued_degraded(ClientOrderId::new());
        assert!(degraded.is_queued());
    }
}
