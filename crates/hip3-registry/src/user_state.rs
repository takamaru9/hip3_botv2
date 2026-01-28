//! User state and fee fetching for HIP-3 (P0-24).
//!
//! Provides REST API types for fetching user-specific information including:
//! - Fee tier and rates (taker/maker fees)
//! - Account state
//!
//! Used at startup to configure FeeCalculator with user-specific rates.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Raw user fee response from exchange REST API.
///
/// Endpoint: GET /info (with user address)
/// Response includes fee schedule based on user's trading volume tier.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawUserFeesResponse {
    /// Taker fee rate as string (e.g., "0.0002" for 2 bps).
    #[serde(rename = "takerRate")]
    pub taker_rate: String,
    /// Maker fee rate as string (can be negative for rebates).
    #[serde(rename = "makerRate")]
    pub maker_rate: String,
    /// Fee tier name (e.g., "tier1", "vip1").
    #[serde(default)]
    pub tier: Option<String>,
    /// Whether user has VIP status.
    #[serde(rename = "isVip", default)]
    pub is_vip: bool,
}

impl RawUserFeesResponse {
    /// Parse taker rate to basis points.
    ///
    /// Converts string rate (e.g., "0.0002") to bps (e.g., 2.0).
    pub fn taker_bps(&self) -> Result<Decimal, rust_decimal::Error> {
        let rate: Decimal = self.taker_rate.parse()?;
        Ok(rate * Decimal::from(10000))
    }

    /// Parse maker rate to basis points.
    pub fn maker_bps(&self) -> Result<Decimal, rust_decimal::Error> {
        let rate: Decimal = self.maker_rate.parse()?;
        Ok(rate * Decimal::from(10000))
    }

    /// Get tier name or default.
    pub fn tier_name(&self) -> String {
        self.tier.clone().unwrap_or_else(|| "default".to_string())
    }
}

/// Parsed user fees for use with FeeCalculator.
///
/// Use `from_response()` to convert from REST API response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedUserFees {
    /// Taker fee in basis points.
    pub taker_bps: Decimal,
    /// Maker fee in basis points.
    pub maker_bps: Decimal,
    /// Fee tier name.
    pub tier: String,
    /// Whether user has VIP status.
    pub is_vip: bool,
}

impl ParsedUserFees {
    /// Parse from raw REST API response.
    pub fn from_response(response: &RawUserFeesResponse) -> Result<Self, rust_decimal::Error> {
        Ok(Self {
            taker_bps: response.taker_bps()?,
            maker_bps: response.maker_bps()?,
            tier: response.tier_name(),
            is_vip: response.is_vip,
        })
    }

    /// Default fees for when API is unavailable.
    pub fn default_fees() -> Self {
        Self {
            taker_bps: Decimal::from(2), // 2 bps default
            maker_bps: Decimal::ONE,     // 1 bps default
            tier: "default".to_string(),
            is_vip: false,
        }
    }
}

impl Default for ParsedUserFees {
    fn default() -> Self {
        Self::default_fees()
    }
}

/// User state response from exchange REST API.
///
/// Contains comprehensive user information including fees, positions, and balances.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawUserStateResponse {
    /// User's fee schedule.
    #[serde(rename = "feeSchedule")]
    pub fee_schedule: Option<RawUserFeesResponse>,
    /// Margin summary (optional).
    #[serde(rename = "marginSummary")]
    pub margin_summary: Option<MarginSummary>,
    /// Cross margin summary (optional).
    #[serde(rename = "crossMarginSummary")]
    pub cross_margin_summary: Option<MarginSummary>,
}

/// Margin summary from user state.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MarginSummary {
    /// Account value in USD.
    #[serde(rename = "accountValue")]
    pub account_value: String,
    /// Total notional position value.
    #[serde(rename = "totalNtlPos")]
    pub total_notional_position: String,
    /// Total raw USD.
    #[serde(rename = "totalRawUsd")]
    pub total_raw_usd: String,
    /// Total margin used.
    #[serde(rename = "totalMarginUsed")]
    pub total_margin_used: String,
}

impl MarginSummary {
    /// Parse account value to Decimal.
    pub fn account_value_decimal(&self) -> Result<Decimal, rust_decimal::Error> {
        self.account_value.parse()
    }
}

/// clearinghouseState response from Hyperliquid API.
///
/// Endpoint: POST /info with `{"type": "clearinghouseState", "user": "<address>"}`
/// Contains full account state including open positions.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClearinghouseStateResponse {
    /// Margin summary.
    #[serde(rename = "marginSummary")]
    pub margin_summary: Option<MarginSummary>,
    /// Cross margin summary.
    #[serde(rename = "crossMarginSummary")]
    pub cross_margin_summary: Option<MarginSummary>,
    /// Cross maintenance margin used.
    #[serde(rename = "crossMaintenanceMarginUsed")]
    pub cross_maintenance_margin_used: Option<String>,
    /// Withdrawable balance.
    pub withdrawable: Option<String>,
    /// Open positions.
    #[serde(rename = "assetPositions", default)]
    pub asset_positions: Vec<AssetPositionEntry>,
    /// Timestamp in milliseconds.
    pub time: Option<u64>,
}

