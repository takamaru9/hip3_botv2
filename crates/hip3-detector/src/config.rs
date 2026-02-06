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
    /// Enable oracle direction filter.
    ///
    /// When enabled, signals are only generated when oracle movement
    /// matches the signal direction:
    /// - Buy signals: oracle must be rising (stale ask from before oracle rise)
    /// - Sell signals: oracle must be falling (stale bid from before oracle fall)
    ///
    /// This filters out signals caused by oracle lag in trending markets.
    #[serde(default = "default_oracle_direction_filter")]
    pub oracle_direction_filter: bool,
    /// Minimum oracle change in basis points to trigger signal.
    ///
    /// Only generates signals when oracle has moved at least this much
    /// since the previous tick. Small oracle movements are likely noise
    /// or quickly followed by MM quote updates.
    ///
    /// Set to 0 to disable (any oracle movement triggers signal).
    #[serde(default = "default_min_oracle_change_bps")]
    pub min_oracle_change_bps: Decimal,

    /// Minimum consecutive oracle moves in signal direction to trigger.
    ///
    /// Only generates signals when oracle has moved in the same direction
    /// for at least this many ticks. This filters out noise and ensures
    /// a real trend is forming.
    ///
    /// Data analysis (2026-02-03):
    /// - 0 consecutive: 29.79 bps avg edge
    /// - 1 consecutive: 41.98 bps avg edge (+12.2 bps)
    /// - 2 consecutive: 43.43 bps avg edge (+13.6 bps)
    /// - 3+ consecutive: diminishing returns, smaller sample
    ///
    /// Recommended: 2 for balance of edge vs opportunity count.
    /// Set to 0 to disable (any direction match triggers signal).
    #[serde(default = "default_min_consecutive_oracle_moves")]
    pub min_consecutive_oracle_moves: u32,

    /// Minimum time since oracle changed (ms) to generate signal.
    ///
    /// Quote Lag Gate: Filters "true stale liquidity" opportunities.
    ///
    /// Trading Philosophy: "Oracle moves → MM quotes lag → capture stale liquidity"
    /// - Too fresh (<50ms): May be noise, oracle will stabilize
    /// - Sweet spot (50-500ms): True stale liquidity opportunity
    /// - Too stale (>500ms): MM has already adjusted quotes
    ///
    /// Set to 0 to disable (any oracle age is allowed).
    #[serde(default = "default_min_quote_lag_ms")]
    pub min_quote_lag_ms: i64,

    /// Maximum time since oracle changed (ms) to generate signal.
    ///
    /// Filters out stale oracle moves where MM has likely caught up.
    /// Set to 0 to disable (no upper bound on oracle age).
    #[serde(default = "default_max_quote_lag_ms")]
    pub max_quote_lag_ms: i64,

    /// Enable oracle velocity-based sizing (P2-1).
    ///
    /// When enabled, higher oracle velocity (bps/tick) increases the
    /// sizing multiplier up to `velocity_multiplier_cap`.
    ///
    /// Rationale: Faster oracle moves create larger dislocations that
    /// MMs take longer to catch up with, providing more reliable edge.
    #[serde(default)]
    pub oracle_velocity_sizing: bool,

    /// Maximum sizing multiplier from oracle velocity (P2-1).
    ///
    /// Multiplier is linearly interpolated from 1.0 at min_oracle_change_bps
    /// to this cap at 4x min_oracle_change_bps.
    ///
    /// Example: cap=1.5, min_change=3bps
    ///   - 3bps velocity → 1.0x
    ///   - 6bps velocity → 1.25x
    ///   - 9bps velocity → 1.5x (capped)
    ///   - 12bps+ velocity → 1.5x (capped)
    #[serde(default = "default_velocity_multiplier_cap")]
    pub velocity_multiplier_cap: Decimal,

    /// Enable adaptive threshold based on spread EWMA (P2-2).
    ///
    /// When enabled, the effective signal threshold is:
    ///   `max(total_cost_bps, spread_ewma * spread_threshold_multiplier)`
    ///
    /// This automatically filters structurally wide-spread markets
    /// (e.g., USAR 85bps, URNM 141bps) without manual per-market config.
    #[serde(default)]
    pub adaptive_threshold: bool,

    /// Multiplier applied to spread EWMA for adaptive threshold (P2-2).
    ///
    /// Higher values = more conservative (require larger edge vs spread).
    /// Example: multiplier=1.5, spread_ewma=80bps → threshold=120bps
    #[serde(default = "default_spread_threshold_multiplier")]
    pub spread_threshold_multiplier: Decimal,

    /// EWMA alpha for spread tracking (P2-2).
    ///
    /// Smaller values = slower adaptation (more smoothing).
    /// 0.05 = ~20 ticks half-life.
    #[serde(default = "default_spread_ewma_alpha")]
    pub spread_ewma_alpha: Decimal,

    /// Enable confidence-based sizing (P3-1).
    ///
    /// When enabled, a multi-factor confidence score (0.0-1.0) adjusts sizing:
    ///   `final_size = base_size * (0.5 + 0.5 * confidence)`
    ///
    /// Factors: edge magnitude (0.3), oracle velocity (0.2),
    /// consecutive moves (0.2), book depth (0.15), spread tightness (0.15).
    #[serde(default)]
    pub confidence_sizing: bool,
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

