//! Executor loop for periodic batch processing.
//!
//! Implements the 100ms tick loop that:
//! - Handles request timeouts
//! - Collects batches from the scheduler
//! - Applies HardStop filtering
//! - Signs and sends orders

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::Address;
use dashmap::DashMap;
use tokio::sync::oneshot;
use tracing::{debug, info, trace, warn};

use crate::error::ExecutorError;
use crate::executor::Executor;
use crate::nonce::{NonceManager, SystemClock};
use crate::signer::{Action, CancelWire, OrderWire, Signer, SigningInput};
use crate::ws_sender::{ActionSignature, DynWsSender, SendResult, SignedAction};
use hip3_core::{ActionBatch, ClientOrderId, MarketKey, OrderState, PendingOrder, Price, Size};
use hip3_registry::SpecCache;
use hip3_ws::OrderResponseStatus;

// ============================================================================
// PostResult
// ============================================================================

/// Result of a post request.
#[derive(Debug, Clone)]
pub enum PostResult {
    /// Request completed successfully.
    Ok {
        /// Post ID.
        post_id: u64,
    },
    /// Request was rejected.
    Rejected {
        /// Post ID.
        post_id: u64,
        /// Rejection reason.
        reason: String,
    },
    /// Request timed out.
    Timeout {
        /// Post ID.
        post_id: u64,
    },
}

// ============================================================================
// PendingRequest
// ============================================================================

/// A pending request awaiting response.
pub struct PendingRequest {
    /// Unique post ID.
    pub post_id: u64,
    /// The batch that was sent.
    pub batch: ActionBatch,
    /// When the request was created (Unix milliseconds).
    pub sent_at: u64,
    /// Whether the request has been sent (vs just created).
    pub sent: bool,
    /// Channel to notify completion (consumed on first use).
    pub tx: Option<oneshot::Sender<PostResult>>,
}

impl std::fmt::Debug for PendingRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingRequest")
            .field("post_id", &self.post_id)
            .field("sent_at", &self.sent_at)
            .field("sent", &self.sent)
            .field("has_tx", &self.tx.is_some())
            .finish()
    }
}

// ============================================================================
// PostRequestManager
// ============================================================================

/// Manages pending post requests and tracks timeouts.
#[derive(Debug)]
pub struct PostRequestManager {
    /// Pending requests by post ID.
    pending: DashMap<u64, PendingRequest>,
    /// Next post ID to assign.
    next_post_id: AtomicU64,
    /// Timeout duration in milliseconds.
    timeout_ms: u64,
}

impl PostRequestManager {
    /// Create a new post request manager.
    ///
    /// # Arguments
    /// * `timeout_ms` - Timeout duration in milliseconds (default: 5000ms)
    #[must_use]
    pub fn new(timeout_ms: u64) -> Self {
        Self {
            pending: DashMap::new(),
            next_post_id: AtomicU64::new(1),
            timeout_ms,
        }
    }

    /// Create a new pending request.
    ///
    /// Returns the post ID and a receiver for the result.
    pub fn create_request(
        &self,
        batch: ActionBatch,
        now_ms: u64,
    ) -> (u64, oneshot::Receiver<PostResult>) {
        let post_id = self.next_post_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();

        let request = PendingRequest {
            post_id,
            batch,
            sent_at: now_ms,
            sent: false,
            tx: Some(tx),
        };

        self.pending.insert(post_id, request);

        (post_id, rx)
    }

    /// Mark a request as sent.
    pub fn mark_sent(&self, post_id: u64, now_ms: u64) {
        if let Some(mut entry) = self.pending.get_mut(&post_id) {
            entry.sent = true;
            entry.sent_at = now_ms;
        }
    }

    /// Complete a request with success.
    pub fn complete_ok(&self, post_id: u64) {
        if let Some((_, mut request)) = self.pending.remove(&post_id) {
            if let Some(tx) = request.tx.take() {
                let _ = tx.send(PostResult::Ok { post_id });
            }
        }
    }

