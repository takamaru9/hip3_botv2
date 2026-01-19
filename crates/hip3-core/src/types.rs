//! Common data types for market data.
//!
//! Contains BBO (best bid/offer), AssetCtx (asset context with oracle),
//! and other market data structures.
//!
//! Implements P0-14: BboNull judgment.

use crate::{Price, Size};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// BBO state for P0-14 (null side detection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BboState {
    /// Both bid and ask are present and valid.
    Valid,
    /// No bid side (bid price is zero or missing).
    NoBid,
    /// No ask side (ask price is zero or missing).
    NoAsk,
    /// Both sides missing.
    Empty,
    /// Invalid (e.g., bid >= ask, negative price).
    Invalid,
}

impl BboState {
    /// Check if this state allows trading decisions.
    pub fn is_tradeable(&self) -> bool {
        matches!(self, Self::Valid)
    }

    /// Check if this state should exclude the market from evaluation.
    pub fn should_exclude(&self) -> bool {
        !self.is_tradeable()
    }
}

impl std::fmt::Display for BboState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Valid => write!(f, "VALID"),
            Self::NoBid => write!(f, "NO_BID"),
            Self::NoAsk => write!(f, "NO_ASK"),
            Self::Empty => write!(f, "EMPTY"),
            Self::Invalid => write!(f, "INVALID"),
        }
    }
}

/// Best Bid and Offer (BBO).
///
/// Represents the top of the order book with best prices and sizes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bbo {
    /// Best bid price.
    pub bid_price: Price,
    /// Best bid size.
    pub bid_size: Size,
    /// Best ask price.
    pub ask_price: Price,
    /// Best ask size.
    pub ask_size: Size,
    /// Timestamp when this BBO was received.
    pub received_at: DateTime<Utc>,
}

impl Bbo {
    /// Create a new BBO.
    pub fn new(bid_price: Price, bid_size: Size, ask_price: Price, ask_size: Size) -> Self {
        Self {
            bid_price,
            bid_size,
            ask_price,
            ask_size,
            received_at: Utc::now(),
        }
    }

    /// Calculate mid price: (bid + ask) / 2.
    ///
    /// Returns None if BBO state is not Valid.
    pub fn mid_price(&self) -> Option<Price> {
        if self.state() != BboState::Valid {
            return None;
        }
        Some(Price::new(
            (self.bid_price.inner() + self.ask_price.inner()) / rust_decimal::Decimal::TWO,
        ))
    }

    /// Calculate mid price without state check (for backwards compatibility).
    pub fn mid_price_unchecked(&self) -> Price {
        Price::new((self.bid_price.inner() + self.ask_price.inner()) / rust_decimal::Decimal::TWO)
    }

    /// Calculate spread: ask - bid.
    pub fn spread(&self) -> Price {
        self.ask_price - self.bid_price
    }

    /// Calculate spread in basis points relative to mid.
    pub fn spread_bps(&self) -> Option<rust_decimal::Decimal> {
        let mid = self.mid_price()?;
        if mid.is_zero() {
            return None;
        }
        Some(self.spread().inner() / mid.inner() * rust_decimal::Decimal::from(10000))
    }

    /// Get BBO state (P0-14).
    ///
    /// Determines if BBO is valid, has missing sides, or is invalid.
    pub fn state(&self) -> BboState {
        let has_bid = self.bid_price.is_positive() && self.bid_size.is_positive();
        let has_ask = self.ask_price.is_positive() && self.ask_size.is_positive();

        match (has_bid, has_ask) {
            (false, false) => BboState::Empty,
            (true, false) => BboState::NoAsk,
            (false, true) => BboState::NoBid,
            (true, true) => {
                // Both sides present, check validity
                if self.bid_price < self.ask_price {
                    BboState::Valid
                } else {
                    BboState::Invalid // Crossed book
                }
            }
        }
    }

    /// Check if BBO is valid (bid < ask, both positive).
    ///
    /// Equivalent to `self.state() == BboState::Valid`.
    pub fn is_valid(&self) -> bool {
        self.state() == BboState::Valid
    }

    /// Check if BBO has no bid side (P0-14).
    pub fn is_no_bid(&self) -> bool {
        matches!(self.state(), BboState::NoBid | BboState::Empty)
    }

    /// Check if BBO has no ask side (P0-14).
    pub fn is_no_ask(&self) -> bool {
        matches!(self.state(), BboState::NoAsk | BboState::Empty)
    }

    /// Check if BBO is null (any side missing) (P0-14).
    pub fn is_null(&self) -> bool {
        self.state().should_exclude()
    }

    /// Age of this BBO in milliseconds.
    pub fn age_ms(&self) -> i64 {
        (Utc::now() - self.received_at).num_milliseconds()
    }
}

/// Oracle data from exchange.
///
/// Contains oracle price used as reference for dislocation detection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleData {
    /// Oracle price (primary reference for dislocation).
    pub oracle_px: Price,
    /// Mark price (used for validation/cross-check).
    pub mark_px: Price,
    /// Timestamp of last oracle price change.
    pub oracle_updated_at: DateTime<Utc>,
    /// Timestamp when this data was received.
    pub received_at: DateTime<Utc>,
}

