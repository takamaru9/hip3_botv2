//! WebSocket sender trait for order execution.
//!
//! Provides a trait-based abstraction for sending signed actions to the exchange.
//! This allows for:
//! - Dependency injection for testing
//! - Separation of signing from transport
//! - Future flexibility in transport implementation

use std::pin::Pin;
use std::sync::Arc;

use crate::signer::Action;

/// Boxed future for dyn-compatible async trait methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Result of a WebSocket send operation.
#[derive(Debug, Clone)]
pub enum SendResult {
    /// Successfully sent to WebSocket.
    Sent,
    /// WebSocket is disconnected.
    Disconnected,
    /// Rate limit exceeded.
    RateLimited,
    /// Send failed with error.
    Error(String),
}

impl SendResult {
    /// Check if the send was successful.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, SendResult::Sent)
    }

    /// Check if the error is retryable.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, SendResult::Disconnected | SendResult::RateLimited)
    }
}

/// Signed action ready for transmission.
#[derive(Debug, Clone)]
pub struct SignedAction {
    /// The action to send.
    pub action: Action,
    /// Nonce used for signing.
    pub nonce: u64,
    /// Signature (r, s, v in hex).
    pub signature: ActionSignature,
    /// Post ID for response correlation.
    pub post_id: u64,
}

/// EIP-712 signature components.
#[derive(Debug, Clone)]
pub struct ActionSignature {
    /// r component (hex with 0x prefix, e.g., "0x1a2b...").
    pub r: String,
    /// s component (hex with 0x prefix, e.g., "0x3c4d...").
    pub s: String,
    /// v component (recovery id, 27 or 28).
    pub v: u8,
}

impl ActionSignature {
    /// Create from raw signature bytes (65 bytes: r(32) + s(32) + v(1)).
    ///
    /// Normalizes v value from EIP-2098 format (0/1) to EIP-155 format (27/28).
    /// Adds 0x prefix to r and s components.
    pub fn from_bytes(bytes: &[u8; 65]) -> Self {
        let v_raw = bytes[64];
        // Normalize v: if 0/1 (EIP-2098), convert to 27/28 (EIP-155)
        let v = if v_raw < 27 { v_raw + 27 } else { v_raw };
        Self {
            r: format!("0x{}", hex::encode(&bytes[0..32])),
            s: format!("0x{}", hex::encode(&bytes[32..64])),
            v,
        }
    }
}

/// Trait for sending actions over WebSocket.
///
/// This trait abstracts the WebSocket send operation, allowing for:
/// - Unit testing with mock implementations
/// - Different transport backends (e.g., test harness, real WS)
pub trait WsSender: Send + Sync {
    /// Send a signed action.
    ///
    /// Returns a future that resolves to the send result.
    fn send(&self, action: SignedAction) -> BoxFuture<'_, SendResult>;

    /// Check if the connection is ready for sending.
    fn is_ready(&self) -> bool;
}

/// Mock WebSocket sender for testing.
#[derive(Debug)]
pub struct MockWsSender {
    /// Recorded sends for verification.
    sends: parking_lot::Mutex<Vec<SignedAction>>,
    /// Next result to return.
    next_result: parking_lot::Mutex<SendResult>,
    /// Whether the mock is ready.
    ready: std::sync::atomic::AtomicBool,
}

impl Default for MockWsSender {
    fn default() -> Self {
        Self::new()
    }
}

impl MockWsSender {
    /// Create a new mock sender.
    pub fn new() -> Self {
        Self {
            sends: parking_lot::Mutex::new(Vec::new()),
            next_result: parking_lot::Mutex::new(SendResult::Sent),
            ready: std::sync::atomic::AtomicBool::new(true),
        }
    }

    /// Set the next result to return.
    pub fn set_next_result(&self, result: SendResult) {
        *self.next_result.lock() = result;
    }

    /// Set whether the mock is ready.
    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, std::sync::atomic::Ordering::SeqCst);
    }

    /// Get recorded sends.
    pub fn get_sends(&self) -> Vec<SignedAction> {
        self.sends.lock().clone()
    }

    /// Clear recorded sends.
    pub fn clear_sends(&self) {
        self.sends.lock().clear();
    }
}

