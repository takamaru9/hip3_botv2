//! Time-based position exit and reduce-only order timeout handling.
//!
//! Provides:
//! - `TimeStop`: Checks if positions exceed holding time threshold
//! - `Flattener`: Creates reduce-only orders to close positions
//! - `TimeStopConfig`: Configuration for time-based exit parameters
//! - `TimeStopMonitor`: Background task for monitoring and triggering flattens
//!
//! Phase B parameter: TIME_STOP_MS = 30 seconds.

use std::sync::Arc;

use rust_decimal::Decimal;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use hip3_core::{ClientOrderId, MarketKey, OrderSide, PendingOrder, Price, TrackedOrder};

use crate::tracker::{Position, PositionTrackerHandle};

/// Default time stop threshold: 30 seconds.
/// Positions held longer than this will be flattened.
pub const TIME_STOP_MS: u64 = 30_000;

/// Default reduce-only timeout: 60 seconds.
/// Reduce-only orders pending longer than this will be retried.
pub const REDUCE_ONLY_TIMEOUT_MS: u64 = 60_000;

// ============================================================================
// TimeStopConfig
// ============================================================================

/// Configuration for time-based exit parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeStopConfig {
    /// Time threshold for position holding (Unix milliseconds).
    /// Default: 30,000 ms (30 seconds)
    pub threshold_ms: u64,

    /// Timeout for reduce-only orders before retry (Unix milliseconds).
    /// Default: 60,000 ms (60 seconds)
    pub reduce_only_timeout_ms: u64,
}

impl Default for TimeStopConfig {
    fn default() -> Self {
        Self {
            threshold_ms: TIME_STOP_MS,
            reduce_only_timeout_ms: REDUCE_ONLY_TIMEOUT_MS,
        }
    }
}

impl TimeStopConfig {
    /// Create a new configuration with custom thresholds.
    #[must_use]
    pub fn new(threshold_ms: u64, reduce_only_timeout_ms: u64) -> Self {
        Self {
            threshold_ms,
            reduce_only_timeout_ms,
        }
    }
}

// ============================================================================
// TimeStop
// ============================================================================

/// TimeStop: Checks positions against holding time threshold.
///
/// Used to identify positions that have been held too long and need to be
/// flattened (closed) to manage risk.
///
/// # Example
/// ```ignore
/// let time_stop = TimeStop::default();
/// let expired = time_stop.check(&positions, now_ms);
/// for market in expired {
///     trigger_flatten(market);
/// }
/// ```
#[derive(Debug, Clone)]
pub struct TimeStop {
    /// Time threshold for position holding (Unix milliseconds).
    threshold_ms: u64,

    /// Timeout for reduce-only orders (Unix milliseconds).
    reduce_only_timeout_ms: u64,
}

impl TimeStop {
    /// Create a new TimeStop checker with the given thresholds.
    #[must_use]
    pub fn new(threshold_ms: u64, reduce_only_timeout_ms: u64) -> Self {
        Self {
            threshold_ms,
            reduce_only_timeout_ms,
        }
    }

    /// Create a TimeStop from a configuration.
    #[must_use]
    pub fn from_config(config: &TimeStopConfig) -> Self {
        Self::new(config.threshold_ms, config.reduce_only_timeout_ms)
    }

    /// Get the threshold in milliseconds.
    #[must_use]
    pub fn threshold_ms(&self) -> u64 {
        self.threshold_ms
    }

    /// Get the reduce-only timeout in milliseconds.
    #[must_use]
    pub fn reduce_only_timeout_ms(&self) -> u64 {
        self.reduce_only_timeout_ms
    }

    /// Check which positions have exceeded the holding time threshold.
    ///
    /// Returns a list of market keys for positions that need to be flattened.
    ///
    /// # Arguments
    /// * `positions` - Slice of current open positions
    /// * `now_ms` - Current timestamp in Unix milliseconds
    ///
    /// # Returns
    /// Vector of `MarketKey` for positions exceeding the threshold
    #[must_use]
    pub fn check(&self, positions: &[Position], now_ms: u64) -> Vec<MarketKey> {
        positions
            .iter()
            .filter(|pos| {
                let holding_time = now_ms.saturating_sub(pos.entry_timestamp_ms);
                holding_time > self.threshold_ms
            })
            .map(|pos| pos.market)
            .collect()
    }

