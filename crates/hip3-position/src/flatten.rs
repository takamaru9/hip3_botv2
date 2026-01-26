//! Position flattening (exit) management.
//!
//! Handles the process of closing positions via reduce-only orders,
//! tracking flatten state, and detecting timeouts.

use crate::tracker::Position;
use hip3_core::{ClientOrderId, MarketKey, OrderSide, Size};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Default timeout for reduce-only orders: 60 seconds.
/// If a position is not flattened within this time, it's considered a failure.
pub const REDUCE_ONLY_TIMEOUT_MS: u64 = 60_000;

/// Reason for flattening a position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlattenReason {
    /// Position exceeded TIME_STOP_MS holding time.
    TimeStop {
        /// How long the position was held (ms).
        elapsed_ms: u64,
    },
    /// HardStop triggered (circuit breaker, risk limit, etc.).
    HardStop,
    /// Manual flatten request from operator.
    Manual,
}

impl std::fmt::Display for FlattenReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimeStop { elapsed_ms } => write!(f, "TimeStop({}ms)", elapsed_ms),
            Self::HardStop => write!(f, "HardStop"),
            Self::Manual => write!(f, "Manual"),
        }
    }
}

/// Request to flatten (close) a position.
///
/// Created by the flattening logic, executed by the executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlattenRequest {
    /// Market to flatten.
    pub market: MarketKey,
    /// Direction of the reduce-only order (opposite of position side).
    pub side: OrderSide,
    /// Size to close.
    pub size: Size,
    /// Reason for flattening.
    pub reason: FlattenReason,
    /// Timestamp when the request was created.
    pub requested_at: u64,
}

/// State of the flattening process for a market.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlattenState {
    /// Flatten has not started yet.
    NotStarted,
    /// Reduce-only order has been submitted, waiting for fill.
    InProgress {
        /// Client order ID of the reduce-only order.
        cloid: ClientOrderId,
        /// Timestamp when flatten started.
        started_at: u64,
    },
    /// Flatten completed successfully (position = 0).
    Completed {
        /// Timestamp when flatten completed.
        completed_at: u64,
    },
    /// Flatten failed (timeout or error).
    Failed {
        /// Reason for failure.
        reason: String,
        /// Timestamp when failure was detected.
        failed_at: u64,
    },
}

impl FlattenState {
    /// Check if this state represents an active flatten attempt.
    pub fn is_in_progress(&self) -> bool {
        matches!(self, Self::InProgress { .. })
    }

    /// Check if this state is terminal (completed or failed).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed { .. } | Self::Failed { .. })
    }
}

/// Manages the flattening process for positions.
///
/// Tracks flatten state per market and handles timeouts.
#[derive(Debug)]
pub struct Flattener {
    /// Flatten state per market.
    states: HashMap<MarketKey, FlattenState>,
    /// Timeout threshold for reduce-only orders.
    timeout_ms: u64,
}

impl Flattener {
    /// Create a new Flattener with custom timeout.
    pub fn new(timeout_ms: u64) -> Self {
        Self {
            states: HashMap::new(),
            timeout_ms,
        }
    }

    /// Create a Flattener with default timeout (REDUCE_ONLY_TIMEOUT_MS = 60s).
    pub fn with_default() -> Self {
        Self::new(REDUCE_ONLY_TIMEOUT_MS)
    }

    /// Get the timeout threshold in milliseconds.
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    /// Initiate flattening for a position.
    ///
    /// Creates a FlattenRequest that should be submitted as a reduce-only order.
    /// Does NOT change state to InProgress - call `mark_in_progress` after
    /// the order is actually submitted.
    ///
    /// # Arguments
    /// * `position` - The position to flatten
    /// * `reason` - Why the position is being flattened
    /// * `now_ms` - Current timestamp
    ///
    /// # Returns
    /// * `Some(FlattenRequest)` if flatten can be started
    /// * `None` if flatten is already in progress or position size is zero
    pub fn start_flatten(
        &mut self,
        position: &Position,
        reason: FlattenReason,
        now_ms: u64,
    ) -> Option<FlattenRequest> {
        // Check if already in progress
        if let Some(state) = self.states.get(&position.market) {
            if state.is_in_progress() {
                tracing::debug!(
                    market = %position.market,
                    "Flatten already in progress, ignoring duplicate request"
                );
                return None;
            }
        }

        // Don't flatten zero-size positions
        if position.size.is_zero() {
            tracing::debug!(
                market = %position.market,
                "Position size is zero, skipping flatten"
            );
            return None;
        }

        // Mark as not started (will transition to InProgress when order is submitted)
        self.states
            .insert(position.market, FlattenState::NotStarted);

        Some(FlattenRequest {
            market: position.market,
            side: position.side.opposite(),
            size: position.size,
            reason,
            requested_at: now_ms,
        })
    }

