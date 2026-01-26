//! Batch scheduling for order and cancel submission.
//!
//! This module implements the batch scheduler that manages order queues
//! and rate-limits submissions to the exchange SDK. Key features:
//!
//! - Three-tier priority queuing (cancels > reduce_only > new_orders)
//! - Inflight order tracking with atomic operations
//! - High watermark degraded mode
//! - HardStop integration for emergency position closing
//!
//! # SDK Constraints
//!
//! The exchange SDK requires:
//! - One action type per tick (orders OR cancels, not both)
//! - Maximum batch sizes per request
//! - Rate limiting via tick intervals

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tracing::{debug, warn};

use hip3_core::{
    ActionBatch, ClientOrderId, EnqueueResult, MarketKey, PendingCancel, PendingOrder,
};

// Import HardStopLatch from risk module (extended implementation)
use crate::risk::HardStopLatch;

// ============================================================================
// InflightTracker
// ============================================================================

/// Thread-safe tracker for in-flight order count.
///
/// Uses atomic operations to safely track how many orders are currently
/// being processed by the exchange. This prevents exceeding the exchange's
/// rate limits and provides backpressure to the order queue.
#[derive(Debug)]
pub struct InflightTracker {
    count: AtomicU32,
    limit: u32,
}

impl InflightTracker {
    /// Create a new inflight tracker with the given limit.
    ///
    /// # Arguments
    /// * `limit` - Maximum number of in-flight orders allowed (typically 100)
    #[must_use]
    pub fn new(limit: u32) -> Self {
        Self {
            count: AtomicU32::new(0),
            limit,
        }
    }

    /// Get the current in-flight count.
    #[must_use]
    pub fn current(&self) -> u32 {
        self.count.load(Ordering::Acquire)
    }

    /// Get the configured limit.
    #[must_use]
    pub fn limit(&self) -> u32 {
        self.limit
    }

    /// Try to increment the in-flight count.
    ///
    /// Uses CAS (Compare-And-Swap) loop to safely increment without
    /// exceeding the limit. This is thread-safe and lock-free.
    ///
    /// # Returns
    /// - `true` if incremented successfully
    /// - `false` if already at limit
    pub fn increment(&self) -> bool {
        loop {
            let current = self.count.load(Ordering::Acquire);
            if current >= self.limit {
                return false;
            }

            // Try to atomically increment
            match self.count.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(_) => continue, // Retry on contention
            }
        }
    }

    /// Decrement the in-flight count.
    ///
    /// Uses saturating subtraction to prevent underflow.
    ///
    /// # Returns
    /// - `true` if decremented successfully
    /// - `false` if already at 0
    pub fn decrement(&self) -> bool {
        loop {
            let current = self.count.load(Ordering::Acquire);
            if current == 0 {
                return false;
            }

            // Try to atomically decrement
            match self.count.compare_exchange_weak(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(_) => continue, // Retry on contention
            }
        }
    }
}

// ============================================================================
// BatchConfig
// ============================================================================

/// Configuration for the batch scheduler.
#[derive(Debug, Clone)]
pub struct BatchConfig {
    /// Interval between ticks in milliseconds.
    pub interval_ms: u64,
    /// Maximum orders per batch submission.
    pub max_orders_per_batch: usize,
    /// Maximum cancels per batch submission.
    pub max_cancels_per_batch: usize,
    /// High watermark for inflight orders (triggers degraded mode).
    pub inflight_high_watermark: u32,
    /// Capacity of the cancel queue.
    pub cancel_queue_capacity: usize,
    /// Capacity of the reduce_only order queue.
    pub reduce_only_queue_capacity: usize,
    /// Capacity of the new order queue.
    pub new_order_queue_capacity: usize,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            interval_ms: 100,
            max_orders_per_batch: 50,
            max_cancels_per_batch: 50,
            inflight_high_watermark: 80,
            cancel_queue_capacity: 200,
            reduce_only_queue_capacity: 500,
            new_order_queue_capacity: 1000,
        }
    }
}

