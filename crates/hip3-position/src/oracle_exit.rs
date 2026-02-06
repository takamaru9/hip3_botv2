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

use rust_decimal::Decimal;

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
    /// P2-5: Entry edge in basis points (for dynamic exit thresholds).
    /// None if unknown (e.g., position opened before feature was enabled).
    entry_edge_bps: Option<Decimal>,
    /// P3-2: Oracle price at entry time (for trailing stop PnL calculation).
    /// None if unknown (e.g., position synced at startup without oracle data).
    entry_oracle_px: Option<Decimal>,
    /// P3-2: Best favorable PnL in bps since entry (for trailing stop).
    best_pnl_bps: Decimal,
    /// P3-2: Whether trailing stop has been activated.
    trail_activated: bool,
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

    /// P2-5: Enable dynamic exit thresholds based on entry edge.
    /// When enabled, high-edge entries get more tolerance (exit_against_moves + 1),
    /// while low-edge entries keep the default (faster loss cut).
    #[serde(default)]
    pub dynamic_thresholds: bool,

    /// P2-5: Entry edge threshold for "high edge" classification (bps).
    /// Entries above this get exit_against_moves + 1 for loss cut.
    #[serde(default = "default_high_edge_bps")]
    pub high_edge_bps: u32,

    /// P2-5: Entry edge threshold for "low edge" classification (bps).
    /// Entries below this keep default exit_against_moves (fastest loss cut).
    #[serde(default = "default_low_edge_bps")]
    pub low_edge_bps: u32,

    /// P3-2: Enable trailing stop exit.
    ///
    /// When enabled, tracks the best oracle PnL since entry.
    /// After oracle moves `activation_bps` in our favor, trailing stop activates.
    /// If oracle then retraces `trail_bps` from the best, exit is triggered.
    ///
    /// This replaces fixed `exit_with_moves` profit-take with dynamic trailing.
    #[serde(default)]
    pub trailing_stop: bool,

    /// P3-2: Trailing stop activation threshold (bps).
    /// Trailing stop activates after oracle PnL reaches this level.
    #[serde(default = "default_activation_bps")]
    pub activation_bps: u32,

    /// P3-2: Trailing stop trail distance (bps).
    /// Exit when oracle retraces this much from best PnL since entry.
    #[serde(default = "default_trail_bps")]
    pub trail_bps: u32,
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

fn default_high_edge_bps() -> u32 {
    40 // High edge: > 40 bps
}

fn default_low_edge_bps() -> u32 {
    25 // Low edge: < 25 bps
}

fn default_activation_bps() -> u32 {
    5 // Activate trailing stop after 5 bps favorable oracle move
}

fn default_trail_bps() -> u32 {
    3 // Exit after 3 bps retrace from best oracle PnL
}

