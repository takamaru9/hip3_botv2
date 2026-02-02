//! Integration tests for subscriptionResponse ACK handling and orderUpdates parsing.
//!
//! Tests that the shared helpers work correctly with real message parsing.

#[allow(deprecated)]
use hip3_ws::{extract_subscription_type, is_order_updates_channel, WsMessage};

/// Test full message flow: JSON -> WsMessage -> subscription type extraction
#[test]
fn test_subscription_response_parsing_official_format() {
    let raw = r#"{
        "channel": "subscriptionResponse",
        "data": {
            "method": "subscribe",
            "subscription": {
                "type": "orderUpdates",
                "user": "0x1234567890abcdef"
            }
        }
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    if let WsMessage::Channel(channel_msg) = msg {
        assert_eq!(channel_msg.channel, "subscriptionResponse");

        let method = channel_msg.data.get("method").and_then(|v| v.as_str());
        assert_eq!(method, Some("subscribe"));

        // Use the shared helper (same as production code)
        let sub_type = extract_subscription_type(&channel_msg.data);
        assert_eq!(sub_type, Some("orderUpdates"));
    } else {
        panic!("Expected Channel message");
    }
}

/// Test orderUpdates data message with exact channel name
#[test]
fn test_order_updates_data_message_exact_channel() {
    let raw = r#"{
        "channel": "orderUpdates",
        "data": [{"order": {"oid": 123}}]
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    // Use shared helper
    assert!(msg.is_order_updates());
}

/// Test as_order_update with exact channel match (not just starts_with)
///
/// Note: OrderUpdatePayload is a single object with:
/// - order: { cloid?, oid, coin, side, px, sz, origSz }
/// - status: String
/// - statusTimestamp: u64
#[test]
fn test_as_order_update_exact_channel_match() {
    // Exact channel name "orderUpdates" (per official docs)
    // data is a SINGLE object (not an array)
    let raw = r#"{
        "channel": "orderUpdates",
        "data": {
            "order": {
                "cloid": "hip3_test_001",
                "oid": 12345,
                "coin": "ETH",
                "side": "B",
                "px": "3000.0",
                "sz": "0.1",
                "origSz": "0.1"
            },
            "status": "open",
            "statusTimestamp": 1700000000000
        }
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    assert!(
        msg.is_order_updates(),
        "is_order_updates should match exact channel"
    );

    // as_order_updates should successfully parse the payload
    let result = msg.as_order_updates();
    assert!(
        !result.updates.is_empty(),
        "as_order_updates should parse exact channel match"
    );

    // Verify parsed fields
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.updates[0].order.coin, "ETH");
    assert_eq!(result.updates[0].order.oid, 12345);
    assert_eq!(result.updates[0].status, "open");
}

/// Test as_order_update with user suffix (legacy format)
#[test]
fn test_as_order_update_with_user_suffix() {
    // data is a SINGLE object (not an array)
    let raw = r#"{
        "channel": "orderUpdates:0x1234567890abcdef",
        "data": {
            "order": {
                "oid": 99999,
                "coin": "BTC",
                "side": "A",
                "px": "50000.0",
                "sz": "0.5",
                "origSz": "0.5"
            },
            "status": "filled",
            "statusTimestamp": 1700000000000
        }
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    assert!(msg.is_order_updates());
    let result = msg.as_order_updates();
    assert!(
        !result.updates.is_empty(),
        "as_order_updates should parse user suffix format"
    );

    // Verify parsed fields
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.updates[0].order.coin, "BTC");
    assert!(
        result.updates[0].is_terminal(),
        "filled status should be terminal"
    );
}

/// Test orderUpdates data message with user suffix
#[test]
fn test_order_updates_data_message_with_user() {
    let raw = r#"{
        "channel": "orderUpdates:0x1234567890abcdef",
        "data": [{"order": {"oid": 123}}]
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    assert!(msg.is_order_updates());
}

/// Test channel name helper directly
#[test]
fn test_channel_name_helper() {
    // Exact match (official docs format)
    assert!(is_order_updates_channel("orderUpdates"));

    // With user suffix (legacy format)
    assert!(is_order_updates_channel("orderUpdates:0xabc"));

    // Other channels
    assert!(!is_order_updates_channel("userFills"));
    assert!(!is_order_updates_channel("subscriptionResponse"));
}

// ============================================================================
// P1: Array format tests for as_order_updates()
// ============================================================================

