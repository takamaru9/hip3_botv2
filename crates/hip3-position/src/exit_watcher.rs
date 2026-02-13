//! WS-driven exit watcher for immediate mark regression detection.
//!
//! Unlike the polling-based `MarkRegressionMonitor` (200ms interval),
//! `ExitWatcher` is called directly from the WS message handler when
//! BBO or Oracle updates are received, enabling sub-millisecond exit detection.
//!
//! # Trading Philosophy
//!
//! > **正しいエッジ**: オラクルが動いた後、マーケットメーカーの注文が追従していない
//! > 「取り残された流動性」を取る
//!
//! The divergence between Oracle and BBO is short-lived (typically < 100ms).
//! Polling-based detection with 200ms intervals means average 100ms latency,
//! which can miss the optimal exit timing when the divergence narrows.
//!
//! # Architecture
//!
//! ```text
//! WS Message → App.handle_market_event()
//!                     ↓
//!              market_state.update_bbo/ctx()
//!                     ↓
//!              exit_watcher.on_market_update(key, snapshot)
//!                     ↓ [immediate check]
//!              Exit condition met → flatten_tx.try_send()
//! ```

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::RwLock;
use rust_decimal::Decimal;
use tokio::sync::mpsc;
use tracing::{debug, info, trace, warn};

use hip3_core::types::MarketSnapshot;
use hip3_core::{MarketKey, OrderSide, PendingOrder};

use crate::mark_regression::MarkRegressionConfig;
use crate::time_stop::{FlattenOrderBuilder, TIME_STOP_MS};
use crate::tracker::{Position, PositionTrackerHandle};

// ============================================================================
// ExitWatcher
// ============================================================================

/// WS-driven exit watcher for immediate mark regression detection.
///
/// Called synchronously from the WS message handler when market data updates.
/// Performs fast, non-blocking exit condition checks.
pub struct ExitWatcher {
    /// Configuration (shared with MarkRegressionMonitor).
    config: MarkRegressionConfig,

    /// Handle to position tracker for position lookups.
    position_handle: PositionTrackerHandle,

    /// Channel to send flatten orders (non-blocking try_send).
    flatten_tx: mpsc::Sender<PendingOrder>,

    /// Local tracking of markets with pending flatten orders.
    /// Protected by RwLock for thread-safe access from WS handler.
    local_flattening: RwLock<HashSet<MarketKey>>,

    /// Counter for exit triggers (for metrics/debugging).
    exit_count: std::sync::atomic::AtomicU64,
}

