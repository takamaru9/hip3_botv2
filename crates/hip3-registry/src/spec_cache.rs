//! Market specification cache.
//!
//! Caches market specifications from perpDexs and detects
//! parameter changes that should trigger trading halt.

use crate::error::{RegistryError, RegistryResult};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hip3_core::{MarketKey, MarketSpec, Price, Size, HIP3_MAX_SIG_FIGS};
use rust_decimal::Decimal;
use serde::Deserialize;
use tracing::error;

/// Raw market spec from perpDexs response.
#[derive(Debug, Deserialize)]
pub struct RawPerpSpec {
    pub name: String,
    #[serde(rename = "szDecimals")]
    pub sz_decimals: u8,
    #[serde(rename = "maxLeverage")]
    pub max_leverage: u8,
    #[serde(rename = "onlyIsolated", default)]
    pub only_isolated: bool,
    /// P1-1: Tick size from exchange (if provided).
    /// Now accepts Decimal directly (already parsed during deserialization).
    #[serde(rename = "tickSize", default)]
    pub tick_size: Option<Decimal>,
}

/// Spec cache entry with change tracking.
#[derive(Debug, Clone)]
pub struct SpecCacheEntry {
    pub spec: MarketSpec,
    pub last_update: DateTime<Utc>,
    pub version: u64,
}

/// Market specification cache.
pub struct SpecCache {
    /// Cached specs by market key.
    specs: DashMap<MarketKey, SpecCacheEntry>,
    /// Default taker fee in bps.
    default_taker_fee_bps: u16,
    /// Default maker fee in bps.
    default_maker_fee_bps: i16,
}

impl SpecCache {
    /// Create a new spec cache with default fees.
    pub fn new(default_taker_fee_bps: u16, default_maker_fee_bps: i16) -> Self {
        Self {
            specs: DashMap::new(),
            default_taker_fee_bps,
            default_maker_fee_bps,
        }
    }

    /// Get spec for a market.
    pub fn get(&self, key: &MarketKey) -> Option<MarketSpec> {
        self.specs.get(key).map(|entry| entry.spec.clone())
    }

    /// Update spec and detect changes.
    ///
    /// Returns `Err(ParamChange)` if material parameters changed.
    pub fn update(&self, key: MarketKey, new_spec: MarketSpec) -> RegistryResult<()> {
        if let Some(existing) = self.specs.get(&key) {
            if existing.spec.has_material_change(&new_spec) {
                let msg = format!(
                    "{}: tick_size {}->{}, lot_size {}->{}, fee {}->{}",
                    key,
                    existing.spec.tick_size,
                    new_spec.tick_size,
                    existing.spec.lot_size,
                    new_spec.lot_size,
                    existing.spec.taker_fee_bps,
                    new_spec.taker_fee_bps
                );
                error!(%msg, "PARAMETER CHANGE DETECTED");
                return Err(RegistryError::ParamChange(msg));
            }
        }

        let entry = SpecCacheEntry {
            spec: new_spec,
            last_update: Utc::now(),
            version: self.specs.get(&key).map(|e| e.version + 1).unwrap_or(1),
        };

        self.specs.insert(key, entry);
        Ok(())
    }

