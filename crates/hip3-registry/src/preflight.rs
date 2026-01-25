//! Preflight validation for HIP-3 trading bot.
//!
//! Validates exchange metadata before starting trading:
//! - P0-15: Identifies xyz DEX from perpDexs
//! - P0-27: Validates Coin-AssetId uniqueness
//! - P0-30: Ensures only perps (not spots) are used

use crate::error::{RegistryError, RegistryResult};
use hip3_core::{AssetId, DexId, MarketKey};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use tracing::{error, info, warn};

/// Deserialize tickSize as Decimal, accepting both String and Number.
fn deserialize_tick_size<'de, D>(deserializer: D) -> Result<Option<Decimal>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct TickSizeVisitor;

    impl<'de> Visitor<'de> for TickSizeVisitor {
        type Value = Option<Decimal>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string, number, or null for tickSize")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            v.parse::<Decimal>().map(Some).map_err(de::Error::custom)
        }

        fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            // Convert to string first to minimize precision loss from f64 rounding.
            // Note: f64 may already have lost some precision at this point,
            // but string parsing preserves what remains.
            let s = v.to_string();
            s.parse::<Decimal>().map(Some).map_err(de::Error::custom)
        }

        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Some(Decimal::from(v)))
        }

        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Some(Decimal::from(v)))
        }
    }

    deserializer.deserialize_any(TickSizeVisitor)
}

/// Raw perpDexs response from exchange meta endpoint.
#[derive(Debug, Deserialize)]
pub struct PerpDexsResponse {
    /// List of perp DEXs, each containing a list of markets.
    #[serde(rename = "perpDexs")]
    pub perp_dexs: Vec<PerpDexInfo>,
}

/// Information about a single perp DEX.
#[derive(Debug, Deserialize)]
pub struct PerpDexInfo {
    /// DEX name (e.g., "xyz/UNIT").
    pub name: String,
    /// Markets in this DEX.
    pub markets: Vec<PerpMarketInfo>,
    /// Original index in perpDexs API array (perp_dex_id for asset ID calculation).
    /// This is NOT the enumeration index after filtering nulls.
    #[serde(skip)]
    pub perp_dex_id: u16,
}

/// Information about a single perp market within a DEX.
#[derive(Debug, Deserialize)]
pub struct PerpMarketInfo {
    /// Market name/symbol (e.g., "BTC", "ETH").
    pub name: String,
    /// Size decimals for this market.
    #[serde(rename = "szDecimals")]
    pub sz_decimals: u8,
    /// Maximum leverage.
    #[serde(rename = "maxLeverage")]
    pub max_leverage: u8,
    /// Whether only isolated margin is supported.
    #[serde(rename = "onlyIsolated", default)]
    pub only_isolated: bool,
    /// Tick size from exchange (if provided).
    /// Accepts both String ("0.01") and Number (0.01) from API.
    #[serde(
        rename = "tickSize",
        default,
        deserialize_with = "deserialize_tick_size"
    )]
    pub tick_size: Option<Decimal>,
    /// Asset index from meta(dex=xyz) API for asset ID calculation.
    /// This is the correct index to use for: 100000 + dex_id * 10000 + asset_index
    /// NOTE: perpDexs API uses different ordering, so this must come from meta(dex=xyz).
    #[serde(skip)]
    pub asset_index: Option<u32>,
}

/// Result of preflight validation.
#[derive(Debug, Clone)]
pub struct PreflightResult {
    /// The identified xyz DEX index.
    pub xyz_dex_id: DexId,
    /// All valid markets discovered.
    pub markets: Vec<DiscoveredMarket>,
    /// Warnings (non-fatal issues).
    pub warnings: Vec<String>,
}

