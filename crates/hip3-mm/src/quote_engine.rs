//! Quote price calculation engine.
//!
//! Computes bid/ask prices based on:
//! - Oracle price (source of truth)
//! - Fixed offset (min_offset_bps)
//! - Inventory skew (shift quotes to reduce exposure)

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use hip3_core::Price;

use crate::config::{LevelDistribution, MakerConfig, SizeDistribution};
use crate::volatility::VolatilityStats;

/// A single quote level (one side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuoteLevel {
    /// Quote price.
    pub price: Price,
    /// Quote size in USD.
    pub size_usd: Decimal,
    /// Level index (0 = tightest).
    pub level: u32,
}

/// Computed quotes for a market (both sides).
#[derive(Debug, Clone)]
pub struct QuotePair {
    /// Bid levels (sorted by price descending, tightest first).
    pub bids: Vec<QuoteLevel>,
    /// Ask levels (sorted by price ascending, tightest first).
    pub asks: Vec<QuoteLevel>,
}

/// Compute `base^exp` for Decimal values via f64 conversion.
/// Used for exponential level distribution where precision loss is acceptable.
fn decimal_pow(base: Decimal, exp: Decimal) -> Decimal {
    use rust_decimal::prelude::ToPrimitive;
    let b = base.to_f64().unwrap_or(0.0);
    let e = exp.to_f64().unwrap_or(1.0);
    Decimal::from_f64_retain(b.powf(e)).unwrap_or(Decimal::ZERO)
}