    /// Complete a request with rejection.
    pub fn complete_rejected(&self, post_id: u64, reason: String) {
        if let Some((_, mut request)) = self.pending.remove(&post_id) {
            if let Some(tx) = request.tx.take() {
                let _ = tx.send(PostResult::Rejected { post_id, reason });
            }
        }
    }

    /// Check for and handle timed out requests.
    ///
    /// Returns a list of (post_id, batch) pairs for timed out requests.
    pub fn check_timeouts(&self, now_ms: u64) -> Vec<(u64, ActionBatch)> {
        let mut timed_out = Vec::new();

        // Collect timed out requests
        let mut to_remove = Vec::new();
        for entry in self.pending.iter() {
            let request = entry.value();
            if request.sent && now_ms.saturating_sub(request.sent_at) >= self.timeout_ms {
                to_remove.push(*entry.key());
            }
        }

        // Remove and notify
        for post_id in to_remove {
            if let Some((_, mut request)) = self.pending.remove(&post_id) {
                if let Some(tx) = request.tx.take() {
                    let _ = tx.send(PostResult::Timeout { post_id });
                }
                timed_out.push((post_id, request.batch));
            }
        }

        timed_out
    }

    /// Get the number of pending requests.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Get a pending request by post ID.
    #[must_use]
    pub fn get(&self, post_id: u64) -> Option<ActionBatch> {
        self.pending.get(&post_id).map(|r| r.batch.clone())
    }

    /// Cancel all pending requests.
    ///
    /// Returns a list of all batches that were pending.
    pub fn cancel_all(&self) -> Vec<ActionBatch> {
        let mut batches = Vec::new();

        for entry in self.pending.iter() {
            batches.push(entry.value().batch.clone());
        }

        self.pending.clear();
        batches
    }

    /// Remove a pending request without notification.
    ///
    /// Used when a send fails and we need to cleanup before retry.
    /// Returns the batch if it was found.
    pub fn remove(&self, post_id: u64) -> Option<ActionBatch> {
        self.pending.remove(&post_id).map(|(_, req)| req.batch)
    }
}

impl Default for PostRequestManager {
    fn default() -> Self {
        Self::new(5000)
    }
}

// ============================================================================
// ExecutorLoop
// ============================================================================

/// The main execution loop.
///
/// Runs on a 100ms tick interval, processing batches from the scheduler
/// and managing request lifecycles.
pub struct ExecutorLoop {
    /// The executor instance.
    executor: Arc<Executor>,
    /// Nonce manager for request signing.
    nonce_manager: Arc<NonceManager<SystemClock>>,
    /// Signer for request authentication.
    signer: Arc<Signer>,
    /// WebSocket sender (optional - if None, tick() will sign but not send).
    ws_sender: Option<DynWsSender>,
    /// Manager for pending requests.
    post_request_manager: PostRequestManager,
    /// Optional vault/active_pool address to include in signing and post payload.
    vault_address: Option<Address>,
    /// Tick interval.
    interval: Duration,
    /// Market spec cache for price/size precision formatting.
    spec_cache: Arc<SpecCache>,
}

impl ExecutorLoop {
    /// Create a new executor loop.
    #[must_use]
    pub fn new(
        executor: Arc<Executor>,
        nonce_manager: Arc<NonceManager<SystemClock>>,
        signer: Arc<Signer>,
        timeout_ms: u64,
        spec_cache: Arc<SpecCache>,
    ) -> Self {
        Self {
            interval: executor.batch_scheduler().interval(),
            executor,
            nonce_manager,
            signer,
            ws_sender: None,
            post_request_manager: PostRequestManager::new(timeout_ms),
            vault_address: None,
            spec_cache,
        }
    }

    /// Create a new executor loop with WebSocket sender.
    #[must_use]
    pub fn with_ws_sender(
        executor: Arc<Executor>,
        nonce_manager: Arc<NonceManager<SystemClock>>,
        signer: Arc<Signer>,
        ws_sender: DynWsSender,
        timeout_ms: u64,
        spec_cache: Arc<SpecCache>,
    ) -> Self {
        Self {
            interval: executor.batch_scheduler().interval(),
            executor,
            nonce_manager,
            signer,
            ws_sender: Some(ws_sender),
            post_request_manager: PostRequestManager::new(timeout_ms),
            vault_address: None,
            spec_cache,
        }
    }

