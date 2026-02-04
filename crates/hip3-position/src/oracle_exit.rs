//! Oracle-driven exit watcher for dynamic position management.
//!
//! Unlike time-based exits (TimeStop) or mark regression exits (ExitWatcher),
//! `OracleExitWatcher` monitors oracle price movements to determine exit timing.
//!
//! # Trading Philosophy
//!
//! > **正しいエッジ**: オラクルが動いた後、マーケットメーカーの注文が追従していない
//! > 「取り残された流動性」を取る
//!
//! Exit conditions:
//! 1. **Loss Cut (OracleReversal)**: Oracle moves against our position N times
//!    - The "stale liquidity" edge has disappeared
//!    - Cut losses quickly before they grow
//!
//! 2. **Profit Take (OracleCatchup)**: Oracle moves in our favor N times
//!    - MMs are catching up, edge is narrowing
//!    - Take profit before spread compresses
//!
//! # Architecture
//!
//! ```text
//! WS Message → App.handle_market_event()
//!                     ↓
//!              oracle_tracker.record_move(key, oracle_px)
//!                     ↓
//!              oracle_exit_watcher.on_market_update(key, snapshot)
//!                     ↓ [check consecutive moves]
//!              Exit condition met → flatten_tx.try_send()
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, info, trace, warn};

use hip3_core::types::MarketSnapshot;
use hip3_core::{MarketKey, OrderSide, PendingOrder};
use hip3_feed::OracleMovementTracker;

use crate::time_stop::FlattenOrderBuilder;
use crate::tracker::{Position, PositionTrackerHandle};

// ============================================================================
// Oracle Baseline
// ============================================================================

/// Baseline oracle consecutive counts at position entry time.
///
/// Used to calculate delta (movements since entry) rather than
/// using global market consecutive counts which include pre-entry movements.
#[derive(Debug, Clone, Copy)]
struct OracleBaseline {
    /// Consecutive count moving WITH our position side at entry.
    consecutive_with: u32,
    /// Consecutive count moving AGAINST our position side at entry.
    consecutive_against: u32,
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for oracle-driven exit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleExitConfig {
    /// Enable oracle-driven exit.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Loss cut: exit when oracle moves against position N consecutive times.
    /// Lower = faster loss cut, higher = more tolerance for noise.
    /// Recommended: 2-3 based on data analysis.
    #[serde(default = "default_exit_against_moves")]
    pub exit_against_moves: u32,

    /// Profit take: exit when oracle moves with position N consecutive times.
    /// This indicates MMs are catching up and edge is narrowing.
    /// Recommended: 3-4 based on data analysis.
    #[serde(default = "default_exit_with_moves")]
    pub exit_with_moves: u32,

    /// Minimum holding time before oracle exit is considered (ms).
    /// Prevents exit on initial noise after entry.
    #[serde(default = "default_min_holding_time_ms")]
    pub min_holding_time_ms: u64,

    /// Slippage in basis points for flatten orders.
    #[serde(default = "default_slippage_bps")]
    pub slippage_bps: u64,
}

fn default_enabled() -> bool {
    true
}

fn default_exit_against_moves() -> u32 {
    2 // Quick loss cut
}

fn default_exit_with_moves() -> u32 {
    3 // Wait for MM catch-up confirmation
}

fn default_min_holding_time_ms() -> u64 {
    1000 // 1 second minimum
}

fn default_slippage_bps() -> u64 {
    50 // 50 bps slippage for flatten
}

impl Default for OracleExitConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            exit_against_moves: default_exit_against_moves(),
            exit_with_moves: default_exit_with_moves(),
            min_holding_time_ms: default_min_holding_time_ms(),
            slippage_bps: default_slippage_bps(),
        }
    }
}

// ============================================================================
// Exit Reason
// ============================================================================

/// Reason for oracle-driven exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleExitReason {
    /// Oracle moved against position (loss cut).
    /// The edge has disappeared - cut losses quickly.
    OracleReversal {
        /// Number of consecutive moves against position.
        moves: u32,
    },

    /// Oracle moved with position (profit take).
    /// MMs are catching up - take profit before spread compresses.
    OracleCatchup {
        /// Number of consecutive moves with position.
        moves: u32,
    },
}

impl std::fmt::Display for OracleExitReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OracleReversal { moves } => {
                write!(f, "OracleReversal({} moves against)", moves)
            }
            Self::OracleCatchup { moves } => {
                write!(f, "OracleCatchup({} moves with)", moves)
            }
        }
    }
}

// ============================================================================
// OracleExitWatcher
// ============================================================================

