//! Detector configuration.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Configuration for dislocation detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectorConfig {
    /// Taker fee in basis points.
    pub taker_fee_bps: Decimal,
    /// Expected slippage in basis points.
    pub slippage_bps: Decimal,
    /// Minimum edge in basis points to trigger.
    pub min_edge_bps: Decimal,
    /// Alpha for sizing (fraction of top-of-book).
    pub sizing_alpha: Decimal,
    /// Maximum notional per trade.
    pub max_notional: Decimal,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            taker_fee_bps: Decimal::from(4),   // 0.04%
            slippage_bps: Decimal::from(2),    // 0.02%
            min_edge_bps: Decimal::from(5),    // 0.05% minimum edge
            sizing_alpha: Decimal::new(10, 2), // 0.10 = 10% of top-of-book
            max_notional: Decimal::from(1000), // $1000 max
        }
    }
}

impl DetectorConfig {
    /// Calculate total cost (fees + slippage + required edge).
    pub fn total_cost_bps(&self) -> Decimal {
        self.taker_fee_bps + self.slippage_bps + self.min_edge_bps
    }

    /// Get threshold multiplier for buy signal.
    /// Buy when: ask <= oracle * (1 - threshold/10000)
    pub fn buy_threshold(&self) -> Decimal {
        Decimal::ONE - self.total_cost_bps() / Decimal::from(10000)
    }

    /// Get threshold multiplier for sell signal.
    /// Sell when: bid >= oracle * (1 + threshold/10000)
    pub fn sell_threshold(&self) -> Decimal {
        Decimal::ONE + self.total_cost_bps() / Decimal::from(10000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_default_config() {
        let config = DetectorConfig::default();
        assert_eq!(config.total_cost_bps(), dec!(11)); // 4 + 2 + 5
    }

    #[test]
    fn test_thresholds() {
        let config = DetectorConfig {
            taker_fee_bps: dec!(4),
            slippage_bps: dec!(2),
            min_edge_bps: dec!(4),
            ..Default::default()
        };

        // Total: 10 bps = 0.10%
        let buy = config.buy_threshold();
        let sell = config.sell_threshold();

        assert_eq!(buy, dec!(0.9990)); // 1 - 0.0010
        assert_eq!(sell, dec!(1.0010)); // 1 + 0.0010
    }
}
