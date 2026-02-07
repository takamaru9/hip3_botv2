//! Market Health Tracker (Sprint 3: P2-E).
//!
//! Tracks per-market trading outcomes and auto-disables markets with
//! consistently poor performance. Re-enables when health improves.
//!
//! Design:
//! - Rolling window of recent trade outcomes per market
//! - Health score = weighted average of win rate and PnL quality
//! - Score < disable_threshold (10+ samples) → auto-disable
//! - Score >= re_enable_threshold → re-enable

use std::collections::{HashMap, VecDeque};

use hip3_core::MarketKey;
use parking_lot::Mutex;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Configuration for Market Health Tracker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketHealthConfig {
    /// Enable market health tracking.
    #[serde(default)]
    pub enabled: bool,

    /// Number of recent trades to track per market.
    #[serde(default = "default_window_size")]
    pub window_size: usize,

    /// Health score below this triggers auto-disable (0.0-1.0).
    #[serde(default = "default_disable_threshold")]
    pub disable_threshold: Decimal,

    /// Health score above this re-enables market (0.0-1.0).
    #[serde(default = "default_re_enable_threshold")]
    pub re_enable_threshold: Decimal,

    /// Minimum samples before auto-disable can trigger.
    #[serde(default = "default_min_samples")]
    pub min_samples_to_disable: usize,
}

fn default_window_size() -> usize {
    20
}
fn default_disable_threshold() -> Decimal {
    Decimal::new(3, 1) // 0.3
}
fn default_re_enable_threshold() -> Decimal {
    Decimal::new(5, 1) // 0.5
}
fn default_min_samples() -> usize {
    10
}

impl Default for MarketHealthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            window_size: default_window_size(),
            disable_threshold: default_disable_threshold(),
            re_enable_threshold: default_re_enable_threshold(),
            min_samples_to_disable: default_min_samples(),
        }
    }
}

/// A single trade outcome for health tracking.
#[derive(Debug, Clone)]
pub struct TradeOutcome {
    /// True if trade was profitable (PnL > 0).
    pub is_win: bool,
    /// Net PnL in USD (can be negative).
    pub pnl_usd: Decimal,
    /// Edge at entry in bps.
    pub entry_edge_bps: Decimal,
}

/// Per-market health state.
#[derive(Debug)]
struct MarketHealth {
    outcomes: VecDeque<TradeOutcome>,
    disabled: bool,
}

impl MarketHealth {
    fn new() -> Self {
        Self {
            outcomes: VecDeque::new(),
            disabled: false,
        }
    }
}

/// Market Health Tracker.
///
/// Thread-safe tracker that monitors per-market trading performance
/// and auto-disables/re-enables markets based on health score.
pub struct MarketHealthTracker {
    config: MarketHealthConfig,
    markets: Mutex<HashMap<MarketKey, MarketHealth>>,
}

impl MarketHealthTracker {
    /// Create a new tracker with given config.
    pub fn new(config: MarketHealthConfig) -> Self {
        Self {
            config,
            markets: Mutex::new(HashMap::new()),
        }
    }

    /// Record a trade outcome for a market.
    ///
    /// Returns `Some(true)` if market was auto-disabled,
    /// `Some(false)` if market was re-enabled, `None` if no change.
    pub fn record_outcome(&self, market: MarketKey, outcome: TradeOutcome) -> Option<bool> {
        if !self.config.enabled {
            return None;
        }

        let mut markets = self.markets.lock();
        let health = markets.entry(market).or_insert_with(MarketHealth::new);

        // Add outcome, trim to window
        health.outcomes.push_back(outcome);
        while health.outcomes.len() > self.config.window_size {
            health.outcomes.pop_front();
        }

        let score = Self::calculate_score(&health.outcomes);
        let sample_count = health.outcomes.len();

        // Check for auto-disable
        if !health.disabled
            && sample_count >= self.config.min_samples_to_disable
            && score < self.config.disable_threshold
        {
            health.disabled = true;
            tracing::warn!(
                %market,
                %score,
                %sample_count,
                "Market auto-disabled by health tracker"
            );
            return Some(true);
        }

        // Check for re-enable
        if health.disabled && score >= self.config.re_enable_threshold {
            health.disabled = false;
            tracing::info!(
                %market,
                %score,
                %sample_count,
                "Market re-enabled by health tracker"
            );
            return Some(false);
        }

        None
    }

