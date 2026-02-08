//! Market making configuration.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Level distribution strategy.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LevelDistribution {
    /// Uniform spacing: L0 + spacing * i
    #[default]
    Linear,
    /// Exponential spacing: inner levels closer, outer levels further
    Exponential,
}

/// Size distribution strategy.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SizeDistribution {
    /// Equal size per level.
    #[default]
    Uniform,
    /// Outer levels get larger sizes (v1 lesson: outer fills = higher profit).
    Convex,
}

/// Market making configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MakerConfig {
    /// Enable market making.
    #[serde(default)]
    pub enabled: bool,

    /// Only run during weekend (Friday 21:00 – Sunday 21:00 UTC).
    #[serde(default = "default_true")]
    pub weekend_only: bool,

    /// Number of quote levels per side.
    #[serde(default = "default_num_levels")]
    pub num_levels: u32,

    /// Minimum offset from oracle in basis points.
    /// This is the distance from oracle price to the closest quote.
    #[serde(default = "default_min_offset_bps")]
    pub min_offset_bps: Decimal,

    /// Additional offset per level in basis points.
    #[serde(default = "default_level_spacing_bps")]
    pub level_spacing_bps: Decimal,

    /// Size per level in USD.
    #[serde(default = "default_size_per_level_usd")]
    pub size_per_level_usd: Decimal,

    /// Maximum total position in USD (across all MM markets).
    #[serde(default = "default_max_position_usd")]
    pub max_position_usd: Decimal,

    /// Inventory skew factor (0.0 = no skew, 1.0 = full skew).
    /// When inventory is long, bid offset increases (less aggressive buying)
    /// and ask offset decreases (more aggressive selling).
    #[serde(default = "default_inventory_skew_factor")]
    pub inventory_skew_factor: Decimal,

    /// Quote refresh interval in milliseconds.
    #[serde(default = "default_requote_interval_ms")]
    pub requote_interval_ms: u64,

    /// Use ALO (Add-Liquidity-Only) instead of GTC.
    /// ALO ensures maker-only fills. Recommended for MM.
    #[serde(default = "default_true")]
    pub use_alo: bool,

    /// Markets to make (by market name, e.g. "GOLD").
    /// Empty = no markets.
    #[serde(default)]
    pub markets: Vec<String>,

    /// Minimum oracle price change (bps) to trigger requote.
    /// Below this threshold, quotes are not updated.
    #[serde(default = "default_min_requote_change_bps")]
    pub min_requote_change_bps: Decimal,

    /// Emergency flatten slippage in basis points.
    #[serde(default = "default_flatten_slippage_bps")]
    pub flatten_slippage_bps: u64,

    // --- P2-1: Inventory skew protection ---
    /// Inventory ratio threshold to stop quoting on the side that increases exposure.
    /// E.g. 0.8 = at 80% of max_position_usd, stop adding to the position side.
    #[serde(default = "default_inventory_warn_ratio")]
    pub inventory_warn_ratio: Decimal,

    /// Inventory ratio threshold for emergency flatten.
    /// At this level, cancel all quotes and flatten immediately.
    #[serde(default = "default_inventory_emergency_ratio")]
    pub inventory_emergency_ratio: Decimal,

    // --- P2-2: Stale quote detection ---
    /// Timeout (ms) for cancel acknowledgement. If a cancel is not acked
    /// within this period, all MM quoting is halted.
    #[serde(default = "default_stale_cancel_timeout_ms")]
    pub stale_cancel_timeout_ms: u64,

    // --- P2-3: Adverse selection protection ---
    /// Number of consecutive same-side fills that triggers spread widening.
    #[serde(default = "default_adverse_consecutive_fills")]
    pub adverse_consecutive_fills: u32,

    /// Spread multiplier applied when adverse selection is detected.
    #[serde(default = "default_adverse_spread_multiplier")]
    pub adverse_spread_multiplier: Decimal,

    // --- P3-1: Dynamic offset (wick-based volatility) ---
    /// Enable dynamic L0 offset based on P99 wick statistics.
    /// When false, WickTracker still runs (observation mode) but offset
    /// uses the fixed `min_offset_bps`.
    #[serde(default)]
    pub dynamic_offset_enabled: bool,

    /// Rolling window size for wick history (in seconds / samples).
    #[serde(default = "default_wick_window_size")]
    pub wick_window_size: usize,

    /// Minimum wick samples required before stats are considered valid.
    #[serde(default = "default_wick_min_samples")]
    pub wick_min_samples: usize,

    /// L0 = optimal_wick × this multiplier.
    #[serde(default = "default_l0_wick_multiplier")]
    pub l0_wick_multiplier: Decimal,

    /// Minimum L0 floor from fee protection (bps).
    #[serde(default = "default_fee_buffer_bps")]
    pub fee_buffer_bps: Decimal,

    /// Percentile stats cache TTL in milliseconds.
    #[serde(default = "default_wick_cache_ttl_ms")]
    pub wick_cache_ttl_ms: u64,

    /// Minimum ratio for breakpoint (cliff) detection in the wick distribution.
    #[serde(default = "default_breakpoint_min_jump_ratio")]
    pub breakpoint_min_jump_ratio: f64,

    // --- Phase B: Level distribution + size distribution ---
    /// Level distribution strategy.
    /// Exponential places inner levels closer together and outer levels further apart,
    /// covering the P100 wick range adaptively.
    #[serde(default)]
    pub level_distribution: LevelDistribution,

    /// Exponent for exponential level distribution (1.0 = linear, 2.0 = quadratic).
    #[serde(default = "default_level_exponent")]
    pub level_exponent: Decimal,

    /// Safety multiplier applied to P100 wick to determine distribution upper bound.
    #[serde(default = "default_p100_safety_multiplier")]
    pub p100_safety_multiplier: Decimal,

    /// Minimum range width (bps) between L0 and the outermost level.
    #[serde(default = "default_min_range_width_bps")]
    pub min_range_width_bps: Decimal,

    /// Size distribution strategy.
    /// Convex gives outer levels larger sizes (v1 lesson: outer fills = higher profit).
    #[serde(default)]
    pub size_distribution: SizeDistribution,

    /// Size multiplier for L0 (innermost level) in convex mode.
    #[serde(default = "default_size_min_multiplier")]
    pub size_min_multiplier: Decimal,

    /// Size multiplier for outermost level in convex mode.
    #[serde(default = "default_size_max_multiplier")]
    pub size_max_multiplier: Decimal,

    // --- Phase C: Counter-order + velocity skew ---
    /// Enable counter-order (mean reversion) on fill.
    /// After an MM fill, places a counter-order aiming for partial reversion.
    #[serde(default)]
    pub counter_order_enabled: bool,

    /// Base reversion percentage for counter-orders.
    /// E.g. 0.6 = counter-order at 60% reversion from fill price to oracle.
    #[serde(default = "default_counter_reversion_pct")]
    pub counter_reversion_pct: Decimal,

    /// Additional reversion percentage per level of the filled quote.
    /// Outer fills get higher reversion (mean reversion is stronger for larger moves).
    #[serde(default = "default_counter_reversion_per_level")]
    pub counter_reversion_per_level: Decimal,

    /// Enable oracle velocity-based asymmetric skew.
    /// When oracle is trending, tighten quotes in trend direction, widen against.
    #[serde(default)]
    pub velocity_skew_enabled: bool,

    /// Maximum velocity skew factor.
    /// E.g. 0.3 = up to 30% asymmetry between bid and ask offsets.
    #[serde(default = "default_velocity_skew_factor")]
    pub velocity_skew_factor: Decimal,

    /// Number of recent oracle updates to track for velocity calculation.
    #[serde(default = "default_velocity_window")]
    pub velocity_window: usize,
}

