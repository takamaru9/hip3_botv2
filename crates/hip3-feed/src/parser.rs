//! Message parsing for market data.
//!
//! Parses WebSocket messages into typed market data structures.
//! Implements P0-30: Perps/Spot混在封じ (spot channel rejection).
//!
//! Supports two channel formats:
//! 1. Internal format: "bbo:perp:0" (type:market:index)
//! 2. Hyperliquid format: "bbo" with coin in data ({"coin": "BTC", "bbo": ...})

use crate::error::{FeedError, FeedResult};
use hip3_core::{AssetCtx, AssetId, Bbo, DexId, MarketKey, OracleData, Price, Size};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, warn};

/// Statistics for spot rejection (P0-30).
#[derive(Debug, Default)]
pub struct SpotRejectionStats {
    /// Number of spot channels rejected.
    pub rejected_count: AtomicU64,
    /// Number of perp channels accepted.
    pub accepted_count: AtomicU64,
}

impl SpotRejectionStats {
    pub fn record_rejected(&self) {
        self.rejected_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_accepted(&self) {
        self.accepted_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn rejected(&self) -> u64 {
        self.rejected_count.load(Ordering::Relaxed)
    }

    pub fn accepted(&self) -> u64 {
        self.accepted_count.load(Ordering::Relaxed)
    }
}

/// Raw BBO message from WebSocket (internal format).
#[derive(Debug, Deserialize)]
pub struct RawBbo {
    #[serde(rename = "coin")]
    pub asset_name: String,
    pub px: String,
    pub sz: String,
    pub side: String,
}

/// Hyperliquid BBO message format.
/// Format: {"coin": "BTC", "time": 123456789, "bbo": [[px, sz, n], [px, sz, n]]}
#[derive(Debug, Deserialize)]
pub struct HyperliquidBbo {
    pub coin: String,
    #[serde(default)]
    pub time: Option<i64>,
    /// BBO array: [bid_level, ask_level] where each level is [px, sz, n] or null
    pub bbo: (Option<HyperliquidLevel>, Option<HyperliquidLevel>),
}

/// Hyperliquid order book level.
#[derive(Debug, Deserialize)]
pub struct HyperliquidLevel {
    pub px: String,
    pub sz: String,
    pub n: i32,
}

/// Hyperliquid activeAssetCtx message format.
/// Format: {"coin": "BTC", "ctx": {...}}
#[derive(Debug, Deserialize)]
pub struct HyperliquidAssetCtx {
    pub coin: String,
    pub ctx: HyperliquidCtxData,
}

/// Hyperliquid perps asset context data.
/// Note: Hyperliquid sends numeric values as strings.
#[derive(Debug, Deserialize)]
pub struct HyperliquidCtxData {
    #[serde(rename = "oraclePx")]
    pub oracle_px: String,
    #[serde(rename = "markPx")]
    pub mark_px: String,
    pub funding: String,
    #[serde(rename = "openInterest")]
    pub open_interest: String,
    #[serde(rename = "dayNtlVlm", default)]
    pub day_ntl_vlm: Option<String>,
    #[serde(rename = "prevDayPx", default)]
    pub prev_day_px: Option<String>,
    #[serde(rename = "midPx", default)]
    pub mid_px: Option<String>,
}

/// Raw asset context from WebSocket (internal format).
#[derive(Debug, Deserialize)]
pub struct RawAssetCtx {
    #[serde(rename = "oraclePx")]
    pub oracle_px: String,
    #[serde(rename = "markPx")]
    pub mark_px: String,
    #[serde(rename = "funding")]
    pub funding: String,
    #[serde(rename = "openInterest")]
    pub open_interest: String,
    #[serde(rename = "premium", default)]
    pub premium: Option<String>,
}

/// Parsed market data event.
#[derive(Debug, Clone)]
pub enum MarketEvent {
    /// BBO update.
    BboUpdate { key: MarketKey, bbo: Bbo },
    /// Asset context update.
    CtxUpdate { key: MarketKey, ctx: AssetCtx },
}

/// Channel type extracted from channel name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelType {
    /// Perpetual futures (allowed).
    Perp,
    /// Spot market (rejected).
    Spot,
    /// Unknown type.
    Unknown,
}

/// Message parser.
pub struct MessageParser {
    /// DEX ID for xyz (always 0 for Phase A).
    dex_id: DexId,
    /// Statistics for spot rejection.
    spot_stats: SpotRejectionStats,
    /// Coin symbol to asset index mapping (for Hyperliquid format).
    coin_to_idx: HashMap<String, u16>,
}

impl MessageParser {
    /// Create a new message parser.
    pub fn new() -> Self {
        Self {
            dex_id: DexId::XYZ,
            spot_stats: SpotRejectionStats::default(),
            coin_to_idx: HashMap::new(),
        }
    }