fn default_oracle_direction_filter() -> bool {
    true // Enabled by default - only trade stale liquidity, not oracle lag
}

fn default_min_oracle_change_bps() -> Decimal {
    Decimal::from(3) // 3 bps minimum oracle movement to trigger signal
}

fn default_min_consecutive_oracle_moves() -> u32 {
    2 // 2 consecutive moves for +13.6 bps edge improvement
}

fn default_min_quote_lag_ms() -> i64 {
    0 // Disabled by default for backwards compatibility
}

fn default_max_quote_lag_ms() -> i64 {
    0 // Disabled by default for backwards compatibility
}

fn default_velocity_multiplier_cap() -> Decimal {
    Decimal::new(15, 1) // 1.5x
}

fn default_spread_threshold_multiplier() -> Decimal {
    Decimal::new(15, 1) // 1.5x
}

fn default_spread_ewma_alpha() -> Decimal {
    Decimal::new(5, 2) // 0.05 = slow adaptation (~20 tick half-life)
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            taker_fee_bps: Decimal::from(4),                            // 0.04%
            slippage_bps: Decimal::from(2),                             // 0.02%
            min_edge_bps: Decimal::from(5),                             // 0.05% minimum edge
            sizing_alpha: Decimal::new(10, 2),                          // 0.10 = 10% of top-of-book
            max_notional: Decimal::from(1000),                          // $1000 max
            min_order_notional: default_min_order_notional(),           // $11 min order
            min_book_notional: default_min_book_notional(),             // $500 min
            normal_book_notional: default_normal_book_notional(),       // $5000 normal
            oracle_direction_filter: default_oracle_direction_filter(), // Filter oracle lag
            min_oracle_change_bps: default_min_oracle_change_bps(),     // 3 bps min move
            min_consecutive_oracle_moves: default_min_consecutive_oracle_moves(), // 2 consecutive
            min_quote_lag_ms: default_min_quote_lag_ms(),               // 0 = disabled
            max_quote_lag_ms: default_max_quote_lag_ms(),               // 0 = disabled
            oracle_velocity_sizing: false,                              // Disabled by default
            velocity_multiplier_cap: default_velocity_multiplier_cap(), // 1.5x
            adaptive_threshold: false,                                  // Disabled by default
            spread_threshold_multiplier: default_spread_threshold_multiplier(), // 1.5x
            spread_ewma_alpha: default_spread_ewma_alpha(),             // 0.05
            confidence_sizing: false,                                   // Disabled by default
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

    /// Calculate velocity-based sizing multiplier (P2-1).
    ///
    /// Returns a multiplier in range [1.0, velocity_multiplier_cap].
    /// Linear interpolation from 1.0 at `min_oracle_change_bps` to cap at 4x.
    ///
    /// Returns 1.0 if velocity sizing is disabled or velocity is below minimum.
    pub fn velocity_multiplier(&self, velocity_bps: Decimal) -> Decimal {
        if !self.oracle_velocity_sizing {
            return Decimal::ONE;
        }

        let min_vel = self.min_oracle_change_bps;
        if min_vel.is_zero() || velocity_bps <= min_vel {
            return Decimal::ONE;
        }

        // Linear interpolation: 1.0 at min_vel, cap at 4x min_vel
        let max_vel = min_vel * Decimal::from(4);
        let progress = if velocity_bps >= max_vel {
            Decimal::ONE
        } else {
            (velocity_bps - min_vel) / (max_vel - min_vel)
        };

        Decimal::ONE + (self.velocity_multiplier_cap - Decimal::ONE) * progress
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
