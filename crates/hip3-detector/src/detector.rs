//! Dislocation detector implementation.
//!
//! Detects when best bid/ask crosses oracle price with sufficient
//! edge to cover fees and slippage.
//!
//! Implements P0-24: HIP-3 2x fee calculation with audit trail.

use crate::config::DetectorConfig;
use crate::error::DetectorError;
use crate::fee::{FeeCalculator, UserFees};
use crate::signal::{DislocationSignal, SignalStrength};
use hip3_core::types::MarketSnapshot;
use hip3_core::{MarketKey, OrderSide, Size};
use rust_decimal::Decimal;
use tracing::info;

/// Dislocation detector.
///
/// Strategy: Enter when best price crosses oracle with edge > (FEE + SLIP + EDGE).
/// - Buy: best_ask <= oracle * (1 - threshold)
/// - Sell: best_bid >= oracle * (1 + threshold)
///
/// IMPORTANT: Only trigger on best crossing oracle, not on mid divergence.
///
/// P0-24: Uses FeeCalculator for HIP-3 2x fee multiplier with full audit trail.
pub struct DislocationDetector {
    config: DetectorConfig,
    fee_calculator: FeeCalculator,
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

    /// Check for dislocation opportunity.
    ///
    /// Returns Some(signal) if a valid opportunity is detected.
    ///
    /// # Arguments
    /// * `key` - Market key
    /// * `snapshot` - Market snapshot with BBO and oracle data
    /// * `threshold_override_bps` - Optional per-market threshold in basis points.
    ///   If provided, uses this instead of fee_calculator's total_cost_bps.
    pub fn check(
        &self,
        key: MarketKey,
        snapshot: &MarketSnapshot,
        threshold_override_bps: Option<Decimal>,
    ) -> Option<DislocationSignal> {
        // Check buy opportunity: ask below oracle
        if let Some(signal) = self.check_buy(key, snapshot, threshold_override_bps) {
            return Some(signal);
        }

        // Check sell opportunity: bid above oracle
        if let Some(signal) = self.check_sell(key, snapshot, threshold_override_bps) {
            return Some(signal);
        }

        None
    }

    /// Check for buy opportunity.
    ///
    /// Buy when: best_ask <= oracle * (1 - cost_threshold)
    ///
    /// P0-24: Uses FeeCalculator with HIP-3 2x multiplier.
    fn check_buy(
        &self,
        key: MarketKey,
        snapshot: &MarketSnapshot,
        threshold_override_bps: Option<Decimal>,
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
    fn check_sell(
        &self,
        key: MarketKey,
        snapshot: &MarketSnapshot,
        threshold_override_bps: Option<Decimal>,
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

        let signal = detector.check(key, &snapshot, None);
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
            ..Default::default()
        };
        // Total cost = 4 (effective) + 2 (slip) + 4 (min_edge) = 10 bps
        let detector = DislocationDetector::with_user_fees(config, user_fees).unwrap();
        let key = test_key();

        // Oracle at 50000, ask at 49940 (12 bps below = edge after 10 bps cost)
        // Edge = (50000 - 49940) / 50000 * 10000 = 12 bps
        let snapshot = make_snapshot(dec!(50000), dec!(49920), dec!(49940));

        let signal = detector.check(key, &snapshot, None);
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
            ..Default::default()
        };
        // Total cost = 4 (effective) + 2 (slip) + 4 (min_edge) = 10 bps
        let detector = DislocationDetector::with_user_fees(config, user_fees).unwrap();
        let key = test_key();

        // Oracle at 50000, bid at 50060 (12 bps above = edge after 10 bps cost)
        let snapshot = make_snapshot(dec!(50000), dec!(50060), dec!(50080));

        let signal = detector.check(key, &snapshot, None);
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
        let signal = detector.check(key, &snapshot, None);
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

        let signal = detector.check(key, &snapshot, None);
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

        let signal = detector.check(key, &snapshot, None);
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
            ..Default::default()
        };
        let detector = DislocationDetector::new(config).unwrap();
        let key = MarketKey::from_indices(1, 0);

        // Snapshot with 15bps edge: oracle=50000, ask=49925 -> edge = 75/50000*10000 = 15bps
        let snapshot = make_snapshot(dec!(50000), dec!(49925), dec!(49930));

        // Test 1: Without override, signal should be generated (15bps > 10bps threshold)
        let signal = detector.check(key, &snapshot, None);
        assert!(
            signal.is_some(),
            "Signal should be generated without override (15bps > 10bps)"
        );

        // Test 2: With higher threshold override (20bps), no signal should be generated
        let signal = detector.check(key, &snapshot, Some(dec!(20)));
        assert!(
            signal.is_none(),
            "No signal should be generated with 20bps threshold (15bps < 20bps)"
        );

        // Test 3: With lower threshold override (12bps), signal should be generated
        let signal = detector.check(key, &snapshot, Some(dec!(12)));
        assert!(
            signal.is_some(),
            "Signal should be generated with 12bps threshold (15bps > 12bps)"
        );
    }
}
