//! Mark regression exit monitor for profit-taking.
//!
//! When the Oracle-BBO divergence is resolved (BBO returns to Oracle proximity),
//! this monitor triggers a position close to capture profits.
//!
//! Exit conditions:
//! - Long: `best_bid >= oracle * (1 - exit_threshold_bps / 10000)`
//! - Short: `best_ask <= oracle * (1 + exit_threshold_bps / 10000)`

use std::collections::HashSet;
use std::sync::Arc;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, info};

use hip3_core::{MarketKey, OrderSide, PendingOrder};
use hip3_feed::MarketState;

use crate::time_stop::FlattenOrderBuilder;
use crate::tracker::{Position, PositionTrackerHandle};

// ============================================================================
// MarkRegressionConfig
// ============================================================================

/// Configuration for mark regression exit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkRegressionConfig {
    /// Whether mark regression exit is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Exit threshold in basis points.
    /// When BBO is within this distance from Oracle, exit is triggered.
    /// Default: 5 bps.
    #[serde(default = "default_exit_threshold_bps")]
    pub exit_threshold_bps: Decimal,
    /// How often to check for exit conditions (ms).
    /// Default: 200ms.
    #[serde(default = "default_check_interval_ms")]
    pub check_interval_ms: u64,
    /// Minimum position holding time before exit can be triggered (ms).
    /// Default: 1000ms (1 second).
    #[serde(default = "default_min_holding_time_ms")]
    pub min_holding_time_ms: u64,
    /// Slippage tolerance for reduce-only orders (bps).
    /// Default: 50 bps.
    #[serde(default = "default_slippage_bps")]
    pub slippage_bps: u64,
}

fn default_enabled() -> bool {
    true
}

fn default_exit_threshold_bps() -> Decimal {
    Decimal::from(5)
}

fn default_check_interval_ms() -> u64 {
    200
}

fn default_min_holding_time_ms() -> u64 {
    1000
}

fn default_slippage_bps() -> u64 {
    50
}

impl Default for MarkRegressionConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            exit_threshold_bps: default_exit_threshold_bps(),
            check_interval_ms: default_check_interval_ms(),
            min_holding_time_ms: default_min_holding_time_ms(),
            slippage_bps: default_slippage_bps(),
        }
    }
}

// ============================================================================
// MarkRegressionMonitor
// ============================================================================

/// Background monitor for mark regression exit.
///
/// Periodically checks all open positions and triggers exit when
/// the Oracle-BBO divergence is resolved (BBO returns to Oracle proximity).
pub struct MarkRegressionMonitor {
    /// Configuration.
    config: MarkRegressionConfig,
    /// Handle to position tracker.
    position_handle: PositionTrackerHandle,
    /// Channel to send flatten orders.
    flatten_tx: mpsc::Sender<PendingOrder>,
    /// Market state for BBO and Oracle data.
    market_state: Arc<MarketState>,
    /// Local tracking of markets with pending flatten orders.
    ///
    /// This provides immediate protection against duplicate flatten orders,
    /// without waiting for the position tracker to be updated asynchronously.
    /// Cleared when the market is no longer in positions snapshot.
    local_flattening: HashSet<MarketKey>,
}

impl MarkRegressionMonitor {
    /// Create a new MarkRegressionMonitor.
    #[must_use]
    pub fn new(
        config: MarkRegressionConfig,
        position_handle: PositionTrackerHandle,
        flatten_tx: mpsc::Sender<PendingOrder>,
        market_state: Arc<MarketState>,
    ) -> Self {
        Self {
            config,
            position_handle,
            flatten_tx,
            market_state,
            local_flattening: HashSet::new(),
        }
    }

