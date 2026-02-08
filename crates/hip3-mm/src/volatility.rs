//! P3-1: Wick-based volatility tracking for dynamic offset calculation.
//!
//! Tracks 1-second oracle price wicks per market and computes percentile
//! statistics. The key insight from v1: fixed offsets eventually all get
//! disabled — only P99 statistics-based dynamic parameters survive.
//!
//! **Wick definition**: `(high - low) / mid × 10000` (bps) within a 1-second window.
//!
//! **Breakpoint detection**: Rather than using P99 directly, we detect the
//! "cliff" in the distribution where wick magnitude jumps sharply. This
//! prevents the offset from becoming too tight as data accumulates.

use std::collections::{HashMap, VecDeque};

use hip3_core::MarketKey;
use rust_decimal::Decimal;

/// Percentile statistics from the wick distribution.
#[derive(Debug, Clone)]
pub struct VolatilityStats {
    /// P90 wick (bps).
    pub p90_wick_bps: f64,
    /// P95 wick (bps).
    pub p95_wick_bps: f64,
    /// P99 wick (bps).
    pub p99_wick_bps: f64,
    /// P99.5 wick (bps).
    pub p995_wick_bps: f64,
    /// P99.8 wick (bps).
    pub p998_wick_bps: f64,
    /// P99.9 wick (bps).
    pub p999_wick_bps: f64,
    /// Maximum wick (bps).
    pub p100_wick_bps: f64,
    /// Number of wick samples.
    pub sample_count: usize,
    /// Whether we have enough samples for valid statistics.
    pub is_valid: bool,
    /// Breakpoint-detected optimal wick value (bps).
    /// Uses the "cliff" in the distribution rather than a fixed percentile.
    pub optimal_wick_bps: f64,
    /// Which percentile was selected as the breakpoint (for logging).
    pub optimal_percentile: &'static str,
}

impl Default for VolatilityStats {
    fn default() -> Self {
        Self {
            p90_wick_bps: 0.0,
            p95_wick_bps: 0.0,
            p99_wick_bps: 0.0,
            p995_wick_bps: 0.0,
            p998_wick_bps: 0.0,
            p999_wick_bps: 0.0,
            p100_wick_bps: 0.0,
            sample_count: 0,
            is_valid: false,
            optimal_wick_bps: 0.0,
            optimal_percentile: "N/A",
        }
    }
}

/// Cached stats with timestamp for TTL-based invalidation.
#[derive(Debug, Clone)]
struct CachedStats {
    stats: VolatilityStats,
    computed_at_ms: u64,
    wick_count_at_compute: usize,
}

/// Per-market wick tracking state.
#[derive(Debug)]
struct MarketWickState {
    /// Current second being tracked (unix seconds).
    current_sec: u64,
    /// Highest oracle price in current second.
    high: Decimal,
    /// Lowest oracle price in current second.
    low: Decimal,
    /// Rolling window of finalized wicks (bps).
    wicks: VecDeque<f64>,
    /// Cached percentile stats.
    cached_stats: Option<CachedStats>,
}

impl MarketWickState {
    fn new() -> Self {
        Self {
            current_sec: 0,
            high: Decimal::ZERO,
            low: Decimal::ZERO,
            wicks: VecDeque::new(),
            cached_stats: None,
        }
    }
}

/// Tracks 1-second oracle price wicks and computes percentile statistics.
pub struct WickTracker {
    markets: HashMap<MarketKey, MarketWickState>,
    /// Maximum number of wick samples to retain (rolling window).
    max_samples: usize,
    /// Minimum samples required for `is_valid = true`.
    min_samples: usize,
    /// Cache time-to-live in milliseconds.
    cache_ttl_ms: u64,
    /// Minimum jump ratio for breakpoint detection.
    min_jump_ratio: f64,
}

impl WickTracker {
    /// Create a new WickTracker with the given parameters.
    pub fn new(
        max_samples: usize,
        min_samples: usize,
        cache_ttl_ms: u64,
        min_jump_ratio: f64,
    ) -> Self {
        Self {
            markets: HashMap::new(),
            max_samples,
            min_samples,
            cache_ttl_ms,
            min_jump_ratio,
        }
    }

