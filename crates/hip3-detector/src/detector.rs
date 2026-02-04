//! Dislocation detector implementation.
//!
//! Detects when best bid/ask crosses oracle price with sufficient
//! edge to cover fees and slippage.
//!
//! Implements P0-24: HIP-3 2x fee calculation with audit trail.
//!
//! Oracle Direction Filter:
//! The HIP-3 strategy targets "stale liquidity" - orders left behind when
//! oracle moves. Signals should only be generated when:
//! - Buy: Oracle is rising (stale ask from before oracle rise)
//! - Sell: Oracle is falling (stale bid from before oracle fall)
//!
//! When oracle is lagging behind market (trending market), signals are
//! filtered out to avoid trading against the trend.

use crate::config::DetectorConfig;
use crate::error::DetectorError;
use crate::fee::{FeeCalculator, UserFees};
use crate::signal::{DislocationSignal, SignalStrength};
use hip3_core::types::MarketSnapshot;
use hip3_core::{MarketKey, OrderSide, Price, Size};
use hip3_feed::{MoveDirection, OracleMovementTracker};
use rust_decimal::Decimal;
use std::cell::RefCell;
use std::collections::HashMap;
use tracing::{info, trace};

/// Oracle direction for filtering signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleDirection {
    /// Oracle is rising (current > previous).
    Rising,
    /// Oracle is falling (current < previous).
    Falling,
    /// Oracle is unchanged or no previous data.
    Unchanged,
}

/// Oracle movement data for filtering (direction + magnitude).
#[derive(Debug, Clone, Copy)]
struct OracleMovement {
    /// Direction of oracle movement.
    direction: OracleDirection,
    /// Absolute change in basis points.
    change_bps: Decimal,
}

/// Dislocation detector.
///
/// Strategy: Enter when best price crosses oracle with edge > (FEE + SLIP + EDGE).
/// - Buy: best_ask <= oracle * (1 - threshold)
/// - Sell: best_bid >= oracle * (1 + threshold)
///
/// IMPORTANT: Only trigger on best crossing oracle, not on mid divergence.
///
/// P0-24: Uses FeeCalculator for HIP-3 2x fee multiplier with full audit trail.
///
/// Oracle Direction Filter (when enabled):
/// - Buy signals only generated when oracle is rising (stale ask)
/// - Sell signals only generated when oracle is falling (stale bid)
pub struct DislocationDetector {
    config: DetectorConfig,
    fee_calculator: FeeCalculator,
    /// Previous oracle prices per market for direction detection.
    /// Uses RefCell for interior mutability since check() takes &self.
    prev_oracle: RefCell<HashMap<MarketKey, Price>>,
}

impl DislocationDetector {
    /// Create a new detector with configuration.
    ///
    /// P0-4: Uses config.taker_fee_bps as effective fee (HIP-3 2x already applied).
    /// Creates UserFees from effective bps by deriving base fee.
    ///
    /// Returns Err if configuration is invalid.
    pub fn new(config: DetectorConfig) -> Result<Self, DetectorError> {
        config.validate().map_err(DetectorError::ConfigError)?;

        // P0-4: config.taker_fee_bps is effective (2x applied), derive base fee
        let user_fees = UserFees::from_effective_taker_bps(config.taker_fee_bps);
        let fee_calculator =
            FeeCalculator::new(user_fees, config.slippage_bps, config.min_edge_bps);
        Ok(Self {
            config,
            fee_calculator,
            prev_oracle: RefCell::new(HashMap::new()),
        })
    }

    /// Create detector with custom user fees.
    ///
    /// Use this when user-specific fees are available from REST API.
    ///
    /// Returns Err if configuration is invalid.
    pub fn with_user_fees(
        config: DetectorConfig,
        user_fees: UserFees,
    ) -> Result<Self, DetectorError> {
        config.validate().map_err(DetectorError::ConfigError)?;

        let fee_calculator =
            FeeCalculator::new(user_fees, config.slippage_bps, config.min_edge_bps);
        Ok(Self {
            config,
            fee_calculator,
            prev_oracle: RefCell::new(HashMap::new()),
        })
    }

    /// Update user fees (e.g., after REST API fetch).
    pub fn update_user_fees(&mut self, user_fees: UserFees) {
        self.fee_calculator.update_user_fees(user_fees);
    }

    /// Get current fee calculator.
    pub fn fee_calculator(&self) -> &FeeCalculator {
        &self.fee_calculator
    }

    /// Get oracle direction and change amount for a market.
    ///
    /// Compares current oracle with previous oracle to determine direction
    /// and magnitude of movement.
    /// Also updates the previous oracle tracking.
    ///
    /// Returns OracleMovement with:
    /// - `direction`: Rising, Falling, or Unchanged
    /// - `change_bps`: Absolute change in basis points (always positive)
    fn get_oracle_movement(&self, key: MarketKey, current_oracle: Price) -> OracleMovement {
        let mut prev_map = self.prev_oracle.borrow_mut();

        let movement = if let Some(&prev) = prev_map.get(&key) {
            if prev.is_zero() {
                OracleMovement {
                    direction: OracleDirection::Unchanged,
                    change_bps: Decimal::ZERO,
                }
            } else {
                // Calculate change in bps: |current - prev| / prev * 10000
                let change_bps = ((current_oracle.inner() - prev.inner()).abs() / prev.inner())
                    * Decimal::from(10000);

                let direction = if current_oracle.inner() > prev.inner() {
                    OracleDirection::Rising
                } else if current_oracle.inner() < prev.inner() {
                    OracleDirection::Falling
                } else {
                    OracleDirection::Unchanged
                };

                OracleMovement {
                    direction,
                    change_bps,
                }
            }
        } else {
            OracleMovement {
                direction: OracleDirection::Unchanged,
                change_bps: Decimal::ZERO,
            }
        };

        // Update previous oracle for next check
        prev_map.insert(key, current_oracle);

        movement
    }

    /// Check if oracle direction is compatible with signal side.
    ///
    /// For HIP-3 strategy, we only want signals caused by "stale liquidity":
    /// - Buy: Oracle rose but ask is still low (stale ask) -> oracle must be Rising
    /// - Sell: Oracle fell but bid is still high (stale bid) -> oracle must be Falling
    ///
    /// When oracle is lagging (market leading), we skip the signal:
    /// - If market drops first, ask < oracle but oracle will follow down -> bad Buy
    /// - If market rises first, bid > oracle but oracle will follow up -> bad Sell
    fn is_direction_compatible(&self, side: OrderSide, direction: OracleDirection) -> bool {
        match side {
            OrderSide::Buy => direction == OracleDirection::Rising,
            OrderSide::Sell => direction == OracleDirection::Falling,
        }
    }

