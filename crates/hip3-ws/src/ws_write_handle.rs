//! WebSocket write handle for sending messages.
//!
//! Provides fire-and-forget sending API. Response tracking is handled
//! by the executor's PostRequestManager.

use crate::connection::ConnectionState;
use crate::rate_limiter::RateLimiter;
use crate::subscription::SubscriptionManager;
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::debug;

/// Outbound message to be sent via WebSocket.
#[derive(Debug)]
pub enum WsOutbound {
    /// Plain text message (subscriptions, ping, etc.).
    Text(String),
    /// Post request (order action) with tracking ID.
    Post {
        /// Post ID for response correlation.
        post_id: u64,
        /// JSON payload to send.
        payload: String,
    },
}

/// Error type for post operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostError {
    /// Rate limit exceeded.
    RateLimited,
    /// Channel closed (WebSocket disconnected or shutting down).
    ChannelClosed,
    /// Not ready (disconnected or READY-TRADING not achieved).
    NotReady,
}

impl std::fmt::Display for PostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimited => write!(f, "rate limited"),
            Self::ChannelClosed => write!(f, "channel closed"),
            Self::NotReady => write!(f, "not ready"),
        }
    }
}

impl std::error::Error for PostError {}

/// Write handle for sending messages to WebSocket.
///
/// This handle provides a channel-based API for sending messages,
/// which is reconnect-safe and avoids lifetime issues with direct
/// WebSocket access.
///
/// # Response Tracking
///
/// `post()` is fire-and-forget - it only confirms that the message
/// was queued for sending. Response tracking is handled by the
/// executor's PostRequestManager. Responses flow through the
/// message stream.
#[derive(Clone)]
pub struct WsWriteHandle {
    tx: mpsc::Sender<WsOutbound>,
    rate_limiter: Arc<RateLimiter>,
    state: Arc<RwLock<ConnectionState>>,
    subscriptions: Arc<SubscriptionManager>,
}

impl WsWriteHandle {
    /// Create a new write handle.
    pub fn new(
        tx: mpsc::Sender<WsOutbound>,
        rate_limiter: Arc<RateLimiter>,
        state: Arc<RwLock<ConnectionState>>,
        subscriptions: Arc<SubscriptionManager>,
    ) -> Self {
        Self {
            tx,
            rate_limiter,
            state,
            subscriptions,
        }
    }

    /// Send a post request (fire-and-forget).
    ///
    /// This method queues the post request for sending. It does NOT
    /// wait for a response from the exchange. The response will arrive
    /// via the message stream and should be handled by the executor.
    ///
    /// # Errors
    ///
    /// - `PostError::NotReady`: Connection is not ready for trading
    /// - `PostError::RateLimited`: Rate limit exceeded
    /// - `PostError::ChannelClosed`: WebSocket channel is closed
    ///
    /// # Inflight Tracking
    ///
    /// `record_post_send()` is called after successful queue insertion.
    /// `record_post_response()` is called by ConnectionManager when
    /// a post response is received.
    pub async fn post(&self, post_id: u64, payload: String) -> Result<(), PostError> {
        // 1. Check connection state and READY-TRADING
        if !self.is_ready() {
            return Err(PostError::NotReady);
        }

        // 2. Check rate limit
        if !self.rate_limiter.can_send_post() {
            return Err(PostError::RateLimited);
        }

        // 3. Queue the message
        self.tx
            .send(WsOutbound::Post { post_id, payload })
            .await
            .map_err(|_| PostError::ChannelClosed)?;

        // 4. Record post send after successful queue insertion
        self.rate_limiter.record_post_send();
        debug!(post_id, "Post queued for sending");

        Ok(())
    }

    /// Send a raw text message (subscriptions, ping, etc.).
    ///
    /// This method does not apply rate limiting (subscriptions are
    /// low frequency) but does check connection state.
    ///
    /// # Errors
    ///
    /// - `PostError::NotReady`: Connection is not connected
    /// - `PostError::ChannelClosed`: WebSocket channel is closed
    pub async fn send_text(&self, text: String) -> Result<(), PostError> {
        if !self.is_connected() {
            return Err(PostError::NotReady);
        }

        self.tx
            .send(WsOutbound::Text(text))
            .await
            .map_err(|_| PostError::ChannelClosed)?;

        Ok(())
    }

    /// Check if ready to send posts.
    ///
    /// Returns true if:
    /// - Connection state is Connected
    /// - READY-TRADING achieved (orderUpdates subscription received)
    /// - Channel is open
    ///
    /// Note: Rate limit is NOT checked here. It is checked separately in
    /// `post()` to allow distinguishing `NotReady` from `RateLimited`.
    pub fn is_ready(&self) -> bool {
        let state = *self.state.read();
        state == ConnectionState::Connected
            && self.subscriptions.is_ready() // READY-TRADING
            && !self.tx.is_closed()
    }

