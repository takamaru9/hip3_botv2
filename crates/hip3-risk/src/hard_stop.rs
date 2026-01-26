//! HardStop and RiskMonitor for emergency stop functionality.
//!
//! HardStopLatch: A latch that, once triggered, remains triggered until manually reset.
//! RiskMonitor: Monitors execution events and triggers HardStop on risk violations.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use tracing::{error, info, warn};

use hip3_core::{ClientOrderId, MarketKey, Price};

// ============================================================================
// HardStopReason
// ============================================================================

/// Reason for HardStop trigger.
#[derive(Debug, Clone, PartialEq)]
pub enum HardStopReason {
    /// ActionBudget exhausted.
    BudgetExhausted,
    /// Maximum loss threshold reached.
    MaxLossReached {
        /// Loss amount in USD.
        loss_usd: Price,
    },
    /// Too many consecutive order failures.
    ConsecutiveFailures {
        /// Number of consecutive failures.
        count: u32,
    },
    /// Manual trigger by operator.
    Manual {
        /// Human-readable message.
        message: String,
    },
    /// Triggered by RiskMonitor detection.
    RiskMonitor {
        /// Event description.
        event: String,
    },
}

impl std::fmt::Display for HardStopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BudgetExhausted => write!(f, "ActionBudget exhausted"),
            Self::MaxLossReached { loss_usd } => write!(f, "Max loss reached: ${}", loss_usd),
            Self::ConsecutiveFailures { count } => {
                write!(f, "Consecutive failures: {}", count)
            }
            Self::Manual { message } => write!(f, "Manual: {}", message),
            Self::RiskMonitor { event } => write!(f, "RiskMonitor: {}", event),
        }
    }
}

// ============================================================================
// HardStopLatch
// ============================================================================

/// HardStop: Emergency stop latch.
///
/// Once triggered, remains triggered until manually reset.
/// This is a safety mechanism to halt trading in emergency situations.
///
/// Thread-safe: Can be safely shared across threads via `Arc<HardStopLatch>`.
pub struct HardStopLatch {
    /// Triggered flag (true = emergency stop active).
    triggered: AtomicBool,
    /// Timestamp when triggered (Unix milliseconds, 0 if not triggered).
    triggered_at: AtomicU64,
    /// Reason for trigger.
    reason: RwLock<Option<HardStopReason>>,
}

impl Default for HardStopLatch {
    fn default() -> Self {
        Self::new()
    }
}

impl HardStopLatch {
    /// Create a new HardStopLatch in non-triggered state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            triggered: AtomicBool::new(false),
            triggered_at: AtomicU64::new(0),
            reason: RwLock::new(None),
        }
    }

    /// Check if HardStop is currently triggered.
    #[must_use]
    pub fn is_triggered(&self) -> bool {
        self.triggered.load(Ordering::SeqCst)
    }

    /// Trigger the HardStop with a reason.
    ///
    /// If already triggered, this is a no-op (keeps original reason).
    pub fn trigger(&self, reason: HardStopReason) {
        // Use compare_exchange to ensure we only set once
        if self
            .triggered
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_millis() as u64;
            self.triggered_at.store(now, Ordering::SeqCst);

            {
                let mut reason_guard = self.reason.write();
                *reason_guard = Some(reason.clone());
            }

            error!(reason = %reason, "HARD STOP TRIGGERED");
        } else {
            warn!(new_reason = %reason, "HardStop already triggered, ignoring new trigger");
        }
    }

    /// Get the timestamp when HardStop was triggered.
    ///
    /// Returns `None` if not triggered.
    #[must_use]
    pub fn triggered_at(&self) -> Option<u64> {
        if self.is_triggered() {
            let ts = self.triggered_at.load(Ordering::SeqCst);
            if ts > 0 {
                return Some(ts);
            }
        }
        None
    }

    /// Get the reason for the HardStop trigger.
    ///
    /// Returns `None` if not triggered.
    #[must_use]
    pub fn reason(&self) -> Option<HardStopReason> {
        if self.is_triggered() {
            self.reason.read().clone()
        } else {
            None
        }
    }

    /// Reset the HardStop latch.
    ///
    /// This is a manual operation that should only be performed by operators
    /// after investigating and resolving the issue that caused the trigger.
    ///
    /// SAFETY: Auto-reset is prohibited. Only manual reset is allowed.
    pub fn reset(&self) {
        if self.is_triggered() {
            let reason = self.reason.read().clone();
            info!(previous_reason = ?reason, "HardStop manually reset");

            self.triggered.store(false, Ordering::SeqCst);
            self.triggered_at.store(0, Ordering::SeqCst);
            {
                let mut reason_guard = self.reason.write();
                *reason_guard = None;
            }
        }
    }
}

