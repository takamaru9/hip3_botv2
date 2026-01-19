//! Cross duration tracker for P0-31.
//!
//! Tracks how long a dislocation (oracle cross) persists in ticks.
//! When a cross ends, emits the duration metric.

use hip3_core::{MarketKey, OrderSide};
use hip3_telemetry::Metrics;
use std::collections::HashMap;

/// State for tracking a single market's cross.
#[derive(Debug, Clone, Default)]
struct CrossState {
    /// Whether cross is currently active.
    is_crossing: bool,
    /// Side of the cross (buy/sell).
    side: Option<OrderSide>,
    /// Number of ticks the cross has persisted.
    tick_count: u64,
}

/// Tracker for cross duration across all markets.
pub struct CrossDurationTracker {
    states: HashMap<MarketKey, CrossState>,
}

impl CrossDurationTracker {
    /// Create a new cross duration tracker.
    pub fn new() -> Self {
        Self {
            states: HashMap::new(),
        }
    }

    /// Update tracker with current tick's cross state.
    ///
    /// Call this on every check, regardless of whether a cross is detected.
    ///
    /// - `key`: The market key
    /// - `is_crossing`: Whether a cross was detected this tick
    /// - `side`: The side of the cross (if any)
    pub fn update(&mut self, key: MarketKey, is_crossing: bool, side: Option<OrderSide>) {
        // First, check if we need to emit a duration metric (before modifying state)
        let emit_data: Option<(String, u64, Option<OrderSide>)> = {
            let state = self.states.get(&key);
            match state {
                Some(s) if !is_crossing && s.is_crossing => {
                    // Cross ending - emit duration
                    Some((key.to_string(), s.tick_count, s.side))
                }
                Some(s) if is_crossing && s.is_crossing && s.side != side => {
                    // Side changed - emit duration of previous side
                    Some((key.to_string(), s.tick_count, s.side))
                }
                _ => None,
            }
        };

        // Emit duration metric if needed (before mutable borrow)
        if let Some((key_str, tick_count, emit_side)) = emit_data {
            Self::emit_duration_static(&key_str, tick_count, emit_side);
        }

        // Now update the state
        let state = self.states.entry(key).or_default();

        if is_crossing {
            if state.is_crossing && state.side == side {
                // Continue existing cross
                state.tick_count += 1;
            } else {
                // Start new cross (either first cross or side changed)
                state.is_crossing = true;
                state.side = side;
                state.tick_count = 1;
            }
        } else {
            // No cross this tick - reset state
            state.is_crossing = false;
            state.side = None;
            state.tick_count = 0;
        }
    }

    /// Emit the cross duration metric (static version to avoid borrow issues).
    fn emit_duration_static(key_str: &str, tick_count: u64, side: Option<OrderSide>) {
        if tick_count > 0 {
            let side_str = match side {
                Some(OrderSide::Buy) => "buy",
                Some(OrderSide::Sell) => "sell",
                None => "unknown",
            };
            Metrics::cross_duration(key_str, side_str, tick_count as f64);
        }
    }

    /// Get current tick count for a market (for testing/debugging).
    pub fn current_tick_count(&self, key: &MarketKey) -> u64 {
        self.states.get(key).map(|s| s.tick_count).unwrap_or(0)
    }
}

impl Default for CrossDurationTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetId, DexId};

    fn test_key() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    #[test]
    fn test_single_tick_cross() {
        let mut tracker = CrossDurationTracker::new();
        let key = test_key();

        // Tick 1: Cross detected
        tracker.update(key, true, Some(OrderSide::Buy));
        assert_eq!(tracker.current_tick_count(&key), 1);

        // Tick 2: No cross (ends cross, emits duration=1)
        tracker.update(key, false, None);
        assert_eq!(tracker.current_tick_count(&key), 0);
    }

    #[test]
    fn test_multi_tick_cross() {
        let mut tracker = CrossDurationTracker::new();
        let key = test_key();

        // Tick 1: Cross detected
        tracker.update(key, true, Some(OrderSide::Buy));
        assert_eq!(tracker.current_tick_count(&key), 1);

        // Tick 2: Cross continues
        tracker.update(key, true, Some(OrderSide::Buy));
        assert_eq!(tracker.current_tick_count(&key), 2);

        // Tick 3: Cross continues
        tracker.update(key, true, Some(OrderSide::Buy));
        assert_eq!(tracker.current_tick_count(&key), 3);

        // Tick 4: No cross (ends cross, emits duration=3)
        tracker.update(key, false, None);
        assert_eq!(tracker.current_tick_count(&key), 0);
    }

    #[test]
    fn test_side_change_resets_count() {
        let mut tracker = CrossDurationTracker::new();
        let key = test_key();

        // Tick 1-2: Buy cross
        tracker.update(key, true, Some(OrderSide::Buy));
        tracker.update(key, true, Some(OrderSide::Buy));
        assert_eq!(tracker.current_tick_count(&key), 2);

        // Tick 3: Side changes to sell (ends buy, starts new sell)
        tracker.update(key, true, Some(OrderSide::Sell));
        assert_eq!(tracker.current_tick_count(&key), 1);
    }
}
