//! HTTP client for fetching exchange metadata.
//!
//! Provides functionality to fetch perpDexs and other metadata from the exchange
//! REST API for preflight validation (P0-15, P0-26, P0-27).

use crate::error::{RegistryError, RegistryResult};
use crate::preflight::PerpDexsResponse;
use crate::user_state::ClearinghouseStateResponse;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Default timeout for API requests.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Request type for info endpoint.
#[derive(Debug, Serialize)]
struct InfoRequest {
    #[serde(rename = "type")]
    request_type: String,
}

/// Request type for info endpoint with dex parameter.
#[derive(Debug, Serialize)]
struct InfoRequestWithDex {
    #[serde(rename = "type")]
    request_type: String,
    /// DEX name for builder-deployed perps (e.g., "xyz").
    dex: String,
}

/// Request type for info endpoint with user address and optional dex.
///
/// Used for clearinghouseState to fetch perpDex positions (BUG-005).
#[derive(Debug, Serialize)]
struct InfoRequestWithUserAndDex {
    #[serde(rename = "type")]
    request_type: String,
    /// User address (0x...).
    user: String,
    /// DEX name for builder-deployed perps (e.g., "xyz").
    /// Required to fetch perpDex positions. Without this, only L1 perp positions are returned.
    #[serde(skip_serializing_if = "Option::is_none")]
    dex: Option<String>,
}

/// Raw perpDex entry from API.
#[derive(Debug, Deserialize)]
struct RawPerpDexEntry {
    /// DEX name (e.g., "xyz").
    name: Option<String>,
    /// Asset to streaming OI cap mapping (e.g., [["xyz:AAPL", "50000000.0"], ...])
    #[serde(rename = "assetToStreamingOiCap", default)]
    asset_to_streaming_oi_cap: Vec<(String, String)>,
}

/// Client for fetching exchange metadata.
pub struct MetaClient {
    /// HTTP client.
    client: Client,
    /// Info endpoint URL.
    info_url: String,
}

impl MetaClient {
    /// Create a new meta client.
    ///
    /// # Arguments
    /// * `info_url` - URL of the info endpoint (e.g., "https://api.hyperliquid.xyz/info")
    pub fn new(info_url: impl Into<String>) -> RegistryResult<Self> {
        let client = Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(|e| RegistryError::HttpClient(format!("Failed to create HTTP client: {e}")))?;

        Ok(Self {
            client,
            info_url: info_url.into(),
        })
    }

