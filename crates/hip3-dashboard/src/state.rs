//! Dashboard state management.
//!
//! DashboardState aggregates data from multiple sources for the dashboard.
//! Supports both Trading mode (full features) and Observation mode (market data only).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use chrono::Utc;
use parking_lot::RwLock;
use rust_decimal::Decimal;
use tokio::sync::mpsc;

use hip3_core::types::MarketSnapshot;
use hip3_core::{MarketKey, OrderSide};
use hip3_executor::HardStopLatch;
use hip3_feed::MarketState;
use hip3_persistence::SignalRecord;
use hip3_position::PositionTrackerHandle;

use crate::types::{
    CompletedTrade, DashboardSnapshot, MarketDataSnapshot, MarketPnlStats, PnlSummary,
    PositionSnapshot, RiskStatus, SignalSnapshot,
};

/// Sender type for pushing signals in real-time to the dashboard.
pub type SignalSender = mpsc::Sender<SignalSnapshot>;

/// Receiver type for the broadcaster to receive signals.
pub type SignalReceiver = mpsc::Receiver<SignalSnapshot>;

/// Dashboard state that aggregates data from multiple sources.
///
/// Supports two modes:
/// - Trading mode: Full features with positions, risk status, and signals
/// - Observation mode: Market data only (positions/risk are empty)
#[derive(Clone)]
pub struct DashboardState {
    /// Market state (BBO, oracle prices).
    market_state: Arc<MarketState>,
    /// Position tracker handle (None in Observation mode).
    position_tracker: Option<PositionTrackerHandle>,
    /// Hard stop latch (None in Observation mode).
    hard_stop_latch: Option<Arc<HardStopLatch>>,
    /// Recent signals buffer.
    recent_signals: Arc<RwLock<VecDeque<SignalRecord>>>,
    /// Gate block state (market, gate) -> blocked.
    gate_block_state: Arc<RwLock<HashMap<(MarketKey, String), bool>>>,
    /// Whether running in observation mode (limited features).
    observation_mode: bool,
    /// Signal sender for real-time signal push (cloneable for external use).
    signal_tx: SignalSender,
    /// Signal receiver wrapped in Arc<Mutex> for one-time extraction by broadcaster.
    signal_rx: Arc<tokio::sync::Mutex<Option<SignalReceiver>>>,
    /// P3-4: Completed trade history for PnL tracking.
    completed_trades: Arc<RwLock<VecDeque<CompletedTrade>>>,
}

impl DashboardState {
    /// Create a new dashboard state for Trading mode (full features).
    pub fn new(
        market_state: Arc<MarketState>,
        position_tracker: PositionTrackerHandle,
        hard_stop_latch: Arc<HardStopLatch>,
        recent_signals: Arc<RwLock<VecDeque<SignalRecord>>>,
    ) -> Self {
        // Create signal channel with buffer for burst handling
        let (signal_tx, signal_rx) = mpsc::channel::<SignalSnapshot>(64);
        Self {
            market_state,
            position_tracker: Some(position_tracker),
            hard_stop_latch: Some(hard_stop_latch),
            recent_signals,
            gate_block_state: Arc::new(RwLock::new(HashMap::new())),
            observation_mode: false,
            signal_tx,
            signal_rx: Arc::new(tokio::sync::Mutex::new(Some(signal_rx))),
            completed_trades: Arc::new(RwLock::new(VecDeque::new())),
        }
    }

    /// Create a new dashboard state for Observation mode (market data only).
    ///
    /// In Observation mode:
    /// - Positions will be empty
    /// - Risk status will show "observation mode"
    /// - Market data and signals will be available
    pub fn new_observation_mode(
        market_state: Arc<MarketState>,
        recent_signals: Arc<RwLock<VecDeque<SignalRecord>>>,
    ) -> Self {
        // Create signal channel with buffer for burst handling
        let (signal_tx, signal_rx) = mpsc::channel::<SignalSnapshot>(64);
        Self {
            market_state,
            position_tracker: None,
            hard_stop_latch: None,
            recent_signals,
            gate_block_state: Arc::new(RwLock::new(HashMap::new())),
            observation_mode: true,
            signal_tx,
            signal_rx: Arc::new(tokio::sync::Mutex::new(Some(signal_rx))),
            completed_trades: Arc::new(RwLock::new(VecDeque::new())),
        }
    }

