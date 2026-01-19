//! Order-related types and identifiers.
//!
//! Provides order side, type, time-in-force, and client order ID types
//! for the trading system.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Order side: buy or sell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderSide {
    Buy,
    Sell,
}

impl OrderSide {
    /// Returns the opposite side.
    pub fn opposite(&self) -> Self {
        match self {
            Self::Buy => Self::Sell,
            Self::Sell => Self::Buy,
        }
    }

    /// Returns 1 for buy, -1 for sell (for position calculations).
    pub fn sign(&self) -> i8 {
        match self {
            Self::Buy => 1,
            Self::Sell => -1,
        }
    }
}

impl fmt::Display for OrderSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Buy => write!(f, "buy"),
            Self::Sell => write!(f, "sell"),
        }
    }
}

/// Order type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderType {
    /// Limit order.
    Limit,
    /// Market order (not recommended for HIP-3).
    Market,
}

impl fmt::Display for OrderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit => write!(f, "limit"),
            Self::Market => write!(f, "market"),
        }
    }
}

/// Time-in-force for orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TimeInForce {
    /// Good-til-cancelled.
    #[serde(rename = "Gtc")]
    GoodTilCancelled,
    /// Immediate-or-cancel (our primary TIF for taker strategy).
    #[default]
    #[serde(rename = "Ioc")]
    ImmediateOrCancel,
    /// Add-liquidity-only.
    #[serde(rename = "Alo")]
    AddLiquidityOnly,
}

impl fmt::Display for TimeInForce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GoodTilCancelled => write!(f, "Gtc"),
            Self::ImmediateOrCancel => write!(f, "Ioc"),
            Self::AddLiquidityOnly => write!(f, "Alo"),
        }
    }
}

/// Client order ID for idempotency.
///
/// CRITICAL: Every order must have a unique cloid to prevent
/// duplicate submissions on retries. This is non-negotiable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClientOrderId(String);

impl ClientOrderId {
    /// Create a new unique client order ID.
    ///
    /// Format: `hip3_{timestamp_ms}_{uuid_short}`
    pub fn new() -> Self {
        let ts = chrono::Utc::now().timestamp_millis();
        let uuid_short = &Uuid::new_v4().to_string()[..8];
        Self(format!("hip3_{ts}_{uuid_short}"))
    }

    /// Create from an existing string (for parsing responses).
    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ClientOrderId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ClientOrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for ClientOrderId {
    fn from(s: String) -> Self {
        Self::from_string(s)
    }
}

impl AsRef<str> for ClientOrderId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_order_side_opposite() {
        assert_eq!(OrderSide::Buy.opposite(), OrderSide::Sell);
        assert_eq!(OrderSide::Sell.opposite(), OrderSide::Buy);
    }

    #[test]
    fn test_order_side_sign() {
        assert_eq!(OrderSide::Buy.sign(), 1);
        assert_eq!(OrderSide::Sell.sign(), -1);
    }

    #[test]
    fn test_client_order_id_unique() {
        let id1 = ClientOrderId::new();
        let id2 = ClientOrderId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_client_order_id_format() {
        let id = ClientOrderId::new();
        assert!(id.as_str().starts_with("hip3_"));
    }
}