/// A discovered market from preflight.
#[derive(Debug, Clone)]
pub struct DiscoveredMarket {
    /// Market key (DEX + Asset).
    pub key: MarketKey,
    /// Market name/symbol.
    pub name: String,
    /// Size decimals.
    pub sz_decimals: u8,
    /// Maximum leverage.
    pub max_leverage: u8,
    /// Tick size from exchange (if provided).
    pub tick_size: Option<Decimal>,
}

/// Preflight validator.
pub struct PreflightChecker {
    /// Expected xyz DEX name pattern.
    xyz_pattern: String,
}

impl PreflightChecker {
    /// Create a new preflight checker.
    ///
    /// # Arguments
    /// * `xyz_pattern` - Pattern to identify xyz DEX (e.g., "xyz" or "xyz/UNIT")
    pub fn new(xyz_pattern: impl Into<String>) -> Self {
        Self {
            xyz_pattern: xyz_pattern.into(),
        }
    }

    /// Validate perpDexs response and extract trading configuration.
    ///
    /// # Returns
    /// - `Ok(PreflightResult)` if validation passes
    /// - `Err(RegistryError)` if critical validation fails
    ///
    /// # Validation Steps
    /// 1. Identify xyz DEX by name pattern
    /// 2. Check Coin-AssetId uniqueness within the DEX
    /// 3. Build market list for trading
    pub fn validate(&self, response: &PerpDexsResponse) -> RegistryResult<PreflightResult> {
        let mut warnings = Vec::new();

        // Step 1: Identify xyz DEX (P0-15)
        let (xyz_idx, xyz_dex) = self.find_xyz_dex(&response.perp_dexs)?;
        let xyz_dex_id = DexId::new(xyz_idx);

        info!(
            dex_name = %xyz_dex.name,
            dex_idx = xyz_idx,
            market_count = xyz_dex.markets.len(),
            "Identified xyz DEX"
        );

        // Step 2: Validate Coin-AssetId uniqueness (P0-27)
        self.validate_coin_uniqueness(&xyz_dex.markets, &mut warnings)?;

        // Step 3: Build market list
        let markets = self.build_market_list(xyz_dex_id, &xyz_dex.markets);

        info!(
            total_markets = markets.len(),
            warnings = warnings.len(),
            "Preflight validation complete"
        );

        Ok(PreflightResult {
            xyz_dex_id,
            markets,
            warnings,
        })
    }