    /// Check for dislocation opportunity.
    ///
    /// Returns Some(signal) if a valid opportunity is detected.
    ///
    /// # Arguments
    /// * `key` - Market key
    /// * `snapshot` - Market snapshot with BBO and oracle data
    /// * `threshold_override_bps` - Optional per-market threshold in basis points.
    ///   If provided, uses this instead of fee_calculator's total_cost_bps.
    /// * `oracle_tracker` - Optional oracle movement tracker for consecutive move filtering.
    ///   If None, consecutive move check is skipped (backward compatible).
    /// * `oracle_age_ms` - Optional time since oracle last changed (ms).
    ///   Used for Quote Lag Gate filtering. If None, gate is skipped.
    pub fn check(
        &self,
        key: MarketKey,
        snapshot: &MarketSnapshot,
        threshold_override_bps: Option<Decimal>,
        oracle_tracker: Option<&OracleMovementTracker>,
        oracle_age_ms: Option<i64>,
    ) -> Option<DislocationSignal> {
        // Get oracle movement (direction + change amount) for filtering
        let oracle = snapshot.ctx.oracle.oracle_px;
        let movement = Self::get_oracle_movement(self, key, oracle);

        // Check buy opportunity: ask below oracle
        if let Some(signal) = self.check_buy(
            key,
            snapshot,
            threshold_override_bps,
            movement,
            oracle_tracker,
            oracle_age_ms,
        ) {
            return Some(signal);
        }

        // Check sell opportunity: bid above oracle
        if let Some(signal) = self.check_sell(
            key,
            snapshot,
            threshold_override_bps,
            movement,
            oracle_tracker,
            oracle_age_ms,
        ) {
            return Some(signal);
        }

        None
    }

    /// Check for buy opportunity.
    ///
    /// Buy when: best_ask <= oracle * (1 - cost_threshold)
    ///
    /// P0-24: Uses FeeCalculator with HIP-3 2x multiplier.
    ///
    /// Oracle Filters (when enabled):
    /// - Direction: Only generates signal if oracle is rising (stale ask)
    /// - Velocity: Only generates signal if oracle moved enough (min_oracle_change_bps)
    /// - Consecutive: Only generates signal if oracle has moved N times in same direction
    /// - Quote Lag: Only generates signal if oracle age is within configured window
    fn check_buy(
        &self,
        key: MarketKey,
        snapshot: &MarketSnapshot,
        threshold_override_bps: Option<Decimal>,
        oracle_movement: OracleMovement,
        oracle_tracker: Option<&OracleMovementTracker>,
        oracle_age_ms: Option<i64>,
    ) -> Option<DislocationSignal> {
        // P0-14: Check if BBO is tradeable
        if !snapshot.is_tradeable() {
            return None;
        }

        let oracle = snapshot.ctx.oracle.oracle_px;
        let ask = snapshot.bbo.ask_price;
        let ask_size = snapshot.bbo.ask_size;

        if oracle.is_zero() || ask.is_zero() {
            return None;
        }

        // Calculate raw edge: (oracle - ask) / oracle * 10000
        let raw_edge_bps = (oracle.inner() - ask.inner()) / oracle.inner() * Decimal::from(10000);

        // Only proceed if ask is actually below oracle (positive edge)
        if raw_edge_bps <= Decimal::ZERO {
            return None;
        }

        // Use per-market threshold if provided, otherwise use FeeCalculator's total_cost
        let total_cost =
            threshold_override_bps.unwrap_or_else(|| self.fee_calculator.total_cost_bps());
        let net_edge_bps = raw_edge_bps - total_cost;

        // Check if edge is sufficient
        let strength = SignalStrength::from_edge(raw_edge_bps, total_cost)?;

        // Oracle Direction Filter: Buy only when oracle is rising (stale ask)
        // This filters out signals caused by oracle lagging in downtrend
        if self.config.oracle_direction_filter
            && !self.is_direction_compatible(OrderSide::Buy, oracle_movement.direction)
        {
            tracing::debug!(
                %key,
                side = "buy",
                raw_edge_bps = %raw_edge_bps,
                oracle_direction = ?oracle_movement.direction,
                "Signal skipped: oracle not rising (oracle lag in downtrend)"
            );
            return None;
        }

        // Oracle Velocity Filter: Skip if oracle movement is too small
        // Small movements are likely noise, MM will quickly follow
        if oracle_movement.change_bps < self.config.min_oracle_change_bps {
            tracing::debug!(
                %key,
                side = "buy",
                raw_edge_bps = %raw_edge_bps,
                oracle_change_bps = %oracle_movement.change_bps,
                min_oracle_change_bps = %self.config.min_oracle_change_bps,
                "Signal skipped: oracle movement too small"
            );
            return None;
        }

        // Oracle Consecutive Filter: Skip if oracle hasn't moved enough times in same direction
        // Multiple consecutive moves indicate a real trend that MMs haven't caught up with
        if self.config.min_consecutive_oracle_moves > 0 {
            if let Some(tracker) = oracle_tracker {
                let consecutive_up = tracker.consecutive(&key, MoveDirection::Up);
                if consecutive_up < self.config.min_consecutive_oracle_moves {
                    trace!(
                        %key,
                        side = "buy",
                        raw_edge_bps = %raw_edge_bps,
                        consecutive_up = consecutive_up,
                        required = self.config.min_consecutive_oracle_moves,
                        "Signal skipped: oracle not consistently rising"
                    );
                    return None;
                }
            }
        }

        // Quote Lag Gate: Only generate signal when oracle changed within time window
        // This ensures "true stale liquidity" - oracle moved but MM hasn't caught up yet
        if let Some(age_ms) = oracle_age_ms {
            // Check minimum lag (filter noise from micro-movements)
            if self.config.min_quote_lag_ms > 0 && age_ms < self.config.min_quote_lag_ms {
                tracing::debug!(
                    %key,
                    side = "buy",
                    raw_edge_bps = %raw_edge_bps,
                    oracle_age_ms = age_ms,
                    min_quote_lag_ms = self.config.min_quote_lag_ms,
                    "Signal skipped: oracle moved too recently (noise filter)"
                );
                return None;
            }

            // Check maximum lag (filter stale - MM caught up)
            if self.config.max_quote_lag_ms > 0 && age_ms > self.config.max_quote_lag_ms {
                tracing::debug!(
                    %key,
                    side = "buy",
                    raw_edge_bps = %raw_edge_bps,
                    oracle_age_ms = age_ms,
                    max_quote_lag_ms = self.config.max_quote_lag_ms,
                    "Signal skipped: oracle moved too long ago (MM caught up)"
                );
                return None;
            }
        }

        // Calculate suggested size (may return ZERO if liquidity too low)
        let suggested_size = self.calculate_size(snapshot, OrderSide::Buy);

        // Skip signal if size is zero (low liquidity)
        if suggested_size.is_zero() {
            tracing::debug!(
                %key,
                side = "buy",
                raw_edge_bps = %raw_edge_bps,
                "Signal skipped due to low liquidity"
            );
            return None;
        }

        // P0-24: Generate fee metadata for audit trail
        let fee_metadata = self.fee_calculator.metadata();

        info!(
            %key,
            side = "buy",
            raw_edge_bps = %raw_edge_bps,
            net_edge_bps = %net_edge_bps,
            total_cost_bps = %total_cost,
            effective_fee_bps = %fee_metadata.effective_taker_fee_bps,
            oracle = %oracle,
            ask = %ask,
            oracle_direction = ?oracle_movement.direction,
            oracle_change_bps = %oracle_movement.change_bps,
            ?oracle_age_ms,
            "Dislocation detected (P0-24: HIP-3 2x fee applied)"
        );

        Some(DislocationSignal::new(
            key,
            OrderSide::Buy,
            raw_edge_bps,
            net_edge_bps,
            strength,
            suggested_size,
            oracle,
            ask,
            ask_size,
            fee_metadata,
        ))
    }

