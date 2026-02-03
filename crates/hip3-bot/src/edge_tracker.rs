//! Edge distribution tracker for market condition monitoring.
//!
//! Tracks the maximum observed edge per market, even when below threshold.
//! This helps validate threshold settings and understand market conditions.
//!
//! # Trading Philosophy Support
//!
//! > **正しいエッジ**: オラクルが動いた後、マーケットメーカーの注文が追従していない
//! > 「取り残された流動性」を取る
//!
//! By tracking edge distribution, we can understand:
//! - How often edge opportunities occur
//! - Whether threshold is appropriately calibrated
//! - Market activity patterns over time

use hip3_core::MarketKey;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::info;

/// Edge statistics for a market within a time window.
#[derive(Debug, Clone)]
struct EdgeStats {
    /// Maximum buy edge observed (ask below oracle).
    max_buy_edge_bps: Decimal,
    /// Maximum sell edge observed (bid above oracle).
    max_sell_edge_bps: Decimal,
    /// Count of updates in this window.
    update_count: u32,
}

impl EdgeStats {
    fn new() -> Self {
        Self {
            max_buy_edge_bps: Decimal::ZERO,
            max_sell_edge_bps: Decimal::ZERO,
            update_count: 0,
        }
    }

    fn update_buy(&mut self, edge_bps: Decimal) {
        if edge_bps > self.max_buy_edge_bps {
            self.max_buy_edge_bps = edge_bps;
        }
        self.update_count += 1;
    }

    fn update_sell(&mut self, edge_bps: Decimal) {
        if edge_bps > self.max_sell_edge_bps {
            self.max_sell_edge_bps = edge_bps;
        }
        self.update_count += 1;
    }

    fn max_edge(&self) -> Decimal {
        std::cmp::max(self.max_buy_edge_bps, self.max_sell_edge_bps)
    }
}

/// Tracks edge distribution per market for threshold calibration.
pub struct EdgeTracker {
    /// Edge stats per market.
    stats: HashMap<MarketKey, EdgeStats>,
    /// Last log time.
    last_log: Instant,
    /// Log interval.
    log_interval: Duration,
    /// Threshold for comparison logging.
    threshold_bps: Decimal,
}

impl EdgeTracker {
    /// Create a new edge tracker.
    ///
    /// # Arguments
    /// * `log_interval_secs` - How often to log edge statistics
    /// * `threshold_bps` - Threshold for comparison (for logging context)
    pub fn new(log_interval_secs: u64, threshold_bps: Decimal) -> Self {
        Self {
            stats: HashMap::new(),
            last_log: Instant::now(),
            log_interval: Duration::from_secs(log_interval_secs),
            threshold_bps,
        }
    }

    /// Record an edge observation for a market.
    ///
    /// Called on each market check, regardless of whether edge exceeds threshold.
    pub fn record_edge(&mut self, key: MarketKey, buy_edge_bps: Decimal, sell_edge_bps: Decimal) {
        let stats = self.stats.entry(key).or_insert_with(EdgeStats::new);

        if buy_edge_bps > Decimal::ZERO {
            stats.update_buy(buy_edge_bps);
        }
        if sell_edge_bps > Decimal::ZERO {
            stats.update_sell(sell_edge_bps);
        }
    }

    /// Check if it's time to log and do so if needed.
    ///
    /// Call this periodically (e.g., after each market check cycle).
    /// Returns true if logging occurred.
    pub fn maybe_log(&mut self) -> bool {
        if self.last_log.elapsed() < self.log_interval {
            return false;
        }

        self.log_stats();
        self.reset();
        self.last_log = Instant::now();
        true
    }

    /// Log edge statistics for all tracked markets.
    fn log_stats(&self) {
        if self.stats.is_empty() {
            return;
        }

        // Find global max edge
        let global_max = self
            .stats
            .values()
            .map(|s| s.max_edge())
            .max()
            .unwrap_or(Decimal::ZERO);

        // Log per-market stats
        for (key, stats) in &self.stats {
            let max_edge = stats.max_edge();
            let threshold_ratio = if !self.threshold_bps.is_zero() {
                (max_edge / self.threshold_bps * Decimal::from(100)).round()
            } else {
                Decimal::ZERO
            };

            info!(
                market = %key,
                max_buy_edge_bps = %stats.max_buy_edge_bps,
                max_sell_edge_bps = %stats.max_sell_edge_bps,
                max_edge_bps = %max_edge,
                threshold_bps = %self.threshold_bps,
                threshold_pct = %threshold_ratio,
                updates = stats.update_count,
                "EdgeTracker: Market edge summary"
            );
        }

        // Log summary
        info!(
            global_max_edge_bps = %global_max,
            threshold_bps = %self.threshold_bps,
            markets_tracked = self.stats.len(),
            "EdgeTracker: Period summary"
        );
    }

    /// Reset all statistics for a new period.
    fn reset(&mut self) {
        for stats in self.stats.values_mut() {
            *stats = EdgeStats::new();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetId, DexId};
    use rust_decimal_macros::dec;

    #[test]
    fn test_edge_tracking() {
        let mut tracker = EdgeTracker::new(60, dec!(40));
        let key = MarketKey::new(DexId::XYZ, AssetId::new(0));

        // Record some edges
        tracker.record_edge(key, dec!(10), dec!(5));
        tracker.record_edge(key, dec!(15), dec!(8));
        tracker.record_edge(key, dec!(12), dec!(20));

        let stats = tracker.stats.get(&key).unwrap();
        assert_eq!(stats.max_buy_edge_bps, dec!(15));
        assert_eq!(stats.max_sell_edge_bps, dec!(20));
        assert_eq!(stats.max_edge(), dec!(20));
    }
}