/// Oracle-driven exit watcher.
///
/// Monitors oracle price movements to determine optimal exit timing.
/// Called from WS message handler for sub-millisecond latency.
pub struct OracleExitWatcher {
    /// Configuration.
    config: OracleExitConfig,

    /// Handle to position tracker for position lookups.
    position_handle: PositionTrackerHandle,

    /// Oracle movement tracker for consecutive move detection.
    oracle_tracker: Arc<OracleMovementTracker>,

    /// Channel to send flatten orders (non-blocking try_send).
    flatten_tx: mpsc::Sender<PendingOrder>,

    /// Local tracking of markets with pending flatten orders.
    local_flattening: RwLock<HashSet<MarketKey>>,

    /// Counter for reversal exits (metrics).
    reversal_count: AtomicU64,

    /// Counter for catchup exits (metrics).
    catchup_count: AtomicU64,
}

impl OracleExitWatcher {
    /// Create a new OracleExitWatcher.
    #[must_use]
    pub fn new(
        config: OracleExitConfig,
        position_handle: PositionTrackerHandle,
        oracle_tracker: Arc<OracleMovementTracker>,
        flatten_tx: mpsc::Sender<PendingOrder>,
    ) -> Self {
        info!(
            enabled = config.enabled,
            exit_against_moves = config.exit_against_moves,
            exit_with_moves = config.exit_with_moves,
            min_holding_time_ms = config.min_holding_time_ms,
            slippage_bps = config.slippage_bps,
            "OracleExitWatcher initialized"
        );

        Self {
            config,
            position_handle,
            oracle_tracker,
            flatten_tx,
            local_flattening: RwLock::new(HashSet::new()),
            reversal_count: AtomicU64::new(0),
            catchup_count: AtomicU64::new(0),
        }
    }

    /// Called when market data is updated.
    ///
    /// This is the main entry point, called from `App::handle_market_event()`
    /// after `oracle_tracker.record_move()` has been called.
    ///
    /// # Performance
    ///
    /// Designed to be fast and non-blocking:
    /// - O(1) position lookup
    /// - O(1) consecutive count lookup
    /// - Non-blocking `try_send` for flatten orders
    pub fn on_market_update(&self, key: MarketKey, snapshot: &MarketSnapshot) {
        // 0. Check if enabled
        if !self.config.enabled {
            return;
        }

        // 1. Fast path: Check if we have a position in this market
        let position = match self.position_handle.get_position(&key) {
            Some(p) => p,
            None => return,
        };

        // 2. Check if already flattening
        {
            let flattening = self.local_flattening.read();
            if flattening.contains(&key) {
                trace!(market = %key, "OracleExitWatcher: already flattening (local)");
                return;
            }
        }

        // 3. Check if flatten order pending in position tracker
        if self.position_handle.is_flattening(&key) {
            trace!(market = %key, "OracleExitWatcher: already flattening (tracker)");
            return;
        }

        // 4. Check minimum holding time
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let held_ms = now_ms.saturating_sub(position.entry_timestamp_ms);
        if held_ms < self.config.min_holding_time_ms {
            return;
        }

        // 5. Check oracle-based exit condition
        if let Some(reason) = self.check_oracle_exit(&position) {
            // 6. Mark as flattening BEFORE sending to prevent duplicates
            {
                let mut flattening = self.local_flattening.write();
                flattening.insert(key);
            }

            // 7. Trigger exit
            self.trigger_exit(&position, reason, snapshot, now_ms);
        }
    }

    /// Check if position should exit based on oracle movements.
    fn check_oracle_exit(&self, position: &Position) -> Option<OracleExitReason> {
        let key = &position.market;

        // Check for loss cut: oracle moving against us
        let against = self.oracle_tracker.consecutive_against(key, position.side);
        if against >= self.config.exit_against_moves {
            return Some(OracleExitReason::OracleReversal { moves: against });
        }

        // Check for profit take: oracle moving with us (MMs catching up)
        let with = self.oracle_tracker.consecutive_with(key, position.side);
        if with >= self.config.exit_with_moves {
            return Some(OracleExitReason::OracleCatchup { moves: with });
        }

        None
    }

