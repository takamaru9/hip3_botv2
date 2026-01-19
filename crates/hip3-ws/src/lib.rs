//! WebSocket client for HIP-3 exchange connections.
//!
//! Provides robust WebSocket connectivity with:
//! - Automatic reconnection with exponential backoff
//! - Subscription management and READY state tracking
//! - Heartbeat monitoring (45s ping, pong timeout detection)
//! - Rate limiting (2000 msg/min, 100 inflight posts)
//! - Channel-based message routing

pub mod connection;
pub mod error;
pub mod heartbeat;
pub mod message;
pub mod rate_limiter;
pub mod subscription;

pub use connection::{ConnectionConfig, ConnectionManager, SubscriptionTarget};
pub use error::{WsError, WsResult};
pub use message::{ChannelMessage, PongMessage, WsMessage, WsRequest, WsResponse};
pub use subscription::{ReadyState, SubscriptionManager};

use std::sync::Once;

static INIT_CRYPTO: Once = Once::new();

/// Initialize the TLS crypto provider.
/// Must be called before any WebSocket connections are made.
pub fn init_crypto() {
    INIT_CRYPTO.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}
