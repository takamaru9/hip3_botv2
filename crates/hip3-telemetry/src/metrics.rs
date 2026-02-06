//! Prometheus metrics for HIP-3 bot.
//!
//! Provides observability from Day 1 for:
//! - Connection state
//! - Feed latency
//! - Signal detection
//! - Risk gate blocks
//! - Rate limiting (P0-8)
//! - Cross judgment (P0-8)
//!
//! # Panics
//!
//! Metric registration uses `unwrap()` intentionally. If registration fails,
//! it indicates a fatal configuration error (e.g., duplicate metric names)
//! that should cause an immediate crash at startup rather than silent failure.
//! These panics only occur during static initialization, never at runtime.

use once_cell::sync::Lazy;
use prometheus::{
    register_counter_vec, register_gauge, register_gauge_vec, register_histogram_vec,
    register_int_gauge, CounterVec, Gauge, GaugeVec, HistogramVec, IntGauge,
};

/// WebSocket connection state (1 = connected, 0 = disconnected).
pub static WS_CONNECTED: Lazy<Gauge> = Lazy::new(|| {
    register_gauge!(
        "hip3_ws_connected",
        "WebSocket connection state (1=connected)"
    )
    .unwrap()
});

/// WebSocket state machine current state.
/// Labels: state (disconnected/connecting/connected/ready/reconnecting)
pub static WS_STATE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "hip3_ws_state",
        "WebSocket state machine current state (1=active, 0=inactive)",
        &["state"]
    )
    .unwrap()
});

/// Total WebSocket reconnection attempts.
pub static WS_RECONNECT_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "hip3_ws_reconnect_total",
        "Total WebSocket reconnection attempts",
        &["reason"]
    )
    .unwrap()
});

/// Feed latency in milliseconds.
pub static FEED_LATENCY_MS: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "hip3_feed_latency_ms",
        "Feed message latency in milliseconds",
        &["channel"],
        vec![0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0]
    )
    .unwrap()
});

/// Total signals triggered.
pub static TRIGGERS_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "hip3_triggers_total",
        "Total dislocation signals triggered",
        &["market_key", "side", "strength"]
    )
    .unwrap()
});

/// Edge distribution in basis points.
pub static EDGE_BPS: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "hip3_edge_bps",
        "Edge distribution in basis points",
        &["market_key", "side"],
        vec![1.0, 2.0, 5.0, 10.0, 15.0, 20.0, 30.0, 50.0, 100.0]
    )
    .unwrap()
});

/// Risk gate block count.
pub static GATE_BLOCKED_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "hip3_gate_blocked_total",
        "Total risk gate blocks",
        &["gate", "market_key"]
    )
    .unwrap()
});

/// Risk gate block duration in milliseconds (recorded when block ends).
pub static GATE_BLOCK_DURATION_MS: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "hip3_gate_block_duration_ms",
        "Duration of continuous gate block periods in milliseconds",
        &["gate", "market_key"],
        vec![10.0, 50.0, 100.0, 500.0, 1000.0, 5000.0, 10000.0, 30000.0, 60000.0]
    )
    .unwrap()
});

/// Oracle stale rate (fraction of time oracle is stale).
pub static ORACLE_STALE_RATE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "hip3_oracle_stale_rate",
        "Fraction of time oracle is stale",
        &["market_key"]
    )
    .unwrap()
});

/// Mark-mid gap in basis points.
pub static MARK_MID_GAP_BPS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "hip3_mark_mid_gap_bps",
        "Mark-mid gap in basis points",
        &["market_key"]
    )
    .unwrap()
});

/// Spread in basis points.
pub static SPREAD_BPS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "hip3_spread_bps",
        "Current spread in basis points",
        &["market_key"]
    )
    .unwrap()
});

/// Oracle age in milliseconds.
pub static ORACLE_AGE_MS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "hip3_oracle_age_ms",
        "Oracle age in milliseconds since last change",
        &["market_key"]
    )
    .unwrap()
});

// =============================================================================
// P0-8: Rate Limiting Metrics
// =============================================================================

/// Total WebSocket messages sent by type.
/// Labels: kind (subscribe/unsubscribe/ping/post/cancel)
pub static WS_MSGS_SENT_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "hip3_ws_msgs_sent_total",
        "Total WebSocket messages sent by type",
        &["kind"]
    )
    .unwrap()
});

