//! Preflight validation for HIP-3 trading bot.
//!
//! Validates exchange metadata before starting trading:
//! - P0-15: Identifies xyz DEX from perpDexs
//! - P0-27: Validates Coin-AssetId uniqueness
//! - P0-30: Ensures only perps (not spots) are used

use crate::error::{RegistryError, RegistryResult};
use hip3_core::{AssetId, DexId, MarketKey};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use tracing::{error, info, warn};

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
    fn find_xyz_dex<'a>(&self, dexs: &'a [PerpDexInfo]) -> RegistryResult<(u16, &'a PerpDexInfo)> {
        for (idx, dex) in dexs.iter().enumerate() {
            // Match by pattern (case-insensitive)
            if dex
                .name
                .to_lowercase()
                .contains(&self.xyz_pattern.to_lowercase())
            {
                return Ok((idx as u16, dex));
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

    /// Build the list of discoveredmarkets.
    fn build_market_list(
        &self,
        dex_id: DexId,
        markets: &[PerpMarketInfo],
    ) -> Vec<DiscoveredMarket> {
        markets
            .iter()
            .enumerate()
            .map(|(idx, market)| DiscoveredMarket {
                key: MarketKey::new(dex_id, AssetId::new(idx as u16)),
                name: market.name.clone(),
                sz_decimals: market.sz_decimals,
                max_leverage: market.max_leverage,
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
                        },
                        PerpMarketInfo {
                            name: "ETH".to_string(),
                            sz_decimals: 2,
                            max_leverage: 50,
                            only_isolated: false,
                        },
                        PerpMarketInfo {
                            name: "SOL".to_string(),
                            sz_decimals: 1,
                            max_leverage: 20,
                            only_isolated: false,
                        },
                    ],
                },
                PerpDexInfo {
                    name: "other/DEX".to_string(),
                    markets: vec![],
                },
            ],
        }
    }

    #[test]
    fn test_preflight_basic() {
        let checker = PreflightChecker::new("xyz");
        let response = sample_perp_dexs();

        let result = checker.validate(&response).unwrap();

        assert_eq!(result.xyz_dex_id.index(), 0);
        assert_eq!(result.markets.len(), 3);
        assert!(result.warnings.is_empty());

        // Check market details
        assert_eq!(result.markets[0].name, "BTC");
        assert_eq!(result.markets[0].key.asset.index(), 0);
        assert_eq!(result.markets[0].sz_decimals, 3);

        assert_eq!(result.markets[1].name, "ETH");
        assert_eq!(result.markets[1].key.asset.index(), 1);

        assert_eq!(result.markets[2].name, "SOL");
        assert_eq!(result.markets[2].key.asset.index(), 2);
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
                    },
                    PerpMarketInfo {
                        name: "ETH".to_string(),
                        sz_decimals: 2,
                        max_leverage: 50,
                        only_isolated: false,
                    },
                    PerpMarketInfo {
                        name: "BTC".to_string(), // Duplicate!
                        sz_decimals: 4,
                        max_leverage: 20,
                        only_isolated: false,
                    },
                ],
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
                    },
                    PerpMarketInfo {
                        name: "TEST_TOKEN".to_string(), // Suspicious
                        sz_decimals: 2,
                        max_leverage: 50,
                        only_isolated: false,
                    },
                ],
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

        // Valid keys
        let valid_keys = vec![
            MarketKey::from_indices(0, 0), // BTC
            MarketKey::from_indices(0, 1), // ETH
        ];
        assert!(validate_market_keys(&valid_keys, &preflight.markets).is_ok());

        // Invalid key
        let invalid_keys = vec![
            MarketKey::from_indices(0, 0),  // BTC - valid
            MarketKey::from_indices(0, 99), // Invalid - doesn't exist
        ];
        let result = validate_market_keys(&invalid_keys, &preflight.markets);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("xyz:99"));
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
                }],
            }],
        };

        let checker = PreflightChecker::new("xyz"); // lowercase
        let result = checker.validate(&response);
        assert!(result.is_ok());
    }
}
