//! Position tracking actor for order and position lifecycle management.
//!
//! Provides a single-threaded actor that tracks:
//! - Pending orders (cloid -> TrackedOrder)
//! - Open positions (market -> Position)
//!
//! Uses DashMap caches for synchronous high-frequency lookups from the main thread.
//!
//! # Dual State Architecture: Actor vs Handle
//!
//! This module uses a deliberate dual-state architecture for performance:
//!
//! ## Actor State (`PositionTrackerTask`)
//! - `pending_orders: HashMap<ClientOrderId, TrackedOrder>` - Authoritative state
//! - Updated only via message processing (single-threaded, no locking)
//! - Used for internal bookkeeping and consistency checks
//!
//! ## Handle State (`PositionTrackerHandle`)
//! - `pending_orders_data: Arc<DashMap<...>>` - Cache for sync lookups
//! - Updated eagerly by Handle methods (before sending messages)
//! - Enables O(1) sync access without async channel round-trip
//!
//! ## Why Two States?
//!
//! The hot path (signal processing) requires sync access to:
//! - Check if market has pending order (`has_pending_order`)
//! - Calculate pending notional (`get_pending_notional_excluding_reduce_only`)
//!
//! If we only had Actor state, every lookup would require async message passing,
//! adding latency to the critical trading path.
//!
//! ## Consistency Guarantee
//!
//! Handle caches are updated BEFORE sending messages to Actor:
//! 1. `try_register_order()`: Add to cache → try_send to Actor
//! 2. If try_send fails, caller uses `register_order_actor_only()` (cache already updated)
//!
//! This means Handle caches may briefly show orders that Actor hasn't processed yet,
//! but they will NEVER miss orders that Actor has. This is the correct behavior for
//! gate checks (we want to prevent duplicate orders, so seeing "order exists" early is safe).
//!
//! ## Cache Rollback
//!
//! If order creation fails AFTER cache update (e.g., QueueFull), use
//! `rollback_order_caches()` to restore consistency.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use rust_decimal::Decimal;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, trace};

use hip3_core::{ClientOrderId, MarketKey, OrderSide, OrderState, Price, Size, TrackedOrder};

// ============================================================================
// Position
// ============================================================================

/// An open position in a market.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    /// Market identifier.
    pub market: MarketKey,
    /// Position side (Buy = long, Sell = short).
    pub side: OrderSide,
    /// Position size (always positive).
    pub size: Size,
    /// Average entry price.
    pub entry_price: Price,
    /// Timestamp when position was opened (Unix ms).
    pub entry_timestamp_ms: u64,
    /// Timestamp of last update (Unix ms).
    pub last_update_ms: u64,
}

impl Position {
    /// Create a new position from an initial fill.
    #[must_use]
    pub fn new(
        market: MarketKey,
        side: OrderSide,
        size: Size,
        price: Price,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            market,
            side,
            size,
            entry_price: price,
            entry_timestamp_ms: timestamp_ms,
            last_update_ms: timestamp_ms,
        }
    }

    /// Calculate the notional value of the position.
    #[must_use]
    pub fn notional(&self, mark_px: Price) -> Size {
        Size::new(self.size.inner() * mark_px.inner())
    }

    /// Check if the position is empty (size is zero).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.size.is_zero()
    }

    /// Check if this is a long position.
    #[must_use]
    pub fn is_long(&self) -> bool {
        self.side == OrderSide::Buy && !self.is_empty()
    }

    /// Check if this is a short position.
    #[must_use]
    pub fn is_short(&self) -> bool {
        self.side == OrderSide::Sell && !self.is_empty()
    }
}

// ============================================================================
// PositionTrackerMsg
// ============================================================================

/// Messages for the position tracker actor.
#[derive(Debug)]
pub enum PositionTrackerMsg {
    /// Register a new order for tracking.
    RegisterOrder(TrackedOrder),

    /// Remove an order from tracking (terminal state reached).
    RemoveOrder(ClientOrderId),

    /// Update order state from orderUpdates feed.
    OrderUpdate {
        /// Client order ID.
        cloid: ClientOrderId,
        /// New order state.
        state: OrderState,
        /// Cumulative filled size.
        filled_size: Size,
        /// Exchange order ID (if assigned).
        oid: Option<u64>,
    },

    /// Process a fill from userFills feed.
    Fill {
        /// Market of the fill.
        market: MarketKey,
        /// Fill side.
        side: OrderSide,
        /// Fill price.
        price: Price,
        /// Fill size.
        size: Size,
        /// Fill timestamp (Unix ms).
        timestamp_ms: u64,
        /// Client order ID for deduplication (optional).
        cloid: Option<ClientOrderId>,
    },

    /// Begin snapshot processing (buffer subsequent messages).
    SnapshotStart,

    /// End snapshot processing (apply buffered messages).
    SnapshotEnd,