/// Calculate quotes for a single market.
///
/// # Arguments
/// * `oracle_price` - Current oracle mid price
/// * `inventory_ratio` - Net inventory as fraction of max (-1.0 to 1.0).
///   Positive = long, negative = short.
/// * `config` - Maker configuration
/// * `spread_multiplier` - Spread multiplier (1.0 = normal, 2.0 = double).
///   Used by P2-3 adverse selection detector to widen spread.
/// * `volatility` - P3-1: Optional wick volatility stats for dynamic L0 offset.
///   When `None` or `!is_valid`, falls back to fixed `min_offset_bps`.
/// * `velocity_trend` - Phase C: Oracle directional trend in [-1.0, 1.0].
///   Positive = oracle rising, negative = falling.
///   When `velocity_skew_enabled`, tightens quotes in trend direction.
///
/// # Returns
/// QuotePair with bid and ask levels.
pub fn compute_quotes(
    oracle_price: Price,
    inventory_ratio: Decimal,
    config: &MakerConfig,
    spread_multiplier: Decimal,
    volatility: Option<&VolatilityStats>,
    velocity_trend: Decimal,
) -> QuotePair {
    let oracle = oracle_price.inner();
    let bps_divisor = dec!(10000);

    // Clamp inventory ratio to [-1, 1]
    let clamped_inv = inventory_ratio.max(dec!(-1)).min(dec!(1));

    let mut bids = Vec::with_capacity(config.num_levels as usize);
    let mut asks = Vec::with_capacity(config.num_levels as usize);

    // Clamp spread multiplier to at least 1.0
    let multiplier = spread_multiplier.max(dec!(1));

    // P3-1: Compute effective minimum offset (dynamic or fixed)
    let effective_min_offset = if config.dynamic_offset_enabled {
        match volatility {
            Some(vol) if vol.is_valid => {
                let optimal =
                    Decimal::from_f64_retain(vol.optimal_wick_bps).unwrap_or(Decimal::ZERO);
                (optimal * config.l0_wick_multiplier)
                    .max(config.min_offset_bps)
                    .max(config.fee_buffer_bps)
            }
            _ => config.min_offset_bps, // insufficient data → fixed fallback
        }
    } else {
        config.min_offset_bps // disabled → fixed offset
    };

    // Phase B: Compute range upper bound for exponential distribution
    let use_exponential =
        config.level_distribution == LevelDistribution::Exponential && config.num_levels > 1;
    let range_upper = if use_exponential {
        // range_upper = max(P100 × safety_mult, L0 + min_range_width)
        let p100_based = volatility
            .filter(|v| v.is_valid)
            .map(|v| {
                Decimal::from_f64_retain(v.p100_wick_bps).unwrap_or(Decimal::ZERO)
                    * config.p100_safety_multiplier
            })
            .unwrap_or(Decimal::ZERO);
        let floor = effective_min_offset + config.min_range_width_bps;
        p100_based.max(floor)
    } else {
        Decimal::ZERO // unused in linear mode
    };

    // Phase B: Convex size distribution
    let use_convex = config.size_distribution == SizeDistribution::Convex && config.num_levels > 1;

    for level in 0..config.num_levels {
        // Phase B: Level offset calculation
        let base_offset = if use_exponential {
            let t = Decimal::from(level) / Decimal::from(config.num_levels - 1);
            let t_exp = decimal_pow(t, config.level_exponent);
            (effective_min_offset + t_exp * (range_upper - effective_min_offset)) * multiplier
        } else {
            // Linear (original behavior)
            let level_offset = config.level_spacing_bps * Decimal::from(level);
            (effective_min_offset + level_offset) * multiplier
        };

        // Inventory skew: when long, widen bid (less aggressive buy) and tighten ask
        // Multiplicative skew: offset * (1 + skew * inventory_ratio)
        let inv_skew = config.inventory_skew_factor * clamped_inv;

        let mut bid_offset_bps = base_offset * (dec!(1) + inv_skew);
        let mut ask_offset_bps = base_offset * (dec!(1) - inv_skew);

        // Phase C: Oracle velocity skew
        // Oracle rising (velocity > 0): tighten ask (sell into strength), widen bid
        // Oracle falling (velocity < 0): tighten bid (buy into weakness), widen ask
        if config.velocity_skew_enabled {
            let clamped_vel = velocity_trend.max(dec!(-1)).min(dec!(1));
            let vel_skew = config.velocity_skew_factor * clamped_vel;
            // Positive vel_skew (oracle rising): ask *= (1 - vel_skew), bid *= (1 + vel_skew)
            bid_offset_bps *= dec!(1) + vel_skew;
            ask_offset_bps *= dec!(1) - vel_skew;
        }

        // Ensure minimum offset is never negative
        let bid_offset_bps = bid_offset_bps.max(dec!(1)); // at least 1 bps
        let ask_offset_bps = ask_offset_bps.max(dec!(1));

        let bid_price = oracle * (dec!(1) - bid_offset_bps / bps_divisor);
        let ask_price = oracle * (dec!(1) + ask_offset_bps / bps_divisor);

        // Phase B: Per-level size
        let level_size = if use_convex {
            let t = Decimal::from(level) / Decimal::from(config.num_levels - 1);
            let size_mult = config.size_min_multiplier
                + (config.size_max_multiplier - config.size_min_multiplier) * t;
            config.size_per_level_usd * size_mult
        } else {
            config.size_per_level_usd
        };

        bids.push(QuoteLevel {
            price: Price::new(bid_price),
            size_usd: level_size,
            level,
        });

        asks.push(QuoteLevel {
            price: Price::new(ask_price),
            size_usd: level_size,
            level,
        });
    }

    QuotePair { bids, asks }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn test_config() -> MakerConfig {
        MakerConfig {
            enabled: true,
            num_levels: 1,
            min_offset_bps: dec!(20),
            level_spacing_bps: dec!(10),
            size_per_level_usd: dec!(5),
            inventory_skew_factor: dec!(0.3),
            ..Default::default()
        }
    }

    #[test]
    fn test_symmetric_quotes_no_inventory() {
        let config = test_config();
        let oracle = Price::new(dec!(100));
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), None, Decimal::ZERO);

        assert_eq!(quotes.bids.len(), 1);
        assert_eq!(quotes.asks.len(), 1);

        // 20 bps offset from 100 = 0.20
        // bid = 100 * (1 - 20/10000) = 99.80
        // ask = 100 * (1 + 20/10000) = 100.20
        assert_eq!(quotes.bids[0].price.inner(), dec!(99.80));
        assert_eq!(quotes.asks[0].price.inner(), dec!(100.20));
    }

    #[test]
    fn test_inventory_skew_long() {
        let config = test_config();
        let oracle = Price::new(dec!(100));
        // Full long inventory (ratio = 1.0)
        let quotes = compute_quotes(oracle, dec!(1.0), &config, dec!(1), None, Decimal::ZERO);

        // Long inventory: bid offset should be wider (less aggressive buy)
        // bid_offset = 20 * (1 + 0.3 * 1.0) = 20 * 1.3 = 26 bps
        // ask_offset = 20 * (1 - 0.3 * 1.0) = 20 * 0.7 = 14 bps
        let bid = quotes.bids[0].price.inner();
        let ask = quotes.asks[0].price.inner();

        // bid = 100 * (1 - 26/10000) = 99.74
        assert_eq!(bid, dec!(99.74));
        // ask = 100 * (1 + 14/10000) = 100.14
        assert_eq!(ask, dec!(100.14));

        // Bid is further from oracle (wider), ask is closer (tighter)
        let bid_distance = oracle.inner() - bid;
        let ask_distance = ask - oracle.inner();
        assert!(bid_distance > ask_distance);
    }

    #[test]
    fn test_inventory_skew_short() {
        let config = test_config();
        let oracle = Price::new(dec!(100));
        // Full short inventory (ratio = -1.0)
        let quotes = compute_quotes(oracle, dec!(-1.0), &config, dec!(1), None, Decimal::ZERO);

        // Short inventory: ask offset should be wider (less aggressive sell)
        let bid = quotes.bids[0].price.inner();
        let ask = quotes.asks[0].price.inner();

        // bid_offset = 20 * (1 + 0.3 * (-1.0)) = 20 * 0.7 = 14 bps
        // ask_offset = 20 * (1 - 0.3 * (-1.0)) = 20 * 1.3 = 26 bps
        assert_eq!(bid, dec!(99.86));
        assert_eq!(ask, dec!(100.26));

        // Ask is further from oracle
        let bid_distance = oracle.inner() - bid;
        let ask_distance = ask - oracle.inner();
        assert!(ask_distance > bid_distance);
    }

    #[test]
    fn test_multi_level() {
        let config = MakerConfig {
            num_levels: 3,
            min_offset_bps: dec!(20),
            level_spacing_bps: dec!(10),
            size_per_level_usd: dec!(5),
            ..Default::default()
        };

        let oracle = Price::new(dec!(100));
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), None, Decimal::ZERO);

        assert_eq!(quotes.bids.len(), 3);
        assert_eq!(quotes.asks.len(), 3);

        // Level 0: offset = 20 bps
        // Level 1: offset = 30 bps
        // Level 2: offset = 40 bps
        assert_eq!(quotes.bids[0].price.inner(), dec!(99.80));
        assert_eq!(quotes.bids[1].price.inner(), dec!(99.70));
        assert_eq!(quotes.bids[2].price.inner(), dec!(99.60));

        assert_eq!(quotes.asks[0].price.inner(), dec!(100.20));
        assert_eq!(quotes.asks[1].price.inner(), dec!(100.30));
        assert_eq!(quotes.asks[2].price.inner(), dec!(100.40));

        // Each level should have specified size
        for level in &quotes.bids {
            assert_eq!(level.size_usd, dec!(5));
        }
    }

    #[test]
    fn test_offset_clamped_to_minimum() {
        // Extreme skew should not produce negative offsets
        let config = MakerConfig {
            num_levels: 1,
            min_offset_bps: dec!(5),
            inventory_skew_factor: dec!(0.9),
            ..Default::default()
        };

        let oracle = Price::new(dec!(100));
        let quotes = compute_quotes(oracle, dec!(1.0), &config, dec!(1), None, Decimal::ZERO);

        // ask_offset = 5 * (1 - 0.9 * 1.0) = 5 * 0.1 = 0.5 bps
        // This is above 1 bps minimum so it stays
        let ask = quotes.asks[0].price.inner();
        assert!(ask > oracle.inner()); // ask should always be above oracle

        // Even more extreme
        let config2 = MakerConfig {
            num_levels: 1,
            min_offset_bps: dec!(5),
            inventory_skew_factor: dec!(1.5), // extreme
            ..Default::default()
        };
        let quotes2 = compute_quotes(oracle, dec!(1.0), &config2, dec!(1), None, Decimal::ZERO);
        // ask_offset = 5 * (1 - 1.5) = 5 * (-0.5) = -2.5 → clamped to 1 bps
        let ask2 = quotes2.asks[0].price.inner();
        assert!(ask2 > oracle.inner());
    }

    #[test]
    fn test_inventory_ratio_clamped() {
        let config = test_config();
        let oracle = Price::new(dec!(100));

        // Ratio > 1 should be clamped to 1
        let q1 = compute_quotes(oracle, dec!(2.0), &config, dec!(1), None, Decimal::ZERO);
        let q2 = compute_quotes(oracle, dec!(1.0), &config, dec!(1), None, Decimal::ZERO);
        assert_eq!(q1.bids[0].price, q2.bids[0].price);
        assert_eq!(q1.asks[0].price, q2.asks[0].price);
    }

    #[test]
    fn test_spread_multiplier_doubles_offset() {
        let config = test_config();
        let oracle = Price::new(dec!(100));

        let normal = compute_quotes(oracle, dec!(0), &config, dec!(1), None, Decimal::ZERO);
        let doubled = compute_quotes(oracle, dec!(0), &config, dec!(2), None, Decimal::ZERO);

        // Normal: offset = 20 bps → bid = 99.80, ask = 100.20
        assert_eq!(normal.bids[0].price.inner(), dec!(99.80));
        assert_eq!(normal.asks[0].price.inner(), dec!(100.20));

        // Doubled: offset = 40 bps → bid = 99.60, ask = 100.40
        assert_eq!(doubled.bids[0].price.inner(), dec!(99.60));
        assert_eq!(doubled.asks[0].price.inner(), dec!(100.40));
    }

    // === P3-1: Dynamic offset tests ===

    #[test]
    fn test_dynamic_offset_disabled_uses_fixed() {
        let config = MakerConfig {
            dynamic_offset_enabled: false,
            min_offset_bps: dec!(20),
            ..test_config()
        };
        let oracle = Price::new(dec!(100));
        let vol = VolatilityStats {
            optimal_wick_bps: 50.0,
            is_valid: true,
            ..Default::default()
        };
        // Even with valid volatility stats, disabled → uses fixed 20 bps
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), Some(&vol), Decimal::ZERO);
        assert_eq!(quotes.bids[0].price.inner(), dec!(99.80));
        assert_eq!(quotes.asks[0].price.inner(), dec!(100.20));
    }

    #[test]
    fn test_dynamic_offset_with_valid_stats() {
        let config = MakerConfig {
            dynamic_offset_enabled: true,
            min_offset_bps: dec!(20),
            l0_wick_multiplier: dec!(2),
            fee_buffer_bps: dec!(8),
            ..test_config()
        };
        let oracle = Price::new(dec!(100));
        let vol = VolatilityStats {
            optimal_wick_bps: 10.0, // 10 bps × 2.0 = 20 bps
            is_valid: true,
            ..Default::default()
        };
        // optimal(10) × mult(2) = 20 bps → same as min_offset (floor)
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), Some(&vol), Decimal::ZERO);
        assert_eq!(quotes.bids[0].price.inner(), dec!(99.80));
        assert_eq!(quotes.asks[0].price.inner(), dec!(100.20));
    }

    #[test]
    fn test_dynamic_offset_respects_floor() {
        let config = MakerConfig {
            dynamic_offset_enabled: true,
            min_offset_bps: dec!(20),
            l0_wick_multiplier: dec!(2),
            fee_buffer_bps: dec!(8),
            ..test_config()
        };
        let oracle = Price::new(dec!(100));
        let vol = VolatilityStats {
            optimal_wick_bps: 5.0, // 5 × 2 = 10 bps < min_offset(20)
            is_valid: true,
            ..Default::default()
        };
        // Should use min_offset_bps (20) as floor
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), Some(&vol), Decimal::ZERO);
        assert_eq!(quotes.bids[0].price.inner(), dec!(99.80));
        assert_eq!(quotes.asks[0].price.inner(), dec!(100.20));
    }

    #[test]
    fn test_dynamic_offset_invalid_fallback() {
        let config = MakerConfig {
            dynamic_offset_enabled: true,
            min_offset_bps: dec!(20),
            l0_wick_multiplier: dec!(2),
            ..test_config()
        };
        let oracle = Price::new(dec!(100));
        let vol = VolatilityStats {
            optimal_wick_bps: 50.0,
            is_valid: false, // insufficient data
            ..Default::default()
        };
        // is_valid=false → falls back to fixed min_offset_bps (20)
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), Some(&vol), Decimal::ZERO);
        assert_eq!(quotes.bids[0].price.inner(), dec!(99.80));
        assert_eq!(quotes.asks[0].price.inner(), dec!(100.20));
    }

    #[test]
    fn test_dynamic_offset_breakpoint_wider() {
        let config = MakerConfig {
            dynamic_offset_enabled: true,
            min_offset_bps: dec!(20),
            l0_wick_multiplier: dec!(2),
            fee_buffer_bps: dec!(8),
            ..test_config()
        };
        let oracle = Price::new(dec!(100));
        // Cliff detected at P99.8 = 25 bps (much larger than P99=10)
        let vol = VolatilityStats {
            p99_wick_bps: 10.0,
            optimal_wick_bps: 25.0, // breakpoint cliff
            optimal_percentile: "P99.8",
            is_valid: true,
            ..Default::default()
        };
        // L0 = 25 × 2.0 = 50 bps (if we used P99 fixed: 10 × 2 = 20 bps)
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), Some(&vol), Decimal::ZERO);
        // bid = 100 * (1 - 50/10000) = 99.50
        assert_eq!(quotes.bids[0].price.inner(), dec!(99.50));
        // ask = 100 * (1 + 50/10000) = 100.50
        assert_eq!(quotes.asks[0].price.inner(), dec!(100.50));
    }

    // === Phase B: Level distribution + size distribution tests ===

    #[test]
    fn test_exponential_linear_equivalent() {
        // exponent=1.0 should produce identical offsets to linear when range matches
        let config = MakerConfig {
            num_levels: 3,
            min_offset_bps: dec!(20),
            level_spacing_bps: dec!(10),
            size_per_level_usd: dec!(5),
            level_distribution: LevelDistribution::Linear,
            ..Default::default()
        };
        let oracle = Price::new(dec!(100));
        let linear = compute_quotes(oracle, dec!(0), &config, dec!(1), None, Decimal::ZERO);

        // With exponent=1.0, exponential is linear: offset[i] = L0 + (i/(N-1))*(upper-L0)
        // For 3 levels: L0=20, upper = L0 + min_range_width(10) = 30
        // L0=20, L1=20+0.5*10=25, L2=20+1.0*10=30
        let exp_config = MakerConfig {
            num_levels: 3,
            min_offset_bps: dec!(20),
            level_distribution: LevelDistribution::Exponential,
            level_exponent: dec!(1), // linear
            min_range_width_bps: dec!(10),
            size_per_level_usd: dec!(5),
            ..Default::default()
        };
        let exp = compute_quotes(oracle, dec!(0), &exp_config, dec!(1), None, Decimal::ZERO);

        // Both should produce 3 levels with monotonic offsets
        assert_eq!(exp.bids.len(), 3);
        assert_eq!(exp.asks.len(), 3);
        // Linear: L0=20, L1=30, L2=40 → bids: 99.80, 99.70, 99.60
        // Exp(1.0): L0=20, L1=25, L2=30 → bids: 99.80, 99.75, 99.70
        // They differ because range_upper differs, but L0 is the same
        assert_eq!(linear.bids[0].price, exp.bids[0].price); // L0 identical
        assert_eq!(linear.asks[0].price, exp.asks[0].price); // L0 identical
    }

    #[test]
    fn test_exponential_quadratic_inner_dense() {
        // exponent=2.0: inner levels should be closer together than outer levels
        let vol = VolatilityStats {
            p100_wick_bps: 50.0,
            optimal_wick_bps: 20.0,
            is_valid: true,
            ..Default::default()
        };
        let config = MakerConfig {
            num_levels: 5,
            dynamic_offset_enabled: true,
            min_offset_bps: dec!(20),
            l0_wick_multiplier: dec!(1), // L0 = 20 bps
            level_distribution: LevelDistribution::Exponential,
            level_exponent: dec!(2), // quadratic
            p100_safety_multiplier: dec!(1.2),
            min_range_width_bps: dec!(10),
            size_per_level_usd: dec!(5),
            ..Default::default()
        };
        let oracle = Price::new(dec!(100));
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), Some(&vol), Decimal::ZERO);

        // range_upper = max(50*1.2=60, 20+10=30) = 60
        // offset[i] = 20 + (i/4)^2 * (60-20)
        // L0: 20, L1: 20 + 0.0625*40 = 22.5, L2: 20 + 0.25*40 = 30, L3: 20 + 0.5625*40 = 42.5, L4: 60
        assert_eq!(quotes.bids.len(), 5);

        // Inner gaps (L0→L1) should be smaller than outer gaps (L3→L4)
        let bid_prices: Vec<Decimal> = quotes.bids.iter().map(|q| q.price.inner()).collect();
        let gap_01 = bid_prices[0] - bid_prices[1]; // L0→L1 gap
        let gap_34 = bid_prices[3] - bid_prices[4]; // L3→L4 gap
        assert!(
            gap_01 < gap_34,
            "Inner gap ({gap_01}) should be less than outer gap ({gap_34})"
        );
    }

    #[test]
    fn test_exponential_monotonic_offsets() {
        // All levels should have monotonically increasing offset from oracle
        let vol = VolatilityStats {
            p100_wick_bps: 80.0,
            optimal_wick_bps: 30.0,
            is_valid: true,
            ..Default::default()
        };
        let config = MakerConfig {
            num_levels: 5,
            dynamic_offset_enabled: true,
            min_offset_bps: dec!(20),
            l0_wick_multiplier: dec!(1),
            level_distribution: LevelDistribution::Exponential,
            level_exponent: dec!(2),
            p100_safety_multiplier: dec!(1.2),
            min_range_width_bps: dec!(10),
            size_per_level_usd: dec!(5),
            ..Default::default()
        };
        let oracle = Price::new(dec!(100));
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), Some(&vol), Decimal::ZERO);

        // Bids should be monotonically decreasing (further from oracle)
        for i in 1..quotes.bids.len() {
            assert!(
                quotes.bids[i].price.inner() < quotes.bids[i - 1].price.inner(),
                "Bid level {i} should be lower than level {}",
                i - 1
            );
        }

        // Asks should be monotonically increasing (further from oracle)
        for i in 1..quotes.asks.len() {
            assert!(
                quotes.asks[i].price.inner() > quotes.asks[i - 1].price.inner(),
                "Ask level {i} should be higher than level {}",
                i - 1
            );
        }
    }

    #[test]
    fn test_range_upper_minimum_floor() {
        // When P100 is tiny, range_upper should still be >= L0 + min_range_width
        let vol = VolatilityStats {
            p100_wick_bps: 5.0, // tiny P100
            optimal_wick_bps: 3.0,
            is_valid: true,
            ..Default::default()
        };
        let config = MakerConfig {
            num_levels: 3,
            dynamic_offset_enabled: true,
            min_offset_bps: dec!(20),
            l0_wick_multiplier: dec!(1), // L0 = 20 (floor wins over 3*1=3)
            level_distribution: LevelDistribution::Exponential,
            level_exponent: dec!(2),
            p100_safety_multiplier: dec!(1.2),
            min_range_width_bps: dec!(10),
            size_per_level_usd: dec!(5),
            ..Default::default()
        };
        let oracle = Price::new(dec!(100));
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), Some(&vol), Decimal::ZERO);

        // L0 = max(3*1, 20, 8) = 20 bps
        // range_upper = max(5*1.2=6, 20+10=30) = 30 bps
        // L2 offset = 20 + 1.0^2 * (30-20) = 30 bps
        // bid[2] = 100 * (1 - 30/10000) = 99.70
        assert_eq!(quotes.bids[0].price.inner(), dec!(99.80)); // L0: 20 bps
        assert_eq!(quotes.bids[2].price.inner(), dec!(99.70)); // L2: 30 bps

        // Outermost level offset (30bps) >= L0 (20bps) + min_range_width (10bps)
        let l0_dist = oracle.inner() - quotes.bids[0].price.inner(); // 0.20
        let l2_dist = oracle.inner() - quotes.bids[2].price.inner(); // 0.30
        let range = l2_dist - l0_dist; // 0.10 = 10 bps on $100
        assert!(
            range >= dec!(0.10),
            "Range ({range}) should be >= min_range_width (0.10)"
        );
    }

    #[test]
    fn test_convex_size_l0_smaller() {
        // In convex mode, L0 size should be smaller than outermost level
        let config = MakerConfig {
            num_levels: 5,
            min_offset_bps: dec!(20),
            level_spacing_bps: dec!(10),
            size_per_level_usd: dec!(10),
            size_distribution: SizeDistribution::Convex,
            size_min_multiplier: dec!(0.5),
            size_max_multiplier: dec!(1.5),
            ..Default::default()
        };
        let oracle = Price::new(dec!(100));
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), None, Decimal::ZERO);

        assert_eq!(quotes.bids.len(), 5);

        // L0: size = 10 * 0.5 = 5.0
        assert_eq!(quotes.bids[0].size_usd, dec!(5.0));
        // L4: size = 10 * 1.5 = 15.0
        assert_eq!(quotes.bids[4].size_usd, dec!(15.0));

        // L0 < L4
        assert!(quotes.bids[0].size_usd < quotes.bids[4].size_usd);

        // Sizes should be monotonically increasing
        for i in 1..quotes.bids.len() {
            assert!(
                quotes.bids[i].size_usd >= quotes.bids[i - 1].size_usd,
                "Bid size at level {i} should be >= level {}",
                i - 1
            );
        }
    }

    #[test]
    fn test_convex_size_average_preserved() {
        // Average size across all levels should equal base size
        let config = MakerConfig {
            num_levels: 5,
            min_offset_bps: dec!(20),
            level_spacing_bps: dec!(10),
            size_per_level_usd: dec!(10),
            size_distribution: SizeDistribution::Convex,
            size_min_multiplier: dec!(0.5),
            size_max_multiplier: dec!(1.5),
            ..Default::default()
        };
        let oracle = Price::new(dec!(100));
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), None, Decimal::ZERO);

        let total_size: Decimal = quotes.bids.iter().map(|q| q.size_usd).sum();
        let avg_size = total_size / Decimal::from(quotes.bids.len() as u64);

        // With symmetric multipliers (0.5 and 1.5), average should be 1.0 × base = 10.0
        assert_eq!(avg_size, dec!(10));
    }

    #[test]
    fn test_uniform_size_backward_compat() {
        // "uniform" distribution should give all levels the same size (original behavior)
        let config = MakerConfig {
            num_levels: 3,
            min_offset_bps: dec!(20),
            level_spacing_bps: dec!(10),
            size_per_level_usd: dec!(5),
            size_distribution: SizeDistribution::Uniform,
            ..Default::default()
        };
        let oracle = Price::new(dec!(100));
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), None, Decimal::ZERO);

        for bid in &quotes.bids {
            assert_eq!(bid.size_usd, dec!(5));
        }
        for ask in &quotes.asks {
            assert_eq!(ask.size_usd, dec!(5));
        }
    }

    #[test]
    fn test_single_level_exponential_no_panic() {
        // num_levels=1 with exponential should not panic (division by zero guard)
        let config = MakerConfig {
            num_levels: 1,
            min_offset_bps: dec!(20),
            level_distribution: LevelDistribution::Exponential,
            level_exponent: dec!(2),
            size_per_level_usd: dec!(5),
            ..Default::default()
        };
        let oracle = Price::new(dec!(100));
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), None, Decimal::ZERO);

        // Falls back to linear for single level
        assert_eq!(quotes.bids.len(), 1);
        assert_eq!(quotes.bids[0].price.inner(), dec!(99.80)); // standard 20 bps
    }

    // === Phase C: Velocity skew tests ===

    #[test]
    fn test_velocity_positive_skew() {
        // Oracle rising (velocity=1.0): tighten ask (sell into strength), widen bid
        let config = MakerConfig {
            velocity_skew_enabled: true,
            velocity_skew_factor: dec!(0.3),
            min_offset_bps: dec!(20),
            ..test_config()
        };
        let oracle = Price::new(dec!(100));
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), None, dec!(1.0));

        // bid_offset = 20 * (1 + 0.3*1.0) = 20 * 1.3 = 26 bps
        // ask_offset = 20 * (1 - 0.3*1.0) = 20 * 0.7 = 14 bps
        let bid = quotes.bids[0].price.inner();
        let ask = quotes.asks[0].price.inner();

        assert_eq!(bid, dec!(99.74)); // 26 bps below oracle
        assert_eq!(ask, dec!(100.14)); // 14 bps above oracle

        // Ask should be tighter than bid (oracle rising = sell opportunity)
        let bid_dist = oracle.inner() - bid;
        let ask_dist = ask - oracle.inner();
        assert!(ask_dist < bid_dist);
    }

    #[test]
    fn test_velocity_negative_skew() {
        // Oracle falling (velocity=-1.0): tighten bid (buy into weakness), widen ask
        let config = MakerConfig {
            velocity_skew_enabled: true,
            velocity_skew_factor: dec!(0.3),
            min_offset_bps: dec!(20),
            ..test_config()
        };
        let oracle = Price::new(dec!(100));
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), None, dec!(-1.0));

        // bid_offset = 20 * (1 + 0.3*(-1.0)) = 20 * 0.7 = 14 bps
        // ask_offset = 20 * (1 - 0.3*(-1.0)) = 20 * 1.3 = 26 bps
        let bid = quotes.bids[0].price.inner();
        let ask = quotes.asks[0].price.inner();

        assert_eq!(bid, dec!(99.86)); // 14 bps below oracle
        assert_eq!(ask, dec!(100.26)); // 26 bps above oracle

        // Bid should be tighter than ask (oracle falling = buy opportunity)
        let bid_dist = oracle.inner() - bid;
        let ask_dist = ask - oracle.inner();
        assert!(bid_dist < ask_dist);
    }

    #[test]
    fn test_velocity_zero_symmetric() {
        // velocity=0: symmetric quotes (same as disabled)
        let config = MakerConfig {
            velocity_skew_enabled: true,
            velocity_skew_factor: dec!(0.3),
            min_offset_bps: dec!(20),
            ..test_config()
        };
        let oracle = Price::new(dec!(100));
        let quotes = compute_quotes(oracle, dec!(0), &config, dec!(1), None, dec!(0));

        assert_eq!(quotes.bids[0].price.inner(), dec!(99.80));
        assert_eq!(quotes.asks[0].price.inner(), dec!(100.20));
    }
}
