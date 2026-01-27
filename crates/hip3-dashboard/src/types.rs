//! Dashboard API types.
//!
//! These types are used for JSON serialization in REST and WebSocket APIs.

use std::collections::HashMap;

use rust_decimal::Decimal;
use serde::Serialize;

/// Full dashboard state snapshot (sent on initial connection and via REST).
#[derive(Debug, Clone, Serialize)]
pub struct DashboardSnapshot {
    /// Timestamp when snapshot was taken (Unix milliseconds).
    pub timestamp_ms: i64,
    /// Market data by market key.
    pub markets: HashMap<String, MarketDataSnapshot>,
    /// Current positions.
    pub positions: Vec<PositionSnapshot>,
    /// Number of pending orders.
    pub pending_orders: usize,
    /// Risk status.
    pub risk: RiskStatus,
    /// Recent signals (newest first).
    pub recent_signals: Vec<SignalSnapshot>,
}

/// Market data snapshot for a single market.
#[derive(Debug, Clone, Serialize)]
pub struct MarketDataSnapshot {
    /// Market key (e.g., "BTC-PERP").
    pub market_key: String,
    /// Best bid price.
    pub bid_price: Option<Decimal>,
    /// Best bid size.
    pub bid_size: Option<Decimal>,
    /// Best ask price.
    pub ask_price: Option<Decimal>,
    /// Best ask size.
    pub ask_size: Option<Decimal>,
    /// Spread in basis points.
    pub spread_bps: Option<f64>,
    /// Oracle price.
    pub oracle_price: Option<Decimal>,
    /// Mark price.
    pub mark_price: Option<Decimal>,
    /// Buy edge: (oracle - best_ask) / oracle * 10000 bps.
    /// Positive = ask is cheap vs oracle (buy opportunity).
    pub buy_edge_bps: Option<f64>,
    /// Sell edge: (best_bid - oracle) / oracle * 10000 bps.
    /// Positive = bid is expensive vs oracle (sell opportunity).
    pub sell_edge_bps: Option<f64>,
    /// BBO age in milliseconds.
    pub bbo_age_ms: Option<i64>,
    /// Oracle age in milliseconds.
    pub oracle_age_ms: Option<i64>,
}

/// Position snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct PositionSnapshot {
    /// Market key.
    pub market_key: String,
    /// Side: "long" or "short".
    pub side: String,
    /// Position size.
    pub size: Decimal,
    /// Entry price.
    pub entry_price: Decimal,
    /// Current mark price.
    pub mark_price: Option<Decimal>,
    /// Unrealized P&L.
    pub unrealized_pnl: Option<Decimal>,
    /// Unrealized P&L in basis points.
    pub unrealized_pnl_bps: Option<f64>,
    /// Time in position (milliseconds).
    pub hold_time_ms: u64,
}

/// Risk status summary.
#[derive(Debug, Clone, Serialize)]
pub struct RiskStatus {
    /// Hard stop latch triggered.
    pub hard_stop_triggered: bool,
    /// Hard stop reason (if triggered).
    pub hard_stop_reason: Option<String>,
    /// Time since hard stop trigger (milliseconds).
    pub hard_stop_elapsed_ms: Option<u64>,
    /// Active gate blocks (gate name -> reason).
    pub gate_blocks: HashMap<String, String>,
    /// Overall trading allowed status.
    pub trading_allowed: bool,
}

/// Signal snapshot for display.
#[derive(Debug, Clone, Serialize)]
pub struct SignalSnapshot {
    /// Signal timestamp (Unix milliseconds).
    pub timestamp_ms: i64,
    /// Market key.
    pub market_key: String,
    /// Side: "buy" or "sell".
    pub side: String,
    /// Raw edge in basis points.
    pub raw_edge_bps: f64,
    /// Net edge in basis points.
    pub net_edge_bps: f64,
    /// Oracle price.
    pub oracle_price: f64,
    /// Best price (bid or ask).
    pub best_price: f64,
    /// Best size available.
    pub best_size: f64,
    /// Suggested trade size.
    pub suggested_size: f64,
    /// Signal ID.
    pub signal_id: String,
}

/// WebSocket message types (tagged enum for type safety).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DashboardMessage {
    /// Full snapshot (sent on connect).
    Snapshot(DashboardSnapshot),
    /// Incremental update.
    Update {
        /// Update timestamp.
        timestamp_ms: i64,
        /// Updated market data (only changed markets).
        #[serde(skip_serializing_if = "Option::is_none")]
        markets: Option<HashMap<String, MarketDataSnapshot>>,
        /// Updated positions (full list if changed).
        #[serde(skip_serializing_if = "Option::is_none")]
        positions: Option<Vec<PositionSnapshot>>,
        /// Updated risk status (if changed).
        #[serde(skip_serializing_if = "Option::is_none")]
        risk: Option<RiskStatus>,
        /// Updated pending orders count (if changed).
        #[serde(skip_serializing_if = "Option::is_none")]
        pending_orders: Option<usize>,
    },
    /// New signal detected.
    Signal(SignalSnapshot),
    /// Risk alert.
    RiskAlert {
        /// Alert timestamp.
        timestamp_ms: i64,
        /// Alert type.
        alert_type: RiskAlertType,
        /// Alert message.
        message: String,
    },
}

/// Risk alert types.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskAlertType {
    /// Hard stop triggered.
    HardStop,
    /// Risk gate triggered.
    GateTriggered,
    /// Spread exceeded threshold.
    SpreadExceeded,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_serialization() {
        let snapshot = DashboardSnapshot {
            timestamp_ms: 1706400000000,
            markets: HashMap::new(),
            positions: vec![],
            pending_orders: 0,
            risk: RiskStatus {
                hard_stop_triggered: false,
                hard_stop_reason: None,
                hard_stop_elapsed_ms: None,
                gate_blocks: HashMap::new(),
                trading_allowed: true,
            },
            recent_signals: vec![],
        };

        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("\"timestamp_ms\":1706400000000"));
        assert!(json.contains("\"trading_allowed\":true"));
    }

    #[test]
    fn test_message_tagging() {
        let msg = DashboardMessage::RiskAlert {
            timestamp_ms: 1706400000000,
            alert_type: RiskAlertType::HardStop,
            message: "Test alert".to_string(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"risk_alert\""));
        assert!(json.contains("\"alert_type\":\"hard_stop\""));
    }
}
