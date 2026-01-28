//! Core executor for trading signal processing.
//!
//! Implements the main execution logic including:
//! - Signal processing with gate checks
//! - Risk limit validation
//! - Order queuing via BatchScheduler
//! - HardStop handling
//!
//! # Gate Check Order (Strict)
//!
//! 1. HardStop        → Rejected(HardStop)
//! 2. READY-TRADING   → Rejected(NotReady)
//! 3. MaxPosition     → Rejected(MaxPositionPerMarket / MaxPositionTotal)
//! 4. has_position    → Skipped(AlreadyHasPosition)
//! 5. PendingOrder    → Skipped(PendingOrderExists)
//! 6. ActionBudget    → Skipped(BudgetExhausted)
//! 7. (all passed)    → try_mark_pending_market + enqueue

use std::cell::Cell;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use rust_decimal::Decimal;
use tracing::{debug, trace, warn};

use hip3_core::{
    ClientOrderId, EnqueueResult, ExecutionResult, MarketKey, OrderSide, PendingOrder, Price,
    RejectReason, Size, SkipReason, TrackedOrder,
};
use hip3_position::PositionTrackerHandle;

use crate::batch::BatchScheduler;
use crate::ready::TradingReadyChecker;
use crate::risk::HardStopLatch;

// ============================================================================
// PostIdGenerator
// ============================================================================

/// Generator for unique post_id values.
///
/// post_id is a WS layer correlation ID used to match responses to requests.
/// It is monotonically increasing and never repeats within a session.
#[derive(Debug)]
pub struct PostIdGenerator {
    counter: AtomicU64,
}

impl PostIdGenerator {
    /// Create a new `PostIdGenerator` starting at 1.
    #[must_use]
    pub fn new() -> Self {
        Self {
            counter: AtomicU64::new(1),
        }
    }

    /// Generate the next post_id.
    pub fn next(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::AcqRel)
    }
}

impl Default for PostIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// ActionBudget
// ============================================================================

/// Action budget for rate limiting new order submissions.
///
/// Provides a simple interval-based budget that resets after each interval.
/// The full implementation will be in hip3-risk crate with more sophisticated
/// budget management (per-market, daily limits, etc.).
#[derive(Debug)]
pub struct ActionBudget {
    /// Maximum orders per interval.
    max_orders: u32,
    /// Current order count in interval.
    current_count: AtomicU32,
    /// Interval start timestamp.
    interval_start_ms: AtomicU64,
    /// Interval duration in milliseconds.
    interval_ms: u64,
}

impl ActionBudget {
    /// Create a new `ActionBudget`.
    ///
    /// # Arguments
    /// * `max_orders` - Maximum orders per interval
    /// * `interval_ms` - Interval duration in milliseconds
    #[must_use]
    pub fn new(max_orders: u32, interval_ms: u64) -> Self {
        Self {
            max_orders,
            current_count: AtomicU32::new(0),
            interval_start_ms: AtomicU64::new(0),
            interval_ms,
        }
    }

    /// Check if a new order can be sent within budget.
    ///
    /// Also resets the interval if it has expired.
    pub fn can_send_new_order(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        self.can_send_new_order_at(now)
    }

    /// Check if a new order can be sent within budget at the given timestamp.
    ///
    /// Note: This is a best-effort check. For actual budget consumption,
    /// use `consume_at()` which handles interval reset atomically.
    pub fn can_send_new_order_at(&self, now_ms: u64) -> bool {
        let interval_start = self.interval_start_ms.load(Ordering::Acquire);

        // Check if interval has expired - budget would be available
        if now_ms.saturating_sub(interval_start) > self.interval_ms {
            return true;
        }

        self.current_count.load(Ordering::Acquire) < self.max_orders
    }

    /// Consume one order from the budget.
    ///
    /// # Returns
    /// `true` if order was consumed, `false` if budget exhausted.
    pub fn consume(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        self.consume_at(now)
    }

    /// Consume one order from the budget at the given timestamp.
    ///
    /// This method atomically handles:
    /// 1. Interval expiration check and reset (via CAS)
    /// 2. Budget consumption (via CAS)
    ///
    /// The two operations are performed in a single loop to avoid race conditions
    /// where multiple threads could both reset the interval.
    pub fn consume_at(&self, now_ms: u64) -> bool {
        // Single loop handles both interval reset and consumption atomically
        loop {
            let interval_start = self.interval_start_ms.load(Ordering::Acquire);
            let current = self.current_count.load(Ordering::Acquire);

            // Check if interval has expired
            if now_ms.saturating_sub(interval_start) > self.interval_ms {
                // Try to atomically reset the interval (only one thread wins)
                match self.interval_start_ms.compare_exchange_weak(
                    interval_start,
                    now_ms,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        // We won the race - reset counter and consume one
                        self.current_count.store(1, Ordering::Release);
                        return true;
                    }
                    Err(_) => {
                        // Another thread reset the interval - retry with new values
                        continue;
                    }
                }
            }

            // Interval still valid - try to consume from existing budget
            if current >= self.max_orders {
                return false;
            }

            match self.current_count.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(_) => continue,
            }
        }
    }

    /// Get remaining budget.
    #[must_use]
    pub fn remaining(&self) -> u32 {
        let current = self.current_count.load(Ordering::Acquire);
        self.max_orders.saturating_sub(current)
    }
}