/// Total WebSocket messages blocked by reason.
/// Labels: reason (rate_limit/inflight_full/circuit_open), kind
pub static WS_MSGS_BLOCKED_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "hip3_ws_msgs_blocked_total",
        "Total WebSocket messages blocked by rate limiting",
        &["reason", "kind"]
    )
    .unwrap()
});

/// Current number of inflight posts (0-100).
pub static POST_INFLIGHT: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!(
        "hip3_post_inflight",
        "Current number of inflight post requests (max 100)"
    )
    .unwrap()
});

/// Total posts rejected by reason.
/// Labels: reason (rate_limit/error/timeout)
pub static POST_REJECTED_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "hip3_post_rejected_total",
        "Total post requests rejected",
        &["reason"]
    )
    .unwrap()
});

/// Circuit breaker state (1=open, 0=closed).
pub static ACTION_BUDGET_CIRCUIT_OPEN: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!(
        "hip3_action_budget_circuit_open",
        "Circuit breaker state (1=open, 0=closed)"
    )
    .unwrap()
});

/// Total address-based limit hits.
pub static ADDRESS_LIMIT_HIT_TOTAL: Lazy<prometheus::IntCounter> = Lazy::new(|| {
    prometheus::register_int_counter!(
        "hip3_address_limit_hit_total",
        "Total address-based rate limit hits (1req/10sec mode)"
    )
    .unwrap()
});

// =============================================================================
// P0-8: Cross Judgment Metrics
// =============================================================================

/// Total cross judgments skipped by reason.
/// Labels: reason (bbo_stale/oracle_stale/bbo_null/freshness_invalid)
pub static CROSS_SKIPPED_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "hip3_cross_skipped_total",
        "Total cross judgments skipped",
        &["reason"]
    )
    .unwrap()
});

/// BBO age in milliseconds (monotonic).
pub static BBO_AGE_MS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "hip3_bbo_age_ms",
        "BBO age in milliseconds since last receive",
        &["market_key"]
    )
    .unwrap()
});

/// AssetCtx age in milliseconds (monotonic).
pub static CTX_AGE_MS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "hip3_ctx_age_ms",
        "AssetCtx age in milliseconds since last receive",
        &["market_key"]
    )
    .unwrap()
});

// =============================================================================
// P0-31: Phase A DoD Metrics
// =============================================================================

/// Total cross detections.
pub static CROSS_COUNT_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "hip3_cross_count_total",
        "Total oracle cross detections",
        &["market_key", "side"]
    )
    .unwrap()
});

/// BBO null rate (fraction of updates with null bid/ask).
pub static BBO_NULL_RATE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "hip3_bbo_null_rate",
        "BBO null rate (fraction of updates with null bid/ask)",
        &["market_key"]
    )
    .unwrap()
});

/// BBO age histogram for percentile calculation (P50/P95/P99).
pub static BBO_AGE_HIST_MS: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "hip3_bbo_age_hist_ms",
        "BBO age distribution in milliseconds",
        &["market_key"],
        vec![1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0]
    )
    .unwrap()
});

/// AssetCtx age histogram for percentile calculation (P50/P95/P99).
pub static CTX_AGE_HIST_MS: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "hip3_ctx_age_hist_ms",
        "AssetCtx age distribution in milliseconds",
        &["market_key"],
        vec![10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 8000.0, 10000.0]
    )
    .unwrap()
});

/// Cross duration in ticks (how long dislocation persists).
pub static CROSS_DURATION_TICKS: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "hip3_cross_duration_ticks",
        "Cross duration in number of ticks",
        &["market_key", "side"],
        vec![1.0, 2.0, 3.0, 5.0, 10.0, 20.0, 50.0, 100.0]
    )
    .unwrap()
});

/// Total BBO updates received (for null rate calculation).
pub static BBO_UPDATE_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "hip3_bbo_update_total",
        "Total BBO updates received",
        &["market_key"]
    )
    .unwrap()
});

/// Total BBO null updates (bid or ask is null/zero).
pub static BBO_NULL_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "hip3_bbo_null_total",
        "Total BBO updates with null bid or ask",
        &["market_key"]
    )
    .unwrap()
});

// =============================================================================
// P1-1: Trade PnL & Position Observability Metrics
// =============================================================================

/// Trade PnL in basis points per closed position.
/// Labels: market, exit_reason (OracleReversal/OracleCatchup/MarkRegression/TimeStop/Unknown)
pub static TRADE_PNL_BPS: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "hip3_trade_pnl_bps",
        "Trade PnL in basis points per closed position",
        &["market", "exit_reason"],
        vec![
            -100.0, -50.0, -30.0, -20.0, -10.0, -5.0, 0.0, 5.0, 10.0, 20.0, 30.0, 50.0, 100.0,
            200.0,
        ]
    )
    .unwrap()
});