    /// Run the monitoring loop.
    ///
    /// Checks positions every `check_interval_ms` and triggers exit
    /// when BBO returns to Oracle proximity.
    pub async fn run(mut self) {
        if !self.config.enabled {
            info!("MarkRegressionMonitor disabled");
            return;
        }

        info!(
            exit_threshold_bps = %self.config.exit_threshold_bps,
            check_interval_ms = self.config.check_interval_ms,
            min_holding_time_ms = self.config.min_holding_time_ms,
            "MarkRegressionMonitor started"
        );

        let interval = tokio::time::Duration::from_millis(self.config.check_interval_ms);
        let mut ticker = tokio::time::interval(interval);

        loop {
            ticker.tick().await;

            let now_ms = chrono::Utc::now().timestamp_millis() as u64;
            let positions = self.position_handle.positions_snapshot();

            // Clean up local_flattening for markets no longer in positions
            let position_markets: HashSet<MarketKey> = positions.iter().map(|p| p.market).collect();
            self.local_flattening
                .retain(|m| position_markets.contains(m));

            // Clean up local_flattening for markets where flatten order was rejected
            // If local_flattening contains a market but is_flattening() returns false,
            // the flatten order was rejected or cancelled - clear local state to allow retry
            self.local_flattening.retain(|m| {
                if self.position_handle.is_flattening(m) {
                    true // Keep: flatten still in progress
                } else {
                    debug!(
                        "MarkRegression: clearing local_flattening for market {} (flatten order completed or rejected)",
                        m
                    );
                    false // Remove: flatten order no longer pending, allow retry
                }
            });

            for position in positions {
                // Check local flattening state first (immediate, no async delay)
                if self.local_flattening.contains(&position.market) {
                    continue;
                }

                // Check if flatten order already pending in position tracker
                // This catches cases where TimeStop sent a flatten
                if self.position_handle.is_flattening(&position.market) {
                    continue;
                }

                if let Some(edge_bps) = self.check_exit(&position, now_ms) {
                    // Mark as flattening BEFORE sending to prevent duplicates
                    self.local_flattening.insert(position.market);
                    self.trigger_exit(&position, edge_bps, now_ms).await;
                }
            }
        }
    }

    /// Check if a position should exit based on mark regression.
    ///
    /// Returns the edge (in bps) if exit condition is met, None otherwise.
    fn check_exit(&self, position: &Position, now_ms: u64) -> Option<Decimal> {
        // 1. Check minimum holding time
        let held_ms = now_ms.saturating_sub(position.entry_timestamp_ms);
        if held_ms < self.config.min_holding_time_ms {
            return None;
        }

        // 2. Get market snapshot
        let snapshot = self.market_state.get_snapshot(&position.market)?;

        // 3. Get oracle price
        let oracle = snapshot.ctx.oracle.oracle_px;
        if oracle.is_zero() {
            return None;
        }

        // 4. Calculate threshold factor
        let threshold_factor = self.config.exit_threshold_bps / Decimal::from(10000);

        // 5. Check exit condition based on position side
        match position.side {
            OrderSide::Buy => {
                // Long: exit when bid >= oracle * (1 - threshold)
                // This means the BBO has recovered to be close to Oracle
                let bid = snapshot.bbo.bid_price;
                let exit_threshold = oracle.inner() * (Decimal::ONE - threshold_factor);

                if bid.inner() >= exit_threshold {
                    // Edge: (bid - oracle) / oracle * 10000
                    // Positive = bid is above oracle (favorable for long exit)
                    // Negative = bid is below oracle (within threshold)
                    let edge_bps =
                        (bid.inner() - oracle.inner()) / oracle.inner() * Decimal::from(10000);
                    return Some(edge_bps);
                }
            }
            OrderSide::Sell => {
                // Short: exit when ask <= oracle * (1 + threshold)
                // This means the BBO has recovered to be close to Oracle
                let ask = snapshot.bbo.ask_price;
                let exit_threshold = oracle.inner() * (Decimal::ONE + threshold_factor);

                if ask.inner() <= exit_threshold {
                    // Edge: (oracle - ask) / oracle * 10000
                    // Positive = ask is below oracle (favorable for short exit)
                    // Negative = ask is above oracle (within threshold)
                    let edge_bps =
                        (oracle.inner() - ask.inner()) / oracle.inner() * Decimal::from(10000);
                    return Some(edge_bps);
                }
            }
        }

        None
    }