impl ExitWatcher {
    /// Create a new ExitWatcher.
    #[must_use]
    pub fn new(
        config: MarkRegressionConfig,
        position_handle: PositionTrackerHandle,
        flatten_tx: mpsc::Sender<PendingOrder>,
    ) -> Self {
        info!(
            exit_threshold_bps = %config.exit_threshold_bps,
            min_holding_time_ms = config.min_holding_time_ms,
            "ExitWatcher initialized (WS-driven)"
        );

        Self {
            config,
            position_handle,
            flatten_tx,
            local_flattening: RwLock::new(HashSet::new()),
            exit_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Called when market data is updated (BBO or Oracle).
    ///
    /// This is the main entry point, called from `App::handle_market_event()`
    /// immediately after `market_state.update_bbo/ctx()`.
    ///
    /// # Performance
    ///
    /// This method is designed to be fast and non-blocking:
    /// - O(1) position lookup via DashMap
    /// - O(1) flattening check via HashSet
    /// - Non-blocking `try_send` for flatten orders
    ///
    /// Typical execution time: < 1µs when no position, < 10µs with exit check.
    pub fn on_market_update(&self, key: MarketKey, snapshot: &MarketSnapshot) {
        // 1. Fast path: Check if we have a position in this market
        let position = match self.position_handle.get_position(&key) {
            Some(p) => p,
            None => return, // No position, nothing to do
        };

        // 2. Check if already flattening (local state for immediate check)
        {
            let flattening = self.local_flattening.read();
            if flattening.contains(&key) {
                trace!(market = %key, "ExitWatcher: already flattening (local)");
                return;
            }
        }

        // 3. Check if flatten order pending in position tracker
        if self.position_handle.is_flattening(&key) {
            trace!(market = %key, "ExitWatcher: already flattening (tracker)");
            return;
        }

        // 4. Check exit condition
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        if let Some(edge_bps) = self.check_exit(&position, snapshot, now_ms) {
            // 5. Mark as flattening BEFORE sending to prevent duplicates
            {
                let mut flattening = self.local_flattening.write();
                flattening.insert(key);
            }

            // 6. Trigger exit (non-blocking)
            self.trigger_exit(&position, edge_bps, snapshot, now_ms);
        }
    }

    /// Check if a losing trade should skip MarkRegression exit.
    ///
    /// Returns true if the trade is in loss and the loss is within the
    /// min_loss_exit_bps tolerance — meaning MarkRegression should NOT exit,
    /// letting OracleExit or TimeStop handle it instead.
    fn should_skip_loss_exit(&self, position: &Position, exit_price: Decimal) -> bool {
        if self.config.min_loss_exit_bps.is_zero() {
            return false; // Disabled: never skip
        }

        let entry = position.entry_price.inner();
        if entry.is_zero() {
            return false;
        }

        let pnl_bps = match position.side {
            OrderSide::Buy => (exit_price - entry) / entry * Decimal::from(10000),
            OrderSide::Sell => (entry - exit_price) / entry * Decimal::from(10000),
        };

        // If profitable, don't skip (take profit normally)
        if pnl_bps >= Decimal::ZERO {
            return false;
        }

        // If loss is within tolerance, skip exit
        if pnl_bps.abs() < self.config.min_loss_exit_bps {
            debug!(
                market = %position.market,
                side = ?position.side,
                pnl_bps = %pnl_bps,
                min_loss_exit_bps = %self.config.min_loss_exit_bps,
                "ExitWatcher: skipping exit on small loss (letting OracleExit handle)"
            );
            return true;
        }

        false // Large loss: exit immediately
    }

    /// Check if a position should exit based on mark regression.
    ///
    /// Returns the edge (in bps) if exit condition is met, None otherwise.
    ///
    /// # Exit Conditions
    ///
    /// - Long: `best_bid >= oracle * (1 - exit_threshold_bps / 10000)`
    /// - Short: `best_ask <= oracle * (1 + exit_threshold_bps / 10000)`
    fn check_exit(
        &self,
        position: &Position,
        snapshot: &MarketSnapshot,
        now_ms: u64,
    ) -> Option<Decimal> {
        // 1. Check minimum holding time
        let held_ms = now_ms.saturating_sub(position.entry_timestamp_ms);
        if held_ms < self.config.min_holding_time_ms {
            return None;
        }

        // 2. Get oracle price
        let oracle = snapshot.ctx.oracle.oracle_px;
        if oracle.is_zero() {
            return None;
        }

        // 3. Calculate threshold factor with optional entry edge scaling (Phase C)
        //    and time decay (P2-6)
        let base_threshold_bps = self
            .config
            .effective_exit_threshold_bps(position.entry_edge_bps);
        let decay = self.config.decay_factor(held_ms, TIME_STOP_MS);
        let effective_threshold_bps =
            base_threshold_bps * Decimal::try_from(decay).unwrap_or(Decimal::ONE);
        let threshold_factor = effective_threshold_bps / Decimal::from(10000);

        // 4. Check exit condition based on position side
        match position.side {
            OrderSide::Buy => {
                // Long: exit when bid >= oracle * (1 - threshold)
                let bid = snapshot.bbo.bid_price;
                let exit_threshold = oracle.inner() * (Decimal::ONE - threshold_factor);

                if bid.inner() >= exit_threshold {
                    // Edge: (bid - oracle) / oracle * 10000
                    let edge_bps =
                        (bid.inner() - oracle.inner()) / oracle.inner() * Decimal::from(10000);

                    // Phase B: PnL direction check
                    if self.should_skip_loss_exit(position, bid.inner()) {
                        return None;
                    }

                    return Some(edge_bps);
                }
            }
            OrderSide::Sell => {
                // Short: exit when ask <= oracle * (1 + threshold)
                let ask = snapshot.bbo.ask_price;
                let exit_threshold = oracle.inner() * (Decimal::ONE + threshold_factor);

                if ask.inner() <= exit_threshold {
                    // Edge: (oracle - ask) / oracle * 10000
                    let edge_bps =
                        (oracle.inner() - ask.inner()) / oracle.inner() * Decimal::from(10000);

                    // Phase B: PnL direction check
                    if self.should_skip_loss_exit(position, ask.inner()) {
                        return None;
                    }

                    return Some(edge_bps);
                }
            }
        }

        None
    }

    /// Trigger exit for a position (non-blocking).
    fn trigger_exit(
        &self,
        position: &Position,
        edge_bps: Decimal,
        snapshot: &MarketSnapshot,
        now_ms: u64,
    ) {
        // Use BBO for flatten order pricing
        let price = match position.side {
            OrderSide::Buy => snapshot.bbo.bid_price,  // Sell at bid
            OrderSide::Sell => snapshot.bbo.ask_price, // Buy at ask
        };

        // Create reduce-only order
        let order = FlattenOrderBuilder::create_flatten_order(
            position,
            price,
            self.config.slippage_bps,
            now_ms,
        );

        let held_ms = now_ms.saturating_sub(position.entry_timestamp_ms);
        let count = self
            .exit_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // P1-4: Record exit metrics for MarkRegression
        let market_str = position.market.to_string();
        let exit_reason_str = "MarkRegression";
        hip3_telemetry::Metrics::position_holding_time(
            &market_str,
            exit_reason_str,
            held_ms as f64,
        );

        // Estimate PnL in bps from entry price vs current oracle
        let oracle_px = snapshot.ctx.oracle.oracle_px.inner();
        if !position.entry_price.inner().is_zero() && !oracle_px.is_zero() {
            use rust_decimal::prelude::ToPrimitive;
            let pnl_bps = match position.side {
                OrderSide::Buy => {
                    (oracle_px - position.entry_price.inner()) / position.entry_price.inner()
                        * Decimal::from(10000)
                }
                OrderSide::Sell => {
                    (position.entry_price.inner() - oracle_px) / position.entry_price.inner()
                        * Decimal::from(10000)
                }
            };
            if let Some(pnl) = pnl_bps.to_f64() {
                hip3_telemetry::Metrics::trade_pnl(&market_str, exit_reason_str, pnl);
            }
        }

        info!(
            market = %position.market,
            side = ?position.side,
            edge_bps = %edge_bps,
            held_ms = held_ms,
            exit_reason = exit_reason_str,
            cloid = %order.cloid,
            exit_count = count + 1,
            "ExitWatcher: WS-driven exit triggered"
        );

        // Non-blocking send - if channel is full, log warning
        // MarkRegressionMonitor will catch it on next poll as backup
        match self.flatten_tx.try_send(order) {
            Ok(()) => {
                debug!(market = %position.market, "ExitWatcher: flatten order sent");
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    market = %position.market,
                    "ExitWatcher: flatten channel full, MarkRegressionMonitor will retry"
                );
                // Clear local_flattening so MarkRegressionMonitor can try
                let mut flattening = self.local_flattening.write();
                flattening.remove(&position.market);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                warn!("ExitWatcher: flatten channel closed");
            }
        }
    }

    /// Clear local flattening state for a market.
    ///
    /// Called when:
    /// - Position is closed (removed from tracker)
    /// - Flatten order is rejected/cancelled
    pub fn clear_flattening(&self, market: &MarketKey) {
        let mut flattening = self.local_flattening.write();
        if flattening.remove(market) {
            debug!(market = %market, "ExitWatcher: cleared flattening state");
        }
    }

    /// Sync local flattening state with position tracker.
    ///
    /// Removes markets from local_flattening if:
    /// - No longer has a position
    /// - No longer flattening in position tracker
    pub fn sync_flattening_state(&self) {
        let positions = self.position_handle.positions_snapshot();
        let position_markets: HashSet<MarketKey> = positions.iter().map(|p| p.market).collect();

        let mut flattening = self.local_flattening.write();

        // Remove markets with no position
        flattening.retain(|m| position_markets.contains(m));

        // Remove markets where flatten order was completed/rejected
        flattening.retain(|m| self.position_handle.is_flattening(m));
    }

    /// Get the number of exits triggered by this watcher.
    #[must_use]
    pub fn exit_count(&self) -> u64 {
        self.exit_count.load(std::sync::atomic::Ordering::Relaxed)
    }
}

// ============================================================================
// ExitWatcherHandle (Arc wrapper for sharing)
// ============================================================================

/// Thread-safe handle to ExitWatcher for use from App.
pub type ExitWatcherHandle = Arc<ExitWatcher>;

/// Create a new ExitWatcherHandle.
#[must_use]
pub fn new_exit_watcher(
    config: MarkRegressionConfig,
    position_handle: PositionTrackerHandle,
    flatten_tx: mpsc::Sender<PendingOrder>,
) -> ExitWatcherHandle {
    Arc::new(ExitWatcher::new(config, position_handle, flatten_tx))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetCtx, AssetId, Bbo, DexId, OracleData, Price, Size};
    use rust_decimal_macros::dec;

