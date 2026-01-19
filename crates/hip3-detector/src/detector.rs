//! Dislocation detector implementation.
//!
//! Detects when best bid/ask crosses oracle price with sufficient
//! edge to cover fees and slippage.
//!
//! Implements P0-24: HIP-3 2x fee calculation with audit trail.

use crate::config::DetectorConfig;
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
    pub fn new(config: DetectorConfig) -> Self {
        // P0-4: config.taker_fee_bps is effective (2x applied), derive base fee
        let user_fees = UserFees::from_effective_taker_bps(config.taker_fee_bps);
        let fee_calculator =
            FeeCalculator::new(user_fees, config.slippage_bps, config.min_edge_bps);
        Self {
            config,
            fee_calculator,
        }
    }

    /// Create detector with custom user fees.
    ///
    /// Use this when user-specific fees are available from REST API.
    pub fn with_user_fees(config: DetectorConfig, user_fees: UserFees) -> Self {
        let fee_calculator =
            FeeCalculator::new(user_fees, config.slippage_bps, config.min_edge_bps);
        Self {
            config,
            fee_calculator,
        }
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
    pub fn check(&self, key: MarketKey, snapshot: &MarketSnapshot) -> Option<DislocationSignal> {
        // Check buy opportunity: ask below oracle
        if let Some(signal) = self.check_buy(key, snapshot) {
            return Some(signal);
        }

        // Check sell opportunity: bid above oracle
        if let Some(signal) = self.check_sell(key, snapshot) {
            return Some(signal);
        }

        None
    }

    /// Check for buy opportunity.
    ///
    /// Buy when: best_ask <= oracle * (1 - cost_threshold)
    ///
    /// P0-24: Uses FeeCalculator with HIP-3 2x multiplier.
    fn check_buy(&self, key: MarketKey, snapshot: &MarketSnapshot) -> Option<DislocationSignal> {
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

        // P0-24: Use FeeCalculator for HIP-3 2x fee
        let total_cost = self.fee_calculator.total_cost_bps();
        let net_edge_bps = self.fee_calculator.net_edge_bps(raw_edge_bps);

        // Check if edge is sufficient
        let strength = SignalStrength::from_edge(raw_edge_bps, total_cost)?;

        // Calculate suggested size
        let suggested_size = self.calculate_size(snapshot, OrderSide::Buy);

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
    fn check_sell(&self, key: MarketKey, snapshot: &MarketSnapshot) -> Option<DislocationSignal> {
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

        // P0-24: Use FeeCalculator for HIP-3 2x fee
        let total_cost = self.fee_calculator.total_cost_bps();
        let net_edge_bps = self.fee_calculator.net_edge_bps(raw_edge_bps);

        // Check if edge is sufficient
        let strength = SignalStrength::from_edge(raw_edge_bps, total_cost)?;

        // Calculate suggested size
        let suggested_size = self.calculate_size(snapshot, OrderSide::Sell);

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

    /// Calculate suggested trade size.
    ///
    /// size = min(alpha * top_of_book_size, max_notional / mid_price)
    fn calculate_size(&self, snapshot: &MarketSnapshot, side: OrderSide) -> Size {
        // P0-14: mid_price() now returns Option<Price>
        let mid = match snapshot.bbo.mid_price() {
            Some(m) if !m.is_zero() => m,
            _ => return Size::ZERO,
        };

        // Top of book size
        let book_size = match side {
            OrderSide::Buy => snapshot.bbo.ask_size,
            OrderSide::Sell => snapshot.bbo.bid_size,
        };

        // Alpha-scaled book size
        let alpha_size = Size::new(book_size.inner() * self.config.sizing_alpha);

        // Max notional size
        let max_size = Size::new(self.config.max_notional / mid.inner());

        // Take minimum
        if alpha_size.inner() < max_size.inner() {
            alpha_size
        } else {
            max_size
        }
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
        let detector = DislocationDetector::with_user_fees(config, user_fees);
        let key = test_key();

        // Oracle at 50000, bid/ask around it with normal spread
        let snapshot = make_snapshot(dec!(50000), dec!(49990), dec!(50010));

        let signal = detector.check(key, &snapshot);
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
        let detector = DislocationDetector::with_user_fees(config, user_fees);
        let key = test_key();

        // Oracle at 50000, ask at 49940 (12 bps below = edge after 10 bps cost)
        // Edge = (50000 - 49940) / 50000 * 10000 = 12 bps
        let snapshot = make_snapshot(dec!(50000), dec!(49920), dec!(49940));

        let signal = detector.check(key, &snapshot);
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
        let detector = DislocationDetector::with_user_fees(config, user_fees);
        let key = test_key();

        // Oracle at 50000, bid at 50060 (12 bps above = edge after 10 bps cost)
        let snapshot = make_snapshot(dec!(50000), dec!(50060), dec!(50080));

        let signal = detector.check(key, &snapshot);
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
            ..Default::default()
        };
        let detector = DislocationDetector::new(config);

        // Book size = 1 BTC, mid = 50000
        // Alpha size = 0.1 * 1 = 0.1 BTC = $5000
        // Max size = 1000 / 50000 = 0.02 BTC
        // Result should be 0.02 (min of the two)
        let snapshot = make_snapshot(dec!(50000), dec!(49990), dec!(50010));
        let size = detector.calculate_size(&snapshot, OrderSide::Buy);

        assert_eq!(size.inner(), dec!(0.02));
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
        let detector = DislocationDetector::with_user_fees(config, user_fees);

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
        let detector = DislocationDetector::with_user_fees(config, user_fees);
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
        let mut detector = DislocationDetector::new(config);

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
        let detector = DislocationDetector::new(config);
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
        let signal = detector.check(key, &snapshot);
        assert!(signal.is_none());
    }
}