impl OracleData {
    /// Create new oracle data.
    pub fn new(oracle_px: Price, mark_px: Price) -> Self {
        let now = Utc::now();
        Self {
            oracle_px,
            mark_px,
            oracle_updated_at: now,
            received_at: now,
        }
    }

    /// Age of oracle price in milliseconds since last change.
    pub fn oracle_age_ms(&self) -> i64 {
        (Utc::now() - self.oracle_updated_at).num_milliseconds()
    }

    /// Calculate mark-oracle divergence in basis points.
    pub fn mark_oracle_divergence_bps(&self) -> Option<rust_decimal::Decimal> {
        if self.oracle_px.is_zero() {
            return None;
        }
        Some(
            (self.mark_px.inner() - self.oracle_px.inner()).abs() / self.oracle_px.inner()
                * rust_decimal::Decimal::from(10000),
        )
    }

    /// Check if oracle is fresh (within threshold).
    pub fn is_fresh(&self, max_age_ms: i64) -> bool {
        self.oracle_age_ms() < max_age_ms
    }
}

/// Asset context from exchange.
///
/// Contains oracle data, funding rate, and other asset-level information.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetCtx {
    /// Oracle and mark price data.
    pub oracle: OracleData,
    /// Current funding rate.
    pub funding_rate: rust_decimal::Decimal,
    /// Open interest.
    pub open_interest: Size,
    /// Premium (funding basis).
    pub premium: rust_decimal::Decimal,
    /// Timestamp when received.
    pub received_at: DateTime<Utc>,
}

impl AssetCtx {
    /// Create new asset context.
    pub fn new(oracle: OracleData, funding_rate: rust_decimal::Decimal) -> Self {
        Self {
            oracle,
            funding_rate,
            open_interest: Size::ZERO,
            premium: rust_decimal::Decimal::ZERO,
            received_at: Utc::now(),
        }
    }

    /// Age of this context in milliseconds.
    pub fn age_ms(&self) -> i64 {
        (Utc::now() - self.received_at).num_milliseconds()
    }
}

/// Combined market state snapshot.
///
/// Contains all real-time data needed for trading decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSnapshot {
    /// Best bid/offer.
    pub bbo: Bbo,
    /// Asset context with oracle.
    pub ctx: AssetCtx,
    /// Snapshot timestamp.
    pub timestamp: DateTime<Utc>,
}

impl MarketSnapshot {
    /// Create new market snapshot.
    pub fn new(bbo: Bbo, ctx: AssetCtx) -> Self {
        Self {
            bbo,
            ctx,
            timestamp: Utc::now(),
        }
    }

    /// Get BBO state (P0-14).
    pub fn bbo_state(&self) -> BboState {
        self.bbo.state()
    }

    /// Check if snapshot is tradeable (P0-14).
    ///
    /// Returns false if BBO is null (any side missing).
    pub fn is_tradeable(&self) -> bool {
        self.bbo.state().is_tradeable()
    }