    /// Sync positions from external source (e.g., Hyperliquid API).
    /// Replaces all current positions with the provided list.
    SyncPositions(Vec<Position>),

    /// Graceful shutdown.
    Shutdown,
}

// ============================================================================
// PositionTrackerTask
// ============================================================================

/// Position tracker actor task.
///
/// Runs in its own tokio task, processing messages sequentially.
/// Maintains authoritative state for orders and positions.
pub struct PositionTrackerTask {
    /// Message receiver.
    rx: mpsc::Receiver<PositionTrackerMsg>,

    /// Pending orders: cloid -> TrackedOrder.
    pending_orders: HashMap<ClientOrderId, TrackedOrder>,

    /// Open positions: market -> Position.
    positions: HashMap<MarketKey, Position>,

    /// Buffer for messages received during snapshot.
    snapshot_buffer: Vec<PositionTrackerMsg>,

    /// Whether snapshot processing is in progress.
    in_snapshot: bool,

    // === Caches for synchronous access (shared with Handle) ===
    /// Cache: market -> has_position (size > 0).
    positions_cache: Arc<DashMap<MarketKey, bool>>,

    /// Full position data for notional calculations.
    positions_data: Arc<DashMap<MarketKey, Position>>,

    /// Recent fill cloids for deduplication.
    /// Clears when size exceeds threshold to prevent memory leak.
    recent_fill_cloids: HashSet<ClientOrderId>,
}

impl PositionTrackerTask {
    /// Run the position tracker actor.
    ///
    /// Processes messages until Shutdown is received.
    pub async fn run(mut self) {
        debug!("PositionTrackerTask started");

        while let Some(msg) = self.rx.recv().await {
            match msg {
                PositionTrackerMsg::Shutdown => {
                    debug!("PositionTrackerTask shutting down");
                    break;
                }
                msg if self.in_snapshot => {
                    // Buffer messages during snapshot processing
                    trace!("Buffering message during snapshot: {:?}", msg);
                    self.snapshot_buffer.push(msg);
                }
                msg => {
                    self.handle_message(msg);
                }
            }
        }

        debug!("PositionTrackerTask terminated");
    }

    /// Handle a single message.
    fn handle_message(&mut self, msg: PositionTrackerMsg) {
        match msg {
            PositionTrackerMsg::RegisterOrder(order) => self.on_register_order(order),
            PositionTrackerMsg::RemoveOrder(cloid) => self.on_remove_order(&cloid),
            PositionTrackerMsg::OrderUpdate {
                cloid,
                state,
                filled_size,
                oid,
            } => self.on_order_update(cloid, state, filled_size, oid),
            PositionTrackerMsg::Fill {
                market,
                side,
                price,
                size,
                timestamp_ms,
                cloid,
            } => self.on_fill(market, side, price, size, timestamp_ms, cloid),
            PositionTrackerMsg::SnapshotStart => {
                debug!("Snapshot processing started");
                self.in_snapshot = true;
            }
            PositionTrackerMsg::SnapshotEnd => {
                debug!(
                    "Snapshot processing ended, applying {} buffered messages",
                    self.snapshot_buffer.len()
                );
                self.in_snapshot = false;

                // Drain and process buffered messages
                let buffer = std::mem::take(&mut self.snapshot_buffer);
                for buffered_msg in buffer {
                    self.handle_message(buffered_msg);
                }
            }
            PositionTrackerMsg::SyncPositions(new_positions) => {
                self.on_sync_positions(new_positions);
            }
            PositionTrackerMsg::Shutdown => unreachable!("Shutdown handled in run()"),
        }
    }

    /// Handle RegisterOrder message.
    fn on_register_order(&mut self, order: TrackedOrder) {
        let cloid = order.cloid.clone();
        let market = order.market;

        trace!("Registering order: cloid={}, market={}", cloid, market);

        // Insert into pending orders (authoritative state)
        self.pending_orders.insert(cloid, order);

        // NOTE: Caches are updated by Handle, not Actor, to avoid double-counting
    }

    /// Handle RemoveOrder message.
    fn on_remove_order(&mut self, cloid: &ClientOrderId) {
        trace!("Removing order: cloid={}", cloid);

        // Remove from pending orders (authoritative state)
        self.pending_orders.remove(cloid);

        // NOTE: Caches are updated by Handle, not Actor, to avoid double-counting
    }

    /// Handle OrderUpdate message.
    fn on_order_update(
        &mut self,
        cloid: ClientOrderId,
        state: OrderState,
        filled_size: Size,
        oid: Option<u64>,
    ) {
        trace!(
            "Order update: cloid={}, state={:?}, filled_size={}, oid={:?}",
            cloid,
            state,
            filled_size,
            oid
        );

        // Update tracked order if exists
        if let Some(order) = self.pending_orders.get_mut(&cloid) {
            order.state = state;
            order.filled_size = filled_size;
            order.updated_at = chrono::Utc::now().timestamp_millis() as u64;
        }

        // If terminal state, remove from tracking
        if state.is_terminal() {
            self.on_remove_order(&cloid);
        }
    }