impl WsSender for MockWsSender {
    fn send(&self, action: SignedAction) -> BoxFuture<'_, SendResult> {
        Box::pin(async move {
            self.sends.lock().push(action);
            self.next_result.lock().clone()
        })
    }

    fn is_ready(&self) -> bool {
        self.ready.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Arc wrapper for WsSender trait objects.
pub type DynWsSender = Arc<dyn WsSender>;

/// Builder for creating SignedAction from Action and signature.
pub struct SignedActionBuilder {
    action: Action,
    nonce: u64,
    post_id: u64,
}

impl SignedActionBuilder {
    /// Create a new builder.
    pub fn new(action: Action, nonce: u64, post_id: u64) -> Self {
        Self {
            action,
            nonce,
            post_id,
        }
    }

    /// Build with signature bytes.
    pub fn with_signature(self, sig_bytes: &[u8; 65]) -> SignedAction {
        SignedAction {
            action: self.action,
            nonce: self.nonce,
            signature: ActionSignature::from_bytes(sig_bytes),
            post_id: self.post_id,
        }
    }

    /// Build with signature components.
    pub fn with_signature_parts(self, r: String, s: String, v: u8) -> SignedAction {
        SignedAction {
            action: self.action,
            nonce: self.nonce,
            signature: ActionSignature { r, s, v },
            post_id: self.post_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_action() -> Action {
        Action {
            action_type: "order".to_string(),
            orders: Some(vec![]),
            cancels: None,
            grouping: Some("na".to_string()),
            builder: None,
        }
    }

    #[tokio::test]
    async fn test_mock_sender_records_sends() {
        let sender = MockWsSender::new();

        let signed = SignedAction {
            action: sample_action(),
            nonce: 12345,
            signature: ActionSignature {
                r: "abc".to_string(),
                s: "def".to_string(),
                v: 27,
            },
            post_id: 1,
        };

        let result = sender.send(signed).await;
        assert!(result.is_success());
        assert_eq!(sender.get_sends().len(), 1);
    }

    #[tokio::test]
    async fn test_mock_sender_returns_configured_result() {
        let sender = MockWsSender::new();
        sender.set_next_result(SendResult::Disconnected);

        let signed = SignedAction {
            action: sample_action(),
            nonce: 12345,
            signature: ActionSignature {
                r: "abc".to_string(),
                s: "def".to_string(),
                v: 27,
            },
            post_id: 1,
        };

        let result = sender.send(signed).await;
        assert!(result.is_retryable());
    }

    #[test]
    fn test_send_result_properties() {
        assert!(SendResult::Sent.is_success());
        assert!(!SendResult::Disconnected.is_success());

        assert!(SendResult::Disconnected.is_retryable());
        assert!(SendResult::RateLimited.is_retryable());
        assert!(!SendResult::Sent.is_retryable());
        assert!(!SendResult::Error("test".to_string()).is_retryable());
    }

    #[test]
    fn test_signature_from_bytes() {
        let mut bytes = [0u8; 65];
        bytes[0..32].copy_from_slice(&[0xab; 32]);
        bytes[32..64].copy_from_slice(&[0xcd; 32]);
        bytes[64] = 28;

        let sig = ActionSignature::from_bytes(&bytes);
        assert_eq!(sig.r.len(), 66); // "0x" + 64 hex chars
        assert!(sig.r.starts_with("0x"));
        assert_eq!(sig.s.len(), 66);
        assert!(sig.s.starts_with("0x"));
        assert_eq!(sig.v, 28);
    }

    #[test]
    fn test_signature_from_bytes_normalizes_v() {
        let mut bytes = [0u8; 65];
        bytes[0..32].copy_from_slice(&[0xab; 32]);
        bytes[32..64].copy_from_slice(&[0xcd; 32]);
        bytes[64] = 1; // EIP-2098 format

        let sig = ActionSignature::from_bytes(&bytes);
        assert_eq!(sig.v, 28); // 1 + 27 = 28 normalized
        assert!(sig.r.starts_with("0x"));
        assert!(sig.s.starts_with("0x"));
    }

    #[test]
    fn test_signature_from_bytes_normalizes_v_zero() {
        let mut bytes = [0u8; 65];
        bytes[0..32].copy_from_slice(&[0xab; 32]);
        bytes[32..64].copy_from_slice(&[0xcd; 32]);
        bytes[64] = 0; // EIP-2098 format

        let sig = ActionSignature::from_bytes(&bytes);
        assert_eq!(sig.v, 27); // 0 + 27 = 27 normalized
    }
}
