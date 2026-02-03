//! Main application orchestration.
//!
//! Coordinates all components:
//! - WebSocket connection
//! - Market data feeds
//! - Risk gate checks
//! - Dislocation detection
//! - Signal persistence
//! - Daily metrics tracking (P0-31)
//! - Automatic market discovery (P0-15, P0-26, P0-27)

use crate::config::{AppConfig, MarketConfig, OperatingMode};
use crate::edge_tracker::EdgeTracker;
use crate::error::{AppError, AppResult};
use alloy::primitives::Address;
use chrono::Utc;
use hip3_core::{
    AssetId, ClientOrderId, DexId, MarketKey, OrderSide, OrderState, PendingOrder, Price, Size,
};
use hip3_dashboard::{DashboardState, SignalSender, SignalSnapshot};
use hip3_detector::{CrossDurationTracker, DislocationDetector, DislocationSignal};
use hip3_executor::{
    ActionBudget, BatchConfig, BatchScheduler, DynWsSender, ExecutionEvent, ExecutorConfig,
    ExecutorHandle, ExecutorLoop, HardStopLatch, InflightTracker, KeyManager, KeySource,
    MarkPriceProvider, MarketStateCache, NonceManager, RealWsSender, RiskMonitor,
    RiskMonitorConfig as ExecutorRiskMonitorConfig, Signer, SystemClock, TradingReadyChecker,
};
use hip3_feed::{MarketEvent, MarketState, MessageParser, OracleMovementTracker, OracleTrackerHandle};
use hip3_persistence::{FollowupRecord, FollowupWriter, ParquetWriter, SignalRecord};
use hip3_position::{
    flatten_all_positions, new_exit_watcher, new_oracle_exit_watcher, spawn_position_tracker,
    ExitWatcherHandle, FlattenReason, MarkRegressionConfig, MarkRegressionMonitor,
    OracleExitWatcherHandle, Position, PositionTrackerHandle,
    TimeStopConfig as PositionTimeStopConfig, TimeStopMonitor,
};
use hip3_registry::{
    validate_market_keys, ClearinghouseStateResponse, MetaClient, PerpDexsResponse,
    PreflightChecker, RawPerpSpec, SpecCache,
};
use hip3_risk::{RiskError, RiskGate};
use hip3_telemetry::{DailyStatsReporter, Metrics};
use hip3_ws::{
    is_order_updates_channel, ConnectionConfig, ConnectionManager, FillPayload, OrderUpdatePayload,
    PostResponseBody, WsMessage,
};
use parking_lot::RwLock;
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Daily stats output interval (1 hour).
const DAILY_STATS_INTERVAL: Duration = Duration::from_secs(3600);

/// Followup snapshot offsets in milliseconds (T+1s, T+3s, T+5s).
const FOLLOWUP_OFFSETS_MS: [u64; 3] = [1000, 3000, 5000];

/// Get current time in milliseconds since UNIX epoch.
///
/// Returns 0 if system time is before UNIX epoch (should never happen).
fn current_time_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_millis() as u64
}

/// Context passed to followup capture tasks.
#[derive(Debug, Clone)]
struct FollowupContext {
    signal_id: String,
    market_key: MarketKey,
    side: OrderSide,
    signal_timestamp_ms: i64,
    t0_oracle_px: f64,
    t0_best_px: f64,
    t0_raw_edge_bps: f64,
}

/// Main application.
pub struct Application {
    config: AppConfig,
    market_state: Arc<MarketState>,
    spec_cache: Arc<SpecCache>,
    risk_gate: RiskGate,
    detector: DislocationDetector,
    writer: ParquetWriter,
    /// Followup writer for signal validation snapshots.
    followup_writer: Arc<tokio::sync::Mutex<FollowupWriter>>,
    // P0-31: Cross duration tracking
    cross_tracker: CrossDurationTracker,
    // P0-31: Daily stats reporter (initialized after preflight)
    daily_stats: Option<DailyStatsReporter>,
    // Last daily stats output time
    last_stats_output: Instant,
    // P0-15: Discovered xyz DEX ID
    xyz_dex_id: Option<DexId>,
    // BUG-003: Track gate block state per (market, gate) for state-change logging
    // Key: (MarketKey, gate_name), Value: was_blocked_last_tick
    gate_block_state: HashMap<(MarketKey, String), bool>,
    // Per-market threshold overrides in basis points.
    // Key: asset_idx (from MarketConfig), Value: threshold_bps
    market_threshold_map: HashMap<u32, Decimal>,
    // Phase B: Trading mode components (None in Observation mode)
    /// Executor loop for batch order processing.
    executor_loop: Option<Arc<ExecutorLoop>>,
    /// Position tracker handle for order/fill state management.
    position_tracker: Option<PositionTrackerHandle>,
    /// Position tracker task join handle for graceful shutdown.
    position_tracker_handle: Option<tokio::task::JoinHandle<()>>,
    /// Connection manager reference for READY check.
    connection_manager: Option<Arc<ConnectionManager>>,
    /// Risk event sender for RiskMonitor.
    risk_event_tx: Option<mpsc::Sender<ExecutionEvent>>,
    /// Recent signals buffer for dashboard display (last 50).
    recent_signals: Arc<RwLock<VecDeque<SignalRecord>>>,
    /// Signal sender for real-time dashboard updates.
    dashboard_signal_tx: Option<SignalSender>,
    /// Deduplication: track last persisted signal time per (market_key, side).
    /// Signals within DEDUP_INTERVAL_MS of the last one are skipped.
    last_persisted_signals: HashMap<(String, String), i64>,
    /// WS-driven exit watcher for immediate mark regression detection.
    exit_watcher: Option<ExitWatcherHandle>,
    /// Oracle movement tracker for consecutive direction detection.
    oracle_tracker: OracleTrackerHandle,
    /// Oracle-driven exit watcher based on consecutive price movements.
    oracle_exit_watcher: Option<OracleExitWatcherHandle>,
    /// Edge distribution tracker for threshold calibration.
    edge_tracker: EdgeTracker,
}

impl Application {
    /// Create a new application.
    ///
    /// Note: Markets may not be set yet. Call `run_preflight()` before `run()`.
    pub fn new(config: AppConfig) -> AppResult<Self> {
        // Initialize components
        let market_state = Arc::new(MarketState::new());
        let spec_cache = Arc::new(SpecCache::default());
        let risk_gate = RiskGate::new(config.risk.clone());
        let detector = DislocationDetector::new(config.detector.clone())?;
        let writer =
            ParquetWriter::new(&config.persistence.data_dir, config.persistence.buffer_size);
        let followup_writer = Arc::new(tokio::sync::Mutex::new(FollowupWriter::new(
            &config.persistence.data_dir,
            config.persistence.buffer_size,
        )));

        // P0-31: Cross tracker initialized, daily_stats deferred until markets known
        let cross_tracker = CrossDurationTracker::new();

        // Oracle movement tracker for consecutive direction detection
        let oracle_tracker_config = config.oracle_tracking.clone().unwrap_or_default();
        let oracle_tracker = OracleMovementTracker::new_shared(oracle_tracker_config);

        // Build per-market threshold map from config
        let market_threshold_map: HashMap<u32, Decimal> = config
            .markets
            .as_ref()
            .map(|markets| {
                markets
                    .iter()
                    .filter_map(|m| m.threshold_bps.map(|t| (m.asset_idx, Decimal::from(t))))
                    .collect()
            })
            .unwrap_or_default();

        if !market_threshold_map.is_empty() {
            info!(
                thresholds = ?market_threshold_map,
                "Per-market thresholds configured"
            );
        }

        Ok(Self {
            config,
            market_state,
            spec_cache,
            risk_gate,
            detector,
            writer,
            followup_writer,
            cross_tracker,
            daily_stats: None, // Initialized after preflight
            last_stats_output: Instant::now(),
            xyz_dex_id: None,
            gate_block_state: HashMap::new(),
            market_threshold_map,
            // Phase B: Initialized in Trading mode only
            executor_loop: None,
            position_tracker: None,
            position_tracker_handle: None,
            connection_manager: None,
            risk_event_tx: None,
            // Dashboard: Recent signals buffer
            recent_signals: Arc::new(RwLock::new(VecDeque::with_capacity(50))),
            // Dashboard: Signal sender (set when dashboard is enabled)
            dashboard_signal_tx: None,
            // Deduplication: Initialize empty map
            last_persisted_signals: HashMap::new(),
            // WS-driven exit watcher (initialized in Trading mode only)
            exit_watcher: None,
            // Oracle movement tracker (always active)
            oracle_tracker,
            // Oracle-driven exit watcher (initialized in Trading mode only)
            oracle_exit_watcher: None,
            // Edge tracker for threshold calibration (60s log interval)
            edge_tracker: EdgeTracker::new(60, Decimal::from(40)),
        })
    }

