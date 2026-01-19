//! Subscription management for WebSocket channels.
//!
//! Tracks subscription state and ensures all required channels are ready
//! before allowing trading operations.
//!
//! Implements P0-4: READY-MD/READY-TRADING phase separation.

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Required channels for READY state.
/// Trading is prohibited until all channels have received initial data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequiredChannel {
    /// Best bid/offer data.
    Bbo,
    /// Asset context (oracle, funding).
    AssetCtx,
    /// Order updates snapshot.
    OrderUpdates,
}

impl RequiredChannel {
    /// Get the channel name pattern.
    ///
    /// P1-2: AssetCtx uses "activeAssetCtx" pattern for WebSocket subscription.
    pub fn channel_pattern(&self) -> &'static str {
        match self {
            Self::Bbo => "bbo",
            Self::AssetCtx => "activeAssetCtx", // P1-2: Changed from "assetCtx"
            Self::OrderUpdates => "orderUpdates",
        }
    }

    /// Check if a channel name matches this required channel.
    pub fn matches(&self, channel: &str) -> bool {
        channel.contains(self.channel_pattern())
    }
}

/// Ready phase for P0-4 separation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyPhase {
    /// Not ready - still initializing.
    NotReady,
    /// READY-MD: Market data ready (bbo + assetCtx).
    /// Sufficient for Phase A observation mode.
    ReadyMD,
    /// READY-TRADING: Full trading ready (bbo + assetCtx + orderUpdates).
    /// Required for Phase B trading mode.
    ReadyTrading,
}

impl ReadyPhase {
    /// Check if market data observation is allowed.
    pub fn can_observe(&self) -> bool {
        matches!(self, Self::ReadyMD | Self::ReadyTrading)
    }

    /// Check if trading is allowed.
    pub fn can_trade(&self) -> bool {
        matches!(self, Self::ReadyTrading)
    }
}

impl std::fmt::Display for ReadyPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReady => write!(f, "NOT_READY"),
            Self::ReadyMD => write!(f, "READY_MD"),
            Self::ReadyTrading => write!(f, "READY_TRADING"),
        }
    }
}

/// Per-market ready state tracking.
#[derive(Debug, Clone, Default)]
pub struct MarketReadyState {
    /// First BBO received timestamp.
    pub bbo_first_recv: Option<DateTime<Utc>>,
    /// Last BBO received timestamp (monotonic tracking for P0-12).
    pub bbo_last_recv: Option<DateTime<Utc>>,
    /// First AssetCtx received timestamp.
    pub ctx_first_recv: Option<DateTime<Utc>>,
    /// Last AssetCtx received timestamp (monotonic tracking for P0-12).
    pub ctx_last_recv: Option<DateTime<Utc>>,
    /// Whether this market has been excluded due to timeout (P0-7).
    pub excluded: bool,
    /// Exclusion reason if excluded.
    pub exclusion_reason: Option<String>,
}

impl MarketReadyState {
    /// Check if market data is ready (P0-4 READY-MD condition).
    pub fn is_md_ready(&self) -> bool {
        !self.excluded && self.bbo_first_recv.is_some() && self.ctx_first_recv.is_some()
    }

    /// Get BBO age in milliseconds (P0-12).
    pub fn bbo_age_ms(&self) -> Option<i64> {
        self.bbo_last_recv
            .map(|t| (Utc::now() - t).num_milliseconds())
    }

    /// Get AssetCtx age in milliseconds (P0-12).
    pub fn ctx_age_ms(&self) -> Option<i64> {
        self.ctx_last_recv
            .map(|t| (Utc::now() - t).num_milliseconds())
    }
}

/// Ready state for trading operations.
#[derive(Debug, Clone, Default)]
pub struct ReadyState {
    pub bbo_ready: bool,
    pub asset_ctx_ready: bool,
    pub order_updates_ready: bool,
}

impl ReadyState {
    /// Check if all required channels are ready (legacy method).
    pub fn is_ready(&self) -> bool {
        self.bbo_ready && self.asset_ctx_ready && self.order_updates_ready
    }

    /// Check if market data is ready (P0-4 READY-MD).
    pub fn is_md_ready(&self) -> bool {
        self.bbo_ready && self.asset_ctx_ready
    }

    /// Get current ready phase (P0-4).
    pub fn phase(&self) -> ReadyPhase {
        if self.bbo_ready && self.asset_ctx_ready && self.order_updates_ready {
            ReadyPhase::ReadyTrading
        } else if self.bbo_ready && self.asset_ctx_ready {
            ReadyPhase::ReadyMD
        } else {
            ReadyPhase::NotReady
        }
    }

