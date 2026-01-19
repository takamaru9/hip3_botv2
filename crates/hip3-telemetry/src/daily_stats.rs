//! Daily statistics output for P0-31.
//!
//! Outputs daily summary of key metrics for Phase A DoD:
//! - cross_count: oracle cross detection count
//! - bbo_null_rate: BBO null rate
//! - ctx_age_ms: AssetCtx delay distribution (P50/P95/P99)
//! - bbo_age_ms: BBO delay distribution (P50/P95/P99)
//! - cross_duration_ticks: Cross duration distribution

use crate::metrics::{
    BBO_AGE_HIST_MS, BBO_NULL_TOTAL, BBO_UPDATE_TOTAL, CROSS_COUNT_TOTAL, CROSS_DURATION_TICKS,
    CTX_AGE_HIST_MS,
};
use chrono::{DateTime, Utc};
use prometheus::core::Collector;
use std::collections::HashMap;
use tracing::info;

/// Daily statistics for a market.
#[derive(Debug, Clone)]
pub struct MarketDailyStats {
    pub market_key: String,
    pub cross_count_buy: u64,
    pub cross_count_sell: u64,
    pub bbo_update_total: u64,
    pub bbo_null_total: u64,
    pub bbo_null_rate: f64,
    pub bbo_age_p50_ms: f64,
    pub bbo_age_p95_ms: f64,
    pub bbo_age_p99_ms: f64,
    pub ctx_age_p50_ms: f64,
    pub ctx_age_p95_ms: f64,
    pub ctx_age_p99_ms: f64,
    pub cross_duration_avg_ticks: f64,
}

/// Daily statistics reporter.
pub struct DailyStatsReporter {
    markets: Vec<String>,
    start_time: DateTime<Utc>,
}

impl DailyStatsReporter {
    /// Create a new daily stats reporter.
    pub fn new(markets: Vec<String>) -> Self {
        Self {
            markets,
            start_time: Utc::now(),
        }
    }

    /// Get current statistics for all markets.
    pub fn get_stats(&self) -> Vec<MarketDailyStats> {
        self.markets
            .iter()
            .map(|market_key| self.get_market_stats(market_key))
            .collect()
    }

    /// Get statistics for a single market.
    fn get_market_stats(&self, market_key: &str) -> MarketDailyStats {
        // Get cross counts
        let cross_count_buy = self.get_counter_value(&CROSS_COUNT_TOTAL, &[market_key, "buy"]);
        let cross_count_sell = self.get_counter_value(&CROSS_COUNT_TOTAL, &[market_key, "sell"]);

        // Get BBO null rate
        let bbo_update_total = self.get_counter_value(&BBO_UPDATE_TOTAL, &[market_key]);
        let bbo_null_total = self.get_counter_value(&BBO_NULL_TOTAL, &[market_key]);
        let bbo_null_rate = if bbo_update_total > 0 {
            bbo_null_total as f64 / bbo_update_total as f64
        } else {
            0.0
        };

        // Get age percentiles from histograms
        let (bbo_age_p50_ms, bbo_age_p95_ms, bbo_age_p99_ms) =
            self.get_histogram_percentiles(&BBO_AGE_HIST_MS, &[market_key]);
        let (ctx_age_p50_ms, ctx_age_p95_ms, ctx_age_p99_ms) =
            self.get_histogram_percentiles(&CTX_AGE_HIST_MS, &[market_key]);

        // Get cross duration average
        let cross_duration_avg_ticks = self.get_histogram_mean(&CROSS_DURATION_TICKS, market_key);

        MarketDailyStats {
            market_key: market_key.to_string(),
            cross_count_buy,
            cross_count_sell,
            bbo_update_total,
            bbo_null_total,
            bbo_null_rate,
            bbo_age_p50_ms,
            bbo_age_p95_ms,
            bbo_age_p99_ms,
            ctx_age_p50_ms,
            ctx_age_p95_ms,
            ctx_age_p99_ms,
            cross_duration_avg_ticks,
        }
    }

    /// Get counter value for given labels.
    fn get_counter_value(&self, counter: &prometheus::CounterVec, labels: &[&str]) -> u64 {
        counter.with_label_values(labels).get() as u64
    }