    /// Run preflight validation and market discovery (P0-15, P0-26, P0-27).
    ///
    /// This fetches perpDexs from the exchange, populates SpecCache,
    /// and discovers xyz markets.
    /// Must be called before `run()` if markets are not specified in config.
    pub async fn run_preflight(&mut self) -> AppResult<()> {
        // Always fetch perpDexs for SpecCache (even if markets are configured)
        info!(
            info_url = %self.config.info_url,
            "Fetching perpDexs for SpecCache initialization"
        );

        let client = MetaClient::new(&self.config.info_url)
            .map_err(|e| AppError::Preflight(format!("Failed to create HTTP client: {e}")))?;

        const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(30);
        let perp_dexs = tokio::time::timeout(PREFLIGHT_TIMEOUT, client.fetch_perp_dexs())
            .await
            .map_err(|_| AppError::Preflight("Preflight HTTP request timed out (30s)".to_string()))?
            .map_err(|e| AppError::Preflight(format!("Failed to fetch perpDexs: {e}")))?;

        // Always populate SpecCache (sets xyz_dex_id too)
        self.populate_spec_cache(&perp_dexs)?;

        // Safety: In Trading mode, require explicit markets allowlist in config.
        // Auto-discovery would subscribe/trade all xyz markets, which is too risky
        // for mainnet micro-tests and can cause accidental multi-market exposure.
        if self.config.mode == OperatingMode::Trading && !self.config.has_markets() {
            return Err(AppError::Preflight(
                "Trading mode requires explicit [[markets]] in config (auto-discovery disabled for safety)"
                    .to_string(),
            ));
        }

        // If markets already configured, validate against perpDexs then skip discovery
        if self.config.has_markets() {
            // Validate configured markets exist in perpDexs
            self.validate_configured_markets(&perp_dexs)?;

            info!("Markets already configured and validated, skipping market discovery");
            self.initialize_daily_stats();
            return Ok(());
        }

        info!(
            xyz_pattern = %self.config.xyz_pattern,
            "Running market discovery (P0-15, P0-26, P0-27)"
        );

        // Validate and discover markets
        let checker = PreflightChecker::new(&self.config.xyz_pattern);
        let result = checker
            .validate(&perp_dexs)
            .map_err(|e| AppError::Preflight(format!("Preflight validation failed: {e}")))?;

        // Log warnings if any
        for warning in &result.warnings {
            warn!(warning = %warning, "Preflight warning");
        }

        // Convert discovered markets to config format
        // WebSocket subscriptions require full coin name with dex prefix (e.g., "xyz:AAPL")
        let dex_prefix = &self.config.xyz_pattern;
        let markets: Vec<MarketConfig> = result
            .markets
            .iter()
            .map(|m| MarketConfig {
                asset_idx: m.key.asset.index(),
                coin: format!("{}:{}", dex_prefix, m.name),
                threshold_bps: None, // Discovered markets use global threshold
            })
            .collect();

        info!(
            xyz_dex_id = result.xyz_dex_id.index(),
            market_count = markets.len(),
            markets = ?markets.iter().map(|m| &m.coin).collect::<Vec<_>>(),
            "Discovered xyz markets"
        );

        // Update config with discovered markets
        self.config.set_discovered_markets(markets);

        // Initialize daily stats now that markets are known
        self.initialize_daily_stats();

        Ok(())
    }

    /// Populate SpecCache from perpDexs response.
    ///
    /// Must be called during initialization to ensure ExecutorLoop has access
    /// to market specifications for order formatting.
    ///
    /// # Important
    /// This method also sets `xyz_dex_id` on the App, ensuring that the correct
    /// DEX index is used throughout the application.
    fn populate_spec_cache(&mut self, perp_dexs: &PerpDexsResponse) -> AppResult<()> {
        // Use PreflightChecker to find xyz DEX (reuse same logic as validate())
        // This ensures consistent DEX detection: contains + case-insensitive
        let checker = PreflightChecker::new(&self.config.xyz_pattern);
        let (dex_idx, xyz_dex) = checker
            .find_xyz_dex(&perp_dexs.perp_dexs)
            .map_err(|e| AppError::Preflight(format!("Failed to find xyz DEX: {e}")))?;

        let dex_id = DexId::new(dex_idx);

        // Set xyz_dex_id on App to ensure get_dex_id() returns correct value
        self.xyz_dex_id = Some(dex_id);

        info!(
            dex_name = %xyz_dex.name,
            dex_idx = dex_idx,
            market_count = xyz_dex.markets.len(),
            "Found xyz DEX for SpecCache initialization"
        );

        for (fallback_idx, market) in xyz_dex.markets.iter().enumerate() {
            let raw = RawPerpSpec {
                name: market.name.clone(),
                sz_decimals: market.sz_decimals,
                max_leverage: market.max_leverage,
                only_isolated: market.only_isolated,
                tick_size: market.tick_size, // Option<Decimal>
            };
            let spec = self.spec_cache.parse_spec(&raw);

            // Use asset_index from meta(dex=xyz) if available
            // IMPORTANT: perpDexs order differs from meta(dex=xyz) order
            // Asset IDs must use indices from meta(dex=xyz) for correct order execution
            let asset_idx = market.asset_index.unwrap_or_else(|| {
                warn!(
                    market = %market.name,
                    fallback_idx = fallback_idx,
                    "Using fallback enumerate index for SpecCache (may cause incorrect asset IDs)"
                );
                fallback_idx as u32
            });

            // Calculate full asset ID for Hyperliquid order API:
            // Formula: 100000 + perp_dex_id * 10000 + asset_index
            let full_asset_id = 100000 + (dex_idx as u32) * 10000 + asset_idx;
            let key = MarketKey::new(dex_id, AssetId::new(full_asset_id));

            self.spec_cache
                .update(key, spec)
                .map_err(|e| AppError::Preflight(format!("Failed to update SpecCache: {e}")))?;

            debug!(
                market = %key,
                name = %market.name,
                sz_decimals = market.sz_decimals,
                tick_size = ?market.tick_size,
                asset_index = asset_idx,
                "Populated SpecCache"
            );
        }

        info!(
            market_count = xyz_dex.markets.len(),
            dex_id = %dex_id,
            "SpecCache populated from perpDexs with correct asset indices"
        );

        Ok(())
    }

    /// Validate that configured markets exist in perpDexs.
    ///
    /// This catches configuration errors like:
    /// - Invalid asset_idx (market doesn't exist)
    /// - Coin name mismatch
    fn validate_configured_markets(&self, perp_dexs: &PerpDexsResponse) -> AppResult<()> {
        let checker = PreflightChecker::new(&self.config.xyz_pattern);
        let result = checker
            .validate(perp_dexs)
            .map_err(|e| AppError::Preflight(format!("Preflight validation failed: {e}")))?;

        let dex_id = self.get_dex_id();

        // Build configured market keys from asset_idx
        let configured_keys: Vec<MarketKey> = self
            .config
            .get_markets()
            .iter()
            .map(|m| MarketKey::new(dex_id, AssetId::new(m.asset_idx)))
            .collect();

        // Validate all configured keys exist in discovered markets
        validate_market_keys(&configured_keys, &result.markets).map_err(|e| {
            AppError::Preflight(format!("Configured market validation failed: {e}"))
        })?;

        // Optional: Warn if coin names don't match
        for configured in self.config.get_markets() {
            let key = MarketKey::new(dex_id, AssetId::new(configured.asset_idx));
            if let Some(discovered) = result.markets.iter().find(|m| m.key == key) {
                if !configured.coin.ends_with(&discovered.name) {
                    warn!(
                        configured_coin = %configured.coin,
                        discovered_name = %discovered.name,
                        key = %key,
                        "Configured coin name doesn't match perpDexs - verify configuration"
                    );
                }
            }
        }

        info!(
            market_count = configured_keys.len(),
            "Configured markets validated against perpDexs"
        );

        Ok(())
    }

    /// Initialize daily stats reporter with market keys.
    fn initialize_daily_stats(&mut self) {
        let dex_id = self.xyz_dex_id.unwrap_or(DexId::XYZ);
        let market_keys: Vec<String> = self
            .config
            .get_markets()
            .iter()
            .map(|m| MarketKey::new(dex_id, AssetId::new(m.asset_idx)).to_string())
            .collect();
        self.daily_stats = Some(DailyStatsReporter::new(market_keys));
    }

    /// Get the xyz DEX ID (discovered during preflight).
    fn get_dex_id(&self) -> DexId {
        self.xyz_dex_id.unwrap_or(DexId::XYZ)
    }

    /// Extract balance from clearinghouse state response.
    ///
    /// Tries margin_summary first, then cross_margin_summary.
    /// Returns Decimal::ZERO if neither is available.
    fn extract_balance_from_state(state: &ClearinghouseStateResponse) -> Decimal {
        // Try margin_summary first
        if let Some(ref margin_summary) = state.margin_summary {
            if let Ok(balance) = margin_summary.account_value_decimal() {
                return balance;
            }
        }
        // Fallback to cross_margin_summary
        if let Some(ref cross_margin) = state.cross_margin_summary {
            if let Ok(balance) = cross_margin.account_value_decimal() {
                return balance;
            }
        }
        Decimal::ZERO
    }

