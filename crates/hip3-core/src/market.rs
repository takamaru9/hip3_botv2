//! Market identification and specification types.
//!
//! HIP-3 markets have a dual structure: DEX (like xyz/UNIT) + Asset index.
//! This module provides types to uniquely identify and describe markets.

use crate::{Price, Size};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

/// DEX identifier (e.g., xyz/UNIT).
///
/// In HIP-3, each DEX is identified by an index. The xyz DEX
/// uses UNIT as its denomination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DexId(pub u16);

impl DexId {
    /// The xyz/UNIT DEX (index 0 in perpDexs).
    pub const XYZ: Self = Self(0);

    pub fn new(id: u16) -> Self {
        Self(id)
    }

    pub fn index(&self) -> u16 {
        self.0
    }
}

impl fmt::Display for DexId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            0 => write!(f, "xyz"),
            n => write!(f, "dex_{n}"),
        }
    }
}

/// Asset identifier within a DEX.
///
/// Each perpetual market within a DEX is identified by an asset index.
/// For xyz/HIP-3 markets, asset IDs use the formula:
///   100000 + perp_dex_id * 10000 + asset_index
/// Example: xyz:SILVER (perpDexId=1, index=27) = 110027
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetId(pub u32);

impl AssetId {
    pub fn new(id: u32) -> Self {
        Self(id)
    }

    pub fn index(&self) -> u32 {
        self.0
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique market identifier combining DEX and Asset.
///
/// This is the primary key for identifying markets in the HIP-3 system.
/// Format: `{dex}:{asset}` (e.g., "xyz:0" for BTC on xyz DEX).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MarketKey {
    pub dex: DexId,
    pub asset: AssetId,
}

impl MarketKey {
    pub fn new(dex: DexId, asset: AssetId) -> Self {
        Self { dex, asset }
    }

    /// Create from DEX index and asset index.
    pub fn from_indices(dex_idx: u16, asset_idx: u32) -> Self {
        Self {
            dex: DexId(dex_idx),
            asset: AssetId(asset_idx),
        }
    }

    /// Returns the canonical string representation.
    pub fn as_string(&self) -> String {
        format!("{}:{}", self.dex, self.asset)
    }
}

impl fmt::Display for MarketKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.dex, self.asset)
    }
}

/// Market specification from exchange.
///
/// Contains tick size, lot size, fees, and other market parameters.
/// Changes to these values should trigger trading halt (ParamChange gate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketSpec {
    /// Minimum price increment.
    pub tick_size: Price,

    /// Minimum size increment.
    pub lot_size: Size,

    /// Minimum order size.
    pub min_size: Size,

    /// Maximum leverage allowed.
    pub max_leverage: u8,

    /// Taker fee in basis points.
    pub taker_fee_bps: u16,

    /// Maker fee in basis points (can be negative for rebates).
    pub maker_fee_bps: i16,

    /// Open interest cap (if any).
    pub oi_cap: Option<Size>,

    /// Whether the market is currently active.
    pub is_active: bool,

    /// Asset name/symbol (e.g., "BTC", "ETH").
    pub name: String,

    // === Precision Fields (P0-23) ===
    /// Size decimals from exchange (szDecimals).
    /// Determines minimum size increment: 10^(-sz_decimals).
    pub sz_decimals: u8,

    /// Maximum significant figures for price formatting.
    /// HIP-3 uses 5 significant figures.
    pub max_sig_figs: u8,

    /// Maximum decimal places for price.
    /// Derived from tick_size or exchange metadata.
    pub max_price_decimals: u8,
}

/// HIP-3 default maximum significant figures.
pub const HIP3_MAX_SIG_FIGS: u8 = 5;

impl MarketSpec {
    /// Check if market spec has materially changed.
    ///
    /// Returns true if tick_size, lot_size, fees, or precision changed,
    /// which should trigger ParamChange risk gate.
    pub fn has_material_change(&self, other: &Self) -> bool {
        self.tick_size != other.tick_size
            || self.lot_size != other.lot_size
            || self.taker_fee_bps != other.taker_fee_bps
            || self.maker_fee_bps != other.maker_fee_bps
            || self.sz_decimals != other.sz_decimals
    }

    /// Calculate taker fee as a decimal multiplier.
    pub fn taker_fee_rate(&self) -> Decimal {
        Decimal::from(self.taker_fee_bps) / Decimal::from(10000)
    }

    /// Format price for order submission (P0-23).
    ///
    /// Applies HIP-3 formatting rules:
    /// - Maximum 5 significant figures
    /// - Maximum `max_price_decimals` decimal places
    /// - Round toward unfavorable direction (ceil for buy, floor for sell)
    ///
    /// Returns the formatted price as a string suitable for JSON serialization.
    pub fn format_price(&self, price: Price, is_buy: bool) -> String {
        let rounded = self.round_price_for_order(price, is_buy);
        format_decimal_with_constraints(rounded.inner(), self.max_sig_figs, self.max_price_decimals)
    }