impl Default for ActionBudget {
    fn default() -> Self {
        // Default: 100 orders per second
        Self::new(100, 1000)
    }
}

// ============================================================================
// ExecutorConfig
// ============================================================================

/// Configuration for the executor.
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Maximum notional value per market (USD).
    /// Uses Decimal for precision in financial calculations.
    pub max_notional_per_market: Decimal,
    /// Maximum total notional value across all markets (USD).
    /// Uses Decimal for precision in financial calculations.
    pub max_notional_total: Decimal,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            max_notional_per_market: Decimal::from(50),
            max_notional_total: Decimal::from(100),
        }
    }
}

// ============================================================================
// MarketStateCache
// ============================================================================

/// Cached market state for mark price lookups.
#[derive(Debug, Clone)]
pub struct MarketState {
    /// Mark price.
    pub mark_px: Price,
    /// Last update timestamp (Unix milliseconds).
    pub updated_at: u64,
}

/// Thread-safe cache for market state.
///
/// Used for quick mark price lookups during signal processing.
#[derive(Debug, Default)]
pub struct MarketStateCache {
    states: DashMap<MarketKey, MarketState>,
}

impl MarketStateCache {
    /// Create a new market state cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            states: DashMap::new(),
        }
    }

    /// Update the market state for a market.
    pub fn update(&self, market: &MarketKey, mark_px: Price, now_ms: u64) {
        self.states.insert(
            *market,
            MarketState {
                mark_px,
                updated_at: now_ms,
            },
        );
    }

    /// Get the mark price for a market.
    #[must_use]
    pub fn get_mark_px(&self, market: &MarketKey) -> Option<Price> {
        self.states.get(market).map(|s| s.mark_px)
    }

    /// Get the full market state for a market.
    #[must_use]
    pub fn get(&self, market: &MarketKey) -> Option<MarketState> {
        self.states.get(market).map(|s| s.clone())
    }

    /// Remove a market from the cache.
    pub fn remove(&self, market: &MarketKey) {
        self.states.remove(market);
    }

    /// Clear all cached market states.
    pub fn clear(&self) {
        self.states.clear();
    }

    /// Get the number of cached markets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// Check if the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
}

// ============================================================================
// Executor
// ============================================================================

/// Core executor for trading signal processing.
///
/// Processes trading signals through a series of gates:
///
/// # Gate Check Order (Strict)
///
/// 1. HardStop        → Rejected(HardStop)
/// 2. (READY-TRADING) → Handled by bot via `connection_manager.is_ready()`
/// 3. MaxPositionPerMarket → Rejected(MaxPositionPerMarket)
/// 4. MaxPositionTotal → Rejected(MaxPositionTotal)
/// 5. has_position    → Skipped(AlreadyHasPosition)
/// 6. PendingOrder    → Skipped(PendingOrderExists)
/// 7. ActionBudget    → Skipped(BudgetExhausted)
/// 8. (all passed)    → try_mark_pending_market + enqueue
///
/// Note: Gate 2 (READY-TRADING) is not checked here. The bot is responsible
/// for verifying WebSocket READY-TRADING state before calling `on_signal`.
///
/// Orders that pass all gates are queued via the BatchScheduler.
pub struct Executor {
    /// Position management handle.
    position_tracker: PositionTrackerHandle,
    /// Batch scheduler for order queuing.
    batch_scheduler: Arc<BatchScheduler>,
    /// READY-TRADING checker.
    ready_checker: Arc<TradingReadyChecker>,
    /// HardStop latch for emergency circuit breaker.
    hard_stop_latch: Arc<HardStopLatch>,
    /// Action budget for rate limiting.
    action_budget: Arc<ActionBudget>,
    /// Executor configuration.
    config: ExecutorConfig,
    /// Market state cache for mark price lookups.
    market_state_cache: Arc<MarketStateCache>,
}