impl Default for MakerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            weekend_only: true,
            num_levels: default_num_levels(),
            min_offset_bps: default_min_offset_bps(),
            level_spacing_bps: default_level_spacing_bps(),
            size_per_level_usd: default_size_per_level_usd(),
            max_position_usd: default_max_position_usd(),
            inventory_skew_factor: default_inventory_skew_factor(),
            requote_interval_ms: default_requote_interval_ms(),
            use_alo: true,
            markets: Vec::new(),
            min_requote_change_bps: default_min_requote_change_bps(),
            flatten_slippage_bps: default_flatten_slippage_bps(),
            inventory_warn_ratio: default_inventory_warn_ratio(),
            inventory_emergency_ratio: default_inventory_emergency_ratio(),
            stale_cancel_timeout_ms: default_stale_cancel_timeout_ms(),
            adverse_consecutive_fills: default_adverse_consecutive_fills(),
            adverse_spread_multiplier: default_adverse_spread_multiplier(),
            dynamic_offset_enabled: false,
            wick_window_size: default_wick_window_size(),
            wick_min_samples: default_wick_min_samples(),
            l0_wick_multiplier: default_l0_wick_multiplier(),
            fee_buffer_bps: default_fee_buffer_bps(),
            wick_cache_ttl_ms: default_wick_cache_ttl_ms(),
            breakpoint_min_jump_ratio: default_breakpoint_min_jump_ratio(),
            level_distribution: LevelDistribution::default(),
            level_exponent: default_level_exponent(),
            p100_safety_multiplier: default_p100_safety_multiplier(),
            min_range_width_bps: default_min_range_width_bps(),
            size_distribution: SizeDistribution::default(),
            size_min_multiplier: default_size_min_multiplier(),
            size_max_multiplier: default_size_max_multiplier(),
            counter_order_enabled: false,
            counter_reversion_pct: default_counter_reversion_pct(),
            counter_reversion_per_level: default_counter_reversion_per_level(),
            velocity_skew_enabled: false,
            velocity_skew_factor: default_velocity_skew_factor(),
            velocity_window: default_velocity_window(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_num_levels() -> u32 {
    1
}
fn default_min_offset_bps() -> Decimal {
    Decimal::new(20, 0) // 20 bps
}
fn default_level_spacing_bps() -> Decimal {
    Decimal::new(10, 0) // 10 bps between levels
}
fn default_size_per_level_usd() -> Decimal {
    Decimal::new(5, 0) // $5 per level
}
fn default_max_position_usd() -> Decimal {
    Decimal::new(25, 0) // $25 max position
}
fn default_inventory_skew_factor() -> Decimal {
    Decimal::new(3, 1) // 0.3
}
fn default_requote_interval_ms() -> u64 {
    2000 // 2 seconds
}
fn default_min_requote_change_bps() -> Decimal {
    Decimal::new(2, 0) // 2 bps
}
fn default_flatten_slippage_bps() -> u64 {
    50 // 50 bps
}
fn default_inventory_warn_ratio() -> Decimal {
    Decimal::new(8, 1) // 0.8 = 80%
}
fn default_inventory_emergency_ratio() -> Decimal {
    Decimal::new(95, 2) // 0.95 = 95%
}
fn default_stale_cancel_timeout_ms() -> u64 {
    10_000 // 10 seconds
}
fn default_adverse_consecutive_fills() -> u32 {
    3
}
fn default_adverse_spread_multiplier() -> Decimal {
    Decimal::new(2, 0) // 2x spread when adverse selection detected
}
fn default_wick_window_size() -> usize {
    3600 // 1 hour of 1-second wicks
}
fn default_wick_min_samples() -> usize {
    60 // 1 minute minimum
}
fn default_l0_wick_multiplier() -> Decimal {
    Decimal::TWO // L0 = optimal_wick × 2.0
}
fn default_fee_buffer_bps() -> Decimal {
    Decimal::new(8, 0) // 8 bps fee protection floor
}
fn default_wick_cache_ttl_ms() -> u64 {
    10_000 // 10 seconds
}
fn default_breakpoint_min_jump_ratio() -> f64 {
    1.5 // 1.5x ratio triggers cliff detection
}
fn default_level_exponent() -> Decimal {
    Decimal::TWO // quadratic distribution
}
fn default_p100_safety_multiplier() -> Decimal {
    Decimal::new(12, 1) // 1.2x P100
}
fn default_min_range_width_bps() -> Decimal {
    Decimal::new(10, 0) // 10 bps minimum range
}
fn default_size_min_multiplier() -> Decimal {
    Decimal::new(5, 1) // 0.5x for L0
}
fn default_size_max_multiplier() -> Decimal {
    Decimal::new(15, 1) // 1.5x for outermost level
}
fn default_counter_reversion_pct() -> Decimal {
    Decimal::new(6, 1) // 0.6 = 60% reversion
}
fn default_counter_reversion_per_level() -> Decimal {
    Decimal::new(3, 2) // 0.03 = 3% additional per level
}
fn default_velocity_skew_factor() -> Decimal {
    Decimal::new(3, 1) // 0.3 = max 30% asymmetry
}
fn default_velocity_window() -> usize {
    5 // last 5 oracle updates
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_default_config() {
        let config = MakerConfig::default();
        assert!(!config.enabled);
        assert!(config.weekend_only);
        assert_eq!(config.num_levels, 1);
        assert_eq!(config.min_offset_bps, dec!(20));
        assert_eq!(config.size_per_level_usd, dec!(5));
        assert_eq!(config.max_position_usd, dec!(25));
        assert_eq!(config.inventory_skew_factor, dec!(0.3));
        assert!(config.use_alo);
        assert!(config.markets.is_empty());
        // P3-1: Dynamic offset defaults
        assert!(!config.dynamic_offset_enabled);
        assert_eq!(config.wick_window_size, 3600);
        assert_eq!(config.wick_min_samples, 60);
        assert_eq!(config.l0_wick_multiplier, dec!(2));
        assert_eq!(config.fee_buffer_bps, dec!(8));
        assert_eq!(config.wick_cache_ttl_ms, 10_000);
        assert!((config.breakpoint_min_jump_ratio - 1.5).abs() < f64::EPSILON);
        // Phase B: Level + size distribution defaults
        assert_eq!(config.level_distribution, LevelDistribution::Linear);
        assert_eq!(config.level_exponent, dec!(2));
        assert_eq!(config.p100_safety_multiplier, dec!(1.2));
        assert_eq!(config.min_range_width_bps, dec!(10));
        assert_eq!(config.size_distribution, SizeDistribution::Uniform);
        assert_eq!(config.size_min_multiplier, dec!(0.5));
        assert_eq!(config.size_max_multiplier, dec!(1.5));
        // Phase C: Counter-order + velocity skew defaults
        assert!(!config.counter_order_enabled);
        assert_eq!(config.counter_reversion_pct, dec!(0.6));
        assert_eq!(config.counter_reversion_per_level, dec!(0.03));
        assert!(!config.velocity_skew_enabled);
        assert_eq!(config.velocity_skew_factor, dec!(0.3));
        assert_eq!(config.velocity_window, 5);
    }

    #[test]
    fn test_config_serde_defaults() {
        let toml_str = r#"
enabled = true
markets = ["GOLD"]
"#;
        let config: MakerConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert!(config.weekend_only);
        assert_eq!(config.num_levels, 1);
        assert_eq!(config.min_offset_bps, dec!(20));
        assert_eq!(config.markets, vec!["GOLD".to_string()]);
    }
}