    /// Get list of missing channels.
    pub fn missing_channels(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !self.bbo_ready {
            missing.push("bbo");
        }
        if !self.asset_ctx_ready {
            missing.push("assetCtx");
        }
        if !self.order_updates_ready {
            missing.push("orderUpdates");
        }
        missing
    }
}

/// Subscription manager configuration.
#[derive(Debug, Clone)]
pub struct SubscriptionConfig {
    /// Timeout for initial BBO reception (P0-7).
    pub bbo_timeout: Duration,
    /// Maximum data age for freshness check (P0-12).
    pub max_data_age: Duration,
}

impl Default for SubscriptionConfig {
    fn default() -> Self {
        Self {
            bbo_timeout: Duration::from_secs(10),
            max_data_age: Duration::from_secs(8),
        }
    }
}

/// Subscription manager.
///
/// Tracks which channels are subscribed and which have received
/// their initial data (snapshot).
///
/// Implements:
/// - P0-4: READY-MD/READY-TRADING phase separation
/// - P0-7: Initial BBO timeout policy
/// - P0-12: Monotonic freshness tracking
pub struct SubscriptionManager {
    /// Configuration.
    config: SubscriptionConfig,
    /// Set of active subscriptions.
    subscriptions: Arc<RwLock<HashSet<String>>>,
    /// Ready state for trading (global).
    ready_state: Arc<RwLock<ReadyState>>,
    /// Per-market ready state (P0-4, P0-7).
    market_states: Arc<RwLock<HashMap<u16, MarketReadyState>>>,
    /// Subscription start time (for timeout calculation).
    start_time: DateTime<Utc>,
}

impl SubscriptionManager {
    /// Create a new subscription manager.
    pub fn new() -> Self {
        Self::with_config(SubscriptionConfig::default())
    }

    /// Create with custom configuration.
    pub fn with_config(config: SubscriptionConfig) -> Self {
        Self {
            config,
            subscriptions: Arc::new(RwLock::new(HashSet::new())),
            ready_state: Arc::new(RwLock::new(ReadyState::default())),
            market_states: Arc::new(RwLock::new(HashMap::new())),
            start_time: Utc::now(),
        }
    }

    /// Get current ready state.
    pub fn ready_state(&self) -> ReadyState {
        self.ready_state.read().clone()
    }

    /// Get current ready phase (P0-4).
    pub fn ready_phase(&self) -> ReadyPhase {
        self.ready_state.read().phase()
    }

    /// Check if all required channels are ready (legacy).
    pub fn is_ready(&self) -> bool {
        self.ready_state.read().is_ready()
    }

    /// Check if market data is ready (P0-4 READY-MD).
    pub fn is_md_ready(&self) -> bool {
        self.ready_state.read().is_md_ready()
    }

    /// Get per-market ready state.
    pub fn market_state(&self, asset_idx: u16) -> Option<MarketReadyState> {
        self.market_states.read().get(&asset_idx).cloned()
    }

    /// Get all market states.
    pub fn all_market_states(&self) -> HashMap<u16, MarketReadyState> {
        self.market_states.read().clone()
    }

    /// Get list of ready markets (not excluded, MD ready).
    pub fn ready_markets(&self) -> Vec<u16> {
        self.market_states
            .read()
            .iter()
            .filter(|(_, state)| state.is_md_ready())
            .map(|(idx, _)| *idx)
            .collect()
    }

    /// Get list of excluded markets.
    pub fn excluded_markets(&self) -> Vec<(u16, String)> {
        self.market_states
            .read()
            .iter()
            .filter(|(_, state)| state.excluded)
            .map(|(idx, state)| {
                (
                    *idx,
                    state
                        .exclusion_reason
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                )
            })
            .collect()
    }

    /// Record a subscription.
    pub fn add_subscription(&self, channel: String) {
        self.subscriptions.write().insert(channel);
    }

    /// Remove a subscription.
    pub fn remove_subscription(&self, channel: &str) {
        self.subscriptions.write().remove(channel);
    }

    /// Handle incoming message and update ready state.
    pub fn handle_message(&self, channel: &str) {
        self.handle_message_with_asset(channel, None);
    }