    /// Fetch perpDexs from the exchange API.
    ///
    /// # Returns
    /// `PerpDexsResponse` containing all perp DEX information.
    ///
    /// # API Details
    /// Uses `{"type": "perpDexs"}` endpoint which returns array of DEX info.
    /// Each DEX has `name`, `assetToStreamingOiCap` (asset list), etc.
    pub async fn fetch_perp_dexs(&self) -> RegistryResult<PerpDexsResponse> {
        info!(url = %self.info_url, "Fetching perpDexs from exchange");

        // First, get the perpDexs list
        let request = InfoRequest {
            request_type: "perpDexs".to_string(),
        };

        let response = self
            .client
            .post(&self.info_url)
            .json(&request)
            .send()
            .await
            .map_err(|e| RegistryError::HttpClient(format!("HTTP request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RegistryError::HttpClient(format!("HTTP {status}: {body}")));
        }

        // Parse perpDexs response (array of DEX entries, some may be null)
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| RegistryError::HttpClient(format!("Failed to parse response: {e}")))?;

        debug!("Raw perpDexs response received");

        let entries = body.as_array().ok_or_else(|| {
            RegistryError::HttpClient("perpDexs response is not an array".to_string())
        })?;

        // Also fetch meta to get szDecimals for each asset
        let asset_specs = self.fetch_hip3_asset_specs().await.unwrap_or_default();

        let mut perp_dexs = Vec::new();

        for (idx, entry) in entries.iter().enumerate() {
            // Skip null entries
            if entry.is_null() {
                continue;
            }

            // Parse DEX entry
            let dex_entry: RawPerpDexEntry =
                serde_json::from_value(entry.clone()).map_err(|e| {
                    RegistryError::HttpClient(format!("Failed to parse DEX entry {idx}: {e}"))
                })?;

            let dex_name = match &dex_entry.name {
                Some(name) if !name.is_empty() => name.clone(),
                _ => {
                    warn!(idx, "Skipping DEX with empty name");
                    continue;
                }
            };

            // Extract markets from assetToStreamingOiCap
            let mut markets = Vec::new();
            for (asset_key, _oi_cap) in &dex_entry.asset_to_streaming_oi_cap {
                // asset_key format: "xyz:AAPL" or similar
                // Extract the asset name after the colon
                let asset_name = if let Some((_dex, asset)) = asset_key.split_once(':') {
                    asset.to_string()
                } else {
                    asset_key.clone()
                };

                // Get specs from meta if available, otherwise use defaults
                let (sz_decimals, max_leverage) =
                    asset_specs.get(asset_key).copied().unwrap_or((2, 10)); // Default: 2 decimals, 10x leverage

                markets.push(crate::preflight::PerpMarketInfo {
                    name: asset_name,
                    sz_decimals,
                    max_leverage,
                    only_isolated: true, // HIP-3 is isolated-only
                    tick_size: None,     // Not available from current API; use default in SpecCache
                    asset_index: None,   // Will be set later from meta(dex=xyz) response
                });
            }

            perp_dexs.push(crate::preflight::PerpDexInfo {
                name: dex_name,
                markets,
                perp_dex_id: idx as u16, // Preserve original array index for asset ID calculation
            });
        }

        // Fetch correct asset indices from meta(dex=xyz) for each xyz DEX
        // IMPORTANT: perpDexs returns markets in different order than meta(dex=xyz)
        // Asset IDs must use indices from meta(dex=xyz), not perpDexs
        for dex in &mut perp_dexs {
            if let Ok(index_map) = self.fetch_dex_meta_indices(&dex.name).await {
                for market in &mut dex.markets {
                    // Look up the correct index using full coin name (e.g., "xyz:SILVER")
                    let full_name = format!("{}:{}", dex.name, market.name);
                    if let Some(&idx) = index_map.get(&full_name) {
                        market.asset_index = Some(idx);
                        debug!(
                            dex = %dex.name,
                            market = %market.name,
                            asset_index = idx,
                            "Set asset_index from meta(dex) API"
                        );
                    } else {
                        warn!(
                            dex = %dex.name,
                            market = %market.name,
                            "Could not find asset_index in meta(dex) response"
                        );
                    }
                }
            } else {
                warn!(
                    dex = %dex.name,
                    "Failed to fetch meta(dex) for asset indices, using perpDexs order (may be incorrect)"
                );
            }
        }

        info!(
            dex_count = perp_dexs.len(),
            total_markets = perp_dexs.iter().map(|d| d.markets.len()).sum::<usize>(),
            "Successfully fetched perpDexs with asset indices"
        );

        Ok(PerpDexsResponse { perp_dexs })
    }

    /// Fetch asset indices from meta(dex=xyz) API.
    ///
    /// This is the correct source for asset IDs. The perpDexs API returns
    /// markets in a different order than what's used for asset ID calculation.
    ///
    /// # Returns
    /// Map from full coin name (e.g., "xyz:SILVER") to asset index.
    async fn fetch_dex_meta_indices(&self, dex_name: &str) -> RegistryResult<HashMap<String, u32>> {
        debug!(dex = %dex_name, "Fetching meta(dex) for asset indices");

        let request = InfoRequestWithDex {
            request_type: "meta".to_string(),
            dex: dex_name.to_string(),
        };

        let response = self
            .client
            .post(&self.info_url)
            .json(&request)
            .send()
            .await
            .map_err(|e| RegistryError::HttpClient(format!("HTTP request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(RegistryError::HttpClient(format!(
                "meta(dex={dex_name}) failed: HTTP {status}: {body}"
            )));
        }

        let body: serde_json::Value = response.json().await.map_err(|e| {
            RegistryError::HttpClient(format!("Failed to parse meta(dex) response: {e}"))
        })?;

        let mut index_map = HashMap::new();

        // Extract universe array and build name -> index map
        if let Some(universe) = body.get("universe").and_then(|u| u.as_array()) {
            for (idx, entry) in universe.iter().enumerate() {
                if let Some(name) = entry.get("name").and_then(|n| n.as_str()) {
                    index_map.insert(name.to_string(), idx as u32);
                }
            }
        }

        info!(
            dex = %dex_name,
            asset_count = index_map.len(),
            "Fetched asset indices from meta(dex)"
        );

        Ok(index_map)
    }

