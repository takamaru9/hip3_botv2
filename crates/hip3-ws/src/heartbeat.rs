//! Heartbeat management for WebSocket connections.
//!
//! Monitors connection health by tracking ping/pong timing and
//! message activity.

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;

/// Heartbeat manager for WebSocket connection health.
pub struct HeartbeatManager {
    /// Heartbeat interval (how often to send ping).
    interval_ms: u64,
    /// Timeout (how long to wait for pong).
    timeout_ms: u64,
    /// Last ping sent time.
    last_ping: Arc<RwLock<Option<DateTime<Utc>>>>,
    /// Last pong received time.
    last_pong: Arc<RwLock<Option<DateTime<Utc>>>>,
    /// Last message received time (any message).
    last_message: Arc<RwLock<DateTime<Utc>>>,
    /// Whether we're waiting for pong.
    waiting_for_pong: Arc<RwLock<bool>>,
}

impl HeartbeatManager {
    /// Create a new heartbeat manager.
    pub fn new(interval_ms: u64, timeout_ms: u64) -> Self {
        Self {
            interval_ms,
            timeout_ms,
            last_ping: Arc::new(RwLock::new(None)),
            last_pong: Arc::new(RwLock::new(None)),
            last_message: Arc::new(RwLock::new(Utc::now())),
            waiting_for_pong: Arc::new(RwLock::new(false)),
        }
    }

    /// Reset heartbeat state (called on connection).
    pub fn reset(&self) {
        *self.last_ping.write() = None;
        *self.last_pong.write() = None;
        *self.last_message.write() = Utc::now();
        *self.waiting_for_pong.write() = false;
    }

    /// Record that a ping was sent.
    pub fn record_ping(&self) {
        let now = Utc::now();
        *self.last_ping.write() = Some(now);
        *self.waiting_for_pong.write() = true;
        debug!(time = %now, "Recorded ping");
    }

    /// Record that a pong was received.
    pub fn record_pong(&self) {
        let now = Utc::now();
        *self.last_pong.write() = Some(now);
        *self.waiting_for_pong.write() = false;

        // Calculate round-trip time
        if let Some(ping_time) = *self.last_ping.read() {
            let rtt_ms = (now - ping_time).num_milliseconds();
            debug!(rtt_ms, "Received pong");
        }
    }

    /// Record that any message was received.
    pub fn record_message(&self) {
        *self.last_message.write() = Utc::now();
    }

    /// Check if heartbeat has timed out.
    pub fn is_timed_out(&self) -> bool {
        if !*self.waiting_for_pong.read() {
            return false;
        }

        if let Some(ping_time) = *self.last_ping.read() {
            let elapsed_ms = (Utc::now() - ping_time).num_milliseconds();
            return elapsed_ms > self.timeout_ms as i64;
        }

        false
    }

    /// Get time since last message.
    pub fn time_since_last_message_ms(&self) -> i64 {
        (Utc::now() - *self.last_message.read()).num_milliseconds()
    }

    /// Check if we should send a heartbeat.
    pub fn should_send_heartbeat(&self) -> bool {
        // Don't send if we're waiting for pong
        if *self.waiting_for_pong.read() {
            return false;
        }

        // Send if no message received for interval_ms
        self.time_since_last_message_ms() >= self.interval_ms as i64
    }

    /// Wait for the next heartbeat check.
    pub async fn wait_for_check(&self) {
        tokio::time::sleep(Duration::from_millis(self.interval_ms / 2)).await;
    }

    /// Get heartbeat statistics.
    pub fn stats(&self) -> HeartbeatStats {
        HeartbeatStats {
            last_ping: *self.last_ping.read(),
            last_pong: *self.last_pong.read(),
            last_message: *self.last_message.read(),
            waiting_for_pong: *self.waiting_for_pong.read(),
            time_since_last_message_ms: self.time_since_last_message_ms(),
        }
    }
}

/// Heartbeat statistics.
#[derive(Debug, Clone)]
pub struct HeartbeatStats {
    pub last_ping: Option<DateTime<Utc>>,
    pub last_pong: Option<DateTime<Utc>>,
    pub last_message: DateTime<Utc>,
    pub waiting_for_pong: bool,
    pub time_since_last_message_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heartbeat_initial_state() {
        let hb = HeartbeatManager::new(45000, 10000);
        assert!(!hb.is_timed_out());
        assert!(!*hb.waiting_for_pong.read());
    }

    #[test]
    fn test_heartbeat_ping_pong() {
        let hb = HeartbeatManager::new(45000, 10000);

        hb.record_ping();
        assert!(*hb.waiting_for_pong.read());

        hb.record_pong();
        assert!(!*hb.waiting_for_pong.read());
    }
}