    /// Check if a market is currently disabled by health tracker.
    pub fn is_disabled(&self, market: &MarketKey) -> bool {
        if !self.config.enabled {
            return false;
        }
        self.markets
            .lock()
            .get(market)
            .is_some_and(|h| h.disabled)
    }

    /// Get the current health score for a market (0.0-1.0).
    ///
    /// Returns None if no data or tracker disabled.
    pub fn health_score(&self, market: &MarketKey) -> Option<Decimal> {
        if !self.config.enabled {
            return None;
        }
        self.markets
            .lock()
            .get(market)
            .map(|h| Self::calculate_score(&h.outcomes))
    }

    /// Get summary of all tracked markets.
    pub fn summary(&self) -> Vec<(MarketKey, Decimal, usize, bool)> {
        let markets = self.markets.lock();
        markets
            .iter()
            .map(|(k, h)| {
                let score = Self::calculate_score(&h.outcomes);
                (*k, score, h.outcomes.len(), h.disabled)
            })
            .collect()
    }

    /// Calculate health score from outcomes (0.0-1.0).
    ///
    /// Score = 0.6 * win_rate + 0.4 * pnl_quality
    /// - win_rate: fraction of winning trades
    /// - pnl_quality: 0.0 if avg_pnl <= -1.0, 1.0 if avg_pnl >= 0.0, linear between
    fn calculate_score(outcomes: &VecDeque<TradeOutcome>) -> Decimal {
        if outcomes.is_empty() {
            return Decimal::new(5, 1); // 0.5 default (neutral)
        }

        let total = Decimal::from(outcomes.len() as u64);
        let wins = Decimal::from(outcomes.iter().filter(|o| o.is_win).count() as u64);
        let win_rate = wins / total;

        let total_pnl: Decimal = outcomes.iter().map(|o| o.pnl_usd).sum();
        let avg_pnl = total_pnl / total;

        // pnl_quality: 0.0 at avg_pnl=-1.0, 1.0 at avg_pnl>=0.0, linear between
        let pnl_quality = if avg_pnl >= Decimal::ZERO {
            Decimal::ONE
        } else if avg_pnl <= Decimal::new(-1, 0) {
            Decimal::ZERO
        } else {
            // Linear: (-1.0 -> 0.0) maps to (0.0 -> 1.0)
            Decimal::ONE + avg_pnl
        };

        // Weighted score
        let score = Decimal::new(6, 1) * win_rate + Decimal::new(4, 1) * pnl_quality;

        // Clamp to 0.0-1.0
        score.min(Decimal::ONE).max(Decimal::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn make_config(enabled: bool) -> MarketHealthConfig {
        MarketHealthConfig {
            enabled,
            window_size: 20,
            disable_threshold: dec!(0.3),
            re_enable_threshold: dec!(0.5),
            min_samples_to_disable: 10,
        }
    }

    fn win_outcome(pnl: Decimal) -> TradeOutcome {
        TradeOutcome {
            is_win: true,
            pnl_usd: pnl,
            entry_edge_bps: dec!(20),
        }
    }

    fn loss_outcome(pnl: Decimal) -> TradeOutcome {
        TradeOutcome {
            is_win: false,
            pnl_usd: pnl,
            entry_edge_bps: dec!(15),
        }
    }

    fn test_market() -> MarketKey {
        use hip3_core::{AssetId, DexId};
        MarketKey::new(DexId::new(1), AssetId::new(110026))
    }

    #[test]
    fn test_disabled_by_default() {
        let tracker = MarketHealthTracker::new(MarketHealthConfig::default());
        assert!(!tracker.is_disabled(&test_market()));
        assert!(tracker.health_score(&test_market()).is_none());
    }

    #[test]
    fn test_healthy_market_not_disabled() {
        let tracker = MarketHealthTracker::new(make_config(true));
        let market = test_market();

        // 10 winning trades
        for _ in 0..10 {
            tracker.record_outcome(market, win_outcome(dec!(0.5)));
        }

        assert!(!tracker.is_disabled(&market));
        let score = tracker.health_score(&market).unwrap();
        assert!(score > dec!(0.5), "score={score} should be > 0.5");
    }

    #[test]
    fn test_auto_disable_poor_market() {
        let tracker = MarketHealthTracker::new(make_config(true));
        let market = test_market();

        // 10 losing trades with -$0.5 each
        for i in 0..10 {
            let result = tracker.record_outcome(market, loss_outcome(dec!(-0.5)));
            if i < 9 {
                // Not enough samples yet
                assert!(result.is_none(), "Should not trigger before min_samples");
            }
        }

        assert!(tracker.is_disabled(&market));
        let score = tracker.health_score(&market).unwrap();
        assert!(score < dec!(0.3), "score={score} should be < 0.3");
    }

    #[test]
    fn test_re_enable_after_improvement() {
        let tracker = MarketHealthTracker::new(make_config(true));
        let market = test_market();

        // First: 10 losses to trigger disable
        for _ in 0..10 {
            tracker.record_outcome(market, loss_outcome(dec!(-0.5)));
        }
        assert!(tracker.is_disabled(&market));

        // Then: 15 wins to push score above re_enable threshold
        for _ in 0..15 {
            let result = tracker.record_outcome(market, win_outcome(dec!(0.5)));
            if let Some(false) = result {
                // Re-enabled
                break;
            }
        }

        assert!(!tracker.is_disabled(&market));
    }

    #[test]
    fn test_per_market_isolation() {
        let tracker = MarketHealthTracker::new(make_config(true));
        let market_a = test_market();
        let market_b = MarketKey::new(hip3_core::DexId::new(1), hip3_core::AssetId::new(110001));

        // Market A: all losses
        for _ in 0..10 {
            tracker.record_outcome(market_a, loss_outcome(dec!(-0.5)));
        }
        // Market B: all wins
        for _ in 0..10 {
            tracker.record_outcome(market_b, win_outcome(dec!(0.5)));
        }

        assert!(tracker.is_disabled(&market_a));
        assert!(!tracker.is_disabled(&market_b));
    }

    #[test]
    fn test_window_rolling() {
        let mut config = make_config(true);
        config.window_size = 10;
        config.min_samples_to_disable = 5;
        let tracker = MarketHealthTracker::new(config);
        let market = test_market();

        // 5 losses to trigger disable
        for _ in 0..5 {
            tracker.record_outcome(market, loss_outcome(dec!(-0.5)));
        }
        assert!(tracker.is_disabled(&market));

        // Add 10 wins to roll out the losses
        for _ in 0..10 {
            tracker.record_outcome(market, win_outcome(dec!(0.5)));
        }

        // Should be re-enabled now
        assert!(!tracker.is_disabled(&market));
    }

    #[test]
    fn test_not_disabled_below_min_samples() {
        let tracker = MarketHealthTracker::new(make_config(true));
        let market = test_market();

        // Only 5 losses (below min_samples_to_disable=10)
        for _ in 0..5 {
            tracker.record_outcome(market, loss_outcome(dec!(-0.5)));
        }

        assert!(!tracker.is_disabled(&market));
    }

    #[test]
    fn test_score_calculation() {
        let tracker = MarketHealthTracker::new(make_config(true));
        let market = test_market();

        // 5 wins + 5 losses = 50% WR, avg PnL = 0 → score = 0.6*0.5 + 0.4*1.0 = 0.7
        for _ in 0..5 {
            tracker.record_outcome(market, win_outcome(dec!(0.5)));
        }
        for _ in 0..5 {
            tracker.record_outcome(market, loss_outcome(dec!(-0.5)));
        }

        let score = tracker.health_score(&market).unwrap();
        assert_eq!(score, dec!(0.7));
    }

    #[test]
    fn test_summary() {
        let tracker = MarketHealthTracker::new(make_config(true));
        let market = test_market();

        for _ in 0..5 {
            tracker.record_outcome(market, win_outcome(dec!(0.3)));
        }

        let summary = tracker.summary();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].0, market);
        assert_eq!(summary[0].2, 5); // 5 samples
        assert!(!summary[0].3); // not disabled
    }

    #[test]
    fn test_config_defaults() {
        let config = MarketHealthConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.window_size, 20);
        assert_eq!(config.disable_threshold, dec!(0.3));
        assert_eq!(config.re_enable_threshold, dec!(0.5));
        assert_eq!(config.min_samples_to_disable, 10);
    }
}