    /// Set the WebSocket sender after construction.
    pub fn set_ws_sender(&mut self, ws_sender: DynWsSender) {
        self.ws_sender = Some(ws_sender);
    }

    /// Set the vault/active_pool address used for signing and post payload.
    pub fn set_vault_address(&mut self, vault_address: Option<Address>) {
        self.vault_address = vault_address;
    }

    /// Get the tick interval.
    #[must_use]
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Process one tick of the execution loop.
    ///
    /// This method:
    /// 1. Checks for and handles timeouts
    /// 2. Collects the next batch from the scheduler
    /// 3. Applies HardStop filtering (drops new orders, keeps reduce_only)
    /// 4. Signs the action and sends via WebSocket
    /// 5. Marks as sent only after successful send
    ///
    /// Returns the post ID if a batch was queued, or None.
    pub async fn tick(&self, now_ms: u64) -> Option<u64> {
        // 1. Handle timeouts
        self.handle_timeouts(now_ms).await;

        // 2. Collect batch from scheduler
        let batch = match self.executor.batch_scheduler().tick() {
            Some(batch) => batch,
            None => return None,
        };

        // 3. Apply HardStop filtering
        let batch = match batch {
            ActionBatch::Orders(orders) => {
                if self.executor.hard_stop_latch().is_triggered() {
                    // Filter to reduce_only orders only
                    let (reduce_only, new_orders): (Vec<_>, Vec<_>) =
                        orders.into_iter().partition(|o| o.reduce_only);

                    // Cleanup new_orders
                    self.cleanup_dropped_orders(new_orders).await;

                    if reduce_only.is_empty() {
                        debug!("HardStop: All orders were new_orders, nothing to send");
                        return None;
                    }

                    debug!(
                        reduce_only_count = reduce_only.len(),
                        "HardStop: Filtered to reduce_only orders"
                    );
                    ActionBatch::Orders(reduce_only)
                } else {
                    ActionBatch::Orders(orders)
                }
            }
            batch => batch,
        };

        // 4. Convert batch to action (may fail if SpecCache not ready)
        let action = match self.batch_to_action(&batch) {
            Ok(action) => action,
            Err(e) => {
                warn!(error = %e, "Failed to build action from batch");
                // SpecCache not ready - no post_id generated yet, so no remove needed
                self.handle_batch_conversion_failure(batch).await;
                return None;
            }
        };

        // 5. Create request (only after action build succeeds)
        let (post_id, _rx) = self
            .post_request_manager
            .create_request(batch.clone(), now_ms);

        // 6. Generate nonce and sign
        let nonce = self.nonce_manager.next();

        let signing_input = SigningInput {
            action: action.clone(),
            nonce,
            vault_address: self.vault_address,
            expires_after: None,
        };

        let signature = match self.signer.sign_action(signing_input).await {
            Ok(sig) => sig,
            Err(e) => {
                warn!(post_id, error = ?e, "Failed to sign action");
                // Cleanup the batch since we can't send
                self.handle_send_failure(post_id, batch).await;
                return None;
            }
        };

        // 7. Create signed action
        // Note: signature.v() returns y_parity (0 or 1), SDK wire requires 27 or 28
        let signed_action = SignedAction {
            action,
            nonce,
            signature: ActionSignature {
                r: format!("0x{}", hex::encode(signature.r().to_be_bytes::<32>())),
                s: format!("0x{}", hex::encode(signature.s().to_be_bytes::<32>())),
                v: 27 + signature.v() as u8, // Convert y_parity (0/1) to recovery id (27/28)
            },
            post_id,
        };

        // 8. Send via WebSocket (if sender is configured)
        if let Some(ref ws_sender) = self.ws_sender {
            if !ws_sender.is_ready() {
                debug!(post_id, "WebSocket not ready, skipping send");
                self.handle_send_failure(post_id, batch).await;
                return None;
            }

            let send_result = ws_sender.send(signed_action).await;

            match send_result {
                SendResult::Sent => {
                    // Only mark as sent after successful transmission
                    self.post_request_manager.mark_sent(post_id, now_ms);
                    self.executor.batch_scheduler().on_batch_sent();
                    trace!(post_id, "Batch sent successfully");
                }
                SendResult::Disconnected | SendResult::RateLimited => {
                    warn!(post_id, result = ?send_result, "Send failed (retryable)");
                    self.handle_send_failure(post_id, batch).await;
                    return None;
                }
                SendResult::Error(e) => {
                    warn!(post_id, error = %e, "Send failed (non-retryable)");
                    self.handle_send_failure(post_id, batch).await;
                    return None;
                }
            }
        } else {
            // No WsSender configured - mark as sent for testing purposes
            trace!(post_id, "No WsSender configured, simulating send");
            self.post_request_manager.mark_sent(post_id, now_ms);
            self.executor.batch_scheduler().on_batch_sent();
        }

        Some(post_id)
    }