    /// Check for sell opportunity.
    ///
    /// Sell when: best_bid >= oracle * (1 + cost_threshold)
    ///
    /// P0-24: Uses FeeCalculator with HIP-3 2x multiplier.
    ///
    /// Oracle Filters (when enabled):
    /// - Direction: Only generates signal if oracle is falling (stale bid)
    /// - Velocity: Only generates signal if oracle moved enough (min_oracle_change_bps)
    /// - Consecutive: Only generates signal if oracle has moved N times in same direction
    /// - Quote Lag: Only generates signal if oracle age is within configured window
    fn check_sell(
        &self,
        key: MarketKey,
        snapshot: &MarketSnapshot,
        threshold_override_bps: Option<Decimal>,
        oracle_movement: OracleMovement,
        oracle_tracker: Option<&OracleMovementTracker>,
        oracle_age_ms: Option<i64>,
    ) -> Option<DislocationSignal> {
        // P0-14: Check if BBO is tradeable
        if !snapshot.is_tradeable() {
            return None;
        }

        let oracle = snapshot.ctx.oracle.oracle_px;
        let bid = snapshot.bbo.bid_price;
        let bid_size = snapshot.bbo.bid_size;

        if oracle.is_zero() || bid.is_zero() {
            return None;
        }

        // Calculate raw edge: (bid - oracle) / oracle * 10000
        let raw_edge_bps = (bid.inner() - oracle.inner()) / oracle.inner() * Decimal::from(10000);

        // Only proceed if bid is actually above oracle (positive edge)
        if raw_edge_bps <= Decimal::ZERO {
            return None;
        }

        // Use per-market threshold if provided, otherwise use FeeCalculator's total_cost
        let total_cost =
            threshold_override_bps.unwrap_or_else(|| self.fee_calculator.total_cost_bps());
        let net_edge_bps = raw_edge_bps - total_cost;

        // Check if edge is sufficient
        let strength = SignalStrength::from_edge(raw_edge_bps, total_cost)?;

        // Oracle Direction Filter: Sell only when oracle is falling (stale bid)
        // This filters out signals caused by oracle lagging in uptrend
        if self.config.oracle_direction_filter
            && !self.is_direction_compatible(OrderSide::Sell, oracle_movement.direction)
        {
            tracing::debug!(
                %key,
                side = "sell",
                raw_edge_bps = %raw_edge_bps,
                oracle_direction = ?oracle_movement.direction,
                "Signal skipped: oracle not falling (oracle lag in uptrend)"
            );
            return None;
        }

        // Oracle Velocity Filter: Skip if oracle movement is too small
        // Small movements are likely noise, MM will quickly follow
        if oracle_movement.change_bps < self.config.min_oracle_change_bps {
            tracing::debug!(
                %key,
                side = "sell",
                raw_edge_bps = %raw_edge_bps,
                oracle_change_bps = %oracle_movement.change_bps,
                min_oracle_change_bps = %self.config.min_oracle_change_bps,
                "Signal skipped: oracle movement too small"
            );
            return None;
        }

        // Oracle Consecutive Filter: Skip if oracle hasn't moved enough times in same direction
        // Multiple consecutive moves indicate a real trend that MMs haven't caught up with
        if self.config.min_consecutive_oracle_moves > 0 {
            if let Some(tracker) = oracle_tracker {
                let consecutive_down = tracker.consecutive(&key, MoveDirection::Down);
                if consecutive_down < self.config.min_consecutive_oracle_moves {
                    trace!(
                        %key,
                        side = "sell",
                        raw_edge_bps = %raw_edge_bps,
                        consecutive_down = consecutive_down,
                        required = self.config.min_consecutive_oracle_moves,
                        "Signal skipped: oracle not consistently falling"
                    );
                    return None;
                }
            }
        }

        // Quote Lag Gate: Only generate signal when oracle changed within time window
        // This ensures "true stale liquidity" - oracle moved but MM hasn't caught up yet
        if let Some(age_ms) = oracle_age_ms {
            // Check minimum lag (filter noise from micro-movements)
            if self.config.min_quote_lag_ms > 0 && age_ms < self.config.min_quote_lag_ms {
                tracing::debug!(
                    %key,
                    side = "sell",
                    raw_edge_bps = %raw_edge_bps,
                    oracle_age_ms = age_ms,
                    min_quote_lag_ms = self.config.min_quote_lag_ms,
                    "Signal skipped: oracle moved too recently (noise filter)"
                );
                return None;
            }

            // Check maximum lag (filter stale - MM caught up)
            if self.config.max_quote_lag_ms > 0 && age_ms > self.config.max_quote_lag_ms {
                tracing::debug!(
                    %key,
                    side = "sell",
                    raw_edge_bps = %raw_edge_bps,
                    oracle_age_ms = age_ms,
                    max_quote_lag_ms = self.config.max_quote_lag_ms,
                    "Signal skipped: oracle moved too long ago (MM caught up)"
                );
                return None;
            }
        }

        // Calculate suggested size (may return ZERO if liquidity too low)
        let suggested_size = self.calculate_size(snapshot, OrderSide::Sell);

        // Skip signal if size is zero (low liquidity)
        if suggested_size.is_zero() {
            tracing::debug!(
                %key,
                side = "sell",
                raw_edge_bps = %raw_edge_bps,
                "Signal skipped due to low liquidity"
            );
            return None;
        }

        // P0-24: Generate fee metadata for audit trail
        let fee_metadata = self.fee_calculator.metadata();

        info!(
            %key,
            side = "sell",
            raw_edge_bps = %raw_edge_bps,
            net_edge_bps = %net_edge_bps,
            total_cost_bps = %total_cost,
            effective_fee_bps = %fee_metadata.effective_taker_fee_bps,
            oracle = %oracle,
            bid = %bid,
            oracle_direction = ?oracle_movement.direction,
            oracle_change_bps = %oracle_movement.change_bps,
            ?oracle_age_ms,
            "Dislocation detected (P0-24: HIP-3 2x fee applied)"
        );

        Some(DislocationSignal::new(
            key,
            OrderSide::Sell,
            raw_edge_bps,
            net_edge_bps,
            strength,
            suggested_size,
            oracle,
            bid,
            bid_size,
            fee_metadata,
        ))
    }