    /// Create a new message parser with coin mapping.
    pub fn with_coin_mapping(coin_mapping: HashMap<String, u16>) -> Self {
        Self {
            dex_id: DexId::XYZ,
            spot_stats: SpotRejectionStats::default(),
            coin_to_idx: coin_mapping,
        }
    }

    /// Add a coin to asset index mapping.
    pub fn add_coin_mapping(&mut self, coin: String, asset_idx: u16) {
        self.coin_to_idx.insert(coin.to_uppercase(), asset_idx);
    }

    /// Get spot rejection statistics.
    pub fn spot_stats(&self) -> &SpotRejectionStats {
        &self.spot_stats
    }

    /// Parse channel message into market event.
    ///
    /// Implements P0-30: Rejects spot channels, only accepts perp channels.
    ///
    /// Supports two formats:
    /// 1. Internal: "bbo:perp:0" or "assetCtx:perp:0"
    /// 2. Hyperliquid: "bbo" or "activeAssetCtx" with coin in data
    pub fn parse_channel_message(
        &self,
        channel: &str,
        data: &serde_json::Value,
    ) -> FeedResult<Option<MarketEvent>> {
        // Check for Hyperliquid simple channel format first
        if channel == "bbo" {
            return self.parse_hyperliquid_bbo(data);
        }

        if channel == "activeAssetCtx" {
            return self.parse_hyperliquid_asset_ctx(data);
        }

        // P0-30: Validate channel type (perps only) for internal format
        let channel_type = self.extract_channel_type(channel);

        match channel_type {
            ChannelType::Spot => {
                self.spot_stats.record_rejected();
                warn!(channel = %channel, "Spot channel rejected (P0-30)");
                return Err(FeedError::SpotRejected(channel.to_string()));
            }
            ChannelType::Unknown => {
                // Don't count unknown as rejected, just ignore
                debug!(channel = %channel, "Unknown channel type, ignoring");
                return Ok(None);
            }
            ChannelType::Perp => {
                self.spot_stats.record_accepted();
            }
        }

        if channel.starts_with("bbo") {
            return self.parse_bbo(channel, data);
        }

        if channel.starts_with("assetCtx") {
            return self.parse_asset_ctx(channel, data);
        }

        // Ignore other channels
        Ok(None)
    }

    /// Parse Hyperliquid BBO format.
    fn parse_hyperliquid_bbo(&self, data: &serde_json::Value) -> FeedResult<Option<MarketEvent>> {
        let hl_bbo: HyperliquidBbo = serde_json::from_value(data.clone())
            .map_err(|e| FeedError::ParseError(format!("Invalid Hyperliquid BBO: {e}")))?;

        // Look up asset index from coin
        let asset_idx = self
            .coin_to_idx
            .get(&hl_bbo.coin.to_uppercase())
            .copied()
            .ok_or_else(|| {
                FeedError::ParseError(format!("Unknown coin: {} (not in mapping)", hl_bbo.coin))
            })?;

        let key = MarketKey::new(self.dex_id, AssetId::new(asset_idx));

        // Extract bid and ask from BBO
        let (bid_price, bid_size) = match &hl_bbo.bbo.0 {
            Some(level) => (self.parse_price(&level.px)?, self.parse_size(&level.sz)?),
            None => (Price::new(Decimal::ZERO), Size::new(Decimal::ZERO)),
        };

        let (ask_price, ask_size) = match &hl_bbo.bbo.1 {
            Some(level) => (self.parse_price(&level.px)?, self.parse_size(&level.sz)?),
            None => (Price::new(Decimal::ZERO), Size::new(Decimal::ZERO)),
        };

        let bbo = Bbo::new(bid_price, bid_size, ask_price, ask_size);

        self.spot_stats.record_accepted();

        debug!(
            ?key,
            coin = %hl_bbo.coin,
            bid = %bbo.bid_price,
            ask = %bbo.ask_price,
            "Hyperliquid BBO update"
        );
        Ok(Some(MarketEvent::BboUpdate { key, bbo }))
    }