/// Test as_order_updates with array format using official schema (limitPx, timestamp)
#[test]
fn test_as_order_updates_array_format_official_schema() {
    let raw = r#"{
        "channel": "orderUpdates",
        "data": [
            {
                "order": {
                    "cloid": "order_001",
                    "oid": 1001,
                    "coin": "ETH",
                    "side": "B",
                    "limitPx": "3000.0",
                    "sz": "0.1",
                    "origSz": "0.1",
                    "timestamp": 1700000000000
                },
                "status": "open",
                "statusTimestamp": 1700000000000
            },
            {
                "order": {
                    "cloid": "order_002",
                    "oid": 1002,
                    "coin": "BTC",
                    "side": "A",
                    "limitPx": "50000.0",
                    "sz": "0.5",
                    "origSz": "0.5",
                    "timestamp": 1700000001000
                },
                "status": "filled",
                "statusTimestamp": 1700000001000
            }
        ]
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");
    assert!(msg.is_order_updates());

    let result = msg.as_order_updates();
    assert_eq!(result.failed_count, 0, "No parse failures expected");
    assert_eq!(result.updates.len(), 2, "Should parse both orders");

    // Verify limitPx was mapped to px field
    assert_eq!(result.updates[0].order.px, "3000.0");
    assert_eq!(result.updates[0].order.coin, "ETH");
    assert_eq!(result.updates[0].order.timestamp, Some(1700000000000));

    assert_eq!(result.updates[1].order.px, "50000.0");
    assert!(result.updates[1].is_terminal());
}

/// Test as_order_updates with array format using px (backward compatibility)
#[test]
fn test_as_order_updates_array_format_px_compat() {
    let raw = r#"{
        "channel": "orderUpdates",
        "data": [
            {
                "order": {
                    "cloid": "order_001",
                    "oid": 1001,
                    "coin": "ETH",
                    "side": "B",
                    "px": "3000.0",
                    "sz": "0.1",
                    "origSz": "0.1"
                },
                "status": "open",
                "statusTimestamp": 1700000000000
            }
        ]
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    let result = msg.as_order_updates();
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.updates.len(), 1);
    assert_eq!(result.updates[0].order.px, "3000.0");
    // timestamp should be None when not provided
    assert_eq!(result.updates[0].order.timestamp, None);
}

/// Test as_order_updates with empty array (initial snapshot)
#[test]
fn test_as_order_updates_empty_array() {
    let raw = r#"{
        "channel": "orderUpdates",
        "data": []
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");
    assert!(msg.is_order_updates());

    let result = msg.as_order_updates();
    assert!(
        result.updates.is_empty(),
        "Empty array should return empty vec"
    );
    assert_eq!(result.failed_count, 0, "Empty array is not a failure");
}

/// Test as_order_updates with single object (backward compatibility)
#[test]
fn test_as_order_updates_single_object() {
    let raw = r#"{
        "channel": "orderUpdates",
        "data": {
            "order": {
                "oid": 9999,
                "coin": "SOL",
                "side": "B",
                "limitPx": "100.0",
                "sz": "1.0",
                "origSz": "1.0",
                "timestamp": 1700000000000
            },
            "status": "canceled",
            "statusTimestamp": 1700000000000
        }
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    let result = msg.as_order_updates();
    assert_eq!(
        result.updates.len(),
        1,
        "Single object should return vec of one"
    );
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.updates[0].order.coin, "SOL");
    assert_eq!(result.updates[0].order.px, "100.0"); // limitPx mapped to px
    assert!(result.updates[0].is_terminal());
}

/// Test as_order_updates with partially invalid array - verify failed_count
#[test]
fn test_as_order_updates_partial_failure() {
    let raw = r#"{
        "channel": "orderUpdates",
        "data": [
            {
                "order": {"oid": 1001, "coin": "ETH", "side": "B", "limitPx": "3000.0", "sz": "0.1", "origSz": "0.1"},
                "status": "open",
                "statusTimestamp": 1700000000000
            },
            {"invalid": "data"},
            {
                "order": {"oid": 1003, "coin": "BTC", "side": "A", "limitPx": "50000.0", "sz": "0.5", "origSz": "0.5"},
                "status": "filled",
                "statusTimestamp": 1700000002000
            }
        ]
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    let result = msg.as_order_updates();
    // Should parse 2 valid orders, skip the invalid one
    assert_eq!(
        result.updates.len(),
        2,
        "Should skip invalid element and parse valid ones"
    );
    assert_eq!(result.failed_count, 1, "Should report 1 failed element");
}

/// Test as_order_updates with invalid single object - verify failed_count
#[test]
fn test_as_order_updates_single_object_failure() {
    let raw = r#"{
        "channel": "orderUpdates",
        "data": {"invalid": "object"}
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    let result = msg.as_order_updates();
    assert!(result.updates.is_empty());
    assert_eq!(
        result.failed_count, 1,
        "Single invalid object should report 1 failure"
    );
}