impl Executor {
    /// Create a new executor.
    #[must_use]
    pub fn new(
        position_tracker: PositionTrackerHandle,
        batch_scheduler: Arc<BatchScheduler>,
        ready_checker: Arc<TradingReadyChecker>,
        hard_stop_latch: Arc<HardStopLatch>,
        action_budget: Arc<ActionBudget>,
        config: ExecutorConfig,
        market_state_cache: Arc<MarketStateCache>,
    ) -> Self {
        Self {
            position_tracker,
            batch_scheduler,
            ready_checker,
            hard_stop_latch,
            action_budget,
            config,
            market_state_cache,
        }
    }

    /// Process a trading signal.
    ///
    /// Runs through all gate checks and queues the order if all pass.
    ///
    /// # Gate Order (Strict)
    ///
    /// 1. HardStop -> Rejected::HardStop
    /// 2. (READY-TRADING) -> Handled by bot, not checked here
    /// 3. MaxPositionPerMarket -> Rejected::MaxPositionPerMarket
    /// 4. MaxPositionTotal -> Rejected::MaxPositionTotal
    /// 5. has_position -> Skipped::AlreadyHasPosition
    /// 6. PendingOrder -> Skipped::PendingOrderExists
    /// 7. ActionBudget -> Skipped::BudgetExhausted
    /// 8. (all passed) -> try_mark_pending_market + enqueue
    ///
    /// # Precondition
    ///
    /// The caller (bot) must verify `connection_manager.is_ready()` before calling
    /// this method to ensure WebSocket READY-TRADING state.
    ///
    /// # Returns
    ///
    /// - `ExecutionResult::Queued` - Order successfully queued
    /// - `ExecutionResult::QueuedDegraded` - Order queued in degraded mode
    /// - `ExecutionResult::Rejected` - Order rejected by gate check
    /// - `ExecutionResult::Skipped` - Signal intentionally skipped
    pub fn on_signal(
        &self,
        market: &MarketKey,
        side: OrderSide,
        price: Price,
        size: Size,
        now_ms: u64,
    ) -> ExecutionResult {
        // Gate 1: HardStop
        if self.hard_stop_latch.is_triggered() {
            debug!(market = %market, "Signal rejected: HardStop triggered");
            return ExecutionResult::rejected(RejectReason::HardStop);
        }

        // Gate 2: READY-TRADING - Handled by bot via connection_manager.is_ready()
        // TradingReadyChecker's 4 flags are not wired in current implementation.
        // The bot checks WS READY-TRADING (bbo + assetCtx + orderUpdates subscriptions)
        // before calling on_signal, so we skip this check here to avoid duplication.
        // To restore: if !self.ready_checker.is_ready() { return Rejected(NotReady); }

        // Gate 3 (was Gate 3): MaxPositionPerMarket
        // MUST fail closed: if mark_px unavailable, reject order
        let mark_px = match self.market_state_cache.get_mark_px(market) {
            Some(px) => px,
            None => {
                warn!(
                    market = %market,
                    "Gate 3: mark price unavailable, rejecting order"
                );
                return ExecutionResult::rejected(RejectReason::MarketDataUnavailable);
            }
        };

        let position_notional = self.position_tracker.get_notional(market, mark_px);
        let pending_notional = self
            .position_tracker
            .get_pending_notional_excluding_reduce_only(market, mark_px);
        // Use mark_px for new order notional (consistent with position/pending)
        let new_order_notional = size.inner() * mark_px.inner();
        let total_notional =
            position_notional.inner() + pending_notional.inner() + new_order_notional;

        // Compare using Decimal (no f64 conversion)
        if total_notional >= self.config.max_notional_per_market {
            debug!(
                market = %market,
                position_notional = %position_notional,
                pending_notional = %pending_notional,
                new_order_notional = %new_order_notional,
                size = %size,
                mark_px = %mark_px,
                total_notional = %total_notional,
                max = %self.config.max_notional_per_market,
                "Signal rejected: Would exceed max position per market"
            );
            return ExecutionResult::rejected(RejectReason::MaxPositionPerMarket);
        }

        // Gate 4: MaxPositionTotal
        // Includes positions + pending (excluding reduce_only)
        // MUST fail closed: if any mark_px unavailable, reject order
        let total_portfolio_notional = match self.calculate_total_portfolio_notional() {
            Ok(total) => total,
            Err(reason) => return ExecutionResult::rejected(reason),
        };

        // Use mark_px from Gate 3 for new order notional (already validated)
        let new_order_notional = size.inner() * mark_px.inner();
        let projected_total = total_portfolio_notional + new_order_notional;

        // Compare using Decimal (no f64 conversion)
        if projected_total >= self.config.max_notional_total {
            debug!(
                market = %market,
                projected_total = %projected_total,
                max = %self.config.max_notional_total,
                "Signal rejected: Would exceed max total position"
            );
            return ExecutionResult::rejected(RejectReason::MaxPositionTotal);
        }

        // Gate 5: has_position
        if self.position_tracker.has_position(market) {
            trace!(market = %market, "Signal skipped: Already has position");
            return ExecutionResult::skipped(SkipReason::AlreadyHasPosition);
        }

        // Gate 6: PendingOrder (atomic mark)
        if !self.position_tracker.try_mark_pending_market(market) {
            trace!(market = %market, "Signal skipped: Pending order exists");
            return ExecutionResult::skipped(SkipReason::PendingOrderExists);
        }

        // Gate 7: ActionBudget
        if !self.action_budget.can_send_new_order() {
            // Rollback: unmark pending market since we won't queue the order
            self.position_tracker.unmark_pending_market(market);
            trace!(
                market = %market,
                remaining = self.action_budget.remaining(),
                "Signal skipped: Budget exhausted"
            );
            return ExecutionResult::skipped(SkipReason::BudgetExhausted);
        }

        // Consume budget
        if !self.action_budget.consume() {
            // Race condition: budget exhausted between check and consume
            self.position_tracker.unmark_pending_market(market);
            return ExecutionResult::skipped(SkipReason::BudgetExhausted);
        }

        // All gates passed - create and queue order
        let cloid = ClientOrderId::new();
        let order = PendingOrder::new(
            cloid.clone(),
            *market,
            side,
            price,
            size,
            false, // reduce_only
            now_ms,
        );

        match self.batch_scheduler.enqueue_new_order(order.clone()) {
            EnqueueResult::Queued => {
                let tracked = TrackedOrder::from_pending(order);
                self.try_register_order(tracked, &cloid);
                debug!(cloid = %cloid, market = %market, "Order queued");
                ExecutionResult::queued(cloid)
            }
            EnqueueResult::QueuedDegraded => {
                let tracked = TrackedOrder::from_pending(order);
                self.try_register_order(tracked, &cloid);
                debug!(cloid = %cloid, market = %market, "Order queued (degraded mode)");
                ExecutionResult::queued_degraded(cloid)
            }
            EnqueueResult::QueueFull => {
                self.position_tracker.unmark_pending_market(market);
                debug!(cloid = %cloid, market = %market, "Order rejected: Queue full");
                ExecutionResult::rejected(RejectReason::QueueFull)
            }
            EnqueueResult::InflightFull => {
                self.position_tracker.unmark_pending_market(market);
                debug!(cloid = %cloid, market = %market, "Order rejected: Inflight full");
                ExecutionResult::rejected(RejectReason::InflightFull)
            }
        }
    }