    /// Parse spec from raw perpDexs data.
    ///
    /// P1-1: Parses tick_size from exchange if provided, otherwise uses default.
    /// Derives max_price_decimals from tick_size, or from formula if tick_size unavailable.
    pub fn parse_spec(&self, raw: &RawPerpSpec) -> MarketSpec {
        // Calculate lot size from sz_decimals
        // sz_decimals=3 means minimum 0.001
        let lot_size = Size::new(Decimal::ONE / Decimal::from(10u64.pow(raw.sz_decimals as u32)));

        // P1-1: Use tick_size from exchange if provided
        let (tick_size, max_price_decimals) = if let Some(ts) = raw.tick_size {
            // tick_size provided: derive max_price_decimals from it
            (Price::new(ts), Self::decimals_from_tick_size(ts))
        } else {
            // No tickSize from API: use default 0.01 and derive max_price_decimals from formula
            // Per Hyperliquid docs: max_price_decimals = MAX_DECIMALS - szDecimals = 6 - szDecimals
            let default_tick = Price::new(Decimal::new(1, 2)); // 0.01
            let max_decimals = 6u8.saturating_sub(raw.sz_decimals);
            (default_tick, max_decimals)
        };

        MarketSpec {
            tick_size,
            lot_size,
            min_size: lot_size,
            max_leverage: raw.max_leverage,
            taker_fee_bps: self.default_taker_fee_bps,
            maker_fee_bps: self.default_maker_fee_bps,
            oi_cap: None,
            is_active: true,
            name: raw.name.clone(),
            // Precision fields (P0-23)
            sz_decimals: raw.sz_decimals,
            max_sig_figs: HIP3_MAX_SIG_FIGS,
            max_price_decimals,
        }
    }

    /// P1-1: Calculate decimal places from tick size.
    ///
    /// Examples:
    /// - tick_size=0.01 -> 2 decimals
    /// - tick_size=0.001 -> 3 decimals
    /// - tick_size=0.5 -> 1 decimal
    /// - tick_size=1 -> 0 decimals
    fn decimals_from_tick_size(tick_size: Decimal) -> u8 {
        if tick_size.is_zero() {
            return 8; // Default to 8 if zero
        }

        // Find how many decimal places in tick_size
        // E.g., 0.01 = 1/100 -> 2 decimals
        let scale = tick_size.scale();
        scale as u8
    }

    /// Get all cached market keys.
    pub fn market_keys(&self) -> Vec<MarketKey> {
        self.specs.iter().map(|entry| *entry.key()).collect()
    }

    /// Check if spec exists for a market.
    pub fn contains(&self, key: &MarketKey) -> bool {
        self.specs.contains_key(key)
    }

    /// Remove a market spec.
    pub fn remove(&self, key: &MarketKey) -> Option<MarketSpec> {
        self.specs.remove(key).map(|(_, entry)| entry.spec)
    }

    /// Clear all cached specs.
    pub fn clear(&self) {
        self.specs.clear();
    }
}

impl Default for SpecCache {
    fn default() -> Self {
        Self::new(4, 1) // Default: 0.04% taker, 0.01% maker
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetId, DexId};
    use rust_decimal_macros::dec;

    fn test_key() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    #[test]
    fn test_spec_cache_basic() {
        let cache = SpecCache::default();
        let key = test_key();

        assert!(cache.get(&key).is_none());

        let spec = MarketSpec::default();
        cache.update(key, spec.clone()).unwrap();

        let cached = cache.get(&key).unwrap();
        assert_eq!(cached.tick_size, spec.tick_size);
    }

