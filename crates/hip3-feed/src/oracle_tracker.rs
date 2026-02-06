//! Oracle movement tracking for consecutive direction detection.
//!
//! Tracks oracle price movements to detect sustained directional trends.
//! Used by both entry (detector) and exit (position) logic.
//!
//! # Trading Philosophy
//!
//! > **正しいエッジ**: オラクルが動いた後、マーケットメーカーの注文が追従していない
//! > 「取り残された流動性」を取る
//!
//! Key insight: A single oracle tick can be noise. Multiple consecutive
//! moves in the same direction indicate a real trend that MMs haven't
//! caught up with yet.
//!
//! # Data Analysis Results (2026-02-03)
//!
//! | Consecutive | Count | Avg Edge | Improvement |
//! |-------------|-------|----------|-------------|
//! | 0 | 21,462 | 29.79 bps | - |
//! | 1 | 1,941 | 41.98 bps | +12.2 bps |
//! | 2 | 1,196 | 43.43 bps | +13.6 bps |
//! | 3 | 760 | 44.49 bps | +14.7 bps |
//! | 4+ | 826 | 45.84 bps | +16.1 bps |
//!
//! Conclusion: 2+ consecutive moves show significant edge improvement.

use dashmap::DashMap;
use hip3_core::{MarketKey, OrderSide, Price};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Direction of oracle price movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MoveDirection {
    /// Oracle price increased.
    Up,
    /// Oracle price decreased.
    Down,
    /// Oracle price unchanged (below min_move_bps threshold).
    Unchanged,
}

impl MoveDirection {
    /// Returns the opposite direction.
    #[must_use]
    pub fn opposite(self) -> Self {
        match self {
            Self::Up => Self::Down,
            Self::Down => Self::Up,
            Self::Unchanged => Self::Unchanged,
        }
    }

    /// Returns true if this direction is favorable for the given position side.
    ///
    /// - Long (Buy): Up is favorable
    /// - Short (Sell): Down is favorable
    #[must_use]
    pub fn is_favorable_for(self, side: OrderSide) -> bool {
        match side {
            OrderSide::Buy => self == Self::Up,
            OrderSide::Sell => self == Self::Down,
        }
    }
}

/// Per-market oracle movement history.
#[derive(Debug, Clone)]
struct OracleHistory {
    /// Last recorded oracle price.
    last_px: Price,
    /// Consecutive moves in the Up direction.
    consecutive_up: u32,
    /// Consecutive moves in the Down direction.
    consecutive_down: u32,
    /// Last recorded oracle change in basis points (absolute value).
    /// Used for velocity-based sizing (P2-1).
    last_change_bps: Decimal,
}

impl OracleHistory {
    fn new(initial_px: Price) -> Self {
        Self {
            last_px: initial_px,
            consecutive_up: 0,
            consecutive_down: 0,
            last_change_bps: Decimal::ZERO,
        }
    }
}

/// Configuration for oracle movement tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleTrackerConfig {
    /// Minimum price change in basis points to count as a "move".
    /// Changes below this are considered noise and treated as Unchanged.
    pub min_move_bps: Decimal,
}

impl Default for OracleTrackerConfig {
    fn default() -> Self {
        Self {
            min_move_bps: Decimal::from(2), // 2 bps = 0.02%
        }
    }
}

/// Tracks oracle price movements and consecutive direction counts.
///
/// Thread-safe via DashMap for concurrent access from multiple tasks.
///
/// # Usage
///
/// ```ignore
/// let tracker = OracleMovementTracker::new(OracleTrackerConfig::default());
///
/// // Record each oracle update
/// let direction = tracker.record_move(market_key, oracle_px);
///
/// // Check consecutive moves
/// let consecutive = tracker.consecutive(&market_key, MoveDirection::Up);
/// ```
pub struct OracleMovementTracker {
    config: OracleTrackerConfig,
    histories: DashMap<MarketKey, OracleHistory>,
}

impl OracleMovementTracker {
    /// Create a new tracker with the given configuration.
    #[must_use]
    pub fn new(config: OracleTrackerConfig) -> Self {
        Self {
            config,
            histories: DashMap::new(),
        }
    }

    /// Create a new tracker wrapped in Arc for sharing.
    #[must_use]
    pub fn new_shared(config: OracleTrackerConfig) -> Arc<Self> {
        Arc::new(Self::new(config))
    }