    /// Trigger exit for a position.
    fn trigger_exit(
        &self,
        position: &Position,
        reason: OracleExitReason,
        snapshot: &MarketSnapshot,
        now_ms: u64,
    ) {
        // Use BBO for flatten order pricing
        let price = match position.side {
            OrderSide::Buy => snapshot.bbo.bid_price,  // Sell at bid
            OrderSide::Sell => snapshot.bbo.ask_price, // Buy at ask
        };

        // Create reduce-only order
        let order = FlattenOrderBuilder::create_flatten_order(
            position,
            price,
            self.config.slippage_bps,
            now_ms,
        );

        let held_ms = now_ms.saturating_sub(position.entry_timestamp_ms);

        // Update counters
        match reason {
            OracleExitReason::OracleReversal { .. } => {
                self.reversal_count.fetch_add(1, Ordering::Relaxed);
            }
            OracleExitReason::OracleCatchup { .. } => {
                self.catchup_count.fetch_add(1, Ordering::Relaxed);
            }
        }

        info!(
            market = %position.market,
            side = ?position.side,
            reason = %reason,
            held_ms = held_ms,
            cloid = %order.cloid,
            reversal_count = self.reversal_count.load(Ordering::Relaxed),
            catchup_count = self.catchup_count.load(Ordering::Relaxed),
            "OracleExitWatcher: exit triggered"
        );

        // Non-blocking send
        match self.flatten_tx.try_send(order) {
            Ok(()) => {
                debug!(market = %position.market, "OracleExitWatcher: flatten order sent");
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    market = %position.market,
                    "OracleExitWatcher: flatten channel full"
                );
                // Clear local_flattening so other watchers can try
                let mut flattening = self.local_flattening.write();
                flattening.remove(&position.market);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                warn!("OracleExitWatcher: flatten channel closed");
            }
        }
    }

    /// Clear local flattening state for a market.
    pub fn clear_flattening(&self, market: &MarketKey) {
        let mut flattening = self.local_flattening.write();
        if flattening.remove(market) {
            debug!(market = %market, "OracleExitWatcher: cleared flattening state");
        }
    }

    /// Sync local flattening state with position tracker.
    pub fn sync_flattening_state(&self) {
        let positions = self.position_handle.positions_snapshot();
        let position_markets: HashSet<MarketKey> = positions.iter().map(|p| p.market).collect();

        let mut flattening = self.local_flattening.write();

        // Remove markets with no position
        flattening.retain(|m| position_markets.contains(m));

        // Remove markets where flatten order was completed/rejected
        flattening.retain(|m| self.position_handle.is_flattening(m));
    }

    /// Get metrics.
    #[must_use]
    pub fn metrics(&self) -> OracleExitMetrics {
        OracleExitMetrics {
            reversal_count: self.reversal_count.load(Ordering::Relaxed),
            catchup_count: self.catchup_count.load(Ordering::Relaxed),
        }
    }

    /// Check if oracle exit is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

/// Metrics for oracle-driven exits.
#[derive(Debug, Clone, Copy, Default)]
pub struct OracleExitMetrics {
    /// Number of exits due to oracle reversal (loss cut).
    pub reversal_count: u64,
    /// Number of exits due to oracle catchup (profit take).
    pub catchup_count: u64,
}

impl OracleExitMetrics {
    /// Total number of oracle-driven exits.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.reversal_count + self.catchup_count
    }
}

// ============================================================================
// Handle (Arc wrapper)
// ============================================================================

/// Thread-safe handle to OracleExitWatcher.
pub type OracleExitWatcherHandle = Arc<OracleExitWatcher>;

/// Create a new OracleExitWatcherHandle.
#[must_use]
pub fn new_oracle_exit_watcher(
    config: OracleExitConfig,
    position_handle: PositionTrackerHandle,
    oracle_tracker: Arc<OracleMovementTracker>,
    flatten_tx: mpsc::Sender<PendingOrder>,
) -> OracleExitWatcherHandle {
    Arc::new(OracleExitWatcher::new(
        config,
        position_handle,
        oracle_tracker,
        flatten_tx,
    ))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = OracleExitConfig::default();
        assert!(config.enabled);
        assert_eq!(config.exit_against_moves, 2);
        assert_eq!(config.exit_with_moves, 3);
        assert_eq!(config.min_holding_time_ms, 1000);
        assert_eq!(config.slippage_bps, 50);
    }

    #[test]
    fn test_exit_reason_display() {
        let reversal = OracleExitReason::OracleReversal { moves: 3 };
        assert_eq!(format!("{}", reversal), "OracleReversal(3 moves against)");

        let catchup = OracleExitReason::OracleCatchup { moves: 4 };
        assert_eq!(format!("{}", catchup), "OracleCatchup(4 moves with)");
    }

    #[test]
    fn test_metrics_total() {
        let metrics = OracleExitMetrics {
            reversal_count: 5,
            catchup_count: 3,
        };
        assert_eq!(metrics.total(), 8);
    }
}