/// Position holding time in milliseconds.
pub static POSITION_HOLDING_TIME_MS: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "hip3_position_holding_time_ms",
        "Position holding time in milliseconds",
        &["market", "exit_reason"],
        vec![100.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 15000.0, 20000.0, 30000.0, 60000.0,]
    )
    .unwrap()
});

/// Entry edge in basis points at signal detection time.
pub static ENTRY_EDGE_BPS: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "hip3_entry_edge_bps",
        "Entry edge in basis points at signal detection time",
        &["market"],
        vec![5.0, 10.0, 15.0, 20.0, 30.0, 40.0, 50.0, 75.0, 100.0, 150.0, 200.0]
    )
    .unwrap()
});

/// Signal-to-order latency in milliseconds.
pub static SIGNAL_TO_ORDER_LATENCY_MS: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "hip3_signal_to_order_latency_ms",
        "Latency from signal detection to order submission in milliseconds",
        &["market"],
        vec![1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0]
    )
    .unwrap()
});

/// Metrics facade for easy access.
pub struct Metrics;

impl Metrics {
    /// Record WebSocket connected.
    pub fn ws_connected() {
        WS_CONNECTED.set(1.0);
    }

    /// Record WebSocket disconnected.
    pub fn ws_disconnected() {
        WS_CONNECTED.set(0.0);
    }

    /// Set WebSocket state machine state.
    /// Only the active state should be set to 1, all others to 0.
    pub fn ws_state_set(state: &str) {
        // Reset all states to 0
        for s in &[
            "disconnected",
            "connecting",
            "connected",
            "ready",
            "reconnecting",
        ] {
            WS_STATE.with_label_values(&[s]).set(0.0);
        }
        // Set active state to 1
        WS_STATE.with_label_values(&[state]).set(1.0);
    }

    /// Record WebSocket reconnection.
    pub fn ws_reconnect(reason: &str) {
        WS_RECONNECT_TOTAL.with_label_values(&[reason]).inc();
    }

    /// Record feed latency.
    pub fn feed_latency(channel: &str, latency_ms: f64) {
        FEED_LATENCY_MS
            .with_label_values(&[channel])
            .observe(latency_ms);
    }

    /// Record signal triggered.
    pub fn signal_triggered(market_key: &str, side: &str, strength: &str) {
        TRIGGERS_TOTAL
            .with_label_values(&[market_key, side, strength])
            .inc();
    }

    /// Record edge observation.
    pub fn edge_observed(market_key: &str, side: &str, edge_bps: f64) {
        EDGE_BPS
            .with_label_values(&[market_key, side])
            .observe(edge_bps);
    }

    /// Record risk gate block.
    pub fn gate_blocked(gate: &str, market_key: &str) {
        GATE_BLOCKED_TOTAL
            .with_label_values(&[gate, market_key])
            .inc();
    }

    /// Record gate block duration when block period ends.
    pub fn gate_block_duration(gate: &str, market_key: &str, duration_ms: f64) {
        GATE_BLOCK_DURATION_MS
            .with_label_values(&[gate, market_key])
            .observe(duration_ms);
    }

    /// Update oracle stale rate.
    pub fn oracle_stale_rate(market_key: &str, rate: f64) {
        ORACLE_STALE_RATE.with_label_values(&[market_key]).set(rate);
    }

    /// Update mark-mid gap.
    pub fn mark_mid_gap(market_key: &str, gap_bps: f64) {
        MARK_MID_GAP_BPS
            .with_label_values(&[market_key])
            .set(gap_bps);
    }

    /// Update spread.
    pub fn spread(market_key: &str, spread_bps: f64) {
        SPREAD_BPS.with_label_values(&[market_key]).set(spread_bps);
    }

    /// Update oracle age.
    pub fn oracle_age(market_key: &str, age_ms: f64) {
        ORACLE_AGE_MS.with_label_values(&[market_key]).set(age_ms);
    }

    // =========================================================================
    // P0-8: Rate Limiting Metrics
    // =========================================================================

    /// Record WebSocket message sent.
    pub fn ws_msg_sent(kind: &str) {
        WS_MSGS_SENT_TOTAL.with_label_values(&[kind]).inc();
    }