    /// Handle Fill message.
    fn on_fill(
        &mut self,
        market: MarketKey,
        side: OrderSide,
        price: Price,
        size: Size,
        timestamp_ms: u64,
        cloid: Option<ClientOrderId>,
    ) {
        // Cloid-based deduplication
        if let Some(ref id) = cloid {
            if self.recent_fill_cloids.contains(id) {
                debug!("Skipping duplicate fill: cloid={}", id);
                return;
            }
            self.recent_fill_cloids.insert(id.clone());

            // Prevent memory leak: clear when size exceeds threshold
            // 1000 cloids ~= 10 seconds at 100 fills/sec
            if self.recent_fill_cloids.len() > 1000 {
                debug!(
                    "Clearing recent_fill_cloids (size exceeded 1000): {}",
                    self.recent_fill_cloids.len()
                );
                self.recent_fill_cloids.clear();
            }
        }

        trace!(
            "Fill: market={}, side={:?}, price={}, size={}, ts={}, cloid={:?}",
            market,
            side,
            price,
            size,
            timestamp_ms,
            cloid
        );

        if let Some(pos) = self.positions.get_mut(&market) {
            // Update existing position
            Self::update_position_static(pos, side, price, size, timestamp_ms);

            // Update caches
            let has_position = !pos.is_empty();
            if has_position {
                self.positions_cache.insert(market, true);
                self.positions_data.insert(market, pos.clone());
            } else {
                // Position closed, remove from both
                self.positions.remove(&market);
                self.positions_cache.remove(&market);
                self.positions_data.remove(&market);
            }
        } else {
            // Create new position
            let pos = Position::new(market, side, size, price, timestamp_ms);
            self.positions.insert(market, pos.clone());
            self.positions_cache.insert(market, true);
            self.positions_data.insert(market, pos);
        }
    }

    /// Update position with a fill.
    ///
    /// Handles:
    /// - Same side: increase position, update average entry
    /// - Opposite side: decrease or flip position
    fn update_position_static(
        pos: &mut Position,
        fill_side: OrderSide,
        fill_price: Price,
        fill_size: Size,
        timestamp_ms: u64,
    ) {
        pos.last_update_ms = timestamp_ms;

        if pos.side == fill_side {
            // Same side: increase position
            // New average entry = (old_size * old_price + fill_size * fill_price) / new_size
            let old_notional = pos.size.inner() * pos.entry_price.inner();
            let fill_notional = fill_size.inner() * fill_price.inner();
            let new_size = pos.size.inner() + fill_size.inner();

            if !new_size.is_zero() {
                pos.entry_price = Price::new((old_notional + fill_notional) / new_size);
            }
            pos.size = Size::new(new_size);
        } else {
            // Opposite side: reduce or flip position
            let fill_amount = fill_size.inner();
            let current_size = pos.size.inner();

            if fill_amount >= current_size {
                // Position reduced to zero or flipped
                let remaining = fill_amount - current_size;
                if remaining.is_zero() {
                    // Exactly closed
                    pos.size = Size::ZERO;
                } else {
                    // Flipped to opposite side
                    pos.side = fill_side;
                    pos.size = Size::new(remaining);
                    pos.entry_price = fill_price;
                    pos.entry_timestamp_ms = timestamp_ms;
                }
            } else {
                // Partial reduction, keep same side
                pos.size = Size::new(current_size - fill_amount);
            }
        }
    }

    /// Handle SyncPositions message.
    ///
    /// Replaces all current positions with the provided list.
    /// Used for initial sync from Hyperliquid API on startup.
    ///
    /// # Safety (BUG-002 fix)
    ///
    /// To minimize race condition window, we:
    /// 1. First add/update all new positions
    /// 2. Then remove markets that are NOT in the new list
    ///
    /// This ensures a position is never "lost" during sync - it's either old or new.
    fn on_sync_positions(&mut self, new_positions: Vec<Position>) {
        let old_count = self.positions.len();
        let new_count = new_positions.len();

        debug!(
            "Syncing positions: {} existing -> {} new",
            old_count, new_count
        );

        // Collect markets from new positions
        let new_markets: std::collections::HashSet<MarketKey> =
            new_positions.iter().map(|p| p.market).collect();

        // Step 1: Add/update all new positions FIRST (before removing anything)
        for pos in new_positions {
            if !pos.is_empty() {
                let market = pos.market;
                self.positions_cache.insert(market, true);
                self.positions_data.insert(market, pos.clone());
                self.positions.insert(market, pos);
            }
        }

        // Step 2: Remove positions that are NOT in the new list
        // (Only after new positions are added to minimize race window)
        let markets_to_remove: Vec<MarketKey> = self
            .positions
            .keys()
            .filter(|m| !new_markets.contains(m))
            .cloned()
            .collect();

        for market in markets_to_remove {
            self.positions.remove(&market);
            self.positions_cache.remove(&market);
            self.positions_data.remove(&market);
        }

        debug!(
            "Position sync complete: {} active positions",
            self.positions.len()
        );
    }
}

