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
//! - Edge percentiles (P50/P75/P90/P99) for adaptive threshold tuning

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
    /// All positive edge observations for percentile calculation.
    edge_samples: Vec<f64>,
}

impl EdgeStats {
    fn new() -> Self {
        Self {
            max_buy_edge_bps: Decimal::ZERO,
            max_sell_edge_bps: Decimal::ZERO,
            update_count: 0,
            edge_samples: Vec::new(),
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

    fn record_sample(&mut self, edge_bps: f64) {
        self.edge_samples.push(edge_bps);
    }

    fn max_edge(&self) -> Decimal {
        std::cmp::max(self.max_buy_edge_bps, self.max_sell_edge_bps)
    }

    /// Calculate percentile from sorted samples.
    /// Returns None if no samples.
    fn percentile(&self, sorted: &[f64], p: f64) -> Option<f64> {
        if sorted.is_empty() {
            return None;
        }
        let idx = (p / 100.0 * (sorted.len() as f64 - 1.0)).round() as usize;
        let idx = idx.min(sorted.len() - 1);
        Some(sorted[idx])
    }

    /// Compute percentiles (P50, P75, P90, P99).
    /// Returns (p50, p75, p90, p99) or None if no samples.
    fn percentiles(&self) -> Option<(f64, f64, f64, f64)> {
        if self.edge_samples.is_empty() {
            return None;
        }
        let mut sorted = self.edge_samples.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        Some((
            self.percentile(&sorted, 50.0).unwrap_or(0.0),
            self.percentile(&sorted, 75.0).unwrap_or(0.0),
            self.percentile(&sorted, 90.0).unwrap_or(0.0),
            self.percentile(&sorted, 99.0).unwrap_or(0.0),
        ))
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
            use rust_decimal::prelude::ToPrimitive;
            if let Some(v) = buy_edge_bps.to_f64() {
                stats.record_sample(v);
            }
        }
        if sell_edge_bps > Decimal::ZERO {
            stats.update_sell(sell_edge_bps);
            use rust_decimal::prelude::ToPrimitive;
            if let Some(v) = sell_edge_bps.to_f64() {
                stats.record_sample(v);
            }
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

        // Log per-market stats with percentiles
        for (key, stats) in &self.stats {
            let max_edge = stats.max_edge();
            let threshold_ratio = if !self.threshold_bps.is_zero() {
                (max_edge / self.threshold_bps * Decimal::from(100)).round()
            } else {
                Decimal::ZERO
            };

            if let Some((p50, p75, p90, p99)) = stats.percentiles() {
                info!(
                    market = %key,
                    max_buy_edge_bps = %stats.max_buy_edge_bps,
                    max_sell_edge_bps = %stats.max_sell_edge_bps,
                    max_edge_bps = %max_edge,
                    p50 = format!("{:.1}", p50),
                    p75 = format!("{:.1}", p75),
                    p90 = format!("{:.1}", p90),
                    p99 = format!("{:.1}", p99),
                    samples = stats.edge_samples.len(),
                    threshold_bps = %self.threshold_bps,
                    threshold_pct = %threshold_ratio,
                    updates = stats.update_count,
                    "EdgeTracker: Market edge summary"
                );
            } else {
                info!(
                    market = %key,
                    max_buy_edge_bps = %stats.max_buy_edge_bps,
                    max_sell_edge_bps = %stats.max_sell_edge_bps,
                    max_edge_bps = %max_edge,
                    threshold_bps = %self.threshold_bps,
                    threshold_pct = %threshold_ratio,
                    updates = stats.update_count,
                    "EdgeTracker: Market edge summary (no samples)"
                );
            }
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

    #[test]
    fn test_percentiles() {
        let mut tracker = EdgeTracker::new(60, dec!(40));
        let key = MarketKey::new(DexId::XYZ, AssetId::new(0));

        // Record 10 edges with known distribution
        for i in 1..=10 {
            let edge = Decimal::from(i * 5); // 5, 10, 15, 20, 25, 30, 35, 40, 45, 50
            tracker.record_edge(key, edge, Decimal::ZERO);
        }

        let stats = tracker.stats.get(&key).unwrap();
        let (p50, p75, p90, p99) = stats.percentiles().unwrap();

        // With 10 samples [5,10,15,20,25,30,35,40,45,50]
        // P50 = index 4.5 -> 30 (rounded)
        assert!(p50 >= 25.0 && p50 <= 30.0, "P50={}", p50);
        assert!(p75 >= 35.0 && p75 <= 40.0, "P75={}", p75);
        assert!(p90 >= 45.0 && p90 <= 50.0, "P90={}", p90);
        assert!((p99 - 50.0).abs() < 1.0, "P99={}", p99);
    }

    #[test]
    fn test_empty_percentiles() {
        let tracker = EdgeTracker::new(60, dec!(40));
        let key = MarketKey::new(DexId::XYZ, AssetId::new(0));

        // No data recorded
        assert!(tracker.stats.get(&key).is_none());
    }
}
