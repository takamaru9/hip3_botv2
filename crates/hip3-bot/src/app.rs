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
use crate::error::{AppError, AppResult};
use hip3_core::{AssetId, DexId, MarketKey};
use hip3_detector::{CrossDurationTracker, DislocationDetector, DislocationSignal};
use hip3_feed::{MarketEvent, MarketState, MessageParser};
use hip3_persistence::{ParquetWriter, SignalRecord};
use hip3_registry::{MetaClient, PreflightChecker, SpecCache};
use hip3_risk::{RiskError, RiskGate};
use hip3_telemetry::{DailyStatsReporter, Metrics};
use hip3_ws::{ConnectionConfig, ConnectionManager, WsMessage};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Daily stats output interval (1 hour).
const DAILY_STATS_INTERVAL: Duration = Duration::from_secs(3600);

/// Main application.
pub struct Application {
    config: AppConfig,
    market_state: Arc<MarketState>,
    spec_cache: Arc<SpecCache>,
    risk_gate: RiskGate,
    detector: DislocationDetector,
    writer: ParquetWriter,
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
        let detector = DislocationDetector::new(config.detector.clone());
        let writer =
            ParquetWriter::new(&config.persistence.data_dir, config.persistence.buffer_size);

        // P0-31: Cross tracker initialized, daily_stats deferred until markets known
        let cross_tracker = CrossDurationTracker::new();