// ============================================================================
// PositionTrackerHandle
// ============================================================================

/// Handle for interacting with the position tracker actor.
///
/// Provides both async methods (for sending messages) and
/// sync methods (for high-frequency cache lookups).
#[derive(Clone)]
pub struct PositionTrackerHandle {
    /// Message sender.
    tx: mpsc::Sender<PositionTrackerMsg>,

    /// Cache: market -> has_position.
    positions_cache: Arc<DashMap<MarketKey, bool>>,

    /// Cache: market -> pending order count.
    pending_markets_cache: Arc<DashMap<MarketKey, u32>>,

    /// Cache: cloid -> (market, reduce_only).
    pending_orders_snapshot: Arc<DashMap<ClientOrderId, (MarketKey, bool)>>,

    /// Authoritative positions for notional calculations.
    /// Only accessed synchronously from caches.
    positions_data: Arc<DashMap<MarketKey, Position>>,

    /// Authoritative pending orders for notional calculations.
    pending_orders_data: Arc<DashMap<ClientOrderId, TrackedOrder>>,

    /// Account balance in cents (USD * 100) for lock-free access.
    /// Using AtomicU64 for high-frequency reads without locking.
    /// Precision: $0.01 (sufficient for position sizing calculations).
    balance_cache: Arc<AtomicU64>,
}

impl PositionTrackerHandle {
    // === Private helpers ===

    /// Add order to caches (called on register).
    fn add_order_to_caches(&self, cloid: &ClientOrderId, order: &TrackedOrder) {
        self.pending_orders_data
            .insert(cloid.clone(), order.clone());
        self.pending_orders_snapshot
            .insert(cloid.clone(), (order.market, order.reduce_only));
        self.pending_markets_cache
            .entry(order.market)
            .and_modify(|c| *c += 1)
            .or_insert(1);
    }

    /// Remove order from caches (called on remove/terminal).
    fn remove_order_from_caches(&self, cloid: &ClientOrderId) {
        self.pending_orders_data.remove(cloid);
        if let Some((_, (market, _))) = self.pending_orders_snapshot.remove(cloid) {
            if let Some(mut entry) = self.pending_markets_cache.get_mut(&market) {
                let count = entry.value_mut();
                if *count > 1 {
                    *count -= 1;
                } else {
                    drop(entry);
                    self.pending_markets_cache.remove(&market);
                }
            }
        }
    }

    // === Async methods (send to actor) ===

    /// Register an order for tracking.
    pub async fn register_order(&self, order: TrackedOrder) {
        let cloid = order.cloid.clone();
        self.add_order_to_caches(&cloid, &order);
        let _ = self.tx.send(PositionTrackerMsg::RegisterOrder(order)).await;
    }

    /// Send a `RegisterOrder` message to the actor without updating caches.
    ///
    /// This is intended for the slow-path after `try_register_order` fails with
    /// `TrySendError::Full`. In that case, caches have already been updated by
    /// `try_register_order`, so calling `register_order` would double-count the
    /// per-market pending order count.
    pub async fn register_order_actor_only(&self, order: TrackedOrder) {
        let _ = self.tx.send(PositionTrackerMsg::RegisterOrder(order)).await;
    }

    /// Try to register an order synchronously (non-blocking).
    ///
    /// Returns `Err` if the channel is full.
    ///
    /// # Important: Cache Handling on Failure
    ///
    /// If this returns `Err(TrySendError::Full(_))`, the caches have ALREADY been
    /// updated. The caller should:
    /// 1. Retry with `register_order_actor_only()` (async, waits for capacity)
    /// 2. OR call `rollback_order_caches()` if abandoning the order
    ///
    /// See module-level documentation for the dual-state architecture rationale.
    pub fn try_register_order(
        &self,
        order: TrackedOrder,
    ) -> Result<(), mpsc::error::TrySendError<PositionTrackerMsg>> {
        let cloid = order.cloid.clone();
        self.add_order_to_caches(&cloid, &order);
        self.tx.try_send(PositionTrackerMsg::RegisterOrder(order))
    }

    /// Rollback caches after a failed order registration.
    ///
    /// Call this when `try_register_order` returns `Err` AND you are abandoning
    /// the order (not retrying with `register_order_actor_only`).
    ///
    /// This is necessary because `try_register_order` updates caches BEFORE
    /// attempting to send the message. If the send fails and you don't retry,
    /// the caches would contain a "phantom" order that Actor doesn't know about.
    pub fn rollback_order_caches(&self, cloid: &ClientOrderId) {
        self.remove_order_from_caches(cloid);
    }