    /// Sync positions from Hyperliquid clearinghouseState API.
    ///
    /// Called at startup to initialize PositionTracker with current positions.
    /// Also updates account balance for dynamic position sizing.
    /// This prevents stale position state after bot restart.
    ///
    /// # Balance Query Strategy
    /// When trading on xyz DEX, funds automatically transfer between L1 and xyz.
    /// We query both and sum the balances to get the total available:
    /// 1. L1 Perp balance (without dex param)
    /// 2. xyz balance + positions (with dex param)
    async fn sync_positions_from_api(
        &self,
        position_tracker: &PositionTrackerHandle,
        user_address: &str,
    ) -> AppResult<()> {
        info!(user_address = %user_address, "Syncing positions from Hyperliquid API");

        let client = MetaClient::new(&self.config.info_url)
            .map_err(|e| AppError::Executor(format!("Failed to create HTTP client: {e}")))?;

        // Step 1: Fetch L1 Perp balance (without dex param)
        let l1_state = client
            .fetch_clearinghouse_state(user_address, None)
            .await
            .map_err(|e| {
                AppError::Executor(format!("Failed to fetch L1 clearinghouseState: {e}"))
            })?;

        // Extract L1 balance
        let l1_balance = Self::extract_balance_from_state(&l1_state);

        // Step 2: Fetch xyz state (with dex param) - for both balance AND positions
        // BUG-005: Pass dex name to fetch perpDex positions.
        // Without this, only L1 perp positions are returned (not xyz perpDex positions).
        let dex_name = Some(self.config.xyz_pattern.as_str());
        let state = client
            .fetch_clearinghouse_state(user_address, dex_name)
            .await
            .map_err(|e| {
                AppError::Executor(format!("Failed to fetch xyz clearinghouseState: {e}"))
            })?;

        // Extract xyz balance
        let xyz_balance = Self::extract_balance_from_state(&state);

        // Total balance = L1 + xyz (funds automatically transfer between them)
        let total_balance = l1_balance + xyz_balance;
        info!(
            l1_balance = %l1_balance,
            xyz_balance = %xyz_balance,
            total_balance = %total_balance,
            "Updating account balance from L1 + xyz"
        );
        position_tracker.update_balance(total_balance);

        let now_ms = current_time_ms();
        let dex_id = self.get_dex_id();
        let mut positions_to_sync = Vec::new();

        for entry in &state.asset_positions {
            let pos_data = &entry.position;

            // Skip empty positions
            if pos_data.is_empty() {
                continue;
            }

            // Parse coin name (e.g., "xyz:SILVER" -> MarketKey)
            let coin = &pos_data.coin;

            // Find matching market in spec_cache
            let market_key = self.coin_to_market_key(coin, dex_id);
            let market_key = match market_key {
                Some(key) => key,
                None => {
                    warn!(coin = %coin, "Could not find market key for position, skipping");
                    continue;
                }
            };

            // Parse size and entry price
            let size = match pos_data.size_decimal() {
                Ok(sz) => sz,
                Err(e) => {
                    warn!(coin = %coin, ?e, "Failed to parse position size");
                    continue;
                }
            };

            let entry_price = match pos_data.entry_price_decimal() {
                Ok(px) => px,
                Err(e) => {
                    warn!(coin = %coin, ?e, "Failed to parse entry price");
                    continue;
                }
            };

            // Determine side from signed size
            let (side, abs_size) = if size > Decimal::ZERO {
                (OrderSide::Buy, size)
            } else {
                (OrderSide::Sell, size.abs())
            };

            let position = Position::new(
                market_key,
                side,
                Size::new(abs_size),
                Price::new(entry_price),
                now_ms, // Use current time as entry time (actual time not available from API)
            );

            info!(
                market = %market_key,
                side = ?side,
                size = %abs_size,
                entry_price = %entry_price,
                "Found existing position from API"
            );

            positions_to_sync.push(position);
        }

        info!(
            position_count = positions_to_sync.len(),
            "Syncing {} positions to PositionTracker",
            positions_to_sync.len()
        );

        position_tracker.sync_positions(positions_to_sync).await;

        Ok(())
    }

    /// Convert coin name (e.g., "xyz:SILVER") to MarketKey.
    ///
    /// Searches spec_cache for matching market.
    fn coin_to_market_key(&self, coin: &str, dex_id: DexId) -> Option<MarketKey> {
        // Extract asset name from coin (e.g., "xyz:SILVER" -> "SILVER")
        let asset_name = coin.split(':').next_back().unwrap_or(coin);

        // Search configured markets for matching name
        for market_config in self.config.get_markets() {
            // Market config coin is like "xyz:SILVER"
            if market_config.coin == coin
                || market_config.coin.ends_with(&format!(":{}", asset_name))
            {
                return Some(MarketKey::new(
                    dex_id,
                    AssetId::new(market_config.asset_idx),
                ));
            }
        }

        None
    }