// ============================================================================
// BatchScheduler
// ============================================================================

/// Batch scheduler for managing order submission queues.
///
/// The scheduler implements a three-tier priority system:
/// 1. **Cancels** - Always processed first (highest priority)
/// 2. **Reduce-only orders** - Processed before new orders
/// 3. **New orders** - Lowest priority
///
/// # Priority Rules
///
/// - SDK allows only one action type per tick
/// - If cancels are pending, only cancels are sent
/// - When cancel queue is empty, orders are sent
/// - In high watermark mode, only reduce_only orders are sent
/// - In HardStop mode, new_orders are skipped entirely
///
/// # Thread Safety
///
/// All queue operations are protected by `parking_lot::Mutex` for
/// efficient locking. The inflight tracker uses lock-free atomics.
#[derive(Debug)]
pub struct BatchScheduler {
    /// Tick interval for batch processing.
    interval: Duration,
    /// Queue of pending cancel requests (highest priority).
    pending_cancels: Mutex<VecDeque<PendingCancel>>,
    /// Queue of pending reduce-only orders (medium priority).
    pending_reduce_only: Mutex<VecDeque<PendingOrder>>,
    /// Queue of pending new orders (lowest priority).
    pending_new_orders: Mutex<VecDeque<PendingOrder>>,
    /// Shared inflight order tracker.
    inflight_tracker: Arc<InflightTracker>,
    /// Scheduler configuration.
    config: BatchConfig,
    /// Hard stop latch for emergency mode.
    hard_stop_latch: Arc<HardStopLatch>,
}

impl BatchScheduler {
    /// Create a new batch scheduler.
    ///
    /// # Arguments
    /// * `config` - Scheduler configuration
    /// * `inflight_tracker` - Shared inflight order tracker
    /// * `hard_stop_latch` - Hard stop latch for emergency mode
    #[must_use]
    pub fn new(
        config: BatchConfig,
        inflight_tracker: Arc<InflightTracker>,
        hard_stop_latch: Arc<HardStopLatch>,
    ) -> Self {
        Self {
            interval: Duration::from_millis(config.interval_ms),
            pending_cancels: Mutex::new(VecDeque::with_capacity(config.cancel_queue_capacity)),
            pending_reduce_only: Mutex::new(VecDeque::with_capacity(
                config.reduce_only_queue_capacity,
            )),
            pending_new_orders: Mutex::new(VecDeque::with_capacity(
                config.new_order_queue_capacity,
            )),
            inflight_tracker,
            config,
            hard_stop_latch,
        }
    }

    /// Get the tick interval.
    #[must_use]
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Enqueue a new order.
    ///
    /// New orders have the lowest priority and are subject to:
    /// - Inflight limit check (rejected if at limit)
    /// - Queue capacity check
    /// - High watermark degradation
    ///
    /// # Returns
    /// - `Queued` - Successfully queued for submission
    /// - `QueuedDegraded` - Queued but system is in degraded mode
    /// - `QueueFull` - Queue capacity exceeded
    /// - `InflightFull` - Too many in-flight orders
    pub fn enqueue_new_order(&self, order: PendingOrder) -> EnqueueResult {
        let inflight = self.inflight_tracker.current();
        let limit = self.inflight_tracker.limit();

        // Check inflight limit first
        if inflight >= limit {
            debug!(
                cloid = %order.cloid,
                inflight,
                limit,
                "New order rejected: inflight at limit"
            );
            return EnqueueResult::InflightFull;
        }

        let mut queue = self.pending_new_orders.lock();

        // Check queue capacity
        if queue.len() >= self.config.new_order_queue_capacity {
            debug!(
                cloid = %order.cloid,
                queue_len = queue.len(),
                capacity = self.config.new_order_queue_capacity,
                "New order rejected: queue full"
            );
            return EnqueueResult::QueueFull;
        }

        queue.push_back(order);

        // Check for degraded mode
        if inflight >= self.config.inflight_high_watermark {
            EnqueueResult::QueuedDegraded
        } else {
            EnqueueResult::Queued
        }
    }