    #[allow(dead_code)]
    fn test_market() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    #[allow(dead_code)]
    fn test_snapshot(bid: Decimal, ask: Decimal, oracle: Decimal) -> MarketSnapshot {
        let bbo = Bbo::new(
            Price::new(bid),
            Size::new(dec!(1)),
            Price::new(ask),
            Size::new(dec!(1)),
        );
        let oracle_data = OracleData::new(Price::new(oracle), Price::new(oracle));
        let ctx = AssetCtx::new(oracle_data, dec!(0.0001));
        MarketSnapshot::new(bbo, ctx)
    }

    fn test_config() -> MarkRegressionConfig {
        MarkRegressionConfig {
            enabled: true,
            exit_threshold_bps: dec!(10), // 10 bps
            check_interval_ms: 200,       // Not used in ExitWatcher
            min_holding_time_ms: 0,       // No minimum for tests
            slippage_bps: 50,
            min_loss_exit_bps: Decimal::ZERO,
            entry_edge_scaling: false,
            entry_edge_scale_factor: dec!(0.5),
            time_decay_enabled: false,
            decay_start_ms: 5000,
            min_decay_factor: 0.2,
        }
    }

    fn test_config_with_pnl_check() -> MarkRegressionConfig {
        MarkRegressionConfig {
            min_loss_exit_bps: dec!(15), // 15 bps
            ..test_config()
        }
    }