    /// Parse Hyperliquid activeAssetCtx format.
    fn parse_hyperliquid_asset_ctx(
        &self,
        data: &serde_json::Value,
    ) -> FeedResult<Option<MarketEvent>> {
        let hl_ctx: HyperliquidAssetCtx = serde_json::from_value(data.clone())
            .map_err(|e| FeedError::ParseError(format!("Invalid Hyperliquid AssetCtx: {e}")))?;

        // Look up asset index from coin
        let asset_idx = self
            .coin_to_idx
            .get(&hl_ctx.coin.to_uppercase())
            .copied()
            .ok_or_else(|| {
                FeedError::ParseError(format!("Unknown coin: {} (not in mapping)", hl_ctx.coin))
            })?;

        let key = MarketKey::new(self.dex_id, AssetId::new(asset_idx));

        // Parse string values to Decimal
        let oracle_px = self.parse_price(&hl_ctx.ctx.oracle_px)?;
        let mark_px = self.parse_price(&hl_ctx.ctx.mark_px)?;
        let funding: Decimal = hl_ctx
            .ctx
            .funding
            .parse()
            .map_err(|_| FeedError::ParseError("Invalid funding rate".to_string()))?;
        let oi = self.parse_size(&hl_ctx.ctx.open_interest)?;

        let oracle = OracleData::new(oracle_px, mark_px);
        let mut ctx = AssetCtx::new(oracle, funding);
        ctx.open_interest = oi;

        self.spot_stats.record_accepted();

        debug!(
            ?key,
            coin = %hl_ctx.coin,
            oracle = %oracle_px,
            mark = %mark_px,
            funding = %funding,
            "Hyperliquid AssetCtx update"
        );
        Ok(Some(MarketEvent::CtxUpdate { key, ctx }))
    }

    /// Extract channel type from channel name.
    ///
    /// Channel format: "channel:type:index" e.g., "bbo:perp:0" or "bbo:spot:0"
    fn extract_channel_type(&self, channel: &str) -> ChannelType {
        let parts: Vec<&str> = channel.split(':').collect();
        if parts.len() >= 2 {
            match parts[1].to_lowercase().as_str() {
                "perp" => ChannelType::Perp,
                "spot" => ChannelType::Spot,
                _ => ChannelType::Unknown,
            }
        } else {
            ChannelType::Unknown
        }
    }

    fn parse_bbo(
        &self,
        channel: &str,
        data: &serde_json::Value,
    ) -> FeedResult<Option<MarketEvent>> {
        // Extract asset index from channel name (e.g., "bbo:perp:0")
        let asset_idx = self.extract_asset_index(channel)?;
        let key = MarketKey::new(self.dex_id, AssetId::new(asset_idx));

        // Parse BBO data
        // HIP-3 sends bbo as array: [[bid_px, bid_sz], [ask_px, ask_sz]]
        let bids_asks = data
            .as_array()
            .ok_or_else(|| FeedError::ParseError("BBO data is not an array".to_string()))?;

        if bids_asks.len() < 2 {
            return Err(FeedError::ParseError("BBO array too short".to_string()));
        }

        let bid = self.parse_level(&bids_asks[0])?;
        let ask = self.parse_level(&bids_asks[1])?;

        let bbo = Bbo::new(bid.0, bid.1, ask.0, ask.1);

        if !bbo.is_valid() {
            warn!(
                ?key,
                bid = %bbo.bid_price,
                ask = %bbo.ask_price,
                "Invalid BBO"
            );
            return Err(FeedError::InvalidData("Invalid BBO".to_string()));
        }

        debug!(?key, bid = %bbo.bid_price, ask = %bbo.ask_price, "BBO update");
        Ok(Some(MarketEvent::BboUpdate { key, bbo }))
    }

