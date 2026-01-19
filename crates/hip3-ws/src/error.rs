//! WebSocket error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WsError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Connection closed: code={code}, reason={reason}")]
    ConnectionClosed { code: u16, reason: String },

    #[error("Send failed: {0}")]
    SendFailed(String),

    #[error("Message parse error: {0}")]
    ParseError(String),

    #[error("Subscription error: {0}")]
    SubscriptionError(String),

    #[error("Rate limit exceeded")]
    RateLimitExceeded,

    #[error("Heartbeat timeout")]
    HeartbeatTimeout,

    #[error("Not ready: {0}")]
    NotReady(String),

    #[error("Tungstenite error: {0}")]
    Tungstenite(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type WsResult<T> = Result<T, WsError>;
