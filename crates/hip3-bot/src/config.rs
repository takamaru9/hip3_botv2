//! Application configuration.

use crate::error::{AppError, AppResult};
use hip3_dashboard::DashboardConfig;
use hip3_detector::DetectorConfig;
use hip3_risk::{
    BurstSignalConfig, CorrelationCooldownConfig, CorrelationPositionConfig, MarketHealthConfig,
    MaxDrawdownConfig, RiskGateConfig,
};
use hip3_ws::{ConnectionConfig, SubscriptionTarget};
use rust_decimal::Decimal;
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
    /// Asset index on Hyperliquid.
    /// For xyz/HIP-3 markets: 100000 + perp_dex_id * 10000 + asset_index
    /// Example: xyz:SILVER (perpDexId=1, index=27) = 110027
    pub asset_idx: u32,
    /// Coin symbol (e.g., "BTC", "ETH", "xyz:SILVER").
    pub coin: String,
    /// Per-market threshold in basis points. If None, uses global detector config.
    /// threshold_bps = taker_fee + slippage + min_edge
    #[serde(default)]
    pub threshold_bps: Option<u32>,
}

/// Time stop configuration for automatic position exit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeStopConfig {
    /// Position holding time threshold (ms). Default: 30,000 (30 seconds).
    #[serde(default = "default_time_stop_threshold_ms")]
    pub threshold_ms: u64,
    /// Reduce-only order timeout before retry (ms). Default: 60,000 (60 seconds).
    #[serde(default = "default_reduce_only_timeout_ms")]
    pub reduce_only_timeout_ms: u64,
    /// Check interval (ms). Default: 1,000 (1 second).
    #[serde(default = "default_check_interval_ms")]
    pub check_interval_ms: u64,
    /// Slippage tolerance for flatten orders (bps). Default: 50 (0.5%).
    #[serde(default = "default_slippage_bps")]
    pub slippage_bps: u64,
}

fn default_time_stop_threshold_ms() -> u64 {
    30_000
}

fn default_reduce_only_timeout_ms() -> u64 {
    60_000
}

fn default_check_interval_ms() -> u64 {
    1_000
}

fn default_slippage_bps() -> u64 {
    50
}

impl Default for TimeStopConfig {
    fn default() -> Self {
        Self {
            threshold_ms: default_time_stop_threshold_ms(),
            reduce_only_timeout_ms: default_reduce_only_timeout_ms(),
            check_interval_ms: default_check_interval_ms(),
            slippage_bps: default_slippage_bps(),
        }
    }
}

/// Risk monitor configuration for HardStop triggering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskMonitorConfig {
    /// Maximum consecutive order failures before HardStop. Default: 5.
    #[serde(default = "default_max_consecutive_failures")]
    pub max_consecutive_failures: u32,
    /// Maximum cumulative loss (USD) before HardStop. Default: 1000.0.
    #[serde(default = "default_max_loss_usd")]
    pub max_loss_usd: f64,
    /// Maximum flatten failures before critical alert. Default: 3.
    #[serde(default = "default_max_flatten_failed")]
    pub max_flatten_failed: u32,
    /// Monitoring window (seconds). Default: 3600 (1 hour).
    #[serde(default = "default_window_seconds")]
    pub window_seconds: u64,
}

/// Mark regression exit configuration for profit-taking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkRegressionConfig {
    /// Whether mark regression exit is enabled. Default: true.
    #[serde(default = "default_mark_regression_enabled")]
    pub enabled: bool,
    /// Exit threshold (bps). When BBO is within this distance from Oracle, exit is triggered.
    /// Default: 5 bps.
    #[serde(default = "default_mark_regression_exit_threshold_bps")]
    pub exit_threshold_bps: u32,
    /// Check interval (ms). Default: 200ms.
    #[serde(default = "default_mark_regression_check_interval_ms")]
    pub check_interval_ms: u64,
    /// Minimum holding time before exit can trigger (ms). Default: 1000ms.
    #[serde(default = "default_mark_regression_min_holding_time_ms")]
    pub min_holding_time_ms: u64,
    /// Slippage tolerance for flatten orders (bps). Default: 50 bps.
    #[serde(default = "default_mark_regression_slippage_bps")]
    pub slippage_bps: u64,

    // --- Time decay ---
    /// Enable time-based decay of exit threshold.
    /// Default: false.
    #[serde(default)]
    pub time_decay_enabled: bool,
    /// Time (ms) after which decay starts. Default: 5000.
    #[serde(default = "default_mark_regression_decay_start_ms")]
    pub decay_start_ms: u64,
    /// Minimum decay factor (0.0-1.0). Default: 0.2.
    #[serde(default = "default_mark_regression_min_decay_factor")]
    pub min_decay_factor: f64,
}