    /// Check a single position for timeout (legacy API compatibility).
    ///
    /// # Arguments
    /// * `entry_timestamp_ms` - Timestamp when the position was opened (ms since epoch)
    /// * `now_ms` - Current timestamp (ms since epoch)
    ///
    /// # Returns
    /// * `Some(elapsed_ms)` if the position has exceeded the timeout threshold
    /// * `None` if the position is still within the time limit
    #[must_use]
    pub fn check_single(&self, entry_timestamp_ms: u64, now_ms: u64) -> Option<u64> {
        let elapsed_ms = now_ms.saturating_sub(entry_timestamp_ms);
        if elapsed_ms >= self.threshold_ms {
            Some(elapsed_ms)
        } else {
            None
        }
    }

    /// Check which reduce-only orders have timed out.
    ///
    /// Returns client order IDs of reduce-only orders that have been pending
    /// for longer than the timeout threshold.
    ///
    /// # Arguments
    /// * `pending_reduce_only` - Slice of pending reduce-only tracked orders
    /// * `now_ms` - Current timestamp in Unix milliseconds
    ///
    /// # Returns
    /// Vector of `ClientOrderId` for timed-out reduce-only orders
    #[must_use]
    pub fn check_reduce_only_timeout(
        &self,
        pending_reduce_only: &[TrackedOrder],
        now_ms: u64,
    ) -> Vec<ClientOrderId> {
        pending_reduce_only
            .iter()
            .filter(|order| {
                let pending_time = now_ms.saturating_sub(order.created_at);
                pending_time > self.reduce_only_timeout_ms
            })
            .map(|order| order.cloid.clone())
            .collect()
    }
}

impl Default for TimeStop {
    fn default() -> Self {
        Self::from_config(&TimeStopConfig::default())
    }
}

// ============================================================================
// FlattenOrderBuilder
// ============================================================================

/// Creates reduce-only orders to close positions.
///
/// Generates IOC (Immediate-Or-Cancel) reduce-only orders with slippage
/// adjustment to ensure execution.
///
/// Note: This is a stateless utility for creating flatten orders.
/// For state management of the flatten process, see `flatten::Flattener`.
pub struct FlattenOrderBuilder;

impl FlattenOrderBuilder {
    /// Create a reduce-only order to flatten (close) a position.
    ///
    /// The order is set to the opposite side of the position with full size,
    /// and the price includes slippage adjustment for IOC execution.
    ///
    /// # Arguments
    /// * `position` - The position to flatten
    /// * `current_price` - Current market price (best bid/ask)
    /// * `slippage_bps` - Slippage tolerance in basis points (e.g., 50 = 0.5%)
    /// * `now_ms` - Current timestamp in Unix milliseconds
    ///
    /// # Returns
    /// A `PendingOrder` configured as reduce-only to close the position
    ///
    /// # Price Calculation
    /// - Long position (Sell): `price * (1 - slippage_bps / 10000)`
    /// - Short position (Buy): `price * (1 + slippage_bps / 10000)`
    #[must_use]
    pub fn create_flatten_order(
        position: &Position,
        current_price: Price,
        slippage_bps: u64,
        now_ms: u64,
    ) -> PendingOrder {
        // Determine opposite side for closing
        let close_side = match position.side {
            OrderSide::Buy => OrderSide::Sell, // Close long with sell
            OrderSide::Sell => OrderSide::Buy, // Close short with buy
        };

        // Calculate price with slippage
        let slippage_factor = Decimal::from(slippage_bps) / Decimal::from(10_000);
        let adjusted_price = match position.side {
            // Long position: sell at lower price (accept slippage down)
            OrderSide::Buy => {
                let factor = Decimal::ONE - slippage_factor;
                Price::new(current_price.inner() * factor)
            }
            // Short position: buy at higher price (accept slippage up)
            OrderSide::Sell => {
                let factor = Decimal::ONE + slippage_factor;
                Price::new(current_price.inner() * factor)
            }
        };

        PendingOrder::new(
            ClientOrderId::new(),
            position.market,
            close_side,
            adjusted_price,
            position.size,
            true, // reduce_only = true
            now_ms,
        )
    }
}