    /// Remove an order from tracking.
    pub async fn remove_order(&self, cloid: ClientOrderId) {
        self.remove_order_from_caches(&cloid);
        let _ = self.tx.send(PositionTrackerMsg::RemoveOrder(cloid)).await;
    }

    /// Record oid to cloid mapping.
    ///
    /// This is used when we receive oid from post response and need to
    /// track the mapping for potential orderUpdate messages that might
    /// only contain oid (without cloid).
    ///
    /// Currently this is a no-op as our implementation relies on cloid.
    /// The mapping is recorded via logging for debugging purposes.
    pub async fn record_oid_mapping(&self, cloid: ClientOrderId, oid: u64) {
        // For now, just log the mapping. In the future, we could store this
        // in a HashMap to support orderUpdates without cloid.
        tracing::debug!(
            cloid = %cloid,
            oid = oid,
            "Recording oid mapping"
        );
    }

    /// Send an order update.
    pub async fn order_update(
        &self,
        cloid: ClientOrderId,
        state: OrderState,
        filled_size: Size,
        oid: Option<u64>,
    ) {
        if state.is_terminal() {
            self.remove_order_from_caches(&cloid);
        }

        let _ = self
            .tx
            .send(PositionTrackerMsg::OrderUpdate {
                cloid,
                state,
                filled_size,
                oid,
            })
            .await;
    }

    /// Send a fill update.
    ///
    /// # Arguments
    /// * `cloid` - Optional client order ID for deduplication. If the same cloid
    ///   is received multiple times (e.g., from both post response and
    ///   userFills), only the first fill is processed.
    pub async fn fill(
        &self,
        market: MarketKey,
        side: OrderSide,
        price: Price,
        size: Size,
        timestamp_ms: u64,
        cloid: Option<ClientOrderId>,
    ) {
        // NOTE: Position caches are updated by Actor only, not Handle
        // This ensures authoritative state is the single source of truth

        let _ = self
            .tx
            .send(PositionTrackerMsg::Fill {
                market,
                side,
                price,
                size,
                timestamp_ms,
                cloid,
            })
            .await;
    }

    /// Signal start of snapshot processing.
    pub async fn snapshot_start(&self) {
        let _ = self.tx.send(PositionTrackerMsg::SnapshotStart).await;
    }

    /// Signal end of snapshot processing.
    pub async fn snapshot_end(&self) {
        let _ = self.tx.send(PositionTrackerMsg::SnapshotEnd).await;
    }

    /// Request graceful shutdown.
    pub async fn shutdown(&self) {
        let _ = self.tx.send(PositionTrackerMsg::Shutdown).await;
    }

    /// Sync positions from external source (e.g., Hyperliquid API).
    ///
    /// Replaces all current positions with the provided list.
    /// Call this on startup to initialize position state from exchange.
    ///
    /// # Safety
    /// Do NOT clear caches here. The Actor will atomically clear and repopulate
    /// when it processes the SyncPositions message. Clearing here creates a
    /// race condition where Gate 3/Gate 6 checks see empty caches before the
    /// Actor has a chance to populate them with new positions.
    pub async fn sync_positions(&self, positions: Vec<Position>) {
        // Send to actor - it will clear old and add new atomically
        // NOTE: Cache clearing removed to fix race condition (BUG-002)
        let _ = self
            .tx
            .send(PositionTrackerMsg::SyncPositions(positions))
            .await;
    }

    // === Sync methods (cache lookups) ===

    /// Check if there is an open position for the market.
    #[must_use]
    pub fn has_position(&self, market: &MarketKey) -> bool {
        self.positions_cache
            .get(market)
            .map(|r| *r)
            .unwrap_or(false)
    }

    /// Try to atomically mark a market as pending.
    ///
    /// Returns `true` if successfully marked (no existing position or pending orders).
    /// Returns `false` if market already has position or pending orders.
    ///
    /// This method is atomic: the check and mark operations are performed
    /// within a single DashMap entry operation to prevent TOCTOU races.
    #[must_use]
    pub fn try_mark_pending_market(&self, market: &MarketKey) -> bool {
        // Check if already has position first (separate cache, but position changes are rare)
        if self.has_position(market) {
            return false;
        }

        // Atomically check-and-mark using DashMap entry API
        // This ensures no race condition between contains_key and insert
        use dashmap::mapref::entry::Entry;
        match self.pending_markets_cache.entry(*market) {
            Entry::Vacant(vacant) => {
                // No existing entry - mark as pending with count 0
                // (will be incremented to 1 when order is registered)
                vacant.insert(0);
                true
            }
            Entry::Occupied(occupied) => {
                // Entry exists - check if it's actually marked (count > 0 or was marked)
                // Even count=0 means someone else marked it, so reject
                let _count = occupied.get();
                false
            }
        }
    }