fn default_mark_regression_decay_start_ms() -> u64 {
    5000
}

fn default_mark_regression_min_decay_factor() -> f64 {
    0.2
}

fn default_mark_regression_enabled() -> bool {
    true
}

fn default_mark_regression_exit_threshold_bps() -> u32 {
    5
}

fn default_mark_regression_check_interval_ms() -> u64 {
    200
}

fn default_mark_regression_min_holding_time_ms() -> u64 {
    1000
}

fn default_mark_regression_slippage_bps() -> u64 {
    50
}

impl Default for MarkRegressionConfig {
    fn default() -> Self {
        Self {
            enabled: default_mark_regression_enabled(),
            exit_threshold_bps: default_mark_regression_exit_threshold_bps(),
            check_interval_ms: default_mark_regression_check_interval_ms(),
            min_holding_time_ms: default_mark_regression_min_holding_time_ms(),
            slippage_bps: default_mark_regression_slippage_bps(),
            time_decay_enabled: false,
            decay_start_ms: default_mark_regression_decay_start_ms(),
            min_decay_factor: default_mark_regression_min_decay_factor(),
        }
    }
}

fn default_max_consecutive_failures() -> u32 {
    5
}

fn default_max_loss_usd() -> f64 {
    1000.0
}

fn default_max_flatten_failed() -> u32 {
    3
}

fn default_window_seconds() -> u64 {
    3600
}

impl Default for RiskMonitorConfig {
    fn default() -> Self {
        Self {
            max_consecutive_failures: default_max_consecutive_failures(),
            max_loss_usd: default_max_loss_usd(),
            max_flatten_failed: default_max_flatten_failed(),
            window_seconds: default_window_seconds(),
        }
    }
}

/// Executor configuration for batch processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorConfig {
    /// Batch processing interval in milliseconds.
    /// Lower values reduce latency but increase CPU usage.
    /// Default: 20ms (optimized from original 100ms for edge erosion reduction).
    #[serde(default = "default_batch_interval_ms")]
    pub batch_interval_ms: u64,
}

fn default_batch_interval_ms() -> u64 {
    20 // Optimized from 100ms - reduces average latency from 50ms to 10ms
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            batch_interval_ms: default_batch_interval_ms(),
        }
    }
}

/// Dynamic position sizing configuration based on account balance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicSizingConfig {
    /// Whether dynamic sizing is enabled. Default: false.
    #[serde(default)]
    pub enabled: bool,

    /// Risk percentage per market (0.0 - 1.0). Default: 0.10 (10%).
    /// Used to calculate: dynamic_max = account_balance * risk_per_market_pct
    #[serde(default = "default_risk_per_market_pct")]
    pub risk_per_market_pct: f64,
}

fn default_risk_per_market_pct() -> f64 {
    0.10
}

impl Default for DynamicSizingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            risk_per_market_pct: default_risk_per_market_pct(),
        }
    }
}

/// Position management configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionConfig {
    /// Maximum number of concurrent positions across all markets.
    /// Default: 5
    #[serde(default = "default_max_concurrent_positions")]
    pub max_concurrent_positions: usize,

    /// Maximum total notional exposure across all positions (USD).
    /// Default: 100
    #[serde(default = "default_max_total_notional")]
    pub max_total_notional: Decimal,

    /// Maximum notional per market (USD).
    /// When dynamic_sizing is enabled, this acts as a hard cap.
    /// Default: 50
    #[serde(default = "default_max_notional_per_market")]
    pub max_notional_per_market: Decimal,

    /// Position resync interval (seconds). Set to 0 to disable.
    /// Default: 60 (1 minute)
    #[serde(default = "default_position_resync_interval_secs")]
    pub position_resync_interval_secs: u64,

    /// Dynamic sizing configuration based on account balance.
    #[serde(default)]
    pub dynamic_sizing: DynamicSizingConfig,
}

fn default_max_concurrent_positions() -> usize {
    5
}

fn default_max_total_notional() -> Decimal {
    Decimal::from(100)
}