    /// Get a clone of the signal sender for external use.
    ///
    /// Use this to send signals in real-time to the dashboard.
    /// The signal will be immediately broadcast to all connected WebSocket clients.
    pub fn signal_sender(&self) -> SignalSender {
        self.signal_tx.clone()
    }

    /// Take the signal receiver (can only be called once).
    ///
    /// This is used internally by run_server to pass the receiver to the broadcaster.
    pub async fn take_signal_receiver(&self) -> Option<SignalReceiver> {
        self.signal_rx.lock().await.take()
    }

    /// Update gate block state (called from main bot loop).
    pub fn update_gate_block(&self, market: MarketKey, gate: String, blocked: bool) {
        let mut state = self.gate_block_state.write();
        state.insert((market, gate), blocked);
    }

    /// Collect a full snapshot of the current state.
    pub fn collect_snapshot(&self) -> DashboardSnapshot {
        let timestamp_ms = Utc::now().timestamp_millis();

        // Collect market data
        let markets = self.collect_markets();

        // Collect positions (empty in Observation mode)
        let positions = self.collect_positions();

        // Collect pending orders count (0 in Observation mode)
        let pending_orders = self
            .position_tracker
            .as_ref()
            .map(|pt| pt.pending_order_count())
            .unwrap_or(0);

        // Collect risk status
        let risk = self.collect_risk_status();

        // Collect recent signals
        let recent_signals = self.collect_recent_signals();

        // P3-4: Collect PnL summary
        let pnl_summary = self.collect_pnl_summary(&positions);

        DashboardSnapshot {
            timestamp_ms,
            markets,
            positions,
            pending_orders,
            risk,
            recent_signals,
            pnl_summary,
        }
    }

    /// Collect market data snapshots.
    fn collect_markets(&self) -> HashMap<String, MarketDataSnapshot> {
        let mut markets = HashMap::new();

        for (market_key, snapshot) in self.market_state.all_snapshots() {
            let market_data = self.snapshot_to_market_data(&market_key, &snapshot);
            markets.insert(market_key.to_string(), market_data);
        }

        markets
    }

    /// Convert MarketSnapshot to MarketDataSnapshot.
    fn snapshot_to_market_data(
        &self,
        market_key: &MarketKey,
        snapshot: &MarketSnapshot,
    ) -> MarketDataSnapshot {
        let bbo = &snapshot.bbo;
        let ctx = &snapshot.ctx;

        // Calculate spread in basis points
        let mid = (bbo.bid_price.inner() + bbo.ask_price.inner()) / Decimal::from(2);
        let spread_bps = if mid.is_zero() {
            None
        } else {
            let spread = bbo.ask_price.inner() - bbo.bid_price.inner();
            Some(
                (spread / mid * Decimal::from(10000))
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )
        };

        // Calculate buy/sell edges matching signal detection logic
        let oracle = ctx.oracle.oracle_px.inner();
        let buy_edge_bps = if oracle.is_zero() {
            None
        } else {
            // Buy edge: (oracle - ask) / oracle
            // Positive means ask is cheap (buy opportunity)
            let edge = (oracle - bbo.ask_price.inner()) / oracle * Decimal::from(10000);
            Some(edge.to_string().parse::<f64>().unwrap_or(0.0))
        };
        let sell_edge_bps = if oracle.is_zero() {
            None
        } else {
            // Sell edge: (bid - oracle) / oracle
            // Positive means bid is expensive (sell opportunity)
            let edge = (bbo.bid_price.inner() - oracle) / oracle * Decimal::from(10000);
            Some(edge.to_string().parse::<f64>().unwrap_or(0.0))
        };

        // Get ages
        let bbo_age_ms = self.market_state.get_bbo_age_ms(market_key);
        let oracle_age_ms = self.market_state.get_oracle_age_ms(market_key);

        MarketDataSnapshot {
            market_key: market_key.to_string(),
            bid_price: Some(bbo.bid_price.inner()),
            bid_size: Some(bbo.bid_size.inner()),
            ask_price: Some(bbo.ask_price.inner()),
            ask_size: Some(bbo.ask_size.inner()),
            spread_bps,
            oracle_price: Some(ctx.oracle.oracle_px.inner()),
            mark_price: Some(ctx.oracle.mark_px.inner()),
            buy_edge_bps,
            sell_edge_bps,
            bbo_age_ms,
            oracle_age_ms,
        }
    }