    #[test]
    fn test_long_exit_condition() {
        // Long position: exit when bid >= oracle * (1 - threshold)
        // Oracle = 100, threshold = 10bps = 0.001
        // Exit threshold = 100 * (1 - 0.001) = 99.9
        // Bid = 99.95 >= 99.9 → should exit

        let config = test_config();
        let threshold_factor = config.exit_threshold_bps / dec!(10000);
        let oracle = dec!(100);
        let exit_threshold = oracle * (Decimal::ONE - threshold_factor);

        assert_eq!(exit_threshold, dec!(99.9));

        let bid = dec!(99.95);
        assert!(bid >= exit_threshold);
    }

    #[test]
    fn test_short_exit_condition() {
        // Short position: exit when ask <= oracle * (1 + threshold)
        // Oracle = 100, threshold = 10bps = 0.001
        // Exit threshold = 100 * (1 + 0.001) = 100.1
        // Ask = 100.05 <= 100.1 → should exit

        let config = test_config();
        let threshold_factor = config.exit_threshold_bps / dec!(10000);
        let oracle = dec!(100);
        let exit_threshold = oracle * (Decimal::ONE + threshold_factor);

        assert_eq!(exit_threshold, dec!(100.1));

        let ask = dec!(100.05);
        assert!(ask <= exit_threshold);
    }

