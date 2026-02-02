//! WebSocket message types.

use serde::{Deserialize, Serialize};

// ============================================================================
// Post Request (Outgoing) - for order submission
// ============================================================================

/// Post request for order actions.
#[derive(Debug, Clone, Serialize)]
pub struct PostRequest {
    /// Method is always "post".
    pub method: String,
    /// Unique request ID for response correlation.
    pub id: u64,
    /// Request body containing the action.
    pub request: PostRequestBody,
}

impl PostRequest {
    /// Create a new post request from components.
    pub fn new(id: u64, request_type: String, payload: PostPayload) -> Self {
        Self {
            method: "post".to_string(),
            id,
            request: PostRequestBody {
                request_type,
                payload,
            },
        }
    }
}

/// Post request body.
#[derive(Debug, Clone, Serialize)]
pub struct PostRequestBody {
    /// Request type, typically "action".
    #[serde(rename = "type")]
    pub request_type: String,
    /// The action payload.
    pub payload: PostPayload,
}

/// Payload for a post request.
#[derive(Debug, Clone, Serialize)]
pub struct PostPayload {
    /// The action to perform (order, cancel, etc.).
    pub action: serde_json::Value,
    /// Nonce for replay protection.
    pub nonce: u64,
    /// EIP-712 signature.
    pub signature: SignaturePayload,
    /// Vault address for vault trading. Omitted for personal trading.
    #[serde(rename = "vaultAddress", skip_serializing_if = "Option::is_none")]
    pub vault_address: Option<String>,
}

/// Signature payload for EIP-712 signed actions.
#[derive(Debug, Clone, Serialize)]
pub struct SignaturePayload {
    /// r component (hex with 0x prefix, e.g., "0x1a2b...").
    pub r: String,
    /// s component (hex with 0x prefix, e.g., "0x3c4d...").
    pub s: String,
    /// Recovery ID (27 or 28) - Hyperliquid uses integer format per Python SDK.
    pub v: u8,
}

// ============================================================================
// Post Response (Incoming) - order submission result
// ============================================================================

/// Post response data structure.
#[derive(Debug, Clone, Deserialize)]
pub struct PostResponseData {
    /// The request ID this response corresponds to.
    pub id: u64,
    /// The response body (success or error).
    pub response: PostResponseBody,
}

/// Post response body - either success or error.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum PostResponseBody {
    /// Successful action response.
    #[serde(rename = "action")]
    Action {
        /// The action response payload.
        payload: ActionResponsePayload,
    },
    /// Error response.
    #[serde(rename = "error")]
    Error {
        /// Error message.
        payload: String,
    },
}

impl PostResponseBody {
    /// Check if this is a success response.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Action { .. })
    }

    /// Check if this is an error response.
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error { .. })
    }

    /// Get the error message if this is an error.
    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::Error { payload } => Some(payload),
            _ => None,
        }
    }
}

/// Successful action response payload.
/// Contains order statuses from the exchange.
#[derive(Debug, Clone, Deserialize)]
pub struct ActionResponsePayload {
    /// Status of the action.
    pub status: String,
    /// Response details (contains order statuses).
    pub response: ActionResponseDetails,
}

/// Response details containing order statuses.
#[derive(Debug, Clone, Deserialize)]
pub struct ActionResponseDetails {
    /// Type of response (e.g., "order").
    #[serde(rename = "type")]
    pub response_type: String,
    /// Order status data - flexible JSON to handle various response formats.
    #[serde(default)]
    pub data: serde_json::Value,
}

/// Order status from post response statuses array.
///
/// Hyperliquid returns one of three status types:
/// - `resting`: Order is on the order book
/// - `filled`: Order was immediately filled
/// - `error`: Order was rejected
#[derive(Debug, Clone)]
pub enum OrderResponseStatus {
    /// Order is resting on order book.
    Resting {
        /// Exchange order ID.
        oid: u64,
    },
    /// Order was immediately filled.
    Filled {
        /// Exchange order ID.
        oid: u64,
        /// Total filled size as string.
        total_sz: String,
        /// Average fill price as string.
        avg_px: String,
    },
    /// Order was rejected.
    Error {
        /// Error message.
        message: String,
    },
}

