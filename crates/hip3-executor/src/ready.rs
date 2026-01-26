//! Trading readiness checker.
//!
//! Manages the READY-TRADING condition by tracking four flags:
//! - MarketData ready
//! - orderUpdates isSnapshot complete
//! - userFills isSnapshot complete
//! - PositionTracker sync complete
//!
//! All four flags must be true for trading to be enabled.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::watch;
use tracing::{debug, info};

/// READY-TRADING condition manager.
///
/// Tracks four independent readiness flags. Trading is only enabled
/// when all flags are true. Changes to readiness are broadcast via
/// a watch channel for subscribers.
///
/// # Thread Safety
///
/// All flags use atomic operations for lock-free access.
/// The watch channel provides efficient notification without polling.
#[derive(Debug)]
pub struct TradingReadyChecker {
    /// MarketData subscription is ready.
    md_ready: AtomicBool,
    /// orderUpdates snapshot has been processed.
    order_snapshot: AtomicBool,
    /// userFills snapshot has been processed.
    fills_snapshot: AtomicBool,
    /// PositionTracker has synchronized state.
    position_synced: AtomicBool,
    /// Watch channel for broadcasting readiness changes.
    tx: watch::Sender<bool>,
}

impl TradingReadyChecker {
    /// Create a new trading ready checker.
    ///
    /// Returns the checker and a watch receiver for subscribing to
    /// readiness changes.
    #[must_use]
    pub fn new() -> (Self, watch::Receiver<bool>) {
        let (tx, rx) = watch::channel(false);

        let checker = Self {
            md_ready: AtomicBool::new(false),
            order_snapshot: AtomicBool::new(false),
            fills_snapshot: AtomicBool::new(false),
            position_synced: AtomicBool::new(false),
            tx,
        };

        (checker, rx)
    }

    /// Set the MarketData ready flag.
    pub fn set_md_ready(&self, ready: bool) {
        let old = self.md_ready.swap(ready, Ordering::SeqCst);
        if old != ready {
            debug!(md_ready = ready, "MarketData ready flag changed");
            self.notify_change();
        }
    }

    /// Set the orderUpdates snapshot complete flag.
    pub fn set_order_snapshot(&self, ready: bool) {
        let old = self.order_snapshot.swap(ready, Ordering::SeqCst);
        if old != ready {
            debug!(order_snapshot = ready, "Order snapshot flag changed");
            self.notify_change();
        }
    }

    /// Set the userFills snapshot complete flag.
    pub fn set_fills_snapshot(&self, ready: bool) {
        let old = self.fills_snapshot.swap(ready, Ordering::SeqCst);
        if old != ready {
            debug!(fills_snapshot = ready, "Fills snapshot flag changed");
            self.notify_change();
        }
    }

    /// Set the PositionTracker sync complete flag.
    pub fn set_position_synced(&self, ready: bool) {
        let old = self.position_synced.swap(ready, Ordering::SeqCst);
        if old != ready {
            debug!(position_synced = ready, "Position synced flag changed");
            self.notify_change();
        }
    }

    /// Check if all readiness conditions are met.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.md_ready.load(Ordering::SeqCst)
            && self.order_snapshot.load(Ordering::SeqCst)
            && self.fills_snapshot.load(Ordering::SeqCst)
            && self.position_synced.load(Ordering::SeqCst)
    }

    /// Get the current state of all flags.
    ///
    /// Returns (md_ready, order_snapshot, fills_snapshot, position_synced).
    #[must_use]
    pub fn flags(&self) -> (bool, bool, bool, bool) {
        (
            self.md_ready.load(Ordering::SeqCst),
            self.order_snapshot.load(Ordering::SeqCst),
            self.fills_snapshot.load(Ordering::SeqCst),
            self.position_synced.load(Ordering::SeqCst),
        )
    }

    /// Subscribe to readiness changes.
    ///
    /// Returns a new watch receiver that will be notified when
    /// readiness state changes.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.tx.subscribe()
    }

    /// Wait until trading is ready.
    ///
    /// Returns immediately if already ready, otherwise blocks until ready.
    pub async fn wait_until_ready(&self) {
        // Fast path: already ready
        if self.is_ready() {
            return;
        }

        // Slow path: wait for notification
        let mut rx = self.tx.subscribe();
        loop {
            if rx.changed().await.is_err() {
                // Sender dropped (should not happen in normal operation)
                tracing::warn!("TradingReadyChecker sender dropped while waiting");
                return;
            }

            if *rx.borrow() {
                return;
            }
        }
    }

    /// Notify subscribers of a potential readiness change.
    fn notify_change(&self) {
        let ready = self.is_ready();
        let flags = self.flags();

        if ready {
            info!(
                md_ready = flags.0,
                order_snapshot = flags.1,
                fills_snapshot = flags.2,
                position_synced = flags.3,
                "READY-TRADING: All conditions met, trading enabled"
            );
        } else {
            debug!(
                md_ready = flags.0,
                order_snapshot = flags.1,
                fills_snapshot = flags.2,
                position_synced = flags.3,
                is_ready = ready,
                "Trading readiness state updated"
            );
        }

        // Ignore send errors (no receivers)
        let _ = self.tx.send(ready);
    }

    /// Reset all flags to false.
    ///
    /// Useful for reconnection scenarios where state needs to be
    /// re-established.
    pub fn reset(&self) {
        self.md_ready.store(false, Ordering::SeqCst);
        self.order_snapshot.store(false, Ordering::SeqCst);
        self.fills_snapshot.store(false, Ordering::SeqCst);
        self.position_synced.store(false, Ordering::SeqCst);
        self.notify_change();
        debug!("TradingReadyChecker reset - all flags cleared");
    }
}