fn default_max_notional_per_market() -> Decimal {
    Decimal::from(50)
}

fn default_position_resync_interval_secs() -> u64 {
    60 // 1 minute
}

impl Default for PositionConfig {
    fn default() -> Self {
        Self {
            max_concurrent_positions: default_max_concurrent_positions(),
            max_total_notional: default_max_total_notional(),
            max_notional_per_market: default_max_notional_per_market(),
            position_resync_interval_secs: default_position_resync_interval_secs(),
            dynamic_sizing: DynamicSizingConfig::default(),
        }
    }
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
    /// Time stop configuration (Trading mode only).
    #[serde(default)]
    pub time_stop: TimeStopConfig,
    /// Mark regression exit configuration (Trading mode only).
    #[serde(default)]
    pub mark_regression: MarkRegressionConfig,
    /// Oracle movement tracking configuration.
    #[serde(default)]
    pub oracle_tracking: Option<hip3_feed::OracleTrackerConfig>,
    /// Oracle-driven exit configuration (Trading mode only).
    #[serde(default)]
    pub oracle_exit: Option<hip3_position::OracleExitConfig>,
    /// Risk monitor configuration (Trading mode only).
    #[serde(default)]
    pub risk_monitor: RiskMonitorConfig,
    /// P2-3: MaxDrawdown gate configuration.
    #[serde(default)]
    pub max_drawdown: MaxDrawdownConfig,
    /// P2-4: Correlation cooldown gate configuration.
    #[serde(default)]
    pub correlation_cooldown: CorrelationCooldownConfig,
    /// P3-3: Correlation-weighted position limit configuration.
    #[serde(default)]
    pub correlation_position: CorrelationPositionConfig,
    /// Burst signal rate limiting configuration.
    #[serde(default)]
    pub burst_signal: BurstSignalConfig,
    /// Sprint 3 P2-E: Market health tracker configuration.
    #[serde(default)]
    pub market_health: MarketHealthConfig,
    /// Executor configuration (Trading mode only).
    #[serde(default)]
    pub executor: ExecutorConfig,
    /// Dashboard configuration.
    #[serde(default)]
    pub dashboard: DashboardConfig,
    /// Position limit configuration.
    #[serde(default)]
    pub position: PositionConfig,
    /// User address for trading subscriptions (required for Trading mode).
    /// Format: "0x..." Ethereum address.
    #[serde(default)]
    pub user_address: Option<String>,
    /// Expected signer address for HIP3_TRADING_KEY (API wallet / execution key).
    /// If set, the derived address from HIP3_TRADING_KEY must match this value.
    #[serde(default)]
    pub signer_address: Option<String>,
    /// Whether to use mainnet (true) or testnet (false).
    /// Default: false (testnet).
    #[serde(default)]
    pub is_mainnet: Option<bool>,
    /// Optional vault/active_pool address for post payload and signing.
    /// If set, it is included as `vaultAddress` and affects the signed action hash.
    #[serde(default)]
    pub vault_address: Option<String>,
    /// Whether to enable trading key (loaded from HIP3_TRADING_KEY env var).
    /// If Some, trading mode will load the private key from environment variable.
    #[serde(default)]
    pub private_key: Option<String>,
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
            user_address: None,        // Set separately for Trading mode
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

    /// Try to get market configs without panicking.
    ///
    /// Returns `None` if markets are not set (preflight not run yet).
    /// Prefer this over `get_markets()` when handling optional/early initialization.
    #[must_use]
    pub fn try_get_markets(&self) -> Option<&[MarketConfig]> {
        self.markets.as_deref()
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
            time_stop: TimeStopConfig::default(),
            mark_regression: MarkRegressionConfig::default(),
            risk_monitor: RiskMonitorConfig::default(),
            max_drawdown: MaxDrawdownConfig::default(),
            correlation_cooldown: CorrelationCooldownConfig::default(),
            correlation_position: CorrelationPositionConfig::default(),
            burst_signal: BurstSignalConfig::default(),
            market_health: MarketHealthConfig::default(),
            executor: ExecutorConfig::default(),
            dashboard: DashboardConfig::default(),
            position: PositionConfig::default(),
            user_address: None,
            signer_address: None,
            is_mainnet: None,
            vault_address: None,
            private_key: None,
            oracle_tracking: None,
            oracle_exit: None,
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
            threshold_bps: None,
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