    /// Handle incoming message with asset index for per-market tracking.
    pub fn handle_message_with_asset(&self, channel: &str, asset_idx: Option<u16>) {
        let now = Utc::now();
        let mut state = self.ready_state.write();

        // Update per-market state if asset index provided
        if let Some(idx) = asset_idx {
            let mut market_states = self.market_states.write();
            let market_state = market_states.entry(idx).or_default();

            if RequiredChannel::Bbo.matches(channel) {
                if market_state.bbo_first_recv.is_none() {
                    market_state.bbo_first_recv = Some(now);
                    debug!(asset_idx = idx, "Market BBO first received");
                }
                market_state.bbo_last_recv = Some(now);
            } else if RequiredChannel::AssetCtx.matches(channel) {
                if market_state.ctx_first_recv.is_none() {
                    market_state.ctx_first_recv = Some(now);
                    debug!(asset_idx = idx, "Market AssetCtx first received");
                }
                market_state.ctx_last_recv = Some(now);
            }
        }

        // Update global ready state
        if RequiredChannel::Bbo.matches(channel) && !state.bbo_ready {
            info!("BBO channel ready");
            state.bbo_ready = true;
        } else if RequiredChannel::AssetCtx.matches(channel) && !state.asset_ctx_ready {
            info!("AssetCtx channel ready");
            state.asset_ctx_ready = true;
        } else if RequiredChannel::OrderUpdates.matches(channel) && !state.order_updates_ready {
            info!("OrderUpdates channel ready");
            state.order_updates_ready = true;
        }

        // Log phase transitions
        let phase = state.phase();
        match phase {
            ReadyPhase::ReadyMD => {
                debug!("READY-MD: Market data observation enabled");
            }
            ReadyPhase::ReadyTrading => {
                debug!("READY-TRADING: Full trading enabled");
            }
            _ => {}
        }
    }

    /// Check and apply timeout policy for markets (P0-7).
    ///
    /// Markets that haven't received initial BBO within timeout are excluded.
    /// Returns list of newly excluded market indices.
    pub fn check_timeouts(&self, expected_markets: &[u16]) -> Vec<u16> {
        let elapsed = (Utc::now() - self.start_time)
            .to_std()
            .unwrap_or(Duration::ZERO);

        if elapsed < self.config.bbo_timeout {
            return Vec::new();
        }

        let mut market_states = self.market_states.write();
        let mut newly_excluded = Vec::new();

        for &idx in expected_markets {
            let market_state = market_states.entry(idx).or_default();

            if !market_state.excluded && market_state.bbo_first_recv.is_none() {
                market_state.excluded = true;
                market_state.exclusion_reason =
                    Some(format!("BBO timeout after {:?}", self.config.bbo_timeout));
                newly_excluded.push(idx);
                warn!(
                    asset_idx = idx,
                    timeout_ms = self.config.bbo_timeout.as_millis(),
                    "Market excluded due to BBO timeout (P0-7)"
                );
            }
        }

        newly_excluded
    }

    /// Check data freshness for a market (P0-12).
    ///
    /// Returns true if both BBO and AssetCtx are fresh.
    pub fn is_market_fresh(&self, asset_idx: u16) -> bool {
        let market_states = self.market_states.read();
        let Some(state) = market_states.get(&asset_idx) else {
            return false;
        };

        if state.excluded {
            return false;
        }

        let max_age_ms = self.config.max_data_age.as_millis() as i64;

        let bbo_fresh = state
            .bbo_age_ms()
            .map(|age| age < max_age_ms)
            .unwrap_or(false);
        let ctx_fresh = state
            .ctx_age_ms()
            .map(|age| age < max_age_ms)
            .unwrap_or(false);

        bbo_fresh && ctx_fresh
    }

    /// Reset ready state (called on reconnection).
    pub fn reset_ready_state(&self) {
        {
            let mut state = self.ready_state.write();
            *state = ReadyState::default();
        }
        {
            let mut market_states = self.market_states.write();
            market_states.clear();
        }
        info!("Ready state reset");
    }

    /// Get list of active subscriptions.
    pub fn active_subscriptions(&self) -> Vec<String> {
        self.subscriptions.read().iter().cloned().collect()
    }
}