impl Default for OracleExitConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            exit_against_moves: default_exit_against_moves(),
            exit_with_moves: default_exit_with_moves(),
            min_holding_time_ms: default_min_holding_time_ms(),
            slippage_bps: default_slippage_bps(),
            dynamic_thresholds: false,
            high_edge_bps: default_high_edge_bps(),
            low_edge_bps: default_low_edge_bps(),
            trailing_stop: false,
            activation_bps: default_activation_bps(),
            trail_bps: default_trail_bps(),
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

    /// P3-2: Trailing stop triggered.
    /// Oracle reached activation threshold, then retraced beyond trail distance.
    TrailingStop {
        /// Best PnL in bps reached since entry.
        best_pnl_bps: Decimal,
        /// Current PnL in bps when trail triggered.
        current_pnl_bps: Decimal,
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
            Self::TrailingStop {
                best_pnl_bps,
                current_pnl_bps,
            } => {
                write!(
                    f,
                    "TrailingStop(best={} bps, current={} bps)",
                    best_pnl_bps, current_pnl_bps
                )
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
///
/// # Position Baseline Tracking
///
/// To correctly measure oracle movements "since entry", this watcher stores
/// the baseline consecutive counts at position open time. Exit conditions
/// are evaluated using the delta from baseline, not the global count.
///
/// Example:
/// - Market had 3 consecutive DOWN moves before entry
/// - Bot opens LONG position
/// - Baseline stored: { consecutive_against: 3, consecutive_with: 0 }
/// - After 2 more DOWN moves: current = 5, delta = 5 - 3 = 2
/// - Exit triggers when delta >= exit_against_moves (not when global >= N)
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

    /// Baseline consecutive counts at position entry time.
    /// Key: MarketKey, Value: OracleBaseline
    ///
    /// This prevents immediate false exits when the market already had
    /// consecutive moves before our position was opened.
    position_baselines: RwLock<HashMap<MarketKey, OracleBaseline>>,

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
            position_baselines: RwLock::new(HashMap::new()),
            reversal_count: AtomicU64::new(0),
            catchup_count: AtomicU64::new(0),
        }
    }

    /// Record baseline consecutive counts when a new position is opened.
    ///
    /// This MUST be called when a position is created to prevent false exits.
    /// Without baseline tracking, if the market already had N consecutive moves
    /// against the position side, exit would trigger immediately.
    ///
    /// # Arguments
    /// * `key` - Market key
    /// * `side` - Position side (Buy = long, Sell = short)
    /// * `entry_edge_bps` - P2-5: Entry edge in basis points (None if unknown)
    /// * `entry_oracle_px` - P3-2: Oracle price at entry (None if unknown, e.g. startup sync)
    pub fn on_position_opened(
        &self,
        key: MarketKey,
        side: OrderSide,
        entry_edge_bps: Option<Decimal>,
        entry_oracle_px: Option<Decimal>,
    ) {
        let consecutive_with = self.oracle_tracker.consecutive_with(&key, side);
        let consecutive_against = self.oracle_tracker.consecutive_against(&key, side);

        let baseline = OracleBaseline {
            consecutive_with,
            consecutive_against,
            entry_edge_bps,
            entry_oracle_px,
            best_pnl_bps: Decimal::ZERO,
            trail_activated: false,
        };

        debug!(
            market = %key,
            side = ?side,
            baseline_with = consecutive_with,
            baseline_against = consecutive_against,
            entry_edge_bps = ?entry_edge_bps,
            entry_oracle_px = ?entry_oracle_px,
            "OracleExitWatcher: recorded position baseline"
        );

        let mut baselines = self.position_baselines.write();
        baselines.insert(key, baseline);
    }

    /// Clear baseline when position is closed.
    ///
    /// Should be called when a position is fully closed to clean up state.
    pub fn on_position_closed(&self, key: &MarketKey) {
        let mut baselines = self.position_baselines.write();
        if baselines.remove(key).is_some() {
            debug!(market = %key, "OracleExitWatcher: cleared position baseline");
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

        // 5. P3-2: Update trailing stop state and check for trail exit
        if self.config.trailing_stop {
            if let Some(reason) = self.update_trailing_state(&key, &position, snapshot) {
                {
                    let mut flattening = self.local_flattening.write();
                    flattening.insert(key);
                }
                self.trigger_exit(&position, reason, snapshot, now_ms);
                return;
            }
        }

        // 6. Check oracle-based exit condition (consecutive moves)
        if let Some(reason) = self.check_oracle_exit(&position) {
            // 7. Mark as flattening BEFORE sending to prevent duplicates
            {
                let mut flattening = self.local_flattening.write();
                flattening.insert(key);
            }

            // 8. Trigger exit
            self.trigger_exit(&position, reason, snapshot, now_ms);
        }
    }

    /// P3-2: Update trailing stop state and check for trail exit.
    ///
    /// Updates best PnL tracking and checks activation/exit conditions.
    /// Returns exit reason if trailing stop fires.
    fn update_trailing_state(
        &self,
        key: &MarketKey,
        position: &Position,
        snapshot: &MarketSnapshot,
    ) -> Option<OracleExitReason> {
        let oracle_px = snapshot.ctx.oracle.oracle_px.inner();
        if oracle_px.is_zero() {
            return None;
        }

        let mut baselines = self.position_baselines.write();
        let baseline = baselines.get_mut(key)?;

        let entry_oracle = match baseline.entry_oracle_px {
            Some(px) if !px.is_zero() => px,
            _ => return None, // No entry oracle, can't track trail
        };

        // Calculate current PnL in bps
        let current_pnl_bps = match position.side {
            OrderSide::Buy => (oracle_px - entry_oracle) / entry_oracle * Decimal::from(10000),
            OrderSide::Sell => (entry_oracle - oracle_px) / entry_oracle * Decimal::from(10000),
        };

        // Update best PnL
        if current_pnl_bps > baseline.best_pnl_bps {
            baseline.best_pnl_bps = current_pnl_bps;
        }

        // Check activation
        if !baseline.trail_activated {
            if baseline.best_pnl_bps >= Decimal::from(self.config.activation_bps) {
                baseline.trail_activated = true;
                info!(
                    market = %key,
                    side = ?position.side,
                    best_pnl_bps = %baseline.best_pnl_bps,
                    activation_bps = self.config.activation_bps,
                    "OracleExitWatcher: trailing stop activated"
                );
            }
            return None; // Not yet activated
        }

        // Check trail exit: retrace from best
        let retrace_bps = baseline.best_pnl_bps - current_pnl_bps;
        if retrace_bps >= Decimal::from(self.config.trail_bps) {
            return Some(OracleExitReason::TrailingStop {
                best_pnl_bps: baseline.best_pnl_bps,
                current_pnl_bps,
            });
        }

        None
    }

    /// Check if position should exit based on oracle movements since entry.
    ///
    /// Uses delta from baseline (not global count) to correctly measure
    /// movements that occurred after position was opened.
    fn check_oracle_exit(&self, position: &Position) -> Option<OracleExitReason> {
        let key = &position.market;

        // Get baseline for this position
        let baseline = {
            let baselines = self.position_baselines.read();
            baselines.get(key).copied()
        };

        // If no baseline, position was opened before OracleExitWatcher started
        // or on_position_opened() wasn't called. Use conservative approach: no exit.
        let baseline = match baseline {
            Some(b) => b,
            None => {
                trace!(
                    market = %key,
                    "OracleExitWatcher: no baseline for position, skipping exit check"
                );
                return None;
            }
        };

        // Get current consecutive counts
        let current_against = self.oracle_tracker.consecutive_against(key, position.side);
        let current_with = self.oracle_tracker.consecutive_with(key, position.side);

        // Calculate delta (movements since entry)
        // Use saturating_sub in case oracle tracker was reset
        let delta_against = current_against.saturating_sub(baseline.consecutive_against);
        let delta_with = current_with.saturating_sub(baseline.consecutive_with);

        trace!(
            market = %key,
            side = ?position.side,
            baseline_against = baseline.consecutive_against,
            baseline_with = baseline.consecutive_with,
            current_against = current_against,
            current_with = current_with,
            delta_against = delta_against,
            delta_with = delta_with,
            "OracleExitWatcher: checking exit condition"
        );

        // P2-5: Compute effective exit_against_moves based on entry edge
        let effective_exit_against = if self.config.dynamic_thresholds {
            if let Some(edge) = baseline.entry_edge_bps {
                if edge >= Decimal::from(self.config.high_edge_bps) {
                    // High edge entry → more tolerant (allow extra move)
                    self.config.exit_against_moves + 1
                } else {
                    // Normal or low edge → use default (fast loss cut)
                    self.config.exit_against_moves
                }
            } else {
                // Unknown edge → use default
                self.config.exit_against_moves
            }
        } else {
            self.config.exit_against_moves
        };

        // Check for loss cut: oracle moving against us since entry
        if delta_against >= effective_exit_against {
            return Some(OracleExitReason::OracleReversal {
                moves: delta_against,
            });
        }

        // Check for profit take: oracle moving with us since entry (MMs catching up)
        if delta_with >= self.config.exit_with_moves {
            return Some(OracleExitReason::OracleCatchup { moves: delta_with });
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
            OracleExitReason::TrailingStop { .. } => {
                // Trailing stop counts as a catchup (profit-take variant)
                self.catchup_count.fetch_add(1, Ordering::Relaxed);
            }
        }

        // P1-4: Record exit metrics
        let exit_reason_str = match reason {
            OracleExitReason::OracleReversal { .. } => "OracleReversal",
            OracleExitReason::OracleCatchup { .. } => "OracleCatchup",
            OracleExitReason::TrailingStop { .. } => "TrailingStop",
        };
        let market_str = position.market.to_string();
        hip3_telemetry::Metrics::position_holding_time(
            &market_str,
            exit_reason_str,
            held_ms as f64,
        );

        // Estimate PnL in bps from entry price vs current oracle
        let oracle_px = snapshot.ctx.oracle.oracle_px.inner();
        if !position.entry_price.inner().is_zero() && !oracle_px.is_zero() {
            use rust_decimal::prelude::ToPrimitive;
            let pnl_bps = match position.side {
                OrderSide::Buy => {
                    (oracle_px - position.entry_price.inner()) / position.entry_price.inner()
                        * Decimal::from(10000)
                }
                OrderSide::Sell => {
                    (position.entry_price.inner() - oracle_px) / position.entry_price.inner()
                        * Decimal::from(10000)
                }
            };
            if let Some(pnl) = pnl_bps.to_f64() {
                hip3_telemetry::Metrics::trade_pnl(&market_str, exit_reason_str, pnl);
            }
        }

        info!(
            market = %position.market,
            side = ?position.side,
            reason = %reason,
            held_ms = held_ms,
            exit_reason = exit_reason_str,
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

    /// Sync local flattening state and baselines with position tracker.
    pub fn sync_flattening_state(&self) {
        let positions = self.position_handle.positions_snapshot();
        let position_markets: HashSet<MarketKey> = positions.iter().map(|p| p.market).collect();

        // Clean up flattening state
        {
            let mut flattening = self.local_flattening.write();

            // Remove markets with no position
            flattening.retain(|m| position_markets.contains(m));

            // Remove markets where flatten order was completed/rejected
            flattening.retain(|m| self.position_handle.is_flattening(m));
        }

        // Clean up baselines for closed positions
        {
            let mut baselines = self.position_baselines.write();
            let before_count = baselines.len();
            baselines.retain(|m, _| position_markets.contains(m));
            let removed = before_count - baselines.len();
            if removed > 0 {
                debug!(
                    removed = removed,
                    remaining = baselines.len(),
                    "OracleExitWatcher: cleaned up stale baselines"
                );
            }
        }
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
    use rust_decimal_macros::dec;

    #[test]
    fn test_default_config() {
        let config = OracleExitConfig::default();
        assert!(config.enabled);
        assert_eq!(config.exit_against_moves, 2);
        assert_eq!(config.exit_with_moves, 3);
        assert_eq!(config.min_holding_time_ms, 1000);
        assert_eq!(config.slippage_bps, 50);
        // P2-5 defaults
        assert!(!config.dynamic_thresholds);
        assert_eq!(config.high_edge_bps, 40);
        assert_eq!(config.low_edge_bps, 25);
        // P3-2 defaults
        assert!(!config.trailing_stop);
        assert_eq!(config.activation_bps, 5);
        assert_eq!(config.trail_bps, 3);
    }

    #[test]
    fn test_exit_reason_display() {
        let reversal = OracleExitReason::OracleReversal { moves: 3 };
        assert_eq!(format!("{}", reversal), "OracleReversal(3 moves against)");

        let catchup = OracleExitReason::OracleCatchup { moves: 4 };
        assert_eq!(format!("{}", catchup), "OracleCatchup(4 moves with)");

        let trail = OracleExitReason::TrailingStop {
            best_pnl_bps: dec!(8),
            current_pnl_bps: dec!(4),
        };
        assert_eq!(
            format!("{}", trail),
            "TrailingStop(best=8 bps, current=4 bps)"
        );
    }

    #[test]
    fn test_metrics_total() {
        let metrics = OracleExitMetrics {
            reversal_count: 5,
            catchup_count: 3,
        };
        assert_eq!(metrics.total(), 8);
    }

    #[test]
    fn test_dynamic_threshold_config_serde() {
        // Verify dynamic_thresholds defaults to false and doesn't break existing TOML
        let toml = r#"
            enabled = true
            exit_against_moves = 2
            exit_with_moves = 3
        "#;
        let config: OracleExitConfig = toml::from_str(toml).unwrap();
        assert!(!config.dynamic_thresholds);
        assert_eq!(config.high_edge_bps, 40);
        assert_eq!(config.low_edge_bps, 25);
    }

    #[test]
    fn test_dynamic_threshold_config_enabled() {
        let toml = r#"
            enabled = true
            exit_against_moves = 2
            exit_with_moves = 3
            dynamic_thresholds = true
            high_edge_bps = 50
            low_edge_bps = 20
        "#;
        let config: OracleExitConfig = toml::from_str(toml).unwrap();
        assert!(config.dynamic_thresholds);
        assert_eq!(config.high_edge_bps, 50);
        assert_eq!(config.low_edge_bps, 20);
    }

    #[test]
    fn test_oracle_baseline_with_entry_edge() {
        let baseline = OracleBaseline {
            consecutive_with: 0,
            consecutive_against: 1,
            entry_edge_bps: Some(Decimal::from(45)),
            entry_oracle_px: Some(dec!(100)),
            best_pnl_bps: Decimal::ZERO,
            trail_activated: false,
        };
        assert_eq!(baseline.entry_edge_bps, Some(Decimal::from(45)));
        assert_eq!(baseline.entry_oracle_px, Some(dec!(100)));
        assert!(!baseline.trail_activated);

        let baseline_no_edge = OracleBaseline {
            consecutive_with: 2,
            consecutive_against: 0,
            entry_edge_bps: None,
            entry_oracle_px: None,
            best_pnl_bps: Decimal::ZERO,
            trail_activated: false,
        };
        assert_eq!(baseline_no_edge.entry_edge_bps, None);
        assert_eq!(baseline_no_edge.entry_oracle_px, None);
    }

    #[test]
    fn test_trailing_stop_config_serde_backward_compat() {
        // Existing TOML without trailing_stop fields should work
        let toml = r#"
            enabled = true
            exit_against_moves = 2
            exit_with_moves = 3
        "#;
        let config: OracleExitConfig = toml::from_str(toml).unwrap();
        assert!(!config.trailing_stop);
        assert_eq!(config.activation_bps, 5);
        assert_eq!(config.trail_bps, 3);
    }

    #[test]
    fn test_trailing_stop_config_enabled() {
        let toml = r#"
            enabled = true
            exit_against_moves = 2
            exit_with_moves = 3
            trailing_stop = true
            activation_bps = 8
            trail_bps = 4
        "#;
        let config: OracleExitConfig = toml::from_str(toml).unwrap();
        assert!(config.trailing_stop);
        assert_eq!(config.activation_bps, 8);
        assert_eq!(config.trail_bps, 4);
    }
}