    /// Find the xyz DEX by name pattern.
    /// Returns (perp_dex_id, dex_info).
    /// NOTE: perp_dex_id is the original API array index, used for asset ID calculation.
    pub fn find_xyz_dex<'a>(
        &self,
        dexs: &'a [PerpDexInfo],
    ) -> RegistryResult<(u16, &'a PerpDexInfo)> {
        for dex in dexs.iter() {
            // Match by pattern (case-insensitive)
            if dex
                .name
                .to_lowercase()
                .contains(&self.xyz_pattern.to_lowercase())
            {
                // Return perp_dex_id (original API array index), not enumeration index
                return Ok((dex.perp_dex_id, dex));
            }
        }

        // List available DEXs for debugging
        let available: Vec<_> = dexs.iter().map(|d| &d.name).collect();
        error!(
            pattern = %self.xyz_pattern,
            available = ?available,
            "xyz DEX not found in perpDexs"
        );

        Err(RegistryError::PreflightFailed(format!(
            "xyz DEX matching '{}' not found. Available: {:?}",
            self.xyz_pattern, available
        )))
    }

    /// Validate that all Coin names are unique within the DEX.
    fn validate_coin_uniqueness(
        &self,
        markets: &[PerpMarketInfo],
        warnings: &mut Vec<String>,
    ) -> RegistryResult<()> {
        let mut seen_names: HashMap<String, usize> = HashMap::new();
        let mut duplicates: Vec<String> = Vec::new();

        for (idx, market) in markets.iter().enumerate() {
            let name_lower = market.name.to_lowercase();

            if let Some(prev_idx) = seen_names.get(&name_lower) {
                duplicates.push(format!(
                    "'{}' at indices {} and {}",
                    market.name, prev_idx, idx
                ));
            } else {
                seen_names.insert(name_lower, idx);
            }
        }

        if !duplicates.is_empty() {
            // This is a critical error - we can't safely trade with duplicate names
            error!(
                duplicates = ?duplicates,
                "Duplicate Coin names detected - cannot ensure unique AssetId mapping"
            );

            return Err(RegistryError::PreflightFailed(format!(
                "Duplicate Coin names detected: {}. This violates P0-27 (Coin-AssetId uniqueness).",
                duplicates.join(", ")
            )));
        }

        // Check for suspicious patterns (warnings, not errors)
        self.check_suspicious_names(markets, warnings);

        Ok(())
    }

    /// Check for suspicious market names that might cause issues.
    fn check_suspicious_names(&self, markets: &[PerpMarketInfo], warnings: &mut Vec<String>) {
        let suspicious_patterns = ["test", "demo", "old", "deprecated"];

        for market in markets {
            let name_lower = market.name.to_lowercase();

            for pattern in &suspicious_patterns {
                if name_lower.contains(pattern) {
                    let msg = format!(
                        "Market '{}' contains suspicious pattern '{}' - verify this is intended",
                        market.name, pattern
                    );
                    warn!("{}", msg);
                    warnings.push(msg);
                }
            }

            // Check for unusual leverage
            if market.max_leverage > 100 {
                let msg = format!(
                    "Market '{}' has unusually high max_leverage: {}",
                    market.name, market.max_leverage
                );
                warn!("{}", msg);
                warnings.push(msg);
            }
        }
    }

    /// Build the list of discovered markets.
    ///
    /// # Asset ID Calculation
    /// Formula: 100000 + perp_dex_id * 10000 + asset_index
    ///
    /// IMPORTANT: asset_index must come from `meta(dex=xyz)` API, NOT `perpDexs` API.
    /// The perpDexs API returns markets in a different order than meta(dex=xyz).
    /// If `market.asset_index` is set, it will be used; otherwise falls back to
    /// enumerate index (which may be incorrect for xyz DEXs).
    fn build_market_list(
        &self,
        dex_id: DexId,
        markets: &[PerpMarketInfo],
    ) -> Vec<DiscoveredMarket> {
        let perp_dex_id = dex_id.index() as u32;
        markets
            .iter()
            .enumerate()
            .map(|(fallback_idx, market)| {
                // Use asset_index from meta(dex=xyz) if available, otherwise fall back
                // to enumerate index (which may be incorrect for builder-deployed perps)
                let asset_idx = market.asset_index.unwrap_or_else(|| {
                    warn!(
                        market = %market.name,
                        fallback_idx = fallback_idx,
                        "Using fallback enumerate index for asset_id (may be incorrect for xyz perps)"
                    );
                    fallback_idx as u32
                });
                // Calculate full asset ID for Hyperliquid order API
                let full_asset_id = 100000 + perp_dex_id * 10000 + asset_idx;
                DiscoveredMarket {
                    key: MarketKey::new(dex_id, AssetId::new(full_asset_id)),
                    name: market.name.clone(),
                    sz_decimals: market.sz_decimals,
                    max_leverage: market.max_leverage,
                    tick_size: market.tick_size,
                }
            })
            .collect()
    }
}

impl Default for PreflightChecker {
    fn default() -> Self {
        // Default to looking for "xyz" pattern
        Self::new("xyz")
    }
}