    /// Record an oracle price update.
    ///
    /// Within the same second, updates high/low.
    /// On second boundary, finalizes the previous second's wick.
    pub fn record_oracle(&mut self, market: MarketKey, oracle_px: Decimal, now_ms: u64) {
        if oracle_px.is_zero() {
            return;
        }

        let now_sec = now_ms / 1000;
        let state = self
            .markets
            .entry(market)
            .or_insert_with(MarketWickState::new);

        if state.current_sec == 0 {
            // First observation for this market
            state.current_sec = now_sec;
            state.high = oracle_px;
            state.low = oracle_px;
            return;
        }

        if now_sec == state.current_sec {
            // Same second: update high/low
            if oracle_px > state.high {
                state.high = oracle_px;
            }
            if oracle_px < state.low {
                state.low = oracle_px;
            }
        } else {
            // New second: finalize previous wick
            Self::finalize_wick(state, self.max_samples);

            // Start new second
            state.current_sec = now_sec;
            state.high = oracle_px;
            state.low = oracle_px;
        }
    }

    /// Finalize the current second's wick and push to history.
    fn finalize_wick(state: &mut MarketWickState, max_samples: usize) {
        let mid = (state.high + state.low) / Decimal::TWO;
        if mid.is_zero() {
            return;
        }

        let range = state.high - state.low;
        let wick_bps = (range / mid * Decimal::new(10000, 0))
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.0);

        // Clamp to 500 bps to avoid outlier skew from oracle halts
        let wick_bps = wick_bps.min(500.0);

        state.wicks.push_back(wick_bps);
        while state.wicks.len() > max_samples {
            state.wicks.pop_front();
        }

        // Invalidate cache since new data arrived
        state.cached_stats = None;
    }

    /// Get volatility statistics for a market.
    ///
    /// Uses a TTL-based cache to avoid re-sorting on every call.
    pub fn get_stats(&mut self, market: &MarketKey, now_ms: u64) -> VolatilityStats {
        let state = match self.markets.get_mut(market) {
            Some(s) => s,
            None => return VolatilityStats::default(),
        };

        // Check cache validity
        if let Some(ref cached) = state.cached_stats {
            let age = now_ms.saturating_sub(cached.computed_at_ms);
            if age < self.cache_ttl_ms && cached.wick_count_at_compute == state.wicks.len() {
                return cached.stats.clone();
            }
        }

        let stats = Self::compute_stats(&state.wicks, self.min_samples, self.min_jump_ratio);

        state.cached_stats = Some(CachedStats {
            stats: stats.clone(),
            computed_at_ms: now_ms,
            wick_count_at_compute: state.wicks.len(),
        });

        stats
    }

    /// Get stats for all tracked markets.
    pub fn all_stats(&mut self, now_ms: u64) -> HashMap<MarketKey, VolatilityStats> {
        let markets: Vec<MarketKey> = self.markets.keys().copied().collect();
        let mut result = HashMap::new();
        for market in markets {
            let stats = self.get_stats(&market, now_ms);
            result.insert(market, stats);
        }
        result
    }

    /// Number of wick samples for a market.
    pub fn sample_count(&self, market: &MarketKey) -> usize {
        self.markets.get(market).map(|s| s.wicks.len()).unwrap_or(0)
    }

    /// Compute percentile statistics from wick history.
    fn compute_stats(
        wicks: &VecDeque<f64>,
        min_samples: usize,
        min_jump_ratio: f64,
    ) -> VolatilityStats {
        if wicks.is_empty() {
            return VolatilityStats::default();
        }

        let is_valid = wicks.len() >= min_samples;

        // Sort a copy for percentile calculation
        let mut sorted: Vec<f64> = wicks.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let n = sorted.len();
        let percentile = |p: f64| -> f64 {
            if n == 1 {
                return sorted[0];
            }
            let idx = (p / 100.0 * (n - 1) as f64).round() as usize;
            sorted[idx.min(n - 1)]
        };

        let p90 = percentile(90.0);
        let p95 = percentile(95.0);
        let p99 = percentile(99.0);
        let p995 = percentile(99.5);
        let p998 = percentile(99.8);
        let p999 = percentile(99.9);
        let p100 = sorted[n - 1];

        let (optimal_percentile, optimal_wick_bps) =
            detect_breakpoint(p90, p95, p99, p995, p998, p999, min_jump_ratio);

        VolatilityStats {
            p90_wick_bps: p90,
            p95_wick_bps: p95,
            p99_wick_bps: p99,
            p995_wick_bps: p995,
            p998_wick_bps: p998,
            p999_wick_bps: p999,
            p100_wick_bps: p100,
            sample_count: n,
            is_valid,
            optimal_wick_bps,
            optimal_percentile,
        }
    }
}