    /// Convert an ActionBatch to a signable Action.
    ///
    /// Returns `Err(ExecutorError::MarketSpecNotFound)` if spec is not found
    /// for any order in the batch. In this case, the entire batch fails and
    /// should be handled by `handle_batch_conversion_failure`.
    fn batch_to_action(&self, batch: &ActionBatch) -> Result<Action, ExecutorError> {
        match batch {
            ActionBatch::Orders(orders) => {
                let mut order_wires = Vec::with_capacity(orders.len());
                for order in orders {
                    let spec = self.spec_cache.get(&order.market).ok_or_else(|| {
                        warn!(
                            market = %order.market,
                            cloid = %order.cloid,
                            "MarketSpec not found, failing batch"
                        );
                        ExecutorError::MarketSpecNotFound(order.market)
                    })?;
                    order_wires.push(OrderWire::from_pending_order(order, &spec));
                }

                Ok(Action {
                    action_type: "order".to_string(),
                    orders: Some(order_wires),
                    cancels: None,
                    grouping: Some("na".to_string()),
                    builder: None,
                })
            }
            ActionBatch::Cancels(cancels) => {
                // Cancels do not require MarketSpec
                let cancel_wires: Vec<CancelWire> = cancels
                    .iter()
                    .map(|c| CancelWire {
                        asset: c.market.asset.0,
                        oid: c.oid,
                    })
                    .collect();

                Ok(Action {
                    action_type: "cancel".to_string(),
                    orders: None,
                    cancels: Some(cancel_wires),
                    grouping: None,
                    builder: None,
                })
            }
        }
    }

    /// Handle send failure - cleanup and potentially requeue.
    async fn handle_send_failure(&self, post_id: u64, batch: ActionBatch) {
        // Remove from pending requests
        self.post_request_manager.remove(post_id);

        // Cleanup orders or requeue reduce_only
        match batch {
            ActionBatch::Orders(orders) => {
                // Separate reduce_only from new orders
                let (reduce_only, new_orders): (Vec<_>, Vec<_>) =
                    orders.into_iter().partition(|o| o.reduce_only);

                // Cleanup new orders
                self.cleanup_dropped_orders(new_orders).await;

                // Requeue reduce_only orders (they must be retried)
                for order in reduce_only {
                    debug!(cloid = %order.cloid, "Requeuing reduce_only order after send failure");
                    let _ = self.executor.batch_scheduler().enqueue_reduce_only(order);
                }
            }
            ActionBatch::Cancels(cancels) => {
                // Cancels can be requeued as they are idempotent
                for cancel in cancels {
                    debug!(oid = cancel.oid, "Requeuing cancel after send failure");
                    let _ = self.executor.batch_scheduler().enqueue_cancel(cancel);
                }
            }
        }
    }