    /// Record WebSocket message blocked.
    pub fn ws_msg_blocked(reason: &str, kind: &str) {
        WS_MSGS_BLOCKED_TOTAL
            .with_label_values(&[reason, kind])
            .inc();
    }

    /// Update inflight post count.
    pub fn post_inflight_set(count: i64) {
        POST_INFLIGHT.set(count);
    }

    /// Increment inflight post count.
    pub fn post_inflight_inc() {
        POST_INFLIGHT.inc();
    }

    /// Decrement inflight post count.
    pub fn post_inflight_dec() {
        POST_INFLIGHT.dec();
    }

    /// Record post rejected.
    pub fn post_rejected(reason: &str) {
        POST_REJECTED_TOTAL.with_label_values(&[reason]).inc();
    }

    /// Set circuit breaker state.
    pub fn circuit_open(is_open: bool) {
        ACTION_BUDGET_CIRCUIT_OPEN.set(if is_open { 1 } else { 0 });
    }

    /// Record address limit hit.
    pub fn address_limit_hit() {
        ADDRESS_LIMIT_HIT_TOTAL.inc();
    }

    // =========================================================================
    // P0-8: Cross Judgment Metrics
    // =========================================================================

    /// Record cross judgment skipped.
    pub fn cross_skipped(reason: &str) {
        CROSS_SKIPPED_TOTAL.with_label_values(&[reason]).inc();
    }

    /// Update BBO age.
    pub fn bbo_age(market_key: &str, age_ms: f64) {
        BBO_AGE_MS.with_label_values(&[market_key]).set(age_ms);
    }

    /// Update AssetCtx age.
    pub fn ctx_age(market_key: &str, age_ms: f64) {
        CTX_AGE_MS.with_label_values(&[market_key]).set(age_ms);
    }

    // =========================================================================
    // P0-31: Phase A DoD Metrics
    // =========================================================================

    /// Record cross detection.
    pub fn cross_detected(market_key: &str, side: &str) {
        CROSS_COUNT_TOTAL
            .with_label_values(&[market_key, side])
            .inc();
    }

    /// Update BBO null rate.
    pub fn bbo_null_rate(market_key: &str, rate: f64) {
        BBO_NULL_RATE.with_label_values(&[market_key]).set(rate);
    }

    /// Record BBO age to histogram (for percentile calculation).
    pub fn bbo_age_hist(market_key: &str, age_ms: f64) {
        BBO_AGE_HIST_MS
            .with_label_values(&[market_key])
            .observe(age_ms);
    }

    /// Record AssetCtx age to histogram (for percentile calculation).
    pub fn ctx_age_hist(market_key: &str, age_ms: f64) {
        CTX_AGE_HIST_MS
            .with_label_values(&[market_key])
            .observe(age_ms);
    }

    /// Record cross duration in ticks.
    pub fn cross_duration(market_key: &str, side: &str, ticks: f64) {
        CROSS_DURATION_TICKS
            .with_label_values(&[market_key, side])
            .observe(ticks);
    }

    /// Record BBO update (for null rate calculation).
    pub fn bbo_update(market_key: &str) {
        BBO_UPDATE_TOTAL.with_label_values(&[market_key]).inc();
    }

    /// Record BBO null update.
    pub fn bbo_null_update(market_key: &str) {
        BBO_NULL_TOTAL.with_label_values(&[market_key]).inc();
    }

    // =========================================================================
    // P1-1: Trade PnL & Position Observability
    // =========================================================================

    /// Record trade PnL in basis points.
    pub fn trade_pnl(market: &str, exit_reason: &str, pnl_bps: f64) {
        TRADE_PNL_BPS
            .with_label_values(&[market, exit_reason])
            .observe(pnl_bps);
    }

    /// Record position holding time in milliseconds.
    pub fn position_holding_time(market: &str, exit_reason: &str, holding_ms: f64) {
        POSITION_HOLDING_TIME_MS
            .with_label_values(&[market, exit_reason])
            .observe(holding_ms);
    }

    /// Record entry edge in basis points.
    pub fn entry_edge(market: &str, edge_bps: f64) {
        ENTRY_EDGE_BPS
            .with_label_values(&[market])
            .observe(edge_bps);
    }

    /// Record signal-to-order latency in milliseconds.
    pub fn signal_to_order_latency(market: &str, latency_ms: f64) {
        SIGNAL_TO_ORDER_LATENCY_MS
            .with_label_values(&[market])
            .observe(latency_ms);
    }
}