    /// Enqueue a reduce-only order.
    ///
    /// Reduce-only orders have medium priority and are used to close
    /// positions. They are accepted even when inflight is at limit
    /// (they will be queued and processed when space is available).
    ///
    /// # Panics
    /// Debug assertion fails if `order.reduce_only` is false.
    ///
    /// # Returns
    /// - `Queued` - Successfully queued for submission
    /// - `QueueFull` - Queue capacity exceeded (CRITICAL)
    /// - `InflightFull` - At inflight limit (still queued)
    pub fn enqueue_reduce_only(&self, order: PendingOrder) -> EnqueueResult {
        debug_assert!(
            order.reduce_only,
            "enqueue_reduce_only called with non-reduce_only order"
        );

        let mut queue = self.pending_reduce_only.lock();

        // Check queue capacity
        if queue.len() >= self.config.reduce_only_queue_capacity {
            // CRITICAL: reduce_only queue overflow is a serious issue
            // as it means we can't close positions
            warn!(
                cloid = %order.cloid,
                queue_len = queue.len(),
                capacity = self.config.reduce_only_queue_capacity,
                "CRITICAL: reduce_only queue full - cannot close position"
            );
            return EnqueueResult::QueueFull;
        }

        queue.push_back(order);

        // Check if at inflight limit (order is still queued)
        let inflight = self.inflight_tracker.current();
        if inflight >= self.inflight_tracker.limit() {
            EnqueueResult::InflightFull
        } else {
            EnqueueResult::Queued
        }
    }

    /// Enqueue a cancel request.
    ///
    /// Cancel requests have the highest priority and are always accepted
    /// unless the queue is full.
    ///
    /// # Returns
    /// - `Queued` - Successfully queued for submission
    /// - `QueueFull` - Queue capacity exceeded (CRITICAL)
    pub fn enqueue_cancel(&self, cancel: PendingCancel) -> EnqueueResult {
        let mut queue = self.pending_cancels.lock();

        // Check queue capacity
        if queue.len() >= self.config.cancel_queue_capacity {
            // CRITICAL: cancel queue overflow is a serious issue
            warn!(
                oid = cancel.oid,
                queue_len = queue.len(),
                capacity = self.config.cancel_queue_capacity,
                "CRITICAL: cancel queue full - cannot cancel order"
            );
            return EnqueueResult::QueueFull;
        }

        queue.push_back(cancel);
        EnqueueResult::Queued
    }