    /// Handle batch conversion failure (SpecCache not ready).
    ///
    /// Similar to handle_send_failure but without post_id removal since
    /// the request was never created.
    async fn handle_batch_conversion_failure(&self, batch: ActionBatch) {
        match batch {
            ActionBatch::Orders(orders) => {
                // Separate reduce_only from new orders
                let (reduce_only, new_orders): (Vec<_>, Vec<_>) =
                    orders.into_iter().partition(|o| o.reduce_only);

                // Cleanup new orders (drop from position_tracker)
                self.cleanup_dropped_orders(new_orders).await;

                // Requeue reduce_only orders (they must be retried)
                for order in reduce_only {
                    debug!(
                        cloid = %order.cloid,
                        "Requeuing reduce_only order after batch conversion failure"
                    );
                    let _ = self.executor.batch_scheduler().enqueue_reduce_only(order);
                }
            }
            ActionBatch::Cancels(cancels) => {
                // Cancels are idempotent, requeue all
                for cancel in cancels {
                    debug!(
                        oid = cancel.oid,
                        "Requeuing cancel after batch conversion failure"
                    );
                    let _ = self.executor.batch_scheduler().enqueue_cancel(cancel);
                }
            }
        }
    }

    /// Handle timed out requests.
    ///
    /// For orders: cleanup new_orders, requeue reduce_only (must be retried).
    /// For cancels: requeue all (idempotent operation).
    async fn handle_timeouts(&self, now_ms: u64) {
        let timed_out = self.post_request_manager.check_timeouts(now_ms);

        for (post_id, batch) in timed_out {
            warn!(post_id, "Request timed out");

            // Decrement inflight counter
            self.executor.batch_scheduler().on_batch_complete();

            // Handle batch-specific cleanup with reduce_only requeue
            match batch {
                ActionBatch::Orders(orders) => {
                    // Separate reduce_only from new orders
                    let (reduce_only, new_orders): (Vec<_>, Vec<_>) =
                        orders.into_iter().partition(|o| o.reduce_only);

                    // Cleanup new orders (they can be regenerated from signals)
                    self.cleanup_dropped_orders(new_orders).await;

                    // Requeue reduce_only orders (they MUST be retried for position safety)
                    for order in reduce_only {
                        warn!(
                            post_id,
                            cloid = %order.cloid,
                            market = %order.market,
                            "Requeuing reduce_only order after timeout"
                        );
                        let _ = self.executor.batch_scheduler().enqueue_reduce_only(order);
                    }
                }
                ActionBatch::Cancels(cancels) => {
                    // Requeue all cancels (idempotent operation)
                    for cancel in cancels {
                        debug!(post_id, oid = cancel.oid, "Requeuing cancel after timeout");
                        let _ = self.executor.batch_scheduler().enqueue_cancel(cancel);
                    }
                }
            }
        }
    }

    /// Cleanup orders that were dropped or timed out.
    ///
    /// Uses `remove_order` only, which handles pending_markets_cache count
    /// decrements correctly. Do NOT call `unmark_pending_market` here since
    /// these orders were already registered via `register_order`.
    async fn cleanup_dropped_orders(&self, orders: Vec<PendingOrder>) {
        for order in orders {
            self.executor
                .position_tracker()
                .remove_order(order.cloid)
                .await;
        }
    }

    /// Complete a request with success.
    pub fn on_response_ok(&self, post_id: u64) {
        self.post_request_manager.complete_ok(post_id);
        self.executor.batch_scheduler().on_batch_complete();
    }