    /// Calculate edge: oracle vs best price, accounting for side.
    ///
    /// For buy: `(oracle - ask) / oracle` (positive = ask below oracle = buy opportunity)
    /// For sell: `(bid - oracle) / oracle` (positive = bid above oracle = sell opportunity)
    ///
    /// Returns None if:
    /// - BBO is null (P0-14)
    /// - Oracle price is zero
    pub fn edge_bps(&self, side: crate::OrderSide) -> Option<rust_decimal::Decimal> {
        // P0-14: Check BBO state first
        if !self.is_tradeable() {
            return None;
        }

        let oracle = self.ctx.oracle.oracle_px;
        if oracle.is_zero() {
            return None;
        }

        let edge = match side {
            crate::OrderSide::Buy => oracle.inner() - self.bbo.ask_price.inner(),
            crate::OrderSide::Sell => self.bbo.bid_price.inner() - oracle.inner(),
        };

        Some(edge / oracle.inner() * rust_decimal::Decimal::from(10000))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_bbo_mid_price() {
        let bbo = Bbo::new(
            Price::new(dec!(100)),
            Size::new(dec!(1)),
            Price::new(dec!(102)),
            Size::new(dec!(1)),
        );
        assert_eq!(bbo.mid_price().unwrap().inner(), dec!(101));
    }

    #[test]
    fn test_bbo_spread_bps() {
        let bbo = Bbo::new(
            Price::new(dec!(100)),
            Size::new(dec!(1)),
            Price::new(dec!(101)),
            Size::new(dec!(1)),
        );
        // Spread = 1, mid = 100.5, spread_bps = 1/100.5 * 10000 ≈ 99.5
        let spread_bps = bbo.spread_bps().unwrap();
        assert!(spread_bps > dec!(99) && spread_bps < dec!(100));
    }

    #[test]
    fn test_oracle_freshness() {
        let oracle = OracleData::new(Price::new(dec!(50000)), Price::new(dec!(50010)));

        // Fresh oracle
        assert!(oracle.is_fresh(1000));

        // Would fail if we could control time
    }

    #[test]
    fn test_edge_calculation() {
        use crate::OrderSide;

        let bbo = Bbo::new(
            Price::new(dec!(99)), // bid
            Size::new(dec!(1)),
            Price::new(dec!(101)), // ask
            Size::new(dec!(1)),
        );
        let oracle = OracleData::new(
            Price::new(dec!(100)), // oracle at 100
            Price::new(dec!(100)),
        );
        let ctx = AssetCtx::new(oracle, dec!(0));
        let snapshot = MarketSnapshot::new(bbo, ctx);

        // Buy edge: (100 - 101) / 100 * 10000 = -100 bps (ask above oracle)
        let buy_edge = snapshot.edge_bps(OrderSide::Buy).unwrap();
        assert_eq!(buy_edge, dec!(-100));

        // Sell edge: (99 - 100) / 100 * 10000 = -100 bps (bid below oracle)
        let sell_edge = snapshot.edge_bps(OrderSide::Sell).unwrap();
        assert_eq!(sell_edge, dec!(-100));
    }

    // === P0-14: BboNull tests ===

    #[test]
    fn test_bbo_state_valid() {
        let bbo = Bbo::new(
            Price::new(dec!(100)),
            Size::new(dec!(1)),
            Price::new(dec!(101)),
            Size::new(dec!(1)),
        );
        assert_eq!(bbo.state(), BboState::Valid);
        assert!(bbo.is_valid());
        assert!(!bbo.is_null());
    }

    #[test]
    fn test_bbo_state_no_bid() {
        let bbo = Bbo::new(
            Price::new(dec!(0)), // No bid
            Size::new(dec!(0)),
            Price::new(dec!(101)),
            Size::new(dec!(1)),
        );
        assert_eq!(bbo.state(), BboState::NoBid);
        assert!(!bbo.is_valid());
        assert!(bbo.is_null());
        assert!(bbo.is_no_bid());
    }

    #[test]
    fn test_bbo_state_no_ask() {
        let bbo = Bbo::new(
            Price::new(dec!(100)),
            Size::new(dec!(1)),
            Price::new(dec!(0)), // No ask
            Size::new(dec!(0)),
        );
        assert_eq!(bbo.state(), BboState::NoAsk);
        assert!(!bbo.is_valid());
        assert!(bbo.is_null());
        assert!(bbo.is_no_ask());
    }

    #[test]
    fn test_bbo_state_empty() {
        let bbo = Bbo::new(
            Price::new(dec!(0)),
            Size::new(dec!(0)),
            Price::new(dec!(0)),
            Size::new(dec!(0)),
        );
        assert_eq!(bbo.state(), BboState::Empty);
        assert!(!bbo.is_valid());
        assert!(bbo.is_null());
    }

    #[test]
    fn test_bbo_state_invalid_crossed() {
        let bbo = Bbo::new(
            Price::new(dec!(101)), // Bid higher than ask (crossed)
            Size::new(dec!(1)),
            Price::new(dec!(100)),
            Size::new(dec!(1)),
        );
        assert_eq!(bbo.state(), BboState::Invalid);
        assert!(!bbo.is_valid());
        assert!(bbo.is_null());
    }

    #[test]
    fn test_bbo_null_mid_price() {
        // Mid price should return None for null BBO
        let bbo = Bbo::new(
            Price::new(dec!(0)),
            Size::new(dec!(0)),
            Price::new(dec!(101)),
            Size::new(dec!(1)),
        );
        assert!(bbo.mid_price().is_none());
    }

    #[test]
    fn test_snapshot_not_tradeable_with_null_bbo() {
        use crate::OrderSide;

        let bbo = Bbo::new(
            Price::new(dec!(0)), // No bid
            Size::new(dec!(0)),
            Price::new(dec!(101)),
            Size::new(dec!(1)),
        );
        let oracle = OracleData::new(Price::new(dec!(100)), Price::new(dec!(100)));
        let ctx = AssetCtx::new(oracle, dec!(0));
        let snapshot = MarketSnapshot::new(bbo, ctx);

        assert!(!snapshot.is_tradeable());
        assert_eq!(snapshot.bbo_state(), BboState::NoBid);

        // Edge calculation should return None for null BBO
        assert!(snapshot.edge_bps(OrderSide::Buy).is_none());
        assert!(snapshot.edge_bps(OrderSide::Sell).is_none());
    }

    #[test]
    fn test_bbo_state_display() {
        assert_eq!(BboState::Valid.to_string(), "VALID");
        assert_eq!(BboState::NoBid.to_string(), "NO_BID");
        assert_eq!(BboState::NoAsk.to_string(), "NO_ASK");
        assert_eq!(BboState::Empty.to_string(), "EMPTY");
        assert_eq!(BboState::Invalid.to_string(), "INVALID");
    }
}