    /// Run the application.
    ///
    /// # Panics
    /// Panics if `run_preflight()` was not called and markets are not configured.
    pub async fn run(mut self) -> AppResult<()> {
        // Ensure preflight has been run
        if !self.config.has_markets() {
            return Err(AppError::Preflight(
                "Markets not configured. Call run_preflight() first.".to_string(),
            ));
        }

        info!(mode = ?self.config.mode, "Starting application");

        // Trading-mode config validation (fail fast before starting background tasks).
        let (
            trading_expected_signer_address,
            trading_user_address,
            trading_is_mainnet,
            trading_vault_address,
            trading_vault_address_str,
        ) = if self.config.mode == OperatingMode::Trading {
            let user_address = self.config.user_address.as_deref().ok_or_else(|| {
                AppError::Config("Trading mode requires `user_address`".to_string())
            })?;

            // Validate formatting early (used for subscriptions/account scoping).
            Address::from_str(user_address).map_err(|e| {
                AppError::Config(format!("Invalid `user_address` (expected 0x...): {e}"))
            })?;

            let expected_signer_address = match self.config.signer_address.as_deref() {
                Some(addr) => Some(Address::from_str(addr).map_err(|e| {
                    AppError::Config(format!("Invalid `signer_address` (expected 0x...): {e}"))
                })?),
                None => None,
            };

            if self.config.private_key.is_none() {
                return Err(AppError::Config(
                    "Trading mode requires `private_key` (enable HIP3_TRADING_KEY env var)"
                        .to_string(),
                ));
            }

            let is_mainnet = self.config.is_mainnet.ok_or_else(|| {
                AppError::Config("Trading mode requires `is_mainnet` = true|false".to_string())
            })?;

            let vault_address_str = self.config.vault_address.clone();
            let vault_address = match vault_address_str.as_deref() {
                Some(addr) => Some(Address::from_str(addr).map_err(|e| {
                    AppError::Config(format!("Invalid `vault_address` (expected 0x...): {e}"))
                })?),
                None => None,
            };

            // Heuristic safety warnings for common misconfiguration.
            if self.config.ws_url.contains("testnet") && is_mainnet {
                warn!(
                    ws_url = %self.config.ws_url,
                    "ws_url looks testnet but is_mainnet=true"
                );
            }
            if !self.config.ws_url.contains("testnet") && !is_mainnet {
                warn!(
                    ws_url = %self.config.ws_url,
                    "ws_url looks mainnet but is_mainnet=false"
                );
            }

            (
                expected_signer_address,
                Some(user_address.to_string()),
                is_mainnet,
                vault_address,
                vault_address_str,
            )
        } else {
            (None, self.config.user_address.clone(), false, None, None)
        };

        // Create message channel
        let (message_tx, mut message_rx) = mpsc::channel::<WsMessage>(1000);

        // Create WebSocket connection manager
        let mut ws_config: ConnectionConfig = self.config.websocket.clone().into();
        ws_config.url = self.config.ws_url.clone();
        ws_config.subscriptions = self.config.subscription_targets();
        ws_config.user_address = trading_user_address.clone();

        info!(
            subscriptions = ?ws_config.subscriptions.iter().map(|s| &s.coin).collect::<Vec<_>>(),
            user_address = ?ws_config.user_address,
            "Configured WebSocket subscriptions"
        );

        let connection_manager = Arc::new(ConnectionManager::new(ws_config, message_tx));
        self.connection_manager = Some(connection_manager.clone());
        let connection_manager_clone = connection_manager.clone();

        // Spawn WebSocket connection task
        let mut ws_handle = tokio::spawn(async move {
            if let Err(e) = connection_manager_clone.connect().await {
                error!(?e, "WebSocket connection failed");
            }
        });

        // Trading mode initialization
        let _tick_handle: Option<tokio::task::JoinHandle<()>> = if self.config.mode
            == OperatingMode::Trading
        {
            info!("Initializing Trading mode components");

            // 1. Position Tracker (actor)
            let (position_tracker, pos_join_handle) = spawn_position_tracker(100);
            self.position_tracker = Some(position_tracker.clone());
            self.position_tracker_handle = Some(pos_join_handle);

            // 1.5. Sync positions from Hyperliquid API (P0-startup-sync)
            // Prevents stale position state after bot restart
            // Note: trading_user_address is always Some(...) in Trading mode
            if let Some(ref user_addr) = trading_user_address {
                if let Err(e) = self
                    .sync_positions_from_api(&position_tracker, user_addr)
                    .await
                {
                    warn!(
                        ?e,
                        "Failed to sync positions from API, starting with empty state"
                    );
                }
            }

            // 2. HardStopLatch and InflightTracker (shared dependencies)
            let hard_stop_latch = Arc::new(HardStopLatch::new());
            let inflight_tracker = Arc::new(InflightTracker::new(10)); // max 10 inflight

            // 3. BatchScheduler (with configurable interval for latency optimization)
            let batch_config = BatchConfig {
                interval_ms: self.config.executor.batch_interval_ms,
                ..BatchConfig::default()
            };
            info!(
                interval_ms = batch_config.interval_ms,
                "Batch scheduler initialized"
            );
            let batch_scheduler = Arc::new(BatchScheduler::new(
                batch_config,
                inflight_tracker.clone(),
                hard_stop_latch.clone(),
            ));

            // 4. TradingReadyChecker
            let (ready_checker, _ready_rx) = TradingReadyChecker::new();
            let ready_checker = Arc::new(ready_checker);

            // 5. ActionBudget
            let action_budget = Arc::new(ActionBudget::default());

            // 6. Executor core
            // Position limits from [position] config section.
            // Note: detector.max_notional controls per-order sizing (separate from position limits).
            let executor_config = ExecutorConfig {
                max_notional_per_market: self.config.position.max_notional_per_market,
                max_notional_total: self.config.position.max_total_notional,
                max_concurrent_positions: self.config.position.max_concurrent_positions,
                dynamic_sizing_enabled: self.config.position.dynamic_sizing.enabled,
                risk_per_market_pct: Decimal::try_from(
                    self.config.position.dynamic_sizing.risk_per_market_pct,
                )
                .unwrap_or(Decimal::new(10, 2)), // Default 0.10 (10%)
            };
            let executor = Arc::new(hip3_executor::Executor::new(
                position_tracker.clone(),
                batch_scheduler.clone(),
                ready_checker.clone(),
                hard_stop_latch.clone(),
                action_budget.clone(),
                executor_config,
                Arc::new(MarketStateCache::default()),
            ));

            // 7. KeyManager (uses KeySource struct variant)
            let key_source = self.config.private_key.as_ref().map(|_| {
                // Use env var for security (config just indicates "use env")
                KeySource::EnvVar {
                    var_name: "HIP3_TRADING_KEY".to_string(),
                }
            });
            let key_manager = Arc::new(
                KeyManager::load(key_source, trading_expected_signer_address)
                    .map_err(|e| AppError::Executor(format!("KeyManager error: {e}")))?,
            );

            // 8. Signer
            let signer = Arc::new(
                Signer::new(key_manager.clone(), trading_is_mainnet)
                    .map_err(|e| AppError::Executor(format!("Signer error: {e}")))?,
            );
            info!(
                trading_address = ?signer.trading_address(),
                user_address = ?trading_user_address,
                expected_signer_address = ?trading_expected_signer_address,
                vault_address = ?trading_vault_address_str,
                is_mainnet = trading_is_mainnet,
                "Signer initialized"
            );

            // 9. NonceManager
            let nonce_manager = Arc::new(NonceManager::new(SystemClock));

            // 10. ExecutorLoop
            let mut executor_loop = ExecutorLoop::new(
                executor.clone(),
                nonce_manager,
                signer,
                5000,
                self.spec_cache.clone(),
            );
            executor_loop.set_vault_address(trading_vault_address);

            // 11. Wire WsSender
            let ws_write_handle = connection_manager.write_handle();
            let real_ws_sender: DynWsSender = Arc::new(RealWsSender::new(
                ws_write_handle,
                trading_vault_address_str.clone(),
            ));
            executor_loop.set_ws_sender(real_ws_sender);

            let executor_loop = Arc::new(executor_loop);
            self.executor_loop = Some(executor_loop.clone());

            // 12. Spawn ExecutorLoop tick task (100ms interval)
            let tick_executor_loop = executor_loop.clone();
            let tick_handle = tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(100));
                loop {
                    interval.tick().await;
                    tick_executor_loop.tick(current_time_ms()).await;
                }
            });

            // 13. TimeStopMonitor for automatic position exit
            {
                // Create flatten channel (TimeStopMonitor -> BatchScheduler)
                let (flatten_tx, mut flatten_rx) = mpsc::channel::<hip3_core::PendingOrder>(100);

                // Spawn flatten receiver task that forwards to BatchScheduler
                let flatten_batch_scheduler = batch_scheduler.clone();
                tokio::spawn(async move {
                    while let Some(order) = flatten_rx.recv().await {
                        debug!(
                            cloid = %order.cloid,
                            market = %order.market,
                            side = ?order.side,
                            "Flatten order received, enqueueing to BatchScheduler"
                        );
                        flatten_batch_scheduler.enqueue_reduce_only(order);
                    }
                    info!("Flatten receiver task stopped (channel closed)");
                });

                // Create MarkPriceProvider from executor's market_state_cache
                let price_provider = Arc::new(MarkPriceProvider::new(
                    executor_loop.executor().market_state_cache().clone(),
                ));

                // Create TimeStopConfig from app config
                let time_stop_config = PositionTimeStopConfig::new(
                    self.config.time_stop.threshold_ms,
                    self.config.time_stop.reduce_only_timeout_ms,
                );

                // Clone flatten_tx for MarkRegressionMonitor, ExitWatcher, and OracleExitWatcher
                let mark_regression_flatten_tx = flatten_tx.clone();
                let exit_watcher_flatten_tx = flatten_tx.clone();
                let oracle_exit_flatten_tx = flatten_tx.clone();

                // Create TimeStopMonitor
                let time_stop_monitor = TimeStopMonitor::new(
                    time_stop_config,
                    position_tracker.clone(),
                    flatten_tx,
                    price_provider,
                    self.config.time_stop.slippage_bps,
                    self.config.time_stop.check_interval_ms,
                );

                // Spawn TimeStopMonitor task
                tokio::spawn(async move {
                    time_stop_monitor.run().await;
                });

                info!(
                    threshold_ms = self.config.time_stop.threshold_ms,
                    slippage_bps = self.config.time_stop.slippage_bps,
                    "TimeStopMonitor started"
                );

                // 13b. MarkRegressionMonitor for profit-taking exit (polling backup)
                if self.config.mark_regression.enabled {
                    let mark_regression_config = MarkRegressionConfig {
                        enabled: true,
                        exit_threshold_bps: rust_decimal::Decimal::from(
                            self.config.mark_regression.exit_threshold_bps,
                        ),
                        check_interval_ms: self.config.mark_regression.check_interval_ms,
                        min_holding_time_ms: self.config.mark_regression.min_holding_time_ms,
                        slippage_bps: self.config.mark_regression.slippage_bps,
                    };

                    let mark_regression_monitor = MarkRegressionMonitor::new(
                        mark_regression_config.clone(),
                        position_tracker.clone(),
                        mark_regression_flatten_tx,
                        self.market_state.clone(),
                    );

                    tokio::spawn(async move {
                        mark_regression_monitor.run().await;
                    });

                    info!(
                        exit_threshold_bps = self.config.mark_regression.exit_threshold_bps,
                        check_interval_ms = self.config.mark_regression.check_interval_ms,
                        min_holding_time_ms = self.config.mark_regression.min_holding_time_ms,
                        "MarkRegressionMonitor spawned (polling backup)"
                    );

                    // 13c. ExitWatcher for WS-driven immediate exit detection
                    let exit_watcher = new_exit_watcher(
                        mark_regression_config,
                        position_tracker.clone(),
                        exit_watcher_flatten_tx,
                    );
                    self.exit_watcher = Some(exit_watcher);

                    info!("ExitWatcher started (WS-driven, < 1ms latency)");

                    // 13d. OracleExitWatcher for oracle-driven exit (reversal/catchup)
                    let oracle_exit_config = self.config.oracle_exit.clone().unwrap_or_default();
                    if oracle_exit_config.enabled {
                        let oracle_exit_watcher = new_oracle_exit_watcher(
                            oracle_exit_config.clone(),
                            position_tracker.clone(),
                            self.oracle_tracker.clone(),
                            oracle_exit_flatten_tx,
                        );
                        self.oracle_exit_watcher = Some(oracle_exit_watcher);

                        info!(
                            exit_against_moves = oracle_exit_config.exit_against_moves,
                            exit_with_moves = oracle_exit_config.exit_with_moves,
                            "OracleExitWatcher started (oracle-driven)"
                        );
                    }
                }
            }

            // 14. RiskMonitor for risk condition monitoring
            {
                // Create event channel (Application -> RiskMonitor)
                let (event_tx, event_rx) = mpsc::channel::<ExecutionEvent>(100);
                self.risk_event_tx = Some(event_tx);

                // Create executor handle channel (RiskMonitor -> Executor on HardStop)
                let (executor_handle_tx, mut executor_handle_rx) = mpsc::channel::<String>(10);
                let executor_handle = ExecutorHandle::new(executor_handle_tx);

                // Spawn executor handle receiver task
                let hard_stop_for_handle = hard_stop_latch.clone();
                tokio::spawn(async move {
                    while let Some(reason) = executor_handle_rx.recv().await {
                        warn!(reason = %reason, "Received HardStop command from RiskMonitor");
                        // HardStop is already triggered by RiskMonitor, but we log for visibility
                        if !hard_stop_for_handle.is_triggered() {
                            hard_stop_for_handle.trigger(&reason);
                        }
                    }
                });

                // Map app config to executor RiskMonitorConfig
                let risk_config = ExecutorRiskMonitorConfig {
                    max_cumulative_loss: Decimal::from_f64_retain(
                        self.config.risk_monitor.max_loss_usd,
                    )
                    .unwrap_or_default(),
                    max_consecutive_losses: self.config.risk_monitor.max_consecutive_failures,
                    max_flatten_failed: self.config.risk_monitor.max_flatten_failed,
                    max_rejected_per_hour: 10,         // Default
                    max_slippage_bps: 50.0,            // Default
                    slippage_consecutive_threshold: 3, // Default
                };

                // Create RiskMonitor
                let risk_monitor = RiskMonitor::new(
                    event_rx,
                    hard_stop_latch.clone(),
                    executor_handle,
                    risk_config,
                );

                // Spawn RiskMonitor task
                tokio::spawn(async move {
                    risk_monitor.run().await;
                });

                info!(
                    max_loss_usd = self.config.risk_monitor.max_loss_usd,
                    max_consecutive_failures = self.config.risk_monitor.max_consecutive_failures,
                    max_flatten_failed = self.config.risk_monitor.max_flatten_failed,
                    "RiskMonitor started"
                );
            }

            // 15. HardStop Flatten Watcher
            {
                let hard_stop_watcher_latch = hard_stop_latch.clone();
                let hard_stop_watcher_tracker = position_tracker.clone();
                let hard_stop_watcher_scheduler = batch_scheduler.clone();
                let hard_stop_watcher_cache = executor_loop.executor().market_state_cache().clone();
                let hard_stop_slippage_bps = self.config.time_stop.slippage_bps;

                tokio::spawn(async move {
                    const MAX_RETRIES: u32 = 3;
                    const RETRY_INTERVAL_MS: u64 = 1000;
                    const CHECK_INTERVAL_MS: u64 = 100;

                    let mut triggered = false;
                    let mut retry_count = 0u32;

                    loop {
                        tokio::time::sleep(Duration::from_millis(CHECK_INTERVAL_MS)).await;

                        if hard_stop_watcher_latch.is_triggered() && !triggered {
                            triggered = true;
                            warn!("🛑 HardStop detected, initiating flatten sequence");
                        }

                        if triggered {
                            // Get all positions
                            let positions = hard_stop_watcher_tracker.positions_snapshot();

                            if positions.is_empty() {
                                info!("All positions flattened successfully (or none existed)");
                                break;
                            }

                            // Create flatten requests
                            let now_ms = current_time_ms();
                            let flatten_requests =
                                flatten_all_positions(&positions, FlattenReason::HardStop, now_ms);

                            if flatten_requests.is_empty() {
                                info!("No non-zero positions to flatten");
                                break;
                            }

                            // Convert to PendingOrders and enqueue
                            for request in &flatten_requests {
                                // Get mark price for limit price calculation
                                let mark_price =
                                    match hard_stop_watcher_cache.get_mark_px(&request.market) {
                                        Some(p) => p,
                                        None => {
                                            error!(
                                                market = %request.market,
                                                "Cannot flatten: no mark price available"
                                            );
                                            continue;
                                        }
                                    };

                                // Calculate limit price with slippage
                                let slippage_multiplier = if request.side == OrderSide::Buy {
                                    // Buy (close short): mark * (1 + slippage)
                                    Decimal::new(10000 + hard_stop_slippage_bps as i64, 4)
                                } else {
                                    // Sell (close long): mark * (1 - slippage)
                                    Decimal::new(10000 - hard_stop_slippage_bps as i64, 4)
                                };
                                let limit_price =
                                    Price::new(mark_price.inner() * slippage_multiplier);

                                // Create reduce-only PendingOrder
                                let pending_order = PendingOrder {
                                    cloid: ClientOrderId::new(),
                                    market: request.market,
                                    side: request.side,
                                    price: limit_price,
                                    size: request.size,
                                    reduce_only: true,
                                    created_at: now_ms,
                                };

                                debug!(
                                    market = %request.market,
                                    side = ?request.side,
                                    size = %request.size,
                                    limit_price = %limit_price,
                                    "Enqueuing HardStop flatten order"
                                );

                                hard_stop_watcher_scheduler.enqueue_reduce_only(pending_order);
                            }

                            info!(
                                count = flatten_requests.len(),
                                retry = retry_count,
                                "Enqueued HardStop flatten orders"
                            );

                            retry_count += 1;
                            if retry_count >= MAX_RETRIES {
                                let remaining =
                                    hard_stop_watcher_tracker.positions_snapshot().len();
                                if remaining > 0 {
                                    error!(
                                            remaining = remaining,
                                            max_retries = MAX_RETRIES,
                                            "⚠️ CRITICAL: Positions remain after max retries. Manual intervention required."
                                        );
                                }
                                break;
                            }

                            // Wait before retry
                            tokio::time::sleep(Duration::from_millis(RETRY_INTERVAL_MS)).await;
                        }
                    }

                    info!("HardStop flatten watcher stopped");
                });

                info!("HardStop flatten watcher started");
            }

            info!("Trading mode initialized with ExecutorLoop, PositionTracker, TimeStopMonitor, MarkRegressionMonitor, RiskMonitor, and HardStop Flatten");

            // 16. Dashboard server (if enabled)
            if self.config.dashboard.enabled {
                let dashboard_state = DashboardState::new(
                    self.market_state.clone(),
                    position_tracker.clone(),
                    hard_stop_latch.clone(),
                    self.recent_signals.clone(),
                );
                // Store signal sender for real-time signal push
                self.dashboard_signal_tx = Some(dashboard_state.signal_sender());
                let dashboard_config = self.config.dashboard.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        hip3_dashboard::run_server(dashboard_state, dashboard_config).await
                    {
                        error!(error = %e, "Dashboard server failed");
                    }
                });
                info!(
                    port = self.config.dashboard.port,
                    "Dashboard server started"
                );
            }

            Some(tick_handle)
        } else {
            // Observation mode: Dashboard with limited components (if enabled)
            if self.config.dashboard.enabled {
                info!("Starting dashboard in Observation mode - limited functionality (market data only)");
                let dashboard_state = DashboardState::new_observation_mode(
                    self.market_state.clone(),
                    self.recent_signals.clone(),
                );
                // Store signal sender for real-time signal push
                self.dashboard_signal_tx = Some(dashboard_state.signal_sender());
                let dashboard_config = self.config.dashboard.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        hip3_dashboard::run_server(dashboard_state, dashboard_config).await
                    {
                        error!(error = %e, "Dashboard server failed");
                    }
                });
                info!(
                    port = self.config.dashboard.port,
                    "Dashboard server started (Observation mode - market data only)"
                );
            }
            None
        };

        // Create message parser with coin mappings and correct DEX ID
        let mut parser = MessageParser::new();
        // Set the discovered DEX ID so market keys match between parser and check_dislocations
        parser.set_dex_id(self.get_dex_id());
        for market in self.config.get_markets() {
            parser.add_coin_mapping(market.coin.clone(), market.asset_idx);
        }
        info!(
            dex_id = %self.get_dex_id(),
            coin_mappings = ?self.config.get_markets().iter().map(|m| (&m.coin, m.asset_idx)).collect::<Vec<_>>(),
            "Parser configured with coin mappings and DEX ID"
        );

        // Main event loop
        info!("Entering main event loop");
        let mut signal_count = 0u64;
        let mut stats_interval = tokio::time::interval(DAILY_STATS_INTERVAL);

        // P1-3: Phase B TODO - Add periodic spec refresh task
        // This would detect parameter changes (tick_size, lot_size, etc.) from exchange
        // let spec_refresh_interval = tokio::time::interval(Duration::from_secs(300));

        // Periodic position resync (P1 safety net - Trading mode only)
        let resync_interval_secs = self.config.position.position_resync_interval_secs;
        let mut resync_interval = if resync_interval_secs > 0 {
            Some(tokio::time::interval(Duration::from_secs(
                resync_interval_secs,
            )))
        } else {
            None
        };

        loop {
            tokio::select! {
                // Handle incoming WebSocket messages
                Some(msg) = message_rx.recv() => {
                    if let Err(e) = self.handle_message(&parser, msg).await {
                        warn!(?e, "Message handling error");
                    }

                    // Check for dislocations on each update
                    if let Some(signals) = self.check_dislocations().await {
                        for signal in signals {
                            signal_count += 1;
                            info!(
                                signal_id = %signal.signal_id,
                                market = %signal.market_key,
                                side = %signal.side,
                                edge_bps = %signal.raw_edge_bps,
                                "Signal detected (#{signal_count})"
                            );

                            // Record metrics
                            Metrics::signal_triggered(
                                &signal.market_key.to_string(),
                                &signal.side.to_string(),
                                &format!("{:?}", signal.strength),
                            );

                            // Persist signal (with deduplication)
                            let persisted = self.persist_signal(&signal)?;

                            // Schedule followup snapshots only if signal was persisted
                            // (not deduplicated)
                            if persisted {
                                self.schedule_followups(&signal);
                            }

                            // Phase B: Execute signal
                            if self.config.mode == OperatingMode::Trading {
                                // Gate: Check WS READY-TRADING before processing signal
                                if let Some(ref cm) = self.connection_manager {
                                    if !cm.is_ready() {
                                        debug!(
                                            market = %signal.market_key,
                                            "Signal dropped: not ready for trading"
                                        );
                                        continue;
                                    }
                                }

                                // Gate: Check if size rounds to zero after lot_size truncation
                                // This prevents "Order has zero size" errors from the exchange
                                // when suggested_size is smaller than lot_size (e.g., 0.00005 with lot_size=0.0001)
                                let lot_size = self
                                    .spec_cache
                                    .get(&signal.market_key)
                                    .map(|spec| spec.lot_size)
                                    .unwrap_or(Size::new(Decimal::new(1, 4))); // Default 0.0001
                                let rounded_size = signal.suggested_size.round_to_lot(lot_size);
                                if rounded_size.is_zero() {
                                    debug!(
                                        market = %signal.market_key,
                                        suggested_size = %signal.suggested_size,
                                        lot_size = %lot_size,
                                        "Signal dropped: size rounds to zero after lot_size truncation"
                                    );
                                    continue;
                                }

                                // Execute signal via Executor
                                if let Some(ref executor_loop) = self.executor_loop {
                                    let result = executor_loop.executor().on_signal(
                                        &signal.market_key,
                                        signal.side,
                                        signal.best_px,
                                        rounded_size, // Use rounded size instead of suggested_size
                                        current_time_ms(),
                                    );
                                    debug!(
                                        signal_id = %signal.signal_id,
                                        result = ?result,
                                        "Signal execution result"
                                    );
                                }
                            }
                        }
                    }
                }

                // P0-31: Periodic daily stats output
                _ = stats_interval.tick() => {
                    info!("Outputting periodic statistics summary");
                    if let Some(ref stats) = self.daily_stats {
                        stats.output_daily_summary();
                    }
                    self.last_stats_output = Instant::now();
                }

                // P1: Periodic position resync (safety net, Trading mode only)
                Some(_) = async {
                    match &mut resync_interval {
                        Some(interval) => Some(interval.tick().await),
                        None => std::future::pending().await,
                    }
                } => {
                    if self.config.mode == OperatingMode::Trading {
                        if let (Some(ref tracker), Some(ref user_addr)) =
                            (&self.position_tracker, &trading_user_address)
                        {
                            match self.sync_positions_from_api(tracker, user_addr).await {
                                Ok(()) => {
                                    debug!("Periodic position resync completed");
                                }
                                Err(e) => {
                                    warn!(?e, "Periodic position resync failed");
                                }
                            }
                        }
                    }
                }

                // Handle shutdown signal
                _ = tokio::signal::ctrl_c() => {
                    info!("Shutdown signal received");
                    break;
                }
            }
        }

        // Cleanup
        info!(signal_count, "Shutting down");

        // P0-31: Output final statistics
        info!("Final statistics summary:");
        if let Some(ref stats) = self.daily_stats {
            stats.output_daily_summary();
        }

        // BUG-001 fix: Call close() instead of flush() to ensure Parquet footer is written.
        // flush() only writes row groups, close() finalizes the file with proper footer.
        self.writer.close()?;

        // Close followup writer
        {
            let mut writer = self.followup_writer.lock().await;
            if let Err(e) = writer.close() {
                warn!(?e, "Failed to close followup writer");
            }
        }

        // Abort tick handle if running
        if let Some(handle) = _tick_handle {
            handle.abort();
        }

        // Graceful shutdown of Position Tracker (P0-2)
        if let Some(ref tracker) = self.position_tracker {
            debug!("Sending shutdown to position tracker");
            tracker.shutdown().await;
        }
        if let Some(handle) = self.position_tracker_handle.take() {
            const POSITION_TRACKER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
            match tokio::time::timeout(POSITION_TRACKER_SHUTDOWN_TIMEOUT, handle).await {
                Ok(Ok(())) => debug!("Position tracker task completed"),
                Ok(Err(e)) => warn!(?e, "Position tracker task panicked"),
                Err(_) => warn!("Position tracker shutdown timed out (5s)"),
            }
        }

        // Graceful shutdown of WebSocket connection (F2-4)
        if let Some(ref cm) = self.connection_manager {
            cm.shutdown();
        }
        const WS_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

        // Use select! instead of timeout to keep handle for abort
        tokio::select! {
            result = &mut ws_handle => {
                match result {
                    Ok(()) => debug!("WebSocket task completed"),
                    Err(e) => warn!(?e, "WebSocket task panicked"),
                }
            }
            () = tokio::time::sleep(WS_SHUTDOWN_TIMEOUT) => {
                warn!("WebSocket shutdown timed out (5s), aborting task");
                ws_handle.abort();
            }
        }

        Ok(())
    }

    /// Handle incoming WebSocket message.
    async fn handle_message(&self, parser: &MessageParser, msg: WsMessage) -> AppResult<()> {
        match &msg {
            WsMessage::Channel(channel_msg) => {
                let channel = &channel_msg.channel;

                // Handle post responses (Trading mode)
                if channel == "post" {
                    if let Some(resp) = msg.as_post_response() {
                        if let Some(ref executor_loop) = self.executor_loop {
                            match resp.response {
                                PostResponseBody::Action { ref payload } => {
                                    // Parse statuses from response to handle immediate fills
                                    let statuses = payload.parse_statuses();
                                    if statuses.is_empty() {
                                        // No statuses (e.g., cancel response) - use simple OK
                                        executor_loop.on_response_ok(resp.id);
                                        debug!(post_id = resp.id, "Post response OK (no statuses)");
                                    } else {
                                        // Process statuses to handle immediate fills/rejects
                                        debug!(
                                            post_id = resp.id,
                                            statuses_count = statuses.len(),
                                            "Post response OK with statuses"
                                        );
                                        executor_loop
                                            .on_response_with_statuses(resp.id, statuses)
                                            .await;
                                    }
                                }
                                PostResponseBody::Error { ref payload } => {
                                    executor_loop.on_response_rejected(resp.id, payload.clone());
                                    warn!(post_id = resp.id, reason = %payload, "Post response rejected");
                                }
                            }
                        } else {
                            debug!(post_id = resp.id, "Post response ignored: no executor_loop");
                        }
                    }
                    return Ok(());
                }

                // Handle orderUpdates (Trading mode)
                if is_order_updates_channel(channel) {
                    let result = msg.as_order_updates();

                    // Log parse failures at warn level for visibility
                    if result.failed_count > 0 {
                        warn!(
                            channel = %channel,
                            failed_count = result.failed_count,
                            parsed_count = result.updates.len(),
                            "Some orderUpdate elements failed to parse"
                        );
                    }

                    if result.updates.is_empty() {
                        // Empty array (initial snapshot) or all elements failed
                        debug!(channel = %channel, "orderUpdates: no updates to process");
                    } else {
                        for update in &result.updates {
                            self.handle_order_update(update);
                        }
                    }
                    return Ok(());
                }

                // Handle userFills (Trading mode)
                if channel == "userFills" {
                    if let Some(user_fills) = msg.as_user_fills() {
                        if user_fills.is_snapshot {
                            // IMPORTANT: Skip processing snapshot fills for position tracking.
                            // Snapshot contains historical fills that would incorrectly rebuild
                            // positions that were already closed. The correct position state
                            // comes from sync_positions_from_api() (clearinghouseState API).
                            info!(
                                fills_count = user_fills.fills.len(),
                                "Received userFills snapshot (skipping for position tracking)"
                            );
                        } else {
                            // Process only streaming updates (non-snapshot)
                            for fill in &user_fills.fills {
                                self.handle_user_fill(fill);
                            }
                            if user_fills.fills.is_empty() {
                                debug!("userFills update with empty fills array");
                            }
                        }
                    } else {
                        // Failed to parse userFills - log the raw data for debugging
                        warn!(
                            raw_data = ?channel_msg.data,
                            "Failed to parse userFills message"
                        );
                    }
                    return Ok(());
                }

                // Parse and update market state (bbo, activeAssetCtx, etc.)
                if let Some(event) = parser
                    .parse_channel_message(channel, &channel_msg.data)
                    .map_err(AppError::Feed)?
                {
                    self.apply_market_event(event);
                }
            }
            WsMessage::Pong(_) => {
                // Pong is handled by connection manager and not forwarded,
                // but we include this arm for completeness
                debug!("Received pong (unexpected - should be handled by connection manager)");
            }
        }

        Ok(())
    }

    /// Map Hyperliquid order status to internal OrderState.
    ///
    /// Status classification based on:
    /// https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint
    fn map_order_status(status: &str) -> OrderState {
        match status {
            // Active states (non-terminal)
            "open" => OrderState::Open,
            "triggered" => OrderState::Open, // Trigger order activated, treat as open

            // Filled state
            "filled" => OrderState::Filled,

            // Explicit cancel
            "canceled" => OrderState::Cancelled,

            // Explicit reject
            "rejected" => OrderState::Rejected,

            // Pattern matching for *Rejected statuses
            s if s.ends_with("Rejected") => {
                debug!(status = %s, "Order rejected by exchange");
                OrderState::Rejected
            }

            // Pattern matching for *Canceled statuses
            s if s.ends_with("Canceled") => {
                debug!(status = %s, "Order canceled by exchange");
                OrderState::Cancelled
            }

            // Special case: scheduledCancel (ends with "Cancel", not "Canceled")
            "scheduledCancel" => {
                debug!("Order canceled by scheduled cancel deadline");
                OrderState::Cancelled
            }

            // Unknown status - treat as terminal to avoid pending order leak
            other => {
                warn!(
                    status = %other,
                    "Unknown order status, treating as cancelled to prevent pending leak"
                );
                OrderState::Cancelled
            }
        }
    }

    /// Handle orderUpdates message.
    fn handle_order_update(&self, update: &OrderUpdatePayload) {
        let cloid_str = update.order.cloid.as_deref().unwrap_or("<no cloid>");
        let oid = update.order.oid;
        let status = &update.status;
        let coin = &update.order.coin;
        let sz = &update.order.sz;

        debug!(
            cloid = %cloid_str,
            oid = oid,
            status = %status,
            coin = %coin,
            sz = %sz,
            "Order update received"
        );

        let Some(ref tracker) = self.position_tracker else {
            debug!("Order update ignored: no position tracker");
            return;
        };

        let cloid = match &update.order.cloid {
            Some(s) => ClientOrderId::from(s.clone()),
            None => {
                warn!(oid = oid, "Order update missing cloid");
                return;
            }
        };

        let state = Self::map_order_status(status);

        // Send Rejected event to RiskMonitor
        if state == OrderState::Rejected {
            if let Some(ref event_tx) = self.risk_event_tx {
                let event = ExecutionEvent::Rejected {
                    cloid: cloid.clone(),
                    reason: status.clone(),
                };
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    if tx.send(event).await.is_err() {
                        warn!("Failed to send Rejected event to RiskMonitor");
                    }
                });
            }
        }

        let filled_size = sz.parse().map(Size::new).unwrap_or(Size::ZERO);

        let tracker = tracker.clone();
        tokio::spawn(async move {
            tracker
                .order_update(cloid, state, filled_size, Some(oid))
                .await;
        });
    }

    /// Handle userFills message.
    fn handle_user_fill(&self, fill: &FillPayload) {
        let coin = &fill.coin;
        let side_str = &fill.side;
        let px = &fill.px;
        let sz = &fill.sz;
        let time = fill.time;

        debug!(
            coin = %coin,
            side = %side_str,
            px = %px,
            sz = %sz,
            time = time,
            "User fill received"
        );

        let Some(ref tracker) = self.position_tracker else {
            debug!("Fill ignored: no position tracker");
            return;
        };

        // Convert coin to MarketKey
        let market = match self.coin_to_market(coin) {
            Some(m) => m,
            None => {
                warn!(coin = %coin, "Unknown coin in fill");
                return;
            }
        };

        let side = match side_str.as_str() {
            "B" => OrderSide::Buy,
            "A" => OrderSide::Sell,
            other => {
                warn!(side = %other, "Unknown side in fill");
                return;
            }
        };

        let price = px.parse().map(Price::new).unwrap_or(Price::ZERO);
        let size = sz.parse().map(Size::new).unwrap_or(Size::ZERO);

        // Extract cloid from FillPayload for deduplication
        let cloid = fill.cloid.as_ref().map(|s| ClientOrderId::from(s.clone()));

        let tracker = tracker.clone();
        let timestamp = time;
        tokio::spawn(async move {
            tracker
                .fill(market, side, price, size, timestamp, cloid)
                .await;
        });
    }

    /// Convert coin name to MarketKey.
    fn coin_to_market(&self, coin: &str) -> Option<MarketKey> {
        let dex_id = self.get_dex_id();
        for market in self.config.get_markets() {
            // Match full coin name (e.g., "xyz:AAPL") or suffix (e.g., "AAPL")
            if market.coin == coin || market.coin.ends_with(&format!(":{}", coin)) {
                return Some(MarketKey::new(dex_id, AssetId::new(market.asset_idx)));
            }
        }
        None
    }

    /// Apply market event to state.
    fn apply_market_event(&self, event: MarketEvent) {
        match event {
            MarketEvent::BboUpdate { key, bbo } => {
                let key_str = key.to_string();

                // P0-31: Record BBO update for null rate calculation
                Metrics::bbo_update(&key_str);

                // P0-31: Check for null BBO (bid or ask has zero price/size)
                let is_null = bbo.bid_price.is_zero()
                    || bbo.ask_price.is_zero()
                    || bbo.bid_size.is_zero()
                    || bbo.ask_size.is_zero();

                if is_null {
                    Metrics::bbo_null_update(&key_str);
                }

                // Update spread metric
                if let Some(spread_bps) = bbo.spread_bps() {
                    Metrics::spread(&key_str, spread_bps.to_string().parse().unwrap_or(0.0));
                }

                // Phase A: No server_time from WebSocket yet
                self.market_state.update_bbo(key, bbo, None);

                // P0-31: Record BBO age to histogram after state update
                if let Some(bbo_age_ms) = self.market_state.get_bbo_age_ms(&key) {
                    Metrics::bbo_age_hist(&key_str, bbo_age_ms as f64);
                }

                // WS-driven exit check: immediately check for mark regression
                if let Some(ref exit_watcher) = self.exit_watcher {
                    if let Some(snapshot) = self.market_state.get_snapshot(&key) {
                        exit_watcher.on_market_update(key, &snapshot);
                    }
                }
            }
            MarketEvent::CtxUpdate { key, ctx } => {
                let key_str = key.to_string();

                // Phase B: keep executor's markPx cache updated for notional gates.
                // Use ctx.received_at as the monotonic timestamp source.
                let mark_px = ctx.oracle.mark_px;
                let now_ms = ctx.received_at.timestamp_millis() as u64;
                if let Some(ref executor_loop) = self.executor_loop {
                    executor_loop
                        .executor()
                        .market_state_cache()
                        .update(&key, mark_px, now_ms);
                }

                // P2-1: Update state first, then record metrics
                self.market_state.update_ctx(key, ctx.clone());

                // Record oracle movement for consecutive direction tracking
                let oracle_px = ctx.oracle.oracle_px;
                self.oracle_tracker.record_move(key, oracle_px);

                // Update oracle age gauge metric (after state update)
                if let Some(oracle_age) = self.market_state.get_oracle_age_ms(&key) {
                    Metrics::oracle_age(&key_str, oracle_age as f64);
                }

                // P0-31: Record ctx age to histogram after state update
                if let Some(ctx_age_ms) = self.market_state.get_ctx_age_ms(&key) {
                    Metrics::ctx_age_hist(&key_str, ctx_age_ms as f64);
                }

                // WS-driven exit check: immediately check for mark regression
                if let Some(ref exit_watcher) = self.exit_watcher {
                    if let Some(snapshot) = self.market_state.get_snapshot(&key) {
                        exit_watcher.on_market_update(key, &snapshot);
                    }
                }

                // Oracle-driven exit check: check for reversal/catchup
                if let Some(ref oracle_exit) = self.oracle_exit_watcher {
                    if let Some(snapshot) = self.market_state.get_snapshot(&key) {
                        oracle_exit.on_market_update(key, &snapshot);
                    }
                }
            }
        }
    }

    /// Check all markets for dislocations.
    async fn check_dislocations(&mut self) -> Option<Vec<DislocationSignal>> {
        let mut signals = Vec::new();
        let dex_id = self.get_dex_id();

        for market in self.config.get_markets() {
            let key = MarketKey::new(dex_id, AssetId::new(market.asset_idx));

            // Get market snapshot
            let snapshot = match self.market_state.get_snapshot(&key) {
                Some(s) => s,
                None => {
                    // P0-31: Update cross tracker (no cross when market not ready)
                    self.cross_tracker.update(key, false, None);
                    continue;
                }
            };

            // Get market spec (use default if not cached yet)
            let spec = self.spec_cache.get(&key).unwrap_or_default();

            // Get data freshness metrics (P0-12, P0-16)
            // BUG-002 fix: oracle_age_ms removed - ctx_age_ms now covers oracle freshness
            let bbo_age_ms = self.market_state.get_bbo_age_ms(&key).unwrap_or(i64::MAX);
            let ctx_age_ms = self.market_state.get_ctx_age_ms(&key).unwrap_or(i64::MAX);
            let bbo_server_time = self.market_state.get_bbo_server_time(&key);

            // Update freshness metrics
            Metrics::bbo_age(&key.to_string(), bbo_age_ms as f64);
            Metrics::ctx_age(&key.to_string(), ctx_age_ms as f64);

            // Check risk gates
            match self.risk_gate.check_all(
                &snapshot,
                &spec,
                bbo_age_ms,
                ctx_age_ms,
                bbo_server_time,
                None,
            ) {
                Ok(_results) => {
                    // BUG-003 fix: Clear block state when gates pass
                    // This allows state-change logging when block resumes
                    self.gate_block_state.retain(|(k, _), _| *k != key);

                    // Edge tracking: Calculate and record edge for threshold calibration
                    let oracle = snapshot.ctx.oracle.oracle_px.inner();
                    if !oracle.is_zero() {
                        // Buy edge: (oracle - ask) / oracle * 10000
                        let buy_edge = (oracle - snapshot.bbo.ask_price.inner()) / oracle
                            * Decimal::from(10000);
                        // Sell edge: (bid - oracle) / oracle * 10000
                        let sell_edge = (snapshot.bbo.bid_price.inner() - oracle) / oracle
                            * Decimal::from(10000);
                        self.edge_tracker.record_edge(key, buy_edge, sell_edge);
                    }

                    // Look up per-market threshold override
                    let threshold_override = self.market_threshold_map.get(&key.asset.0).copied();

                    // All gates passed, check for dislocation
                    if let Some(signal) = self.detector.check(
                        key,
                        &snapshot,
                        threshold_override,
                        Some(&self.oracle_tracker),
                    ) {
                        // P0-31: Cross detected - record cross count and update tracker
                        let side = signal.side;
                        Metrics::cross_detected(&key.to_string(), &side.to_string());
                        self.cross_tracker.update(key, true, Some(side));
                        signals.push(signal);
                    } else {
                        // P0-31: No cross this tick
                        self.cross_tracker.update(key, false, None);
                    }
                }
                Err(e) => {
                    // BUG-003 fix: State-change-only logging to reduce log spam.
                    // Extract gate name and reason from error.
                    let (gate_name, reason) = match &e {
                        RiskError::GateBlocked { gate, reason } => (gate.clone(), reason.clone()),
                        _ => ("unknown".to_string(), e.to_string()),
                    };

                    // Check if this is a state change (wasn't blocked before)
                    let state_key = (key, gate_name.clone());
                    let was_blocked = self
                        .gate_block_state
                        .get(&state_key)
                        .copied()
                        .unwrap_or(false);

                    if !was_blocked {
                        // State changed: was passing, now blocked -> log once
                        warn!(
                            market = %key,
                            gate = %gate_name,
                            reason = %reason,
                            "Gate block started"
                        );
                    }

                    // Update state to blocked
                    self.gate_block_state.insert(state_key, true);

                    // Always record metrics (no spam, just counters)
                    Metrics::gate_blocked(&gate_name, &key.to_string());
                    self.cross_tracker.update(key, false, None);
                }
            }
        }

        // Edge tracking: Periodic logging for threshold calibration
        self.edge_tracker.maybe_log();

        if signals.is_empty() {
            None
        } else {
            Some(signals)
        }
    }

    /// Deduplication interval in milliseconds.
    /// Signals within this interval for the same (market, side) are skipped.
    const DEDUP_INTERVAL_MS: i64 = 500;

    /// Persist signal to JSON Lines and add to recent signals buffer.
    /// Returns true if signal was persisted, false if deduplicated (skipped).
    fn persist_signal(&mut self, signal: &DislocationSignal) -> AppResult<bool> {
        let timestamp_ms = signal.detected_at.timestamp_millis();
        let market_key = signal.market_key.to_string();
        let side = signal.side.to_string();

        // Deduplication: skip if same (market, side) signal was persisted recently
        let dedup_key = (market_key.clone(), side.clone());
        if let Some(&last_ts) = self.last_persisted_signals.get(&dedup_key) {
            if timestamp_ms - last_ts < Self::DEDUP_INTERVAL_MS {
                // Skip: too close to last persisted signal
                return Ok(false);
            }
        }
        let raw_edge_bps = signal.raw_edge_bps.to_string().parse().unwrap_or(0.0);
        let net_edge_bps = signal.net_edge_bps.to_string().parse().unwrap_or(0.0);
        let oracle_px = signal.oracle_px.inner().to_string().parse().unwrap_or(0.0);
        let best_px = signal.best_px.inner().to_string().parse().unwrap_or(0.0);
        let best_size = signal.book_size.inner().to_string().parse().unwrap_or(0.0);
        let suggested_size = signal
            .suggested_size
            .inner()
            .to_string()
            .parse()
            .unwrap_or(0.0);

        let record = SignalRecord {
            timestamp_ms,
            market_key: market_key.clone(),
            side: side.clone(),
            raw_edge_bps,
            net_edge_bps,
            oracle_px,
            best_px,
            best_size,
            suggested_size,
            signal_id: signal.signal_id.clone(),
        };

        // Add to recent signals buffer (for dashboard)
        {
            let mut signals = self.recent_signals.write();
            signals.push_back(record.clone());
            // Keep only last 50 signals
            while signals.len() > 50 {
                signals.pop_front();
            }
        }

        // Send real-time signal to dashboard (non-blocking)
        if let Some(tx) = &self.dashboard_signal_tx {
            let snapshot = SignalSnapshot {
                timestamp_ms,
                market_key,
                side,
                raw_edge_bps,
                net_edge_bps,
                oracle_price: oracle_px,
                best_price: best_px,
                best_size,
                suggested_size,
                signal_id: signal.signal_id.clone(),
            };
            // Use try_send to avoid blocking on full channel
            if let Err(e) = tx.try_send(snapshot) {
                debug!(error = %e, "Failed to send signal to dashboard (channel full or closed)");
            }
        }

        // Update deduplication map before writing
        self.last_persisted_signals.insert(dedup_key, timestamp_ms);

        debug!(
            market = %record.market_key,
            side = %record.side,
            "Signal persisted (not deduplicated)"
        );

        self.writer
            .add_record(record)
            .map_err(AppError::Persistence)?;

        Ok(true)
    }

    /// Schedule followup snapshots at T+1s, T+3s, T+5s.
    ///
    /// Spawns background tasks to capture market state after the signal
    /// for validation analysis.
    fn schedule_followups(&self, signal: &DislocationSignal) {
        let ctx = FollowupContext {
            signal_id: signal.signal_id.clone(),
            market_key: signal.market_key,
            side: signal.side,
            signal_timestamp_ms: signal.detected_at.timestamp_millis(),
            t0_oracle_px: signal.oracle_px.inner().to_string().parse().unwrap_or(0.0),
            t0_best_px: signal.best_px.inner().to_string().parse().unwrap_or(0.0),
            t0_raw_edge_bps: signal.raw_edge_bps.to_string().parse().unwrap_or(0.0),
        };

        for offset_ms in FOLLOWUP_OFFSETS_MS {
            let market_state = self.market_state.clone();
            let followup_writer = self.followup_writer.clone();
            let ctx = ctx.clone();

            tokio::spawn(async move {
                capture_followup(market_state, followup_writer, ctx, offset_ms).await;
            });
        }

        debug!(
            signal_id = %signal.signal_id,
            "Scheduled followup snapshots at T+1s, T+3s, T+5s"
        );
    }
}