    /// Complete a request with success and process order statuses.
    ///
    /// This processes the statuses array from the post response to:
    /// - Mark immediately filled orders as complete
    /// - Mark rejected orders as failed
    /// - Record oid mappings for resting orders
    ///
    /// This is critical for IOC orders that may fill immediately without
    /// a subsequent orderUpdate message from WebSocket.
    pub async fn on_response_with_statuses(
        &self,
        post_id: u64,
        statuses: Vec<OrderResponseStatus>,
    ) {
        // Get the batch for this post_id to map statuses to orders
        let batch = self.post_request_manager.get(post_id);

        if let Some(ActionBatch::Orders(orders)) = batch {
            // Process each status in order (1:1 mapping with orders)
            for (status, order) in statuses.iter().zip(orders.iter()) {
                let cloid = &order.cloid;

                match status {
                    OrderResponseStatus::Filled {
                        oid,
                        total_sz,
                        avg_px,
                    } => {
                        // Order was immediately filled - update tracker
                        debug!(
                            cloid = %cloid,
                            oid = oid,
                            total_sz = %total_sz,
                            avg_px = %avg_px,
                            "Order immediately filled (from post response)"
                        );

                        // 1. Update ORDER state (terminal)
                        self.executor
                            .position_tracker()
                            .order_update(cloid.clone(), OrderState::Filled, order.size, Some(*oid))
                            .await;

                        // 2. Update POSITION state directly (don't rely on userFills)
                        // Parse fill size and price from response
                        let fill_price = avg_px
                            .parse::<rust_decimal::Decimal>()
                            .map(Price::new)
                            .unwrap_or(order.price);
                        let fill_size = total_sz
                            .parse::<rust_decimal::Decimal>()
                            .map(Size::new)
                            .unwrap_or(order.size);

                        info!(
                            cloid = %cloid,
                            market = %order.market,
                            side = ?order.side,
                            fill_price = %fill_price,
                            fill_size = %fill_size,
                            "Position updated from post response fill"
                        );

                        self.executor
                            .position_tracker()
                            .fill(
                                order.market,
                                order.side,
                                fill_price,
                                fill_size,
                                chrono::Utc::now().timestamp_millis() as u64,
                                Some(cloid.clone()), // cloid for deduplication
                                None, // entry_edge_bps (not available from post response)
                            )
                            .await;
                    }
                    OrderResponseStatus::Error { message } => {
                        // Order was rejected - release pending
                        warn!(
                            cloid = %cloid,
                            error = %message,
                            "Order rejected (from post response)"
                        );
                        self.executor
                            .position_tracker()
                            .order_update(cloid.clone(), OrderState::Rejected, order.size, None)
                            .await;
                    }
                    OrderResponseStatus::Resting { oid } => {
                        // Order is on order book - wait for orderUpdate
                        debug!(
                            cloid = %cloid,
                            oid = oid,
                            "Order resting on book (from post response)"
                        );
                        // Record oid mapping for later use
                        self.executor
                            .position_tracker()
                            .record_oid_mapping(cloid.clone(), *oid)
                            .await;
                    }
                    OrderResponseStatus::Success => {
                        // ALO order accepted - OID will arrive via orderUpdate
                        debug!(
                            cloid = %cloid,
                            "ALO order accepted (OID pending via orderUpdate)"
                        );
                    }
                }
            }
        }

        // Complete the request as normal
        self.post_request_manager.complete_ok(post_id);
        self.executor.batch_scheduler().on_batch_complete();
    }

    /// Complete a request with rejection.
    pub fn on_response_rejected(&self, post_id: u64, reason: String) {
        self.post_request_manager.complete_rejected(post_id, reason);
        self.executor.batch_scheduler().on_batch_complete();
    }

    /// Get the post request manager for testing.
    #[must_use]
    pub fn post_request_manager(&self) -> &PostRequestManager {
        &self.post_request_manager
    }

    /// Get the executor.
    #[must_use]
    pub fn executor(&self) -> &Arc<Executor> {
        &self.executor
    }

    /// Get the signer.
    #[must_use]
    pub fn signer(&self) -> &Arc<Signer> {
        &self.signer
    }
}