    /// Mark that a reduce-only order has been submitted.
    ///
    /// Call this after the order is successfully submitted to the exchange.
    ///
    /// # Arguments
    /// * `market` - The market being flattened
    /// * `cloid` - Client order ID of the submitted reduce-only order
    /// * `now_ms` - Current timestamp
    pub fn mark_in_progress(&mut self, market: &MarketKey, cloid: ClientOrderId, now_ms: u64) {
        self.states.insert(
            *market,
            FlattenState::InProgress {
                cloid,
                started_at: now_ms,
            },
        );
        tracing::info!(
            market = %market,
            "Flatten marked as in_progress"
        );
    }

    /// Mark that flattening has completed successfully.
    ///
    /// Call this when the position size reaches zero.
    ///
    /// # Arguments
    /// * `market` - The market that was flattened
    /// * `now_ms` - Current timestamp
    pub fn mark_completed(&mut self, market: &MarketKey, now_ms: u64) {
        self.states.insert(
            *market,
            FlattenState::Completed {
                completed_at: now_ms,
            },
        );
        tracing::info!(
            market = %market,
            "Flatten completed"
        );
    }

    /// Check for timed-out flatten attempts and mark them as failed.
    ///
    /// # Arguments
    /// * `now_ms` - Current timestamp
    ///
    /// # Returns
    /// Vector of (MarketKey, error_message) for markets that timed out
    pub fn check_timeouts(&mut self, now_ms: u64) -> Vec<(MarketKey, String)> {
        let mut timed_out = Vec::new();

        for (market, state) in &self.states {
            if let FlattenState::InProgress { started_at, .. } = state {
                let elapsed = now_ms.saturating_sub(*started_at);
                if elapsed >= self.timeout_ms {
                    timed_out.push((*market, format!("Flatten timeout after {}ms", elapsed)));
                }
            }
        }

        // Mark as failed
        for (market, reason) in &timed_out {
            self.states.insert(
                *market,
                FlattenState::Failed {
                    reason: reason.clone(),
                    failed_at: now_ms,
                },
            );
            tracing::error!(
                market = %market,
                reason = %reason,
                "Flatten failed: timeout"
            );
        }

        timed_out
    }

    /// Get the flatten state for a market.
    pub fn get_state(&self, market: &MarketKey) -> Option<&FlattenState> {
        self.states.get(market)
    }

    /// Clear all flatten states.
    ///
    /// Use with caution - typically only at startup or after a full reset.
    pub fn clear(&mut self) {
        self.states.clear();
    }

    /// Get all markets currently in progress.
    pub fn in_progress_markets(&self) -> Vec<MarketKey> {
        self.states
            .iter()
            .filter(|(_, state)| state.is_in_progress())
            .map(|(market, _)| *market)
            .collect()
    }

    /// Get count of markets in each state.
    pub fn state_counts(&self) -> (usize, usize, usize, usize) {
        let mut not_started = 0;
        let mut in_progress = 0;
        let mut completed = 0;
        let mut failed = 0;

        for state in self.states.values() {
            match state {
                FlattenState::NotStarted => not_started += 1,
                FlattenState::InProgress { .. } => in_progress += 1,
                FlattenState::Completed { .. } => completed += 1,
                FlattenState::Failed { .. } => failed += 1,
            }
        }

        (not_started, in_progress, completed, failed)
    }
}

impl Default for Flattener {
    fn default() -> Self {
        Self::with_default()
    }
}

