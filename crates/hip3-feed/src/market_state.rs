//! Market state aggregation.
//!
//! Combines BBO, AssetCtx, and other market data into a unified
//! state per market.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hip3_core::types::MarketSnapshot;
use hip3_core::{AssetCtx, Bbo, MarketKey, Price};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Instant;
use tracing::debug;

/// Per-market state container.
#[derive(Debug)]
pub struct MarketStateEntry {
    /// Best bid/offer.
    pub bbo: Option<Bbo>,
    /// Asset context with oracle.
    pub ctx: Option<AssetCtx>,
    /// Last update time.
    pub last_update: DateTime<Utc>,
    /// Oracle price at last change.
    pub last_oracle_px: Option<Price>,
    /// Time when oracle price last changed.
    pub oracle_changed_at: Option<DateTime<Utc>>,
    /// Monotonic time when BBO was last received (P0-12).
    pub bbo_recv_mono: Option<Instant>,
    /// Monotonic time when AssetCtx was last received (P0-12).
    pub ctx_recv_mono: Option<Instant>,
    /// BBO server time from WebSocket (for TimeRegression P0-16).
    pub bbo_server_time: Option<i64>,
}

impl MarketStateEntry {
    fn new() -> Self {
        Self {
            bbo: None,
            ctx: None,
            last_update: Utc::now(),
            last_oracle_px: None,
            oracle_changed_at: None,
            bbo_recv_mono: None,
            ctx_recv_mono: None,
            bbo_server_time: None,
        }
    }

    /// Check if state is complete (has both BBO and ctx).
    pub fn is_complete(&self) -> bool {
        self.bbo.is_some() && self.ctx.is_some()
    }

    /// Get market snapshot if complete.
    pub fn snapshot(&self) -> Option<MarketSnapshot> {
        match (&self.bbo, &self.ctx) {
            (Some(bbo), Some(ctx)) => Some(MarketSnapshot::new(bbo.clone(), ctx.clone())),
            _ => None,
        }
    }

    /// Update BBO with optional server time.
    ///
    /// # Arguments
    /// - `bbo`: The new BBO data
    /// - `server_time`: Optional server timestamp from WebSocket (for TimeRegression)
    pub fn update_bbo(&mut self, bbo: Bbo, server_time: Option<i64>) {
        self.bbo = Some(bbo);
        self.last_update = Utc::now();
        self.bbo_recv_mono = Some(Instant::now());
        self.bbo_server_time = server_time;
    }

    /// Update asset context with oracle tracking.
    pub fn update_ctx(&mut self, ctx: AssetCtx) {
        let now = Utc::now();

        // Track oracle price changes
        let new_oracle = ctx.oracle.oracle_px;
        if let Some(last) = &self.last_oracle_px {
            if *last != new_oracle {
                debug!(
                    last = %last,
                    new = %new_oracle,
                    "Oracle price changed"
                );
                self.oracle_changed_at = Some(now);
            }
        } else {
            self.oracle_changed_at = Some(now);
        }
        self.last_oracle_px = Some(new_oracle);

        self.ctx = Some(ctx);
        self.last_update = now;
        self.ctx_recv_mono = Some(Instant::now());
    }

    /// Get BBO age in milliseconds (P0-12: monotonic).
    pub fn bbo_age_ms(&self) -> Option<i64> {
        self.bbo_recv_mono.map(|t| t.elapsed().as_millis() as i64)
    }

    /// Get AssetCtx age in milliseconds (P0-12: monotonic).
    pub fn ctx_age_ms(&self) -> Option<i64> {
        self.ctx_recv_mono.map(|t| t.elapsed().as_millis() as i64)
    }

    /// Get oracle age in milliseconds since last change.
    pub fn oracle_age_ms(&self) -> Option<i64> {
        self.oracle_changed_at
            .map(|t| (Utc::now() - t).num_milliseconds())
    }
}

type StateEntry = Arc<RwLock<MarketStateEntry>>;

/// Aggregated market state manager.
pub struct MarketState {
    /// Per-market state.
    markets: DashMap<MarketKey, StateEntry>,
}

impl MarketState {
    /// Create a new market state manager.
    pub fn new() -> Self {
        Self {
            markets: DashMap::new(),
        }
    }

    /// Get or create market entry.
    fn get_or_create(&self, key: MarketKey) -> StateEntry {
        self.markets
            .entry(key)
            .or_insert_with(|| Arc::new(RwLock::new(MarketStateEntry::new())))
            .clone()
    }

    /// Update BBO for a market.
    ///
    /// # Arguments
    /// - `key`: Market key
    /// - `bbo`: BBO data
    /// - `server_time`: Optional server timestamp from WebSocket (for TimeRegression)
    pub fn update_bbo(&self, key: MarketKey, bbo: Bbo, server_time: Option<i64>) {
        let entry = self.get_or_create(key);
        entry.write().update_bbo(bbo, server_time);
    }