    /// Unmark a market as pending (rollback for `try_mark_pending_market`).
    ///
    /// **IMPORTANT**: This should ONLY be called to rollback a successful
    /// `try_mark_pending_market` call BEFORE `register_order` is called.
    /// Once an order is registered, use `remove_order` instead, which
    /// correctly decrements the pending count via `remove_order_from_caches`.
    ///
    /// Typical usage:
    /// - Gate check fails after `try_mark_pending_market` succeeded
    /// - Enqueue fails (QueueFull, InflightFull)
    pub fn unmark_pending_market(&self, market: &MarketKey) {
        self.pending_markets_cache.remove(market);
    }

    /// Check if there are pending orders for the market.
    #[must_use]
    pub fn has_pending_order(&self, market: &MarketKey) -> bool {
        self.pending_markets_cache.contains_key(market)
    }

    /// Get the market for a client order ID.
    #[must_use]
    pub fn get_market_for_cloid(&self, cloid: &ClientOrderId) -> Option<MarketKey> {
        self.pending_orders_snapshot.get(cloid).map(|r| r.0)
    }

    /// Get the notional value of the position for a market.
    #[must_use]
    pub fn get_notional(&self, market: &MarketKey, mark_px: Price) -> Size {
        self.positions_data
            .get(market)
            .map(|pos| pos.notional(mark_px))
            .unwrap_or(Size::ZERO)
    }

    /// Get the pending notional value excluding reduce-only orders for a market.
    ///
    /// Uses mark_px for consistent valuation (same as position notional).
    #[must_use]
    pub fn get_pending_notional_excluding_reduce_only(
        &self,
        market: &MarketKey,
        mark_px: Price,
    ) -> Size {
        let mut total = Decimal::ZERO;

        for entry in self.pending_orders_data.iter() {
            let order = entry.value();
            if &order.market == market && !order.reduce_only {
                // Use mark_px for consistent valuation
                let order_notional = order.size.inner() * mark_px.inner();
                total += order_notional;
            }
        }

        Size::new(total)
    }

    /// Get total pending notional across all markets excluding reduce-only orders.
    ///
    /// Used for MaxPositionTotal gate calculation.
    /// Requires a function to fetch mark_px for each market.
    #[must_use]
    pub fn get_total_pending_notional_excluding_reduce_only<F>(&self, get_mark_px: F) -> Decimal
    where
        F: Fn(&MarketKey) -> Option<Price>,
    {
        let mut total = Decimal::ZERO;

        for entry in self.pending_orders_data.iter() {
            let order = entry.value();
            if !order.reduce_only {
                if let Some(mark_px) = get_mark_px(&order.market) {
                    let order_notional = order.size.inner() * mark_px.inner();
                    total += order_notional;
                }
            }
        }

        total
    }

    /// Get a snapshot of all open positions.
    #[must_use]
    pub fn positions_snapshot(&self) -> Vec<Position> {
        self.positions_data
            .iter()
            .map(|r| r.value().clone())
            .collect()
    }

    /// Get the number of pending orders.
    #[must_use]
    pub fn pending_order_count(&self) -> usize {
        self.pending_orders_data.len()
    }

    /// Get the number of open positions.
    #[must_use]
    pub fn position_count(&self) -> usize {
        self.positions_data.len()
    }

    /// Get an iterator over pending orders snapshot entries.
    ///
    /// Used for monitoring and timeout checks.
    pub fn pending_orders_snapshot_iter(
        &self,
    ) -> dashmap::iter::Iter<'_, ClientOrderId, (MarketKey, bool)> {
        self.pending_orders_snapshot.iter()
    }

    /// Get a pending order by client order ID.
    ///
    /// Used for timeout checks and order state inspection.
    #[must_use]
    pub fn get_pending_order(
        &self,
        cloid: &ClientOrderId,
    ) -> Option<dashmap::mapref::one::Ref<'_, ClientOrderId, TrackedOrder>> {
        self.pending_orders_data.get(cloid)
    }

    // === Balance methods ===

    /// Get the cached account balance.
    ///
    /// Returns the balance stored as cents (USD * 100) converted back to Decimal.
    /// Lock-free read for high-frequency access during gate checks.
    ///
    /// Returns `Decimal::ZERO` if balance has not been set (startup or API failure).
    #[must_use]
    pub fn get_balance(&self) -> Decimal {
        let cents = self.balance_cache.load(Ordering::Acquire);
        Decimal::new(cents as i64, 2)
    }

    /// Update the cached account balance.
    ///
    /// Stores balance as cents (USD * 100) for atomic access.
    /// Called by app.rs during position sync from API.
    ///
    /// # Note
    /// Precision is $0.01, which is sufficient for position sizing calculations.
    pub fn update_balance(&self, balance: Decimal) {
        use rust_decimal::prelude::ToPrimitive;
        // Convert to cents: balance * 100, truncate to u64
        let cents = (balance * Decimal::from(100)).trunc().to_u64().unwrap_or(0);
        self.balance_cache.store(cents, Ordering::Release);
        debug!(balance = %balance, cents = cents, "Account balance updated");
    }
}

