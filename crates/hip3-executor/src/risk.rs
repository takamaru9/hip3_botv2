//! Risk management components for HardStop and monitoring.
//!
//! This module provides:
//! - `HardStopLatch`: Circuit breaker for emergency trading halt
//! - `ExecutionEvent`: Events for risk monitoring
//! - `RiskMonitor`: Background task for monitoring risk conditions
//! - `RiskMonitorConfig`: Configuration for risk thresholds

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rust_decimal::Decimal;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use hip3_core::{ClientOrderId, MarketKey, Price, Size};

// ============================================================================
// HardStopLatch
// ============================================================================

/// Hard stop latch for emergency circuit breaker.
///
/// When triggered, the system enters an emergency mode where:
/// - New orders are rejected/dropped
/// - Only reduce_only orders are processed (to close positions)
/// - Cancels continue to be processed
///
/// Once triggered, the latch remains triggered until manually reset.
/// This prevents accidental resumption of trading after an emergency.
///
/// # Example
/// ```
/// use hip3_executor::HardStopLatch;
///
/// let latch = HardStopLatch::new();
/// assert!(!latch.is_triggered());
///
/// latch.trigger("Test trigger");
/// assert!(latch.is_triggered());
/// assert!(latch.trigger_reason().is_some());
/// ```
#[derive(Debug)]
pub struct HardStopLatch {
    /// Whether the hard stop has been triggered.
    triggered: AtomicBool,
    /// Reason for triggering (set on first trigger only).
    trigger_reason: Mutex<Option<String>>,
    /// Time when triggered (set on first trigger only).
    trigger_time: Mutex<Option<Instant>>,
}

impl HardStopLatch {
    /// Create a new hard stop latch (not triggered).
    #[must_use]
    pub fn new() -> Self {
        Self {
            triggered: AtomicBool::new(false),
            trigger_reason: Mutex::new(None),
            trigger_time: Mutex::new(None),
        }
    }

    /// Check if the hard stop has been triggered.
    #[must_use]
    pub fn is_triggered(&self) -> bool {
        self.triggered.load(Ordering::Acquire)
    }

    /// Trigger the hard stop.
    ///
    /// Once triggered, new orders will be rejected and only
    /// reduce_only orders will be processed.
    ///
    /// If already triggered, the reason and time are NOT updated.
    /// This preserves the original trigger context.
    pub fn trigger(&self, reason: &str) {
        // Use swap to detect if we're the first to trigger
        if !self.triggered.swap(true, Ordering::AcqRel) {
            // We're the first to trigger - record reason and time
            *self.trigger_reason.lock() = Some(reason.to_string());
            *self.trigger_time.lock() = Some(Instant::now());
            error!(reason, "🛑 HARD STOP TRIGGERED");
        }
    }

    /// Get the trigger reason (if triggered).
    #[must_use]
    pub fn trigger_reason(&self) -> Option<String> {
        self.trigger_reason.lock().clone()
    }

    /// Get the time since trigger (if triggered).
    #[must_use]
    pub fn elapsed_since_trigger(&self) -> Option<Duration> {
        self.trigger_time.lock().map(|t| t.elapsed())
    }

    /// Reset the hard stop latch.
    ///
    /// Only call this after the emergency condition has been resolved
    /// and the operator has confirmed it's safe to resume trading.
    ///
    /// # Warning
    /// This should only be called by a human operator, not automatically.
    pub fn reset(&self) {
        self.triggered.store(false, Ordering::Release);
        *self.trigger_reason.lock() = None;
        *self.trigger_time.lock() = None;
        warn!("⚠️ HardStop RESET by operator - normal operation resumed");
    }
}

impl Default for HardStopLatch {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// ExecutionEvent
// ============================================================================

/// Events for risk monitoring.
///
/// These events are sent from various components to the `RiskMonitor`
/// for tracking risk conditions and triggering HardStop when thresholds
/// are exceeded.
#[derive(Debug, Clone)]
pub enum ExecutionEvent {
    /// A fill occurred.
    Fill {
        /// Market of the fill.
        market: MarketKey,
        /// Client order ID.
        cloid: ClientOrderId,
        /// Fill price.
        price: Price,
        /// Fill size.
        size: Size,
        /// Realized PnL (only for position close).
        pnl: Option<Decimal>,
    },

    /// Position was closed.
    PositionClosed {
        /// Market of the position.
        market: MarketKey,
        /// Realized PnL from the close.
        realized_pnl: Decimal,
    },