    /// Update asset context for a market.
    pub fn update_ctx(&self, key: MarketKey, ctx: AssetCtx) {
        let entry = self.get_or_create(key);
        entry.write().update_ctx(ctx);
    }

    /// Get market snapshot.
    pub fn get_snapshot(&self, key: &MarketKey) -> Option<MarketSnapshot> {
        self.markets.get(key).and_then(|entry| {
            let guard = entry.read();
            guard.snapshot()
        })
    }

    /// Get BBO for a market.
    pub fn get_bbo(&self, key: &MarketKey) -> Option<Bbo> {
        self.markets.get(key).and_then(|entry| {
            let guard = entry.read();
            guard.bbo.clone()
        })
    }

    /// Get asset context for a market.
    pub fn get_ctx(&self, key: &MarketKey) -> Option<AssetCtx> {
        self.markets.get(key).and_then(|entry| {
            let guard = entry.read();
            guard.ctx.clone()
        })
    }

    /// Get oracle age for a market.
    pub fn get_oracle_age_ms(&self, key: &MarketKey) -> Option<i64> {
        self.markets.get(key).and_then(|entry| {
            let guard = entry.read();
            guard.oracle_age_ms()
        })
    }

    /// Get BBO age for a market (P0-12: monotonic).
    pub fn get_bbo_age_ms(&self, key: &MarketKey) -> Option<i64> {
        self.markets.get(key).and_then(|entry| {
            let guard = entry.read();
            guard.bbo_age_ms()
        })
    }

    /// Get AssetCtx age for a market (P0-12: monotonic).
    pub fn get_ctx_age_ms(&self, key: &MarketKey) -> Option<i64> {
        self.markets.get(key).and_then(|entry| {
            let guard = entry.read();
            guard.ctx_age_ms()
        })
    }

    /// Get BBO server time for a market (P0-16: TimeRegression).
    pub fn get_bbo_server_time(&self, key: &MarketKey) -> Option<i64> {
        self.markets.get(key).and_then(|entry| {
            let guard = entry.read();
            guard.bbo_server_time
        })
    }

    /// Check if market state is complete.
    pub fn is_complete(&self, key: &MarketKey) -> bool {
        self.markets
            .get(key)
            .map(|entry| entry.read().is_complete())
            .unwrap_or(false)
    }

    /// Get all market keys.
    pub fn market_keys(&self) -> Vec<MarketKey> {
        self.markets.iter().map(|entry| *entry.key()).collect()
    }

    /// Get all complete market snapshots.
    pub fn all_snapshots(&self) -> Vec<(MarketKey, MarketSnapshot)> {
        self.markets
            .iter()
            .filter_map(|entry| {
                let key = *entry.key();
                entry.read().snapshot().map(|snap| (key, snap))
            })
            .collect()
    }
}

impl Default for MarketState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetId, DexId, OracleData, Size};
    use rust_decimal_macros::dec;

    fn test_key() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    fn test_bbo() -> Bbo {
        Bbo::new(
            Price::new(dec!(50000)),
            Size::new(dec!(1)),
            Price::new(dec!(50010)),
            Size::new(dec!(1)),
        )
    }

    fn test_ctx() -> AssetCtx {
        let oracle = OracleData::new(Price::new(dec!(50005)), Price::new(dec!(50005)));
        AssetCtx::new(oracle, dec!(0.0001))
    }

    #[test]
    fn test_market_state_incomplete() {
        let state = MarketState::new();
        let key = test_key();

        state.update_bbo(key, test_bbo(), None);
        assert!(!state.is_complete(&key));
        assert!(state.get_snapshot(&key).is_none());
    }

    #[test]
    fn test_market_state_complete() {
        let state = MarketState::new();
        let key = test_key();

        state.update_bbo(key, test_bbo(), None);
        state.update_ctx(key, test_ctx());

        assert!(state.is_complete(&key));
        assert!(state.get_snapshot(&key).is_some());
    }

    #[test]
    fn test_oracle_tracking() {
        let state = MarketState::new();
        let key = test_key();

        state.update_ctx(key, test_ctx());

        let age = state.get_oracle_age_ms(&key);
        assert!(age.is_some());
        assert!(age.unwrap() >= 0);
    }

    #[test]
    fn test_bbo_age_tracking() {
        let state = MarketState::new();
        let key = test_key();

        state.update_bbo(key, test_bbo(), Some(1000));

        // BBO age should be available
        let age = state.get_bbo_age_ms(&key);
        assert!(age.is_some());
        assert!(age.unwrap() >= 0);

        // Server time should be stored
        let server_time = state.get_bbo_server_time(&key);
        assert_eq!(server_time, Some(1000));
    }

    #[test]
    fn test_ctx_age_tracking() {
        let state = MarketState::new();
        let key = test_key();

        state.update_ctx(key, test_ctx());

        // Ctx age should be available
        let age = state.get_ctx_age_ms(&key);
        assert!(age.is_some());
        assert!(age.unwrap() >= 0);
    }
}