/// Helper struct for tracking dropped orders during cleanup.
#[derive(Debug)]
pub struct DroppedOrder {
    /// Client order ID.
    pub cloid: ClientOrderId,
    /// Market.
    pub market: MarketKey,
}

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetId, DexId, OrderSide, PendingCancel, Price, Size};
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

    // PostRequestManager tests

    #[test]
    fn test_post_request_manager_create() {
        let manager = PostRequestManager::new(5000);

        let batch = ActionBatch::Orders(vec![sample_pending_order(false)]);
        let (post_id, _rx) = manager.create_request(batch, 1000);

        assert_eq!(post_id, 1);
        assert_eq!(manager.pending_count(), 1);

        // Second request gets next ID
        let batch2 = ActionBatch::Orders(vec![sample_pending_order(false)]);
        let (post_id2, _rx2) = manager.create_request(batch2, 1000);

        assert_eq!(post_id2, 2);
        assert_eq!(manager.pending_count(), 2);
    }

    #[test]
    fn test_post_request_manager_complete_ok() {
        let manager = PostRequestManager::new(5000);

        let batch = ActionBatch::Orders(vec![sample_pending_order(false)]);
        let (post_id, mut rx) = manager.create_request(batch, 1000);

        manager.mark_sent(post_id, 1000);
        manager.complete_ok(post_id);

        assert_eq!(manager.pending_count(), 0);

        // Check result
        let result = rx.try_recv().unwrap();
        assert!(matches!(result, PostResult::Ok { post_id: 1 }));
    }

    #[test]
    fn test_post_request_manager_complete_rejected() {
        let manager = PostRequestManager::new(5000);

        let batch = ActionBatch::Orders(vec![sample_pending_order(false)]);
        let (post_id, mut rx) = manager.create_request(batch, 1000);

        manager.mark_sent(post_id, 1000);
        manager.complete_rejected(post_id, "Rate limited".to_string());

        assert_eq!(manager.pending_count(), 0);

        // Check result
        let result = rx.try_recv().unwrap();
        match result {
            PostResult::Rejected {
                post_id: id,
                reason,
            } => {
                assert_eq!(id, 1);
                assert_eq!(reason, "Rate limited");
            }
            _ => panic!("Expected Rejected"),
        }
    }

    #[test]
    fn test_post_request_manager_timeout() {
        let manager = PostRequestManager::new(5000);

        let batch = ActionBatch::Orders(vec![sample_pending_order(false)]);
        let (post_id, mut rx) = manager.create_request(batch, 1000);

        manager.mark_sent(post_id, 1000);

        // No timeout at 5999ms
        let timed_out = manager.check_timeouts(5999);
        assert!(timed_out.is_empty());
        assert_eq!(manager.pending_count(), 1);

        // Timeout at 6000ms (1000 + 5000)
        let timed_out = manager.check_timeouts(6000);
        assert_eq!(timed_out.len(), 1);
        assert_eq!(timed_out[0].0, post_id);
        assert_eq!(manager.pending_count(), 0);

        // Check result
        let result = rx.try_recv().unwrap();
        assert!(matches!(result, PostResult::Timeout { post_id: 1 }));
    }

    #[test]
    fn test_post_request_manager_timeout_not_sent() {
        let manager = PostRequestManager::new(5000);

        let batch = ActionBatch::Orders(vec![sample_pending_order(false)]);
        let (post_id, _rx) = manager.create_request(batch, 1000);

        // Request not marked as sent - should not timeout
        let timed_out = manager.check_timeouts(10000);
        assert!(timed_out.is_empty());
        assert_eq!(manager.pending_count(), 1);

        // Mark sent
        manager.mark_sent(post_id, 1000);

        // Now should timeout
        let timed_out = manager.check_timeouts(10000);
        assert_eq!(timed_out.len(), 1);
    }

    #[test]
    fn test_post_request_manager_cancel_all() {
        let manager = PostRequestManager::new(5000);

        // Create multiple requests
        let batch1 = ActionBatch::Orders(vec![sample_pending_order(false)]);
        let batch2 = ActionBatch::Cancels(vec![sample_pending_cancel()]);
        let batch3 = ActionBatch::Orders(vec![sample_pending_order(true)]);

        manager.create_request(batch1, 1000);
        manager.create_request(batch2, 1000);
        manager.create_request(batch3, 1000);

        assert_eq!(manager.pending_count(), 3);

        let batches = manager.cancel_all();

        assert_eq!(batches.len(), 3);
        assert_eq!(manager.pending_count(), 0);
    }

    #[test]
    fn test_post_request_manager_get() {
        let manager = PostRequestManager::new(5000);

        let batch = ActionBatch::Orders(vec![sample_pending_order(false)]);
        let (post_id, _rx) = manager.create_request(batch.clone(), 1000);

        let retrieved = manager.get(post_id);
        assert!(retrieved.is_some());

        let non_existent = manager.get(999);
        assert!(non_existent.is_none());
    }
}