    /// Flatten attempt failed.
    FlattenFailed {
        /// Market that failed to flatten.
        market: MarketKey,
        /// Reason for failure.
        reason: String,
    },

    /// Order was rejected by exchange.
    Rejected {
        /// Client order ID.
        cloid: ClientOrderId,
        /// Rejection reason.
        reason: String,
    },

    /// Slippage measurement for a trade.
    SlippageMeasured {
        /// Market of the trade.
        market: MarketKey,
        /// Expected edge in basis points.
        expected_edge_bps: f64,
        /// Actual edge in basis points.
        actual_edge_bps: f64,
    },
}

// ============================================================================
// RiskMonitorConfig
// ============================================================================

/// Configuration for risk monitoring thresholds.
#[derive(Debug, Clone)]
pub struct RiskMonitorConfig {
    /// Maximum cumulative loss before HardStop (e.g., $20).
    pub max_cumulative_loss: Decimal,
    /// Maximum consecutive losses before HardStop (e.g., 5).
    pub max_consecutive_losses: u32,
    /// Maximum flatten failures before HardStop (e.g., 3).
    pub max_flatten_failed: u32,
    /// Maximum rejected orders per hour before HardStop (e.g., 10).
    pub max_rejected_per_hour: u32,
    /// Maximum slippage in basis points (e.g., 50).
    pub max_slippage_bps: f64,
    /// Number of consecutive high-slippage trades before HardStop (e.g., 3).
    pub slippage_consecutive_threshold: u32,
}

impl Default for RiskMonitorConfig {
    fn default() -> Self {
        Self {
            max_cumulative_loss: Decimal::new(20, 0), // $20
            max_consecutive_losses: 5,
            max_flatten_failed: 3,
            max_rejected_per_hour: 10,
            max_slippage_bps: 50.0,
            slippage_consecutive_threshold: 3,
        }
    }
}

// ============================================================================
// RiskMonitor
// ============================================================================

/// Handle for sending HardStop commands to Executor.
#[derive(Clone)]
pub struct ExecutorHandle {
    tx: mpsc::Sender<String>,
}

impl ExecutorHandle {
    /// Create a new executor handle.
    #[must_use]
    pub fn new(tx: mpsc::Sender<String>) -> Self {
        Self { tx }
    }

    /// Send a HardStop command to the executor.
    pub async fn on_hard_stop(&self, reason: &str) {
        let _ = self.tx.send(reason.to_string()).await;
    }
}

/// Background task for monitoring risk conditions.
///
/// The `RiskMonitor` receives `ExecutionEvent`s and tracks various
/// risk metrics. When thresholds are exceeded, it triggers HardStop.
///
/// # Monitored Conditions
///
/// | Trigger | Threshold |
/// |---------|-----------|
/// | Cumulative loss | > $20 |
/// | Consecutive losses | > 5 |
/// | Flatten failures | > 3 |
/// | Rejected orders | > 10/hour |
/// | Slippage | > 50 bps for 3 consecutive |
pub struct RiskMonitor {
    /// Event receiver.
    event_rx: mpsc::Receiver<ExecutionEvent>,
    /// Hard stop latch (shared with other components).
    hard_stop_latch: std::sync::Arc<HardStopLatch>,
    /// Executor handle for triggering cleanup.
    executor_handle: ExecutorHandle,

    // === Counters ===
    /// Cumulative PnL (negative = loss).
    cumulative_pnl: Decimal,
    /// Current consecutive loss count.
    consecutive_losses: u32,
    /// Flatten failure count.
    flatten_failed_count: u32,
    /// Rejected orders in current hour.
    rejected_count_hourly: u32,
    /// Time when hourly counter was last reset.
    rejected_reset_time: Instant,
    /// Recent slippage measurements.
    slippage_history: VecDeque<f64>,

    /// Configuration.
    config: RiskMonitorConfig,
}

impl RiskMonitor {
    /// Create a new risk monitor.
    ///
    /// # Arguments
    /// * `event_rx` - Channel to receive execution events
    /// * `hard_stop_latch` - Shared hard stop latch
    /// * `executor_handle` - Handle for sending HardStop commands
    /// * `config` - Risk monitoring configuration
    #[must_use]
    pub fn new(
        event_rx: mpsc::Receiver<ExecutionEvent>,
        hard_stop_latch: std::sync::Arc<HardStopLatch>,
        executor_handle: ExecutorHandle,
        config: RiskMonitorConfig,
    ) -> Self {
        Self {
            event_rx,
            hard_stop_latch,
            executor_handle,
            cumulative_pnl: Decimal::ZERO,
            consecutive_losses: 0,
            flatten_failed_count: 0,
            rejected_count_hourly: 0,
            rejected_reset_time: Instant::now(),
            slippage_history: VecDeque::with_capacity(10),
            config,
        }
    }