// ============================================================================
// PriceProvider Trait
// ============================================================================

/// Trait for providing current market prices.
///
/// Implemented by components that can provide real-time price data
/// (e.g., order book managers, price feeds).
pub trait PriceProvider: Send + Sync {
    /// Get the current price for a market.
    ///
    /// Returns `None` if price is unavailable (e.g., no data, stale data).
    fn get_price(&self, market: &MarketKey) -> Option<Price>;
}

// ============================================================================
// TimeStopMonitor
// ============================================================================

/// Background monitor for time-based position exit.
///
/// Periodically checks positions against time thresholds and sends
/// flatten orders when positions exceed the holding time limit.
pub struct TimeStopMonitor<P: PriceProvider> {
    /// Time stop configuration and checker.
    time_stop: TimeStop,

    /// Handle to position tracker for position data.
    position_handle: PositionTrackerHandle,

    /// Channel to send flatten orders.
    flatten_tx: mpsc::Sender<PendingOrder>,

    /// Price provider for current market prices.
    price_provider: Arc<P>,

    /// Slippage tolerance in basis points.
    slippage_bps: u64,

    /// Check interval in milliseconds.
    check_interval_ms: u64,
}

impl<P: PriceProvider + 'static> TimeStopMonitor<P> {
    /// Create a new TimeStopMonitor.
    ///
    /// # Arguments
    /// * `config` - Time stop configuration
    /// * `position_handle` - Handle to position tracker
    /// * `flatten_tx` - Channel to send flatten orders
    /// * `price_provider` - Provider for current prices
    /// * `slippage_bps` - Slippage tolerance in basis points (default: 50)
    /// * `check_interval_ms` - How often to check (default: 1000ms)
    #[must_use]
    pub fn new(
        config: TimeStopConfig,
        position_handle: PositionTrackerHandle,
        flatten_tx: mpsc::Sender<PendingOrder>,
        price_provider: Arc<P>,
        slippage_bps: u64,
        check_interval_ms: u64,
    ) -> Self {
        Self {
            time_stop: TimeStop::from_config(&config),
            position_handle,
            flatten_tx,
            price_provider,
            slippage_bps,
            check_interval_ms,
        }
    }

    /// Create with default slippage (50 bps) and check interval (1 second).
    #[must_use]
    pub fn with_defaults(
        config: TimeStopConfig,
        position_handle: PositionTrackerHandle,
        flatten_tx: mpsc::Sender<PendingOrder>,
        price_provider: Arc<P>,
    ) -> Self {
        Self::new(
            config,
            position_handle,
            flatten_tx,
            price_provider,
            50,   // 0.5% slippage
            1000, // 1 second interval
        )
    }

    /// Run the monitoring loop.
    ///
    /// Checks positions every `check_interval_ms` and sends flatten orders
    /// for positions that exceed the time threshold.
    ///
    /// Also monitors reduce-only orders and emits alerts when they exceed
    /// the 60-second timeout threshold.
    ///
    /// This method runs until the flatten channel is closed.
    pub async fn run(self) {
        info!(
            "TimeStopMonitor started: threshold={}ms, reduce_only_timeout={}ms, interval={}ms",
            self.time_stop.threshold_ms(),
            self.time_stop.reduce_only_timeout_ms(),
            self.check_interval_ms
        );

        let interval = tokio::time::Duration::from_millis(self.check_interval_ms);
        let mut ticker = tokio::time::interval(interval);

        loop {
            ticker.tick().await;

            // Get current time
            let now_ms = chrono::Utc::now().timestamp_millis() as u64;

            // Get current positions
            let positions = self.position_handle.positions_snapshot();

            // Check for reduce-only order timeout alerts (60s threshold)
            self.check_reduce_only_timeout_alerts(now_ms);

            if positions.is_empty() {
                continue;
            }

            // Check for positions exceeding threshold
            let expired_markets = self.time_stop.check(&positions, now_ms);

            for market in expired_markets {
                // Re-check position exists before creating flatten order
                // This reduces race conditions where position was closed between snapshot and now
                if !self.position_handle.has_position(&market) {
                    debug!(
                        "TimeStop: position for market {} no longer exists, skipping flatten",
                        market
                    );
                    continue;
                }

                // Find the position for this market
                let Some(position) = positions.iter().find(|p| p.market == market) else {
                    continue;
                };

                // Get current price
                let Some(price) = self.price_provider.get_price(&market) else {
                    warn!("No price available for market {}, skipping flatten", market);
                    continue;
                };

                // Create flatten order
                let order = FlattenOrderBuilder::create_flatten_order(
                    position,
                    price,
                    self.slippage_bps,
                    now_ms,
                );

                debug!(
                    "TimeStop triggered for market {}: creating flatten order cloid={}, side={:?}, size={}, price={}",
                    market, order.cloid, order.side, order.size, order.price
                );

                // Send flatten order
                if self.flatten_tx.send(order).await.is_err() {
                    info!("Flatten channel closed, stopping TimeStopMonitor");
                    return;
                }
            }
        }
    }

    /// Check for reduce-only orders that have exceeded the 60-second timeout.
    ///
    /// Emits warning logs for each timed-out order to alert operators
    /// that flatten orders are not being filled in a timely manner.
    fn check_reduce_only_timeout_alerts(&self, now_ms: u64) {
        // Get all pending reduce-only orders from position tracker
        // We iterate through pending_orders_snapshot to find reduce-only orders
        let mut timed_out_count = 0;

        for entry in self.position_handle.pending_orders_snapshot_iter() {
            let (cloid, (market, is_reduce_only)) = entry.pair();
            if !is_reduce_only {
                continue;
            }

            // Get the order creation time from pending_orders_data
            if let Some(order_entry) = self.position_handle.get_pending_order(cloid) {
                let order = order_entry.value();
                let pending_time = now_ms.saturating_sub(order.created_at);

                if pending_time > self.time_stop.reduce_only_timeout_ms() {
                    warn!(
                        "⚠️ REDUCE-ONLY TIMEOUT: cloid={} market={} pending for {}ms (threshold={}ms)",
                        cloid,
                        market,
                        pending_time,
                        self.time_stop.reduce_only_timeout_ms()
                    );
                    timed_out_count += 1;
                }
            }
        }

        if timed_out_count > 0 {
            warn!(
                "⚠️ {} reduce-only orders exceeded 60s timeout - flatten may be failing",
                timed_out_count
            );
        }
    }
}