/// Asset position entry from clearinghouseState.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AssetPositionEntry {
    /// Position details.
    pub position: AssetPositionData,
    /// Position type ("oneWay" or "twoWay").
    #[serde(rename = "type")]
    pub position_type: Option<String>,
}

/// Position data within AssetPositionEntry.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AssetPositionData {
    /// Coin identifier (e.g., "xyz:SILVER").
    pub coin: String,
    /// Position size (signed: positive = long, negative = short).
    pub szi: String,
    /// Entry price.
    #[serde(rename = "entryPx")]
    pub entry_px: Option<String>,
    /// Liquidation price.
    #[serde(rename = "liquidationPx")]
    pub liquidation_px: Option<String>,
    /// Position value.
    #[serde(rename = "positionValue")]
    pub position_value: Option<String>,
    /// Unrealized PnL.
    #[serde(rename = "unrealizedPnl")]
    pub unrealized_pnl: Option<String>,
    /// Return on position.
    #[serde(rename = "returnOnEquity")]
    pub return_on_equity: Option<String>,
    /// Leverage.
    pub leverage: Option<LeverageInfo>,
    /// Cumulative funding.
    #[serde(rename = "cumFunding")]
    pub cum_funding: Option<CumFunding>,
    /// Margin used.
    #[serde(rename = "marginUsed")]
    pub margin_used: Option<String>,
    /// Max trade sizes.
    #[serde(rename = "maxTradeSzs")]
    pub max_trade_szs: Option<[String; 2]>,
}

/// Leverage information.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LeverageInfo {
    /// Leverage type.
    #[serde(rename = "type")]
    pub leverage_type: Option<String>,
    /// Leverage value.
    pub value: Option<u32>,
    /// Raw USD amount.
    #[serde(rename = "rawUsd")]
    pub raw_usd: Option<String>,
}

/// Cumulative funding information.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CumFunding {
    /// All time funding.
    #[serde(rename = "allTime")]
    pub all_time: Option<String>,
    /// Since position open.
    #[serde(rename = "sinceOpen")]
    pub since_open: Option<String>,
    /// Since position change.
    #[serde(rename = "sinceChange")]
    pub since_change: Option<String>,
}

impl AssetPositionData {
    /// Parse position size to Decimal.
    pub fn size_decimal(&self) -> Result<Decimal, rust_decimal::Error> {
        self.szi.parse()
    }

    /// Parse entry price to Decimal.
    pub fn entry_price_decimal(&self) -> Result<Decimal, rust_decimal::Error> {
        self.entry_px
            .as_ref()
            .ok_or(rust_decimal::Error::ExceedsMaximumPossibleValue)
            .and_then(|px| px.parse())
    }

    /// Check if position is long (positive size).
    pub fn is_long(&self) -> bool {
        self.size_decimal()
            .map(|sz| sz > Decimal::ZERO)
            .unwrap_or(false)
    }

    /// Check if position is short (negative size).
    pub fn is_short(&self) -> bool {
        self.size_decimal()
            .map(|sz| sz < Decimal::ZERO)
            .unwrap_or(false)
    }

    /// Check if position is empty (zero size).
    pub fn is_empty(&self) -> bool {
        self.size_decimal()
            .map(|sz| sz == Decimal::ZERO)
            .unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_parse_user_fees() {
        let response = RawUserFeesResponse {
            taker_rate: "0.0002".to_string(), // 2 bps
            maker_rate: "0.0001".to_string(), // 1 bps
            tier: Some("tier1".to_string()),
            is_vip: false,
        };

        assert_eq!(response.taker_bps().unwrap(), dec!(2));
        assert_eq!(response.maker_bps().unwrap(), dec!(1));
        assert_eq!(response.tier_name(), "tier1");
    }

    #[test]
    fn test_parse_vip_fees() {
        let response = RawUserFeesResponse {
            taker_rate: "0.00015".to_string(),  // 1.5 bps (VIP rate)
            maker_rate: "-0.00005".to_string(), // -0.5 bps (maker rebate)
            tier: Some("vip1".to_string()),
            is_vip: true,
        };

        let parsed = ParsedUserFees::from_response(&response).unwrap();
        assert_eq!(parsed.taker_bps, dec!(1.5));
        assert_eq!(parsed.maker_bps, dec!(-0.5));
        assert!(parsed.is_vip);
        assert_eq!(parsed.tier, "vip1");
    }

    #[test]
    fn test_default_fees() {
        let fees = ParsedUserFees::default_fees();
        assert_eq!(fees.taker_bps, dec!(2));
        assert_eq!(fees.maker_bps, dec!(1));
        assert_eq!(fees.tier, "default");
        assert!(!fees.is_vip);
    }

    #[test]
    fn test_missing_tier_defaults() {
        let response = RawUserFeesResponse {
            taker_rate: "0.0002".to_string(),
            maker_rate: "0.0001".to_string(),
            tier: None,
            is_vip: false,
        };

        assert_eq!(response.tier_name(), "default");
    }

    #[test]
    fn test_margin_summary_parsing() {
        let summary = MarginSummary {
            account_value: "10000.50".to_string(),
            total_notional_position: "5000.00".to_string(),
            total_margin_used: "1000.00".to_string(),
            withdrawable: "9000.50".to_string(),
        };

        assert_eq!(summary.account_value_decimal().unwrap(), dec!(10000.50));
    }
}