impl Default for SubscriptionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ready_state_initial() {
        let state = ReadyState::default();
        assert!(!state.is_ready());
        assert_eq!(state.missing_channels().len(), 3);
    }

    #[test]
    fn test_ready_state_complete() {
        let state = ReadyState {
            bbo_ready: true,
            asset_ctx_ready: true,
            order_updates_ready: true,
        };
        assert!(state.is_ready());
        assert!(state.missing_channels().is_empty());
    }

    #[test]
    fn test_subscription_manager_ready() {
        let manager = SubscriptionManager::new();
        assert!(!manager.is_ready());

        manager.handle_message("bbo:BTC");
        manager.handle_message("activeAssetCtx:perp:0"); // P1-2: Changed from assetCtx
        manager.handle_message("orderUpdates:user:abc");

        assert!(manager.is_ready());
    }

    #[test]
    fn test_subscription_manager_reset() {
        let manager = SubscriptionManager::new();
        manager.handle_message("bbo:BTC");
        manager.handle_message("activeAssetCtx:perp:0"); // P1-2: Changed from assetCtx
        manager.handle_message("orderUpdates:user:abc");

        assert!(manager.is_ready());

        manager.reset_ready_state();
        assert!(!manager.is_ready());
    }

    // === P0-4: READY-MD/READY-TRADING tests ===

    #[test]
    fn test_ready_phase_not_ready() {
        let state = ReadyState::default();
        assert_eq!(state.phase(), ReadyPhase::NotReady);
        assert!(!state.phase().can_observe());
        assert!(!state.phase().can_trade());
    }

    #[test]
    fn test_ready_phase_md() {
        let state = ReadyState {
            bbo_ready: true,
            asset_ctx_ready: true,
            order_updates_ready: false,
        };
        assert_eq!(state.phase(), ReadyPhase::ReadyMD);
        assert!(state.phase().can_observe());
        assert!(!state.phase().can_trade());
    }

    #[test]
    fn test_ready_phase_trading() {
        let state = ReadyState {
            bbo_ready: true,
            asset_ctx_ready: true,
            order_updates_ready: true,
        };
        assert_eq!(state.phase(), ReadyPhase::ReadyTrading);
        assert!(state.phase().can_observe());
        assert!(state.phase().can_trade());
    }

    #[test]
    fn test_subscription_manager_ready_phase() {
        let manager = SubscriptionManager::new();
        assert_eq!(manager.ready_phase(), ReadyPhase::NotReady);

        manager.handle_message("bbo:perp:0");
        assert_eq!(manager.ready_phase(), ReadyPhase::NotReady);

        manager.handle_message("activeAssetCtx:perp:0"); // P1-2: Changed from assetCtx
        assert_eq!(manager.ready_phase(), ReadyPhase::ReadyMD);
        assert!(manager.is_md_ready());

        manager.handle_message("orderUpdates:user:abc");
        assert_eq!(manager.ready_phase(), ReadyPhase::ReadyTrading);
    }

    // === P0-7: BBO timeout tests ===

    #[test]
    fn test_bbo_timeout_before_deadline() {
        let config = SubscriptionConfig {
            bbo_timeout: Duration::from_secs(10),
            ..Default::default()
        };
        let manager = SubscriptionManager::with_config(config);

        // Before timeout, no markets should be excluded
        let excluded = manager.check_timeouts(&[0, 1, 2]);
        assert!(excluded.is_empty());
    }

    #[test]
    fn test_market_ready_state_tracking() {
        let manager = SubscriptionManager::new();

        // Handle message with asset index
        manager.handle_message_with_asset("bbo:perp:0", Some(0));
        manager.handle_message_with_asset("activeAssetCtx:perp:0", Some(0)); // P1-2: Changed

        let state = manager.market_state(0).unwrap();
        assert!(state.bbo_first_recv.is_some());
        assert!(state.ctx_first_recv.is_some());
        assert!(state.is_md_ready());

        // Market 1 not ready
        assert!(manager.market_state(1).is_none());
    }

    #[test]
    fn test_ready_markets_list() {
        let manager = SubscriptionManager::new();

        manager.handle_message_with_asset("bbo:perp:0", Some(0));
        manager.handle_message_with_asset("activeAssetCtx:perp:0", Some(0)); // P1-2: Changed
        manager.handle_message_with_asset("bbo:perp:1", Some(1));
        // Market 1 missing activeAssetCtx

        let ready = manager.ready_markets();
        assert_eq!(ready.len(), 1);
        assert!(ready.contains(&0));
    }

    // === P0-12: Freshness tests ===

    #[test]
    fn test_market_freshness() {
        let config = SubscriptionConfig {
            max_data_age: Duration::from_secs(8),
            ..Default::default()
        };
        let manager = SubscriptionManager::with_config(config);

        // Fresh data
        manager.handle_message_with_asset("bbo:perp:0", Some(0));
        manager.handle_message_with_asset("activeAssetCtx:perp:0", Some(0)); // P1-2: Changed

        assert!(manager.is_market_fresh(0));

        // Non-existent market
        assert!(!manager.is_market_fresh(99));
    }

    #[test]
    fn test_market_ready_state_age() {
        let manager = SubscriptionManager::new();
        manager.handle_message_with_asset("bbo:perp:0", Some(0));

        let state = manager.market_state(0).unwrap();
        let age = state.bbo_age_ms().unwrap();

        // Age should be very small (just created)
        assert!(age < 100);
    }
}