    /// Run the risk monitoring loop.
    ///
    /// Processes events until the channel is closed. When a threshold
    /// is exceeded, triggers HardStop and notifies the executor.
    pub async fn run(mut self) {
        info!("RiskMonitor started");

        while let Some(event) = self.event_rx.recv().await {
            if let Some(reason) = self.process_event(event) {
                // HardStop triggered
                self.hard_stop_latch.trigger(&reason);
                self.executor_handle.on_hard_stop(&reason).await;
                // Continue processing events for logging/metrics
            }
        }

        info!("RiskMonitor stopped (channel closed)");
    }

    /// Process a single event and check for HardStop conditions.
    ///
    /// Returns `Some(reason)` if HardStop should be triggered.
    fn process_event(&mut self, event: ExecutionEvent) -> Option<String> {
        match event {
            ExecutionEvent::PositionClosed { realized_pnl, .. } => {
                self.cumulative_pnl += realized_pnl;

                if realized_pnl < Decimal::ZERO {
                    self.consecutive_losses += 1;
                } else {
                    self.consecutive_losses = 0;
                }

                // Check cumulative loss threshold
                if self.cumulative_pnl < -self.config.max_cumulative_loss {
                    return Some(format!(
                        "Cumulative loss exceeded: {} (threshold: -{})",
                        self.cumulative_pnl, self.config.max_cumulative_loss
                    ));
                }

                // Check consecutive loss threshold
                if self.consecutive_losses > self.config.max_consecutive_losses {
                    return Some(format!(
                        "Consecutive losses exceeded: {} (threshold: {})",
                        self.consecutive_losses, self.config.max_consecutive_losses
                    ));
                }
            }

            ExecutionEvent::FlattenFailed { market, reason } => {
                self.flatten_failed_count += 1;
                error!(?market, reason, "Flatten failed");

                if self.flatten_failed_count > self.config.max_flatten_failed {
                    return Some(format!(
                        "Flatten failed count exceeded: {} (threshold: {})",
                        self.flatten_failed_count, self.config.max_flatten_failed
                    ));
                }
            }

            ExecutionEvent::Rejected { cloid, reason } => {
                // Skip counting for expected/benign rejection types:
                // - iocCancelRejected: IOC order couldn't match (normal for thin orderbooks)
                // - reduceOnlyRejected: Position already closed (race condition, not a problem)
                // - Order has zero size: Rounding to zero (edge case, not a problem)
                let is_benign_rejection = reason == "iocCancelRejected"
                    || reason == "reduceOnlyRejected"
                    || reason.contains("Order could not immediately match")
                    || reason.contains("Reduce only order would increase position")
                    || reason.contains("Order has zero size");

                if is_benign_rejection {
                    debug!(
                        ?cloid,
                        reason, "Order rejected (benign, not counted toward HardStop)"
                    );
                    return None;
                }

                // Reset hourly counter if hour has passed
                if self.rejected_reset_time.elapsed() > Duration::from_secs(3600) {
                    self.rejected_count_hourly = 0;
                    self.rejected_reset_time = Instant::now();
                }

                self.rejected_count_hourly += 1;
                warn!(?cloid, reason, "Order rejected");

                if self.rejected_count_hourly > self.config.max_rejected_per_hour {
                    return Some(format!(
                        "Rejected count exceeded: {}/hour (threshold: {})",
                        self.rejected_count_hourly, self.config.max_rejected_per_hour
                    ));
                }
            }

            ExecutionEvent::SlippageMeasured {
                expected_edge_bps,
                actual_edge_bps,
                ..
            } => {
                let slippage = expected_edge_bps - actual_edge_bps;
                self.slippage_history.push_back(slippage);

                // Keep only last 10 measurements
                while self.slippage_history.len() > 10 {
                    self.slippage_history.pop_front();
                }

                // Check for consecutive high slippage
                let threshold = self.config.slippage_consecutive_threshold as usize;
                if self.slippage_history.len() >= threshold {
                    let consecutive_high = self
                        .slippage_history
                        .iter()
                        .rev()
                        .take(threshold)
                        .all(|&s| s > self.config.max_slippage_bps);

                    if consecutive_high {
                        return Some(format!(
                            "Slippage exceeded {} bps for {} consecutive trades",
                            self.config.max_slippage_bps, threshold
                        ));
                    }
                }
            }

            ExecutionEvent::Fill { .. } => {
                // Fills are tracked for metrics but don't trigger HardStop directly
                // PnL impact is captured via PositionClosed
            }
        }

        None
    }