    fn parse_asset_ctx(
        &self,
        channel: &str,
        data: &serde_json::Value,
    ) -> FeedResult<Option<MarketEvent>> {
        let asset_idx = self.extract_asset_index(channel)?;
        let key = MarketKey::new(self.dex_id, AssetId::new(asset_idx));

        let raw: RawAssetCtx = serde_json::from_value(data.clone())
            .map_err(|e| FeedError::ParseError(e.to_string()))?;

        let oracle_px = self.parse_price(&raw.oracle_px)?;
        let mark_px = self.parse_price(&raw.mark_px)?;
        let funding: Decimal = raw
            .funding
            .parse()
            .map_err(|_| FeedError::ParseError("Invalid funding rate".to_string()))?;
        let oi = self.parse_size(&raw.open_interest)?;

        let oracle = OracleData::new(oracle_px, mark_px);
        let mut ctx = AssetCtx::new(oracle, funding);
        ctx.open_interest = oi;

        if let Some(premium_str) = &raw.premium {
            if let Ok(premium) = premium_str.parse() {
                ctx.premium = premium;
            }
        }

        debug!(
            ?key,
            oracle = %oracle_px,
            mark = %mark_px,
            funding = %funding,
            "AssetCtx update"
        );
        Ok(Some(MarketEvent::CtxUpdate { key, ctx }))
    }

    fn extract_asset_index(&self, channel: &str) -> FeedResult<u16> {
        // Channel format: "channel:type:index" e.g., "bbo:perp:0"
        let parts: Vec<&str> = channel.split(':').collect();
        if parts.len() >= 3 {
            parts[2]
                .parse()
                .map_err(|_| FeedError::ParseError(format!("Invalid asset index in {channel}")))
        } else {
            // Fallback: try to parse last part
            parts.last().and_then(|s| s.parse().ok()).ok_or_else(|| {
                FeedError::ParseError(format!("Cannot extract asset index from {channel}"))
            })
        }
    }

    fn parse_level(&self, level: &serde_json::Value) -> FeedResult<(Price, Size)> {
        let arr = level
            .as_array()
            .ok_or_else(|| FeedError::ParseError("Level is not an array".to_string()))?;

        if arr.len() < 2 {
            return Err(FeedError::ParseError("Level array too short".to_string()));
        }

        let px_str = arr[0]
            .as_str()
            .ok_or_else(|| FeedError::ParseError("Price is not a string".to_string()))?;
        let sz_str = arr[1]
            .as_str()
            .ok_or_else(|| FeedError::ParseError("Size is not a string".to_string()))?;

        let price = self.parse_price(px_str)?;
        let size = self.parse_size(sz_str)?;

        Ok((price, size))
    }

    fn parse_price(&self, s: &str) -> FeedResult<Price> {
        let d: Decimal = s
            .parse()
            .map_err(|_| FeedError::ParseError(format!("Invalid price: {s}")))?;
        Ok(Price::new(d))
    }

    fn parse_size(&self, s: &str) -> FeedResult<Size> {
        let d: Decimal = s
            .parse()
            .map_err(|_| FeedError::ParseError(format!("Invalid size: {s}")))?;
        Ok(Size::new(d))
    }
}

impl Default for MessageParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_bbo() {
        let parser = MessageParser::new();
        let data = json!([["50000.00", "1.5"], ["50010.00", "2.0"]]);

        let result = parser.parse_bbo("bbo:perp:0", &data).unwrap();
        assert!(result.is_some());

        if let Some(MarketEvent::BboUpdate { key, bbo }) = result {
            assert_eq!(key.asset.index(), 0);
            assert_eq!(bbo.bid_price.to_string(), "50000.00");
            assert_eq!(bbo.ask_price.to_string(), "50010.00");
        } else {
            panic!("Expected BboUpdate");
        }
    }