    /// Format size for order submission (P0-23).
    ///
    /// Applies HIP-3 formatting rules:
    /// - Maximum 5 significant figures
    /// - Exactly `sz_decimals` decimal places (or fewer if trailing zeros)
    /// - Always round down (floor) to avoid oversizing
    ///
    /// Returns the formatted size as a string suitable for JSON serialization.
    pub fn format_size(&self, size: Size) -> String {
        let rounded = size.round_to_lot(self.lot_size);
        format_decimal_with_constraints(rounded.inner(), self.max_sig_figs, self.sz_decimals)
    }

    /// Round price for order submission.
    ///
    /// - Buy orders: round UP to tick (pay more, ensure fill)
    /// - Sell orders: round DOWN to tick (receive less, ensure fill)
    pub fn round_price_for_order(&self, price: Price, is_buy: bool) -> Price {
        if self.tick_size.is_zero() {
            return price;
        }
        let tick = self.tick_size.inner();
        let p = price.inner();
        let rounded = if is_buy {
            // Round up for buy (ceiling)
            (p / tick).ceil() * tick
        } else {
            // Round down for sell (floor)
            (p / tick).floor() * tick
        };
        Price::new(rounded)
    }

    /// Calculate the number of decimal places in tick_size.
    pub fn tick_decimals(&self) -> u8 {
        count_decimals(self.tick_size.inner())
    }
}

/// Format a Decimal with max significant figures and max decimal places.
///
/// HIP-3 rule: min(max_sig_figs significant figures, max_decimals decimal places)
/// Truncates (floors) to constraints - does NOT round up.
/// Trailing zeros after decimal point are stripped.
fn format_decimal_with_constraints(value: Decimal, max_sig_figs: u8, max_decimals: u8) -> String {
    if value.is_zero() {
        return "0".to_string();
    }

    let abs_value = value.abs();
    let sign = if value.is_sign_negative() { "-" } else { "" };

    // Step 1: Truncate to max_sig_figs significant figures
    let truncated_sig = truncate_to_sig_figs(abs_value, max_sig_figs);

    // Step 2: Truncate to max_decimals decimal places
    let truncated = truncate_to_decimals(truncated_sig, max_decimals);

    // Format without trailing zeros
    let formatted = format_without_trailing_zeros(truncated);

    format!("{sign}{formatted}")
}

/// Truncate a Decimal to N significant figures (floor, not round).
fn truncate_to_sig_figs(value: Decimal, max_sig_figs: u8) -> Decimal {
    if value.is_zero() || max_sig_figs == 0 {
        return Decimal::ZERO;
    }

    let abs_value = value.abs();

    // Find the order of magnitude (number of digits to left of decimal - 1)
    // 12345 -> magnitude 4 (10^4)
    // 0.00123 -> magnitude -3 (10^-3)
    let magnitude = calculate_magnitude(abs_value);

    // Calculate how many decimal places we can keep
    // For 12345 (magnitude 4) with 5 sig figs: scale = 5 - 4 - 1 = 0
    // For 1234.5 (magnitude 3) with 5 sig figs: scale = 5 - 3 - 1 = 1
    // For 0.00123 (magnitude -3) with 5 sig figs: scale = 5 - (-3) - 1 = 7
    let scale = (max_sig_figs as i32) - magnitude - 1;

    if scale >= 0 {
        // Truncate decimal places
        truncate_to_decimals(abs_value, scale as u8)
    } else {
        // Need to truncate integer digits
        // scale = -2 means multiply by 10^2, truncate, divide by 10^2
        let factor = Decimal::from(10i64.pow((-scale) as u32));
        (abs_value / factor).trunc() * factor
    }
}

/// Truncate to N decimal places (floor).
fn truncate_to_decimals(value: Decimal, max_decimals: u8) -> Decimal {
    let factor = Decimal::from(10i64.pow(max_decimals as u32));
    (value * factor).trunc() / factor
}

/// Calculate the magnitude (order of magnitude) of a decimal.
/// 12345 -> 4, 1234.5 -> 3, 123.45 -> 2, 0.123 -> -1, 0.00123 -> -3
fn calculate_magnitude(value: Decimal) -> i32 {
    if value.is_zero() {
        return 0;
    }

    let abs_value = value.abs();
    let int_part = abs_value.trunc();

    if !int_part.is_zero() {
        // Count digits in integer part
        let int_str = int_part.to_string();
        (int_str.len() as i32) - 1
    } else {
        // Value is < 1, find first non-zero decimal digit
        let s = abs_value.to_string();
        let mut magnitude: i32 = 0;
        let mut after_decimal = false;

        for c in s.chars() {
            if c == '.' {
                after_decimal = true;
                continue;
            }
            if after_decimal {
                magnitude -= 1;
                if c != '0' {
                    break;
                }
            }
        }
        magnitude
    }
}