impl Default for TradingReadyChecker {
    fn default() -> Self {
        Self::new().0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_not_ready() {
        let (checker, _rx) = TradingReadyChecker::new();
        assert!(!checker.is_ready());
    }

    #[test]
    fn test_all_flags_required() {
        let (checker, _rx) = TradingReadyChecker::new();

        // Set flags one by one - should not be ready until all are set
        checker.set_md_ready(true);
        assert!(!checker.is_ready());

        checker.set_order_snapshot(true);
        assert!(!checker.is_ready());

        checker.set_fills_snapshot(true);
        assert!(!checker.is_ready());

        checker.set_position_synced(true);
        assert!(checker.is_ready());
    }

    #[test]
    fn test_any_flag_false_not_ready() {
        let (checker, _rx) = TradingReadyChecker::new();

        // Set all flags true
        checker.set_md_ready(true);
        checker.set_order_snapshot(true);
        checker.set_fills_snapshot(true);
        checker.set_position_synced(true);
        assert!(checker.is_ready());

        // Clearing any flag makes not ready
        checker.set_md_ready(false);
        assert!(!checker.is_ready());

        checker.set_md_ready(true);
        checker.set_order_snapshot(false);
        assert!(!checker.is_ready());

        checker.set_order_snapshot(true);
        checker.set_fills_snapshot(false);
        assert!(!checker.is_ready());

        checker.set_fills_snapshot(true);
        checker.set_position_synced(false);
        assert!(!checker.is_ready());
    }

    #[test]
    fn test_flags_returns_current_state() {
        let (checker, _rx) = TradingReadyChecker::new();

        assert_eq!(checker.flags(), (false, false, false, false));

        checker.set_md_ready(true);
        assert_eq!(checker.flags(), (true, false, false, false));

        checker.set_fills_snapshot(true);
        assert_eq!(checker.flags(), (true, false, true, false));
    }

    #[test]
    fn test_reset_clears_all_flags() {
        let (checker, _rx) = TradingReadyChecker::new();

        checker.set_md_ready(true);
        checker.set_order_snapshot(true);
        checker.set_fills_snapshot(true);
        checker.set_position_synced(true);
        assert!(checker.is_ready());

        checker.reset();

        assert!(!checker.is_ready());
        assert_eq!(checker.flags(), (false, false, false, false));
    }

    #[tokio::test]
    async fn test_watch_channel_notifications() {
        let (checker, mut rx) = TradingReadyChecker::new();

        // Initial state is false
        assert!(!*rx.borrow());

        // Set all flags
        checker.set_md_ready(true);
        checker.set_order_snapshot(true);
        checker.set_fills_snapshot(true);
        checker.set_position_synced(true);

        // Wait for notification
        rx.changed().await.unwrap();
        assert!(*rx.borrow());

        // Clear a flag
        checker.set_md_ready(false);

        // Wait for notification
        rx.changed().await.unwrap();
        assert!(!*rx.borrow());
    }

    #[test]
    fn test_subscribe_returns_new_receiver() {
        let (checker, rx1) = TradingReadyChecker::new();
        let rx2 = checker.subscribe();

        // Both receivers should have same value
        assert_eq!(*rx1.borrow(), *rx2.borrow());

        // Set flags to ready
        checker.set_md_ready(true);
        checker.set_order_snapshot(true);
        checker.set_fills_snapshot(true);
        checker.set_position_synced(true);

        // Both should now show ready
        assert!(*rx1.borrow());
        assert!(*rx2.borrow());
    }

    #[test]
    fn test_idempotent_flag_setting() {
        let (checker, _rx) = TradingReadyChecker::new();

        // Setting same value multiple times should be idempotent
        checker.set_md_ready(true);
        checker.set_md_ready(true);
        checker.set_md_ready(true);

        assert!(checker.flags().0);

        checker.set_md_ready(false);
        checker.set_md_ready(false);

        assert!(!checker.flags().0);
    }
}