    /// Submit a reduce-only order to close a position.
    ///
    /// Reduce-only orders bypass some gates (position checks) and are
    /// queued with higher priority.
    pub fn submit_reduce_only(
        &self,
        market: &MarketKey,
        side: OrderSide,
        price: Price,
        size: Size,
        now_ms: u64,
    ) -> ExecutionResult {
        // Gate 0: HardStop - reduce_only orders are allowed during HardStop
        // (they help close positions)

        // Gate 1: READY-TRADING - reduce_only orders are allowed even when not ready
        // (they are essential for position management)

        // Create and queue reduce-only order
        let cloid = ClientOrderId::new();
        let order = PendingOrder::new(
            cloid.clone(),
            *market,
            side,
            price,
            size,
            true, // reduce_only
            now_ms,
        );

        match self.batch_scheduler.enqueue_reduce_only(order.clone()) {
            EnqueueResult::Queued | EnqueueResult::InflightFull => {
                // InflightFull still queues for reduce_only
                let tracked = TrackedOrder::from_pending(order);
                self.try_register_order(tracked, &cloid);
                debug!(cloid = %cloid, market = %market, "Reduce-only order queued");
                ExecutionResult::queued(cloid)
            }
            EnqueueResult::QueuedDegraded => {
                let tracked = TrackedOrder::from_pending(order);
                self.try_register_order(tracked, &cloid);
                debug!(cloid = %cloid, market = %market, "Reduce-only order queued (degraded)");
                ExecutionResult::queued_degraded(cloid)
            }
            EnqueueResult::QueueFull => {
                warn!(
                    cloid = %cloid,
                    market = %market,
                    "CRITICAL: Reduce-only queue full - cannot close position"
                );
                ExecutionResult::rejected(RejectReason::QueueFull)
            }
        }
    }

    /// Handle HardStop trigger.
    ///
    /// Drops all pending new orders and prepares for position flattening.
    /// Uses `remove_order` only for cleanup since these orders were already
    /// registered. The `pending_markets_cache` count is decremented correctly
    /// via `remove_order_from_caches`.
    pub async fn on_hard_stop(&self) {
        warn!("HardStop triggered - dropping new orders");

        // Drop all pending new orders
        let dropped = self.batch_scheduler.drop_new_orders();

        for (cloid, _market) in dropped {
            self.position_tracker.remove_order(cloid).await;
        }

        // Note: Position flattening would be triggered via Flattener
        // which is separate from this method
    }