    /// Fetch HIP-3 asset specifications (szDecimals, maxLeverage).
    ///
    /// This fetches the meta endpoint and extracts specs for HIP-3 assets.
    /// Returns a map from "dex:asset" key to (sz_decimals, max_leverage).
    async fn fetch_hip3_asset_specs(&self) -> RegistryResult<HashMap<String, (u8, u8)>> {
        debug!("Fetching HIP-3 asset specs");

        // Get metaAndAssetCtxs to find HIP-3 assets with full specs
        let request = InfoRequest {
            request_type: "metaAndAssetCtxs".to_string(),
        };

        let response = self
            .client
            .post(&self.info_url)
            .json(&request)
            .send()
            .await
            .map_err(|e| RegistryError::HttpClient(format!("HTTP request failed: {e}")))?;

        if !response.status().is_success() {
            return Ok(HashMap::new());
        }

        let body: serde_json::Value = response.json().await.map_err(|e| {
            RegistryError::HttpClient(format!("Failed to parse meta response: {e}"))
        })?;

        // Try to extract specs from the meta response
        // The structure may vary, so we use a best-effort approach
        let mut specs = HashMap::new();

        // Check if there's a universe in the meta part
        if let Some(meta) = body.get(0) {
            if let Some(universe) = meta.get("universe").and_then(|u| u.as_array()) {
                for entry in universe {
                    if let (Some(name), Some(sz_decimals)) = (
                        entry.get("name").and_then(|n| n.as_str()),
                        entry.get("szDecimals").and_then(|s| s.as_u64()),
                    ) {
                        let max_leverage = entry
                            .get("maxLeverage")
                            .and_then(|m| m.as_u64())
                            .unwrap_or(10) as u8;

                        // Check if this is a HIP-3 asset (contains ':')
                        if name.contains(':') {
                            specs.insert(name.to_string(), (sz_decimals as u8, max_leverage));
                        }
                    }
                }
            }
        }

        debug!(spec_count = specs.len(), "Fetched HIP-3 asset specs");
        Ok(specs)
    }

    /// Fetch clearinghouse state for a user.
    ///
    /// Contains account summary and open positions.
    ///
    /// # Arguments
    /// * `user_address` - User's Ethereum address (0x...).
    /// * `dex` - Optional DEX name for perpDex positions (e.g., "xyz").
    ///   If None, only L1 perp positions are returned.
    ///   BUG-005: Required for fetching perpDex positions.
    ///
    /// # Returns
    /// `ClearinghouseStateResponse` containing margin summary and positions.
    pub async fn fetch_clearinghouse_state(
        &self,
        user_address: &str,
        dex: Option<&str>,
    ) -> RegistryResult<ClearinghouseStateResponse> {
        info!(
            url = %self.info_url,
            user = %user_address,
            dex = ?dex,
            "Fetching clearinghouseState from exchange"
        );

        let request = InfoRequestWithUserAndDex {
            request_type: "clearinghouseState".to_string(),
            user: user_address.to_string(),
            dex: dex.map(|s| s.to_string()),
        };

        let response = self
            .client
            .post(&self.info_url)
            .json(&request)
            .send()
            .await
            .map_err(|e| RegistryError::HttpClient(format!("HTTP request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RegistryError::HttpClient(format!("HTTP {status}: {body}")));
        }

        let state: ClearinghouseStateResponse = response.json().await.map_err(|e| {
            RegistryError::HttpClient(format!("Failed to parse clearinghouseState: {e}"))
        })?;

        info!(
            positions = state.asset_positions.len(),
            "Fetched clearinghouseState successfully"
        );

        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_info_request_serialization() {
        let request = InfoRequest {
            request_type: "perpDexs".to_string(),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(json, r#"{"type":"perpDexs"}"#);
    }
}