    /// Get current risk metrics for monitoring.
    #[must_use]
    pub fn metrics(&self) -> RiskMetrics {
        RiskMetrics {
            cumulative_pnl: self.cumulative_pnl,
            consecutive_losses: self.consecutive_losses,
            flatten_failed_count: self.flatten_failed_count,
            rejected_count_hourly: self.rejected_count_hourly,
            recent_slippage: self.slippage_history.iter().copied().collect(),
        }
    }
}

/// Current risk metrics snapshot.
#[derive(Debug, Clone)]
pub struct RiskMetrics {
    /// Cumulative PnL since start.
    pub cumulative_pnl: Decimal,
    /// Current consecutive loss count.
    pub consecutive_losses: u32,
    /// Total flatten failures.
    pub flatten_failed_count: u32,
    /// Rejected orders this hour.
    pub rejected_count_hourly: u32,
    /// Recent slippage measurements.
    pub recent_slippage: Vec<f64>,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetId, DexId};
    use rust_decimal_macros::dec;
    use std::sync::Arc;

    fn sample_market() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    // ========================================================================
    // HardStopLatch Tests
    // ========================================================================

    #[test]
    fn test_hard_stop_latch_initial_state() {
        let latch = HardStopLatch::new();
        assert!(!latch.is_triggered());
        assert!(latch.trigger_reason().is_none());
        assert!(latch.elapsed_since_trigger().is_none());
    }

    #[test]
    fn test_hard_stop_latch_trigger() {
        let latch = HardStopLatch::new();

        latch.trigger("Test reason");

        assert!(latch.is_triggered());
        assert_eq!(latch.trigger_reason(), Some("Test reason".to_string()));
        assert!(latch.elapsed_since_trigger().is_some());
    }

    #[test]
    fn test_hard_stop_latch_trigger_idempotent() {
        let latch = HardStopLatch::new();

        latch.trigger("First reason");
        latch.trigger("Second reason");

        // First reason should be preserved
        assert_eq!(latch.trigger_reason(), Some("First reason".to_string()));
    }

    #[test]
    fn test_hard_stop_latch_reset() {
        let latch = HardStopLatch::new();

        latch.trigger("Test reason");
        assert!(latch.is_triggered());

        latch.reset();
        assert!(!latch.is_triggered());
        assert!(latch.trigger_reason().is_none());
        assert!(latch.elapsed_since_trigger().is_none());
    }

    // ========================================================================
    // RiskMonitor Tests
    // ========================================================================

    fn create_test_monitor() -> (
        mpsc::Sender<ExecutionEvent>,
        RiskMonitor,
        Arc<HardStopLatch>,
    ) {
        let (event_tx, event_rx) = mpsc::channel(100);
        let (executor_tx, _executor_rx) = mpsc::channel(10);
        let hard_stop_latch = Arc::new(HardStopLatch::new());
        let executor_handle = ExecutorHandle::new(executor_tx);
        let config = RiskMonitorConfig::default();

        let monitor = RiskMonitor::new(
            event_rx,
            Arc::clone(&hard_stop_latch),
            executor_handle,
            config,
        );

        (event_tx, monitor, hard_stop_latch)
    }

    #[test]
    fn test_risk_monitor_cumulative_loss() {
        let (_, mut monitor, _) = create_test_monitor();

        // Simulate losses
        for i in 0..3 {
            let event = ExecutionEvent::PositionClosed {
                market: sample_market(),
                realized_pnl: dec!(-8), // 3 × -8 = -24 > -20 threshold
            };

            if i < 2 {
                assert!(monitor.process_event(event).is_none());
            } else {
                let result = monitor.process_event(event);
                assert!(result.is_some());
                assert!(result.unwrap().contains("Cumulative loss"));
            }
        }
    }

