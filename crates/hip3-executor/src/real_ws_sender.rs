//! Real WebSocket sender implementation.
//!
//! This module provides the production implementation of the `WsSender` trait
//! that uses the hip3-ws `WsWriteHandle` for actual WebSocket communication.

use crate::ws_sender::{BoxFuture, SendResult, SignedAction, WsSender};
use hip3_ws::{PostError, PostPayload, PostRequest, SignaturePayload, WsWriteHandle};

/// Real WebSocket sender using hip3-ws.
///
/// This implementation converts `SignedAction` to the Hyperliquid post request
/// format and sends it via `WsWriteHandle`.
///
/// # Response Handling
///
/// `send()` only confirms that the request was queued for sending (fire-and-forget).
/// Actual responses flow through the message stream and should be handled by the
/// executor's `on_response_ok` / `on_response_rejected` methods.
pub struct RealWsSender {
    handle: WsWriteHandle,
    vault_address: Option<String>,
}

impl RealWsSender {
    /// Create a new RealWsSender with the given write handle.
    pub fn new(handle: WsWriteHandle, vault_address: Option<String>) -> Self {
        Self {
            handle,
            vault_address,
        }
    }
}

impl WsSender for RealWsSender {
    fn send(&self, action: SignedAction) -> BoxFuture<'_, SendResult> {
        Box::pin(async move {
            // Convert SignedAction to PostRequest JSON
            let action_value = match serde_json::to_value(&action.action) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(error = ?e, "Action serialization failed");
                    return SendResult::Error(format!("Action serialization: {e}"));
                }
            };

            let payload = PostPayload {
                action: action_value,
                nonce: action.nonce,
                signature: SignaturePayload {
                    r: action.signature.r.clone(),
                    s: action.signature.s.clone(),
                    // Hyperliquid uses integer v (27 or 28) per Python SDK
                    v: action.signature.v,
                },
                // vaultAddress is omitted (not serialized) when None for personal trading
                vault_address: self.vault_address.clone(),
            };

            let request = PostRequest::new(action.post_id, "action".to_string(), payload);

            let json = match serde_json::to_string(&request) {
                Ok(j) => j,
                Err(e) => {
                    tracing::error!(error = ?e, "PostRequest serialization failed");
                    return SendResult::Error(format!("PostRequest serialization: {e}"));
                }
            };

            match self.handle.post(action.post_id, json).await {
                Ok(()) => SendResult::Sent,
                Err(PostError::RateLimited) => SendResult::RateLimited,
                Err(PostError::ChannelClosed) => SendResult::Disconnected,
                // NotReady = disconnected or READY-TRADING not achieved → retryable
                Err(PostError::NotReady) => SendResult::Disconnected,
            }
        })
    }

    fn is_ready(&self) -> bool {
        self.handle.is_ready()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signer::Action;
    use crate::ws_sender::ActionSignature;
    use hip3_ws::connection::ConnectionState;
    use hip3_ws::rate_limiter::RateLimiter;
    use hip3_ws::subscription::SubscriptionManager;
    use parking_lot::RwLock;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn create_test_sender() -> (RealWsSender, mpsc::Receiver<hip3_ws::WsOutbound>) {
        let (tx, rx) = mpsc::channel(100);
        let rate_limiter = Arc::new(RateLimiter::new(2000, 60));
        let state = Arc::new(RwLock::new(ConnectionState::Connected));
        let subscriptions = Arc::new(SubscriptionManager::new());

        // Mark as READY-TRADING
        subscriptions.handle_message("bbo:BTC");
        subscriptions.handle_message("activeAssetCtx:perp:0");
        subscriptions.handle_message("orderUpdates:user:test");

        let handle = WsWriteHandle::new(tx, rate_limiter, state, subscriptions);
        (RealWsSender::new(handle, None), rx)
    }

    fn sample_signed_action() -> SignedAction {
        SignedAction {
            action: Action {
                action_type: "order".to_string(),
                orders: Some(vec![]),
                cancels: None,
                grouping: Some("na".to_string()),
                builder: None,
            },
            nonce: 12345,
            signature: ActionSignature {
                r: "abc123".to_string(),
                s: "def456".to_string(),
                v: 27,
            },
            post_id: 1,
        }
    }

    #[tokio::test]
    async fn test_send_success() {
        let (sender, mut rx) = create_test_sender();

        let result = sender.send(sample_signed_action()).await;
        assert!(result.is_success());

        // Verify message was queued
        let msg = rx.recv().await.unwrap();
        match msg {
            hip3_ws::WsOutbound::Post { post_id, payload } => {
                assert_eq!(post_id, 1);
                // Verify JSON structure
                let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
                assert_eq!(parsed["method"], "post");
                assert_eq!(parsed["id"], 1);
                assert_eq!(parsed["request"]["type"], "action");
                assert_eq!(parsed["request"]["payload"]["nonce"], 12345);
            }
            _ => panic!("expected Post message"),
        }
    }

    #[tokio::test]
    async fn test_is_ready() {
        let (sender, _rx) = create_test_sender();
        assert!(sender.is_ready());
    }

    #[tokio::test]
    async fn test_not_ready_when_disconnected() {
        let (tx, _rx) = mpsc::channel(100);
        let rate_limiter = Arc::new(RateLimiter::new(2000, 60));
        let state = Arc::new(RwLock::new(ConnectionState::Disconnected));
        let subscriptions = Arc::new(SubscriptionManager::new());

        let handle = WsWriteHandle::new(tx, rate_limiter, state, subscriptions);
        let sender = RealWsSender::new(handle, None);

        assert!(!sender.is_ready());

        let result = sender.send(sample_signed_action()).await;
        assert!(result.is_retryable());
        assert!(matches!(result, SendResult::Disconnected));
    }

    #[tokio::test]
    async fn test_not_ready_without_subscriptions() {
        let (tx, _rx) = mpsc::channel(100);
        let rate_limiter = Arc::new(RateLimiter::new(2000, 60));
        let state = Arc::new(RwLock::new(ConnectionState::Connected));
        let subscriptions = Arc::new(SubscriptionManager::new());

        // Only READY-MD, not READY-TRADING
        subscriptions.handle_message("bbo:BTC");
        subscriptions.handle_message("activeAssetCtx:perp:0");

        let handle = WsWriteHandle::new(tx, rate_limiter, state, subscriptions);
        let sender = RealWsSender::new(handle, None);

        assert!(!sender.is_ready());
    }
}