/// Convert all positions to flatten requests (e.g., for HardStop).
///
/// # Arguments
/// * `positions` - All positions to flatten
/// * `reason` - Reason for flattening (typically HardStop)
/// * `now_ms` - Current timestamp
///
/// # Returns
/// Vector of FlattenRequests for all non-zero positions
pub fn flatten_all_positions(
    positions: &[Position],
    reason: FlattenReason,
    now_ms: u64,
) -> Vec<FlattenRequest> {
    positions
        .iter()
        .filter(|p| !p.size.is_zero())
        .map(|p| FlattenRequest {
            market: p.market,
            side: p.side.opposite(),
            size: p.size,
            reason: reason.clone(),
            requested_at: now_ms,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetId, DexId};
    use rust_decimal_macros::dec;

    fn make_position(market: MarketKey, side: OrderSide, size: Size) -> Position {
        use hip3_core::Price;
        Position::new(market, side, size, Price::new(dec!(100)), 0)
    }

    fn market(asset: u32) -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(asset))
    }

    #[test]
    fn test_flatten_reason_display() {
        assert_eq!(
            FlattenReason::TimeStop { elapsed_ms: 35000 }.to_string(),
            "TimeStop(35000ms)"
        );
        assert_eq!(FlattenReason::HardStop.to_string(), "HardStop");
        assert_eq!(FlattenReason::Manual.to_string(), "Manual");
    }

    #[test]
    fn test_start_flatten_creates_request() {
        let mut flattener = Flattener::with_default();
        let position = make_position(market(0), OrderSide::Buy, Size::new(dec!(1)));

        let request = flattener
            .start_flatten(
                &position,
                FlattenReason::TimeStop { elapsed_ms: 35000 },
                100_000,
            )
            .expect("Should create request");

        assert_eq!(request.market, market(0));
        assert_eq!(request.side, OrderSide::Sell); // Opposite of Buy
        assert_eq!(request.size, Size::new(dec!(1)));
        assert_eq!(request.requested_at, 100_000);
    }

    #[test]
    fn test_start_flatten_opposite_side() {
        let mut flattener = Flattener::with_default();

        // Long position -> Sell to close
        let long_pos = make_position(market(0), OrderSide::Buy, Size::new(dec!(1)));
        let request = flattener
            .start_flatten(&long_pos, FlattenReason::Manual, 0)
            .unwrap();
        assert_eq!(request.side, OrderSide::Sell);

        flattener.clear();

        // Short position -> Buy to close
        let short_pos = make_position(market(1), OrderSide::Sell, Size::new(dec!(1)));
        let request = flattener
            .start_flatten(&short_pos, FlattenReason::Manual, 0)
            .unwrap();
        assert_eq!(request.side, OrderSide::Buy);
    }

    #[test]
    fn test_start_flatten_ignores_duplicate() {
        let mut flattener = Flattener::with_default();
        let position = make_position(market(0), OrderSide::Buy, Size::new(dec!(1)));

        // First request succeeds
        let request1 = flattener.start_flatten(&position, FlattenReason::Manual, 100);
        assert!(request1.is_some());

        // Mark as in progress
        flattener.mark_in_progress(&market(0), ClientOrderId::new(), 100);

        // Second request is ignored (already in progress)
        let request2 = flattener.start_flatten(&position, FlattenReason::Manual, 200);
        assert!(request2.is_none());
    }

    #[test]
    fn test_start_flatten_skips_zero_size() {
        let mut flattener = Flattener::with_default();
        let position = make_position(market(0), OrderSide::Buy, Size::ZERO);

        let request = flattener.start_flatten(&position, FlattenReason::Manual, 100);
        assert!(request.is_none());
    }

    #[test]
    fn test_mark_in_progress_state_transition() {
        let mut flattener = Flattener::with_default();
        let market_key = market(0);

        flattener.mark_in_progress(&market_key, ClientOrderId::new(), 100);

        let state = flattener.get_state(&market_key).unwrap();
        assert!(matches!(
            state,
            FlattenState::InProgress {
                started_at: 100,
                ..
            }
        ));
        assert!(state.is_in_progress());
        assert!(!state.is_terminal());
    }

    #[test]
    fn test_mark_completed_state_transition() {
        let mut flattener = Flattener::with_default();
        let market_key = market(0);

        flattener.mark_in_progress(&market_key, ClientOrderId::new(), 100);
        flattener.mark_completed(&market_key, 200);

        let state = flattener.get_state(&market_key).unwrap();
        assert!(matches!(
            state,
            FlattenState::Completed { completed_at: 200 }
        ));
        assert!(!state.is_in_progress());
        assert!(state.is_terminal());
    }

    #[test]
    fn test_check_timeouts_detects_timeout() {
        let mut flattener = Flattener::new(1000); // 1 second timeout

        flattener.mark_in_progress(&market(0), ClientOrderId::new(), 0);
        flattener.mark_in_progress(&market(1), ClientOrderId::new(), 600);

        // At t=1500:
        // - market(0) elapsed = 1500ms >= 1000ms -> timed out
        // - market(1) elapsed = 900ms < 1000ms -> not timed out
        let timed_out = flattener.check_timeouts(1500);

        assert_eq!(timed_out.len(), 1);
        assert_eq!(timed_out[0].0, market(0));

        // market(0) should now be Failed
        let state0 = flattener.get_state(&market(0)).unwrap();
        assert!(matches!(state0, FlattenState::Failed { .. }));

        // market(1) should still be InProgress
        let state1 = flattener.get_state(&market(1)).unwrap();
        assert!(state1.is_in_progress());
    }

    #[test]
    fn test_check_timeouts_exact_boundary() {
        let mut flattener = Flattener::new(1000);

        flattener.mark_in_progress(&market(0), ClientOrderId::new(), 0);

        // At exactly timeout, should be marked as timed out
        let timed_out = flattener.check_timeouts(1000);
        assert_eq!(timed_out.len(), 1);

        // Check that it doesn't timeout before the threshold
        let mut flattener2 = Flattener::new(1000);
        flattener2.mark_in_progress(&market(0), ClientOrderId::new(), 0);
        let timed_out2 = flattener2.check_timeouts(999);
        assert!(timed_out2.is_empty());
    }

    #[test]
    fn test_flatten_all_positions() {
        let positions = vec![
            make_position(market(0), OrderSide::Buy, Size::new(dec!(1))),
            make_position(market(1), OrderSide::Sell, Size::new(dec!(2))),
            make_position(market(2), OrderSide::Buy, Size::ZERO), // Should be filtered
        ];

        let requests = flatten_all_positions(&positions, FlattenReason::HardStop, 100);

        assert_eq!(requests.len(), 2);

        // Check first request
        let req0 = &requests[0];
        assert_eq!(req0.market, market(0));
        assert_eq!(req0.side, OrderSide::Sell);
        assert_eq!(req0.size, Size::new(dec!(1)));
        assert_eq!(req0.reason, FlattenReason::HardStop);

        // Check second request
        let req1 = &requests[1];
        assert_eq!(req1.market, market(1));
        assert_eq!(req1.side, OrderSide::Buy);
        assert_eq!(req1.size, Size::new(dec!(2)));
    }

    #[test]
    fn test_flatten_all_positions_empty() {
        let positions: Vec<Position> = vec![];
        let requests = flatten_all_positions(&positions, FlattenReason::HardStop, 100);
        assert!(requests.is_empty());
    }

    #[test]
    fn test_state_counts() {
        let mut flattener = Flattener::with_default();

        // Add various states
        flattener.states.insert(market(0), FlattenState::NotStarted);
        flattener.states.insert(
            market(1),
            FlattenState::InProgress {
                cloid: ClientOrderId::new(),
                started_at: 0,
            },
        );
        flattener.states.insert(
            market(2),
            FlattenState::InProgress {
                cloid: ClientOrderId::new(),
                started_at: 0,
            },
        );
        flattener
            .states
            .insert(market(3), FlattenState::Completed { completed_at: 100 });
        flattener.states.insert(
            market(4),
            FlattenState::Failed {
                reason: "test".to_string(),
                failed_at: 100,
            },
        );

        let (not_started, in_progress, completed, failed) = flattener.state_counts();
        assert_eq!(not_started, 1);
        assert_eq!(in_progress, 2);
        assert_eq!(completed, 1);
        assert_eq!(failed, 1);
    }

    #[test]
    fn test_in_progress_markets() {
        let mut flattener = Flattener::with_default();

        flattener.mark_in_progress(&market(0), ClientOrderId::new(), 0);
        flattener.mark_in_progress(&market(1), ClientOrderId::new(), 0);
        flattener.mark_completed(&market(2), 100);

        let in_progress = flattener.in_progress_markets();
        assert_eq!(in_progress.len(), 2);
        assert!(in_progress.contains(&market(0)));
        assert!(in_progress.contains(&market(1)));
        assert!(!in_progress.contains(&market(2)));
    }

    #[test]
    fn test_clear() {
        let mut flattener = Flattener::with_default();

        flattener.mark_in_progress(&market(0), ClientOrderId::new(), 0);
        flattener.mark_in_progress(&market(1), ClientOrderId::new(), 0);

        assert_eq!(flattener.in_progress_markets().len(), 2);

        flattener.clear();

        assert!(flattener.in_progress_markets().is_empty());
        assert!(flattener.get_state(&market(0)).is_none());
    }
}