    /// Trigger exit for a position.
    async fn trigger_exit(&self, position: &Position, edge_bps: Decimal, now_ms: u64) {
        // Get current snapshot for pricing
        let Some(snapshot) = self.market_state.get_snapshot(&position.market) else {
            return;
        };

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

        info!(
            market = %position.market,
            side = ?position.side,
            edge_bps = %edge_bps,
            held_ms = held_ms,
            cloid = %order.cloid,
            "MarkRegression exit triggered"
        );

        // Send flatten order
        if self.flatten_tx.send(order).await.is_err() {
            debug!("Flatten channel closed, stopping MarkRegressionMonitor");
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetId, DexId, MarketKey, Price, Size};
    use rust_decimal_macros::dec;

    #[allow(dead_code)]
    fn sample_market() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    #[allow(dead_code)]
    fn sample_position(market: MarketKey, side: OrderSide, entry_timestamp_ms: u64) -> Position {
        Position::new(
            market,
            side,
            Size::new(dec!(0.1)),
            Price::new(dec!(100)),
            entry_timestamp_ms,
        )
    }

    #[test]
    fn test_config_default() {
        let config = MarkRegressionConfig::default();
        assert!(config.enabled);
        assert_eq!(config.exit_threshold_bps, dec!(5));
        assert_eq!(config.check_interval_ms, 200);
        assert_eq!(config.min_holding_time_ms, 1000);
        assert_eq!(config.slippage_bps, 50);
    }

    #[test]
    fn test_long_exit_condition_calculation() {
        // Long: exit when bid >= oracle * (1 - threshold)
        // oracle = 100, threshold = 5bps = 0.0005
        // exit_threshold = 100 * (1 - 0.0005) = 99.95
        // bid = 99.96 >= 99.95 → should exit

        let oracle = dec!(100);
        let threshold_bps = dec!(5);
        let threshold_factor = threshold_bps / dec!(10000);
        let exit_threshold = oracle * (Decimal::ONE - threshold_factor);

        assert_eq!(exit_threshold, dec!(99.95));

        // Bid at 99.96 should trigger exit
        let bid = dec!(99.96);
        assert!(bid >= exit_threshold);

        // Edge calculation: (bid - oracle) / oracle * 10000
        // (99.96 - 100) / 100 * 10000 = -0.04 / 100 * 10000 = -4 bps
        let edge = (bid - oracle) / oracle * dec!(10000);
        assert_eq!(edge, dec!(-4)); // Negative because bid < oracle
    }

    #[test]
    fn test_long_exit_no_trigger() {
        // Long: bid = 99.94 < 99.95 → should NOT exit

        let oracle = dec!(100);
        let threshold_bps = dec!(5);
        let threshold_factor = threshold_bps / dec!(10000);
        let exit_threshold = oracle * (Decimal::ONE - threshold_factor);

        let bid = dec!(99.94);
        assert!(bid < exit_threshold); // Should NOT trigger
    }

    #[test]
    fn test_short_exit_condition_calculation() {
        // Short: exit when ask <= oracle * (1 + threshold)
        // oracle = 100, threshold = 5bps = 0.0005
        // exit_threshold = 100 * (1 + 0.0005) = 100.05
        // ask = 100.04 <= 100.05 → should exit

        let oracle = dec!(100);
        let threshold_bps = dec!(5);
        let threshold_factor = threshold_bps / dec!(10000);
        let exit_threshold = oracle * (Decimal::ONE + threshold_factor);

        assert_eq!(exit_threshold, dec!(100.05));

        // Ask at 100.04 should trigger exit
        let ask = dec!(100.04);
        assert!(ask <= exit_threshold);

        // Edge calculation: (oracle - ask) / oracle * 10000
        // (100 - 100.04) / 100 * 10000 = -0.04 / 100 * 10000 = -4 bps
        let edge = (oracle - ask) / oracle * dec!(10000);
        assert_eq!(edge, dec!(-4)); // Negative because ask > oracle
    }

    #[test]
    fn test_short_exit_no_trigger() {
        // Short: ask = 100.06 > 100.05 → should NOT exit

        let oracle = dec!(100);
        let threshold_bps = dec!(5);
        let threshold_factor = threshold_bps / dec!(10000);
        let exit_threshold = oracle * (Decimal::ONE + threshold_factor);

        let ask = dec!(100.06);
        assert!(ask > exit_threshold); // Should NOT trigger
    }

    #[test]
    fn test_min_holding_time_filter() {
        // If held_ms < min_holding_time_ms, should not trigger exit
        let now_ms = 5000_u64;
        let entry_ms = 4500_u64; // Held for 500ms
        let min_holding_time_ms = 1000_u64;

        let held_ms = now_ms.saturating_sub(entry_ms);
        assert_eq!(held_ms, 500);
        assert!(held_ms < min_holding_time_ms);
    }

    #[test]
    fn test_edge_bps_positive_for_favorable_long_exit() {
        // Long position: if bid > oracle, edge is positive (favorable)
        let oracle = dec!(100);
        let bid = dec!(100.02);

        let edge = (bid - oracle) / oracle * dec!(10000);
        assert_eq!(edge, dec!(2)); // 2 bps profit
    }

    #[test]
    fn test_edge_bps_positive_for_favorable_short_exit() {
        // Short position: if ask < oracle, edge is positive (favorable)
        let oracle = dec!(100);
        let ask = dec!(99.98);

        let edge = (oracle - ask) / oracle * dec!(10000);
        assert_eq!(edge, dec!(2)); // 2 bps profit
    }
}