    /// Check if connected (for subscriptions, doesn't require READY-TRADING).
    ///
    /// Returns true if:
    /// - Connection state is Connected
    /// - Channel is open
    pub fn is_connected(&self) -> bool {
        let state = *self.state.read();
        state == ConnectionState::Connected && !self.tx.is_closed()
    }

    /// Get current inflight count.
    pub fn inflight_count(&self) -> u32 {
        self.rate_limiter.inflight_count()
    }

    /// Get current connection state.
    pub fn connection_state(&self) -> ConnectionState {
        *self.state.read()
    }

    /// Check if the underlying channel is closed.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::ConnectionState;

    fn create_test_handle() -> (WsWriteHandle, mpsc::Receiver<WsOutbound>) {
        let (tx, rx) = mpsc::channel(100);
        let rate_limiter = Arc::new(RateLimiter::new(2000, 60));
        let state = Arc::new(RwLock::new(ConnectionState::Connected));
        let subscriptions = Arc::new(SubscriptionManager::new());

        // Mark as READY-TRADING
        subscriptions.handle_message("bbo:BTC");
        subscriptions.handle_message("activeAssetCtx:perp:0");
        subscriptions.handle_message("orderUpdates:user:test");

        let handle = WsWriteHandle::new(tx, rate_limiter, state, subscriptions);
        (handle, rx)
    }

    #[tokio::test]
    async fn test_post_success() {
        let (handle, mut rx) = create_test_handle();

        let result = handle.post(1, "test payload".to_string()).await;
        assert!(result.is_ok());

        let msg = rx.recv().await.unwrap();
        match msg {
            WsOutbound::Post { post_id, payload } => {
                assert_eq!(post_id, 1);
                assert_eq!(payload, "test payload");
            }
            _ => panic!("expected Post message"),
        }

        assert_eq!(handle.inflight_count(), 1);
    }

    #[tokio::test]
    async fn test_post_not_ready_disconnected() {
        let (tx, _rx) = mpsc::channel(100);
        let rate_limiter = Arc::new(RateLimiter::new(2000, 60));
        let state = Arc::new(RwLock::new(ConnectionState::Disconnected));
        let subscriptions = Arc::new(SubscriptionManager::new());

        let handle = WsWriteHandle::new(tx, rate_limiter, state, subscriptions);

        let result = handle.post(1, "test".to_string()).await;
        assert_eq!(result, Err(PostError::NotReady));
    }

    #[tokio::test]
    async fn test_post_not_ready_no_subscriptions() {
        let (tx, _rx) = mpsc::channel(100);
        let rate_limiter = Arc::new(RateLimiter::new(2000, 60));
        let state = Arc::new(RwLock::new(ConnectionState::Connected));
        let subscriptions = Arc::new(SubscriptionManager::new());

        let handle = WsWriteHandle::new(tx, rate_limiter, state, subscriptions);

        let result = handle.post(1, "test".to_string()).await;
        assert_eq!(result, Err(PostError::NotReady));
    }

    #[tokio::test]
    async fn test_send_text_success() {
        let (handle, mut rx) = create_test_handle();

        // send_text doesn't require READY-TRADING, just Connected
        let result = handle.send_text("subscription msg".to_string()).await;
        assert!(result.is_ok());

        let msg = rx.recv().await.unwrap();
        match msg {
            WsOutbound::Text(text) => {
                assert_eq!(text, "subscription msg");
            }
            _ => panic!("expected Text message"),
        }
    }

    #[tokio::test]
    async fn test_is_ready() {
        let (handle, _rx) = create_test_handle();
        assert!(handle.is_ready());
        assert!(handle.is_connected());
    }

    #[tokio::test]
    async fn test_is_connected_without_ready_trading() {
        let (tx, _rx) = mpsc::channel(100);
        let rate_limiter = Arc::new(RateLimiter::new(2000, 60));
        let state = Arc::new(RwLock::new(ConnectionState::Connected));
        let subscriptions = Arc::new(SubscriptionManager::new());

        // Only mark as READY-MD, not READY-TRADING
        subscriptions.handle_message("bbo:BTC");
        subscriptions.handle_message("activeAssetCtx:perp:0");

        let handle = WsWriteHandle::new(tx, rate_limiter, state, subscriptions);

        // is_connected should be true (for subscriptions)
        assert!(handle.is_connected());
        // is_ready should be false (no orderUpdates)
        assert!(!handle.is_ready());
    }
}
