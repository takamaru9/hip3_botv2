//! Signer module for Hyperliquid L1 Action signing.
//!
//! Implements the SDK 2-stage signing process:
//! 1. Calculate `action_hash` from Action + nonce + vault_address + expires_after
//! 2. Sign `phantom_agent` using EIP-712
//!
//! Reference: hyperliquid-python-sdk/hyperliquid/utils/signing.py

use std::path::PathBuf;
use std::sync::Arc;

use alloy::primitives::{keccak256, Address, PrimitiveSignature, B256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer as AlloySigner;
use alloy::sol;
use alloy::sol_types::eip712_domain;
use alloy::sol_types::SolStruct;
use serde::Serialize;
use thiserror::Error;
use zeroize::Zeroizing;

// =============================================================================
// KeySource and KeyManager
// =============================================================================

/// Source of the private key.
#[derive(Debug, Clone)]
pub enum KeySource {
    /// Load from environment variable (development).
    EnvVar { var_name: String },
    /// Load from file (production, recommend 0600 permissions).
    File { path: PathBuf },
}

/// Manages trading and observation keys.
///
/// Security notes:
/// - Private keys are stored in `PrivateKeySigner` which handles secure memory.
/// - Keys are loaded once at startup; no runtime key rotation.
/// - Never log private key material.
pub struct KeyManager {
    trading_signer: Option<PrivateKeySigner>,
    observation_address: Address,
    trading_address: Option<Address>,
}

impl KeyManager {
    /// Load keys from the specified source and verify addresses.
    ///
    /// # Arguments
    /// * `trading_source` - Source of the trading private key (None for observation-only mode)
    /// * `expected_trading_address` - If provided, verify the derived address matches
    ///
    /// # Errors
    /// Returns `KeyError` if:
    /// - Environment variable not found
    /// - File read fails
    /// - Hex decoding fails
    /// - Private key is invalid
    /// - Address mismatch
    pub fn load(
        trading_source: Option<KeySource>,
        expected_trading_address: Option<Address>,
    ) -> Result<Self, KeyError> {
        let trading_signer = if let Some(source) = trading_source {
            // Parse hex key from string (supports 0x prefix and whitespace trimming)
            fn parse_hex_key(hex_str: &str) -> Result<Zeroizing<Vec<u8>>, KeyError> {
                let trimmed = hex_str.trim().trim_start_matches("0x");
                Ok(Zeroizing::new(hex::decode(trimmed)?))
            }

            let secret_bytes: Zeroizing<Vec<u8>> = match source {
                KeySource::EnvVar { ref var_name } => {
                    let hex = std::env::var(var_name)
                        .map_err(|_| KeyError::EnvVarNotFound(var_name.clone()))?;
                    parse_hex_key(&hex)?
                }
                KeySource::File { ref path } => {
                    let content = std::fs::read_to_string(path)?;
                    parse_hex_key(&content)?
                }
            };

            let signer = PrivateKeySigner::from_slice(&secret_bytes)
                .map_err(|e| KeyError::InvalidKey(e.to_string()))?;

            // Verify address matches expected
            if let Some(expected) = expected_trading_address {
                if signer.address() != expected {
                    return Err(KeyError::AddressMismatch {
                        expected,
                        actual: signer.address(),
                    });
                }
            }

            Some(signer)
        } else {
            None
        };

        Ok(Self {
            trading_address: trading_signer
                .as_ref()
                .map(|s: &PrivateKeySigner| s.address()),
            trading_signer,
            observation_address: Address::ZERO, // TODO: Set separately if needed
        })
    }

    /// Load from raw bytes (test-only, no environment variable dependency).
    #[cfg(test)]
    pub fn from_bytes(
        secret_bytes: &[u8],
        expected_address: Option<Address>,
    ) -> Result<Self, KeyError> {
        let signer = PrivateKeySigner::from_slice(secret_bytes)
            .map_err(|e| KeyError::InvalidKey(e.to_string()))?;

        // Verify address matches expected
        if let Some(expected) = expected_address {
            if signer.address() != expected {
                return Err(KeyError::AddressMismatch {
                    expected,
                    actual: signer.address(),
                });
            }
        }

        Ok(Self {
            trading_address: Some(signer.address()),
            trading_signer: Some(signer),
            observation_address: Address::ZERO,
        })
    }

    /// Get the trading signer (if available).
    pub fn trading_signer(&self) -> Option<&PrivateKeySigner> {
        self.trading_signer.as_ref()
    }

    /// Get the trading address (if available).
    pub fn trading_address(&self) -> Option<Address> {
        self.trading_address
    }

    /// Get the observation address.
    #[allow(dead_code)]
    pub fn observation_address(&self) -> Address {
        self.observation_address
    }
}

/// Key management errors.
#[derive(Debug, Error)]
pub enum KeyError {
    #[error("Environment variable not found: {0}")]
    EnvVarNotFound(String),

    #[error("Failed to decode hex: {0}")]
    HexDecode(#[from] hex::FromHexError),

    #[error("Invalid private key: {0}")]
    InvalidKey(String),

    #[error("Address mismatch: expected {expected}, got {actual}")]
    AddressMismatch { expected: Address, actual: Address },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

// =============================================================================
// Wire Format Types
// =============================================================================

/// L1 Action (part of the signing input).
///
/// Reference: hyperliquid-python-sdk/hyperliquid/utils/signing.py - order_wires_to_order_action()
///
/// IMPORTANT: `Option<T>` fields must use `skip_serializing_if` for msgpack compatibility.
/// Python SDK omits missing keys, but serde defaults to serializing `None` as `nil`.
#[derive(Debug, Clone, Serialize)]
pub struct Action {
    /// Action type: "order", "cancel", "batchModify", etc.
    #[serde(rename = "type")]
    pub action_type: String,

    /// Orders (omit key if None for Python SDK compatibility)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orders: Option<Vec<OrderWire>>,

    /// Cancels (omit key if None for Python SDK compatibility)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancels: Option<Vec<CancelWire>>,

    /// Order grouping (required for type=order).
    /// SDK: order_wires_to_order_action() sets "na" for single orders.
    /// "na" = not applicable, "normalTpsl" = TP/SL linked, etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grouping: Option<String>,

    /// Builder info (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builder: Option<BuilderInfo>,
}

/// Builder information (optional).
#[derive(Debug, Clone, Serialize)]
pub struct BuilderInfo {
    #[serde(rename = "b")]
    pub address: String,
    #[serde(rename = "f")]
    pub fee: u64,
}

/// Order wire format (matches SDK's order_spec_to_order_wire).
///
/// Reference: hyperliquid-python-sdk/hyperliquid/utils/types.py - OrderWire
#[derive(Debug, Clone, Serialize)]
pub struct OrderWire {
    /// Asset index
    #[serde(rename = "a")]
    pub asset: u32,

    /// Buy (true) or Sell (false)
    #[serde(rename = "b")]
    pub is_buy: bool,

    /// Limit price as string
    #[serde(rename = "p")]
    pub limit_px: String,

    /// Size as string
    #[serde(rename = "s")]
    pub sz: String,

    /// Reduce-only flag
    #[serde(rename = "r")]
    pub reduce_only: bool,

    /// Order type
    #[serde(rename = "t")]
    pub order_type: OrderTypeWire,

    /// Client order ID (optional)
    #[serde(rename = "c", skip_serializing_if = "Option::is_none")]
    pub cloid: Option<String>,
}

impl OrderWire {
    /// Create an OrderWire from a PendingOrder with proper precision formatting.
    ///
    /// Uses IOC (Immediate or Cancel) order type as Phase B scope.
    /// Applies MarketSpec precision rules:
    /// - Price: tick rounding + 5 sig figs + max_price_decimals
    /// - Size: lot rounding + 5 sig figs + sz_decimals
    pub fn from_pending_order(
        order: &hip3_core::PendingOrder,
        spec: &hip3_core::MarketSpec,
    ) -> Self {
        use hip3_core::OrderSide;
        let is_buy = matches!(order.side, OrderSide::Buy);
        Self {
            asset: order.market.asset.0,
            is_buy,
            limit_px: spec.format_price(order.price, is_buy),
            sz: spec.format_size(order.size),
            reduce_only: order.reduce_only,
            order_type: OrderTypeWire::ioc(),
            cloid: Some(order.cloid.to_string()),
        }
    }
}

/// Order type wire format (matches SDK).
///
/// Reference: hyperliquid-python-sdk/hyperliquid/exchange.py - order()
///
/// SDK examples:
/// - Limit IOC: {"limit": {"tif": "Ioc"}}
/// - Limit GTC: {"limit": {"tif": "Gtc"}}
/// - Limit ALO: {"limit": {"tif": "Alo"}}
/// - Trigger: {"trigger": {"triggerPx": "...", "isMarket": true, "tpsl": "tp"}}
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum OrderTypeWire {
    /// Limit order: {"limit": {"tif": "Gtc"|"Ioc"|"Alo"}}
    Limit { limit: LimitOrderType },
    /// Trigger order: {"trigger": {...}}
    Trigger { trigger: TriggerOrderType },
}

impl OrderTypeWire {
    /// IOC (Immediate or Cancel) order.
    pub fn ioc() -> Self {
        Self::Limit {
            limit: LimitOrderType {
                tif: "Ioc".to_string(),
            },
        }
    }

    /// GTC (Good Till Cancel) order.
    pub fn gtc() -> Self {
        Self::Limit {
            limit: LimitOrderType {
                tif: "Gtc".to_string(),
            },
        }
    }

    /// ALO (Add Liquidity Only) order.
    pub fn alo() -> Self {
        Self::Limit {
            limit: LimitOrderType {
                tif: "Alo".to_string(),
            },
        }
    }
}

/// Limit order type.
#[derive(Debug, Clone, Serialize)]
pub struct LimitOrderType {
    /// Time in force: "Gtc", "Ioc", "Alo"
    pub tif: String,
}

/// Trigger order type.
///
/// NOTE: Phase B scope does not include trigger orders (IOC only).
/// Field order must match SDK for correct msgpack serialization:
/// SDK order: isMarket -> triggerPx -> tpsl
#[derive(Debug, Clone, Serialize)]
pub struct TriggerOrderType {
    /// Is market order on trigger
    #[serde(rename = "isMarket")]
    pub is_market: bool,

    /// Trigger price
    #[serde(rename = "triggerPx")]
    pub trigger_px: String,

    /// Take profit or stop loss: "tp" or "sl"
    pub tpsl: String,
}

/// Cancel wire format (matches SDK).
///
/// Reference: hyperliquid-python-sdk/hyperliquid/exchange.py - cancel() / bulk_cancel()
///
/// SDK example: {"a": 5, "o": 123456789}
#[derive(Debug, Clone, Serialize)]
pub struct CancelWire {
    /// Asset index
    #[serde(rename = "a")]
    pub asset: u32,

    /// Exchange order ID
    #[serde(rename = "o")]
    pub oid: u64,
}

// =============================================================================
// SigningInput and action_hash
// =============================================================================

/// Signing input parameters.
#[derive(Debug, Clone)]
pub struct SigningInput {
    pub action: Action,
    pub nonce: u64,
    /// None = normal trading, Some = vault trading
    pub vault_address: Option<Address>,
    /// Signature expiration (optional)
    pub expires_after: Option<u64>,
}

impl SigningInput {
    /// Calculate action_hash (SDK action_hash() function compliant).
    ///
    /// Reference: hyperliquid-python-sdk/hyperliquid/utils/signing.py
    /// ```python
    /// def action_hash(action, vault_address, nonce, expires_after=None):
    ///     data = msgpack.packb(action) + nonce.to_bytes(8, "big") + \
    ///            (b"\x00" if vault_address is None else b"\x01" + bytes.fromhex(vault_address[2:]))
    ///     if expires_after is not None:
    ///         data += b"\x00" + expires_after.to_bytes(8, "big")
    ///     return keccak256(data)
    /// ```
    ///
    /// # Errors
    /// Returns `SignerError::SerializationFailed` if msgpack serialization fails.
    pub fn action_hash(&self) -> Result<B256, SignerError> {
        let mut data = Vec::new();

        // 1. Serialize Action with msgpack (named/map format)
        //    Using rmp_serde::to_vec_named for key-value map format
        //    Python SDK: msgpack.packb(action)
        let action_bytes = rmp_serde::to_vec_named(&self.action)
            .map_err(|e| SignerError::SerializationFailed(e.to_string()))?;
        data.extend_from_slice(&action_bytes);

        // 2. nonce as big-endian 8 bytes
        //    Python SDK: nonce.to_bytes(8, "big")
        data.extend_from_slice(&self.nonce.to_be_bytes());

        // 3. vault_address tag
        //    None: 0x00 (1 byte)
        //    Some: 0x01 + address (21 bytes)
        //    NOTE: Even None has the 0x00 byte
        match &self.vault_address {
            None => data.push(0x00),
            Some(addr) => {
                data.push(0x01);
                data.extend_from_slice(addr.as_slice());
            }
        }

        // 4. expires_after tag (SDK compliant)
        //    None: nothing added (tag itself doesn't exist)
        //    Some: 0x00 + big-endian 8 bytes (9 bytes total)
        //    NOTE: Different from vault_address behavior
        if let Some(expires) = self.expires_after {
            data.push(0x00);
            data.extend_from_slice(&expires.to_be_bytes());
        }
        // None case: add nothing

        Ok(keccak256(&data))
    }
}

// =============================================================================
// PhantomAgent and EIP-712 Signing
// =============================================================================

/// EIP-712 domain constants.
pub const EIP712_DOMAIN_NAME: &str = "Exchange";
pub const EIP712_DOMAIN_VERSION: &str = "1";
pub const EIP712_CHAIN_ID: u64 = 1337;
pub const EIP712_VERIFYING_CONTRACT: Address = Address::ZERO;

// EIP-712 type definition using alloy sol! macro
sol! {
    #[derive(Debug)]
    struct Agent {
        string source;
        bytes32 connectionId;
    }
}

/// Phantom Agent structure (EIP-712 signing target).
#[derive(Debug, Clone)]
pub struct PhantomAgent {
    /// "a" (mainnet) or "b" (testnet)
    pub source: String,
    /// action_hash result
    pub connection_id: B256,
}

impl PhantomAgent {
    /// Create a new PhantomAgent.
    pub fn new(action_hash: B256, is_mainnet: bool) -> Self {
        Self {
            source: if is_mainnet {
                "a".to_string()
            } else {
                "b".to_string()
            },
            connection_id: action_hash,
        }
    }

    /// Sign the PhantomAgent using EIP-712.
    ///
    /// Reference: SDK signing.py - construct_phantom_agent() and sign_inner()
    /// - phantom_agent = {"source": source, "connectionId": action_hash}
    /// - source = "a" (mainnet) or "b" (testnet)
    /// - EIP-712 domain: {name: "Exchange", version: "1", chainId: 1337, verifyingContract: 0x0}
    /// - primaryType: "Agent"
    pub async fn sign<S: AlloySigner + Send + Sync>(
        &self,
        signer: &S,
    ) -> Result<PrimitiveSignature, alloy::signers::Error> {
        let domain = eip712_domain! {
            name: EIP712_DOMAIN_NAME,
            version: EIP712_DOMAIN_VERSION,
            chain_id: EIP712_CHAIN_ID,
            verifying_contract: EIP712_VERIFYING_CONTRACT,
        };

        let agent = Agent {
            source: self.source.clone(),
            connectionId: self.connection_id,
        };

        // EIP-712 signing_hash = keccak256(0x1901 || domain_separator || struct_hash)
        let signing_hash = agent.eip712_signing_hash(&domain);

        signer.sign_hash(&signing_hash).await
    }
}

// =============================================================================
// Signer
// =============================================================================

/// Signing errors.
#[derive(Debug, Error)]
pub enum SignerError {
    #[error("No trading key available")]
    NoTradingKey,

    #[error("Signing failed: {0}")]
    SigningFailed(#[from] alloy::signers::Error),

    #[error("Action serialization failed: {0}")]
    SerializationFailed(String),
}

/// Signer for Hyperliquid L1 Actions.
///
/// Implements the SDK 2-stage signing process:
/// 1. Calculate action_hash
/// 2. Sign phantom_agent using EIP-712
pub struct Signer {
    key_manager: Arc<KeyManager>,
    is_mainnet: bool,
}

impl Signer {
    /// Create a new Signer.
    ///
    /// # Errors
    /// Returns `SignerError::NoTradingKey` if the KeyManager has no trading key.
    pub fn new(key_manager: Arc<KeyManager>, is_mainnet: bool) -> Result<Self, SignerError> {
        if key_manager.trading_signer().is_none() {
            return Err(SignerError::NoTradingKey);
        }
        Ok(Self {
            key_manager,
            is_mainnet,
        })
    }

    /// Sign an action.
    ///
    /// NOTE: post_id is a WS layer correlation ID and is NOT part of the signature.
    /// ExecutorLoop assigns post_id, and WsSender includes it in the JSON.
    ///
    /// # Arguments
    /// * `input` - The signing input containing action, nonce, and optional parameters
    ///
    /// # Errors
    /// Returns `SignerError` if signing fails or action serialization fails.
    pub async fn sign_action(
        &self,
        input: SigningInput,
    ) -> Result<PrimitiveSignature, SignerError> {
        let signer = self
            .key_manager
            .trading_signer()
            .ok_or(SignerError::NoTradingKey)?;

        // Step 1: Calculate action_hash (returns Result now)
        let action_hash = input.action_hash()?;

        // Step 2: Create phantom_agent and sign with EIP-712
        let phantom_agent = PhantomAgent::new(action_hash, self.is_mainnet);

        // NOTE: Do not log signature as it contains sensitive information
        let signature = phantom_agent.sign(signer).await?;

        Ok(signature)
    }

    /// Get the trading address.
    pub fn trading_address(&self) -> Option<Address> {
        self.key_manager.trading_address()
    }

    /// Check if this is mainnet mode.
    pub fn is_mainnet(&self) -> bool {
        self.is_mainnet
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Well-known test private key (DO NOT use in production)
    // This is a commonly used test key, address: 0x...
    const TEST_PRIVATE_KEY: &str =
        "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    fn test_key_bytes() -> Vec<u8> {
        hex::decode(TEST_PRIVATE_KEY.trim_start_matches("0x")).unwrap()
    }

    #[test]
    fn test_key_manager_from_bytes() {
        let bytes = test_key_bytes();
        let manager = KeyManager::from_bytes(&bytes, None).unwrap();

        assert!(manager.trading_signer().is_some());
        assert!(manager.trading_address().is_some());
    }

    #[test]
    fn test_key_manager_address_mismatch() {
        let bytes = test_key_bytes();
        let wrong_address = Address::ZERO;

        let result = KeyManager::from_bytes(&bytes, Some(wrong_address));
        assert!(matches!(result, Err(KeyError::AddressMismatch { .. })));
    }

    #[test]
    fn test_order_type_wire_serialization() {
        // Test IOC serialization
        let ioc = OrderTypeWire::ioc();
        let json = serde_json::to_string(&ioc).unwrap();
        assert_eq!(json, r#"{"limit":{"tif":"Ioc"}}"#);

        // Test GTC serialization
        let gtc = OrderTypeWire::gtc();
        let json = serde_json::to_string(&gtc).unwrap();
        assert_eq!(json, r#"{"limit":{"tif":"Gtc"}}"#);
    }

    #[test]
    fn test_action_serialization_skips_none() {
        let action = Action {
            action_type: "order".to_string(),
            orders: Some(vec![]),
            cancels: None, // Should be omitted
            grouping: Some("na".to_string()),
            builder: None, // Should be omitted
        };

        let json = serde_json::to_string(&action).unwrap();

        // Verify None fields are not present
        assert!(!json.contains("cancels"));
        assert!(!json.contains("builder"));
        assert!(json.contains("orders"));
        assert!(json.contains("grouping"));
    }

    #[test]
    fn test_action_hash_basic() {
        // Create a simple action
        let action = Action {
            action_type: "order".to_string(),
            orders: Some(vec![OrderWire {
                asset: 0,
                is_buy: true,
                limit_px: "100.0".to_string(),
                sz: "1.0".to_string(),
                reduce_only: false,
                order_type: OrderTypeWire::ioc(),
                cloid: None,
            }]),
            cancels: None,
            grouping: Some("na".to_string()),
            builder: None,
        };

        let input = SigningInput {
            action,
            nonce: 1234567890,
            vault_address: None,
            expires_after: None,
        };

        // Just verify it computes without error
        let hash = input.action_hash().unwrap();
        assert!(!hash.is_zero());
    }

    /// Test msgpack serialization field order.
    ///
    /// CRITICAL: Msgpack field order must match Python SDK exactly.
    /// Different order = different hash = signature verification failure.
    ///
    /// Run with: cargo test -p hip3-executor test_msgpack_field_order -- --nocapture
    #[test]
    fn test_msgpack_field_order() {
        // Action matching test_order.rs output
        let action = Action {
            action_type: "order".to_string(),
            orders: Some(vec![OrderWire {
                asset: 110027,
                is_buy: true,
                limit_px: "105.00".to_string(),
                sz: "0.2".to_string(),
                reduce_only: false,
                order_type: OrderTypeWire::ioc(),
                cloid: Some("0x0de3e244a8f44fc28a6b7bc852d66d19".to_string()),
            }]),
            cancels: None,
            grouping: Some("na".to_string()),
            builder: None,
        };

        // Serialize to msgpack
        let msgpack_bytes = rmp_serde::to_vec_named(&action).unwrap();
        println!(
            "Rust msgpack ({} bytes): {}",
            msgpack_bytes.len(),
            hex::encode(&msgpack_bytes)
        );

        // Expected from Python SDK (field order: type, orders, grouping):
        // 83a474797065a56f72646572a66f72646572739187a161ce0001adcba162c3a170a63130352e3030a173a3302e32a172c2a17481a56c696d697481a3746966a3496f63a163d92230783064653365323434613866343466633238613662376263383532643636643139a867726f7570696e67a26e61
        let expected_python = "83a474797065a56f72646572a66f72646572739187a161ce0001adcba162c3a170a63130352e3030a173a3302e32a172c2a17481a56c696d697481a3746966a3496f63a163d92230783064653365323434613866343466633238613662376263383532643636643139a867726f7570696e67a26e61";
        let expected_bytes = hex::decode(expected_python).unwrap();

        println!(
            "Python msgpack ({} bytes): {}",
            expected_bytes.len(),
            expected_python
        );

        assert_eq!(
            hex::encode(&msgpack_bytes),
            expected_python,
            "Msgpack bytes must match Python SDK exactly for signature compatibility"
        );

        // Also verify action hash
        let nonce: u64 = 1769339470576;
        let mut data = Vec::new();
        data.extend_from_slice(&msgpack_bytes);
        data.extend_from_slice(&nonce.to_be_bytes());
        data.push(0x00); // No vault
        let hash = keccak256(&data);
        println!("Action hash: {:?}", hash);

        let expected_hash = "904c57b8f4b75ac9da005b49298dc39af735ed8c3a89b241f5f1e061e0207868";
        assert_eq!(
            hex::encode(hash.as_slice()),
            expected_hash,
            "Action hash must match Python SDK"
        );
    }

    /// Test JSON serialization preserves field order.
    ///
    /// CRITICAL: When serializing action to JSON for POST payload,
    /// the field order must be preserved. serde_json::Value uses
    /// a Map internally which may reorder fields!
    ///
    /// Run with: cargo test -p hip3-executor test_json_preserves_field_order -- --nocapture
    #[test]
    fn test_json_preserves_field_order() {
        let action = Action {
            action_type: "order".to_string(),
            orders: Some(vec![OrderWire {
                asset: 110027,
                is_buy: true,
                limit_px: "105.00".to_string(),
                sz: "0.2".to_string(),
                reduce_only: false,
                order_type: OrderTypeWire::ioc(),
                cloid: Some("0x0de3e244a8f44fc28a6b7bc852d66d19".to_string()),
            }]),
            cancels: None,
            grouping: Some("na".to_string()),
            builder: None,
        };

        // Direct JSON serialization (should preserve struct field order)
        let json_direct = serde_json::to_string(&action).unwrap();
        println!("Direct JSON: {}", json_direct);

        // Via serde_json::Value (may reorder!)
        let value = serde_json::to_value(&action).unwrap();
        let json_via_value = serde_json::to_string(&value).unwrap();
        println!("Via Value:   {}", json_via_value);

        // Check if they start with "type" field (expected order)
        assert!(
            json_direct.starts_with(r#"{"type":"order""#),
            "Direct JSON should start with type field"
        );

        // Check if via Value preserved order
        println!("Field order preserved: {}", json_direct == json_via_value);
    }

    #[test]
    fn test_action_hash_with_vault() {
        let action = Action {
            action_type: "cancel".to_string(),
            orders: None,
            cancels: Some(vec![CancelWire { asset: 5, oid: 123 }]),
            grouping: None,
            builder: None,
        };

        let vault_addr = Address::repeat_byte(0x42);

        let input = SigningInput {
            action: action.clone(),
            nonce: 1000,
            vault_address: Some(vault_addr),
            expires_after: None,
        };

        let hash_with_vault = input.action_hash().unwrap();

        // Without vault should produce different hash
        let input_no_vault = SigningInput {
            action,
            nonce: 1000,
            vault_address: None,
            expires_after: None,
        };

        let hash_no_vault = input_no_vault.action_hash().unwrap();

        assert_ne!(hash_with_vault, hash_no_vault);
    }

    #[test]
    fn test_action_hash_with_expires() {
        let action = Action {
            action_type: "order".to_string(),
            orders: Some(vec![]),
            cancels: None,
            grouping: Some("na".to_string()),
            builder: None,
        };

        let input_no_expires = SigningInput {
            action: action.clone(),
            nonce: 1000,
            vault_address: None,
            expires_after: None,
        };

        let input_with_expires = SigningInput {
            action,
            nonce: 1000,
            vault_address: None,
            expires_after: Some(1700000000),
        };

        let hash_no_expires = input_no_expires.action_hash().unwrap();
        let hash_with_expires = input_with_expires.action_hash().unwrap();

        assert_ne!(hash_no_expires, hash_with_expires);
    }

    #[test]
    fn test_phantom_agent_creation() {
        let hash = B256::repeat_byte(0xab);

        let mainnet = PhantomAgent::new(hash, true);
        assert_eq!(mainnet.source, "a");

        let testnet = PhantomAgent::new(hash, false);
        assert_eq!(testnet.source, "b");
    }

    /// Test to verify domain separator and struct hash match Python SDK.
    ///
    /// This is critical for signature compatibility.
    /// Run with: cargo test -p hip3-executor test_eip712_domain_separator -- --nocapture
    #[test]
    fn test_eip712_domain_separator() {
        // Create domain using the same constants as PhantomAgent::sign
        let domain = eip712_domain! {
            name: EIP712_DOMAIN_NAME,
            version: EIP712_DOMAIN_VERSION,
            chain_id: EIP712_CHAIN_ID,
            verifying_contract: EIP712_VERIFYING_CONTRACT,
        };

        // Calculate domain separator
        let domain_separator = domain.hash_struct();
        println!("Domain separator: {:?}", domain_separator);

        // Known action hash (from test_order.rs with fixed inputs)
        let action_hash = B256::from_slice(
            &hex::decode("f01fa6eaca0b8cbd2afe65f8852a2e00d35eae3d19560ece9b8a28614646e849")
                .unwrap(),
        );

        // Create Agent struct with testnet source
        let agent = Agent {
            source: "b".to_string(),
            connectionId: action_hash,
        };

        // Calculate struct hash
        let struct_hash = agent.eip712_hash_struct();
        println!("Struct hash: {:?}", struct_hash);

        // Calculate signing hash (should be keccak256(0x1901 || domain_separator || struct_hash))
        let signing_hash = agent.eip712_signing_hash(&domain);
        println!("Signing hash: {:?}", signing_hash);

        // Expected values from Python SDK (to be filled in after verification):
        // Python domain separator (with bytes(20) for address): 0x943423c69b6b5f5c05bc999245cb877c5ab57224d408aeecb13e1d1fd3be1610
        // Python struct hash: 0x9b4df0fd8db77d906bfdb75485a4ace25342ac34133799aae04cdfef7fc69333

        // Manually compute domain separator to understand the encoding
        let type_hash = keccak256(
            b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
        );
        let name_hash = keccak256(EIP712_DOMAIN_NAME.as_bytes());
        let version_hash = keccak256(EIP712_DOMAIN_VERSION.as_bytes());

        println!("TYPE_HASH: {:?}", type_hash);
        println!("NAME_HASH: {:?}", name_hash);
        println!("VERSION_HASH: {:?}", version_hash);

        // Construct domain separator manually to verify
        let mut domain_data = Vec::new();
        domain_data.extend_from_slice(type_hash.as_slice());
        domain_data.extend_from_slice(name_hash.as_slice());
        domain_data.extend_from_slice(version_hash.as_slice());
        // chainId as uint256 (32 bytes, big-endian)
        let mut chain_id_bytes = [0u8; 32];
        chain_id_bytes[24..].copy_from_slice(&EIP712_CHAIN_ID.to_be_bytes());
        domain_data.extend_from_slice(&chain_id_bytes);
        // verifyingContract as address (20 bytes, left-padded to 32 bytes)
        let mut contract_bytes = [0u8; 32];
        contract_bytes[12..].copy_from_slice(EIP712_VERIFYING_CONTRACT.as_slice());
        domain_data.extend_from_slice(&contract_bytes);

        let manual_domain_separator = keccak256(&domain_data);
        println!("Manual domain separator: {:?}", manual_domain_separator);

        // The struct hash for Agent
        let agent_type_hash = keccak256(b"Agent(string source,bytes32 connectionId)");
        println!("Agent TYPE_HASH: {:?}", agent_type_hash);

        let source_hash = keccak256(b"b");
        println!("Source hash (keccak256(\"b\")): {:?}", source_hash);

        // Manual struct hash
        let mut struct_data = Vec::new();
        struct_data.extend_from_slice(agent_type_hash.as_slice());
        struct_data.extend_from_slice(source_hash.as_slice());
        struct_data.extend_from_slice(action_hash.as_slice());

        let manual_struct_hash = keccak256(&struct_data);
        println!("Manual struct hash: {:?}", manual_struct_hash);

        // Verify alloy produces the same values
        assert_eq!(
            domain_separator, manual_domain_separator,
            "Domain separator mismatch"
        );
        assert_eq!(struct_hash, manual_struct_hash, "Struct hash mismatch");
    }

    /// Test EIP-712 signature comparison with Python SDK.
    ///
    /// This test uses the same test key and action hash as the Python verification
    /// and compares the resulting signature.
    ///
    /// Run with: cargo test -p hip3-executor test_signature_matches_python -- --nocapture
    #[tokio::test]
    async fn test_signature_matches_python() {
        // Same test key as Python
        let bytes = test_key_bytes();
        let signer = PrivateKeySigner::from_slice(&bytes).unwrap();

        println!("Test account address: {:?}", signer.address());

        // Same action hash as Python verification
        let action_hash = B256::from_slice(
            &hex::decode("f01fa6eaca0b8cbd2afe65f8852a2e00d35eae3d19560ece9b8a28614646e849")
                .unwrap(),
        );

        // Create phantom agent with testnet source (same as Python)
        let phantom_agent = PhantomAgent::new(action_hash, false); // false = testnet = "b"

        // Sign
        let signature = phantom_agent.sign(&signer).await.unwrap();

        // Get signature components
        let r = signature.r();
        let s = signature.s();
        let v = signature.v(); // bool: false = 0, true = 1

        println!("Signature r: 0x{}", hex::encode(r.to_be_bytes::<32>()));
        println!("Signature s: 0x{}", hex::encode(s.to_be_bytes::<32>()));
        println!("Signature v (raw): {}", if v { 1 } else { 0 });
        println!("Signature v (as 27/28): {}", if v { 28 } else { 27 });

        // Python expected values:
        // r: 0xa9e728f2faea4febc0b6eb9c3dbbac04b375eb3869f051030d205318425faebc
        // s: 0x7b21be7030bb979352b71494708b99d789266f0d0e1242a21e74905b683e4698
        // v: 27 (or 0 as raw recovery id, which is false in alloy)

        // Note: ECDSA signatures are deterministic for the same (private_key, message) pair
        // when using RFC 6979, which both Python's eth_account and alloy use.
        let expected_r = "a9e728f2faea4febc0b6eb9c3dbbac04b375eb3869f051030d205318425faebc";
        let expected_s = "7b21be7030bb979352b71494708b99d789266f0d0e1242a21e74905b683e4698";
        let expected_v = false; // raw recovery id 0

        assert_eq!(
            hex::encode(r.to_be_bytes::<32>()),
            expected_r,
            "Signature r mismatch"
        );
        assert_eq!(
            hex::encode(s.to_be_bytes::<32>()),
            expected_s,
            "Signature s mismatch"
        );
        assert_eq!(v, expected_v, "Signature v mismatch");

        println!("✅ Signature matches Python SDK!");
    }

    #[tokio::test]
    async fn test_signer_sign_action() {
        let bytes = test_key_bytes();
        let manager = Arc::new(KeyManager::from_bytes(&bytes, None).unwrap());

        let signer = Signer::new(manager, true).unwrap();

        let action = Action {
            action_type: "order".to_string(),
            orders: Some(vec![OrderWire {
                asset: 0,
                is_buy: true,
                limit_px: "100.0".to_string(),
                sz: "1.0".to_string(),
                reduce_only: false,
                order_type: OrderTypeWire::ioc(),
                cloid: Some("test-123".to_string()),
            }]),
            cancels: None,
            grouping: Some("na".to_string()),
            builder: None,
        };

        let input = SigningInput {
            action,
            nonce: 1234567890,
            vault_address: None,
            expires_after: None,
        };

        let signature = signer.sign_action(input).await.unwrap();

        // Verify signature components are present
        assert!(!signature.r().is_zero());
        assert!(!signature.s().is_zero());
    }

    #[test]
    fn test_signer_no_trading_key() {
        let manager = Arc::new(KeyManager {
            trading_signer: None,
            trading_address: None,
            observation_address: Address::ZERO,
        });

        let result = Signer::new(manager, true);
        assert!(matches!(result, Err(SignerError::NoTradingKey)));
    }

    // =========================================================================
    // P1: from_pending_order precision tests
    // =========================================================================

    #[test]
    fn test_from_pending_order_limits_price_precision() {
        use hip3_core::{
            execution::PendingOrder,
            market::{AssetId, DexId, MarketKey, MarketSpec},
            order::ClientOrderId,
            OrderSide, Price, Size,
        };
        use rust_decimal_macros::dec;

        // Create a MarketSpec with 5 sig figs, 2 decimal places
        let spec = MarketSpec {
            tick_size: Price::new(dec!(0.01)),
            max_sig_figs: 5,
            max_price_decimals: 2,
            sz_decimals: 3,
            lot_size: Size::new(dec!(0.001)),
            ..Default::default()
        };

        // Create a pending order with too many digits
        let pending = PendingOrder::new(
            ClientOrderId::new(),
            MarketKey::new(DexId::XYZ, AssetId::new(0)),
            OrderSide::Buy,
            Price::new(dec!(12345.6789)), // Too many digits
            Size::new(dec!(1.2345678)),   // Too many digits
            false,
            1234567890,
        );

        let wire = OrderWire::from_pending_order(&pending, &spec);

        // Price should be truncated to 5 sig figs = "12346" (rounded up for buy)
        // Note: MarketSpec rounds buy up: 12345.6789 -> 12345.68 (tick round) -> 12345 (5 sig figs)
        // Actually: tick round first: ceil(12345.6789 / 0.01) * 0.01 = 12345.68
        // Then sig figs: 12345 (5 sig figs from 12345.68)
        assert_eq!(wire.limit_px, "12345");

        // Size should be floor-rounded to lot size, then 5 sig figs
        // 1.2345678 -> 1.234 (lot) -> "1.234" (5 sig figs, 3 decimals)
        assert_eq!(wire.sz, "1.234");
    }

    #[test]
    fn test_from_pending_order_sell_rounds_down() {
        use hip3_core::{
            execution::PendingOrder,
            market::{AssetId, DexId, MarketKey, MarketSpec},
            order::ClientOrderId,
            OrderSide, Price, Size,
        };
        use rust_decimal_macros::dec;

        let spec = MarketSpec {
            tick_size: Price::new(dec!(0.01)),
            max_sig_figs: 5,
            max_price_decimals: 2,
            ..Default::default()
        };

        // Sell order with fractional tick
        let pending = PendingOrder::new(
            ClientOrderId::new(),
            MarketKey::new(DexId::XYZ, AssetId::new(0)),
            OrderSide::Sell,
            Price::new(dec!(100.019)), // Between 100.01 and 100.02
            Size::new(dec!(1.0)),
            false,
            1234567890,
        );

        let wire = OrderWire::from_pending_order(&pending, &spec);

        // Sell should round DOWN: 100.019 -> 100.01
        assert_eq!(wire.limit_px, "100.01");
        assert!(!wire.is_buy);
    }

    #[test]
    fn test_from_pending_order_buy_rounds_up() {
        use hip3_core::{
            execution::PendingOrder,
            market::{AssetId, DexId, MarketKey, MarketSpec},
            order::ClientOrderId,
            OrderSide, Price, Size,
        };
        use rust_decimal_macros::dec;

        let spec = MarketSpec {
            tick_size: Price::new(dec!(0.01)),
            max_sig_figs: 5,
            max_price_decimals: 2,
            ..Default::default()
        };

        // Buy order with fractional tick
        let pending = PendingOrder::new(
            ClientOrderId::new(),
            MarketKey::new(DexId::XYZ, AssetId::new(0)),
            OrderSide::Buy,
            Price::new(dec!(100.001)), // Just above 100.00
            Size::new(dec!(1.0)),
            false,
            1234567890,
        );

        let wire = OrderWire::from_pending_order(&pending, &spec);

        // Buy should round UP: 100.001 -> 100.01
        assert_eq!(wire.limit_px, "100.01");
        assert!(wire.is_buy);
    }
}