// ============================================================================
// RiskMonitorConfig
// ============================================================================

/// Configuration for RiskMonitor.
#[derive(Debug, Clone)]
pub struct RiskMonitorConfig {
    /// Maximum consecutive failures before triggering HardStop.
    pub max_consecutive_failures: u32,
    /// Maximum cumulative loss in USD before triggering HardStop.
    pub max_loss_usd: f64,
}

impl Default for RiskMonitorConfig {
    fn default() -> Self {
        Self {
            max_consecutive_failures: 5,
            max_loss_usd: 1000.0, // $1000 default max loss
        }
    }
}

// ============================================================================
// ExecutionEvent
// ============================================================================

/// Events monitored by RiskMonitor.
#[derive(Debug, Clone)]
pub enum ExecutionEvent {
    /// Order successfully submitted.
    OrderSubmitted {
        /// Client order ID.
        cloid: ClientOrderId,
    },
    /// Order filled (partially or completely).
    OrderFilled {
        /// Client order ID.
        cloid: ClientOrderId,
        /// PnL from this fill (positive = profit, negative = loss).
        pnl: Price,
    },
    /// Order rejected by exchange.
    OrderRejected {
        /// Client order ID.
        cloid: ClientOrderId,
        /// Rejection reason.
        reason: String,
    },
    /// Order timed out.
    OrderTimeout {
        /// Client order ID.
        cloid: ClientOrderId,
    },
    /// Position closed.
    PositionClosed {
        /// Market of the closed position.
        market: MarketKey,
        /// Realized PnL (positive = profit, negative = loss).
        pnl: Price,
    },
}

// ============================================================================
// RiskMonitor
// ============================================================================

/// RiskMonitor: Monitors execution events and triggers HardStop on violations.
///
/// Tracks:
/// - Consecutive order failures (rejects, timeouts)
/// - Cumulative loss
///
/// Thread-safe: Can be safely shared across threads via `Arc<RiskMonitor>`.
pub struct RiskMonitor {
    /// Reference to HardStop latch.
    hard_stop: Arc<HardStopLatch>,
    /// Configuration.
    config: RiskMonitorConfig,
    /// Consecutive failure counter.
    consecutive_failures: AtomicU32,
    /// Cumulative loss in microdollars (1 USD = 1_000_000 microdollars).
    /// Using i64 to handle both profits and losses.
    cumulative_loss_micros: AtomicI64,
}

impl RiskMonitor {
    /// Scale factor for microdollars (1 USD = 1,000,000 microdollars).
    const MICRO_SCALE: f64 = 1_000_000.0;

    /// Create a new RiskMonitor.
    #[must_use]
    pub fn new(hard_stop: Arc<HardStopLatch>, config: RiskMonitorConfig) -> Self {
        Self {
            hard_stop,
            config,
            consecutive_failures: AtomicU32::new(0),
            cumulative_loss_micros: AtomicI64::new(0),
        }
    }

    /// Process an execution event.
    ///
    /// This method should be called for every execution event to monitor
    /// for risk violations.
    pub fn on_event(&self, event: ExecutionEvent) {
        match event {
            ExecutionEvent::OrderSubmitted { cloid } => {
                tracing::trace!(cloid = %cloid, "Order submitted");
                // Submission alone doesn't reset failures - wait for actual success
            }
            ExecutionEvent::OrderFilled { cloid, pnl } => {
                tracing::debug!(cloid = %cloid, pnl = %pnl, "Order filled");
                // Success: reset consecutive failures
                self.reset_failures();
                // Track PnL (negative pnl = loss)
                self.add_pnl(pnl);
            }
            ExecutionEvent::OrderRejected { cloid, reason } => {
                warn!(cloid = %cloid, reason = %reason, "Order rejected");
                self.increment_failures();
            }
            ExecutionEvent::OrderTimeout { cloid } => {
                warn!(cloid = %cloid, "Order timeout");
                self.increment_failures();
            }
            ExecutionEvent::PositionClosed { market, pnl } => {
                tracing::debug!(market = %market, pnl = %pnl, "Position closed");
                // Position close is a success
                self.reset_failures();
                // Track PnL
                self.add_pnl(pnl);
            }
        }
    }

    /// Reset consecutive failure counter.
    fn reset_failures(&self) {
        let prev = self.consecutive_failures.swap(0, Ordering::SeqCst);
        if prev > 0 {
            tracing::debug!(previous_count = prev, "Consecutive failures reset");
        }
    }

