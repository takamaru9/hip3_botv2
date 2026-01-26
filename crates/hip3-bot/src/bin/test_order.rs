//! Direct WebSocket POST test for JSON format validation.
//!
//! Tests both info and action POST requests on mainnet.

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use hip3_core::order::ClientOrderId;
use hip3_executor::{
    signer::{Action, KeyManager, KeySource, OrderTypeWire, OrderWire, Signer, SigningInput},
    ws_sender::ActionSignature,
};
use hip3_ws::{PostPayload, PostRequest, SignaturePayload};
use serde_json::json;
use std::sync::Arc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const WS_URL: &str = "wss://api.hyperliquid.xyz/ws";
// xyz perp asset ID calculation:
// Formula: 100000 + perp_dex_id * 10000 + asset_index
// xyz is perpDexId 1
// Verified from API: {"type": "meta", "dex": "xyz"}
//   - xyz:SILVER = index 26 = 110026
//   - xyz:RIVN   = index 27 = 110027
//   - xyz:CL     = index 29 = 110029
const SILVER_ASSET_IDX: u32 = 110026;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    // Connect to WebSocket
    tracing::info!("Connecting to {}", WS_URL);
    let (ws_stream, _) = connect_async(WS_URL).await?;
    let (mut write, mut read) = ws_stream.split();
    tracing::info!("WebSocket connected");

    // Test 1: Info request (no signature needed)
    tracing::info!("=== Test 1: Info POST request ===");
    let info_request = json!({
        "method": "post",
        "id": 1,
        "request": {
            "type": "info",
            "payload": {
                "type": "l2Book",
                "coin": "BTC"
            }
        }
    });

    tracing::info!("Sending: {}", info_request);
    write.send(Message::Text(info_request.to_string())).await?;

    // Wait for response
    let timeout = tokio::time::Duration::from_secs(5);
    let info_result = tokio::time::timeout(timeout, async {
        while let Some(msg) = read.next().await {
            if let Ok(Message::Text(text)) = msg {
                tracing::info!("Received: {}", &text[..text.len().min(200)]);
                if text.contains("\"id\":1") || text.contains("error") {
                    return Some(text);
                }
            }
        }
        None
    })
    .await;

    match info_result {
        Ok(Some(resp)) if resp.contains("Error parsing") => {
            tracing::error!("❌ Info POST failed - basic format issue");
            return Ok(());
        }
        Ok(Some(_)) => tracing::info!("✅ Info POST worked"),
        _ => tracing::warn!("No response for info POST"),
    }

    // Test 2: Action request with signature
    tracing::info!("\n=== Test 2: Action POST request (order) ===");

    // Create KeyManager from env var (set HIP3_TRADING_KEY before running)
    let key_source = KeySource::EnvVar {
        var_name: "HIP3_TRADING_KEY".to_string(),
    };
    let key_manager = Arc::new(KeyManager::load(Some(key_source), None)?);
    let trading_address = key_manager
        .trading_address()
        .expect("Trading address required");
    tracing::info!("Trading address: {}", trading_address);

    // MAINNET requires source="a" (is_mainnet=true)
    // Using source="b" (testnet) on mainnet API causes signature verification failure
    let signer = Signer::new(key_manager, true)?;
    tracing::info!("Using mainnet source (a) for signature");

    // Build order - use current ask price for buy IOC
    // Testing with 0.2 to confirm min order issue
    // Use fixed cloid for debugging/comparison with Python SDK
    let order = OrderWire {
        asset: SILVER_ASSET_IDX,
        is_buy: true,
        limit_px: "105".to_string(), // Above ask for IOC fill (no trailing zeros per SDK)
        sz: "0.2".to_string(),       // ~$21 to safely meet any minimum
        reduce_only: false,
        order_type: OrderTypeWire::ioc(),
        cloid: Some(ClientOrderId::new().to_string()),
    };

    let action = Action {
        action_type: "order".to_string(),
        orders: Some(vec![order]),
        cancels: None,
        grouping: Some("na".to_string()),
        builder: None,
    };

    // Use current timestamp as nonce (standard SDK behavior)
    let nonce: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    tracing::info!("Using nonce: {}", nonce);

    // API wallet trading:
    // - API wallet is registered/approved to trade for master account
    // - vaultAddress is only needed for SUBACCOUNT/VAULT trading
    // - For direct trading on master account, vaultAddress should be None
    tracing::info!("API wallet trading (vaultAddress=None for master account)");

    let input = SigningInput {
        action: action.clone(),
        nonce,
        vault_address: None, // None for API wallet trading on master account
        expires_after: None,
    };

    // Debug: print action hash for verification
    let action_hash = input.action_hash()?;
    tracing::info!("Action hash: {:?}", action_hash);

    let sig = signer.sign_action(input).await?;
    let sig_bytes = sig.as_bytes();
    let signature = ActionSignature::from_bytes(&sig_bytes);

    // Build POST payload
    // vaultAddress must match what was used for signing (None for API wallet)
    let action_value = serde_json::to_value(&action)?;
    let payload = PostPayload {
        action: action_value,
        nonce,
        signature: SignaturePayload {
            r: signature.r.clone(),
            s: signature.s.clone(),
            v: signature.v,
        },
        vault_address: None, // None for API wallet trading
    };

    let request = PostRequest::new(2, "action".to_string(), payload);
    let json = serde_json::to_string(&request)?;

    tracing::info!("Sending: {}", json);
    write.send(Message::Text(json)).await?;

    let action_result = tokio::time::timeout(timeout, async {
        while let Some(msg) = read.next().await {
            if let Ok(Message::Text(text)) = msg {
                tracing::info!("Received: {}", text);
                if text.contains("\"id\":2") || text.contains("error") || text.contains("Error") {
                    return Some(text);
                }
            }
        }
        None
    })
    .await;

    match action_result {
        Ok(Some(response)) => {
            if response.contains("Error parsing JSON") {
                tracing::error!("❌ Action POST JSON PARSE ERROR");
            } else if response.contains("error") {
                tracing::warn!("Order rejected (but JSON parsed OK): {}", response);
                tracing::info!("✅ JSON format is correct!");
            } else {
                tracing::info!("✅ Action POST succeeded!");
            }
        }
        Ok(None) => tracing::warn!("No response for action POST"),
        Err(_) => tracing::warn!("Timeout waiting for action POST response"),
    }

    Ok(())
}