// ============================================================================
// Legacy API (for backward compatibility)
// ============================================================================

/// Manages TimeStop checks across multiple markets.
///
/// Provides batch checking functionality for all open positions.
///
/// **Deprecated**: Use `TimeStop::check()` directly instead.
#[derive(Debug, Clone)]
pub struct TimeStopManager {
    time_stop: TimeStop,
}

impl TimeStopManager {
    /// Create a new TimeStopManager with custom timeout.
    #[must_use]
    pub fn new(timeout_ms: u64) -> Self {
        Self {
            time_stop: TimeStop::new(timeout_ms, REDUCE_ONLY_TIMEOUT_MS),
        }
    }

    /// Create a TimeStopManager with default timeout.
    #[must_use]
    pub fn with_default() -> Self {
        Self {
            time_stop: TimeStop::default(),
        }
    }

    /// Get the underlying TimeStop.
    #[must_use]
    pub fn time_stop(&self) -> &TimeStop {
        &self.time_stop
    }

    /// Check all positions and return those that have timed out.
    ///
    /// # Arguments
    /// * `positions` - Slice of all open positions
    /// * `now_ms` - Current timestamp (ms since epoch)
    ///
    /// # Returns
    /// Vector of (MarketKey, elapsed_ms) for positions that have timed out
    #[must_use]
    pub fn check_all(&self, positions: &[Position], now_ms: u64) -> Vec<(MarketKey, u64)> {
        positions
            .iter()
            .filter_map(|pos| {
                self.time_stop
                    .check_single(pos.entry_timestamp_ms, now_ms)
                    .map(|elapsed| (pos.market, elapsed))
            })
            .collect()
    }
}