    /// Calculate liquidity adjustment factor (0.0 ~ 1.0).
    ///
    /// - Below min_book_notional: returns 0.0 (skip signal)
    /// - Between min and normal: linear interpolation
    /// - Above normal_book_notional: returns 1.0 (full size)
    fn liquidity_factor(&self, book_notional: Decimal) -> Decimal {
        let min = self.config.min_book_notional;
        let normal = self.config.normal_book_notional;

        if book_notional >= normal {
            Decimal::ONE
        } else if book_notional <= min {
            Decimal::ZERO
        } else {
            // Linear interpolation: (book_notional - min) / (normal - min)
            (book_notional - min) / (normal - min)
        }
    }

    /// Calculate suggested trade size with liquidity adjustment.
    ///
    /// size = clamp(alpha * liquidity_factor * top_of_book_size, min_notional, max_notional) / mid_price
    ///
    /// Returns Size::ZERO if liquidity is below minimum threshold.
    fn calculate_size(&self, snapshot: &MarketSnapshot, side: OrderSide) -> Size {
        // P0-14: mid_price() now returns Option<Price>
        let mid = match snapshot.bbo.mid_price() {
            Some(m) if !m.is_zero() => m,
            _ => return Size::ZERO,
        };

        // Side-aware book size and price
        // Buy: take liquidity from ask side, Sell: from bid side
        let (book_size, book_price) = match side {
            OrderSide::Buy => (snapshot.bbo.ask_size, snapshot.bbo.ask_price),
            OrderSide::Sell => (snapshot.bbo.bid_size, snapshot.bbo.bid_price),
        };

        // Calculate book notional using side's price (not mid)
        // This provides more accurate liquidity assessment
        let book_notional = book_size.inner() * book_price.inner();

        // Calculate liquidity factor (0.0 ~ 1.0)
        let liquidity_factor = self.liquidity_factor(book_notional);
        if liquidity_factor.is_zero() {
            tracing::debug!(
                %book_notional,
                min = %self.config.min_book_notional,
                normal = %self.config.normal_book_notional,
                %liquidity_factor,
                "Liquidity too low, skipping signal"
            );
            return Size::ZERO;
        }

        // Adjusted alpha (scaled by liquidity)
        let adjusted_alpha = self.config.sizing_alpha * liquidity_factor;

        tracing::debug!(
            %book_notional,
            %liquidity_factor,
            %adjusted_alpha,
            "Liquidity factor applied"
        );

        // Alpha-scaled book size
        let alpha_size = Size::new(book_size.inner() * adjusted_alpha);

        // Max notional size with 1% buffer to avoid boundary rejection at executor
        // (executor uses mark_px which may differ slightly from mid_price)
        let buffer_factor = Decimal::new(99, 2); // 0.99
        let max_size = Size::new((self.config.max_notional * buffer_factor) / mid.inner());

        // Min notional size (to avoid minTradeNtlRejected from exchange)
        let min_size = if self.config.min_order_notional.is_zero() {
            Size::ZERO
        } else {
            Size::new(self.config.min_order_notional / mid.inner())
        };

        // Clamp: max(min_size, min(alpha_size, max_size))
        let clamped_size = if alpha_size.inner() > max_size.inner() {
            max_size
        } else if alpha_size.inner() < min_size.inner() {
            tracing::debug!(
                alpha_notional = %alpha_size.inner() * mid.inner(),
                min_order_notional = %self.config.min_order_notional,
                "Boosting size to min_order_notional"
            );
            min_size
        } else {
            alpha_size
        };

        clamped_size
    }