    /// Process one tick and return the next action batch.
    ///
    /// This method is called periodically by the execution loop. It:
    /// 1. Returns `None` if at inflight limit
    /// 2. Returns `Cancels` batch if cancels are pending
    /// 3. Returns `Orders` batch otherwise (reduce_only + new_orders)
    ///
    /// # Priority Rules
    /// - Cancels always take priority over orders
    /// - In high watermark mode, only reduce_only orders are processed
    /// - In HardStop mode, new_orders are skipped entirely
    ///
    /// # Important
    /// This method does NOT increment the inflight counter.
    /// Call `on_batch_sent()` after successfully sending the batch.
    #[must_use]
    pub fn tick(&self) -> Option<ActionBatch> {
        let inflight = self.inflight_tracker.current();
        let limit = self.inflight_tracker.limit();

        // Cannot process if at inflight limit
        if inflight >= limit {
            debug!(inflight, limit, "tick: at inflight limit, returning None");
            return None;
        }

        // Priority 1: Cancels
        {
            let mut cancels = self.pending_cancels.lock();
            if !cancels.is_empty() {
                let batch_size = cancels.len().min(self.config.max_cancels_per_batch);
                let batch: Vec<_> = cancels.drain(..batch_size).collect();
                debug!(batch_size = batch.len(), "tick: returning cancel batch");
                return Some(ActionBatch::Cancels(batch));
            }
        }

        // Priority 2: Orders (reduce_only + new_orders)
        let mut orders = Vec::new();
        let max_orders = self.config.max_orders_per_batch;
        let is_high_watermark = inflight >= self.config.inflight_high_watermark;
        let is_hard_stop = self.hard_stop_latch.is_triggered();

        // First, drain reduce_only orders
        {
            let mut reduce_only = self.pending_reduce_only.lock();
            while orders.len() < max_orders && !reduce_only.is_empty() {
                if let Some(order) = reduce_only.pop_front() {
                    orders.push(order);
                }
            }
        }

        // Then, drain new_orders (unless in high watermark or hard stop mode)
        if !is_high_watermark && !is_hard_stop {
            let mut new_orders = self.pending_new_orders.lock();
            while orders.len() < max_orders && !new_orders.is_empty() {
                if let Some(order) = new_orders.pop_front() {
                    orders.push(order);
                }
            }
        } else if is_hard_stop {
            debug!("tick: HardStop active, skipping new_orders");
        } else {
            debug!(inflight, "tick: high watermark, skipping new_orders");
        }

        if orders.is_empty() {
            None
        } else {
            debug!(batch_size = orders.len(), "tick: returning order batch");
            Some(ActionBatch::Orders(orders))
        }
    }

    /// Called after a batch is successfully sent to the exchange.
    ///
    /// Increments the inflight counter.
    pub fn on_batch_sent(&self) {
        self.inflight_tracker.increment();
    }

    /// Called when a batch completes (response received or timeout).
    ///
    /// Decrements the inflight counter.
    pub fn on_batch_complete(&self) {
        self.inflight_tracker.decrement();
    }

    /// Drop all pending new orders (HardStop cleanup).
    ///
    /// Called when HardStop is triggered to clear the new order queue.
    /// Returns the client order IDs and markets of dropped orders for
    /// proper cleanup (e.g., notifying the strategy).
    ///
    /// # Returns
    /// Vector of (ClientOrderId, MarketKey) pairs for dropped orders.
    #[must_use]
    pub fn drop_new_orders(&self) -> Vec<(ClientOrderId, MarketKey)> {
        let mut queue = self.pending_new_orders.lock();
        let dropped: Vec<_> = queue
            .drain(..)
            .map(|order| (order.cloid, order.market))
            .collect();

        if !dropped.is_empty() {
            warn!(
                count = dropped.len(),
                "HardStop: dropped all pending new orders"
            );
        }

        dropped
    }

    /// Requeue failed reduce-only orders at the front of the queue.
    ///
    /// Called when reduce_only orders fail to execute and need to be
    /// retried. They are placed at the front to maintain priority.
    ///
    /// # Arguments
    /// * `orders` - Orders to requeue (will be placed at front)
    pub fn requeue_reduce_only(&self, orders: Vec<PendingOrder>) {
        if orders.is_empty() {
            return;
        }

        let mut queue = self.pending_reduce_only.lock();

        // Insert at front in reverse order to maintain original order
        for order in orders.into_iter().rev() {
            debug_assert!(
                order.reduce_only,
                "requeue_reduce_only called with non-reduce_only order"
            );
            queue.push_front(order);
        }
    }

    /// Get the current queue lengths for monitoring.
    #[must_use]
    pub fn queue_lengths(&self) -> (usize, usize, usize) {
        let cancels = self.pending_cancels.lock().len();
        let reduce_only = self.pending_reduce_only.lock().len();
        let new_orders = self.pending_new_orders.lock().len();
        (cancels, reduce_only, new_orders)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetId, DexId, MarketKey, OrderSide, Price, Size};
    use rust_decimal_macros::dec;