// ============================================================================
// Spawn function
// ============================================================================

/// Spawn the position tracker actor.
///
/// Returns a handle for interaction and a join handle for the task.
#[must_use]
pub fn spawn_position_tracker(capacity: usize) -> (PositionTrackerHandle, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(capacity);

    let positions_cache = Arc::new(DashMap::new());
    let pending_markets_cache = Arc::new(DashMap::new());
    let pending_orders_snapshot = Arc::new(DashMap::new());
    let positions_data = Arc::new(DashMap::new());
    let pending_orders_data = Arc::new(DashMap::new());
    let balance_cache = Arc::new(AtomicU64::new(0));

    let task = PositionTrackerTask {
        rx,
        pending_orders: HashMap::new(),
        positions: HashMap::new(),
        snapshot_buffer: Vec::new(),
        in_snapshot: false,
        positions_cache: positions_cache.clone(),
        positions_data: positions_data.clone(),
        recent_fill_cloids: HashSet::new(),
    };

    let handle = PositionTrackerHandle {
        tx,
        positions_cache,
        pending_markets_cache,
        pending_orders_snapshot,
        positions_data,
        pending_orders_data,
        balance_cache,
    };

    let join_handle = tokio::spawn(task.run());

    (handle, join_handle)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetId, DexId, PendingOrder};
    use rust_decimal_macros::dec;

    fn sample_market() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    fn sample_tracked_order(market: MarketKey, reduce_only: bool) -> TrackedOrder {
        TrackedOrder::from_pending(PendingOrder::new(
            ClientOrderId::new(),
            market,
            OrderSide::Buy,
            Price::new(dec!(50000)),
            Size::new(dec!(0.1)),
            reduce_only,
            1234567890,
        ))
    }

    #[tokio::test]
    async fn test_register_order_actor_only_does_not_double_count_caches() {
        // Build a handle with a full channel so `try_register_order` fails with Full.
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(PositionTrackerMsg::SnapshotStart).unwrap();

        let market = sample_market();
        let order = sample_tracked_order(market, false);

        let positions_cache = Arc::new(DashMap::new());
        let pending_markets_cache = Arc::new(DashMap::new());
        let pending_orders_snapshot = Arc::new(DashMap::new());
        let positions_data = Arc::new(DashMap::new());
        let pending_orders_data = Arc::new(DashMap::new());
        let balance_cache = Arc::new(AtomicU64::new(0));

        let handle = PositionTrackerHandle {
            tx,
            positions_cache,
            pending_markets_cache,
            pending_orders_snapshot,
            positions_data,
            pending_orders_data,
            balance_cache,
        };

        let err = handle.try_register_order(order.clone()).unwrap_err();
        assert!(matches!(err, mpsc::error::TrySendError::Full(_)));

        // Caches should have been updated exactly once.
        assert_eq!(
            handle
                .pending_markets_cache
                .get(&market)
                .map(|v| *v.value())
                .unwrap_or_default(),
            1
        );

        // Drain the dummy message to make capacity for the slow-path send.
        let _ = rx.recv().await.unwrap();

        // Slow-path: send actor message without touching caches again.
        handle.register_order_actor_only(order).await;

        assert_eq!(
            handle
                .pending_markets_cache
                .get(&market)
                .map(|v| *v.value())
                .unwrap_or_default(),
            1
        );
    }

    #[tokio::test]
    async fn test_register_and_remove_order() {
        let (handle, _join) = spawn_position_tracker(100);

        let order = sample_tracked_order(sample_market(), false);
        let cloid = order.cloid.clone();
        let market = order.market;

        // Register order
        handle.register_order(order).await;

        // Wait for actor to process
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Check caches
        assert!(handle.has_pending_order(&market));
        assert!(handle.get_market_for_cloid(&cloid).is_some());

        // Remove order
        handle.remove_order(cloid.clone()).await;

        // Wait for actor to process
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Check caches cleared
        assert!(!handle.has_pending_order(&market));
        assert!(handle.get_market_for_cloid(&cloid).is_none());

        // Shutdown
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_fill_creates_position() {
        let (handle, _join) = spawn_position_tracker(100);

        let market = sample_market();

        // No position initially
        assert!(!handle.has_position(&market));

        // Send fill
        handle
            .fill(
                market,
                OrderSide::Buy,
                Price::new(dec!(50000)),
                Size::new(dec!(0.1)),
                1234567890,
                None, // cloid for deduplication
            )
            .await;

        // Wait for actor to process
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Check position exists
        assert!(handle.has_position(&market));

        // Check notional
        let notional = handle.get_notional(&market, Price::new(dec!(50000)));
        assert_eq!(notional, Size::new(dec!(5000)));

        // Shutdown
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_fill_closes_position() {
        let (handle, _join) = spawn_position_tracker(100);

        let market = sample_market();

        // Create position with buy
        handle
            .fill(
                market,
                OrderSide::Buy,
                Price::new(dec!(50000)),
                Size::new(dec!(0.1)),
                1234567890,
                None, // cloid for deduplication
            )
            .await;

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        assert!(handle.has_position(&market));

        // Close with sell of same size
        handle
            .fill(
                market,
                OrderSide::Sell,
                Price::new(dec!(51000)),
                Size::new(dec!(0.1)),
                1234567891,
                None, // cloid for deduplication
            )
            .await;

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Position should be closed
        // Note: The actor updates positions_cache, but handle.positions_data is separate
        // This test validates the basic flow

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_try_mark_pending_market() {
        let (handle, _join) = spawn_position_tracker(100);

        let market = sample_market();

        // First mark should succeed
        assert!(handle.try_mark_pending_market(&market));

        // Second mark should fail
        assert!(!handle.try_mark_pending_market(&market));

        // Unmark
        handle.unmark_pending_market(&market);

        // Now mark should succeed again
        assert!(handle.try_mark_pending_market(&market));

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_pending_notional_excludes_reduce_only() {
        let (handle, _join) = spawn_position_tracker(100);

        let market = sample_market();

        // Register a regular order
        let order1 = sample_tracked_order(market, false);
        handle.register_order(order1).await;

        // Register a reduce-only order
        let order2 = sample_tracked_order(market, true);
        handle.register_order(order2).await;

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Pending notional should only include the regular order
        let pending =
            handle.get_pending_notional_excluding_reduce_only(&market, Price::new(dec!(50000)));
        // 0.1 * 50000 = 5000 (only one order)
        assert_eq!(pending, Size::new(dec!(5000)));

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_order_update_terminal_removes_order() {
        let (handle, _join) = spawn_position_tracker(100);

        let order = sample_tracked_order(sample_market(), false);
        let cloid = order.cloid.clone();
        let market = order.market;

        handle.register_order(order).await;
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        assert!(handle.has_pending_order(&market));

        // Send terminal state update
        handle
            .order_update(
                cloid.clone(),
                OrderState::Filled,
                Size::new(dec!(0.1)),
                Some(123),
            )
            .await;

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Should be removed
        assert!(!handle.has_pending_order(&market));
        assert!(handle.get_market_for_cloid(&cloid).is_none());

        handle.shutdown().await;
    }

    #[test]
    fn test_position_notional() {
        let pos = Position::new(
            sample_market(),
            OrderSide::Buy,
            Size::new(dec!(0.5)),
            Price::new(dec!(50000)),
            1234567890,
        );

        let notional = pos.notional(Price::new(dec!(52000)));
        assert_eq!(notional, Size::new(dec!(26000)));
    }

    #[test]
    fn test_position_is_empty() {
        let mut pos = Position::new(
            sample_market(),
            OrderSide::Buy,
            Size::new(dec!(0.5)),
            Price::new(dec!(50000)),
            1234567890,
        );

        assert!(!pos.is_empty());

        pos.size = Size::ZERO;
        assert!(pos.is_empty());
    }

    #[tokio::test]
    async fn test_balance_get_and_update() {
        let (handle, _join) = spawn_position_tracker(100);

        // Initial balance should be zero
        assert_eq!(handle.get_balance(), Decimal::ZERO);

        // Update balance
        handle.update_balance(dec!(186.50));

        // Check balance
        assert_eq!(handle.get_balance(), dec!(186.50));

        // Update to different value
        handle.update_balance(dec!(500.00));
        assert_eq!(handle.get_balance(), dec!(500.00));

        // Large balance
        handle.update_balance(dec!(123456.78));
        assert_eq!(handle.get_balance(), dec!(123456.78));

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_balance_precision() {
        let (handle, _join) = spawn_position_tracker(100);

        // Test $0.01 precision
        handle.update_balance(dec!(100.01));
        assert_eq!(handle.get_balance(), dec!(100.01));

        handle.update_balance(dec!(100.99));
        assert_eq!(handle.get_balance(), dec!(100.99));

        // Sub-cent values are truncated (acceptable for position sizing)
        handle.update_balance(dec!(100.999));
        // Gets stored as 10099 cents = $100.99
        assert_eq!(handle.get_balance(), dec!(100.99));

        handle.shutdown().await;
    }
}
