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
    /// Minimum order notional - orders below this are boosted to this value.
    /// This prevents `minTradeNtlRejected` errors from the exchange.
    /// Set to 0 to disable (use calculated size as-is).
    #[serde(default = "default_min_order_notional")]
    pub min_order_notional: Decimal,
    /// Minimum book notional - signals below this are skipped.
    /// Book notional = book_size × side_price (buy=ask_price / sell=bid_price).
    #[serde(default = "default_min_book_notional")]
    pub min_book_notional: Decimal,
    /// Normal book notional - 100% sizing above this.
    /// Between min and normal, sizing is linearly interpolated.
    #[serde(default = "default_normal_book_notional")]
    pub normal_book_notional: Decimal,
}

fn default_min_order_notional() -> Decimal {
    Decimal::from(11) // $11 - Hyperliquid minimum trade notional
}

fn default_min_book_notional() -> Decimal {
    Decimal::from(500) // $500
}

fn default_normal_book_notional() -> Decimal {
    Decimal::from(5000) // $5000
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            taker_fee_bps: Decimal::from(4),                      // 0.04%
            slippage_bps: Decimal::from(2),                       // 0.02%
            min_edge_bps: Decimal::from(5),                       // 0.05% minimum edge
            sizing_alpha: Decimal::new(10, 2),                    // 0.10 = 10% of top-of-book
            max_notional: Decimal::from(1000),                    // $1000 max
            min_order_notional: default_min_order_notional(),     // $11 min order
            min_book_notional: default_min_book_notional(),       // $500 min
            normal_book_notional: default_normal_book_notional(), // $5000 normal
        }
    }
}

impl DetectorConfig {
    /// Validate configuration values.
    ///
    /// Returns Err if values are invalid:
    /// - min_book_notional >= normal_book_notional
    /// - min_book_notional < 0
    /// - normal_book_notional <= 0
    pub fn validate(&self) -> Result<(), String> {
        // min must be less than normal
        if self.min_book_notional >= self.normal_book_notional {
            return Err(format!(
                "min_book_notional ({}) must be less than normal_book_notional ({})",
                self.min_book_notional, self.normal_book_notional
            ));
        }

        // No negative values for min
        if self.min_book_notional.is_sign_negative() {
            return Err(format!(
                "min_book_notional ({}) must be non-negative",
                self.min_book_notional
            ));
        }

        // normal must be positive
        if self.normal_book_notional.is_sign_negative() || self.normal_book_notional.is_zero() {
            return Err(format!(
                "normal_book_notional ({}) must be positive",
                self.normal_book_notional
            ));
        }

        Ok(())
    }

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

    #[test]
    fn test_validate_valid_config() {
        let config = DetectorConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_min_ge_normal() {
        // min_book_notional >= normal_book_notional should fail
        let config = DetectorConfig {
            min_book_notional: dec!(5000),
            normal_book_notional: dec!(500),
            ..Default::default()
        };
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must be less than"));
    }

    #[test]
    fn test_validate_min_equals_normal() {
        // min == normal should also fail
        let config = DetectorConfig {
            min_book_notional: dec!(500),
            normal_book_notional: dec!(500),
            ..Default::default()
        };
        let result = config.validate();
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_negative_min() {
        let config = DetectorConfig {
            min_book_notional: dec!(-100),
            normal_book_notional: dec!(5000),
            ..Default::default()
        };
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must be non-negative"));
    }

    #[test]
    fn test_validate_zero_normal() {
        // min=0, normal=0 triggers "min >= normal" check first
        // Test that *some* error is returned for this invalid state
        let config = DetectorConfig {
            min_book_notional: dec!(0),
            normal_book_notional: dec!(0),
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_normal_must_be_positive() {
        // To specifically test normal > 0, set min < normal but normal = 0
        // This requires a different setup: min negative, normal zero
        let config = DetectorConfig {
            min_book_notional: dec!(-1), // negative so min < normal
            normal_book_notional: dec!(0),
            ..Default::default()
        };
        let result = config.validate();
        assert!(result.is_err());
        // First error is "min must be non-negative", but that's fine
        // The point is invalid config is rejected
    }

    #[test]
    fn test_validate_negative_normal() {
        let config = DetectorConfig {
            min_book_notional: dec!(-100),
            normal_book_notional: dec!(-50),
            ..Default::default()
        };
        let result = config.validate();
        assert!(result.is_err());
    }
}