impl ActionResponsePayload {
    /// Parse statuses array from response.
    ///
    /// The data field contains: `{"statuses": [...]}`
    /// Each status is one of:
    /// - `{"resting": {"oid": 12345}}`
    /// - `{"filled": {"totalSz": "0.02", "avgPx": "1891.4", "oid": 12345}}`
    /// - `{"error": "Error message"}`
    pub fn parse_statuses(&self) -> Vec<OrderResponseStatus> {
        let mut results = Vec::new();

        // Get statuses array from data
        let statuses = match self.response.data.get("statuses") {
            Some(serde_json::Value::Array(arr)) => arr,
            _ => return results,
        };

        for status in statuses {
            // Try parsing as resting
            if let Some(resting) = status.get("resting") {
                if let Some(oid) = resting.get("oid").and_then(|v| v.as_u64()) {
                    results.push(OrderResponseStatus::Resting { oid });
                    continue;
                }
            }

            // Try parsing as filled
            if let Some(filled) = status.get("filled") {
                let oid = filled.get("oid").and_then(|v| v.as_u64()).unwrap_or(0);
                let total_sz = filled
                    .get("totalSz")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0")
                    .to_string();
                let avg_px = filled
                    .get("avgPx")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0")
                    .to_string();
                results.push(OrderResponseStatus::Filled {
                    oid,
                    total_sz,
                    avg_px,
                });
                continue;
            }

            // Try parsing as error
            if let Some(error) = status.get("error") {
                let message = error.as_str().unwrap_or("Unknown error").to_string();
                results.push(OrderResponseStatus::Error { message });
                continue;
            }

            // Unknown status format - log and skip
            tracing::warn!(status = ?status, "Unknown order status format in post response");
        }

        results
    }
}

// ============================================================================
// Order Updates (Incoming) - order state changes
// ============================================================================

/// Order update from orderUpdates subscription.
#[derive(Debug, Clone, Deserialize)]
pub struct OrderUpdatePayload {
    /// Order information.
    pub order: OrderInfo,
    /// Order status: "open", "filled", "canceled", "rejected".
    pub status: String,
    /// Timestamp of this status change.
    #[serde(rename = "statusTimestamp")]
    pub status_timestamp: u64,
}

/// Order information in orderUpdates.
#[derive(Debug, Clone, Deserialize)]
pub struct OrderInfo {
    /// Client order ID (our cloid).
    #[serde(default)]
    pub cloid: Option<String>,
    /// Exchange order ID.
    pub oid: u64,
    /// Coin symbol.
    pub coin: String,
    /// Side: "B" for buy, "A" for sell.
    pub side: String,
    /// Price.
    /// Official docs use "limitPx", but some responses may use "px".
    /// Accept both via alias.
    #[serde(alias = "limitPx")]
    pub px: String,
    /// Size.
    pub sz: String,
    /// Original size.
    #[serde(rename = "origSz")]
    pub orig_sz: String,
    /// Timestamp (optional - present in official docs but may be missing in some responses).
    #[serde(default)]
    pub timestamp: Option<u64>,
}

impl OrderUpdatePayload {
    /// Check if this is a terminal state (no more updates expected).
    ///
    /// Terminal states include:
    /// - Explicit: filled, canceled, rejected, scheduledCancel
    /// - Pattern: *Rejected (e.g., perpMarginRejected)
    /// - Pattern: *Canceled (e.g., marginCanceled)
    /// - Unknown status (fail safe - treat as terminal to prevent pending leak)
    pub fn is_terminal(&self) -> bool {
        let status = self.status.as_str();
        matches!(
            status,
            "filled" | "canceled" | "rejected" | "scheduledCancel"
        ) || status.ends_with("Rejected")
            || status.ends_with("Canceled")
            // Unknown status is also terminal (fail safe)
            || !matches!(status, "open" | "triggered")
    }
}

/// Result of parsing order updates.
#[derive(Debug, Clone, Default)]
pub struct OrderUpdatesResult {
    /// Successfully parsed order updates.
    pub updates: Vec<OrderUpdatePayload>,
    /// Number of elements that failed to parse.
    pub failed_count: usize,
}

// ============================================================================
// User Fills (Incoming) - fill notifications
// ============================================================================

/// Fill notification from userFills subscription.
/// Note: Uses deny_unknown_fields=false to allow additional fields from API.
#[derive(Debug, Clone, Deserialize)]
pub struct FillPayload {
    /// Coin symbol.
    pub coin: String,
    /// Side: "B" for buy, "A" for sell.
    pub side: String,
    /// Fill price.
    pub px: String,
    /// Fill size.
    pub sz: String,
    /// Fill timestamp (milliseconds).
    pub time: u64,
    /// Trade ID.
    #[serde(rename = "tid")]
    pub trade_id: u64,
    /// Fee charged.
    pub fee: String,
    /// Starting position size before this fill.
    #[serde(rename = "startPosition")]
    pub start_position: String,
    /// Direction indicator.
    pub dir: String,
    /// Closed PnL (optional, present in streaming updates).
    #[serde(rename = "closedPnl")]
    pub closed_pnl: Option<String>,
    /// Order ID on exchange.
    pub oid: Option<u64>,
    /// Client order ID.
    pub cloid: Option<String>,
    /// Transaction hash.
    pub hash: Option<String>,
    /// Whether order crossed the spread.
    pub crossed: Option<bool>,
    /// Fee token (e.g., "USDC").
    #[serde(rename = "feeToken")]
    pub fee_token: Option<String>,
    /// TWAP order ID if applicable.
    #[serde(rename = "twapId")]
    pub twap_id: Option<serde_json::Value>,
}