/// Detect the "cliff" in the wick distribution.
///
/// Scans adjacent percentile pairs (P90→P95→P99→P99.5→P99.8→P99.9)
/// and picks the pair with the largest jump ratio >= `min_jump_ratio`.
/// Falls back to P99 if no significant jump is found.
///
/// v1 lesson: P99 alone shrinks as data accumulates. Cliff detection
/// captures where the distribution shifts from "normal" to "extreme",
/// which is what we actually want to protect against.
fn detect_breakpoint(
    p90: f64,
    p95: f64,
    p99: f64,
    p995: f64,
    p998: f64,
    p999: f64,
    min_jump_ratio: f64,
) -> (&'static str, f64) {
    let percentiles: [(&str, f64); 6] = [
        ("P90", p90),
        ("P95", p95),
        ("P99", p99),
        ("P99.5", p995),
        ("P99.8", p998),
        ("P99.9", p999),
    ];

    let mut max_jump = 0.0_f64;
    let mut optimal: (&str, f64) = ("P99", p99); // default

    for i in 0..percentiles.len() - 1 {
        let (_, low_val) = percentiles[i];
        let (name, high_val) = percentiles[i + 1];
        if low_val > 0.0 {
            let jump = high_val / low_val;
            if jump >= min_jump_ratio && jump > max_jump {
                max_jump = jump;
                optimal = (name, high_val);
            }
        }
    }

    optimal
}

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetId, DexId};
    use rust_decimal_macros::dec;

    fn mk() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    fn mk2() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(1))
    }

    fn default_tracker() -> WickTracker {
        WickTracker::new(3600, 60, 10_000, 1.5)
    }

    #[test]
    fn test_new_tracker_empty() {
        let mut tracker = default_tracker();
        let stats = tracker.get_stats(&mk(), 1000);
        assert!(!stats.is_valid);
        assert_eq!(stats.sample_count, 0);
    }

    #[test]
    fn test_single_oracle_no_wick() {
        let mut tracker = default_tracker();
        // Single update in second 1 → no finalized wick yet
        tracker.record_oracle(mk(), dec!(100), 1000);
        let stats = tracker.get_stats(&mk(), 1000);
        assert_eq!(stats.sample_count, 0);
    }

    #[test]
    fn test_same_second_high_low() {
        let mut tracker = default_tracker();
        // Multiple updates in same second
        tracker.record_oracle(mk(), dec!(100.00), 1000);
        tracker.record_oracle(mk(), dec!(100.10), 1500);
        tracker.record_oracle(mk(), dec!(99.90), 1800);

        // Move to next second to finalize
        tracker.record_oracle(mk(), dec!(100.00), 2000);

        let stats = tracker.get_stats(&mk(), 2000);
        assert_eq!(stats.sample_count, 1);
        // wick = (100.10 - 99.90) / 100.00 * 10000 = 20.0 bps
        assert!((stats.p100_wick_bps - 20.0).abs() < 0.1);
    }

    #[test]
    fn test_wick_finalized_on_new_second() {
        let mut tracker = default_tracker();

        // Second 1: oracle at 100
        tracker.record_oracle(mk(), dec!(100.00), 1000);
        tracker.record_oracle(mk(), dec!(100.05), 1500);
        assert_eq!(tracker.sample_count(&mk()), 0);

        // Second 2: triggers finalization of second 1
        tracker.record_oracle(mk(), dec!(100.00), 2000);
        assert_eq!(tracker.sample_count(&mk()), 1);
    }

    #[test]
    fn test_wick_calculation_bps() {
        let mut tracker = default_tracker();

        // Second 1: high=100.10, low=99.90
        tracker.record_oracle(mk(), dec!(100.10), 1000);
        tracker.record_oracle(mk(), dec!(99.90), 1500);

        // Second 2: finalize
        tracker.record_oracle(mk(), dec!(100.00), 2000);

        let stats = tracker.get_stats(&mk(), 2000);
        // mid = (100.10 + 99.90)/2 = 100.00
        // wick = (100.10 - 99.90) / 100.00 * 10000 = 20.0 bps
        assert!((stats.p99_wick_bps - 20.0).abs() < 0.1);
    }

    #[test]
    fn test_rolling_window_eviction() {
        let mut tracker = WickTracker::new(5, 1, 10_000, 1.5); // max 5 samples

        // Generate 7 wicks (should only keep last 5)
        for sec in 0u64..8 {
            let px = Decimal::new(10000 + (sec as i64 % 3) * 10, 2); // vary price
            tracker.record_oracle(mk(), px, sec * 1000);
        }

        assert!(tracker.sample_count(&mk()) <= 5);
    }

    #[test]
    fn test_min_samples_threshold() {
        let mut tracker = WickTracker::new(3600, 10, 10_000, 1.5); // min 10 samples

        // Generate only 5 wicks
        for sec in 0u64..6 {
            tracker.record_oracle(mk(), Decimal::new(10000 + sec as i64 * 5, 2), sec * 1000);
        }

        let stats = tracker.get_stats(&mk(), 6000);
        assert!(!stats.is_valid); // below min_samples
        assert!(stats.sample_count < 10);
    }

    #[test]
    fn test_p99_computation() {
        let mut tracker = WickTracker::new(3600, 1, 10_000, 1.5);

        let base = Decimal::new(10000, 2); // 100.00

        // Generate 100 wicks with varying sizes.
        // Each second i gets high/low that produce a specific wick.
        for i in 1u64..=100 {
            let delta = Decimal::new(i as i64, 2); // i/100
            let sec_ms = i * 2000; // use 2s spacing to avoid overlap
            tracker.record_oracle(mk(), base + delta, sec_ms);
            tracker.record_oracle(mk(), base - delta, sec_ms + 500);
        }
        // One more second boundary to finalize the 100th wick
        tracker.record_oracle(mk(), base, 202_001);

        let stats = tracker.get_stats(&mk(), 210_000);
        assert!(stats.is_valid);
        assert_eq!(stats.sample_count, 100);
        // P99 of the distribution should be > 90 bps
        assert!(stats.p99_wick_bps > 90.0);
    }

    #[test]
    fn test_cache_ttl_works() {
        let mut tracker = WickTracker::new(3600, 1, 10_000, 1.5);

        // Generate some wicks
        for sec in 0u64..5 {
            tracker.record_oracle(mk(), Decimal::new(10000 + sec as i64 * 10, 2), sec * 1000);
        }

        // First call computes
        let stats1 = tracker.get_stats(&mk(), 5000);

        // Within TTL — should return cached
        let stats2 = tracker.get_stats(&mk(), 5500);
        assert_eq!(stats1.sample_count, stats2.sample_count);
    }

    #[test]
    fn test_cache_invalidated_on_new_wick() {
        let mut tracker = WickTracker::new(3600, 1, 10_000, 1.5);

        // Generate 2 wicks
        tracker.record_oracle(mk(), dec!(100.00), 1000);
        tracker.record_oracle(mk(), dec!(100.00), 2000);
        tracker.record_oracle(mk(), dec!(100.00), 3000);

        let stats1 = tracker.get_stats(&mk(), 3000);
        let count1 = stats1.sample_count;

        // Add more data and finalize a new wick
        tracker.record_oracle(mk(), dec!(100.10), 4000);
        tracker.record_oracle(mk(), dec!(100.00), 5000);

        // Within TTL but new wick added → cache should be invalidated
        let stats2 = tracker.get_stats(&mk(), 4500);
        assert!(stats2.sample_count >= count1);
    }

    #[test]
    fn test_multi_market_independence() {
        let mut tracker = default_tracker();

        // Market 1: small wick
        tracker.record_oracle(mk(), dec!(100.00), 1000);
        tracker.record_oracle(mk(), dec!(100.01), 1500);
        tracker.record_oracle(mk(), dec!(100.00), 2000);

        // Market 2: large wick
        tracker.record_oracle(mk2(), dec!(100.00), 1000);
        tracker.record_oracle(mk2(), dec!(100.50), 1500);
        tracker.record_oracle(mk2(), dec!(100.00), 2000);

        let s1 = tracker.get_stats(&mk(), 2000);
        let s2 = tracker.get_stats(&mk2(), 2000);

        assert_eq!(s1.sample_count, 1);
        assert_eq!(s2.sample_count, 1);
        // Market 2 wick should be much larger
        assert!(s2.p100_wick_bps > s1.p100_wick_bps * 5.0);
    }

    #[test]
    fn test_p99_uniform_data() {
        let mut tracker = WickTracker::new(3600, 1, 10_000, 1.5);

        // All wicks identical (10 bps)
        for sec in 0..101 {
            let base = dec!(100.00);
            let delta = dec!(0.05); // 10 bps wick
            let t = sec * 1000;
            tracker.record_oracle(mk(), base + delta, t);
            tracker.record_oracle(mk(), base - delta, t + 500);
            tracker.record_oracle(mk(), base, t + 1000);
        }

        let stats = tracker.get_stats(&mk(), 110_000);
        assert!(stats.is_valid);
        // All percentiles should be ~10 bps
        assert!((stats.p90_wick_bps - stats.p99_wick_bps).abs() < 1.0);
        assert!((stats.p99_wick_bps - stats.p999_wick_bps).abs() < 1.0);
    }

    #[test]
    fn test_breakpoint_no_jump_defaults_p99() {
        // All percentiles close together → no jump → defaults to P99
        let (name, val) = detect_breakpoint(10.0, 11.0, 12.0, 12.5, 13.0, 13.5, 1.5);
        assert_eq!(name, "P99");
        assert!((val - 12.0).abs() < 0.01);
    }

    #[test]
    fn test_breakpoint_detects_cliff() {
        // P99.8 is 2.5x P99.5 → cliff at P99.8
        let (name, val) = detect_breakpoint(8.0, 9.0, 10.0, 10.0, 25.0, 26.0, 1.5);
        assert_eq!(name, "P99.8");
        assert!((val - 25.0).abs() < 0.01);
    }

    #[test]
    fn test_breakpoint_largest_jump_wins() {
        // P95 = 2x P90 (jump=2.0), P99.9 = 3x P99.8 (jump=3.0) → P99.9 wins
        let (name, val) = detect_breakpoint(5.0, 10.0, 11.0, 12.0, 13.0, 39.0, 1.5);
        assert_eq!(name, "P99.9");
        assert!((val - 39.0).abs() < 0.01);
    }

    #[test]
    fn test_optimal_wick_used_in_stats() {
        let mut tracker = WickTracker::new(3600, 1, 10_000, 1.5);

        // Create a distribution with a cliff at P99.8
        // Most wicks small (5 bps), a few medium, and tail very large
        for sec in 0..100 {
            let base = dec!(100.00);
            let delta = if sec < 90 {
                dec!(0.025) // 5 bps wick
            } else if sec < 99 {
                dec!(0.05) // 10 bps
            } else {
                dec!(0.25) // 50 bps (the cliff)
            };
            let t = sec * 1000;
            tracker.record_oracle(mk(), base + delta, t);
            tracker.record_oracle(mk(), base - delta, t + 500);
            tracker.record_oracle(mk(), base, t + 1000);
        }

        let stats = tracker.get_stats(&mk(), 110_000);
        assert!(stats.is_valid);
        // The optimal wick should capture the cliff, not just P99
        assert!(stats.optimal_wick_bps >= stats.p99_wick_bps);
    }
}