    /// Try to register an order with the position tracker.
    ///
    /// Uses non-blocking try_send first, falls back to async spawn if full.
    fn try_register_order(&self, tracked: TrackedOrder, cloid: &ClientOrderId) {
        if let Err(e) = self.position_tracker.try_register_order(tracked.clone()) {
            // Channel full - spawn async registration
            debug!(
                cloid = %cloid,
                error = ?e,
                "Position tracker channel full, spawning async registration"
            );

            let handle = self.position_tracker.clone();
            let hard_stop = self.hard_stop_latch.clone();
            let tracked_clone = tracked;

            tokio::spawn(async move {
                // Don't register if HardStop triggered during spawn
                if hard_stop.is_triggered() {
                    debug!("Skipping order registration - HardStop active");
                    return;
                }
                // If the order was already removed from caches before this runs, skip to avoid
                // resurrecting stale actor state.
                let cloid = tracked_clone.cloid.clone();
                if handle.get_pending_order(&cloid).is_none() {
                    debug!(cloid = %cloid, "Skipping order registration - already removed");
                    return;
                }
                handle.register_order_actor_only(tracked_clone).await;
            });
        }
    }

    /// Calculate total portfolio notional across all markets.
    ///
    /// Includes:
    /// - All open positions (valued at mark_px)
    /// - All pending orders excluding reduce_only (valued at mark_px)
    ///
    /// # Errors
    /// Returns `Err(RejectReason::MarketDataUnavailable)` if mark_px is unavailable
    /// for any position or pending order market (fail closed).
    fn calculate_total_portfolio_notional(&self) -> Result<Decimal, RejectReason> {
        let positions = self.position_tracker.positions_snapshot();
        let mut total = Decimal::ZERO;

        // Add position notional (fail closed if any mark_px missing)
        for pos in positions {
            let mark_px = self
                .market_state_cache
                .get_mark_px(&pos.market)
                .ok_or_else(|| {
                    warn!(
                        market = %pos.market,
                        "Gate 4: mark price unavailable for position, rejecting order"
                    );
                    RejectReason::MarketDataUnavailable
                })?;
            total += pos.notional(mark_px).inner();
        }

        // Add pending notional (excluding reduce-only), valued at mark_px
        // Fail closed if any pending order's market lacks mark_px
        let cache = &self.market_state_cache;
        let pending_mark_px_missing = Cell::new(false);
        let pending_notional = self
            .position_tracker
            .get_total_pending_notional_excluding_reduce_only(|market| {
                let px = cache.get_mark_px(market);
                if px.is_none() {
                    pending_mark_px_missing.set(true);
                    warn!(
                        market = %market,
                        "Gate 4: mark price unavailable for pending order, rejecting order"
                    );
                }
                px
            });

        if pending_mark_px_missing.get() {
            return Err(RejectReason::MarketDataUnavailable);
        }

        total += pending_notional;

        Ok(total)
    }

    /// Get the batch scheduler (for direct access in tests).
    #[must_use]
    pub fn batch_scheduler(&self) -> &Arc<BatchScheduler> {
        &self.batch_scheduler
    }

    /// Get the position tracker handle.
    #[must_use]
    pub fn position_tracker(&self) -> &PositionTrackerHandle {
        &self.position_tracker
    }

    /// Get the market state cache.
    #[must_use]
    pub fn market_state_cache(&self) -> &Arc<MarketStateCache> {
        &self.market_state_cache
    }

    /// Get the ready checker.
    #[must_use]
    pub fn ready_checker(&self) -> &Arc<TradingReadyChecker> {
        &self.ready_checker
    }

