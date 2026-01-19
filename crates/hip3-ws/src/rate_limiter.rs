//! Rate limiting for WebSocket messages.
//!
//! Implements token bucket rate limiting to prevent exceeding
//! exchange rate limits (2000 msg/min, 100 inflight posts).

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::warn;

/// Token bucket rate limiter.
pub struct RateLimiter {
    /// Maximum messages per window.
    max_messages: u32,
    /// Window size in seconds.
    window_secs: u64,
    /// Timestamps of recent messages.
    timestamps: Arc<Mutex<VecDeque<Instant>>>,
    /// Current inflight count (for posts).
    inflight: Arc<Mutex<u32>>,
    /// Maximum inflight messages.
    max_inflight: u32,
}

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// # Arguments
    /// * `max_messages` - Maximum messages per window
    /// * `window_secs` - Window size in seconds
    pub fn new(max_messages: u32, window_secs: u64) -> Self {
        Self {
            max_messages,
            window_secs,
            timestamps: Arc::new(Mutex::new(VecDeque::with_capacity(max_messages as usize))),
            inflight: Arc::new(Mutex::new(0)),
            max_inflight: 100, // HIP-3 limit
        }
    }

    /// Check if we can send a message.
    pub fn can_send(&self) -> bool {
        self.cleanup_old_timestamps();

        let timestamps = self.timestamps.lock();
        timestamps.len() < self.max_messages as usize
    }

    /// Check if we can send a post (order) message.
    pub fn can_send_post(&self) -> bool {
        self.can_send() && *self.inflight.lock() < self.max_inflight
    }

    /// Record a message send.
    pub fn record_send(&self) {
        self.cleanup_old_timestamps();

        let mut timestamps = self.timestamps.lock();
        timestamps.push_back(Instant::now());

        if timestamps.len() >= self.max_messages as usize {
            warn!(
                count = timestamps.len(),
                max = self.max_messages,
                "Approaching rate limit"
            );
        }
    }

    /// Record a post (order) message send.
    pub fn record_post_send(&self) {
        self.record_send();
        *self.inflight.lock() += 1;
    }

    /// Record a post response received.
    pub fn record_post_response(&self) {
        let mut inflight = self.inflight.lock();
        *inflight = inflight.saturating_sub(1);
    }

    /// Get current message count in window.
    pub fn current_count(&self) -> u32 {
        self.cleanup_old_timestamps();
        self.timestamps.lock().len() as u32
    }

    /// Get current inflight count.
    pub fn inflight_count(&self) -> u32 {
        *self.inflight.lock()
    }

    /// Get remaining capacity.
    pub fn remaining_capacity(&self) -> u32 {
        self.max_messages.saturating_sub(self.current_count())
    }

    /// Wait until we can send a message.
    pub async fn wait_for_capacity(&self) {
        while !self.can_send() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Wait until we can send a post message.
    pub async fn wait_for_post_capacity(&self) {
        while !self.can_send_post() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn cleanup_old_timestamps(&self) {
        let window = Duration::from_secs(self.window_secs);
        let cutoff = Instant::now() - window;

        let mut timestamps = self.timestamps.lock();
        while timestamps.front().is_some_and(|&t| t < cutoff) {
            timestamps.pop_front();
        }
    }

    /// Reset rate limiter state.
    pub fn reset(&self) {
        self.timestamps.lock().clear();
        *self.inflight.lock() = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter_basic() {
        let limiter = RateLimiter::new(10, 60);

        assert!(limiter.can_send());
        assert_eq!(limiter.current_count(), 0);

        for _ in 0..5 {
            limiter.record_send();
        }

        assert!(limiter.can_send());
        assert_eq!(limiter.current_count(), 5);
        assert_eq!(limiter.remaining_capacity(), 5);
    }

    #[test]
    fn test_rate_limiter_at_limit() {
        let limiter = RateLimiter::new(5, 60);

        for _ in 0..5 {
            limiter.record_send();
        }

        assert!(!limiter.can_send());
        assert_eq!(limiter.remaining_capacity(), 0);
    }

    #[test]
    fn test_inflight_tracking() {
        let limiter = RateLimiter::new(100, 60);

        limiter.record_post_send();
        limiter.record_post_send();
        assert_eq!(limiter.inflight_count(), 2);

        limiter.record_post_response();
        assert_eq!(limiter.inflight_count(), 1);
    }
}