impl Default for TimeStopManager {
    fn default() -> Self {
        Self::with_default()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetId, DexId, Size};
    use rust_decimal_macros::dec;

    fn sample_market() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    fn sample_market_2() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(1))
    }

    fn sample_market_3() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(2))
    }

    fn sample_position(market: MarketKey, side: OrderSide, entry_timestamp_ms: u64) -> Position {
        Position::new(
            market,
            side,
            Size::new(dec!(0.1)),
            Price::new(dec!(50000)),
            entry_timestamp_ms,
        )
    }

    fn sample_tracked_order(
        cloid: ClientOrderId,
        created_at: u64,
        reduce_only: bool,
    ) -> TrackedOrder {
        TrackedOrder::from_pending(PendingOrder::new(
            cloid,
            sample_market(),
            OrderSide::Buy,
            Price::new(dec!(50000)),
            Size::new(dec!(0.1)),
            reduce_only,
            created_at,
        ))
    }

    // ========================================================================
    // TimeStopConfig Tests
    // ========================================================================

    #[test]
    fn test_config_default() {
        let config = TimeStopConfig::default();
        assert_eq!(config.threshold_ms, 30_000);
        assert_eq!(config.reduce_only_timeout_ms, 60_000);
    }

    #[test]
    fn test_config_custom() {
        let config = TimeStopConfig::new(10_000, 20_000);
        assert_eq!(config.threshold_ms, 10_000);
        assert_eq!(config.reduce_only_timeout_ms, 20_000);
    }

    // ========================================================================
    // TimeStop Tests
    // ========================================================================

    #[test]
    fn test_time_stop_check_exceeds_threshold() {
        let time_stop = TimeStop::new(30_000, 60_000);

        // Position opened 40 seconds ago (exceeds 30s threshold)
        let now_ms = 100_000;
        let entry_ms = 60_000; // 40 seconds before now
        let positions = vec![sample_position(sample_market(), OrderSide::Buy, entry_ms)];

        let expired = time_stop.check(&positions, now_ms);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0], sample_market());
    }

    #[test]
    fn test_time_stop_check_within_threshold() {
        let time_stop = TimeStop::new(30_000, 60_000);

        // Position opened 20 seconds ago (within 30s threshold)
        let now_ms = 100_000;
        let entry_ms = 80_000; // 20 seconds before now
        let positions = vec![sample_position(sample_market(), OrderSide::Buy, entry_ms)];

        let expired = time_stop.check(&positions, now_ms);
        assert!(expired.is_empty());
    }

    #[test]
    fn test_time_stop_check_exactly_at_threshold() {
        let time_stop = TimeStop::new(30_000, 60_000);

        // Position opened exactly 30 seconds ago (at threshold, not exceeded)
        let now_ms = 100_000;
        let entry_ms = 70_000; // exactly 30 seconds before now
        let positions = vec![sample_position(sample_market(), OrderSide::Buy, entry_ms)];

        let expired = time_stop.check(&positions, now_ms);
        // Exactly at threshold should NOT trigger (we use > not >=)
        assert!(expired.is_empty());
    }

    #[test]
    fn test_time_stop_check_multiple_positions() {
        let time_stop = TimeStop::new(30_000, 60_000);
        let now_ms = 100_000;

        let positions = vec![
            sample_position(sample_market(), OrderSide::Buy, 60_000), // 40s - expired
            sample_position(sample_market_2(), OrderSide::Sell, 80_000), // 20s - not expired
        ];

        let expired = time_stop.check(&positions, now_ms);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0], sample_market());
    }

    #[test]
    fn test_time_stop_check_empty_positions() {
        let time_stop = TimeStop::new(30_000, 60_000);
        let positions: Vec<Position> = vec![];
        let expired = time_stop.check(&positions, 100_000);
        assert!(expired.is_empty());
    }

    // ========================================================================
    // Reduce-Only Timeout Tests
    // ========================================================================

    #[test]
    fn test_reduce_only_timeout_exceeds_threshold() {
        let time_stop = TimeStop::new(30_000, 60_000);

        // Order created 70 seconds ago (exceeds 60s timeout)
        let now_ms = 100_000;
        let created_at = 30_000; // 70 seconds before now
        let cloid = ClientOrderId::new();
        let orders = vec![sample_tracked_order(cloid.clone(), created_at, true)];

        let timed_out = time_stop.check_reduce_only_timeout(&orders, now_ms);
        assert_eq!(timed_out.len(), 1);
        assert_eq!(timed_out[0], cloid);
    }

    #[test]
    fn test_reduce_only_timeout_within_threshold() {
        let time_stop = TimeStop::new(30_000, 60_000);

        // Order created 50 seconds ago (within 60s timeout)
        let now_ms = 100_000;
        let created_at = 50_000; // 50 seconds before now
        let orders = vec![sample_tracked_order(ClientOrderId::new(), created_at, true)];

        let timed_out = time_stop.check_reduce_only_timeout(&orders, now_ms);
        assert!(timed_out.is_empty());
    }

    #[test]
    fn test_reduce_only_timeout_exactly_at_threshold() {
        let time_stop = TimeStop::new(30_000, 60_000);

        // Order created exactly 60 seconds ago (at threshold, not exceeded)
        let now_ms = 100_000;
        let created_at = 40_000; // exactly 60 seconds before now
        let orders = vec![sample_tracked_order(ClientOrderId::new(), created_at, true)];

        let timed_out = time_stop.check_reduce_only_timeout(&orders, now_ms);
        // Exactly at threshold should NOT trigger (we use > not >=)
        assert!(timed_out.is_empty());
    }

    // ========================================================================
    // Flattener Tests
    // ========================================================================

    #[test]
    fn test_flatten_long_position() {
        let position = sample_position(sample_market(), OrderSide::Buy, 50_000);
        let current_price = Price::new(dec!(50000));
        let slippage_bps = 50; // 0.5%
        let now_ms = 100_000;

        let order = FlattenOrderBuilder::create_flatten_order(
            &position,
            current_price,
            slippage_bps,
            now_ms,
        );

        // Long -> Sell to close
        assert_eq!(order.side, OrderSide::Sell);
        assert!(order.reduce_only);
        assert_eq!(order.size, position.size);
        assert_eq!(order.market, sample_market());
        assert_eq!(order.created_at, now_ms);

        // Price should be lower (selling with slippage)
        // 50000 * (1 - 0.005) = 49750
        let expected_price = Price::new(dec!(49750));
        assert_eq!(order.price, expected_price);
    }

    #[test]
    fn test_flatten_short_position() {
        let position = sample_position(sample_market(), OrderSide::Sell, 50_000);
        let current_price = Price::new(dec!(50000));
        let slippage_bps = 50; // 0.5%
        let now_ms = 100_000;

        let order = FlattenOrderBuilder::create_flatten_order(
            &position,
            current_price,
            slippage_bps,
            now_ms,
        );

        // Short -> Buy to close
        assert_eq!(order.side, OrderSide::Buy);
        assert!(order.reduce_only);
        assert_eq!(order.size, position.size);

        // Price should be higher (buying with slippage)
        // 50000 * (1 + 0.005) = 50250
        let expected_price = Price::new(dec!(50250));
        assert_eq!(order.price, expected_price);
    }

    #[test]
    fn test_flatten_zero_slippage() {
        let position = sample_position(sample_market(), OrderSide::Buy, 50_000);
        let current_price = Price::new(dec!(50000));
        let slippage_bps = 0;
        let now_ms = 100_000;

        let order = FlattenOrderBuilder::create_flatten_order(
            &position,
            current_price,
            slippage_bps,
            now_ms,
        );

        // With zero slippage, price should be unchanged
        assert_eq!(order.price, current_price);
    }

    #[test]
    fn test_flatten_high_slippage() {
        let position = sample_position(sample_market(), OrderSide::Buy, 50_000);
        let current_price = Price::new(dec!(50000));
        let slippage_bps = 100; // 1%
        let now_ms = 100_000;

        let order = FlattenOrderBuilder::create_flatten_order(
            &position,
            current_price,
            slippage_bps,
            now_ms,
        );

        // 50000 * (1 - 0.01) = 49500
        let expected_price = Price::new(dec!(49500));
        assert_eq!(order.price, expected_price);
    }

    // ========================================================================
    // TimeStop Default Tests
    // ========================================================================

    #[test]
    fn test_time_stop_default() {
        let time_stop = TimeStop::default();
        assert_eq!(time_stop.threshold_ms(), 30_000);
        assert_eq!(time_stop.reduce_only_timeout_ms(), 60_000);
    }

    #[test]
    fn test_time_stop_from_config() {
        let config = TimeStopConfig::new(15_000, 45_000);
        let time_stop = TimeStop::from_config(&config);
        assert_eq!(time_stop.threshold_ms(), 15_000);
        assert_eq!(time_stop.reduce_only_timeout_ms(), 45_000);
    }

    // ========================================================================
    // Legacy API Tests (TimeStopManager)
    // ========================================================================

    #[test]
    fn test_time_stop_single_before_timeout() {
        let time_stop = TimeStop::default();

        // Entry at 1000ms, now at 10000ms -> 9 seconds elapsed, not timed out
        let result = time_stop.check_single(1000, 10000);
        assert!(result.is_none());
    }

    #[test]
    fn test_time_stop_single_after_timeout() {
        let time_stop = TimeStop::default();

        // Entry at 0ms, now at 31000ms -> 31 seconds elapsed, timed out
        let result = time_stop.check_single(0, 31000);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), 31000);
    }

    #[test]
    fn test_time_stop_single_exact_boundary() {
        let time_stop = TimeStop::default();

        // Entry at 0ms, now at exactly 30000ms -> exactly at threshold, timed out
        let result = time_stop.check_single(0, TIME_STOP_MS);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), TIME_STOP_MS);

        // Entry at 0ms, now at 29999ms -> 1ms before threshold, not timed out
        let result = time_stop.check_single(0, TIME_STOP_MS - 1);
        assert!(result.is_none());
    }

    #[test]
    fn test_time_stop_manager_check_all_empty() {
        let manager = TimeStopManager::with_default();
        let positions: Vec<Position> = vec![];

        let timed_out = manager.check_all(&positions, 100_000);
        assert!(timed_out.is_empty());
    }

    #[test]
    fn test_time_stop_manager_check_all_none_timed_out() {
        let manager = TimeStopManager::with_default();
        let market = sample_market();
        let positions = vec![
            sample_position(market, OrderSide::Buy, 80_000), // 20s old
            sample_position(market, OrderSide::Buy, 90_000), // 10s old
        ];

        let timed_out = manager.check_all(&positions, 100_000);
        assert!(timed_out.is_empty());
    }

    #[test]
    fn test_time_stop_manager_check_all_some_timed_out() {
        let manager = TimeStopManager::with_default();
        let market1 = sample_market();
        let market2 = sample_market_2();
        let market3 = sample_market_3();

        let positions = vec![
            sample_position(market1, OrderSide::Buy, 50_000), // 50s old -> timed out
            sample_position(market2, OrderSide::Buy, 80_000), // 20s old -> not timed out
            sample_position(market3, OrderSide::Buy, 60_000), // 40s old -> timed out
        ];

        let timed_out = manager.check_all(&positions, 100_000);
        assert_eq!(timed_out.len(), 2);

        // Check markets
        let markets: Vec<_> = timed_out.iter().map(|(m, _)| *m).collect();
        assert!(markets.contains(&market1));
        assert!(markets.contains(&market3));
        assert!(!markets.contains(&market2));

        // Check elapsed times
        for (market, elapsed) in &timed_out {
            if *market == market1 {
                assert_eq!(*elapsed, 50_000);
            } else if *market == market3 {
                assert_eq!(*elapsed, 40_000);
            }
        }
    }

    #[test]
    fn test_time_stop_saturating_sub() {
        let time_stop = TimeStop::default();

        // Edge case: now is before entry (clock skew)
        // Should not panic, elapsed = 0 (saturating_sub)
        let result = time_stop.check_single(100_000, 50_000);
        assert!(result.is_none()); // 0 < timeout
    }
}