    /// Get the hard stop latch.
    #[must_use]
    pub fn hard_stop_latch(&self) -> &Arc<HardStopLatch> {
        &self.hard_stop_latch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::{BatchConfig, InflightTracker};
    use hip3_core::{AssetId, DexId};
    use hip3_position::spawn_position_tracker;
    use rust_decimal_macros::dec;

    fn sample_market() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    fn sample_market_2() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(1))
    }

    fn setup_executor() -> (Executor, PositionTrackerHandle) {
        let (position_tracker, _join) = spawn_position_tracker(100);
        let config = BatchConfig::default();
        let inflight = Arc::new(InflightTracker::new(100));
        let hard_stop = Arc::new(HardStopLatch::new());
        let batch_scheduler = Arc::new(BatchScheduler::new(config, inflight, hard_stop.clone()));
        let (ready_checker, _rx) = TradingReadyChecker::new();
        let ready_checker = Arc::new(ready_checker);
        let action_budget = Arc::new(ActionBudget::default());
        let market_state_cache = Arc::new(MarketStateCache::new());
        let exec_config = ExecutorConfig::default();

        let executor = Executor::new(
            position_tracker.clone(),
            batch_scheduler,
            ready_checker,
            hard_stop,
            action_budget,
            exec_config,
            market_state_cache,
        );

        (executor, position_tracker)
    }

    #[test]
    fn test_market_state_cache() {
        let cache = MarketStateCache::new();
        let market = sample_market();

        assert!(cache.is_empty());
        assert!(cache.get_mark_px(&market).is_none());

        cache.update(&market, Price::new(dec!(50000)), 1234567890);

        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get_mark_px(&market), Some(Price::new(dec!(50000))));

        let state = cache.get(&market).unwrap();
        assert_eq!(state.mark_px, Price::new(dec!(50000)));
        assert_eq!(state.updated_at, 1234567890);

        cache.remove(&market);
        assert!(cache.is_empty());
    }

    // Note: test_on_signal_not_ready removed
    // Gate 2 (READY-TRADING) is now handled by bot via connection_manager.is_ready().
    // The Executor trusts that the caller has verified readiness before calling on_signal.

    #[tokio::test]
    async fn test_on_signal_hard_stop() {
        let (executor, _pt) = setup_executor();

        // Trigger hard stop
        executor.hard_stop_latch.trigger("test: hard stop");

        let result = executor.on_signal(
            &sample_market(),
            OrderSide::Buy,
            Price::new(dec!(50000)),
            Size::new(dec!(0.001)),
            1234567890,
        );

        assert!(matches!(
            result,
            ExecutionResult::Rejected {
                reason: RejectReason::HardStop
            }
        ));
    }

    #[tokio::test]
    async fn test_on_signal_queued() {
        let (executor, _pt) = setup_executor();
        let market = sample_market();

        // Gate 3 requires mark_px to be set (fail closed)
        executor
            .market_state_cache
            .update(&market, Price::new(dec!(50000)), 1234567890);

        // Use size that results in notional < $50 limit
        // 0.0005 * 50000 = $25
        let result = executor.on_signal(
            &market,
            OrderSide::Buy,
            Price::new(dec!(50000)),
            Size::new(dec!(0.0005)),
            1234567890,
        );

        assert!(
            matches!(result, ExecutionResult::Queued { .. }),
            "Expected Queued, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_on_signal_pending_order_exists() {
        let (executor, _pt) = setup_executor();
        let market = sample_market();

        // Gate 3 requires mark_px to be set (fail closed)
        executor
            .market_state_cache
            .update(&market, Price::new(dec!(50000)), 1234567890);

        // First signal should queue
        // Use small size: 0.0004 * 50000 = $20
        let result1 = executor.on_signal(
            &market,
            OrderSide::Buy,
            Price::new(dec!(50000)),
            Size::new(dec!(0.0004)),
            1234567890,
        );
        assert!(matches!(result1, ExecutionResult::Queued { .. }));

        // Second signal for same market should be skipped due to PendingOrderExists
        // Note: Gate 3 (MaxPositionPerMarket) is checked first and includes pending orders
        // $20 (pending) + $20 (new) = $40 < $50, so Gate 3 passes
        // Gate 6 (PendingOrderExists) should then catch this
        let result2 = executor.on_signal(
            &market,
            OrderSide::Buy,
            Price::new(dec!(50000)),
            Size::new(dec!(0.0004)),
            1234567891,
        );
        assert!(
            matches!(
                result2,
                ExecutionResult::Skipped {
                    reason: SkipReason::PendingOrderExists
                }
            ),
            "Expected PendingOrderExists, got: {result2:?}"
        );
    }

    #[tokio::test]
    async fn test_on_signal_different_markets() {
        let (executor, _pt) = setup_executor();
        let market1 = sample_market();
        let market2 = sample_market_2();

        // Gate 3 requires mark_px to be set for both markets (fail closed)
        executor
            .market_state_cache
            .update(&market1, Price::new(dec!(50000)), 1234567890);
        executor
            .market_state_cache
            .update(&market2, Price::new(dec!(3000)), 1234567890);

        // First market (0.0005 * 50000 = $25 < $50 per-market limit)
        let result1 = executor.on_signal(
            &market1,
            OrderSide::Buy,
            Price::new(dec!(50000)),
            Size::new(dec!(0.0005)),
            1234567890,
        );
        assert!(matches!(result1, ExecutionResult::Queued { .. }));

        // Second market (0.01 * 3000 = $30 < $50 per-market limit)
        // Total: $25 + $30 = $55 < $100 total limit
        let result2 = executor.on_signal(
            &market2,
            OrderSide::Buy,
            Price::new(dec!(3000)),
            Size::new(dec!(0.01)),
            1234567890,
        );
        assert!(matches!(result2, ExecutionResult::Queued { .. }));
    }

    #[tokio::test]
    async fn test_submit_reduce_only() {
        let (executor, _pt) = setup_executor();

        // Reduce-only should work even when not ready
        let result = executor.submit_reduce_only(
            &sample_market(),
            OrderSide::Sell,
            Price::new(dec!(50000)),
            Size::new(dec!(0.001)),
            1234567890,
        );

        assert!(matches!(result, ExecutionResult::Queued { .. }));
    }

    #[tokio::test]
    async fn test_submit_reduce_only_during_hard_stop() {
        let (executor, _pt) = setup_executor();

        // Trigger hard stop
        executor
            .hard_stop_latch
            .trigger("test: hard stop for reduce-only");

        // Reduce-only should still work
        let result = executor.submit_reduce_only(
            &sample_market(),
            OrderSide::Sell,
            Price::new(dec!(50000)),
            Size::new(dec!(0.001)),
            1234567890,
        );

        assert!(matches!(result, ExecutionResult::Queued { .. }));
    }

    #[tokio::test]
    async fn test_max_position_per_market() {
        let (executor, _pt) = setup_executor();

        // Gate 2 (READY-TRADING) is now handled by bot, not checked in on_signal
        let market = sample_market();

        // Set market state with mark price
        executor
            .market_state_cache
            .update(&market, Price::new(dec!(50000)), 1234567890);

        // Try to place order that would exceed $50 limit
        // size * price = 0.002 * 50000 = $100 > $50
        let result = executor.on_signal(
            &market,
            OrderSide::Buy,
            Price::new(dec!(50000)),
            Size::new(dec!(0.002)),
            1234567890,
        );

        assert!(matches!(
            result,
            ExecutionResult::Rejected {
                reason: RejectReason::MaxPositionPerMarket
            }
        ));
    }

    #[tokio::test]
    async fn test_max_position_total() {
        let (executor, _pt) = setup_executor();
        let market1 = sample_market();
        let market2 = sample_market_2();
        let market3 = MarketKey::new(DexId::XYZ, AssetId::new(2));

        // Gate 3 requires mark_px to be set (fail closed)
        executor
            .market_state_cache
            .update(&market1, Price::new(dec!(50000)), 1234567890);
        executor
            .market_state_cache
            .update(&market2, Price::new(dec!(3000)), 1234567890);
        executor
            .market_state_cache
            .update(&market3, Price::new(dec!(1000)), 1234567890);

        // First order: 0.0009 * 50000 = $45 < $50 per-market, < $100 total
        let result1 = executor.on_signal(
            &market1,
            OrderSide::Buy,
            Price::new(dec!(50000)),
            Size::new(dec!(0.0009)),
            1234567890,
        );
        assert!(matches!(result1, ExecutionResult::Queued { .. }));

        // Second order: 0.015 * 3000 = $45 < $50 per-market
        // Total pending: $45 + $45 = $90 < $100 total
        let result2 = executor.on_signal(
            &market2,
            OrderSide::Buy,
            Price::new(dec!(3000)),
            Size::new(dec!(0.015)),
            1234567891,
        );
        assert!(matches!(result2, ExecutionResult::Queued { .. }));

        // Third order: 0.02 * 1000 = $20 < $50 per-market
        // But total: $45 + $45 + $20 = $110 >= $100 total limit
        let result3 = executor.on_signal(
            &market3,
            OrderSide::Buy,
            Price::new(dec!(1000)),
            Size::new(dec!(0.02)),
            1234567892,
        );

        assert!(
            matches!(
                result3,
                ExecutionResult::Rejected {
                    reason: RejectReason::MaxPositionTotal
                }
            ),
            "Expected MaxPositionTotal, got: {result3:?}"
        );
    }

    #[tokio::test]
    async fn test_gate3_rejects_when_mark_px_unavailable() {
        let (executor, _pt) = setup_executor();
        let market = sample_market();

        // Do NOT set mark_px - Gate 3 should reject with MarketDataUnavailable
        let result = executor.on_signal(
            &market,
            OrderSide::Buy,
            Price::new(dec!(50000)),
            Size::new(dec!(0.001)),
            1234567890,
        );

        assert!(matches!(
            result,
            ExecutionResult::Rejected {
                reason: RejectReason::MarketDataUnavailable
            }
        ));
    }

    #[tokio::test]
    async fn test_gate4_rejects_when_position_mark_px_unavailable() {
        let (executor, pt) = setup_executor();
        let market1 = sample_market();
        let market2 = sample_market_2();

        // Set mark_px for market1 (the order we're placing)
        executor
            .market_state_cache
            .update(&market1, Price::new(dec!(50000)), 1234567890);

        // Add a position in market2 (without mark_px)
        pt.fill(
            market2,
            OrderSide::Buy,
            hip3_core::Price::new(dec!(3000)),
            hip3_core::Size::new(dec!(0.001)),
            1234567890,
        )
        .await;

        // Wait for position to be registered
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Try to place order in market1 - Gate 4 should reject because
        // it can't calculate total portfolio notional (market2 has no mark_px)
        // Use small size to pass Gate 3 (0.0005 * 50000 = $25 < $50)
        let result = executor.on_signal(
            &market1,
            OrderSide::Buy,
            Price::new(dec!(50000)),
            Size::new(dec!(0.0005)),
            1234567890,
        );

        assert!(matches!(
            result,
            ExecutionResult::Rejected {
                reason: RejectReason::MarketDataUnavailable
            }
        ));
    }

    #[tokio::test]
    async fn test_gate4_rejects_when_pending_order_mark_px_unavailable() {
        let (executor, _pt) = setup_executor();
        let market1 = sample_market();
        let market2 = sample_market_2();

        // Set mark_px for both markets initially
        executor
            .market_state_cache
            .update(&market1, Price::new(dec!(50000)), 1234567890);
        executor
            .market_state_cache
            .update(&market2, Price::new(dec!(3000)), 1234567890);

        // Place first order in market2 (0.01 * 3000 = $30 < $50)
        let result1 = executor.on_signal(
            &market2,
            OrderSide::Buy,
            Price::new(dec!(3000)),
            Size::new(dec!(0.01)),
            1234567890,
        );
        assert!(matches!(result1, ExecutionResult::Queued { .. }));

        // Remove mark_px for market2 (simulating cache staleness)
        executor.market_state_cache.remove(&market2);

        // Try to place order in market1 - Gate 4 should reject because
        // it can't calculate pending notional for market2
        // Use small size to pass Gate 3 (0.0005 * 50000 = $25 < $50)
        let result2 = executor.on_signal(
            &market1,
            OrderSide::Buy,
            Price::new(dec!(50000)),
            Size::new(dec!(0.0005)),
            1234567891,
        );

        assert!(matches!(
            result2,
            ExecutionResult::Rejected {
                reason: RejectReason::MarketDataUnavailable
            }
        ));
    }

    // ========================================================================
    // PostIdGenerator tests
    // ========================================================================

    #[test]
    fn test_post_id_generator_monotonic() {
        let gen = PostIdGenerator::new();

        let mut prev = 0u64;
        for _ in 0..1000 {
            let id = gen.next();
            assert!(id > prev, "post_id must be strictly increasing");
            prev = id;
        }
    }

    #[test]
    fn test_post_id_generator_starts_at_one() {
        let gen = PostIdGenerator::new();
        assert_eq!(gen.next(), 1);
        assert_eq!(gen.next(), 2);
        assert_eq!(gen.next(), 3);
    }

    #[test]
    fn test_post_id_generator_concurrent() {
        use std::thread;

        let gen = Arc::new(PostIdGenerator::new());
        let mut handles = vec![];

        for _ in 0..8 {
            let gen = Arc::clone(&gen);
            handles.push(thread::spawn(move || {
                let mut ids = vec![];
                for _ in 0..100 {
                    ids.push(gen.next());
                }
                ids
            }));
        }

        let mut all_ids: Vec<u64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();

        all_ids.sort_unstable();
        let original_len = all_ids.len();
        all_ids.dedup();

        assert_eq!(all_ids.len(), original_len, "All post_ids must be unique");
    }

    // ========================================================================
    // ActionBudget tests
    // ========================================================================

    #[test]
    fn test_action_budget_basic() {
        let budget = ActionBudget::new(3, 1000);

        assert!(budget.can_send_new_order());
        assert_eq!(budget.remaining(), 3);

        // Consume 3
        assert!(budget.consume());
        assert!(budget.consume());
        assert!(budget.consume());

        // 4th should fail
        assert!(!budget.consume());
        assert!(!budget.can_send_new_order());
        assert_eq!(budget.remaining(), 0);
    }

    #[test]
    fn test_action_budget_interval_reset() {
        let budget = ActionBudget::new(3, 1000);

        // Consume all budget at time 0
        assert!(budget.consume_at(0));
        assert!(budget.consume_at(0));
        assert!(budget.consume_at(0));
        assert!(!budget.consume_at(0)); // Exhausted

        // After interval expires, budget should reset
        assert!(budget.consume_at(1001));
        assert_eq!(budget.remaining(), 2);
    }

    #[test]
    fn test_action_budget_default() {
        let budget = ActionBudget::default();
        // Default: 100 orders per second
        assert_eq!(budget.remaining(), 100);
    }
}