    fn sample_market() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    fn sample_pending_order(reduce_only: bool) -> PendingOrder {
        PendingOrder::new(
            ClientOrderId::new(),
            sample_market(),
            OrderSide::Buy,
            Price::new(dec!(50000)),
            Size::new(dec!(0.1)),
            reduce_only,
            1234567890,
        )
    }

    fn sample_pending_cancel() -> PendingCancel {
        PendingCancel::new(sample_market(), 123, 1234567890)
    }

    fn default_scheduler() -> BatchScheduler {
        let config = BatchConfig::default();
        let inflight = Arc::new(InflightTracker::new(100));
        let hard_stop = Arc::new(HardStopLatch::new());
        BatchScheduler::new(config, inflight, hard_stop)
    }

    // Test 1: Normal enqueue - new_order/reduce_only/cancel returns Queued
    #[test]
    fn test_normal_enqueue() {
        let scheduler = default_scheduler();

        // New order
        let result = scheduler.enqueue_new_order(sample_pending_order(false));
        assert_eq!(result, EnqueueResult::Queued);

        // Reduce-only order
        let result = scheduler.enqueue_reduce_only(sample_pending_order(true));
        assert_eq!(result, EnqueueResult::Queued);

        // Cancel
        let result = scheduler.enqueue_cancel(sample_pending_cancel());
        assert_eq!(result, EnqueueResult::Queued);
    }

    // Test 2: new_order queue overflow - 1001st item returns QueueFull
    #[test]
    fn test_new_order_queue_overflow() {
        let scheduler = default_scheduler();

        // Fill queue to capacity (1000)
        for _ in 0..1000 {
            let result = scheduler.enqueue_new_order(sample_pending_order(false));
            assert!(result.is_queued());
        }

        // 1001st should fail
        let result = scheduler.enqueue_new_order(sample_pending_order(false));
        assert_eq!(result, EnqueueResult::QueueFull);
    }

    // Test 3: reduce_only queue overflow - 501st item returns QueueFull
    #[test]
    fn test_reduce_only_queue_overflow() {
        let scheduler = default_scheduler();

        // Fill queue to capacity (500)
        for _ in 0..500 {
            let result = scheduler.enqueue_reduce_only(sample_pending_order(true));
            assert!(result.is_queued() || result == EnqueueResult::InflightFull);
        }

        // 501st should fail
        let result = scheduler.enqueue_reduce_only(sample_pending_order(true));
        assert_eq!(result, EnqueueResult::QueueFull);
    }

    // Test 4: cancel queue overflow - 201st item returns QueueFull
    #[test]
    fn test_cancel_queue_overflow() {
        let scheduler = default_scheduler();

        // Fill queue to capacity (200)
        for _ in 0..200 {
            let result = scheduler.enqueue_cancel(sample_pending_cancel());
            assert_eq!(result, EnqueueResult::Queued);
        }

        // 201st should fail
        let result = scheduler.enqueue_cancel(sample_pending_cancel());
        assert_eq!(result, EnqueueResult::QueueFull);
    }

    // Test 5: High watermark degradation - new_order returns QueuedDegraded, reduce_only returns Queued
    #[test]
    fn test_high_watermark_degradation() {
        let config = BatchConfig::default();
        let inflight = Arc::new(InflightTracker::new(100));
        let hard_stop = Arc::new(HardStopLatch::new());

        // Set inflight to high watermark (80)
        for _ in 0..80 {
            inflight.increment();
        }

        let scheduler = BatchScheduler::new(config, inflight, hard_stop);

        // New order should be degraded
        let result = scheduler.enqueue_new_order(sample_pending_order(false));
        assert_eq!(result, EnqueueResult::QueuedDegraded);

        // Reduce-only should still be Queued (or InflightFull but queued)
        let result = scheduler.enqueue_reduce_only(sample_pending_order(true));
        assert_eq!(result, EnqueueResult::Queued);
    }