    #[test]
    fn test_param_change_detection() {
        let cache = SpecCache::default();
        let key = test_key();

        let mut spec1 = MarketSpec::default();
        spec1.tick_size = Price::new(dec!(0.01));
        cache.update(key, spec1).unwrap();

        // Same spec should be fine
        let mut spec2 = MarketSpec::default();
        spec2.tick_size = Price::new(dec!(0.01));
        cache.update(key, spec2).unwrap();

        // Different tick_size should fail
        let mut spec3 = MarketSpec::default();
        spec3.tick_size = Price::new(dec!(0.001));
        let result = cache.update(key, spec3);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_spec() {
        let cache = SpecCache::default();
        let raw = RawPerpSpec {
            name: "BTC".to_string(),
            sz_decimals: 3,
            max_leverage: 50,
            only_isolated: false,
            tick_size: None, // P1-1: No tick_size -> use default 0.01, max_price_decimals = 6 - sz_decimals
        };

        let spec = cache.parse_spec(&raw);
        assert_eq!(spec.name, "BTC");
        assert_eq!(spec.max_leverage, 50);
        assert_eq!(spec.lot_size.inner(), dec!(0.001));
        // P0-23: sz_decimals preserved
        assert_eq!(spec.sz_decimals, 3);
        assert_eq!(spec.max_sig_figs, 5);
        // P1-1: Default tick_size 0.01, max_price_decimals = 6 - 3 = 3
        assert_eq!(spec.tick_size.inner(), dec!(0.01));
        assert_eq!(spec.max_price_decimals, 3); // 6 - sz_decimals
    }

    #[test]
    fn test_parse_spec_various_sz_decimals() {
        let cache = SpecCache::default();

        // BTC-like (3 decimals)
        let raw_btc = RawPerpSpec {
            name: "BTC".to_string(),
            sz_decimals: 3,
            max_leverage: 50,
            only_isolated: false,
            tick_size: None,
        };
        let spec_btc = cache.parse_spec(&raw_btc);
        assert_eq!(spec_btc.sz_decimals, 3);
        assert_eq!(spec_btc.lot_size.inner(), dec!(0.001));

        // SHIB-like (0 decimals, integer sizes)
        let raw_shib = RawPerpSpec {
            name: "SHIB".to_string(),
            sz_decimals: 0,
            max_leverage: 20,
            only_isolated: false,
            tick_size: None,
        };
        let spec_shib = cache.parse_spec(&raw_shib);
        assert_eq!(spec_shib.sz_decimals, 0);
        assert_eq!(spec_shib.lot_size.inner(), dec!(1));

        // Some alt with 5 decimals
        let raw_alt = RawPerpSpec {
            name: "ALT".to_string(),
            sz_decimals: 5,
            max_leverage: 10,
            only_isolated: false,
            tick_size: None,
        };
        let spec_alt = cache.parse_spec(&raw_alt);
        assert_eq!(spec_alt.sz_decimals, 5);
        assert_eq!(spec_alt.lot_size.inner(), dec!(0.00001));
    }

    /// P1-1: Test tick_size parsing from exchange.
    #[test]
    fn test_parse_spec_with_tick_size() {
        let cache = SpecCache::default();

        // Tick size 0.001 -> 3 decimals
        let raw = RawPerpSpec {
            name: "ETH".to_string(),
            sz_decimals: 4,
            max_leverage: 25,
            only_isolated: false,
            tick_size: Some(dec!(0.001)),
        };
        let spec = cache.parse_spec(&raw);
        assert_eq!(spec.tick_size.inner(), dec!(0.001));
        assert_eq!(spec.max_price_decimals, 3);

        // Tick size 0.5 -> 1 decimal
        let raw2 = RawPerpSpec {
            name: "SHIB".to_string(),
            sz_decimals: 0,
            max_leverage: 20,
            only_isolated: false,
            tick_size: Some(dec!(0.5)),
        };
        let spec2 = cache.parse_spec(&raw2);
        assert_eq!(spec2.tick_size.inner(), dec!(0.5));
        assert_eq!(spec2.max_price_decimals, 1);

        // Tick size 1 -> 0 decimals
        let raw3 = RawPerpSpec {
            name: "TEST".to_string(),
            sz_decimals: 0,
            max_leverage: 10,
            only_isolated: false,
            tick_size: Some(dec!(1)),
        };
        let spec3 = cache.parse_spec(&raw3);
        assert_eq!(spec3.tick_size.inner(), dec!(1));
        assert_eq!(spec3.max_price_decimals, 0);
    }

    /// P1-1: Test decimals_from_tick_size function.
    #[test]
    fn test_decimals_from_tick_size() {
        assert_eq!(SpecCache::decimals_from_tick_size(dec!(0.01)), 2);
        assert_eq!(SpecCache::decimals_from_tick_size(dec!(0.001)), 3);
        assert_eq!(SpecCache::decimals_from_tick_size(dec!(0.5)), 1);
        assert_eq!(SpecCache::decimals_from_tick_size(dec!(1)), 0);
        assert_eq!(SpecCache::decimals_from_tick_size(dec!(0.00000001)), 8);
        assert_eq!(SpecCache::decimals_from_tick_size(dec!(0)), 8); // Default for zero
    }
}
