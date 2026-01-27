//! WebSocket broadcast functionality.
//!
//! The broadcaster collects state updates at a fixed interval and broadcasts
//! them to all connected WebSocket clients.

use std::time::Duration;

use tokio::sync::broadcast;
use tracing::{debug, trace};

use crate::state::DashboardState;
use crate::types::DashboardMessage;

/// Run the broadcaster task.
///
/// This function collects state updates at the specified interval and sends
/// them to the broadcast channel. All connected WebSocket clients receive
/// these updates.
pub async fn run_broadcaster(
    state: DashboardState,
    tx: broadcast::Sender<String>,
    interval_ms: u64,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(interval_ms));

    // Track previous state for change detection (optional optimization)
    let mut last_hard_stop = state.is_hard_stop_triggered();

    loop {
        interval.tick().await;

        // Check for hard stop state change (send alert)
        let current_hard_stop = state.is_hard_stop_triggered();
        if current_hard_stop && !last_hard_stop {
            // Hard stop just triggered - send alert
            let alert = DashboardMessage::RiskAlert {
                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                alert_type: crate::types::RiskAlertType::HardStop,
                message: state
                    .get_hard_stop_reason()
                    .unwrap_or_else(|| "Unknown reason".to_string()),
            };
            if let Ok(json) = serde_json::to_string(&alert) {
                let _ = tx.send(json);
            }
        }
        last_hard_stop = current_hard_stop;

        // Collect and send update
        let snapshot = state.collect_snapshot();
        let msg = DashboardMessage::Update {
            timestamp_ms: snapshot.timestamp_ms,
            markets: Some(snapshot.markets),
            positions: Some(snapshot.positions),
            risk: Some(snapshot.risk),
            pending_orders: Some(snapshot.pending_orders),
        };

        match serde_json::to_string(&msg) {
            Ok(json) => {
                // Send to broadcast channel
                // Ignore errors (no receivers connected)
                match tx.send(json) {
                    Ok(n) => {
                        trace!(receivers = n, "Broadcast update sent");
                    }
                    Err(_) => {
                        // No receivers - this is normal when no clients connected
                        trace!("No WebSocket receivers connected");
                    }
                }
            }
            Err(e) => {
                debug!(error = %e, "Failed to serialize dashboard update");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_broadcast_channel() {
        let (tx, _rx) = broadcast::channel::<String>(16);

        // Subscribe before sending
        let mut rx2 = tx.subscribe();
        let result = tx.send("test".to_string());
        assert!(result.is_ok());

        // Receiver should get the message
        assert_eq!(rx2.recv().await.unwrap(), "test");
    }
}