impl FillPayload {
    /// Check if this is a buy fill.
    pub fn is_buy(&self) -> bool {
        self.side == "B"
    }

    /// Check if this is a sell fill.
    pub fn is_sell(&self) -> bool {
        self.side == "A"
    }
}

/// userFills subscription response from Hyperliquid.
/// Format: { "isSnapshot"?: bool, "user": string, "fills": [FillPayload, ...] }
/// Note: isSnapshot is only present in the initial snapshot message.
#[derive(Debug, Clone, Deserialize)]
pub struct UserFillsPayload {
    /// True for initial snapshot. Missing (defaults to false) for streaming updates.
    #[serde(rename = "isSnapshot", default)]
    pub is_snapshot: bool,
    /// User address.
    pub user: String,
    /// Array of fill events.
    pub fills: Vec<FillPayload>,
}

// ============================================================================
// Subscription Response Helpers
// ============================================================================

/// Extract subscription type from subscriptionResponse data.
///
/// Handles both formats:
/// - Official: `data.subscription.type`
/// - Fallback: `data.type` (legacy compatibility)
pub fn extract_subscription_type(data: &serde_json::Value) -> Option<&str> {
    data.get("subscription")
        .and_then(|s| s.get("type"))
        .and_then(|v| v.as_str())
        .or_else(|| data.get("type").and_then(|v| v.as_str()))
}

/// Check if channel name matches orderUpdates (both formats).
///
/// Supports:
/// - `"orderUpdates"` (per official docs: channel = subscription type)
/// - `"orderUpdates:<user>"` (legacy/alternative format)
#[inline]
pub fn is_order_updates_channel(channel: &str) -> bool {
    channel == "orderUpdates" || channel.starts_with("orderUpdates:")
}

// ============================================================================
// Core WebSocket Messages
// ============================================================================

/// Incoming WebSocket message wrapper.
///
/// All messages from the Hyperliquid WebSocket use channel-based format.
/// The `channel` field determines the message type:
/// - "pong": Heartbeat response
/// - "bbo:*", "activeAssetCtx:*": Market data feeds
/// - "subscriptionResponse": Subscription confirmation
/// - "post": Post action response
/// - "orderUpdates:*": Order state updates
/// - "userFills": Fill notifications
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WsMessage {
    /// Pong response (no data field, just channel: "pong").
    Pong(PongMessage),
    /// Channel-based message (all other messages with data field).
    Channel(ChannelMessage),
}

impl WsMessage {
    /// Check if this is a pong message.
    pub fn is_pong(&self) -> bool {
        matches!(self, Self::Pong(p) if p.is_pong())
    }

    /// Check if this is a post response.
    pub fn is_post_response(&self) -> bool {
        matches!(self, Self::Channel(c) if c.channel == "post")
    }

    /// Get the channel name if this is a channel message.
    pub fn channel(&self) -> Option<&str> {
        match self {
            Self::Pong(p) => Some(&p.channel),
            Self::Channel(c) => Some(&c.channel),
        }
    }

    /// Try to parse as post response data.
    pub fn as_post_response(&self) -> Option<PostResponseData> {
        match self {
            Self::Channel(c) if c.channel == "post" => serde_json::from_value(c.data.clone()).ok(),
            _ => None,
        }
    }

    /// Check if this is an order updates message.
    pub fn is_order_updates(&self) -> bool {
        matches!(self, Self::Channel(c) if is_order_updates_channel(&c.channel))
    }