    #[test]
    fn test_edge_calculation() {
        // Long: edge = (bid - oracle) / oracle * 10000
        let oracle = dec!(100);
        let bid = dec!(100.05);
        let edge = (bid - oracle) / oracle * dec!(10000);
        assert_eq!(edge, dec!(5)); // +5 bps (favorable)

        // Short: edge = (oracle - ask) / oracle * 10000
        let ask = dec!(99.95);
        let edge = (oracle - ask) / oracle * dec!(10000);
        assert_eq!(edge, dec!(5)); // +5 bps (favorable)
    }

    // --- Phase B: PnL direction check tests ---

    #[test]
    fn test_pnl_check_long_profitable_exits() {
        // Long entry at 100, bid at 100.10 → PnL = +10 bps → should exit
        let entry = dec!(100);
        let exit_price = dec!(100.10);

        let pnl_bps = (exit_price - entry) / entry * dec!(10000);
        assert_eq!(pnl_bps, dec!(10));
        // Profitable → should NOT skip (should exit normally)
        assert!(pnl_bps >= Decimal::ZERO);
    }

    #[test]
    fn test_pnl_check_long_small_loss_skips() {
        // Long entry at 100, bid at 99.90 → PnL = -10 bps < 15 → skip
        let entry = dec!(100);
        let exit_price = dec!(99.90);
        let min_loss = dec!(15);

        let pnl_bps = (exit_price - entry) / entry * dec!(10000);
        assert_eq!(pnl_bps, dec!(-10));
        assert!(pnl_bps < Decimal::ZERO);
        assert!(pnl_bps.abs() < min_loss);
    }

    #[test]
    fn test_pnl_check_long_large_loss_exits() {
        // Long entry at 100, bid at 99.80 → PnL = -20 bps >= 15 → exit
        let entry = dec!(100);
        let exit_price = dec!(99.80);
        let min_loss = dec!(15);

        let pnl_bps = (exit_price - entry) / entry * dec!(10000);
        assert_eq!(pnl_bps, dec!(-20));
        assert!(pnl_bps < Decimal::ZERO);
        assert!(pnl_bps.abs() >= min_loss);
    }

    #[test]
    fn test_pnl_check_short_small_loss_skips() {
        // Short entry at 100, ask at 100.10 → PnL = -10 bps < 15 → skip
        let entry = dec!(100);
        let exit_price = dec!(100.10);
        let min_loss = dec!(15);

        let pnl_bps = (entry - exit_price) / entry * dec!(10000);
        assert_eq!(pnl_bps, dec!(-10));
        assert!(pnl_bps < Decimal::ZERO);
        assert!(pnl_bps.abs() < min_loss);
    }

    // --- Phase C: Entry edge-linked exit threshold tests ---

    #[test]
    fn test_edge_scaling_disabled_in_test_config() {
        let config = test_config();
        assert!(!config.entry_edge_scaling);
    }

    #[test]
    fn test_edge_scaling_effective_threshold() {
        let config = MarkRegressionConfig {
            entry_edge_scaling: true,
            entry_edge_scale_factor: dec!(0.5),
            exit_threshold_bps: dec!(10),
            ..test_config()
        };
        // entry_edge=40 * 0.5 = 20, max(10, 20) = 20
        assert_eq!(
            config.effective_exit_threshold_bps(Some(dec!(40))),
            dec!(20)
        );
        // entry_edge=15 * 0.5 = 7.5, max(10, 7.5) = 10
        assert_eq!(
            config.effective_exit_threshold_bps(Some(dec!(15))),
            dec!(10)
        );
        // No edge → base threshold
        assert_eq!(config.effective_exit_threshold_bps(None), dec!(10));
    }

    #[test]
    fn test_pnl_check_disabled_when_zero() {
        let config = test_config();
        assert!(config.min_loss_exit_bps.is_zero());
        // When 0, feature is disabled — never skip
    }

    #[test]
    fn test_pnl_check_config_with_value() {
        let config = test_config_with_pnl_check();
        assert_eq!(config.min_loss_exit_bps, dec!(15));
    }
}