/// Format decimal without trailing zeros after decimal point.
fn format_without_trailing_zeros(value: Decimal) -> String {
    let s = value.to_string();

    if !s.contains('.') {
        return s;
    }

    // Remove trailing zeros
    let trimmed = s.trim_end_matches('0');

    // Remove trailing decimal point if no fractional part
    trimmed.trim_end_matches('.').to_string()
}

/// Count decimal places in a Decimal value.
fn count_decimals(value: Decimal) -> u8 {
    let s = value.to_string();
    if let Some(pos) = s.find('.') {
        let decimal_part = &s[pos + 1..];
        let trimmed = decimal_part.trim_end_matches('0');
        trimmed.len() as u8
    } else {
        0
    }
}

impl Default for MarketSpec {
    fn default() -> Self {
        Self {
            tick_size: Price::new(rust_decimal_macros::dec!(0.01)),
            lot_size: Size::new(rust_decimal_macros::dec!(0.001)),
            min_size: Size::new(rust_decimal_macros::dec!(0.001)),
            max_leverage: 50,
            taker_fee_bps: 4, // 0.04%
            maker_fee_bps: 1, // 0.01%
            oi_cap: None,
            is_active: true,
            name: String::new(),
            sz_decimals: 3,
            max_sig_figs: HIP3_MAX_SIG_FIGS,
            max_price_decimals: 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_market_key_display() {
        let key = MarketKey::new(DexId::XYZ, AssetId::new(0));
        assert_eq!(key.to_string(), "xyz:0");
    }

    #[test]
    fn test_market_key_equality() {
        let key1 = MarketKey::from_indices(0, 1);
        let key2 = MarketKey::from_indices(0, 1);
        let key3 = MarketKey::from_indices(0, 2);

        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[test]
    fn test_market_spec_material_change() {
        let spec1 = MarketSpec::default();
        let mut spec2 = spec1.clone();

        assert!(!spec1.has_material_change(&spec2));

        spec2.tick_size = Price::new(dec!(0.001));
        assert!(spec1.has_material_change(&spec2));
    }

    #[test]
    fn test_market_spec_sz_decimals_change() {
        let spec1 = MarketSpec::default();
        let mut spec2 = spec1.clone();

        spec2.sz_decimals = 5;
        assert!(spec1.has_material_change(&spec2));
    }

    // === P0-28: Golden Test Vectors for format_price/format_size ===

    #[test]
    fn test_format_price_basic() {
        let spec = MarketSpec {
            tick_size: Price::new(dec!(0.01)),
            max_sig_figs: 5,
            max_price_decimals: 2,
            ..Default::default()
        };

        // Basic cases
        assert_eq!(spec.format_price(Price::new(dec!(12345)), false), "12345");
        assert_eq!(
            spec.format_price(Price::new(dec!(12345.67)), false),
            "12345"
        );
        assert_eq!(
            spec.format_price(Price::new(dec!(1234.56)), false),
            "1234.5"
        );
        assert_eq!(spec.format_price(Price::new(dec!(123.45)), false), "123.45");
    }

    #[test]
    fn test_format_price_sig_figs() {
        let spec = MarketSpec {
            tick_size: Price::new(dec!(0.0001)),
            max_sig_figs: 5,
            max_price_decimals: 4,
            ..Default::default()
        };

        // 5 significant figures constraint
        assert_eq!(
            spec.format_price(Price::new(dec!(123456.789)), false),
            "123450"
        );
        assert_eq!(
            spec.format_price(Price::new(dec!(12345.6789)), false),
            "12345"
        );
        assert_eq!(
            spec.format_price(Price::new(dec!(1234.5678)), false),
            "1234.5"
        );
        assert_eq!(
            spec.format_price(Price::new(dec!(123.45678)), false),
            "123.45"
        );
        assert_eq!(
            spec.format_price(Price::new(dec!(12.345678)), false),
            "12.345"
        );
        assert_eq!(
            spec.format_price(Price::new(dec!(1.2345678)), false),
            "1.2345"
        );
    }

    #[test]
    fn test_format_price_small_values() {
        let spec = MarketSpec {
            tick_size: Price::new(dec!(0.00000001)),
            max_sig_figs: 5,
            max_price_decimals: 8,
            ..Default::default()
        };

        // Small values (e.g., SHIB)
        // 0.00001234 has 4 sig figs, stays as-is
        assert_eq!(
            spec.format_price(Price::new(dec!(0.00001234)), false),
            "0.00001234"
        );
        // 0.000012345 has 5 sig figs, stays as-is
        assert_eq!(
            spec.format_price(Price::new(dec!(0.000012345)), false),
            "0.00001234" // Truncated to 5 sig figs (12345), which is 0.000012345 -> 0.00001234
        );
        // 0.0000123456 has 6 sig figs, truncated to 5
        assert_eq!(
            spec.format_price(Price::new(dec!(0.0000123456)), false),
            "0.00001234"
        );
    }

    #[test]
    fn test_format_price_rounding_direction() {
        let spec = MarketSpec {
            tick_size: Price::new(dec!(0.01)),
            max_sig_figs: 5,
            max_price_decimals: 2,
            ..Default::default()
        };

        // Buy: round up (ceil)
        assert_eq!(spec.format_price(Price::new(dec!(100.001)), true), "100.01");
        assert_eq!(spec.format_price(Price::new(dec!(100.009)), true), "100.01");

        // Sell: round down (floor)
        assert_eq!(
            spec.format_price(Price::new(dec!(100.019)), false),
            "100.01"
        );
    }

    #[test]
    fn test_format_size_basic() {
        let spec = MarketSpec {
            lot_size: Size::new(dec!(0.001)),
            sz_decimals: 3,
            max_sig_figs: 5,
            ..Default::default()
        };

        // Basic cases
        assert_eq!(spec.format_size(Size::new(dec!(1.234))), "1.234");
        assert_eq!(spec.format_size(Size::new(dec!(1.2345))), "1.234"); // Rounded down
        assert_eq!(spec.format_size(Size::new(dec!(12.345))), "12.345");
        assert_eq!(spec.format_size(Size::new(dec!(123.456))), "123.45"); // 5 sig figs
    }

    #[test]
    fn test_format_size_trailing_zeros() {
        let spec = MarketSpec {
            lot_size: Size::new(dec!(0.001)),
            sz_decimals: 3,
            max_sig_figs: 5,
            ..Default::default()
        };

        // Trailing zeros should be stripped
        assert_eq!(spec.format_size(Size::new(dec!(1.0))), "1");
        assert_eq!(spec.format_size(Size::new(dec!(1.100))), "1.1");
        assert_eq!(spec.format_size(Size::new(dec!(1.010))), "1.01");
    }

    #[test]
    fn test_format_size_large_values() {
        let spec = MarketSpec {
            lot_size: Size::new(dec!(1)),
            sz_decimals: 0,
            max_sig_figs: 5,
            ..Default::default()
        };

        // Large integer sizes
        assert_eq!(spec.format_size(Size::new(dec!(12345))), "12345");
        // 123456 truncated to 5 sig figs = 123450 (floor, not round)
        assert_eq!(spec.format_size(Size::new(dec!(123456))), "123450");
    }

    #[test]
    fn test_format_zero() {
        let spec = MarketSpec::default();

        assert_eq!(spec.format_price(Price::new(dec!(0)), false), "0");
        assert_eq!(spec.format_size(Size::new(dec!(0))), "0");
    }

    #[test]
    fn test_tick_decimals() {
        let spec = MarketSpec {
            tick_size: Price::new(dec!(0.01)),
            ..Default::default()
        };
        assert_eq!(spec.tick_decimals(), 2);

        let spec = MarketSpec {
            tick_size: Price::new(dec!(0.0001)),
            ..Default::default()
        };
        assert_eq!(spec.tick_decimals(), 4);

        let spec = MarketSpec {
            tick_size: Price::new(dec!(1)),
            ..Default::default()
        };
        assert_eq!(spec.tick_decimals(), 0);
    }

    #[test]
    fn test_count_decimals() {
        assert_eq!(count_decimals(dec!(0.01)), 2);
        assert_eq!(count_decimals(dec!(0.001)), 3);
        assert_eq!(count_decimals(dec!(0.0001)), 4);
        assert_eq!(count_decimals(dec!(1.0)), 0);
        assert_eq!(count_decimals(dec!(100)), 0);
        assert_eq!(count_decimals(dec!(0.00000001)), 8);
    }

    #[test]
    fn test_format_decimal_with_constraints() {
        // 5 sig figs, 4 max decimals
        assert_eq!(
            format_decimal_with_constraints(dec!(12345.6789), 5, 4),
            "12345"
        );
        assert_eq!(
            format_decimal_with_constraints(dec!(1234.5678), 5, 4),
            "1234.5"
        );
        assert_eq!(
            format_decimal_with_constraints(dec!(123.4567), 5, 4),
            "123.45"
        );
        assert_eq!(
            format_decimal_with_constraints(dec!(12.34567), 5, 4),
            "12.345"
        );
        assert_eq!(
            format_decimal_with_constraints(dec!(1.234567), 5, 4),
            "1.2345"
        );

        // max_decimals more restrictive
        assert_eq!(
            format_decimal_with_constraints(dec!(1.234567), 5, 2),
            "1.23"
        );
    }
}