    /// Collect position snapshots (empty in Observation mode).
    fn collect_positions(&self) -> Vec<PositionSnapshot> {
        let position_tracker = match &self.position_tracker {
            Some(pt) => pt,
            None => return Vec::new(), // Observation mode
        };

        let now_ms = Utc::now().timestamp_millis() as u64;
        let positions = position_tracker.positions_snapshot();

        positions
            .into_iter()
            .map(|pos| {
                // Get mark price for P&L calculation
                let mark_price = self
                    .market_state
                    .get_ctx(&pos.market)
                    .map(|ctx| ctx.oracle.mark_px.inner());

                // Calculate unrealized P&L
                let (unrealized_pnl, unrealized_pnl_bps) = match mark_price {
                    Some(mark) => {
                        let entry = pos.entry_price.inner();
                        let size = pos.size.inner();
                        let pnl = match pos.side {
                            OrderSide::Buy => (mark - entry) * size,
                            OrderSide::Sell => (entry - mark) * size,
                        };
                        let pnl_bps = if entry.is_zero() {
                            0.0
                        } else {
                            let bps = (mark - entry) / entry * Decimal::from(10000);
                            let bps_f64 = bps.to_string().parse::<f64>().unwrap_or(0.0);
                            match pos.side {
                                OrderSide::Buy => bps_f64,
                                OrderSide::Sell => -bps_f64,
                            }
                        };
                        (Some(pnl), Some(pnl_bps))
                    }
                    None => (None, None),
                };

                let hold_time_ms = now_ms.saturating_sub(pos.entry_timestamp_ms);

                PositionSnapshot {
                    market_key: pos.market.to_string(),
                    side: match pos.side {
                        OrderSide::Buy => "long".to_string(),
                        OrderSide::Sell => "short".to_string(),
                    },
                    size: pos.size.inner(),
                    entry_price: pos.entry_price.inner(),
                    mark_price,
                    unrealized_pnl,
                    unrealized_pnl_bps,
                    hold_time_ms,
                }
            })
            .collect()
    }

    /// Collect risk status (limited info in Observation mode).
    fn collect_risk_status(&self) -> RiskStatus {
        // In Observation mode, return safe defaults
        let (hard_stop_triggered, hard_stop_reason, hard_stop_elapsed_ms) =
            match &self.hard_stop_latch {
                Some(latch) => (
                    latch.is_triggered(),
                    latch.trigger_reason(),
                    latch.elapsed_since_trigger().map(|d| d.as_millis() as u64),
                ),
                None => (false, None, None), // Observation mode
            };

        // Collect gate blocks
        let gate_blocks = {
            let state = self.gate_block_state.read();
            state
                .iter()
                .filter(|(_, &blocked)| blocked)
                .map(|((market, gate), _)| (format!("{}:{}", market, gate), "blocked".to_string()))
                .collect()
        };

        // In Observation mode, trading is not applicable (show as allowed for display)
        let trading_allowed = !hard_stop_triggered;

        RiskStatus {
            hard_stop_triggered,
            hard_stop_reason: if self.observation_mode && hard_stop_reason.is_none() {
                Some("Observation mode - trading disabled".to_string())
            } else {
                hard_stop_reason
            },
            hard_stop_elapsed_ms,
            gate_blocks,
            trading_allowed,
        }
    }