    /// Increment consecutive failure counter and check threshold.
    fn increment_failures(&self) {
        let count = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
        tracing::debug!(count = count, "Consecutive failure count");

        if count >= self.config.max_consecutive_failures {
            self.hard_stop
                .trigger(HardStopReason::ConsecutiveFailures { count });
        }
    }

    /// Add PnL and check loss threshold.
    fn add_pnl(&self, pnl: Price) {
        // Convert Price to microdollars
        let pnl_f64 = pnl.inner().to_string().parse::<f64>().unwrap_or(0.0);
        let pnl_micros = (pnl_f64 * Self::MICRO_SCALE) as i64;

        // Subtract PnL from loss (negative PnL = increase loss)
        let prev_loss = self
            .cumulative_loss_micros
            .fetch_sub(pnl_micros, Ordering::SeqCst);
        let new_loss = prev_loss - pnl_micros;

        // Convert to USD for display
        let loss_usd = new_loss as f64 / Self::MICRO_SCALE;

        tracing::trace!(
            pnl_usd = pnl_f64,
            cumulative_loss_usd = loss_usd,
            "PnL updated"
        );

        // Check if cumulative loss exceeds threshold
        // new_loss > 0 means we have net losses
        if new_loss as f64 / Self::MICRO_SCALE > self.config.max_loss_usd {
            self.hard_stop.trigger(HardStopReason::MaxLossReached {
                loss_usd: Price::new(rust_decimal::Decimal::from_f64_retain(loss_usd).unwrap()),
            });
        }
    }

    /// Get current consecutive failure count.
    #[must_use]
    pub fn consecutive_failure_count(&self) -> u32 {
        self.consecutive_failures.load(Ordering::SeqCst)
    }

    /// Get current cumulative loss in USD.
    #[must_use]
    pub fn cumulative_loss_usd(&self) -> f64 {
        let micros = self.cumulative_loss_micros.load(Ordering::SeqCst);
        micros as f64 / Self::MICRO_SCALE
    }

    /// Get reference to the HardStop latch.
    #[must_use]
    pub fn hard_stop(&self) -> &Arc<HardStopLatch> {
        &self.hard_stop
    }