        Ok(Self {
            config,
            market_state,
            spec_cache,
            risk_gate,
            detector,
            writer,
            cross_tracker,
            daily_stats: None, // Initialized after preflight
            last_stats_output: Instant::now(),
            xyz_dex_id: None,
            gate_block_state: HashMap::new(),
        })
    }

    /// Run preflight validation and market discovery (P0-15, P0-26, P0-27).
    ///
    /// This fetches perpDexs from the exchange and discovers xyz markets.
    /// Must be called before `run()` if markets are not specified in config.
    pub async fn run_preflight(&mut self) -> AppResult<()> {
        // Skip if markets already configured
        if self.config.has_markets() {
            info!("Markets already configured, skipping preflight");
            self.initialize_daily_stats();
            return Ok(());
        }

        info!(
            info_url = %self.config.info_url,
            xyz_pattern = %self.config.xyz_pattern,
            "Running preflight validation (P0-15, P0-26, P0-27)"
        );

        // Fetch perpDexs from exchange
        let client = MetaClient::new(&self.config.info_url)
            .map_err(|e| AppError::Preflight(format!("Failed to create HTTP client: {e}")))?;

        let perp_dexs = client
            .fetch_perp_dexs()
            .await
            .map_err(|e| AppError::Preflight(format!("Failed to fetch perpDexs: {e}")))?;

        // Validate and discover markets
        let checker = PreflightChecker::new(&self.config.xyz_pattern);
        let result = checker
            .validate(&perp_dexs)
            .map_err(|e| AppError::Preflight(format!("Preflight validation failed: {e}")))?;

        // Log warnings if any
        for warning in &result.warnings {
            warn!(warning = %warning, "Preflight warning");
        }

        // Store xyz DEX ID
        self.xyz_dex_id = Some(result.xyz_dex_id);

        // Convert discovered markets to config format
        // WebSocket subscriptions require full coin name with dex prefix (e.g., "xyz:AAPL")
        let dex_prefix = &self.config.xyz_pattern;
        let markets: Vec<MarketConfig> = result
            .markets
            .iter()
            .map(|m| MarketConfig {
                asset_idx: m.key.asset.index(),
                coin: format!("{}:{}", dex_prefix, m.name),
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

        // Create message channel
        let (message_tx, mut message_rx) = mpsc::channel::<WsMessage>(1000);

        // Create WebSocket connection manager
        let mut ws_config: ConnectionConfig = self.config.websocket.clone().into();
        ws_config.url = self.config.ws_url.clone();
        ws_config.subscriptions = self.config.subscription_targets();

        info!(
            subscriptions = ?ws_config.subscriptions.iter().map(|s| &s.coin).collect::<Vec<_>>(),
            "Configured WebSocket subscriptions"
        );

        let connection_manager = Arc::new(ConnectionManager::new(ws_config, message_tx));
        let connection_manager_clone = connection_manager.clone();

        // Spawn WebSocket connection task
        let ws_handle = tokio::spawn(async move {
            if let Err(e) = connection_manager_clone.connect().await {
                error!(?e, "WebSocket connection failed");
            }
        });

        // Create message parser with coin mappings
        let mut parser = MessageParser::new();
        for market in self.config.get_markets() {
            parser.add_coin_mapping(market.coin.clone(), market.asset_idx);
        }
        info!(
            coin_mappings = ?self.config.get_markets().iter().map(|m| (&m.coin, m.asset_idx)).collect::<Vec<_>>(),
            "Parser configured with coin mappings"
        );

        // Main event loop
        info!("Entering main event loop");
        let mut signal_count = 0u64;
        let mut stats_interval = tokio::time::interval(DAILY_STATS_INTERVAL);

        // P1-3: Phase B TODO - Add periodic spec refresh task
        // This would detect parameter changes (tick_size, lot_size, etc.) from exchange
        // let spec_refresh_interval = tokio::time::interval(Duration::from_secs(300));

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

                            // Persist signal
                            self.persist_signal(&signal)?;

                            // Phase B: Would execute here
                            if self.config.mode == OperatingMode::Trading {
                                warn!("Trading mode not yet implemented");
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

        // Wait for WebSocket to close
        ws_handle.abort();

        Ok(())
    }

    /// Handle incoming WebSocket message.
    async fn handle_message(&self, parser: &MessageParser, msg: WsMessage) -> AppResult<()> {
        match msg {
            WsMessage::Channel(channel_msg) => {
                // Parse and update market state
                if let Some(event) = parser
                    .parse_channel_message(&channel_msg.channel, &channel_msg.data)
                    .map_err(AppError::Feed)?
                {
                    self.apply_market_event(event);
                }
            }
            WsMessage::Response(_) => {
                // Handle response (subscriptions, etc.)
                debug!("Received response");
            }
            WsMessage::Pong(_) => {
                // Pong is handled by connection manager and not forwarded,
                // but we include this arm for completeness
                debug!("Received pong (unexpected - should be handled by connection manager)");
            }
        }

        Ok(())
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
            }
            MarketEvent::CtxUpdate { key, ctx } => {
                let key_str = key.to_string();

                // P2-1: Update state first, then record metrics
                self.market_state.update_ctx(key, ctx);

                // Update oracle age gauge metric (after state update)
                if let Some(oracle_age) = self.market_state.get_oracle_age_ms(&key) {
                    Metrics::oracle_age(&key_str, oracle_age as f64);
                }

                // P0-31: Record ctx age to histogram after state update
                if let Some(ctx_age_ms) = self.market_state.get_ctx_age_ms(&key) {
                    Metrics::ctx_age_hist(&key_str, ctx_age_ms as f64);
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

                    // All gates passed, check for dislocation
                    if let Some(signal) = self.detector.check(key, &snapshot) {
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
                        RiskError::GateBlocked { gate, reason } => {
                            (gate.clone(), reason.clone())
                        }
                        _ => ("unknown".to_string(), e.to_string()),
                    };

                    // Check if this is a state change (wasn't blocked before)
                    let state_key = (key, gate_name.clone());
                    let was_blocked = self.gate_block_state.get(&state_key).copied().unwrap_or(false);

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

        if signals.is_empty() {
            None
        } else {
            Some(signals)
        }
    }

    /// Persist signal to Parquet.
    fn persist_signal(&mut self, signal: &DislocationSignal) -> AppResult<()> {
        let record = SignalRecord {
            timestamp_ms: signal.detected_at.timestamp_millis(),
            market_key: signal.market_key.to_string(),
            side: signal.side.to_string(),
            raw_edge_bps: signal.raw_edge_bps.to_string().parse().unwrap_or(0.0),
            net_edge_bps: signal.net_edge_bps.to_string().parse().unwrap_or(0.0),
            oracle_px: signal.oracle_px.inner().to_string().parse().unwrap_or(0.0),
            best_px: signal.best_px.inner().to_string().parse().unwrap_or(0.0),
            suggested_size: signal
                .suggested_size
                .inner()
                .to_string()
                .parse()
                .unwrap_or(0.0),
            signal_id: signal.signal_id.clone(),
        };

        self.writer
            .add_record(record)
            .map_err(AppError::Persistence)
    }
}