    /// Get current configuration.
    pub fn config(&self) -> &DetectorConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetCtx, AssetId, Bbo, DexId, OracleData, Price, Size};
    use rust_decimal_macros::dec;

    fn test_key() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    fn make_snapshot(oracle: Decimal, bid: Decimal, ask: Decimal) -> MarketSnapshot {
        make_snapshot_with_size(oracle, bid, ask, dec!(1), dec!(1))
    }

    fn make_snapshot_with_size(
        oracle: Decimal,
        bid: Decimal,
        ask: Decimal,
        bid_size: Decimal,
        ask_size: Decimal,
    ) -> MarketSnapshot {
        let bbo = Bbo::new(
            Price::new(bid),
            Size::new(bid_size),
            Price::new(ask),
            Size::new(ask_size),
        );
        let oracle_data = OracleData::new(Price::new(oracle), Price::new(oracle));
        let ctx = AssetCtx::new(oracle_data, dec!(0.0001));
        MarketSnapshot::new(bbo, ctx)
    }

    #[test]
    fn test_no_dislocation() {
        // P0-24: Using custom user fees to control effective fee
        let user_fees = UserFees {
            taker_bps: dec!(2), // Base 2 bps -> HIP-3 2x = 4 bps effective
            ..Default::default()
        };
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(5),
            ..Default::default()
        };
        // Total cost = 4 (effective) + 2 (slip) + 5 (min_edge) = 11 bps
        let detector = DislocationDetector::with_user_fees(config, user_fees).unwrap();
        let key = test_key();

        // Oracle at 50000, bid/ask around it with normal spread
        let snapshot = make_snapshot(dec!(50000), dec!(49990), dec!(50010));

        let signal = detector.check(key, &snapshot, None, None, None);
        assert!(signal.is_none());
    }

    #[test]
    fn test_buy_dislocation() {
        // P0-24: Using custom user fees
        let user_fees = UserFees {
            taker_bps: dec!(2), // Base 2 bps -> HIP-3 2x = 4 bps effective
            ..Default::default()
        };
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: false, // Disable direction filter
            min_oracle_change_bps: dec!(0), // Disable velocity filter
            ..Default::default()
        };
        // Total cost = 4 (effective) + 2 (slip) + 4 (min_edge) = 10 bps
        let detector = DislocationDetector::with_user_fees(config, user_fees).unwrap();
        let key = test_key();

        // Oracle at 50000, ask at 49940 (12 bps below = edge after 10 bps cost)
        // Edge = (50000 - 49940) / 50000 * 10000 = 12 bps
        let snapshot = make_snapshot(dec!(50000), dec!(49920), dec!(49940));

        let signal = detector.check(key, &snapshot, None, None, None);
        assert!(signal.is_some());

        let signal = signal.unwrap();
        assert_eq!(signal.side, OrderSide::Buy);
        assert!(signal.raw_edge_bps > dec!(10));

        // P0-24: Verify fee metadata is present
        assert_eq!(signal.fee_metadata.base_taker_fee_bps, dec!(2));
        assert_eq!(signal.fee_metadata.hip3_multiplier, dec!(2));
        assert_eq!(signal.fee_metadata.effective_taker_fee_bps, dec!(4));
        assert_eq!(signal.fee_metadata.total_cost_bps, dec!(10));
    }

    #[test]
    fn test_sell_dislocation() {
        // P0-24: Using custom user fees
        let user_fees = UserFees {
            taker_bps: dec!(2), // Base 2 bps -> HIP-3 2x = 4 bps effective
            ..Default::default()
        };
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: false, // Disable direction filter
            min_oracle_change_bps: dec!(0), // Disable velocity filter
            ..Default::default()
        };
        // Total cost = 4 (effective) + 2 (slip) + 4 (min_edge) = 10 bps
        let detector = DislocationDetector::with_user_fees(config, user_fees).unwrap();
        let key = test_key();

        // Oracle at 50000, bid at 50060 (12 bps above = edge after 10 bps cost)
        let snapshot = make_snapshot(dec!(50000), dec!(50060), dec!(50080));

        let signal = detector.check(key, &snapshot, None, None, None);
        assert!(signal.is_some());

        let signal = signal.unwrap();
        assert_eq!(signal.side, OrderSide::Sell);
        assert!(signal.raw_edge_bps > dec!(10));

        // P0-24: Verify fee metadata is present
        assert_eq!(signal.fee_metadata.effective_taker_fee_bps, dec!(4));
    }

    #[test]
    fn test_size_calculation() {
        let config = DetectorConfig {
            sizing_alpha: dec!(0.10),
            max_notional: dec!(1000),
            min_order_notional: dec!(0), // Disable min to test alpha/max clamping
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();

        // Book size = 1 BTC, mid = 50000
        // Alpha size = 0.1 * 1 = 0.1 BTC = $5000
        // Max size = 1000 * 0.99 / 50000 = 0.0198 BTC (with 1% buffer)
        // Result should be 0.0198 (clamped to max)
        let snapshot = make_snapshot(dec!(50000), dec!(49990), dec!(50010));
        let size = detector.calculate_size(&snapshot, OrderSide::Buy);

        assert_eq!(size.inner(), dec!(0.0198));
    }

    #[test]
    fn test_hip3_2x_fee_multiplier() {
        // P0-24: Verify HIP-3 2x multiplier is applied correctly
        let user_fees = UserFees {
            taker_bps: dec!(3), // VIP rate: 3 bps base -> 6 bps effective
            maker_bps: dec!(1),
            is_vip: true,
            tier: "vip1".to_string(),
        };
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(2),
            ..Default::default()
        };
        // Total cost = 6 (effective) + 2 (slip) + 2 (min_edge) = 10 bps
        let detector = DislocationDetector::with_user_fees(config, user_fees).unwrap();

        assert_eq!(detector.fee_calculator().effective_taker_fee_bps(), dec!(6));
        assert_eq!(detector.fee_calculator().total_cost_bps(), dec!(10));
    }

    #[test]
    fn test_fee_metadata_audit_trail() {
        // P0-24: Verify fee metadata provides full audit trail
        let user_fees = UserFees {
            taker_bps: dec!(2),
            maker_bps: dec!(1),
            is_vip: false,
            tier: "default".to_string(),
        };
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(5),
            ..Default::default()
        };
        let detector = DislocationDetector::with_user_fees(config, user_fees).unwrap();
        let metadata = detector.fee_calculator().metadata();

        // Full audit trail
        assert_eq!(metadata.base_taker_fee_bps, dec!(2));
        assert_eq!(metadata.hip3_multiplier, dec!(2));
        assert_eq!(metadata.effective_taker_fee_bps, dec!(4));
        assert_eq!(metadata.slippage_bps, dec!(2));
        assert_eq!(metadata.min_edge_bps, dec!(5));
        assert_eq!(metadata.total_cost_bps, dec!(11));
    }

    #[test]
    fn test_update_user_fees() {
        // P0-24: Verify user fees can be updated at runtime
        let config = DetectorConfig::default();
        let mut detector = DislocationDetector::new(config).unwrap();

        // Initial: default fees (2 bps base -> 4 bps effective)
        assert_eq!(detector.fee_calculator().effective_taker_fee_bps(), dec!(4));

        // Update to VIP fees (1.5 bps base -> 3 bps effective)
        let vip_fees = UserFees {
            taker_bps: dec!(1.5),
            maker_bps: dec!(0.5),
            is_vip: true,
            tier: "vip1".to_string(),
        };
        detector.update_user_fees(vip_fees);

        assert_eq!(detector.fee_calculator().effective_taker_fee_bps(), dec!(3));
    }

    #[test]
    fn test_null_bbo_rejected() {
        // P0-14: Verify null BBO markets are rejected
        let config = DetectorConfig::default();
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        // Null BBO: no bid
        let bbo = Bbo::new(
            Price::ZERO, // No bid
            Size::ZERO,
            Price::new(dec!(50010)),
            Size::new(dec!(1)),
        );
        let oracle_data = OracleData::new(Price::new(dec!(50000)), Price::new(dec!(50000)));
        let ctx = AssetCtx::new(oracle_data, dec!(0.0001));
        let snapshot = MarketSnapshot::new(bbo, ctx);

        // Should return None even if ask is below oracle
        let signal = detector.check(key, &snapshot, None, None, None);
        assert!(signal.is_none());
    }

    // ==========================================
    // Liquidity Factor Tests
    // ==========================================

    #[test]
    fn test_liquidity_factor_below_minimum() {
        // Book notional < min_book_notional -> factor = 0
        let config = DetectorConfig {
            min_book_notional: dec!(500),
            normal_book_notional: dec!(5000),
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();

        // $300 book notional (below $500 min)
        assert_eq!(detector.liquidity_factor(dec!(300)), Decimal::ZERO);
        // $500 exactly (at boundary, should be 0)
        assert_eq!(detector.liquidity_factor(dec!(500)), Decimal::ZERO);
    }

    #[test]
    fn test_liquidity_factor_above_normal() {
        // Book notional >= normal_book_notional -> factor = 1.0
        let config = DetectorConfig {
            min_book_notional: dec!(500),
            normal_book_notional: dec!(5000),
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();

        // $5000 exactly (at boundary)
        assert_eq!(detector.liquidity_factor(dec!(5000)), Decimal::ONE);
        // $10000 (above normal)
        assert_eq!(detector.liquidity_factor(dec!(10000)), Decimal::ONE);
    }

    #[test]
    fn test_liquidity_factor_interpolation() {
        // Book notional between min and normal -> linear interpolation
        // Factor = (book_notional - min) / (normal - min)
        let config = DetectorConfig {
            min_book_notional: dec!(500),
            normal_book_notional: dec!(5000),
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();

        // $1000: (1000-500)/(5000-500) = 500/4500 ≈ 0.111
        let factor_1000 = detector.liquidity_factor(dec!(1000));
        assert!(factor_1000 > dec!(0.11) && factor_1000 < dec!(0.12));

        // $2750 (midpoint): (2750-500)/(5000-500) = 2250/4500 = 0.5
        let factor_2750 = detector.liquidity_factor(dec!(2750));
        assert_eq!(factor_2750, dec!(0.5));

        // $3000: (3000-500)/(5000-500) = 2500/4500 ≈ 0.556
        let factor_3000 = detector.liquidity_factor(dec!(3000));
        assert!(factor_3000 > dec!(0.55) && factor_3000 < dec!(0.56));
    }

    #[test]
    fn test_low_liquidity_skips_signal() {
        // When book notional is below min, signal should be skipped
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            min_book_notional: dec!(500),
            normal_book_notional: dec!(5000),
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        // Oracle at 50000, ask at 49940 (12 bps edge - would normally trigger)
        // Book size = 0.005 BTC, book_notional = 0.005 * 50000 = $250 (below min)
        let snapshot = make_snapshot_with_size(
            dec!(50000),
            dec!(49920),
            dec!(49940),
            dec!(0.005),
            dec!(0.005),
        );

        let signal = detector.check(key, &snapshot, None, None, None);
        assert!(
            signal.is_none(),
            "Signal should be skipped due to low liquidity"
        );
    }

    #[test]
    fn test_partial_liquidity_reduces_size() {
        // When book notional is between min and normal, size should be reduced
        let config = DetectorConfig {
            sizing_alpha: dec!(0.10),
            max_notional: dec!(10000), // High max so alpha is limiting factor
            min_book_notional: dec!(500),
            normal_book_notional: dec!(5000),
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();

        // Ask price = $50010
        // Book size = 0.055 BTC -> book_notional = 0.055 * 50010 = $2750.55
        // (Changed from mid_price to ask_price for Buy side)
        // Liquidity factor = (2750.55 - 500) / (5000 - 500) ≈ 0.5001
        // Adjusted alpha = 0.10 * 0.5001 = 0.05001
        // Alpha size = 0.055 * 0.05001 ≈ 0.002750 BTC
        let snapshot = make_snapshot_with_size(
            dec!(50000),
            dec!(49990),
            dec!(50010),
            dec!(0.055),
            dec!(0.055),
        );

        let size = detector.calculate_size(&snapshot, OrderSide::Buy);

        // Expected: approximately 0.00275 (within 1% tolerance due to price-based calculation)
        let expected = dec!(0.00275);
        let diff = (size.inner() - expected).abs();
        assert!(
            diff < dec!(0.00003),
            "Size {} should be close to {} (diff: {})",
            size.inner(),
            expected,
            diff
        );
    }

    #[test]
    fn test_sell_side_uses_bid_price_for_book_notional() {
        // Verify Sell side uses bid_price × bid_size for book_notional
        let config = DetectorConfig {
            sizing_alpha: dec!(0.10),
            max_notional: dec!(10000),
            min_book_notional: dec!(500),
            normal_book_notional: dec!(5000),
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();

        // Bid price = $49990, Ask price = $50010
        // Sell side should use bid_price for book_notional
        // book_size = 0.055 BTC → book_notional = 0.055 * 49990 = $2749.45
        // liquidity_factor = (2749.45 - 500) / (5000 - 500) ≈ 0.4999
        let snapshot = make_snapshot_with_size(
            dec!(50000),
            dec!(49990),
            dec!(50010),
            dec!(0.055),
            dec!(0.055),
        );

        let size = detector.calculate_size(&snapshot, OrderSide::Sell);

        // Expected: approximately 0.00275 (similar to buy but using bid_price)
        let expected = dec!(0.00275);
        let diff = (size.inner() - expected).abs();
        assert!(
            diff < dec!(0.00003),
            "Size {} should be close to {} (diff: {})",
            size.inner(),
            expected,
            diff
        );
    }

    #[test]
    fn test_full_liquidity_no_reduction() {
        // When book notional >= normal, size should not be reduced
        let config = DetectorConfig {
            sizing_alpha: dec!(0.10),
            max_notional: dec!(10000), // High max so alpha is limiting factor
            min_book_notional: dec!(500),
            normal_book_notional: dec!(5000),
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();

        // Mid price = $50000
        // Book size = 0.2 BTC -> book_notional = 0.2 * 50000 = $10000 (above normal)
        // Liquidity factor = 1.0
        // Alpha size = 0.2 * 0.10 = 0.02 BTC
        let snapshot =
            make_snapshot_with_size(dec!(50000), dec!(49990), dec!(50010), dec!(0.2), dec!(0.2));

        let size = detector.calculate_size(&snapshot, OrderSide::Buy);

        // Expected: 0.2 * 0.10 * 1.0 = 0.02
        assert_eq!(size.inner(), dec!(0.02));
    }

    #[test]
    fn test_signal_with_partial_liquidity() {
        // Verify signal is generated but with reduced size
        let user_fees = UserFees {
            taker_bps: dec!(2),
            ..Default::default()
        };
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            sizing_alpha: dec!(0.10),
            max_notional: dec!(10000),
            min_book_notional: dec!(500),
            normal_book_notional: dec!(5000),
            oracle_direction_filter: false, // Disable direction filter
            min_oracle_change_bps: dec!(0), // Disable velocity filter
            ..Default::default()
        };
        let detector = DislocationDetector::with_user_fees(config, user_fees).unwrap();
        let key = test_key();

        // Oracle at 50000, ask at 49930 (14 bps edge - enough to trigger)
        // bid=49920, ask=49930 -> mid=49925
        // Total cost = 4 + 2 + 4 = 10 bps, so 14 bps edge is sufficient
        // Book size = 0.055 BTC -> book_notional = 0.055 * 49925 ≈ $2745.875
        // Liquidity factor = (2745.875 - 500) / (5000 - 500) ≈ 0.499
        let snapshot = make_snapshot_with_size(
            dec!(50000),
            dec!(49920),
            dec!(49930),
            dec!(0.055),
            dec!(0.055),
        );

        let signal = detector.check(key, &snapshot, None, None, None);
        assert!(
            signal.is_some(),
            "Signal should be generated with partial liquidity"
        );

        let signal = signal.unwrap();
        // Size should be reduced by liquidity factor (roughly 50%)
        // Full size = 0.055 * 0.10 = 0.0055
        // With ~50% liquidity factor: ~0.00275
        assert!(
            signal.suggested_size.inner() > dec!(0.002)
                && signal.suggested_size.inner() < dec!(0.003),
            "Size should be reduced by liquidity factor, got: {}",
            signal.suggested_size.inner()
        );
    }

    /// Test per-market threshold override.
    /// When threshold_override_bps is provided, it should be used instead of
    /// the FeeCalculator's total_cost_bps.
    #[test]
    fn test_threshold_override() {
        // Default config: taker_fee=4, slippage=2, min_edge=4 -> total 10bps
        let config = DetectorConfig {
            taker_fee_bps: dec!(4),
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            sizing_alpha: dec!(0.1),
            max_notional: dec!(1000),
            min_book_notional: dec!(100),
            normal_book_notional: dec!(1000),
            oracle_direction_filter: false, // Disable direction filter
            min_oracle_change_bps: dec!(0), // Disable velocity filter
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = MarketKey::from_indices(1, 0);

        // Snapshot with 15bps edge: oracle=50000, ask=49925 -> edge = 75/50000*10000 = 15bps
        let snapshot = make_snapshot(dec!(50000), dec!(49925), dec!(49930));

        // Test 1: Without override, signal should be generated (15bps > 10bps threshold)
        let signal = detector.check(key, &snapshot, None, None, None);
        assert!(
            signal.is_some(),
            "Signal should be generated without override (15bps > 10bps)"
        );

        // Test 2: With higher threshold override (20bps), no signal should be generated
        let signal = detector.check(key, &snapshot, Some(dec!(20)), None, None);
        assert!(
            signal.is_none(),
            "No signal should be generated with 20bps threshold (15bps < 20bps)"
        );

        // Test 3: With lower threshold override (12bps), signal should be generated
        let signal = detector.check(key, &snapshot, Some(dec!(12)), None, None);
        assert!(
            signal.is_some(),
            "Signal should be generated with 12bps threshold (15bps > 12bps)"
        );
    }

    // ==========================================
    // Oracle Direction Filter Tests
    // ==========================================

    #[test]
    fn test_oracle_direction_filter_buy_rising() {
        // Buy signal should be generated when oracle is rising (stale ask)
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: true, // Enable filter
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        // First tick: establish baseline oracle at 49900
        let snapshot1 = make_snapshot(dec!(49900), dec!(49880), dec!(49890));
        let _ = detector.check(key, &snapshot1, None, None, None);

        // Second tick: oracle rises to 50000, ask still at 49940 (stale)
        // Edge = (50000 - 49940) / 50000 * 10000 = 12 bps
        let snapshot2 = make_snapshot(dec!(50000), dec!(49920), dec!(49940));
        let signal = detector.check(key, &snapshot2, None, None, None);

        assert!(
            signal.is_some(),
            "Buy signal should be generated when oracle is rising"
        );
        assert_eq!(signal.unwrap().side, OrderSide::Buy);
    }

    #[test]
    fn test_oracle_direction_filter_buy_falling_skipped() {
        // Buy signal should be skipped when oracle is falling (oracle lag)
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: true, // Enable filter
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        // First tick: establish baseline oracle at 50100
        let snapshot1 = make_snapshot(dec!(50100), dec!(50080), dec!(50090));
        let _ = detector.check(key, &snapshot1, None, None, None);

        // Second tick: oracle falls to 50000, ask at 49940 (looks like edge but oracle lagging)
        // This is oracle lagging in downtrend - should skip
        let snapshot2 = make_snapshot(dec!(50000), dec!(49920), dec!(49940));
        let signal = detector.check(key, &snapshot2, None, None, None);

        assert!(
            signal.is_none(),
            "Buy signal should be skipped when oracle is falling (oracle lag in downtrend)"
        );
    }

    #[test]
    fn test_oracle_direction_filter_sell_falling() {
        // Sell signal should be generated when oracle is falling (stale bid)
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: true, // Enable filter
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        // First tick: establish baseline oracle at 50100
        let snapshot1 = make_snapshot(dec!(50100), dec!(50080), dec!(50090));
        let _ = detector.check(key, &snapshot1, None, None, None);

        // Second tick: oracle falls to 50000, bid still at 50060 (stale)
        // Edge = (50060 - 50000) / 50000 * 10000 = 12 bps
        let snapshot2 = make_snapshot(dec!(50000), dec!(50060), dec!(50080));
        let signal = detector.check(key, &snapshot2, None, None, None);

        assert!(
            signal.is_some(),
            "Sell signal should be generated when oracle is falling"
        );
        assert_eq!(signal.unwrap().side, OrderSide::Sell);
    }

    #[test]
    fn test_oracle_direction_filter_sell_rising_skipped() {
        // Sell signal should be skipped when oracle is rising (oracle lag)
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: true, // Enable filter
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        // First tick: establish baseline oracle at 49900
        let snapshot1 = make_snapshot(dec!(49900), dec!(49880), dec!(49890));
        let _ = detector.check(key, &snapshot1, None, None, None);

        // Second tick: oracle rises to 50000, bid at 50060 (looks like edge but oracle lagging)
        // This is oracle lagging in uptrend - should skip
        let snapshot2 = make_snapshot(dec!(50000), dec!(50060), dec!(50080));
        let signal = detector.check(key, &snapshot2, None, None, None);

        assert!(
            signal.is_none(),
            "Sell signal should be skipped when oracle is rising (oracle lag in uptrend)"
        );
    }

    #[test]
    fn test_oracle_direction_filter_unchanged_skipped() {
        // Signal should be skipped when oracle is unchanged (first tick or no movement)
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: true, // Enable filter
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        // First tick with edge - should be skipped (no previous oracle)
        let snapshot = make_snapshot(dec!(50000), dec!(49920), dec!(49940));
        let signal = detector.check(key, &snapshot, None, None, None);

        assert!(
            signal.is_none(),
            "Signal should be skipped on first tick (no oracle direction)"
        );
    }

    #[test]
    fn test_oracle_direction_filter_disabled() {
        // When filter is disabled, signals should be generated regardless of direction
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: false, // Disable direction filter
            min_oracle_change_bps: dec!(0), // Disable velocity filter
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        // First tick with edge - should NOT be skipped when filter disabled
        let snapshot = make_snapshot(dec!(50000), dec!(49920), dec!(49940));
        let signal = detector.check(key, &snapshot, None, None, None);

        assert!(
            signal.is_some(),
            "Signal should be generated when filter is disabled"
        );
    }

    // ==========================================
    // Oracle Velocity Filter Tests
    // ==========================================

    #[test]
    fn test_oracle_velocity_filter_sufficient_movement() {
        // Signal should be generated when oracle moves enough
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: true,
            min_oracle_change_bps: dec!(5), // 5 bps minimum movement
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        // First tick: establish baseline oracle at 49900
        let snapshot1 = make_snapshot(dec!(49900), dec!(49880), dec!(49890));
        let _ = detector.check(key, &snapshot1, None, None, None);

        // Second tick: oracle rises by 200 bps (49900 -> 50000 = 0.2%)
        // That's 200 bps > 5 bps min, should pass velocity filter
        let snapshot2 = make_snapshot(dec!(50000), dec!(49920), dec!(49940));
        let signal = detector.check(key, &snapshot2, None, None, None);

        assert!(
            signal.is_some(),
            "Signal should be generated when oracle moves enough (200 bps > 5 bps min)"
        );
    }

    #[test]
    fn test_oracle_velocity_filter_insufficient_movement() {
        // Signal should be skipped when oracle movement is too small
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: true,
            min_oracle_change_bps: dec!(10), // 10 bps minimum movement
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        // First tick: establish baseline oracle at 49995
        let snapshot1 = make_snapshot(dec!(49995), dec!(49975), dec!(49985));
        let _ = detector.check(key, &snapshot1, None, None, None);

        // Second tick: oracle rises by only 1 bps (49995 -> 50000 = 0.01%)
        // That's ~1 bps < 10 bps min, should NOT pass velocity filter
        // Edge = (50000 - 49940) / 50000 * 10000 = 12 bps (sufficient edge)
        let snapshot2 = make_snapshot(dec!(50000), dec!(49920), dec!(49940));
        let signal = detector.check(key, &snapshot2, None, None, None);

        assert!(
            signal.is_none(),
            "Signal should be skipped when oracle movement too small (1 bps < 10 bps min)"
        );
    }

    #[test]
    fn test_oracle_velocity_filter_zero_disables() {
        // When min_oracle_change_bps is 0, velocity filter is disabled
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: false, // Disable direction filter too
            min_oracle_change_bps: dec!(0), // Disable velocity filter
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        // First tick with edge - no previous oracle, change_bps = 0
        // But since min_oracle_change_bps = 0, should still generate signal
        let snapshot = make_snapshot(dec!(50000), dec!(49920), dec!(49940));
        let signal = detector.check(key, &snapshot, None, None, None);

        assert!(
            signal.is_some(),
            "Signal should be generated when velocity filter is disabled (min=0)"
        );
    }

    #[test]
    fn test_oracle_velocity_combined_with_direction() {
        // Both filters must pass for signal to be generated
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: true,
            min_oracle_change_bps: dec!(5), // 5 bps minimum
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        // First tick: establish baseline
        let snapshot1 = make_snapshot(dec!(50000), dec!(49980), dec!(49990));
        let _ = detector.check(key, &snapshot1, None, None, None);

        // Second tick: oracle FALLS by 200 bps (50000 -> 49900)
        // Velocity passes (200 bps > 5 bps)
        // But for BUY signal, direction is WRONG (falling, not rising)
        // Ask is below oracle (edge exists), but direction filter should block
        let snapshot2 = make_snapshot(dec!(49900), dec!(49820), dec!(49840));
        let signal = detector.check(key, &snapshot2, None, None, None);

        assert!(
            signal.is_none(),
            "Buy signal should be skipped: velocity OK but direction wrong (falling)"
        );
    }

    // ==========================================
    // Quote Lag Gate Tests
    // ==========================================

    #[test]
    fn test_quote_lag_gate_disabled_by_default() {
        // When both min and max are 0, gate should be disabled
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: false,
            min_oracle_change_bps: dec!(0),
            min_quote_lag_ms: 0, // Disabled
            max_quote_lag_ms: 0, // Disabled
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        // Signal should pass with any oracle_age_ms
        let snapshot = make_snapshot(dec!(50000), dec!(49920), dec!(49940));
        let signal = detector.check(key, &snapshot, None, None, Some(100));
        assert!(
            signal.is_some(),
            "Signal should pass when quote lag gate is disabled"
        );
    }

    #[test]
    fn test_quote_lag_gate_min_blocks_too_fresh() {
        // When oracle age < min_quote_lag_ms, signal should be blocked
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: false,
            min_oracle_change_bps: dec!(0),
            min_quote_lag_ms: 50, // Minimum 50ms
            max_quote_lag_ms: 0,  // No upper bound
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        let snapshot = make_snapshot(dec!(50000), dec!(49920), dec!(49940));

        // Too fresh (30ms < 50ms min) - should be blocked
        let signal = detector.check(key, &snapshot, None, None, Some(30));
        assert!(
            signal.is_none(),
            "Signal should be blocked when oracle moved too recently (30ms < 50ms min)"
        );

        // At threshold (50ms == 50ms min) - should pass
        let signal = detector.check(key, &snapshot, None, None, Some(50));
        assert!(
            signal.is_some(),
            "Signal should pass when oracle age equals min (50ms)"
        );

        // Above threshold (100ms > 50ms min) - should pass
        let signal = detector.check(key, &snapshot, None, None, Some(100));
        assert!(
            signal.is_some(),
            "Signal should pass when oracle age above min (100ms > 50ms)"
        );
    }

    #[test]
    fn test_quote_lag_gate_max_blocks_too_stale() {
        // When oracle age > max_quote_lag_ms, signal should be blocked
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: false,
            min_oracle_change_bps: dec!(0),
            min_quote_lag_ms: 0,   // No lower bound
            max_quote_lag_ms: 500, // Maximum 500ms
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        let snapshot = make_snapshot(dec!(50000), dec!(49920), dec!(49940));

        // Too stale (1000ms > 500ms max) - should be blocked
        let signal = detector.check(key, &snapshot, None, None, Some(1000));
        assert!(
            signal.is_none(),
            "Signal should be blocked when oracle moved too long ago (1000ms > 500ms max)"
        );

        // At threshold (500ms == 500ms max) - should pass
        let signal = detector.check(key, &snapshot, None, None, Some(500));
        assert!(
            signal.is_some(),
            "Signal should pass when oracle age equals max (500ms)"
        );

        // Below threshold (200ms < 500ms max) - should pass
        let signal = detector.check(key, &snapshot, None, None, Some(200));
        assert!(
            signal.is_some(),
            "Signal should pass when oracle age below max (200ms < 500ms)"
        );
    }

    #[test]
    fn test_quote_lag_gate_window() {
        // When both min and max are set, only signals within window should pass
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: false,
            min_oracle_change_bps: dec!(0),
            min_quote_lag_ms: 50,  // Minimum 50ms
            max_quote_lag_ms: 500, // Maximum 500ms
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        let snapshot = make_snapshot(dec!(50000), dec!(49920), dec!(49940));

        // Below window (30ms < 50ms) - blocked
        let signal = detector.check(key, &snapshot, None, None, Some(30));
        assert!(signal.is_none(), "Should be blocked: below min window");

        // In window (200ms, within 50-500ms) - pass
        let signal = detector.check(key, &snapshot, None, None, Some(200));
        assert!(signal.is_some(), "Should pass: within window");

        // Above window (1000ms > 500ms) - blocked
        let signal = detector.check(key, &snapshot, None, None, Some(1000));
        assert!(signal.is_none(), "Should be blocked: above max window");
    }

    #[test]
    fn test_quote_lag_gate_none_allows() {
        // When oracle_age_ms is None, gate should be skipped
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: false,
            min_oracle_change_bps: dec!(0),
            min_quote_lag_ms: 50,  // Minimum 50ms
            max_quote_lag_ms: 500, // Maximum 500ms
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        let snapshot = make_snapshot(dec!(50000), dec!(49920), dec!(49940));

        // None should skip the gate (first tick scenario)
        let signal = detector.check(key, &snapshot, None, None, None);
        assert!(
            signal.is_some(),
            "Signal should pass when oracle_age_ms is None (gate skipped)"
        );
    }

    #[test]
    fn test_quote_lag_gate_sell_side() {
        // Verify gate works for sell signals too
        let config = DetectorConfig {
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            oracle_direction_filter: false,
            min_oracle_change_bps: dec!(0),
            min_quote_lag_ms: 50,
            max_quote_lag_ms: 500,
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = test_key();

        // Sell signal: bid above oracle
        let snapshot = make_snapshot(dec!(50000), dec!(50060), dec!(50080));

        // Too fresh - blocked
        let signal = detector.check(key, &snapshot, None, None, Some(30));
        assert!(signal.is_none(), "Sell signal should be blocked: too fresh");

        // In window - pass
        let signal = detector.check(key, &snapshot, None, None, Some(200));
        assert!(signal.is_some(), "Sell signal should pass: in window");
        assert_eq!(signal.unwrap().side, OrderSide::Sell);

        // Too stale - blocked
        let signal = detector.check(key, &snapshot, None, None, Some(1000));
        assert!(signal.is_none(), "Sell signal should be blocked: too stale");
    }
}
