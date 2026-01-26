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
pub mod ws_write_handle;

pub use connection::{ConnectionConfig, ConnectionManager, ConnectionState, SubscriptionTarget};
pub use error::{WsError, WsResult};
pub use message::{
    extract_subscription_type, is_order_updates_channel, ActionResponseDetails,
    ActionResponsePayload, ChannelMessage, FillPayload, OrderInfo, OrderUpdatePayload,
    OrderUpdatesResult, PongMessage, PostPayload, PostRequest, PostRequestBody, PostResponseBody,
    PostResponseData, SignaturePayload, WsMessage, WsRequest,
};
pub use subscription::{ReadyState, SubscriptionManager};
pub use ws_write_handle::{PostError, WsOutbound, WsWriteHandle};

use std::sync::Once;

static INIT_CRYPTO: Once = Once::new();

/// Initialize the TLS crypto provider.
/// Must be called before any WebSocket connections are made.
pub fn init_crypto() {
    INIT_CRYPTO.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}