    /// Collect recent signals.
    fn collect_recent_signals(&self) -> Vec<SignalSnapshot> {
        let signals = self.recent_signals.read();
        signals
            .iter()
            .rev() // Newest first
            .take(20) // Limit for dashboard
            .map(|s| SignalSnapshot {
                timestamp_ms: s.timestamp_ms,
                market_key: s.market_key.clone(),
                side: s.side.clone(),
                raw_edge_bps: s.raw_edge_bps,
                net_edge_bps: s.net_edge_bps,
                oracle_price: s.oracle_px,
                best_price: s.best_px,
                best_size: s.best_size,
                suggested_size: s.suggested_size,
                signal_id: s.signal_id.clone(),
            })
            .collect()
    }

    /// P3-4: Report a completed trade for PnL tracking.
    pub fn report_completed_trade(&self, trade: CompletedTrade) {
        let mut trades = self.completed_trades.write();
        trades.push_back(trade);
        // Keep at most 500 trades in memory.
        while trades.len() > 500 {
            trades.pop_front();
        }
    }

    /// P3-4: Collect PnL summary from completed trades and open positions.
    fn collect_pnl_summary(&self, positions: &[PositionSnapshot]) -> PnlSummary {
        let trades = self.completed_trades.read();

        // Compute session realized PnL.
        let session_realized_pnl: f64 = trades.iter().map(|t| t.pnl).sum();
        let total_trades = trades.len() as u32;
        let winning_trades = trades.iter().filter(|t| t.pnl > 0.0).count() as u32;
        let losing_trades = total_trades - winning_trades;
        let win_rate_pct = if total_trades > 0 {
            winning_trades as f64 / total_trades as f64 * 100.0
        } else {
            0.0
        };

        // Compute per-market stats.
        let mut market_map: HashMap<String, MarketPnlStats> = HashMap::new();
        for trade in trades.iter() {
            let stats = market_map
                .entry(trade.market.clone())
                .or_insert_with(|| MarketPnlStats {
                    market: trade.market.clone(),
                    ..Default::default()
                });
            stats.realized_pnl += trade.pnl;
            stats.trade_count += 1;
            if trade.pnl > 0.0 {
                stats.win_count += 1;
            } else {
                stats.loss_count += 1;
            }
        }
        let mut market_stats: Vec<MarketPnlStats> = market_map.into_values().collect();
        market_stats.sort_by(|a, b| b.trade_count.cmp(&a.trade_count));

        // Current unrealized PnL from open positions.
        let current_unrealized_pnl: f64 = positions
            .iter()
            .filter_map(|p| {
                p.unrealized_pnl
                    .map(|pnl| pnl.to_string().parse::<f64>().unwrap_or(0.0))
            })
            .sum();

        // Recent trades (newest first, max 50).
        let recent_trades: Vec<CompletedTrade> = trades.iter().rev().take(50).cloned().collect();

        PnlSummary {
            session_realized_pnl,
            current_unrealized_pnl,
            total_pnl: session_realized_pnl + current_unrealized_pnl,
            total_trades,
            winning_trades,
            losing_trades,
            win_rate_pct,
            market_stats,
            recent_trades,
        }
    }

    /// Check if hard stop is triggered (for alerts).
    /// Returns false in Observation mode (no hard stop mechanism).
    pub fn is_hard_stop_triggered(&self) -> bool {
        self.hard_stop_latch
            .as_ref()
            .map(|l| l.is_triggered())
            .unwrap_or(false)
    }

    /// Get hard stop reason.
    /// Returns None in Observation mode.
    pub fn get_hard_stop_reason(&self) -> Option<String> {
        self.hard_stop_latch
            .as_ref()
            .and_then(|l| l.trigger_reason())
    }

    /// Check if running in observation mode.
    pub fn is_observation_mode(&self) -> bool {
        self.observation_mode
    }
}

impl std::fmt::Debug for DashboardState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hard_stop_triggered = self
            .hard_stop_latch
            .as_ref()
            .map(|l| l.is_triggered())
            .unwrap_or(false);
        let position_count = self
            .position_tracker
            .as_ref()
            .map(|pt| pt.positions_snapshot().len())
            .unwrap_or(0);

        f.debug_struct("DashboardState")
            .field("observation_mode", &self.observation_mode)
            .field("hard_stop_triggered", &hard_stop_triggered)
            .field("position_count", &position_count)
            .finish()
    }
}
