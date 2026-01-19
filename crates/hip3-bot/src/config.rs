//! Application configuration.

use crate::error::{AppError, AppResult};
use hip3_detector::DetectorConfig;
use hip3_risk::RiskGateConfig;
use hip3_ws::{ConnectionConfig, SubscriptionTarget};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperatingMode {
    /// Phase A: Observation only, no trading.
    #[default]
    Observation,
    /// Phase B: Live trading enabled.
    Trading,
}

/// Market configuration with coin symbol mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketConfig {
    /// Asset index on Hyperliquid (0=BTC, 1=ETH, etc.).
    pub asset_idx: u16,
    /// Coin symbol (e.g., "BTC", "ETH", "SOL").
    pub coin: String,
}

/// Application configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Operating mode.
    pub mode: OperatingMode,
    /// WebSocket endpoint URL.
    pub ws_url: String,
    /// REST API info endpoint URL for preflight.
    /// Used to fetch perpDexs and discover xyz markets automatically.
    #[serde(default = "default_info_url")]
    pub info_url: String,
    /// Pattern to identify xyz DEX in perpDexs (case-insensitive).
    #[serde(default = "default_xyz_pattern")]
    pub xyz_pattern: String,
    /// Markets to monitor with coin symbols.
    /// If not specified, all markets from xyz DEX are used (auto-discovery).
    #[serde(default)]
    pub markets: Option<Vec<MarketConfig>>,
    /// WebSocket configuration.
    #[serde(default)]
    pub websocket: WsConfig,
    /// Risk gate configuration.
    #[serde(default)]
    pub risk: RiskGateConfig,
    /// Detector configuration.
    #[serde(default)]
    pub detector: DetectorConfig,
    /// Persistence configuration.
    #[serde(default)]
    pub persistence: PersistenceConfig,
    /// Telemetry configuration.
    #[serde(default)]
    pub telemetry: TelemetryConfig,
}

fn default_info_url() -> String {
    "https://api.hyperliquid.xyz/info".to_string()
}

fn default_xyz_pattern() -> String {
    "xyz".to_string()
}

/// WebSocket configuration subset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsConfig {
    /// Maximum reconnection attempts (0 = infinite).
    pub max_reconnect_attempts: u32,
    /// Base delay for reconnection backoff (ms).
    pub reconnect_base_delay_ms: u64,
    /// Heartbeat interval (ms).
    pub heartbeat_interval_ms: u64,
}

impl Default for WsConfig {
    fn default() -> Self {
        Self {
            max_reconnect_attempts: 0,
            reconnect_base_delay_ms: 1000,
            heartbeat_interval_ms: 45000,
        }
    }
}

impl From<WsConfig> for ConnectionConfig {
    fn from(cfg: WsConfig) -> Self {
        Self {
            url: String::new(), // Set separately
            max_reconnect_attempts: cfg.max_reconnect_attempts,
            reconnect_base_delay_ms: cfg.reconnect_base_delay_ms,
            reconnect_max_delay_ms: 60000,
            heartbeat_interval_ms: cfg.heartbeat_interval_ms,
            heartbeat_timeout_ms: 10000,
            subscriptions: Vec::new(), // Set separately from markets
        }
    }
}

impl AppConfig {
    /// Load configuration from file.
    pub fn load() -> AppResult<Self> {
        // Try to load from config file
        let config_path =
            std::env::var("HIP3_CONFIG").unwrap_or_else(|_| "config/default.toml".to_string());

        if Path::new(&config_path).exists() {
            Self::from_file(&config_path)
        } else {
            tracing::warn!(path = %config_path, "Config file not found, using defaults");
            Ok(Self::default())
        }
    }

    /// Load from a specific file.
    pub fn from_file(path: &str) -> AppResult<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| AppError::Config(format!("Failed to read config: {e}")))?;

        toml::from_str(&content)
            .map_err(|e| AppError::Config(format!("Failed to parse config: {e}")))
    }

    /// Check if in observation mode.
    pub fn is_observation_mode(&self) -> bool {
        self.mode == OperatingMode::Observation
    }

    /// Build subscription targets from market configuration.
    ///
    /// # Panics
    /// Panics if markets are not set. Must call `set_discovered_markets` first
    /// if markets were not specified in config.
    pub fn subscription_targets(&self) -> Vec<SubscriptionTarget> {
        self.markets
            .as_ref()
            .expect("Markets not set - run preflight first")
            .iter()
            .map(|m| SubscriptionTarget {
                coin: m.coin.clone(),
                asset_idx: m.asset_idx,
            })
            .collect()
    }

    /// Get market configs for iteration.
    ///
    /// # Panics
    /// Panics if markets are not set.
    pub fn get_markets(&self) -> &[MarketConfig] {
        self.markets
            .as_ref()
            .expect("Markets not set - run preflight first")
    }

    /// Set markets discovered from preflight.
    pub fn set_discovered_markets(&mut self, markets: Vec<MarketConfig>) {
        self.markets = Some(markets);
    }

    /// Check if markets are configured (either from config or auto-discovery).
    pub fn has_markets(&self) -> bool {
        self.markets.is_some()
    }
}

/// Persistence configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistenceConfig {
    /// Base directory for Parquet files.
    pub data_dir: String,
    /// Buffer size before flush.
    pub buffer_size: usize,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            data_dir: "./data/signals".to_string(),
            buffer_size: 100,
        }
    }
}

/// Telemetry configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    /// Prometheus metrics port.
    pub metrics_port: u16,
    /// Log level.
    pub log_level: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            metrics_port: 9090,
            log_level: "info".to_string(),
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            mode: OperatingMode::Observation,
            ws_url: "wss://api.hyperliquid.xyz/ws".to_string(),
            info_url: default_info_url(),
            xyz_pattern: default_xyz_pattern(),
            markets: None, // Auto-discover from perpDexs
            websocket: WsConfig::default(),
            risk: RiskGateConfig::default(),
            detector: DetectorConfig::default(),
            persistence: PersistenceConfig::default(),
            telemetry: TelemetryConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AppConfig::default();
        assert!(config.is_observation_mode());
        // Markets are None by default (auto-discovery)
        assert!(config.markets.is_none());
        assert!(!config.has_markets());
    }

    #[test]
    fn test_config_with_markets() {
        let mut config = AppConfig::default();
        config.set_discovered_markets(vec![MarketConfig {
            asset_idx: 0,
            coin: "BTC".to_string(),
        }]);
        assert!(config.has_markets());
        assert_eq!(config.get_markets().len(), 1);
    }

    #[test]
    fn test_config_serialization() {
        let config = AppConfig::default();
        let toml_str = toml::to_string(&config).unwrap();
        assert!(toml_str.contains("mode"));
        assert!(toml_str.contains("xyz_pattern"));
    }
}