/// Validate that the provided market keys are a subset of discovered markets.
pub fn validate_market_keys(
    requested: &[MarketKey],
    discovered: &[DiscoveredMarket],
) -> RegistryResult<()> {
    let discovered_set: HashSet<MarketKey> = discovered.iter().map(|m| m.key).collect();

    let mut missing = Vec::new();
    for key in requested {
        if !discovered_set.contains(key) {
            missing.push(key.to_string());
        }
    }

    if !missing.is_empty() {
        return Err(RegistryError::PreflightFailed(format!(
            "Requested markets not found in perpDexs: {}",
            missing.join(", ")
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_perp_dexs() -> PerpDexsResponse {
        PerpDexsResponse {
            perp_dexs: vec![
                PerpDexInfo {
                    name: "xyz/UNIT".to_string(),
                    markets: vec![
                        PerpMarketInfo {
                            name: "BTC".to_string(),
                            sz_decimals: 3,
                            max_leverage: 50,
                            only_isolated: false,
                            tick_size: None,
                            asset_index: None,
                        },
                        PerpMarketInfo {
                            name: "ETH".to_string(),
                            sz_decimals: 2,
                            max_leverage: 50,
                            only_isolated: false,
                            tick_size: None,
                            asset_index: None,
                        },
                        PerpMarketInfo {
                            name: "SOL".to_string(),
                            sz_decimals: 1,
                            max_leverage: 20,
                            only_isolated: false,
                            tick_size: None,
                            asset_index: None,
                        },
                    ],
                    perp_dex_id: 1, // xyz is at index 1 in perpDexs API
                },
                PerpDexInfo {
                    name: "other/DEX".to_string(),
                    markets: vec![],
                    perp_dex_id: 2,
                },
            ],
        }
    }

    #[test]
    fn test_preflight_basic() {
        let checker = PreflightChecker::new("xyz");
        let response = sample_perp_dexs();

        let result = checker.validate(&response).unwrap();

        assert_eq!(result.xyz_dex_id.index(), 1);
        assert_eq!(result.markets.len(), 3);
        assert!(result.warnings.is_empty());

        // Check market details
        // Asset IDs use formula: 100000 + perpDexId * 10000 + assetIndex
        // perpDexId = 1 for this test, so: 110000, 110001, 110002
        assert_eq!(result.markets[0].name, "BTC");
        assert_eq!(result.markets[0].key.asset.index(), 110000);
        assert_eq!(result.markets[0].sz_decimals, 3);

        assert_eq!(result.markets[1].name, "ETH");
        assert_eq!(result.markets[1].key.asset.index(), 110001);

        assert_eq!(result.markets[2].name, "SOL");
        assert_eq!(result.markets[2].key.asset.index(), 110002);
    }

    #[test]
    fn test_preflight_xyz_not_found() {
        let checker = PreflightChecker::new("nonexistent");
        let response = sample_perp_dexs();

        let result = checker.validate(&response);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("xyz DEX matching 'nonexistent' not found"));
    }

    #[test]
    fn test_preflight_duplicate_coins() {
        let response = PerpDexsResponse {
            perp_dexs: vec![PerpDexInfo {
                name: "xyz/UNIT".to_string(),
                markets: vec![
                    PerpMarketInfo {
                        name: "BTC".to_string(),
                        sz_decimals: 3,
                        max_leverage: 50,
                        only_isolated: false,
                        tick_size: None,
                        asset_index: None,
                    },
                    PerpMarketInfo {
                        name: "ETH".to_string(),
                        sz_decimals: 2,
                        max_leverage: 50,
                        only_isolated: false,
                        tick_size: None,
                        asset_index: None,
                    },
                    PerpMarketInfo {
                        name: "BTC".to_string(), // Duplicate!
                        sz_decimals: 4,
                        max_leverage: 20,
                        only_isolated: false,
                        tick_size: None,
                        asset_index: None,
                    },
                ],
                perp_dex_id: 1,
            }],
        };

        let checker = PreflightChecker::default();
        let result = checker.validate(&response);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Duplicate Coin"));
    }

    #[test]
    fn test_preflight_suspicious_market_warning() {
        let response = PerpDexsResponse {
            perp_dexs: vec![PerpDexInfo {
                name: "xyz/UNIT".to_string(),
                markets: vec![
                    PerpMarketInfo {
                        name: "BTC".to_string(),
                        sz_decimals: 3,
                        max_leverage: 50,
                        only_isolated: false,
                        tick_size: None,
                        asset_index: None,
                    },
                    PerpMarketInfo {
                        name: "TEST_TOKEN".to_string(), // Suspicious
                        sz_decimals: 2,
                        max_leverage: 50,
                        only_isolated: false,
                        tick_size: None,
                        asset_index: None,
                    },
                ],
                perp_dex_id: 1,
            }],
        };

        let checker = PreflightChecker::default();
        let result = checker.validate(&response).unwrap();

        assert!(!result.warnings.is_empty());
        assert!(result.warnings[0].contains("TEST_TOKEN"));
    }

    #[test]
    fn test_validate_market_keys() {
        let checker = PreflightChecker::default();
        let response = sample_perp_dexs();
        let preflight = checker.validate(&response).unwrap();

        // Valid keys - using full asset ID format: 100000 + perpDexId * 10000 + assetIndex
        // perpDexId = 1 for xyz DEX (index 1 in perpDexs API array)
        let valid_keys = vec![
            MarketKey::from_indices(1, 110000), // BTC (asset 0): 100000 + 1*10000 + 0
            MarketKey::from_indices(1, 110001), // ETH (asset 1): 100000 + 1*10000 + 1
        ];
        assert!(validate_market_keys(&valid_keys, &preflight.markets).is_ok());

        // Invalid key
        let invalid_keys = vec![
            MarketKey::from_indices(1, 110000), // BTC - valid
            MarketKey::from_indices(1, 110099), // Invalid - doesn't exist
        ];
        let result = validate_market_keys(&invalid_keys, &preflight.markets);
        assert!(result.is_err());
        // MarketKey Display format is "dex:asset" (e.g., "1:110099")
        assert!(result.unwrap_err().to_string().contains("1:110099"));
    }

    #[test]
    fn test_preflight_case_insensitive() {
        let response = PerpDexsResponse {
            perp_dexs: vec![PerpDexInfo {
                name: "XYZ/UNIT".to_string(), // Uppercase
                markets: vec![PerpMarketInfo {
                    name: "BTC".to_string(),
                    sz_decimals: 3,
                    max_leverage: 50,
                    only_isolated: false,
                    tick_size: None,
                    asset_index: None,
                }],
                perp_dex_id: 1,
            }],
        };

        let checker = PreflightChecker::new("xyz"); // lowercase
        let result = checker.validate(&response);
        assert!(result.is_ok());
    }

    /// Test tick_size deserialization from String.
    #[test]
    fn test_deserialize_tick_size_string() {
        use rust_decimal::Decimal;
        use std::str::FromStr;

        let json = r#"{"name":"BTC","szDecimals":3,"maxLeverage":50,"tickSize":"0.01"}"#;
        let info: PerpMarketInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.tick_size, Some(Decimal::from_str("0.01").unwrap()));
    }

    /// Test tick_size deserialization from Number.
    #[test]
    fn test_deserialize_tick_size_number() {
        use rust_decimal::Decimal;
        use std::str::FromStr;

        let json = r#"{"name":"BTC","szDecimals":3,"maxLeverage":50,"tickSize":0.01}"#;
        let info: PerpMarketInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.tick_size, Some(Decimal::from_str("0.01").unwrap()));
    }

    /// Test tick_size deserialization from null.
    #[test]
    fn test_deserialize_tick_size_null() {
        let json = r#"{"name":"BTC","szDecimals":3,"maxLeverage":50,"tickSize":null}"#;
        let info: PerpMarketInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.tick_size, None);
    }

    /// Test tick_size deserialization when field is missing.
    #[test]
    fn test_deserialize_tick_size_missing() {
        let json = r#"{"name":"BTC","szDecimals":3,"maxLeverage":50}"#;
        let info: PerpMarketInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.tick_size, None);
    }
}