    // Test 6: Cancel priority - when cancels pending, only CancelBatch is returned
    #[test]
    fn test_cancel_priority() {
        let scheduler = default_scheduler();

        // Add orders and cancels
        scheduler.enqueue_new_order(sample_pending_order(false));
        scheduler.enqueue_reduce_only(sample_pending_order(true));
        scheduler.enqueue_cancel(sample_pending_cancel());

        // tick() should return cancels only
        let batch = scheduler.tick().unwrap();
        assert!(matches!(batch, ActionBatch::Cancels(_)));
    }

    // Test 7: Orders only - when cancel queue is empty, OrderBatch is returned
    #[test]
    fn test_orders_only() {
        let scheduler = default_scheduler();

        // Add orders only
        scheduler.enqueue_new_order(sample_pending_order(false));
        scheduler.enqueue_reduce_only(sample_pending_order(true));

        // tick() should return orders
        let batch = scheduler.tick().unwrap();
        assert!(matches!(batch, ActionBatch::Orders(_)));
    }

    // Test 8: Inflight limit - tick returns None when at limit
    #[test]
    fn test_inflight_limit_tick() {
        let config = BatchConfig::default();
        let inflight = Arc::new(InflightTracker::new(100));
        let hard_stop = Arc::new(HardStopLatch::new());

        // Set inflight to limit (100)
        for _ in 0..100 {
            inflight.increment();
        }

        let scheduler = BatchScheduler::new(config, inflight, hard_stop);

        // Add orders
        scheduler.enqueue_cancel(sample_pending_cancel());

        // tick() should return None
        assert!(scheduler.tick().is_none());
    }

    // Test 9: requeue_reduce_only - failed orders go to front of queue
    #[test]
    fn test_requeue_reduce_only() {
        let scheduler = default_scheduler();

        // Add some orders first
        let order1 = sample_pending_order(true);
        let order2 = sample_pending_order(true);
        let cloid1 = order1.cloid.clone();
        let cloid2 = order2.cloid.clone();

        scheduler.enqueue_reduce_only(order1);
        scheduler.enqueue_reduce_only(order2);

        // Drain the queue via tick
        let batch = scheduler.tick().unwrap();
        let orders = match batch {
            ActionBatch::Orders(o) => o,
            _ => panic!("Expected orders batch"),
        };

        // Requeue the orders
        scheduler.requeue_reduce_only(orders);

        // Next tick should return the requeued orders in original order
        let batch = scheduler.tick().unwrap();
        let orders = match batch {
            ActionBatch::Orders(o) => o,
            _ => panic!("Expected orders batch"),
        };

        // First order should be cloid1
        assert_eq!(orders[0].cloid, cloid1);
        assert_eq!(orders[1].cloid, cloid2);
    }

    // Test 10: tick does not increment inflight
    #[test]
    fn test_tick_does_not_increment() {
        let config = BatchConfig::default();
        let inflight = Arc::new(InflightTracker::new(100));
        let hard_stop = Arc::new(HardStopLatch::new());
        let scheduler = BatchScheduler::new(config, Arc::clone(&inflight), hard_stop);

        scheduler.enqueue_new_order(sample_pending_order(false));

        let before = inflight.current();
        let _ = scheduler.tick();
        let after = inflight.current();

        assert_eq!(before, after, "tick should not change inflight count");
    }

    // Test 11: InflightTracker integrity - increment/decrement work correctly
    #[test]
    fn test_inflight_tracker_integrity() {
        let tracker = InflightTracker::new(3);

        assert_eq!(tracker.current(), 0);

        // Increment to limit
        assert!(tracker.increment());
        assert_eq!(tracker.current(), 1);
        assert!(tracker.increment());
        assert_eq!(tracker.current(), 2);
        assert!(tracker.increment());
        assert_eq!(tracker.current(), 3);

        // At limit, should fail
        assert!(!tracker.increment());
        assert_eq!(tracker.current(), 3);

        // Decrement
        assert!(tracker.decrement());
        assert_eq!(tracker.current(), 2);
        assert!(tracker.decrement());
        assert_eq!(tracker.current(), 1);
        assert!(tracker.decrement());
        assert_eq!(tracker.current(), 0);

        // At zero, should fail
        assert!(!tracker.decrement());
        assert_eq!(tracker.current(), 0);
    }