    /// Get percentiles from histogram.
    /// Returns (p50, p95, p99).
    fn get_histogram_percentiles(
        &self,
        histogram: &prometheus::HistogramVec,
        labels: &[&str],
    ) -> (f64, f64, f64) {
        let metric_families = histogram.collect();
        for mf in metric_families {
            for m in mf.get_metric() {
                // Check if labels match
                let label_pairs = m.get_label();
                if label_pairs.len() != labels.len() {
                    continue;
                }
                let mut matches = true;
                for (i, pair) in label_pairs.iter().enumerate() {
                    if pair.get_value() != labels[i] {
                        matches = false;
                        break;
                    }
                }
                if !matches {
                    continue;
                }

                let h = m.get_histogram();
                let count = h.get_sample_count();
                if count == 0 {
                    return (0.0, 0.0, 0.0);
                }

                let buckets = h.get_bucket();
                let p50 = self.percentile_from_buckets(buckets, count, 0.50);
                let p95 = self.percentile_from_buckets(buckets, count, 0.95);
                let p99 = self.percentile_from_buckets(buckets, count, 0.99);

                return (p50, p95, p99);
            }
        }
        (0.0, 0.0, 0.0)
    }

    /// Calculate percentile from histogram buckets.
    fn percentile_from_buckets(
        &self,
        buckets: &[prometheus::proto::Bucket],
        total_count: u64,
        percentile: f64,
    ) -> f64 {
        let target = (total_count as f64 * percentile) as u64;
        let mut prev_bound = 0.0;
        let mut prev_count = 0u64;

        for bucket in buckets {
            let upper_bound = bucket.get_upper_bound();
            let cumulative_count = bucket.get_cumulative_count();

            if cumulative_count >= target {
                // Linear interpolation within bucket
                let bucket_count = cumulative_count - prev_count;
                if bucket_count == 0 {
                    return upper_bound;
                }
                let position = (target - prev_count) as f64 / bucket_count as f64;
                return prev_bound + position * (upper_bound - prev_bound);
            }

            prev_bound = upper_bound;
            prev_count = cumulative_count;
        }

        // Return last bucket bound if target exceeds all buckets
        buckets.last().map(|b| b.get_upper_bound()).unwrap_or(0.0)
    }

    /// Get histogram mean for cross duration (aggregated across buy/sell).
    fn get_histogram_mean(&self, histogram: &prometheus::HistogramVec, market_key: &str) -> f64 {
        let mut total_sum = 0.0;
        let mut total_count = 0u64;

        for side in &["buy", "sell"] {
            let metric_families = histogram.collect();
            for mf in metric_families {
                for m in mf.get_metric() {
                    let label_pairs = m.get_label();
                    if label_pairs.len() != 2 {
                        continue;
                    }
                    if label_pairs[0].get_value() == market_key
                        && label_pairs[1].get_value() == *side
                    {
                        let h = m.get_histogram();
                        total_sum += h.get_sample_sum();
                        total_count += h.get_sample_count();
                    }
                }
            }
        }

        if total_count > 0 {
            total_sum / total_count as f64
        } else {
            0.0
        }
    }

    /// Output daily statistics to logs.
    pub fn output_daily_summary(&self) {
        let stats = self.get_stats();
        let duration = Utc::now() - self.start_time;
        let hours = duration.num_hours();
        let minutes = duration.num_minutes() % 60;

        info!("========== Daily Statistics Summary ==========");
        info!(
            "Period: {} ({} hours {} minutes)",
            self.start_time.format("%Y-%m-%d %H:%M:%S UTC"),
            hours,
            minutes
        );

        for s in &stats {
            info!("--- {} ---", s.market_key);
            info!(
                "  Cross count: {} (buy: {}, sell: {})",
                s.cross_count_buy + s.cross_count_sell,
                s.cross_count_buy,
                s.cross_count_sell
            );
            info!(
                "  BBO null rate: {:.4}% ({}/{})",
                s.bbo_null_rate * 100.0,
                s.bbo_null_total,
                s.bbo_update_total
            );
            info!(
                "  BBO age (ms): P50={:.1}, P95={:.1}, P99={:.1}",
                s.bbo_age_p50_ms, s.bbo_age_p95_ms, s.bbo_age_p99_ms
            );
            info!(
                "  Ctx age (ms): P50={:.1}, P95={:.1}, P99={:.1}",
                s.ctx_age_p50_ms, s.ctx_age_p95_ms, s.ctx_age_p99_ms
            );
            info!(
                "  Cross duration (ticks): avg={:.2}",
                s.cross_duration_avg_ticks
            );
        }

        info!("==============================================");
    }

    /// Get JSON-formatted statistics.
    pub fn to_json(&self) -> HashMap<String, MarketDailyStats> {
        self.get_stats()
            .into_iter()
            .map(|s| (s.market_key.clone(), s))
            .collect()
    }
}