    /// Try to parse as order update payload (single object).
    ///
    /// **Deprecated**: Use `as_order_updates()` instead, which handles both
    /// array format (official) and single object (legacy).
    #[deprecated(
        since = "0.2.0",
        note = "Use as_order_updates() which handles array format"
    )]
    pub fn as_order_update(&self) -> Option<OrderUpdatePayload> {
        self.as_order_updates().updates.into_iter().next()
    }

    /// Try to parse as order update payloads.
    /// Returns OrderUpdatesResult containing parsed updates and failure count.
    ///
    /// # Official format (WsOrder[])
    /// ```json
    /// {"channel": "orderUpdates", "data": [{"order": {...}, "status": "open", ...}]}
    /// ```
    ///
    /// # Legacy format (single object)
    /// ```json
    /// {"channel": "orderUpdates", "data": {"order": {...}, "status": "open", ...}}
    /// ```
    pub fn as_order_updates(&self) -> OrderUpdatesResult {
        match self {
            Self::Channel(c) if is_order_updates_channel(&c.channel) => match &c.data {
                serde_json::Value::Array(arr) => {
                    // Official format: WsOrder[]
                    let mut updates = Vec::with_capacity(arr.len());
                    let mut failed_count = 0;

                    for v in arr {
                        match serde_json::from_value::<OrderUpdatePayload>(v.clone()) {
                            Ok(payload) => updates.push(payload),
                            Err(e) => {
                                tracing::debug!(
                                    error = %e,
                                    element = ?v,
                                    "Failed to parse orderUpdate element"
                                );
                                failed_count += 1;
                            }
                        }
                    }

                    OrderUpdatesResult {
                        updates,
                        failed_count,
                    }
                }
                serde_json::Value::Object(_) => {
                    // Legacy format: single object
                    match serde_json::from_value::<OrderUpdatePayload>(c.data.clone()) {
                        Ok(p) => OrderUpdatesResult {
                            updates: vec![p],
                            failed_count: 0,
                        },
                        Err(e) => {
                            tracing::debug!(
                                error = %e,
                                "Failed to parse orderUpdate single object"
                            );
                            OrderUpdatesResult {
                                updates: vec![],
                                failed_count: 1,
                            }
                        }
                    }
                }
                other => {
                    // Unexpected data type (not Array or Object)
                    tracing::warn!(
                        data_type = ?other,
                        "orderUpdates data is neither Array nor Object"
                    );
                    OrderUpdatesResult {
                        updates: vec![],
                        failed_count: 1,
                    }
                }
            },
            // Not an orderUpdates channel - not a failure, just not applicable
            _ => OrderUpdatesResult::default(),
        }
    }

    /// Check if this is a user fills message.
    pub fn is_user_fills(&self) -> bool {
        matches!(self, Self::Channel(c) if c.channel == "userFills")
    }

    /// Try to parse as userFills payload (array of fills).
    /// Returns the full UserFillsPayload with isSnapshot flag and all fills.
    pub fn as_user_fills(&self) -> Option<UserFillsPayload> {
        match self {
            Self::Channel(c) if c.channel == "userFills" => {
                serde_json::from_value(c.data.clone()).ok()
            }
            _ => None,
        }
    }

    /// Try to parse as fill payload (single fill - DEPRECATED).
    /// Use as_user_fills() instead for correct parsing of Hyperliquid format.
    #[deprecated(note = "Use as_user_fills() which handles the array format correctly")]
    pub fn as_fill(&self) -> Option<FillPayload> {
        // Legacy method - try to parse as single fill (will fail with new format)
        match self {
            Self::Channel(c) if c.channel == "userFills" => {
                serde_json::from_value(c.data.clone()).ok()
            }
            _ => None,
        }
    }
}

/// Channel-based message from subscription or post response.
/// Used for all messages that include a data field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    /// Channel identifier (e.g., "bbo:BTC", "post", "orderUpdates:0x...").
    pub channel: String,
    /// Message data (flexible JSON).
    pub data: serde_json::Value,
}