    /// Get reference to the configuration.
    #[must_use]
    pub fn config(&self) -> &RiskMonitorConfig {
        &self.config
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    // === HardStopLatch tests ===

    #[test]
    fn test_hard_stop_initially_not_triggered() {
        let latch = HardStopLatch::new();
        assert!(!latch.is_triggered());
        assert!(latch.triggered_at().is_none());
        assert!(latch.reason().is_none());
    }

    #[test]
    fn test_hard_stop_trigger() {
        let latch = HardStopLatch::new();
        latch.trigger(HardStopReason::BudgetExhausted);

        assert!(latch.is_triggered());
        assert!(latch.triggered_at().is_some());
        assert_eq!(latch.reason(), Some(HardStopReason::BudgetExhausted));
    }

    #[test]
    fn test_hard_stop_reset() {
        let latch = HardStopLatch::new();
        latch.trigger(HardStopReason::BudgetExhausted);
        assert!(latch.is_triggered());

        latch.reset();
        assert!(!latch.is_triggered());
        assert!(latch.triggered_at().is_none());
        assert!(latch.reason().is_none());
    }

    #[test]
    fn test_hard_stop_reason_preserved() {
        let latch = HardStopLatch::new();
        latch.trigger(HardStopReason::MaxLossReached {
            loss_usd: Price::new(dec!(500)),
        });

        let reason = latch.reason().unwrap();
        match reason {
            HardStopReason::MaxLossReached { loss_usd } => {
                assert_eq!(loss_usd.inner(), dec!(500));
            }
            _ => panic!("Wrong reason type"),
        }
    }

    #[test]
    fn test_hard_stop_second_trigger_ignored() {
        let latch = HardStopLatch::new();
        latch.trigger(HardStopReason::BudgetExhausted);
        latch.trigger(HardStopReason::Manual {
            message: "second".to_string(),
        });

        // Original reason should be preserved
        assert_eq!(latch.reason(), Some(HardStopReason::BudgetExhausted));
    }

    // === RiskMonitor tests ===

    #[test]
    fn test_risk_monitor_consecutive_failures_trigger() {
        let hard_stop = Arc::new(HardStopLatch::new());
        let config = RiskMonitorConfig {
            max_consecutive_failures: 3,
            max_loss_usd: 1000.0,
        };
        let monitor = RiskMonitor::new(hard_stop.clone(), config);

        // 2 failures should not trigger
        monitor.on_event(ExecutionEvent::OrderRejected {
            cloid: ClientOrderId::new(),
            reason: "test".to_string(),
        });
        monitor.on_event(ExecutionEvent::OrderRejected {
            cloid: ClientOrderId::new(),
            reason: "test".to_string(),
        });
        assert!(!hard_stop.is_triggered());
        assert_eq!(monitor.consecutive_failure_count(), 2);

        // 3rd failure should trigger
        monitor.on_event(ExecutionEvent::OrderRejected {
            cloid: ClientOrderId::new(),
            reason: "test".to_string(),
        });
        assert!(hard_stop.is_triggered());

        match hard_stop.reason() {
            Some(HardStopReason::ConsecutiveFailures { count }) => {
                assert_eq!(count, 3);
            }
            _ => panic!("Wrong reason type"),
        }
    }

    #[test]
    fn test_risk_monitor_success_resets_failures() {
        let hard_stop = Arc::new(HardStopLatch::new());
        let config = RiskMonitorConfig {
            max_consecutive_failures: 3,
            max_loss_usd: 1000.0,
        };
        let monitor = RiskMonitor::new(hard_stop.clone(), config);

        // 2 failures
        monitor.on_event(ExecutionEvent::OrderRejected {
            cloid: ClientOrderId::new(),
            reason: "test".to_string(),
        });
        monitor.on_event(ExecutionEvent::OrderRejected {
            cloid: ClientOrderId::new(),
            reason: "test".to_string(),
        });
        assert_eq!(monitor.consecutive_failure_count(), 2);

        // Success resets counter
        monitor.on_event(ExecutionEvent::OrderFilled {
            cloid: ClientOrderId::new(),
            pnl: Price::new(dec!(0)),
        });
        assert_eq!(monitor.consecutive_failure_count(), 0);
        assert!(!hard_stop.is_triggered());
    }

    #[test]
    fn test_risk_monitor_cumulative_loss_trigger() {
        let hard_stop = Arc::new(HardStopLatch::new());
        let config = RiskMonitorConfig {
            max_consecutive_failures: 10,
            max_loss_usd: 100.0, // $100 max loss
        };
        let monitor = RiskMonitor::new(hard_stop.clone(), config);

        // $50 loss should not trigger
        monitor.on_event(ExecutionEvent::OrderFilled {
            cloid: ClientOrderId::new(),
            pnl: Price::new(dec!(-50)),
        });
        assert!(!hard_stop.is_triggered());
        assert!((monitor.cumulative_loss_usd() - 50.0).abs() < 0.01);

        // Another $60 loss should trigger (total $110 > $100)
        monitor.on_event(ExecutionEvent::OrderFilled {
            cloid: ClientOrderId::new(),
            pnl: Price::new(dec!(-60)),
        });
        assert!(hard_stop.is_triggered());

        match hard_stop.reason() {
            Some(HardStopReason::MaxLossReached { .. }) => {}
            _ => panic!("Wrong reason type"),
        }
    }

    #[test]
    fn test_risk_monitor_profit_reduces_loss() {
        let hard_stop = Arc::new(HardStopLatch::new());
        let config = RiskMonitorConfig {
            max_consecutive_failures: 10,
            max_loss_usd: 100.0,
        };
        let monitor = RiskMonitor::new(hard_stop.clone(), config);

        // $80 loss
        monitor.on_event(ExecutionEvent::OrderFilled {
            cloid: ClientOrderId::new(),
            pnl: Price::new(dec!(-80)),
        });
        assert!((monitor.cumulative_loss_usd() - 80.0).abs() < 0.01);

        // $50 profit reduces cumulative loss to $30
        monitor.on_event(ExecutionEvent::OrderFilled {
            cloid: ClientOrderId::new(),
            pnl: Price::new(dec!(50)),
        });
        assert!((monitor.cumulative_loss_usd() - 30.0).abs() < 0.01);
        assert!(!hard_stop.is_triggered());
    }

    #[test]
    fn test_hard_stop_reason_display() {
        let reasons = [
            (HardStopReason::BudgetExhausted, "ActionBudget exhausted"),
            (
                HardStopReason::MaxLossReached {
                    loss_usd: Price::new(dec!(500)),
                },
                "Max loss reached: $500",
            ),
            (
                HardStopReason::ConsecutiveFailures { count: 5 },
                "Consecutive failures: 5",
            ),
            (
                HardStopReason::Manual {
                    message: "test".to_string(),
                },
                "Manual: test",
            ),
            (
                HardStopReason::RiskMonitor {
                    event: "test event".to_string(),
                },
                "RiskMonitor: test event",
            ),
        ];

        for (reason, expected) in reasons {
            assert_eq!(reason.to_string(), expected);
        }
    }
}