    #[test]
    fn test_risk_monitor_consecutive_losses() {
        let (_, mut monitor, _) = create_test_monitor();

        // Simulate 6 consecutive small losses (threshold is 5)
        for i in 0..6 {
            let event = ExecutionEvent::PositionClosed {
                market: sample_market(),
                realized_pnl: dec!(-1), // Small loss, won't hit cumulative
            };

            if i < 5 {
                assert!(monitor.process_event(event).is_none());
            } else {
                let result = monitor.process_event(event);
                assert!(result.is_some());
                assert!(result.unwrap().contains("Consecutive losses"));
            }
        }
    }

    #[test]
    fn test_risk_monitor_consecutive_losses_reset_on_win() {
        let (_, mut monitor, _) = create_test_monitor();

        // 4 losses, then 1 win, then 4 more losses
        for _ in 0..4 {
            let event = ExecutionEvent::PositionClosed {
                market: sample_market(),
                realized_pnl: dec!(-1),
            };
            assert!(monitor.process_event(event).is_none());
        }

        // Win resets the counter
        let event = ExecutionEvent::PositionClosed {
            market: sample_market(),
            realized_pnl: dec!(1),
        };
        assert!(monitor.process_event(event).is_none());
        assert_eq!(monitor.consecutive_losses, 0);

        // 4 more losses still shouldn't trigger
        for _ in 0..4 {
            let event = ExecutionEvent::PositionClosed {
                market: sample_market(),
                realized_pnl: dec!(-1),
            };
            assert!(monitor.process_event(event).is_none());
        }
    }

    #[test]
    fn test_risk_monitor_flatten_failed() {
        let (_, mut monitor, _) = create_test_monitor();

        // 4 flatten failures (threshold is 3)
        for i in 0..4 {
            let event = ExecutionEvent::FlattenFailed {
                market: sample_market(),
                reason: "Test failure".to_string(),
            };

            if i < 3 {
                assert!(monitor.process_event(event).is_none());
            } else {
                let result = monitor.process_event(event);
                assert!(result.is_some());
                assert!(result.unwrap().contains("Flatten failed"));
            }
        }
    }

    #[test]
    fn test_risk_monitor_slippage() {
        let (_, mut monitor, _) = create_test_monitor();

        // 3 consecutive high slippage trades
        for i in 0..3 {
            let event = ExecutionEvent::SlippageMeasured {
                market: sample_market(),
                expected_edge_bps: 100.0,
                actual_edge_bps: 40.0, // slippage = 60 bps > 50 bps
            };

            if i < 2 {
                assert!(monitor.process_event(event).is_none());
            } else {
                let result = monitor.process_event(event);
                assert!(result.is_some());
                assert!(result.unwrap().contains("Slippage exceeded"));
            }
        }
    }

    #[test]
    fn test_risk_monitor_slippage_broken_by_good_trade() {
        let (_, mut monitor, _) = create_test_monitor();

        // 2 high slippage trades
        for _ in 0..2 {
            let event = ExecutionEvent::SlippageMeasured {
                market: sample_market(),
                expected_edge_bps: 100.0,
                actual_edge_bps: 40.0, // slippage = 60 bps
            };
            assert!(monitor.process_event(event).is_none());
        }

        // 1 good trade
        let event = ExecutionEvent::SlippageMeasured {
            market: sample_market(),
            expected_edge_bps: 100.0,
            actual_edge_bps: 80.0, // slippage = 20 bps
        };
        assert!(monitor.process_event(event).is_none());

        // 1 more high slippage - shouldn't trigger (sequence broken)
        let event = ExecutionEvent::SlippageMeasured {
            market: sample_market(),
            expected_edge_bps: 100.0,
            actual_edge_bps: 40.0, // slippage = 60 bps
        };
        assert!(monitor.process_event(event).is_none());
    }

    #[test]
    fn test_risk_metrics() {
        let (_, mut monitor, _) = create_test_monitor();

        // Add some events
        let _ = monitor.process_event(ExecutionEvent::PositionClosed {
            market: sample_market(),
            realized_pnl: dec!(-5),
        });

        let _ = monitor.process_event(ExecutionEvent::FlattenFailed {
            market: sample_market(),
            reason: "Test".to_string(),
        });

        let metrics = monitor.metrics();
        assert_eq!(metrics.cumulative_pnl, dec!(-5));
        assert_eq!(metrics.consecutive_losses, 1);
        assert_eq!(metrics.flatten_failed_count, 1);
    }
}
