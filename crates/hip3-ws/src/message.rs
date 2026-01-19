//! WebSocket message types.

use serde::{Deserialize, Serialize};

/// Incoming WebSocket message wrapper.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WsMessage {
    /// Channel data message (bbo, assetCtx, etc.)
    Channel(ChannelMessage),
    /// Request response (subscriptionResponse, post response).
    Response(WsResponse),
    /// Pong response (no data field).
    Pong(PongMessage),
}

/// Channel-based message from subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub channel: String,
    pub data: serde_json::Value,
}

/// Response to a request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsResponse {
    pub channel: String,
    pub data: serde_json::Value,
}

/// Pong response message (Hyperliquid format: {"channel": "pong"}).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PongMessage {
    pub channel: String,
}

impl PongMessage {
    pub fn is_pong(&self) -> bool {
        self.channel == "pong"
    }
}

/// Ping message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingMessage {
    pub method: String,
}

/// Outgoing request to WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsRequest {
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
}

impl WsRequest {
    /// Create a ping request.
    pub fn ping() -> Self {
        Self {
            method: "ping".to_string(),
            subscription: None,
            id: None,
        }
    }

    /// Create a subscribe request.
    pub fn subscribe(subscription: serde_json::Value) -> Self {
        Self {
            method: "subscribe".to_string(),
            subscription: Some(subscription),
            id: None,
        }
    }

    /// Create an unsubscribe request.
    pub fn unsubscribe(subscription: serde_json::Value) -> Self {
        Self {
            method: "unsubscribe".to_string(),
            subscription: Some(subscription),
            id: None,
        }
    }
}