    // Test 12: 1 tick = 1 action type - orders and cancels never mixed
    #[test]
    fn test_one_action_type_per_tick() {
        let scheduler = default_scheduler();

        // Add both orders and cancels
        scheduler.enqueue_new_order(sample_pending_order(false));
        scheduler.enqueue_reduce_only(sample_pending_order(true));
        scheduler.enqueue_cancel(sample_pending_cancel());
        scheduler.enqueue_cancel(sample_pending_cancel());

        // First tick should be cancels only
        let batch1 = scheduler.tick().unwrap();
        assert!(matches!(batch1, ActionBatch::Cancels(_)));

        // Second tick should be orders only (since cancels are drained)
        let batch2 = scheduler.tick().unwrap();
        assert!(matches!(batch2, ActionBatch::Orders(_)));
    }

    // Additional test: HardStop skips new_orders
    #[test]
    fn test_hard_stop_skips_new_orders() {
        let config = BatchConfig::default();
        let inflight = Arc::new(InflightTracker::new(100));
        let hard_stop = Arc::new(HardStopLatch::new());

        let scheduler = BatchScheduler::new(config, inflight, Arc::clone(&hard_stop));

        // Add orders
        scheduler.enqueue_new_order(sample_pending_order(false));
        scheduler.enqueue_reduce_only(sample_pending_order(true));

        // Trigger hard stop
        hard_stop.trigger("test: hard stop triggered");
        assert!(hard_stop.is_triggered());

        // tick() should only return reduce_only
        let batch = scheduler.tick().unwrap();
        let orders = match batch {
            ActionBatch::Orders(o) => o,
            _ => panic!("Expected orders batch"),
        };

        // Should only contain the reduce_only order
        assert_eq!(orders.len(), 1);
        assert!(orders[0].reduce_only);

        // New order should still be in queue
        let (_, _, new_orders) = scheduler.queue_lengths();
        assert_eq!(new_orders, 1);
    }

    // Additional test: drop_new_orders
    #[test]
    fn test_drop_new_orders() {
        let scheduler = default_scheduler();

        // Add new orders
        let order1 = sample_pending_order(false);
        let order2 = sample_pending_order(false);
        let cloid1 = order1.cloid.clone();
        let cloid2 = order2.cloid.clone();
        let market = order1.market;

        scheduler.enqueue_new_order(order1);
        scheduler.enqueue_new_order(order2);

        // Also add reduce_only (should not be dropped)
        scheduler.enqueue_reduce_only(sample_pending_order(true));

        // Drop new orders
        let dropped = scheduler.drop_new_orders();

        assert_eq!(dropped.len(), 2);
        assert!(dropped.iter().any(|(c, _)| c == &cloid1));
        assert!(dropped.iter().any(|(c, _)| c == &cloid2));
        assert!(dropped.iter().all(|(_, m)| m == &market));

        // Queue should be empty
        let (_, _, new_orders) = scheduler.queue_lengths();
        assert_eq!(new_orders, 0);

        // reduce_only should still be there
        let (_, reduce_only, _) = scheduler.queue_lengths();
        assert_eq!(reduce_only, 1);
    }

    // Additional test: HardStopLatch basic operations
    #[test]
    fn test_hard_stop_latch() {
        let latch = HardStopLatch::new();

        assert!(!latch.is_triggered());

        latch.trigger("test reason");
        assert!(latch.is_triggered());

        latch.reset();
        assert!(!latch.is_triggered());
    }
}