    /// Record an oracle price update and return the detected direction.
    ///
    /// Updates the consecutive counts based on the direction:
    /// - Up: consecutive_up += 1, consecutive_down = 0
    /// - Down: consecutive_down += 1, consecutive_up = 0
    /// - Unchanged: both counts are preserved (no reset)
    ///
    /// # Design Decision: Unchanged Behavior
    ///
    /// When oracle doesn't move (Unchanged), we preserve the consecutive counts
    /// rather than resetting them. Rationale:
    /// - Oracle not moving = MM has time to catch up
    /// - But it doesn't negate the previous trend
    /// - The "stale liquidity" from previous moves may still exist
    pub fn record_move(&self, key: MarketKey, oracle_px: Price) -> MoveDirection {
        let mut entry = self.histories.entry(key).or_insert_with(|| {
            // First observation: no direction yet
            OracleHistory::new(oracle_px)
        });

        let history = entry.value_mut();

        // Skip if oracle hasn't changed (exact same price)
        if history.last_px == oracle_px {
            return MoveDirection::Unchanged;
        }

        // Calculate change in basis points
        let change_bps = if history.last_px.is_zero() {
            Decimal::ZERO
        } else {
            ((oracle_px.inner() - history.last_px.inner()).abs() / history.last_px.inner())
                * Decimal::from(10000)
        };

        // Determine direction
        let direction = if change_bps < self.config.min_move_bps {
            MoveDirection::Unchanged
        } else if oracle_px.inner() > history.last_px.inner() {
            MoveDirection::Up
        } else {
            MoveDirection::Down
        };

        // Update consecutive counts and velocity
        match direction {
            MoveDirection::Up => {
                history.consecutive_up += 1;
                history.consecutive_down = 0;
                history.last_change_bps = change_bps;
            }
            MoveDirection::Down => {
                history.consecutive_down += 1;
                history.consecutive_up = 0;
                history.last_change_bps = change_bps;
            }
            MoveDirection::Unchanged => {
                // Preserve counts - see design decision above
                // Reset velocity to 0 since no meaningful movement
                history.last_change_bps = Decimal::ZERO;
            }
        }

        // Update last price
        history.last_px = oracle_px;

        direction
    }

    /// Get consecutive count for a specific direction.
    #[must_use]
    pub fn consecutive(&self, key: &MarketKey, direction: MoveDirection) -> u32 {
        self.histories
            .get(key)
            .map(|h| match direction {
                MoveDirection::Up => h.consecutive_up,
                MoveDirection::Down => h.consecutive_down,
                MoveDirection::Unchanged => 0,
            })
            .unwrap_or(0)
    }

    /// Get consecutive count in the direction that's unfavorable for a position.
    ///
    /// Used for exit logic: exit when oracle moves against the position.
    /// - Long (Buy): counts consecutive Down moves
    /// - Short (Sell): counts consecutive Up moves
    #[must_use]
    pub fn consecutive_against(&self, key: &MarketKey, position_side: OrderSide) -> u32 {
        match position_side {
            OrderSide::Buy => self.consecutive(key, MoveDirection::Down),
            OrderSide::Sell => self.consecutive(key, MoveDirection::Up),
        }
    }

    /// Get consecutive count in the direction that's favorable for a position.
    ///
    /// Used for exit logic: exit when oracle moves back in our favor (MM catch-up).
    /// - Long (Buy): counts consecutive Up moves
    /// - Short (Sell): counts consecutive Down moves
    #[must_use]
    pub fn consecutive_with(&self, key: &MarketKey, position_side: OrderSide) -> u32 {
        match position_side {
            OrderSide::Buy => self.consecutive(key, MoveDirection::Up),
            OrderSide::Sell => self.consecutive(key, MoveDirection::Down),
        }
    }

    /// Get the last recorded oracle price for a market.
    #[must_use]
    pub fn last_price(&self, key: &MarketKey) -> Option<Price> {
        self.histories.get(key).map(|h| h.last_px)
    }

    /// Get both consecutive counts for a market.
    #[must_use]
    pub fn consecutive_counts(&self, key: &MarketKey) -> (u32, u32) {
        self.histories
            .get(key)
            .map(|h| (h.consecutive_up, h.consecutive_down))
            .unwrap_or((0, 0))
    }