/// Pong response message (Hyperliquid format: {"channel": "pong"}).
/// Uses deny_unknown_fields to distinguish from ChannelMessage in untagged enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ========================================================================
    // PostRequest serialization tests
    // ========================================================================

    #[test]
    fn test_post_request_serialization() {
        let payload = PostPayload {
            action: json!({"type": "order", "orders": []}),
            nonce: 12345,
            signature: SignaturePayload {
                r: "abc123".to_string(),
                s: "def456".to_string(),
                v: 27,
            },
            vault_address: None,
        };

        let request = PostRequest::new(1, "action".to_string(), payload);
        let json = serde_json::to_value(&request).unwrap();

        assert_eq!(json["method"], "post");
        assert_eq!(json["id"], 1);
        assert_eq!(json["request"]["type"], "action");
        assert_eq!(json["request"]["payload"]["nonce"], 12345);
        assert_eq!(json["request"]["payload"]["signature"]["r"], "abc123");
        assert_eq!(json["request"]["payload"]["signature"]["s"], "def456");
        assert_eq!(json["request"]["payload"]["signature"]["v"], 27);
        // vaultAddress should be omitted when None (skip_serializing_if)
        assert!(!json["request"]["payload"]
            .as_object()
            .unwrap()
            .contains_key("vaultAddress"));
    }

    #[test]
    fn test_post_request_with_vault_address() {
        let payload = PostPayload {
            action: json!({"type": "order"}),
            nonce: 999,
            signature: SignaturePayload {
                r: "r".to_string(),
                s: "s".to_string(),
                v: 28,
            },
            vault_address: Some("0x1234".to_string()),
        };

        let request = PostRequest::new(2, "action".to_string(), payload);
        let json = serde_json::to_value(&request).unwrap();

        assert_eq!(json["request"]["payload"]["vaultAddress"], "0x1234");
    }

    #[test]
    fn test_post_payload_vault_address_omitted_when_none() {
        let payload = PostPayload {
            action: serde_json::json!({"type": "test"}),
            nonce: 12345,
            signature: SignaturePayload {
                r: "0xabc".to_string(),
                s: "0xdef".to_string(),
                v: 27,
            },
            vault_address: None,
        };

        let json = serde_json::to_value(&payload).unwrap();

        // vaultAddress should be omitted when None (skip_serializing_if)
        assert!(
            !json.as_object().unwrap().contains_key("vaultAddress"),
            "vaultAddress key should NOT be present when None"
        );
    }

    // ========================================================================
    // PostResponse parsing tests
    // ========================================================================

    #[test]
    fn test_post_response_action_success() {
        let json = json!({
            "id": 42,
            "response": {
                "type": "action",
                "payload": {
                    "status": "ok",
                    "response": {
                        "type": "order",
                        "data": {"statuses": [{"resting": {"oid": 12345}}]}
                    }
                }
            }
        });

        let response: PostResponseData = serde_json::from_value(json).unwrap();
        assert_eq!(response.id, 42);
        assert!(response.response.is_success());
        assert!(!response.response.is_error());
        assert!(response.response.error_message().is_none());
    }

    #[test]
    fn test_post_response_error() {
        let json = json!({
            "id": 99,
            "response": {
                "type": "error",
                "payload": "Order rejected: insufficient margin"
            }
        });

        let response: PostResponseData = serde_json::from_value(json).unwrap();
        assert_eq!(response.id, 99);
        assert!(!response.response.is_success());
        assert!(response.response.is_error());
        assert_eq!(
            response.response.error_message(),
            Some("Order rejected: insufficient margin")
        );
    }

    #[test]
    fn test_parse_statuses_resting() {
        let payload = ActionResponsePayload {
            status: "ok".to_string(),
            response: ActionResponseDetails {
                response_type: "order".to_string(),
                data: json!({"statuses": [{"resting": {"oid": 12345}}]}),
            },
        };

        let statuses = payload.parse_statuses();
        assert_eq!(statuses.len(), 1);
        match &statuses[0] {
            OrderResponseStatus::Resting { oid } => assert_eq!(*oid, 12345),
            _ => panic!("Expected Resting status"),
        }
    }

    #[test]
    fn test_parse_statuses_filled() {
        let payload = ActionResponsePayload {
            status: "ok".to_string(),
            response: ActionResponseDetails {
                response_type: "order".to_string(),
                data: json!({"statuses": [{"filled": {"oid": 77747314, "totalSz": "0.02", "avgPx": "1891.4"}}]}),
            },
        };

        let statuses = payload.parse_statuses();
        assert_eq!(statuses.len(), 1);
        match &statuses[0] {
            OrderResponseStatus::Filled {
                oid,
                total_sz,
                avg_px,
            } => {
                assert_eq!(*oid, 77747314);
                assert_eq!(total_sz, "0.02");
                assert_eq!(avg_px, "1891.4");
            }
            _ => panic!("Expected Filled status"),
        }
    }

    #[test]
    fn test_parse_statuses_error() {
        let payload = ActionResponsePayload {
            status: "ok".to_string(),
            response: ActionResponseDetails {
                response_type: "order".to_string(),
                data: json!({"statuses": [{"error": "Order must have minimum value of $10."}]}),
            },
        };

        let statuses = payload.parse_statuses();
        assert_eq!(statuses.len(), 1);
        match &statuses[0] {
            OrderResponseStatus::Error { message } => {
                assert_eq!(message, "Order must have minimum value of $10.")
            }
            _ => panic!("Expected Error status"),
        }
    }

    #[test]
    fn test_parse_statuses_empty() {
        let payload = ActionResponsePayload {
            status: "ok".to_string(),
            response: ActionResponseDetails {
                response_type: "order".to_string(),
                data: json!({}),
            },
        };

        let statuses = payload.parse_statuses();
        assert!(statuses.is_empty());
    }

    // ========================================================================
    // OrderUpdatePayload parsing tests
    // ========================================================================

    #[test]
    fn test_order_update_payload_parsing() {
        let json = json!({
            "order": {
                "cloid": "hip3_test_001",
                "oid": 12345,
                "coin": "BTC",
                "side": "B",
                "px": "50000.0",
                "sz": "0.1",
                "origSz": "0.1"
            },
            "status": "open",
            "statusTimestamp": 1700000000000_u64
        });

        let update: OrderUpdatePayload = serde_json::from_value(json).unwrap();
        assert_eq!(update.order.cloid, Some("hip3_test_001".to_string()));
        assert_eq!(update.order.oid, 12345);
        assert_eq!(update.order.coin, "BTC");
        assert_eq!(update.order.side, "B");
        assert_eq!(update.order.px, "50000.0");
        assert_eq!(update.order.sz, "0.1");
        assert_eq!(update.order.orig_sz, "0.1");
        assert_eq!(update.status, "open");
        assert!(!update.is_terminal());
    }

    #[test]
    fn test_order_update_terminal_states_explicit() {
        // Explicit terminal statuses
        for status in &["filled", "canceled", "rejected", "scheduledCancel"] {
            let json = json!({
                "order": {
                    "oid": 1,
                    "coin": "ETH",
                    "side": "A",
                    "px": "3000",
                    "sz": "1",
                    "origSz": "1"
                },
                "status": status,
                "statusTimestamp": 1700000000000_u64
            });

            let update: OrderUpdatePayload = serde_json::from_value(json).unwrap();
            assert!(
                update.is_terminal(),
                "status '{}' should be terminal",
                status
            );
        }
    }

    #[test]
    fn test_order_update_terminal_states_rejected_pattern() {
        // *Rejected pattern statuses
        for status in &[
            "perpMarginRejected",
            "oracleRejected",
            "tickRejected",
            "minTradeNtlRejected",
        ] {
            let json = json!({
                "order": {
                    "oid": 1,
                    "coin": "ETH",
                    "side": "A",
                    "px": "3000",
                    "sz": "1",
                    "origSz": "1"
                },
                "status": status,
                "statusTimestamp": 1700000000000_u64
            });

            let update: OrderUpdatePayload = serde_json::from_value(json).unwrap();
            assert!(
                update.is_terminal(),
                "status '{}' (*Rejected) should be terminal",
                status
            );
        }
    }

    #[test]
    fn test_order_update_terminal_states_canceled_pattern() {
        // *Canceled pattern statuses
        for status in &[
            "marginCanceled",
            "liquidatedCanceled",
            "selfTradeCanceled",
            "delistedCanceled",
        ] {
            let json = json!({
                "order": {
                    "oid": 1,
                    "coin": "ETH",
                    "side": "A",
                    "px": "3000",
                    "sz": "1",
                    "origSz": "1"
                },
                "status": status,
                "statusTimestamp": 1700000000000_u64
            });

            let update: OrderUpdatePayload = serde_json::from_value(json).unwrap();
            assert!(
                update.is_terminal(),
                "status '{}' (*Canceled) should be terminal",
                status
            );
        }
    }

    #[test]
    fn test_order_update_terminal_states_unknown() {
        // Unknown status is terminal (fail safe)
        let json = json!({
            "order": {
                "oid": 1,
                "coin": "ETH",
                "side": "A",
                "px": "3000",
                "sz": "1",
                "origSz": "1"
            },
            "status": "unknownFutureStatus",
            "statusTimestamp": 1700000000000_u64
        });

        let update: OrderUpdatePayload = serde_json::from_value(json).unwrap();
        assert!(
            update.is_terminal(),
            "Unknown status should be terminal (fail safe)"
        );
    }

    #[test]
    fn test_order_update_non_terminal_states() {
        // Non-terminal statuses: open, triggered
        for status in &["open", "triggered"] {
            let json = json!({
                "order": {
                    "oid": 1,
                    "coin": "ETH",
                    "side": "A",
                    "px": "3000",
                    "sz": "1",
                    "origSz": "1"
                },
                "status": status,
                "statusTimestamp": 1700000000000_u64
            });

            let update: OrderUpdatePayload = serde_json::from_value(json).unwrap();
            assert!(
                !update.is_terminal(),
                "status '{}' should NOT be terminal",
                status
            );
        }
    }

    #[test]
    fn test_order_update_without_cloid() {
        let json = json!({
            "order": {
                "oid": 99999,
                "coin": "SOL",
                "side": "B",
                "px": "100",
                "sz": "10",
                "origSz": "10"
            },
            "status": "open",
            "statusTimestamp": 1700000000000_u64
        });

        let update: OrderUpdatePayload = serde_json::from_value(json).unwrap();
        assert!(update.order.cloid.is_none());
    }

    // ========================================================================
    // FillPayload parsing tests
    // ========================================================================

    #[test]
    fn test_fill_payload_parsing() {
        let json = json!({
            "coin": "BTC",
            "side": "B",
            "px": "50000.5",
            "sz": "0.05",
            "time": 1700000000123_u64,
            "tid": 987654321_u64,
            "fee": "0.25",
            "startPosition": "0.1",
            "dir": "Open Long"
        });

        let fill: FillPayload = serde_json::from_value(json).unwrap();
        assert_eq!(fill.coin, "BTC");
        assert_eq!(fill.side, "B");
        assert_eq!(fill.px, "50000.5");
        assert_eq!(fill.sz, "0.05");
        assert_eq!(fill.time, 1700000000123);
        assert_eq!(fill.trade_id, 987654321);
        assert_eq!(fill.fee, "0.25");
        assert_eq!(fill.start_position, "0.1");
        assert!(fill.is_buy());
        assert!(!fill.is_sell());
    }

    #[test]
    fn test_fill_payload_sell_side() {
        let json = json!({
            "coin": "ETH",
            "side": "A",
            "px": "3000",
            "sz": "1.5",
            "time": 1700000000000_u64,
            "tid": 111222333_u64,
            "fee": "0.15",
            "startPosition": "5.0",
            "dir": "Close Long"
        });

        let fill: FillPayload = serde_json::from_value(json).unwrap();
        assert!(!fill.is_buy());
        assert!(fill.is_sell());
    }

    // ========================================================================
    // WsMessage parsing tests
    // ========================================================================

    #[test]
    fn test_ws_message_pong() {
        let json = json!({"channel": "pong"});
        let msg: WsMessage = serde_json::from_value(json).unwrap();

        assert!(msg.is_pong());
        assert_eq!(msg.channel(), Some("pong"));
    }

    #[test]
    fn test_ws_message_post_response() {
        let json = json!({
            "channel": "post",
            "data": {
                "id": 1,
                "response": {
                    "type": "action",
                    "payload": {
                        "status": "ok",
                        "response": {"type": "order", "data": {}}
                    }
                }
            }
        });

        let msg: WsMessage = serde_json::from_value(json).unwrap();
        assert!(msg.is_post_response());
        assert!(!msg.is_pong());

        let resp = msg.as_post_response().unwrap();
        assert_eq!(resp.id, 1);
        assert!(resp.response.is_success());
    }

    #[test]
    fn test_ws_message_order_updates() {
        let json = json!({
            "channel": "orderUpdates:0xabc123",
            "data": {
                "order": {
                    "cloid": "test_order",
                    "oid": 555,
                    "coin": "BTC",
                    "side": "B",
                    "px": "45000",
                    "sz": "0.01",
                    "origSz": "0.01"
                },
                "status": "filled",
                "statusTimestamp": 1700000000000_u64
            }
        });

        let msg: WsMessage = serde_json::from_value(json).unwrap();
        assert!(msg.is_order_updates());

        let result = msg.as_order_updates();
        assert!(!result.updates.is_empty());
        assert_eq!(result.failed_count, 0);
        assert_eq!(result.updates[0].order.oid, 555);
        assert!(result.updates[0].is_terminal());
    }

    #[test]
    fn test_ws_message_user_fills() {
        // Test the correct Hyperliquid format: { isSnapshot, user, fills: [] }
        let json = json!({
            "channel": "userFills",
            "data": {
                "isSnapshot": false,
                "user": "0x1234567890abcdef",
                "fills": [{
                    "coin": "SOL",
                    "side": "A",
                    "px": "150",
                    "sz": "5",
                    "time": 1700000000000_u64,
                    "tid": 123_u64,
                    "fee": "0.01",
                    "startPosition": "10",
                    "dir": "Close Long"
                }]
            }
        });

        let msg: WsMessage = serde_json::from_value(json).unwrap();
        assert!(msg.is_user_fills());

        let user_fills = msg.as_user_fills().unwrap();
        assert!(!user_fills.is_snapshot);
        assert_eq!(user_fills.user, "0x1234567890abcdef");
        assert_eq!(user_fills.fills.len(), 1);

        let fill = &user_fills.fills[0];
        assert_eq!(fill.coin, "SOL");
        assert!(fill.is_sell());
    }

    #[test]
    fn test_ws_message_user_fills_snapshot() {
        // Test snapshot with multiple fills
        let json = json!({
            "channel": "userFills",
            "data": {
                "isSnapshot": true,
                "user": "0xabcdef",
                "fills": [
                    {
                        "coin": "BTC",
                        "side": "B",
                        "px": "50000",
                        "sz": "0.1",
                        "time": 1700000000000_u64,
                        "tid": 100_u64,
                        "fee": "0.5",
                        "startPosition": "0",
                        "dir": "Open Long"
                    },
                    {
                        "coin": "ETH",
                        "side": "A",
                        "px": "3000",
                        "sz": "1.0",
                        "time": 1700000001000_u64,
                        "tid": 101_u64,
                        "fee": "0.3",
                        "startPosition": "1.0",
                        "dir": "Close Long"
                    }
                ]
            }
        });

        let msg: WsMessage = serde_json::from_value(json).unwrap();
        let user_fills = msg.as_user_fills().unwrap();

        assert!(user_fills.is_snapshot);
        assert_eq!(user_fills.fills.len(), 2);
        assert_eq!(user_fills.fills[0].coin, "BTC");
        assert!(user_fills.fills[0].is_buy());
        assert_eq!(user_fills.fills[1].coin, "ETH");
        assert!(user_fills.fills[1].is_sell());
    }

    #[test]
    fn test_ws_message_user_fills_empty() {
        // Test empty fills array (common after subscription)
        let json = json!({
            "channel": "userFills",
            "data": {
                "isSnapshot": true,
                "user": "0xabcdef",
                "fills": []
            }
        });

        let msg: WsMessage = serde_json::from_value(json).unwrap();
        let user_fills = msg.as_user_fills().unwrap();

        assert!(user_fills.is_snapshot);
        assert!(user_fills.fills.is_empty());
    }

    #[test]
    fn test_ws_message_user_fills_streaming_update() {
        // Test streaming update format (no isSnapshot field, with additional fields)
        // This is the actual format received from Hyperliquid
        let json = json!({
            "channel": "userFills",
            "data": {
                "user": "0x0116a3d95994bcc7d6a84380ed6256fbb32cd25d",
                "fills": [{
                    "coin": "xyz:SILVER",
                    "px": "112.89",
                    "sz": "2.7",
                    "side": "B",
                    "time": 1769594065851_u64,
                    "startPosition": "-6.66",
                    "dir": "Close Short",
                    "closedPnl": "-0.02673",
                    "hash": "0x403b2645eccc82d441b4043434c1a3020115002b87cfa1a6e403d198abc05cbe",
                    "oid": 304587174168_u64,
                    "crossed": true,
                    "fee": "0.023317",
                    "tid": 694586448408875_u64,
                    "cloid": "0xd2ed5e997c5a4cb9946d84c35f0b737c",
                    "feeToken": "USDC",
                    "twapId": null
                }]
            }
        });

        let msg: WsMessage = serde_json::from_value(json).unwrap();
        assert!(msg.is_user_fills());

        let user_fills = msg.as_user_fills().unwrap();
        // isSnapshot defaults to false when not present
        assert!(!user_fills.is_snapshot);
        assert_eq!(user_fills.fills.len(), 1);

        let fill = &user_fills.fills[0];
        assert_eq!(fill.coin, "xyz:SILVER");
        assert_eq!(fill.px, "112.89");
        assert_eq!(fill.sz, "2.7");
        assert!(fill.is_buy());
        assert_eq!(fill.closed_pnl, Some("-0.02673".to_string()));
        assert_eq!(fill.oid, Some(304587174168));
        assert_eq!(
            fill.cloid,
            Some("0xd2ed5e997c5a4cb9946d84c35f0b737c".to_string())
        );
        assert_eq!(fill.crossed, Some(true));
        assert_eq!(fill.fee_token, Some("USDC".to_string()));
    }

    #[test]
    fn test_ws_message_bbo_channel() {
        let json = json!({
            "channel": "bbo:BTC",
            "data": {
                "bid": {"px": "50000", "sz": "1.5"},
                "ask": {"px": "50001", "sz": "2.0"}
            }
        });

        let msg: WsMessage = serde_json::from_value(json).unwrap();
        assert!(!msg.is_pong());
        assert!(!msg.is_post_response());
        assert_eq!(msg.channel(), Some("bbo:BTC"));
    }

    // ========================================================================
    // Subscription Response Helper tests
    // ========================================================================

    #[test]
    fn test_extract_subscription_type_official_format() {
        let data = json!({
            "method": "subscribe",
            "subscription": {
                "type": "orderUpdates",
                "user": "0x1234567890abcdef"
            }
        });

        assert_eq!(extract_subscription_type(&data), Some("orderUpdates"));
    }

    #[test]
    fn test_extract_subscription_type_fallback_format() {
        let data = json!({
            "method": "subscribe",
            "type": "orderUpdates",
            "user": "0x1234567890abcdef"
        });

        assert_eq!(extract_subscription_type(&data), Some("orderUpdates"));
    }

    #[test]
    fn test_extract_subscription_type_empty() {
        let data = json!({});
        assert_eq!(extract_subscription_type(&data), None);
    }

    #[test]
    fn test_is_order_updates_channel_exact_match() {
        assert!(is_order_updates_channel("orderUpdates"));
    }

    #[test]
    fn test_is_order_updates_channel_with_user() {
        assert!(is_order_updates_channel("orderUpdates:0x1234"));
    }

    #[test]
    fn test_is_order_updates_channel_other() {
        assert!(!is_order_updates_channel("userFills"));
        assert!(!is_order_updates_channel("allMids"));
        assert!(!is_order_updates_channel("orderUpdate")); // no 's'
    }
}