    #[test]
    fn test_parse_asset_ctx() {
        let parser = MessageParser::new();
        let data = json!({
            "oraclePx": "50005.00",
            "markPx": "50007.00",
            "funding": "0.0001",
            "openInterest": "1000.0"
        });

        let result = parser.parse_asset_ctx("assetCtx:perp:0", &data).unwrap();
        assert!(result.is_some());

        if let Some(MarketEvent::CtxUpdate { key, ctx }) = result {
            assert_eq!(key.asset.index(), 0);
            assert_eq!(ctx.oracle.oracle_px.to_string(), "50005.00");
        } else {
            panic!("Expected CtxUpdate");
        }
    }

    // === P0-30: Perps/Spot混在封じ tests ===

    #[test]
    fn test_spot_channel_rejected() {
        let parser = MessageParser::new();
        let data = json!([["50000.00", "1.5"], ["50010.00", "2.0"]]);

        // Spot channel should be rejected
        let result = parser.parse_channel_message("bbo:spot:0", &data);
        assert!(result.is_err());

        if let Err(FeedError::SpotRejected(channel)) = result {
            assert_eq!(channel, "bbo:spot:0");
        } else {
            panic!("Expected SpotRejected error");
        }

        // Check stats
        assert_eq!(parser.spot_stats().rejected(), 1);
        assert_eq!(parser.spot_stats().accepted(), 0);
    }

    #[test]
    fn test_perp_channel_accepted() {
        let parser = MessageParser::new();
        let data = json!([["50000.00", "1.5"], ["50010.00", "2.0"]]);

        // Perp channel should be accepted
        let result = parser.parse_channel_message("bbo:perp:0", &data);
        assert!(result.is_ok());

        // Check stats
        assert_eq!(parser.spot_stats().rejected(), 0);
        assert_eq!(parser.spot_stats().accepted(), 1);
    }

    #[test]
    fn test_unknown_channel_type_ignored() {
        let parser = MessageParser::new();
        let data = json!({});

        // Unknown channel type should return None, not error
        let result = parser.parse_channel_message("unknown:type:0", &data);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());

        // Unknown should not count as rejected or accepted
        assert_eq!(parser.spot_stats().rejected(), 0);
        assert_eq!(parser.spot_stats().accepted(), 0);
    }

    #[test]
    fn test_extract_channel_type() {
        let parser = MessageParser::new();

        assert_eq!(parser.extract_channel_type("bbo:perp:0"), ChannelType::Perp);
        assert_eq!(
            parser.extract_channel_type("assetCtx:perp:5"),
            ChannelType::Perp
        );
        assert_eq!(parser.extract_channel_type("bbo:spot:0"), ChannelType::Spot);
        assert_eq!(
            parser.extract_channel_type("assetCtx:spot:5"),
            ChannelType::Spot
        );
        assert_eq!(parser.extract_channel_type("unknown"), ChannelType::Unknown);
        assert_eq!(parser.extract_channel_type("bbo:PERP:0"), ChannelType::Perp); // Case insensitive
        assert_eq!(parser.extract_channel_type("bbo:SPOT:0"), ChannelType::Spot);
        // Case insensitive
    }

    #[test]
    fn test_spot_rejection_stats_accumulate() {
        let parser = MessageParser::new();
        let data = json!([["50000.00", "1.5"], ["50010.00", "2.0"]]);

        // Multiple spot rejections
        let _ = parser.parse_channel_message("bbo:spot:0", &data);
        let _ = parser.parse_channel_message("bbo:spot:1", &data);
        let _ = parser.parse_channel_message("assetCtx:spot:0", &data);

        // Multiple perp acceptances
        let _ = parser.parse_channel_message("bbo:perp:0", &data);
        let _ = parser.parse_channel_message("bbo:perp:1", &data);

        assert_eq!(parser.spot_stats().rejected(), 3);
        assert_eq!(parser.spot_stats().accepted(), 2);
    }
}