    /// Get the last oracle change velocity in basis points.
    ///
    /// Returns the absolute change in bps from the most recent oracle move.
    /// Used for velocity-based sizing (P2-1).
    #[must_use]
    pub fn velocity_bps(&self, key: &MarketKey) -> Decimal {
        self.histories
            .get(key)
            .map(|h| h.last_change_bps)
            .unwrap_or(Decimal::ZERO)
    }

    /// Clear tracking data for a market (e.g., on reconnect).
    pub fn clear(&self, key: &MarketKey) {
        self.histories.remove(key);
    }

    /// Clear all tracking data.
    pub fn clear_all(&self) {
        self.histories.clear();
    }

    /// Get number of markets being tracked.
    #[must_use]
    pub fn market_count(&self) -> usize {
        self.histories.len()
    }
}

/// Thread-safe handle to OracleMovementTracker.
pub type OracleTrackerHandle = Arc<OracleMovementTracker>;

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetId, DexId};
    use rust_decimal_macros::dec;

    fn test_key() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    fn test_key_2() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(1))
    }

    fn config() -> OracleTrackerConfig {
        OracleTrackerConfig {
            min_move_bps: dec!(2), // 2 bps minimum
        }
    }

    #[test]
    fn test_first_observation_no_direction() {
        let tracker = OracleMovementTracker::new(config());
        let key = test_key();

        // First observation - no previous price to compare
        let _dir = tracker.record_move(key, Price::new(dec!(100)));

        // Should be Unchanged (no movement yet)
        // But consecutive counts should still be 0
        assert_eq!(tracker.consecutive(&key, MoveDirection::Up), 0);
        assert_eq!(tracker.consecutive(&key, MoveDirection::Down), 0);
    }

    #[test]
    fn test_consecutive_up_moves() {
        let tracker = OracleMovementTracker::new(config());
        let key = test_key();

        // Initial price
        tracker.record_move(key, Price::new(dec!(100)));

        // 3 consecutive Up moves (each > 2 bps)
        let dir1 = tracker.record_move(key, Price::new(dec!(100.10))); // +10 bps
        assert_eq!(dir1, MoveDirection::Up);
        assert_eq!(tracker.consecutive(&key, MoveDirection::Up), 1);

        let dir2 = tracker.record_move(key, Price::new(dec!(100.20))); // +10 bps
        assert_eq!(dir2, MoveDirection::Up);
        assert_eq!(tracker.consecutive(&key, MoveDirection::Up), 2);

        let dir3 = tracker.record_move(key, Price::new(dec!(100.30))); // +10 bps
        assert_eq!(dir3, MoveDirection::Up);
        assert_eq!(tracker.consecutive(&key, MoveDirection::Up), 3);

        // Down count should be 0
        assert_eq!(tracker.consecutive(&key, MoveDirection::Down), 0);
    }

    #[test]
    fn test_direction_change_resets_opposite() {
        let tracker = OracleMovementTracker::new(config());
        let key = test_key();

        // Initial + 2 Up moves
        tracker.record_move(key, Price::new(dec!(100)));
        tracker.record_move(key, Price::new(dec!(100.10)));
        tracker.record_move(key, Price::new(dec!(100.20)));
        assert_eq!(tracker.consecutive(&key, MoveDirection::Up), 2);

        // Down move resets Up count
        let dir = tracker.record_move(key, Price::new(dec!(100.00)));
        assert_eq!(dir, MoveDirection::Down);
        assert_eq!(tracker.consecutive(&key, MoveDirection::Up), 0);
        assert_eq!(tracker.consecutive(&key, MoveDirection::Down), 1);
    }

    #[test]
    fn test_unchanged_preserves_counts() {
        let tracker = OracleMovementTracker::new(config());
        let key = test_key();

        // Initial + 2 Up moves
        tracker.record_move(key, Price::new(dec!(100)));
        tracker.record_move(key, Price::new(dec!(100.10)));
        tracker.record_move(key, Price::new(dec!(100.20)));
        assert_eq!(tracker.consecutive(&key, MoveDirection::Up), 2);

        // Small move (< 2 bps) should be Unchanged and preserve counts
        let dir = tracker.record_move(key, Price::new(dec!(100.201))); // +0.5 bps
        assert_eq!(dir, MoveDirection::Unchanged);
        assert_eq!(tracker.consecutive(&key, MoveDirection::Up), 2); // Preserved!

        // Same price should also be Unchanged
        let dir = tracker.record_move(key, Price::new(dec!(100.201)));
        assert_eq!(dir, MoveDirection::Unchanged);
        assert_eq!(tracker.consecutive(&key, MoveDirection::Up), 2); // Still preserved
    }

    #[test]
    fn test_consecutive_against_long() {
        let tracker = OracleMovementTracker::new(config());
        let key = test_key();

        // Initial + 3 Down moves
        tracker.record_move(key, Price::new(dec!(100)));
        tracker.record_move(key, Price::new(dec!(99.90)));
        tracker.record_move(key, Price::new(dec!(99.80)));
        tracker.record_move(key, Price::new(dec!(99.70)));

        // Long position: Down is against us
        assert_eq!(tracker.consecutive_against(&key, OrderSide::Buy), 3);
        assert_eq!(tracker.consecutive_with(&key, OrderSide::Buy), 0);
    }

    #[test]
    fn test_consecutive_against_short() {
        let tracker = OracleMovementTracker::new(config());
        let key = test_key();

        // Initial + 3 Up moves
        tracker.record_move(key, Price::new(dec!(100)));
        tracker.record_move(key, Price::new(dec!(100.10)));
        tracker.record_move(key, Price::new(dec!(100.20)));
        tracker.record_move(key, Price::new(dec!(100.30)));

        // Short position: Up is against us
        assert_eq!(tracker.consecutive_against(&key, OrderSide::Sell), 3);
        assert_eq!(tracker.consecutive_with(&key, OrderSide::Sell), 0);
    }

    #[test]
    fn test_multiple_markets_independent() {
        let tracker = OracleMovementTracker::new(config());
        let key1 = test_key();
        let key2 = test_key_2();

        // Market 1: Up trend
        tracker.record_move(key1, Price::new(dec!(100)));
        tracker.record_move(key1, Price::new(dec!(100.10)));
        tracker.record_move(key1, Price::new(dec!(100.20)));

        // Market 2: Down trend
        tracker.record_move(key2, Price::new(dec!(200)));
        tracker.record_move(key2, Price::new(dec!(199.80)));
        tracker.record_move(key2, Price::new(dec!(199.60)));

        // Should be independent
        assert_eq!(tracker.consecutive(&key1, MoveDirection::Up), 2);
        assert_eq!(tracker.consecutive(&key1, MoveDirection::Down), 0);
        assert_eq!(tracker.consecutive(&key2, MoveDirection::Up), 0);
        assert_eq!(tracker.consecutive(&key2, MoveDirection::Down), 2);
    }

    #[test]
    fn test_clear_market() {
        let tracker = OracleMovementTracker::new(config());
        let key = test_key();

        tracker.record_move(key, Price::new(dec!(100)));
        tracker.record_move(key, Price::new(dec!(100.10)));
        assert_eq!(tracker.consecutive(&key, MoveDirection::Up), 1);

        tracker.clear(&key);
        assert_eq!(tracker.consecutive(&key, MoveDirection::Up), 0);
        assert!(tracker.last_price(&key).is_none());
    }

    #[test]
    fn test_move_direction_opposite() {
        assert_eq!(MoveDirection::Up.opposite(), MoveDirection::Down);
        assert_eq!(MoveDirection::Down.opposite(), MoveDirection::Up);
        assert_eq!(
            MoveDirection::Unchanged.opposite(),
            MoveDirection::Unchanged
        );
    }

    #[test]
    fn test_move_direction_favorable() {
        // Long: Up is favorable
        assert!(MoveDirection::Up.is_favorable_for(OrderSide::Buy));
        assert!(!MoveDirection::Down.is_favorable_for(OrderSide::Buy));
        assert!(!MoveDirection::Unchanged.is_favorable_for(OrderSide::Buy));

        // Short: Down is favorable
        assert!(MoveDirection::Down.is_favorable_for(OrderSide::Sell));
        assert!(!MoveDirection::Up.is_favorable_for(OrderSide::Sell));
        assert!(!MoveDirection::Unchanged.is_favorable_for(OrderSide::Sell));
    }
}