/// Capture a followup snapshot after delay.
///
/// Called from spawned tasks to record market state at T+N ms after signal.
async fn capture_followup(
    market_state: Arc<MarketState>,
    followup_writer: Arc<tokio::sync::Mutex<FollowupWriter>>,
    ctx: FollowupContext,
    offset_ms: u64,
) {
    // Wait for the specified offset
    tokio::time::sleep(Duration::from_millis(offset_ms)).await;

    let captured_at = Utc::now();

    // Get current market state
    let snapshot = match market_state.get_snapshot(&ctx.market_key) {
        Some(s) => s,
        None => {
            debug!(
                signal_id = %ctx.signal_id,
                offset_ms,
                "Followup capture skipped: market state not available"
            );
            return;
        }
    };

    // Get current prices
    let (best_px, best_size) = match ctx.side {
        OrderSide::Buy => (
            snapshot
                .bbo
                .ask_price
                .inner()
                .to_string()
                .parse()
                .unwrap_or(0.0),
            snapshot
                .bbo
                .ask_size
                .inner()
                .to_string()
                .parse()
                .unwrap_or(0.0),
        ),
        OrderSide::Sell => (
            snapshot
                .bbo
                .bid_price
                .inner()
                .to_string()
                .parse()
                .unwrap_or(0.0),
            snapshot
                .bbo
                .bid_size
                .inner()
                .to_string()
                .parse()
                .unwrap_or(0.0),
        ),
    };
    let oracle_px: f64 = snapshot
        .ctx
        .oracle
        .oracle_px
        .inner()
        .to_string()
        .parse()
        .unwrap_or(0.0);

    // Calculate current edge
    let raw_edge_bps = if oracle_px > 0.0 {
        match ctx.side {
            OrderSide::Buy => (oracle_px - best_px) / oracle_px * 10000.0,
            OrderSide::Sell => (best_px - oracle_px) / oracle_px * 10000.0,
        }
    } else {
        0.0
    };

    // Calculate movements
    let oracle_moved_bps = if ctx.t0_oracle_px > 0.0 {
        (oracle_px - ctx.t0_oracle_px) / ctx.t0_oracle_px * 10000.0
    } else {
        0.0
    };
    let market_moved_bps = if ctx.t0_best_px > 0.0 {
        (best_px - ctx.t0_best_px) / ctx.t0_best_px * 10000.0
    } else {
        0.0
    };
    let edge_change_bps = raw_edge_bps - ctx.t0_raw_edge_bps;

    let record = FollowupRecord {
        signal_id: ctx.signal_id.clone(),
        market_key: ctx.market_key.to_string(),
        side: ctx.side.to_string(),
        signal_timestamp_ms: ctx.signal_timestamp_ms,
        offset_ms,
        captured_at_ms: captured_at.timestamp_millis(),
        t0_oracle_px: ctx.t0_oracle_px,
        t0_best_px: ctx.t0_best_px,
        t0_raw_edge_bps: ctx.t0_raw_edge_bps,
        oracle_px,
        best_px,
        best_size,
        raw_edge_bps,
        edge_change_bps,
        oracle_moved_bps,
        market_moved_bps,
    };

    // Write record
    {
        let mut writer = followup_writer.lock().await;
        if let Err(e) = writer.add_record(record) {
            warn!(
                ?e,
                signal_id = %ctx.signal_id,
                offset_ms,
                "Failed to write followup record"
            );
        } else {
            debug!(
                signal_id = %ctx.signal_id,
                offset_ms,
                edge_change_bps = format!("{:.2}", edge_change_bps),
                "Captured followup snapshot"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a minimal config for testing.
    fn test_config_with_markets(markets: Vec<MarketConfig>) -> AppConfig {
        AppConfig {
            mode: OperatingMode::Observation,
            markets: Some(markets),
            ..Default::default()
        }
    }

    /// Test coin_to_market with full match.
    #[test]
    fn test_coin_to_market_full_match() {
        let markets = vec![
            MarketConfig {
                coin: "BTC".to_string(),
                asset_idx: 0,
                threshold_bps: None,
            },
            MarketConfig {
                coin: "ETH".to_string(),
                asset_idx: 1,
                threshold_bps: None,
            },
        ];
        let config = test_config_with_markets(markets);
        let app = Application::new(config).unwrap();

        let result = app.coin_to_market("BTC");
        assert!(result.is_some(), "Should find BTC");
        let market_key = result.unwrap();
        assert_eq!(market_key.asset, AssetId::new(0));

        let result = app.coin_to_market("ETH");
        assert!(result.is_some(), "Should find ETH");
        let market_key = result.unwrap();
        assert_eq!(market_key.asset, AssetId::new(1));
    }

    /// Test coin_to_market with suffix match (e.g., "xyz:AAPL" matches "AAPL").
    #[test]
    fn test_coin_to_market_suffix_match() {
        let markets = vec![MarketConfig {
            coin: "xyz:AAPL".to_string(),
            asset_idx: 10,
            threshold_bps: None,
        }];
        let config = test_config_with_markets(markets);
        let app = Application::new(config).unwrap();

        // Should match by suffix
        let result = app.coin_to_market("AAPL");
        assert!(result.is_some(), "Should find AAPL by suffix match");
        let market_key = result.unwrap();
        assert_eq!(market_key.asset, AssetId::new(10));
    }

    /// Test coin_to_market with not found.
    #[test]
    fn test_coin_to_market_not_found() {
        let markets = vec![MarketConfig {
            coin: "BTC".to_string(),
            asset_idx: 0,
            threshold_bps: None,
        }];
        let config = test_config_with_markets(markets);
        let app = Application::new(config).unwrap();

        let result = app.coin_to_market("UNKNOWN");
        assert!(result.is_none(), "Should not find unknown coin");
    }

    /// Test get_dex_id with default value.
    #[test]
    fn test_get_dex_id_default() {
        let config = test_config_with_markets(vec![]);
        let app = Application::new(config).unwrap();

        // When xyz_dex_id is None, should return default DexId::XYZ
        let dex_id = app.get_dex_id();
        assert_eq!(dex_id, DexId::XYZ);
    }

    /// Test current_time_ms helper function.
    #[test]
    fn test_current_time_ms() {
        let t1 = current_time_ms();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let t2 = current_time_ms();

        assert!(t2 > t1, "Time should advance");
        assert!(t2 - t1 >= 10, "Should have at least 10ms difference");
    }

    /// Test map_order_status for active states.
    #[test]
    fn test_map_order_status_active() {
        assert_eq!(Application::map_order_status("open"), OrderState::Open);
        assert_eq!(Application::map_order_status("triggered"), OrderState::Open);
    }

    /// Test map_order_status for filled state.
    #[test]
    fn test_map_order_status_filled() {
        assert_eq!(Application::map_order_status("filled"), OrderState::Filled);
    }

    /// Test map_order_status for explicit cancel/reject.
    #[test]
    fn test_map_order_status_explicit_terminal() {
        assert_eq!(
            Application::map_order_status("canceled"),
            OrderState::Cancelled
        );
        assert_eq!(
            Application::map_order_status("rejected"),
            OrderState::Rejected
        );
    }

    /// Test map_order_status for *Rejected pattern.
    #[test]
    fn test_map_order_status_rejected_pattern() {
        assert_eq!(
            Application::map_order_status("perpMarginRejected"),
            OrderState::Rejected
        );
        assert_eq!(
            Application::map_order_status("oracleRejected"),
            OrderState::Rejected
        );
        assert_eq!(
            Application::map_order_status("tickRejected"),
            OrderState::Rejected
        );
    }

    /// Test map_order_status for *Canceled pattern.
    #[test]
    fn test_map_order_status_canceled_pattern() {
        assert_eq!(
            Application::map_order_status("marginCanceled"),
            OrderState::Cancelled
        );
        assert_eq!(
            Application::map_order_status("liquidatedCanceled"),
            OrderState::Cancelled
        );
        assert_eq!(
            Application::map_order_status("selfTradeCanceled"),
            OrderState::Cancelled
        );
    }

    /// Test map_order_status for scheduledCancel special case.
    #[test]
    fn test_map_order_status_scheduled_cancel() {
        assert_eq!(
            Application::map_order_status("scheduledCancel"),
            OrderState::Cancelled
        );
    }

    /// Test map_order_status for unknown status (fail safe).
    #[test]
    fn test_map_order_status_unknown() {
        // Unknown status should be treated as Cancelled to prevent pending leak
        assert_eq!(
            Application::map_order_status("unknownFutureStatus"),
            OrderState::Cancelled
        );
    }
}
