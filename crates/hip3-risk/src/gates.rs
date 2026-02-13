//! Hard Risk Gates implementation.
//!
//! These gates must ALL pass before any trade can be executed.
//! The bot prioritizes stopping over trading when in doubt.
//!
//! # Gate Types
//!
//! ## Market Data Gates
//! - OracleFresh (deprecated): Oracle data not stale
//! - BboUpdate: BBO data not stale (P0-12)
//! - CtxUpdate: AssetCtx data not stale (P0-12)
//! - TimeRegression: No backwards time detected (P0-16)
//! - MarkMidDivergence: Mark-Mid gap within threshold
//! - SpreadShock: Spread not abnormally wide
//!
//! ## Position Gates
//! - OiCap: Open interest below limit
//! - MaxPositionPerMarket: Position per market within limit
//! - MaxPositionTotal: Total portfolio position within limit
//!
//! ## System Gates
//! - ParamChange: No tick/lot/fee changes
//! - Halt: Market not halted
//! - BufferLow: Liquidation buffer adequate

use std::collections::{HashMap, HashSet};

use crate::error::{RiskError, RiskResult};
use chrono::{NaiveTime, Timelike, Utc};
use hip3_core::types::MarketSnapshot;
use hip3_core::{MarketKey, MarketSpec, OrderSide, Price, RejectReason, Size};
use hip3_position::PositionTrackerHandle;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{debug, trace, warn};

/// Time window for trading blackout.
///
/// Trading Philosophy: Market open times have different MM behavior.
/// During these windows, the assumption "MM quotes lag" may not hold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlackoutWindow {
    /// Start time in HH:MM format (UTC).
    pub start: String,
    /// End time in HH:MM format (UTC).
    pub end: String,
}

impl BlackoutWindow {
    /// Parse start time as NaiveTime.
    pub fn start_time(&self) -> Option<NaiveTime> {
        NaiveTime::parse_from_str(&self.start, "%H:%M").ok()
    }

    /// Parse end time as NaiveTime.
    pub fn end_time(&self) -> Option<NaiveTime> {
        NaiveTime::parse_from_str(&self.end, "%H:%M").ok()
    }

    /// Check if a given time is within this blackout window.
    pub fn contains(&self, time: NaiveTime) -> bool {
        let start = match self.start_time() {
            Some(t) => t,
            None => return false,
        };
        let end = match self.end_time() {
            Some(t) => t,
            None => return false,
        };

        if start <= end {
            // Normal case: e.g., 09:00-09:30
            time >= start && time < end
        } else {
            // Wrap around midnight: e.g., 23:00-01:00
            time >= start || time < end
        }
    }
}

/// Risk gate configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskGateConfig {
    /// Maximum oracle age in milliseconds before blocking new orders.
    pub max_oracle_age_ms: i64,
    /// Maximum mark-mid divergence in basis points.
    pub max_mark_mid_divergence_bps: Decimal,
    /// Spread shock multiplier (vs EWMA).
    pub spread_shock_multiplier: Decimal,
    /// Minimum liquidation buffer ratio.
    pub min_buffer_ratio: Decimal,
    /// Maximum position as fraction of OI cap.
    pub max_oi_fraction: Decimal,
    /// Maximum BBO age in milliseconds (P0-12: monotonic freshness).
    #[serde(default = "default_max_bbo_age_ms")]
    pub max_bbo_age_ms: i64,
    /// Maximum AssetCtx age in milliseconds (P0-12: monotonic freshness).
    #[serde(default = "default_max_ctx_age_ms")]
    pub max_ctx_age_ms: i64,
    /// Trading blackout windows (UTC).
    ///
    /// Trading Philosophy: During market open times (e.g., US Pre-market 09:00 UTC),
    /// MM behavior changes and the assumption "MM quotes lag" may not hold.
    /// Block new entries during these high-risk periods.
    #[serde(default = "default_blackout_windows")]
    pub blackout_windows: Vec<BlackoutWindow>,
}

fn default_max_bbo_age_ms() -> i64 {
    2000
}

fn default_max_ctx_age_ms() -> i64 {
    8000
}

fn default_blackout_windows() -> Vec<BlackoutWindow> {
    Vec::new() // Empty by default for backwards compatibility
}

impl Default for RiskGateConfig {
    fn default() -> Self {
        Self {
            max_oracle_age_ms: 8000,
            max_mark_mid_divergence_bps: Decimal::from(50), // 50 bps = 0.5%
            spread_shock_multiplier: Decimal::from(3),
            min_buffer_ratio: Decimal::new(15, 2), // 0.15 = 15%
            max_oi_fraction: Decimal::new(1, 2),   // 0.01 = 1%
            max_bbo_age_ms: 2000,                  // P0-12: 2 seconds
            max_ctx_age_ms: 8000,                  // P0-12: 8 seconds (matches oracle)
            blackout_windows: Vec::new(),          // Empty by default
        }
    }
}

/// Result of a gate check.
#[derive(Debug, Clone)]
pub enum GateResult {
    /// Gate passed.
    Pass,
    /// Gate blocked with reason.
    Block(String),
    /// Gate passed but size should be reduced.
    ReduceSize { factor: Decimal, reason: String },
}

impl GateResult {
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Pass)
    }

    pub fn is_block(&self) -> bool {
        matches!(self, Self::Block(_))
    }
}

/// Hard Risk Gate system.
///
/// CRITICAL: All gates must pass for trading to be allowed.
/// When in doubt, block.
pub struct RiskGate {
    config: RiskGateConfig,
    /// EWMA of spread for shock detection.
    spread_ewma: Decimal,
    /// EWMA decay factor.
    ewma_alpha: Decimal,
    /// Whether param change has been detected.
    param_change_detected: bool,
    /// Whether halt has been detected.
    halt_detected: bool,
    /// Last BBO server time seen (for TimeRegression).
    last_bbo_time: Option<i64>,
    /// Time regression detected.
    time_regression_detected: bool,
}

impl RiskGate {
    /// Create a new risk gate with configuration.
    pub fn new(config: RiskGateConfig) -> Self {
        Self {
            config,
            spread_ewma: Decimal::ZERO,
            ewma_alpha: Decimal::new(5, 2), // 0.05 = slow adaptation
            param_change_detected: false,
            halt_detected: false,
            last_bbo_time: None,
            time_regression_detected: false,
        }
    }

    /// Check all gates for a trading opportunity.
    ///
    /// P0-2: Uses early return to prevent EWMA contamination.
    /// Prerequisite gates are checked first; if they block, gates with
    /// side effects (like spread_shock EWMA update) are NOT executed.
    ///
    /// Returns Ok(()) if all gates pass, or the first blocking error.
    ///
    /// # Gate Evaluation Order (P0-2, BUG-002 fix)
    /// 1. bbo_update - prerequisite (data freshness, P0-12)
    /// 2. ctx_update - prerequisite (data freshness, P0-12, also covers oracle freshness)
    /// 3. time_regression - prerequisite (data integrity, P0-16)
    /// 4. mark_mid_divergence - BBO validity check
    /// 5. spread_shock - EWMA update (side effect, only after prerequisites pass)
    /// 6. oi_cap - position limits
    /// 7. param_change - market change detection
    /// 8. halt - market status
    /// 9. time_of_day - blackout windows for high-risk periods
    ///
    /// NOTE: oracle_fresh gate was removed (BUG-002). ctx_update gate now covers
    /// oracle freshness because it checks when we last received an AssetCtx update,
    /// which includes the oracle price. This fixes the issue where oracle_age was
    /// measuring "price change" instead of "update received".
    ///
    /// # Arguments
    /// - `snapshot`: Current market snapshot
    /// - `spec`: Market specification
    /// - `bbo_age_ms`: BBO age in milliseconds (monotonic, P0-12)
    /// - `ctx_age_ms`: AssetCtx age in milliseconds (monotonic, P0-12)
    /// - `bbo_server_time`: BBO server time (for TimeRegression, P0-16)
    /// - `position_size`: Current position size
    pub fn check_all(
        &mut self,
        snapshot: &MarketSnapshot,
        spec: &MarketSpec,
        bbo_age_ms: i64,
        ctx_age_ms: i64,
        bbo_server_time: Option<i64>,
        position_size: Option<Size>,
    ) -> RiskResult<Vec<GateResult>> {
        let mut results = Vec::with_capacity(9);

        // P0-2: Phase 1 - Prerequisite gates (early return on block)
        // These gates check data freshness and integrity.
        // If any fails, we MUST NOT update EWMA with stale/invalid data.

        // Gate 1: BBO Update (P0-12)
        // BUG-003 fix: Use trace! instead of warn! to reduce log spam.
        // Logging is handled by app.rs with market context and sampling.
        let gate1 = self.check_bbo_update(bbo_age_ms);
        if let GateResult::Block(reason) = &gate1 {
            trace!(gate = "bbo_update", reason, "prerequisite failed");
            return Err(RiskError::GateBlocked {
                gate: "bbo_update".to_string(),
                reason: reason.clone(),
            });
        }
        results.push(gate1);

        // Gate 2: AssetCtx Update (P0-12)
        // BUG-002 fix: This gate now covers oracle freshness.
        // ctx_age_ms measures time since we last received an AssetCtx update,
        // which includes the oracle price. This is the correct metric for freshness.
        let gate2 = self.check_ctx_update(ctx_age_ms);
        if let GateResult::Block(reason) = &gate2 {
            trace!(gate = "ctx_update", reason, "prerequisite failed");
            return Err(RiskError::GateBlocked {
                gate: "ctx_update".to_string(),
                reason: reason.clone(),
            });
        }
        results.push(gate2);

        // Gate 3: Time Regression (P0-16)
        let gate3 = self.check_time_regression(bbo_server_time);
        if let GateResult::Block(reason) = &gate3 {
            trace!(gate = "time_regression", reason, "prerequisite failed");
            return Err(RiskError::GateBlocked {
                gate: "time_regression".to_string(),
                reason: reason.clone(),
            });
        }
        results.push(gate3);

        // P0-2: Phase 2 - BBO validity check (before EWMA update)
        // Gate 4: Mark-Mid Divergence
        let gate4 = self.check_mark_mid_divergence(snapshot);
        if let GateResult::Block(reason) = &gate4 {
            trace!(gate = "mark_mid_divergence", reason, "BBO invalid");
            return Err(RiskError::GateBlocked {
                gate: "mark_mid_divergence".to_string(),
                reason: reason.clone(),
            });
        }
        results.push(gate4);

        // P0-2: Phase 3 - Gates with side effects (EWMA update)
        // Only execute after all prerequisites pass
        // Gate 5: Spread Shock (updates EWMA)
        let gate5 = self.check_spread_shock(snapshot);
        if let GateResult::Block(reason) = &gate5 {
            trace!(gate = "spread_shock", reason, "spread shock detected");
            return Err(RiskError::GateBlocked {
                gate: "spread_shock".to_string(),
                reason: reason.clone(),
            });
        }
        results.push(gate5);

        // P0-2: Phase 4 - Position and market status gates
        // Gate 6: OI Cap
        let gate6 = self.check_oi_cap(snapshot, spec, position_size);
        if let GateResult::Block(reason) = &gate6 {
            trace!(gate = "oi_cap", reason, "OI cap reached");
            return Err(RiskError::GateBlocked {
                gate: "oi_cap".to_string(),
                reason: reason.clone(),
            });
        }
        results.push(gate6);

        // Gate 7: Param Change
        let gate7 = self.check_param_change();
        if let GateResult::Block(reason) = &gate7 {
            trace!(gate = "param_change", reason, "parameter change detected");
            return Err(RiskError::GateBlocked {
                gate: "param_change".to_string(),
                reason: reason.clone(),
            });
        }
        results.push(gate7);

        // Gate 8: Halt
        let gate8 = self.check_halt(spec);
        if let GateResult::Block(reason) = &gate8 {
            trace!(gate = "halt", reason, "market halted");
            return Err(RiskError::GateBlocked {
                gate: "halt".to_string(),
                reason: reason.clone(),
            });
        }
        results.push(gate8);

        // Gate 9: Time of Day (Blackout Windows)
        let gate9 = self.check_time_of_day();
        if let GateResult::Block(reason) = &gate9 {
            trace!(gate = "time_of_day", reason, "trading blackout");
            return Err(RiskError::GateBlocked {
                gate: "time_of_day".to_string(),
                reason: reason.clone(),
            });
        }
        results.push(gate9);

        Ok(results)
    }

    /// Oracle Freshness check (DEPRECATED - BUG-002).
    ///
    /// This gate was removed from `check_all()` because `oracle_age_ms` was
    /// measuring "price change time" instead of "update received time".
    /// The `ctx_update` gate now covers oracle freshness correctly.
    ///
    /// Kept for backwards compatibility but not called from `check_all()`.
    #[allow(dead_code)]
    pub fn check_oracle_fresh(&self, oracle_age_ms: i64) -> GateResult {
        if oracle_age_ms > self.config.max_oracle_age_ms {
            return GateResult::Block(format!(
                "Oracle stale: {}ms > {}ms max",
                oracle_age_ms, self.config.max_oracle_age_ms
            ));
        }

        // Warn if approaching limit
        if oracle_age_ms > self.config.max_oracle_age_ms * 3 / 4 {
            debug!(oracle_age_ms, "Oracle approaching stale threshold");
        }

        GateResult::Pass
    }

    /// Gate 2: Mark-Mid Divergence
    ///
    /// Block if mark price diverges too much from mid price.
    /// P0-14: mid_price() now returns Option<Price>.
    ///
    /// Set `max_mark_mid_divergence_bps = 0` to disable this gate.
    /// Trading Philosophy: Mark-Mid divergence = MM not following oracle = edge.
    pub fn check_mark_mid_divergence(&self, snapshot: &MarketSnapshot) -> GateResult {
        // max_mark_mid_divergence_bps = 0 means gate is disabled
        // Rationale: Mark-Mid divergence IS the edge we want to capture
        if self.config.max_mark_mid_divergence_bps.is_zero() {
            return GateResult::Pass;
        }

        // P0-14: Handle null BBO case
        let mid = match snapshot.bbo.mid_price() {
            Some(m) if !m.is_zero() => m,
            Some(_) => return GateResult::Block("Mid price is zero".to_string()),
            None => return GateResult::Block("BBO is null (P0-14)".to_string()),
        };
        let mark = snapshot.ctx.oracle.mark_px;

        let divergence_bps =
            ((mark.inner() - mid.inner()).abs() / mid.inner()) * Decimal::from(10000);

        if divergence_bps > self.config.max_mark_mid_divergence_bps {
            return GateResult::Block(format!(
                "Mark-mid divergence: {:.1} bps > {:.1} bps max",
                divergence_bps, self.config.max_mark_mid_divergence_bps
            ));
        }

        GateResult::Pass
    }

    /// Gate 3: Spread Shock
    ///
    /// Reduce size or block if spread is abnormally wide.
    pub fn check_spread_shock(&mut self, snapshot: &MarketSnapshot) -> GateResult {
        let spread_bps = snapshot.bbo.spread_bps().unwrap_or(Decimal::MAX);

        // Update EWMA
        if self.spread_ewma.is_zero() {
            self.spread_ewma = spread_bps;
        } else {
            self.spread_ewma =
                self.ewma_alpha * spread_bps + (Decimal::ONE - self.ewma_alpha) * self.spread_ewma;
        }

        // Check for shock
        let threshold = self.spread_ewma * self.config.spread_shock_multiplier;

        if spread_bps > threshold * Decimal::from(2) {
            return GateResult::Block(format!(
                "Spread shock: {:.1} bps > {:.1} bps (2x threshold)",
                spread_bps, threshold
            ));
        }

        if spread_bps > threshold {
            return GateResult::ReduceSize {
                factor: Decimal::new(2, 1), // 0.2 = 1/5
                reason: format!(
                    "Spread elevated: {:.1} bps > {:.1} bps threshold",
                    spread_bps, threshold
                ),
            };
        }

        GateResult::Pass
    }

    /// Gate 4: OI Cap
    ///
    /// Block if open interest at cap or position would exceed limit.
    pub fn check_oi_cap(
        &self,
        snapshot: &MarketSnapshot,
        spec: &MarketSpec,
        _position_size: Option<Size>,
    ) -> GateResult {
        if let Some(cap) = &spec.oi_cap {
            let current_oi = snapshot.ctx.open_interest;
            let _max_allowed = Size::new(cap.inner() * self.config.max_oi_fraction);

            if current_oi >= *cap {
                return GateResult::Block(format!("OI cap reached: {} >= {} cap", current_oi, cap));
            }

            // Additional check: warn if approaching cap
            let oi_ratio = current_oi.inner() / cap.inner();
            if oi_ratio > Decimal::new(95, 2) {
                // 95%
                debug!(oi_ratio = %oi_ratio, "OI approaching cap");
            }
        }

        GateResult::Pass
    }

    /// Gate 5: Param Change
    ///
    /// Block permanently if parameter change was detected.
    pub fn check_param_change(&self) -> GateResult {
        if self.param_change_detected {
            return GateResult::Block(
                "Parameter change detected - requires manual restart".to_string(),
            );
        }
        GateResult::Pass
    }

    /// Gate 6: Halt
    ///
    /// Block if market is halted or spec shows inactive.
    pub fn check_halt(&self, spec: &MarketSpec) -> GateResult {
        if self.halt_detected {
            return GateResult::Block("Market halt detected".to_string());
        }

        if !spec.is_active {
            return GateResult::Block("Market not active".to_string());
        }

        GateResult::Pass
    }

    /// Signal that a parameter change was detected.
    pub fn signal_param_change(&mut self) {
        self.param_change_detected = true;
        warn!("PARAM CHANGE SIGNALED - trading blocked");
    }

    /// Signal that a halt was detected.
    pub fn signal_halt(&mut self) {
        self.halt_detected = true;
        warn!("HALT SIGNALED - trading blocked");
    }

    /// Gate 7: No BBO Update (P0-12)
    ///
    /// Block new orders if BBO update is stale (monotonic age).
    /// This is different from OracleFresh which checks oracle change time.
    ///
    /// Set `max_bbo_age_ms = 0` to disable this gate.
    /// Trading Philosophy: BBO not updating = MM lazy = stale liquidity = edge.
    pub fn check_bbo_update(&self, bbo_age_ms: i64) -> GateResult {
        // max_bbo_age_ms = 0 means gate is disabled
        // Rationale: Stale BBO is the edge we want to capture (lazy MM quotes)
        if self.config.max_bbo_age_ms == 0 {
            return GateResult::Pass;
        }

        if bbo_age_ms > self.config.max_bbo_age_ms {
            return GateResult::Block(format!(
                "BBO stale: {}ms > {}ms max (P0-12)",
                bbo_age_ms, self.config.max_bbo_age_ms
            ));
        }

        // Warn if approaching limit
        if bbo_age_ms > self.config.max_bbo_age_ms * 3 / 4 {
            debug!(bbo_age_ms, "BBO approaching stale threshold");
        }

        GateResult::Pass
    }

    /// Gate 8: No AssetCtx Update (P0-12)
    ///
    /// Block new orders if AssetCtx update is stale (monotonic age).
    pub fn check_ctx_update(&self, ctx_age_ms: i64) -> GateResult {
        if ctx_age_ms > self.config.max_ctx_age_ms {
            return GateResult::Block(format!(
                "AssetCtx stale: {}ms > {}ms max (P0-12)",
                ctx_age_ms, self.config.max_ctx_age_ms
            ));
        }

        // Warn if approaching limit
        if ctx_age_ms > self.config.max_ctx_age_ms * 3 / 4 {
            debug!(ctx_age_ms, "AssetCtx approaching stale threshold");
        }

        GateResult::Pass
    }

    /// Gate 9: Time Regression (P0-16)
    ///
    /// Block if BBO server time goes backwards (data integrity issue).
    /// Only applies to channels with time field (bbo has time, assetCtx does not).
    pub fn check_time_regression(&mut self, bbo_server_time: Option<i64>) -> GateResult {
        // If already detected, keep blocking
        if self.time_regression_detected {
            return GateResult::Block("Time regression detected - requires reconnect".to_string());
        }

        // Only check if we have both times
        if let (Some(last), Some(current)) = (self.last_bbo_time, bbo_server_time) {
            if current < last {
                self.time_regression_detected = true;
                warn!(
                    last_time = last,
                    current_time = current,
                    "TIME REGRESSION DETECTED - blocking"
                );
                return GateResult::Block(format!(
                    "Time regression: {} < {} (P0-16)",
                    current, last
                ));
            }
        }

        // Update last seen time
        if let Some(t) = bbo_server_time {
            self.last_bbo_time = Some(t);
        }

        GateResult::Pass
    }

    /// Signal time regression (called externally when detected).
    pub fn signal_time_regression(&mut self) {
        self.time_regression_detected = true;
        warn!("TIME REGRESSION SIGNALED - trading blocked");
    }

    /// Reset time regression flag (after reconnect).
    pub fn reset_time_regression(&mut self) {
        self.time_regression_detected = false;
        self.last_bbo_time = None;
        debug!("Time regression flag reset");
    }

    /// Gate 9: Time of Day (Blackout Windows)
    ///
    /// Block new entries during high-risk time windows (e.g., market open).
    ///
    /// Trading Philosophy: During market open times, MM behavior changes:
    /// - High volatility, rapid quote updates
    /// - The assumption "MM quotes lag" may not hold
    /// - Better to avoid these periods for new entries
    ///
    /// Empty blackout_windows = gate disabled (backwards compatible).
    pub fn check_time_of_day(&self) -> GateResult {
        if self.config.blackout_windows.is_empty() {
            return GateResult::Pass;
        }

        let now_utc = Utc::now();
        let current_time =
            NaiveTime::from_hms_opt(now_utc.hour(), now_utc.minute(), now_utc.second())
                .unwrap_or_else(|| NaiveTime::from_hms_opt(0, 0, 0).unwrap());

        for window in &self.config.blackout_windows {
            if window.contains(current_time) {
                return GateResult::Block(format!(
                    "Trading blackout: {} UTC is within {}-{} window",
                    current_time.format("%H:%M"),
                    window.start,
                    window.end
                ));
            }
        }

        GateResult::Pass
    }

    /// Check if any critical flag is set.
    pub fn has_critical_block(&self) -> bool {
        self.param_change_detected || self.halt_detected || self.time_regression_detected
    }

    /// Get current spread EWMA.
    pub fn spread_ewma(&self) -> Decimal {
        self.spread_ewma
    }

    /// Get current config.
    pub fn config(&self) -> &RiskGateConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveTime;
    use hip3_core::{AssetCtx, Bbo, OracleData, Price, Size};
    use rust_decimal_macros::dec;

    fn test_snapshot() -> MarketSnapshot {
        let bbo = Bbo::new(
            Price::new(dec!(50000)),
            Size::new(dec!(1)),
            Price::new(dec!(50010)),
            Size::new(dec!(1)),
        );
        let oracle = OracleData::new(Price::new(dec!(50005)), Price::new(dec!(50005)));
        let ctx = AssetCtx::new(oracle, dec!(0.0001));
        MarketSnapshot::new(bbo, ctx)
    }

    #[test]
    fn test_oracle_fresh_pass() {
        let gate = RiskGate::new(RiskGateConfig::default());
        let result = gate.check_oracle_fresh(5000);
        assert!(result.is_pass());
    }

    #[test]
    fn test_oracle_fresh_block() {
        let gate = RiskGate::new(RiskGateConfig::default());
        let result = gate.check_oracle_fresh(10000);
        assert!(result.is_block());
    }

    #[test]
    fn test_mark_mid_divergence_pass() {
        let gate = RiskGate::new(RiskGateConfig::default());
        let snapshot = test_snapshot();
        let result = gate.check_mark_mid_divergence(&snapshot);
        assert!(result.is_pass());
    }

    #[test]
    fn test_param_change_blocks() {
        let mut gate = RiskGate::new(RiskGateConfig::default());
        assert!(gate.check_param_change().is_pass());

        gate.signal_param_change();
        assert!(gate.check_param_change().is_block());
        assert!(gate.has_critical_block());
    }

    #[test]
    fn test_bbo_update_pass() {
        let gate = RiskGate::new(RiskGateConfig::default());
        // Default max_bbo_age_ms is 2000
        let result = gate.check_bbo_update(1000);
        assert!(result.is_pass());
    }

    #[test]
    fn test_bbo_update_block() {
        let gate = RiskGate::new(RiskGateConfig::default());
        // Default max_bbo_age_ms is 2000
        let result = gate.check_bbo_update(3000);
        assert!(result.is_block());
    }

    #[test]
    fn test_ctx_update_pass() {
        let gate = RiskGate::new(RiskGateConfig::default());
        // Default max_ctx_age_ms is 8000
        let result = gate.check_ctx_update(5000);
        assert!(result.is_pass());
    }

    #[test]
    fn test_ctx_update_block() {
        let gate = RiskGate::new(RiskGateConfig::default());
        // Default max_ctx_age_ms is 8000
        let result = gate.check_ctx_update(10000);
        assert!(result.is_block());
    }

    #[test]
    fn test_time_regression_pass() {
        let mut gate = RiskGate::new(RiskGateConfig::default());
        // First update
        let result1 = gate.check_time_regression(Some(1000));
        assert!(result1.is_pass());
        // Second update (forward in time)
        let result2 = gate.check_time_regression(Some(2000));
        assert!(result2.is_pass());
    }

    #[test]
    fn test_time_regression_block() {
        let mut gate = RiskGate::new(RiskGateConfig::default());
        // First update
        let result1 = gate.check_time_regression(Some(2000));
        assert!(result1.is_pass());
        // Second update (backward in time - regression!)
        let result2 = gate.check_time_regression(Some(1000));
        assert!(result2.is_block());
        assert!(gate.has_critical_block());
    }

    #[test]
    fn test_time_regression_reset() {
        let mut gate = RiskGate::new(RiskGateConfig::default());
        // Trigger regression
        gate.check_time_regression(Some(2000));
        gate.check_time_regression(Some(1000));
        assert!(gate.has_critical_block());

        // Reset
        gate.reset_time_regression();
        assert!(!gate.has_critical_block());

        // Should pass again
        let result = gate.check_time_regression(Some(1000));
        assert!(result.is_pass());
    }

    // === Time of Day (Blackout Window) tests ===

    #[test]
    fn test_time_of_day_gate_disabled_by_default() {
        let gate = RiskGate::new(RiskGateConfig::default());
        let result = gate.check_time_of_day();
        assert!(result.is_pass(), "Should pass when no blackout windows");
    }

    #[test]
    fn test_blackout_window_contains() {
        // Normal window: 09:00-09:30
        let window = BlackoutWindow {
            start: "09:00".to_string(),
            end: "09:30".to_string(),
        };

        assert!(window.contains(NaiveTime::from_hms_opt(9, 0, 0).unwrap()));
        assert!(window.contains(NaiveTime::from_hms_opt(9, 15, 0).unwrap()));
        assert!(!window.contains(NaiveTime::from_hms_opt(9, 30, 0).unwrap())); // End is exclusive
        assert!(!window.contains(NaiveTime::from_hms_opt(8, 59, 0).unwrap()));
    }

    #[test]
    fn test_blackout_window_midnight_wrap() {
        // Wrap around midnight: 23:00-01:00
        let window = BlackoutWindow {
            start: "23:00".to_string(),
            end: "01:00".to_string(),
        };

        assert!(window.contains(NaiveTime::from_hms_opt(23, 30, 0).unwrap()));
        assert!(window.contains(NaiveTime::from_hms_opt(0, 30, 0).unwrap()));
        assert!(!window.contains(NaiveTime::from_hms_opt(1, 0, 0).unwrap())); // End is exclusive
        assert!(!window.contains(NaiveTime::from_hms_opt(22, 0, 0).unwrap()));
    }

    #[test]
    fn test_time_of_day_gate_blocks_in_window() {
        use chrono::Utc;

        let now = Utc::now();
        let current_hour = now.hour();
        let current_minute = now.minute();

        // Create a window that includes the current time
        let start = format!("{:02}:{:02}", current_hour, current_minute);
        let end_minute = (current_minute + 30) % 60;
        let end_hour = if current_minute + 30 >= 60 {
            (current_hour + 1) % 24
        } else {
            current_hour
        };
        let end = format!("{:02}:{:02}", end_hour, end_minute);

        let config = RiskGateConfig {
            blackout_windows: vec![BlackoutWindow { start, end }],
            ..Default::default()
        };
        let gate = RiskGate::new(config);

        let result = gate.check_time_of_day();
        assert!(result.is_block(), "Should block during blackout window");
    }

    #[test]
    fn test_time_of_day_gate_passes_outside_window() {
        use chrono::Utc;

        let now = Utc::now();
        let current_hour = now.hour();

        // Create a window that does NOT include the current time (2 hours ago)
        let start_hour = (current_hour + 22) % 24; // 2 hours ago
        let end_hour = (current_hour + 23) % 24; // 1 hour ago
        let start = format!("{:02}:00", start_hour);
        let end = format!("{:02}:00", end_hour);

        let config = RiskGateConfig {
            blackout_windows: vec![BlackoutWindow { start, end }],
            ..Default::default()
        };
        let gate = RiskGate::new(config);

        let result = gate.check_time_of_day();
        assert!(result.is_pass(), "Should pass outside blackout window");
    }

    // === P0-2: Early Return tests ===

    /// P0-2: Verify EWMA is not updated when ctx is stale.
    /// BUG-002: oracle_fresh gate was removed; ctx_update now covers oracle freshness.
    #[test]
    fn test_ewma_not_updated_when_ctx_stale() {
        let config = RiskGateConfig::default();
        let mut gate = RiskGate::new(config);

        // First, establish a baseline EWMA with valid data
        let snapshot = test_snapshot();
        let spec = MarketSpec::default();

        // Valid call to initialize EWMA
        let _ = gate.check_all(&snapshot, &spec, 500, 500, Some(1000), None);
        let ewma_after_valid = gate.spread_ewma();
        assert!(
            ewma_after_valid > Decimal::ZERO,
            "EWMA should be initialized"
        );

        // Now try with stale ctx - should fail early
        let result = gate.check_all(&snapshot, &spec, 500, 10000, Some(2000), None);
        assert!(result.is_err(), "Should fail due to stale ctx");

        // EWMA should not have changed
        assert_eq!(
            gate.spread_ewma(),
            ewma_after_valid,
            "P0-2: EWMA should not change when ctx stale"
        );
    }

    /// P0-2: Verify EWMA is not updated when BBO is stale.
    #[test]
    fn test_ewma_not_updated_when_bbo_stale() {
        let config = RiskGateConfig::default();
        let mut gate = RiskGate::new(config);
        let snapshot = test_snapshot();
        let spec = MarketSpec::default();

        // Valid call to initialize EWMA
        let _ = gate.check_all(&snapshot, &spec, 500, 500, Some(1000), None);
        let ewma_after_valid = gate.spread_ewma();

        // Now try with stale BBO - should fail early
        let result = gate.check_all(&snapshot, &spec, 5000, 500, Some(2000), None);
        assert!(result.is_err(), "Should fail due to stale BBO");

        // EWMA should not have changed
        assert_eq!(
            gate.spread_ewma(),
            ewma_after_valid,
            "P0-2: EWMA should not change when BBO stale"
        );
    }

    /// P0-2: Verify gate ordering - early failures prevent later gates from running.
    /// BUG-002: oracle_fresh gate was removed; first gate is now bbo_update.
    #[test]
    fn test_early_return_gate_order() {
        let config = RiskGateConfig::default();
        let mut gate = RiskGate::new(config);
        let snapshot = test_snapshot();
        let spec = MarketSpec::default();

        // Stale BBO should cause early return (first gate)
        let result = gate.check_all(&snapshot, &spec, 5000, 500, None, None);

        // Verify error is from bbo_update gate (first in order after BUG-002 fix)
        match result {
            Err(RiskError::GateBlocked {
                gate: gate_name, ..
            }) => {
                assert_eq!(
                    gate_name, "bbo_update",
                    "First blocking gate should be bbo_update"
                );
            }
            _ => panic!("Expected GateBlocked error"),
        }

        // Stale ctx should cause early return (BBO OK, ctx stale)
        let result2 = gate.check_all(&snapshot, &spec, 500, 10000, None, None);
        match result2 {
            Err(RiskError::GateBlocked {
                gate: gate_name, ..
            }) => {
                assert_eq!(gate_name, "ctx_update", "Should fail at ctx_update gate");
            }
            _ => panic!("Expected GateBlocked error"),
        }
    }

    /// P0-2: Verify all gates pass with valid data.
    /// BUG-002: oracle_fresh gate was removed; now 9 gates total (incl. time_of_day).
    #[test]
    fn test_all_gates_pass_with_valid_data() {
        let config = RiskGateConfig::default();
        let mut gate = RiskGate::new(config);
        let snapshot = test_snapshot();
        let spec = MarketSpec::default();

        let result = gate.check_all(&snapshot, &spec, 500, 500, Some(1000), None);
        assert!(result.is_ok(), "All gates should pass with valid data");

        let results = result.unwrap();
        assert_eq!(
            results.len(),
            9,
            "Should have 9 gate results (BUG-002: oracle_fresh removed, time_of_day added)"
        );

        for r in &results {
            assert!(!r.is_block(), "No gate should block with valid data");
        }
    }
}

// ============================================================================
// MaxPositionPerMarket Gate
// ============================================================================

/// MaxPositionPerMarket Gate: Limits position size per market.
///
/// Checks that current position + pending order notional does not exceed
/// the maximum notional limit for a single market.
pub struct MaxPositionPerMarketGate {
    /// Maximum notional value in USD per market.
    max_notional_usd: Price,
    /// Position tracker handle for checking current positions.
    position_handle: PositionTrackerHandle,
}

impl MaxPositionPerMarketGate {
    /// Create a new MaxPositionPerMarket gate.
    ///
    /// # Arguments
    /// - `max_notional_usd`: Maximum notional value in USD per market
    /// - `position_handle`: Handle to the position tracker
    #[must_use]
    pub fn new(max_notional_usd: Price, position_handle: PositionTrackerHandle) -> Self {
        Self {
            max_notional_usd,
            position_handle,
        }
    }

    /// Check if the order would exceed the maximum position limit.
    ///
    /// # Arguments
    /// - `market`: Target market
    /// - `order_size`: Size of the proposed order
    /// - `order_price`: Price of the proposed order
    /// - `mark_price`: Current mark price for position valuation
    ///
    /// # Returns
    /// - `Ok(())` if the order is allowed
    /// - `Err(RejectReason::MaxPositionPerMarket)` if it would exceed the limit
    pub fn check(
        &self,
        market: &MarketKey,
        order_size: Size,
        order_price: Price,
        mark_price: Price,
    ) -> Result<(), RejectReason> {
        // Calculate current position notional at mark price
        let current_notional = self.position_handle.get_notional(market, mark_price);

        // Calculate pending notional (excluding reduce-only orders)
        let pending_notional = self
            .position_handle
            .get_pending_notional_excluding_reduce_only(market, mark_price);

        // Calculate order notional
        let order_notional = Size::new(order_size.inner() * order_price.inner());

        // Total notional after this order
        let total_notional =
            Size::new(current_notional.inner() + pending_notional.inner() + order_notional.inner());

        // Check against limit
        if total_notional.inner() > self.max_notional_usd.inner() {
            debug!(
                market = %market,
                current_notional = %current_notional,
                pending_notional = %pending_notional,
                order_notional = %order_notional,
                total_notional = %total_notional,
                max_notional = %self.max_notional_usd,
                "MaxPositionPerMarket gate blocked"
            );
            return Err(RejectReason::MaxPositionPerMarket);
        }

        trace!(
            market = %market,
            total_notional = %total_notional,
            max_notional = %self.max_notional_usd,
            "MaxPositionPerMarket gate passed"
        );

        Ok(())
    }

    /// Get the maximum notional limit.
    #[must_use]
    pub fn max_notional_usd(&self) -> Price {
        self.max_notional_usd
    }
}

// ============================================================================
// MaxPositionTotal Gate
// ============================================================================

/// MaxPositionTotal Gate: Limits total portfolio position across all markets.
///
/// Checks that the sum of all positions + pending orders does not exceed
/// the maximum total notional limit.
pub struct MaxPositionTotalGate {
    /// Maximum total notional value in USD across all markets.
    max_total_notional_usd: Price,
    /// Position tracker handle for checking current positions.
    position_handle: PositionTrackerHandle,
}

impl MaxPositionTotalGate {
    /// Create a new MaxPositionTotal gate.
    ///
    /// # Arguments
    /// - `max_total_notional_usd`: Maximum total notional value in USD
    /// - `position_handle`: Handle to the position tracker
    #[must_use]
    pub fn new(max_total_notional_usd: Price, position_handle: PositionTrackerHandle) -> Self {
        Self {
            max_total_notional_usd,
            position_handle,
        }
    }

    /// Check if the order would exceed the total position limit.
    ///
    /// # Arguments
    /// - `order_notional`: Notional value of the proposed order
    /// - `mark_prices`: Current mark prices for all markets with positions
    ///
    /// # Returns
    /// - `Ok(())` if the order is allowed
    /// - `Err(RejectReason::MaxPositionTotal)` if it would exceed the limit
    pub fn check(
        &self,
        order_notional: Size,
        mark_prices: &HashMap<MarketKey, Price>,
    ) -> Result<(), RejectReason> {
        // Calculate total current notional across all markets
        let positions = self.position_handle.positions_snapshot();
        let mut total_current_notional = Decimal::ZERO;

        for pos in &positions {
            if let Some(mark_px) = mark_prices.get(&pos.market) {
                let pos_notional = pos.notional(*mark_px);
                total_current_notional += pos_notional.inner();
            } else {
                // If no mark price available, use entry price as fallback
                let pos_notional = pos.notional(pos.entry_price);
                total_current_notional += pos_notional.inner();
                warn!(
                    market = %pos.market,
                    "No mark price available for position, using entry price"
                );
            }
        }

        // Add order notional
        let total_notional = total_current_notional + order_notional.inner();

        // Check against limit
        if total_notional > self.max_total_notional_usd.inner() {
            debug!(
                total_current_notional = %total_current_notional,
                order_notional = %order_notional,
                total_notional = %total_notional,
                max_total_notional = %self.max_total_notional_usd,
                "MaxPositionTotal gate blocked"
            );
            return Err(RejectReason::MaxPositionTotal);
        }

        trace!(
            total_notional = %total_notional,
            max_total_notional = %self.max_total_notional_usd,
            "MaxPositionTotal gate passed"
        );

        Ok(())
    }

    /// Get the maximum total notional limit.
    #[must_use]
    pub fn max_total_notional_usd(&self) -> Price {
        self.max_total_notional_usd
    }
}

// ============================================================================
// CorrelationPositionGate (P3-3)
// ============================================================================

fn default_correlation_weight() -> f64 {
    1.5
}

fn default_max_weighted_positions() -> f64 {
    5.0
}

/// A single correlation group definition from config.
///
/// Example TOML:
/// ```toml
/// [[correlation_position.groups]]
/// name = "precious_metals"
/// markets = ["GOLD", "SILVER", "PLATINUM"]
/// weight = 1.5
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelationGroupDef {
    /// Human-readable group name (e.g., "precious_metals").
    pub name: String,
    /// Coin names in this group (e.g., ["GOLD", "SILVER", "PLATINUM"]).
    pub markets: Vec<String>,
    /// Correlation weight applied when ≥2 same-direction positions exist.
    /// Default: 1.5
    #[serde(default = "default_correlation_weight")]
    pub weight: f64,
}

/// P3-3: Configuration for correlation-weighted position limits.
///
/// When enabled, replaces the simple `max_concurrent_positions` check (Gate 5)
/// with correlation-aware weighted counting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelationPositionConfig {
    /// Whether correlation position limits are enabled. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Correlation group definitions.
    #[serde(default)]
    pub groups: Vec<CorrelationGroupDef>,
    /// Maximum weighted position count. Replaces max_concurrent_positions when enabled.
    /// Default: 5.0
    #[serde(default = "default_max_weighted_positions")]
    pub max_weighted_positions: f64,
}

impl Default for CorrelationPositionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            groups: Vec::new(),
            max_weighted_positions: default_max_weighted_positions(),
        }
    }
}

/// A resolved correlation group with `MarketKey`s (runtime representation).
///
/// Created at startup by resolving coin names from config to `MarketKey`s.
#[derive(Debug)]
pub struct ResolvedCorrelationGroup {
    /// Group name.
    pub name: String,
    /// Resolved `MarketKey`s for markets in this group.
    pub markets: HashSet<MarketKey>,
    /// Correlation weight.
    pub weight: Decimal,
}

/// P3-3: Correlation-weighted position limit gate.
///
/// Trading Philosophy: `max_concurrent_positions=10→5` is a blunt instrument.
/// Correlated same-direction positions (e.g., GOLD+SILVER both long) carry
/// amplified risk and should "count for more" toward the position limit.
///
/// When ≥2 positions in the same correlation group share the same direction,
/// each position in that (group, direction) cluster counts as `group.weight`
/// instead of 1.0 toward the total weighted position count.
pub struct CorrelationPositionGate {
    /// Resolved correlation groups.
    groups: Vec<ResolvedCorrelationGroup>,
    /// Position tracker handle.
    position_handle: PositionTrackerHandle,
    /// Maximum weighted position count.
    max_weighted: Decimal,
}

impl CorrelationPositionGate {
    /// Create a new `CorrelationPositionGate`.
    #[must_use]
    pub fn new(
        groups: Vec<ResolvedCorrelationGroup>,
        position_handle: PositionTrackerHandle,
        max_weighted: Decimal,
    ) -> Self {
        Self {
            groups,
            position_handle,
            max_weighted,
        }
    }

    /// Check if adding a position at `market` with `side` would exceed the weighted limit.
    ///
    /// # Algorithm
    ///
    /// 1. Get current positions and virtually add the proposed entry.
    /// 2. For each position, determine its weight:
    ///    - If in a correlation group with ≥2 same-direction positions → `group.weight`
    ///    - Otherwise → 1.0
    /// 3. Sum all weights. If sum > `max_weighted` → reject.
    pub fn check(&self, market: &MarketKey, side: OrderSide) -> Result<(), RejectReason> {
        let positions = self.position_handle.positions_snapshot();

        // Build list of (market, side) including the proposed entry.
        let mut all_entries: Vec<(MarketKey, OrderSide)> =
            positions.iter().map(|p| (p.market, p.side)).collect();
        all_entries.push((*market, side));

        // Calculate weighted count.
        let mut total_weighted = Decimal::ZERO;
        for &(m, s) in &all_entries {
            total_weighted += self.position_weight(m, s, &all_entries);
        }

        if total_weighted > self.max_weighted {
            debug!(
                market = %market,
                side = ?side,
                total_weighted = %total_weighted,
                max_weighted = %self.max_weighted,
                "CorrelationPositionGate blocked"
            );
            return Err(RejectReason::CorrelationPositionLimit);
        }

        trace!(
            market = %market,
            total_weighted = %total_weighted,
            max_weighted = %self.max_weighted,
            "CorrelationPositionGate passed"
        );

        Ok(())
    }

    /// Calculate the weight for a single position.
    ///
    /// If the position is in a correlation group and there are ≥2 same-direction
    /// positions in that group, the weight is `group.weight` (e.g., 1.5).
    /// Otherwise, the weight is 1.0.
    fn position_weight(
        &self,
        market: MarketKey,
        side: OrderSide,
        all_entries: &[(MarketKey, OrderSide)],
    ) -> Decimal {
        for group in &self.groups {
            if !group.markets.contains(&market) {
                continue;
            }
            // Count same-direction positions in this group.
            let same_dir_count = all_entries
                .iter()
                .filter(|&&(m, s)| group.markets.contains(&m) && s == side)
                .count();

            if same_dir_count >= 2 {
                return group.weight;
            }
            // In a group but only 1 same-direction position → weight 1.0.
            return Decimal::ONE;
        }
        // Not in any group → weight 1.0.
        Decimal::ONE
    }
}

// ============================================================================
// MaxPosition Gate Tests
// ============================================================================

#[cfg(test)]
mod max_position_tests {
    use super::*;
    use hip3_core::{AssetId, DexId, OrderSide, PendingOrder, TrackedOrder};
    use hip3_position::spawn_position_tracker;
    use rust_decimal_macros::dec;

    fn sample_market() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    fn sample_market_2() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(1))
    }

    #[tokio::test]
    async fn test_max_position_per_market_pass() {
        let (handle, _join) = spawn_position_tracker(100);

        // Set max notional to $10,000
        let gate = MaxPositionPerMarketGate::new(Price::new(dec!(10000)), handle.clone());

        let market = sample_market();
        let order_size = Size::new(dec!(0.1)); // 0.1 BTC
        let order_price = Price::new(dec!(50000)); // $50,000
        let mark_price = Price::new(dec!(50000));

        // Order notional = 0.1 * 50000 = $5000 < $10000 limit
        let result = gate.check(&market, order_size, order_price, mark_price);
        assert!(result.is_ok());

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_max_position_per_market_block() {
        let (handle, _join) = spawn_position_tracker(100);

        // Set max notional to $10,000
        let gate = MaxPositionPerMarketGate::new(Price::new(dec!(10000)), handle.clone());

        let market = sample_market();
        let order_size = Size::new(dec!(0.3)); // 0.3 BTC
        let order_price = Price::new(dec!(50000)); // $50,000
        let mark_price = Price::new(dec!(50000));

        // Order notional = 0.3 * 50000 = $15000 > $10000 limit
        let result = gate.check(&market, order_size, order_price, mark_price);
        assert_eq!(result, Err(RejectReason::MaxPositionPerMarket));

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_max_position_per_market_with_existing_position() {
        let (handle, _join) = spawn_position_tracker(100);

        let market = sample_market();

        // Create existing position: 0.1 BTC @ $50000 = $5000 notional
        handle
            .fill(
                market,
                OrderSide::Buy,
                Price::new(dec!(50000)),
                Size::new(dec!(0.1)),
                1234567890,
                None, // cloid for deduplication
                None, // entry_edge_bps
            )
            .await;

        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        // Set max notional to $10,000
        let gate = MaxPositionPerMarketGate::new(Price::new(dec!(10000)), handle.clone());

        // New order: 0.05 BTC @ $50000 = $2500 notional
        // Total = $5000 + $2500 = $7500 < $10000 limit
        let result = gate.check(
            &market,
            Size::new(dec!(0.05)),
            Price::new(dec!(50000)),
            Price::new(dec!(50000)),
        );
        assert!(result.is_ok());

        // New order: 0.15 BTC @ $50000 = $7500 notional
        // Total = $5000 + $7500 = $12500 > $10000 limit
        let result = gate.check(
            &market,
            Size::new(dec!(0.15)),
            Price::new(dec!(50000)),
            Price::new(dec!(50000)),
        );
        assert_eq!(result, Err(RejectReason::MaxPositionPerMarket));

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_max_position_total_pass() {
        let (handle, _join) = spawn_position_tracker(100);

        // Set max total notional to $20,000
        let gate = MaxPositionTotalGate::new(Price::new(dec!(20000)), handle.clone());

        let order_notional = Size::new(dec!(5000)); // $5000
        let mark_prices = HashMap::new();

        // No existing positions, order $5000 < $20000 limit
        let result = gate.check(order_notional, &mark_prices);
        assert!(result.is_ok());

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_max_position_total_block() {
        let (handle, _join) = spawn_position_tracker(100);

        let market1 = sample_market();
        let market2 = sample_market_2();

        // Create positions in two markets
        // Market 1: 0.2 BTC @ $50000 = $10000
        handle
            .fill(
                market1,
                OrderSide::Buy,
                Price::new(dec!(50000)),
                Size::new(dec!(0.2)),
                1234567890,
                None, // cloid for deduplication
                None, // entry_edge_bps
            )
            .await;

        // Market 2: 0.1 ETH @ $3000 = $300
        handle
            .fill(
                market2,
                OrderSide::Buy,
                Price::new(dec!(3000)),
                Size::new(dec!(0.1)),
                1234567891,
                None, // cloid for deduplication
                None, // entry_edge_bps
            )
            .await;

        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        // Set max total notional to $15,000
        let gate = MaxPositionTotalGate::new(Price::new(dec!(15000)), handle.clone());

        let mut mark_prices = HashMap::new();
        mark_prices.insert(market1, Price::new(dec!(50000)));
        mark_prices.insert(market2, Price::new(dec!(3000)));

        // Total current: $10000 + $300 = $10300
        // New order: $5000
        // Total after: $15300 > $15000 limit
        let result = gate.check(Size::new(dec!(5000)), &mark_prices);
        assert_eq!(result, Err(RejectReason::MaxPositionTotal));

        // Smaller order: $4000
        // Total after: $14300 < $15000 limit
        let result = gate.check(Size::new(dec!(4000)), &mark_prices);
        assert!(result.is_ok());

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_max_position_per_market_with_pending_order() {
        let (handle, _join) = spawn_position_tracker(100);

        let market = sample_market();

        // Register a pending order (not reduce-only)
        let pending = PendingOrder::new(
            hip3_core::ClientOrderId::new(),
            market,
            OrderSide::Buy,
            Price::new(dec!(50000)),
            Size::new(dec!(0.1)), // $5000 notional
            false,                // not reduce-only
            1234567890,
        );
        let tracked = TrackedOrder::from_pending(pending);
        handle.register_order(tracked).await;

        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        // Set max notional to $10,000
        let gate = MaxPositionPerMarketGate::new(Price::new(dec!(10000)), handle.clone());

        // New order: 0.08 BTC @ $50000 = $4000 notional
        // Pending: $5000
        // Total = $4000 + $5000 = $9000 < $10000 limit
        let result = gate.check(
            &market,
            Size::new(dec!(0.08)),
            Price::new(dec!(50000)),
            Price::new(dec!(50000)),
        );
        assert!(result.is_ok());

        // New order: 0.12 BTC @ $50000 = $6000 notional
        // Pending: $5000
        // Total = $6000 + $5000 = $11000 > $10000 limit
        let result = gate.check(
            &market,
            Size::new(dec!(0.12)),
            Price::new(dec!(50000)),
            Price::new(dec!(50000)),
        );
        assert_eq!(result, Err(RejectReason::MaxPositionPerMarket));

        handle.shutdown().await;
    }
}

// ============================================================================
// MaxDrawdownGate (P2-3)
// ============================================================================

/// Configuration for the MaxDrawdown gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaxDrawdownConfig {
    /// Maximum hourly drawdown in USD before blocking new entries.
    /// Set to 0 to disable this gate.
    #[serde(default)]
    pub max_hourly_drawdown_usd: f64,
}

impl Default for MaxDrawdownConfig {
    fn default() -> Self {
        Self {
            max_hourly_drawdown_usd: 0.0, // Disabled by default
        }
    }
}

/// MaxDrawdownGate: Blocks new entries when session drawdown exceeds threshold.
///
/// Tracks cumulative PnL within a rolling time window. When losses exceed
/// the configured threshold, new entries are blocked while existing position
/// management continues.
///
/// Thread-safe: Uses AtomicI64 for PnL tracking (units = cents, i.e. USD * 100).
pub struct MaxDrawdownGate {
    config: MaxDrawdownConfig,
    /// Cumulative PnL in cents (USD * 100) for the current window.
    /// Negative = loss. Using AtomicI64 for lock-free thread safety.
    cumulative_pnl_cents: std::sync::atomic::AtomicI64,
    /// Timestamp (ms) when the current window started.
    window_start_ms: std::sync::atomic::AtomicU64,
    /// Window duration in milliseconds (1 hour = 3_600_000).
    window_duration_ms: u64,
}

impl MaxDrawdownGate {
    /// Create a new MaxDrawdownGate.
    #[must_use]
    pub fn new(config: MaxDrawdownConfig) -> Self {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        Self {
            config,
            cumulative_pnl_cents: std::sync::atomic::AtomicI64::new(0),
            window_start_ms: std::sync::atomic::AtomicU64::new(now_ms),
            window_duration_ms: 3_600_000, // 1 hour
        }
    }

    /// Report a realized PnL from a closed position.
    ///
    /// # Arguments
    /// - `pnl_usd`: PnL in USD (positive = profit, negative = loss)
    pub fn report_pnl(&self, pnl_usd: f64) {
        self.maybe_reset_window();
        let pnl_cents = (pnl_usd * 100.0) as i64;
        self.cumulative_pnl_cents
            .fetch_add(pnl_cents, std::sync::atomic::Ordering::Relaxed);
    }

    /// Check if new entries should be blocked due to drawdown.
    ///
    /// Returns `Ok(())` if allowed, `Err(RejectReason::MaxDrawdown)` if blocked.
    pub fn check(&self) -> Result<(), RejectReason> {
        // Gate disabled when threshold is 0
        if self.config.max_hourly_drawdown_usd <= 0.0 {
            return Ok(());
        }

        self.maybe_reset_window();

        let pnl_cents = self
            .cumulative_pnl_cents
            .load(std::sync::atomic::Ordering::Relaxed);
        let threshold_cents = (self.config.max_hourly_drawdown_usd * -100.0) as i64;

        if pnl_cents <= threshold_cents {
            debug!(
                pnl_usd = pnl_cents as f64 / 100.0,
                threshold_usd = self.config.max_hourly_drawdown_usd,
                "MaxDrawdownGate blocked: drawdown exceeded"
            );
            return Err(RejectReason::MaxDrawdown);
        }

        Ok(())
    }

    /// Reset window if expired.
    fn maybe_reset_window(&self) {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let window_start = self
            .window_start_ms
            .load(std::sync::atomic::Ordering::Relaxed);

        if now_ms.saturating_sub(window_start) >= self.window_duration_ms {
            // Reset window (compare-and-swap to handle concurrent resets)
            if self
                .window_start_ms
                .compare_exchange(
                    window_start,
                    now_ms,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .is_ok()
            {
                self.cumulative_pnl_cents
                    .store(0, std::sync::atomic::Ordering::Relaxed);
                debug!("MaxDrawdownGate: window reset");
            }
        }
    }

    /// Get current cumulative PnL in USD.
    #[must_use]
    pub fn cumulative_pnl_usd(&self) -> f64 {
        let cents = self
            .cumulative_pnl_cents
            .load(std::sync::atomic::Ordering::Relaxed);
        cents as f64 / 100.0
    }

    /// Check if the gate is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.config.max_hourly_drawdown_usd > 0.0
    }
}

// ============================================================================
// CorrelationCooldownGate (P2-4)
// ============================================================================

/// Configuration for the CorrelationCooldown gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelationCooldownConfig {
    /// Number of position closes within window to trigger cooldown.
    /// Set to 0 to disable this gate.
    #[serde(default)]
    pub correlation_close_threshold: u32,
    /// Window in seconds to count position closes.
    #[serde(default = "default_correlation_window_secs")]
    pub correlation_window_secs: u64,
    /// Cooldown duration in seconds after threshold is breached.
    #[serde(default = "default_correlation_cooldown_secs")]
    pub correlation_cooldown_secs: u64,
}

fn default_correlation_window_secs() -> u64 {
    30
}

fn default_correlation_cooldown_secs() -> u64 {
    60
}

impl Default for CorrelationCooldownConfig {
    fn default() -> Self {
        Self {
            correlation_close_threshold: 0, // Disabled by default
            correlation_window_secs: default_correlation_window_secs(),
            correlation_cooldown_secs: default_correlation_cooldown_secs(),
        }
    }
}

/// CorrelationCooldownGate: Blocks new entries after correlated position closes.
///
/// When N or more positions close within a short time window, it suggests
/// a correlated market event (e.g., US Pre-market at 09:00 UTC causing
/// multiple simultaneous losses). During cooldown, new entries are blocked.
///
/// Thread-safe: Uses Mutex for close event tracking.
pub struct CorrelationCooldownGate {
    config: CorrelationCooldownConfig,
    /// Timestamps (ms) of recent position closes.
    close_timestamps: parking_lot::Mutex<Vec<u64>>,
    /// When cooldown expires (0 = not in cooldown).
    cooldown_until_ms: std::sync::atomic::AtomicU64,
}

impl CorrelationCooldownGate {
    /// Create a new CorrelationCooldownGate.
    #[must_use]
    pub fn new(config: CorrelationCooldownConfig) -> Self {
        Self {
            config,
            close_timestamps: parking_lot::Mutex::new(Vec::new()),
            cooldown_until_ms: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Report a position close event.
    pub fn report_close(&self) {
        if self.config.correlation_close_threshold == 0 {
            return;
        }

        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let window_ms = self.config.correlation_window_secs * 1000;

        let mut timestamps = self.close_timestamps.lock();

        // Add new close event
        timestamps.push(now_ms);

        // Remove events outside the window
        timestamps.retain(|&t| now_ms.saturating_sub(t) < window_ms);

        // Check if threshold breached
        if timestamps.len() >= self.config.correlation_close_threshold as usize {
            let cooldown_until = now_ms + self.config.correlation_cooldown_secs * 1000;
            self.cooldown_until_ms
                .store(cooldown_until, std::sync::atomic::Ordering::Relaxed);

            warn!(
                close_count = timestamps.len(),
                window_secs = self.config.correlation_window_secs,
                cooldown_secs = self.config.correlation_cooldown_secs,
                "CorrelationCooldownGate: cooldown triggered"
            );

            // Clear timestamps to prevent re-triggering
            timestamps.clear();
        }
    }

    /// Check if new entries should be blocked due to cooldown.
    ///
    /// Returns `Ok(())` if allowed, `Err(RejectReason::CorrelationCooldown)` if blocked.
    pub fn check(&self) -> Result<(), RejectReason> {
        // Gate disabled when threshold is 0
        if self.config.correlation_close_threshold == 0 {
            return Ok(());
        }

        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let cooldown_until = self
            .cooldown_until_ms
            .load(std::sync::atomic::Ordering::Relaxed);

        if now_ms < cooldown_until {
            let remaining_secs = (cooldown_until - now_ms) / 1000;
            debug!(
                remaining_secs,
                "CorrelationCooldownGate blocked: cooldown active"
            );
            return Err(RejectReason::CorrelationCooldown);
        }

        Ok(())
    }

    /// Check if the gate is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.config.correlation_close_threshold > 0
    }

    /// Check if currently in cooldown.
    #[must_use]
    pub fn is_in_cooldown(&self) -> bool {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let cooldown_until = self
            .cooldown_until_ms
            .load(std::sync::atomic::Ordering::Relaxed);
        now_ms < cooldown_until
    }
}

// ============================================================================
// BurstSignalGate (Sprint 1 - Strategy Evolution)
// ============================================================================

/// Configuration for the BurstSignal gate.
///
/// Limits the number of signals per market within a rolling time window.
/// Prevents over-trading during burst activity periods which have significantly
/// worse win rates (backtest: burst trades = losing, isolated trades = +$0.20).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BurstSignalConfig {
    /// Enable burst signal rate limiting.
    /// Set to false to disable this gate entirely.
    #[serde(default)]
    pub enabled: bool,
    /// Rolling window in seconds for counting signals per market.
    /// Default: 1800 (30 minutes).
    #[serde(default = "default_burst_window_secs")]
    pub burst_window_secs: u64,
    /// Maximum number of signals allowed per market within the window.
    /// Default: 3.
    #[serde(default = "default_burst_max_signals")]
    pub burst_max_signals: u32,
    /// Cooldown in seconds after burst limit is hit for a market.
    /// During cooldown, all signals for that market are blocked.
    /// Default: 300 (5 minutes).
    #[serde(default = "default_burst_cooldown_secs")]
    pub burst_cooldown_secs: u64,
}

fn default_burst_window_secs() -> u64 {
    1800 // 30 minutes
}

fn default_burst_max_signals() -> u32 {
    3
}

fn default_burst_cooldown_secs() -> u64 {
    300 // 5 minutes
}

impl Default for BurstSignalConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default
            burst_window_secs: default_burst_window_secs(),
            burst_max_signals: default_burst_max_signals(),
            burst_cooldown_secs: default_burst_cooldown_secs(),
        }
    }
}

/// Per-market burst state (signals + cooldown), consolidated under a single lock.
#[derive(Default)]
struct MarketBurstState {
    /// Timestamps (ms) of recorded signals.
    signals: Vec<u64>,
    /// Cooldown expiry (ms). 0 = no cooldown.
    cooldown_until: u64,
}

/// BurstSignalGate: Limits per-market signal frequency within a rolling window.
///
/// Backtest analysis (Feb 4-6) showed that burst trading (3+ signals in same
/// market within 30 min) accounts for 84% of trades but has significantly
/// worse performance. Non-burst (isolated/normal) trades: 127 trades, 33.9% WR.
///
/// Uses split `check()` / `record()` API:
/// - `check()` at early gate (read-only): rejects if cooldown active or count at limit
/// - `record()` after all gates pass: records the signal, triggers cooldown if threshold hit
///
/// This ensures only signals that actually trade count toward the burst limit.
///
/// Thread-safe: Single `parking_lot::Mutex` for per-market state.
pub struct BurstSignalGate {
    config: BurstSignalConfig,
    /// Per-market state under a single lock (eliminates nested-mutex concern).
    state: parking_lot::Mutex<std::collections::HashMap<MarketKey, MarketBurstState>>,
}

impl BurstSignalGate {
    /// Create a new BurstSignalGate.
    #[must_use]
    pub fn new(config: BurstSignalConfig) -> Self {
        Self {
            config,
            state: parking_lot::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Check if a signal for this market should be allowed (read-only).
    ///
    /// Called at Gate 1d (early in the pipeline). Does NOT record the signal.
    /// Returns `Ok(())` if the signal may proceed, or `Err(RejectReason::BurstSignal)`
    /// if cooldown is active or the burst limit is already reached.
    pub fn check(&self, market: &MarketKey) -> Result<(), RejectReason> {
        if !self.config.enabled {
            return Ok(());
        }

        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let state = self.state.lock();

        if let Some(ms) = state.get(market) {
            // Check cooldown
            if now_ms < ms.cooldown_until {
                debug!(
                    market = %market,
                    remaining_secs = (ms.cooldown_until - now_ms) / 1000,
                    "BurstSignalGate blocked: cooldown active"
                );
                return Err(RejectReason::BurstSignal);
            }

            // Count signals in window
            let window_ms = self.config.burst_window_secs * 1000;
            let count = ms
                .signals
                .iter()
                .filter(|&&t| now_ms.saturating_sub(t) < window_ms)
                .count();

            if count >= self.config.burst_max_signals as usize {
                debug!(
                    market = %market,
                    count,
                    max = self.config.burst_max_signals,
                    "BurstSignalGate blocked: burst limit reached"
                );
                return Err(RejectReason::BurstSignal);
            }
        }

        Ok(())
    }

    /// Record a signal for a market. Call only after ALL gates pass (before enqueue).
    ///
    /// Prunes expired signals, records the new one, and triggers cooldown
    /// if the burst threshold is reached.
    pub fn record(&self, market: &MarketKey) {
        if !self.config.enabled {
            return;
        }

        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let window_ms = self.config.burst_window_secs * 1000;

        let mut state = self.state.lock();
        let ms = state.entry(*market).or_default();

        // Prune expired signals
        ms.signals.retain(|&t| now_ms.saturating_sub(t) < window_ms);

        // Record new signal
        ms.signals.push(now_ms);

        // Trigger cooldown if at limit
        if ms.signals.len() >= self.config.burst_max_signals as usize {
            ms.cooldown_until = now_ms + self.config.burst_cooldown_secs * 1000;

            warn!(
                market = %market,
                signals_in_window = ms.signals.len(),
                window_secs = self.config.burst_window_secs,
                cooldown_secs = self.config.burst_cooldown_secs,
                "BurstSignalGate: burst limit hit, cooldown triggered"
            );

            // Clear signals to prevent repeated triggering
            ms.signals.clear();
        }
    }

    /// Check if the gate is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Check if a specific market is in cooldown.
    #[must_use]
    pub fn is_market_in_cooldown(&self, market: &MarketKey) -> bool {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let state = self.state.lock();
        state
            .get(market)
            .is_some_and(|ms| now_ms < ms.cooldown_until)
    }

    /// Get the number of signals recorded for a market in the current window.
    #[must_use]
    pub fn signal_count(&self, market: &MarketKey) -> usize {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let window_ms = self.config.burst_window_secs * 1000;
        let state = self.state.lock();
        state.get(market).map_or(0, |ms| {
            ms.signals
                .iter()
                .filter(|&&t| now_ms.saturating_sub(t) < window_ms)
                .count()
        })
    }
}

// ============================================================================
// TiltGuardGate — consecutive loss cooldown
// ============================================================================

/// Configuration for tilt guard (consecutive loss cooldown).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TiltGuardConfig {
    /// Enable tilt guard.
    pub enabled: bool,
    /// Number of consecutive losses before cooldown triggers.
    pub max_consecutive_losses: u32,
    /// Cooldown duration in seconds after triggering.
    pub cooldown_secs: u64,
}

impl Default for TiltGuardConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_consecutive_losses: 3,
            cooldown_secs: 300,
        }
    }
}

/// Per-session tilt guard state.
struct TiltGuardState {
    consecutive_losses: u32,
    cooldown_until_ms: u64,
}

/// Gate that blocks new entries after consecutive losses.
///
/// When N consecutive losses occur, a cooldown period is activated.
/// During cooldown, all new entries are rejected (TiltGuard).
/// A winning trade resets the consecutive count.
pub struct TiltGuardGate {
    config: TiltGuardConfig,
    state: parking_lot::Mutex<TiltGuardState>,
}

impl TiltGuardGate {
    /// Create a new TiltGuardGate.
    #[must_use]
    pub fn new(config: TiltGuardConfig) -> Self {
        Self {
            config,
            state: parking_lot::Mutex::new(TiltGuardState {
                consecutive_losses: 0,
                cooldown_until_ms: 0,
            }),
        }
    }

    /// Report a trade P&L. Negative = loss, positive = win.
    pub fn report_pnl(&self, pnl_usd: f64) {
        let mut state = self.state.lock();
        if pnl_usd < 0.0 {
            state.consecutive_losses += 1;
            if state.consecutive_losses >= self.config.max_consecutive_losses {
                let now_ms = chrono::Utc::now().timestamp_millis() as u64;
                state.cooldown_until_ms = now_ms + self.config.cooldown_secs * 1000;
                warn!(
                    consecutive = state.consecutive_losses,
                    cooldown_secs = self.config.cooldown_secs,
                    "TiltGuard: cooldown triggered after consecutive losses"
                );
            }
        } else {
            state.consecutive_losses = 0;
        }
    }

    /// Check if trading is allowed.
    pub fn check(&self) -> Result<(), RejectReason> {
        if !self.config.enabled {
            return Ok(());
        }
        let state = self.state.lock();
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        if now_ms < state.cooldown_until_ms {
            Err(RejectReason::TiltGuard)
        } else {
            Ok(())
        }
    }

    /// Check if the gate is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

// ============================================================================
// ReEntryDelayGate — same-market re-entry delay
// ============================================================================

/// Configuration for same-market re-entry delay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReEntryDelayConfig {
    /// Enable re-entry delay.
    pub enabled: bool,
    /// Delay in milliseconds before re-entering the same market.
    pub re_entry_delay_ms: u64,
}

impl Default for ReEntryDelayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            re_entry_delay_ms: 30_000,
        }
    }
}

/// Gate that blocks re-entry into a market too soon after closing a position.
///
/// Data shows same-market re-entry WR degrades from 45.1% to 30.2%.
/// This gate enforces a minimum delay between position close and next entry.
pub struct ReEntryDelayGate {
    config: ReEntryDelayConfig,
    last_close: parking_lot::Mutex<std::collections::HashMap<MarketKey, u64>>,
}

impl ReEntryDelayGate {
    /// Create a new ReEntryDelayGate.
    #[must_use]
    pub fn new(config: ReEntryDelayConfig) -> Self {
        Self {
            config,
            last_close: parking_lot::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Report a position close for a market.
    pub fn report_close(&self, market: &MarketKey) {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        self.last_close.lock().insert(*market, now_ms);
    }

    /// Check if re-entry is allowed for a market.
    pub fn check(&self, market: &MarketKey) -> Result<(), RejectReason> {
        if !self.config.enabled {
            return Ok(());
        }
        let state = self.last_close.lock();
        if let Some(&close_ms) = state.get(market) {
            let now_ms = chrono::Utc::now().timestamp_millis() as u64;
            if now_ms < close_ms + self.config.re_entry_delay_ms {
                return Err(RejectReason::ReEntryDelay);
            }
        }
        Ok(())
    }

    /// Check if the gate is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

// ============================================================================
// P2-3/P2-4 Gate Tests
// ============================================================================

#[cfg(test)]
mod drawdown_tests {
    use super::*;

    #[test]
    fn test_drawdown_gate_disabled_by_default() {
        let gate = MaxDrawdownGate::new(MaxDrawdownConfig::default());
        assert!(!gate.is_enabled());
        assert!(gate.check().is_ok());
    }

    #[test]
    fn test_drawdown_gate_blocks_on_loss() {
        let config = MaxDrawdownConfig {
            max_hourly_drawdown_usd: 10.0,
        };
        let gate = MaxDrawdownGate::new(config);

        // Report $5 loss
        gate.report_pnl(-5.0);
        assert!(gate.check().is_ok());

        // Report another $6 loss (total -$11 > -$10 threshold)
        gate.report_pnl(-6.0);
        assert_eq!(gate.check(), Err(RejectReason::MaxDrawdown));
    }

    #[test]
    fn test_drawdown_gate_profits_offset_losses() {
        let config = MaxDrawdownConfig {
            max_hourly_drawdown_usd: 10.0,
        };
        let gate = MaxDrawdownGate::new(config);

        // Report $8 loss
        gate.report_pnl(-8.0);
        assert!(gate.check().is_ok());

        // Report $5 profit (net = -$3)
        gate.report_pnl(5.0);
        assert!(gate.check().is_ok());

        // Report $8 loss (net = -$11 > -$10 threshold)
        gate.report_pnl(-8.0);
        assert_eq!(gate.check(), Err(RejectReason::MaxDrawdown));
    }

    #[test]
    fn test_drawdown_gate_cumulative_pnl() {
        let config = MaxDrawdownConfig {
            max_hourly_drawdown_usd: 10.0,
        };
        let gate = MaxDrawdownGate::new(config);

        gate.report_pnl(-3.5);
        gate.report_pnl(1.0);
        assert!((gate.cumulative_pnl_usd() - (-2.5)).abs() < 0.01);
    }
}

#[cfg(test)]
mod cooldown_tests {
    use super::*;

    #[test]
    fn test_cooldown_gate_disabled_by_default() {
        let gate = CorrelationCooldownGate::new(CorrelationCooldownConfig::default());
        assert!(!gate.is_enabled());
        assert!(gate.check().is_ok());
    }

    #[test]
    fn test_cooldown_gate_triggers_on_threshold() {
        let config = CorrelationCooldownConfig {
            correlation_close_threshold: 3,
            correlation_window_secs: 30,
            correlation_cooldown_secs: 60,
        };
        let gate = CorrelationCooldownGate::new(config);

        // Two closes - not enough
        gate.report_close();
        gate.report_close();
        assert!(gate.check().is_ok());
        assert!(!gate.is_in_cooldown());

        // Third close - triggers cooldown
        gate.report_close();
        assert_eq!(gate.check(), Err(RejectReason::CorrelationCooldown));
        assert!(gate.is_in_cooldown());
    }
}

// ============================================================================
// CorrelationPositionGate Tests (P3-3)
// ============================================================================

#[cfg(test)]
mod correlation_position_tests {
    use super::*;
    use hip3_core::{AssetId, DexId, OrderSide};
    use hip3_position::spawn_position_tracker;
    use rust_decimal_macros::dec;

    fn market(idx: u32) -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(idx))
    }

    fn make_group(name: &str, indices: &[u32], weight: Decimal) -> ResolvedCorrelationGroup {
        ResolvedCorrelationGroup {
            name: name.to_string(),
            markets: indices.iter().map(|&i| market(i)).collect(),
            weight,
        }
    }

    #[test]
    fn test_config_default() {
        let config = CorrelationPositionConfig::default();
        assert!(!config.enabled);
        assert!(config.groups.is_empty());
        assert!((config.max_weighted_positions - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_config_backward_compat() {
        // Default config should have all features disabled.
        let config = CorrelationPositionConfig::default();
        assert!(!config.enabled);
        assert!(config.groups.is_empty());
        // max_weighted_positions should have a sensible default.
        assert!(config.max_weighted_positions > 0.0);
    }

    #[tokio::test]
    async fn test_no_positions_passes() {
        let (handle, _join) = spawn_position_tracker(100);
        let groups = vec![make_group("metals", &[0, 1, 2], dec!(1.5))];
        let gate = CorrelationPositionGate::new(groups, handle.clone(), dec!(5));

        // Adding first position → weight 1.0 (single in group), total 1.0 ≤ 5
        assert!(gate.check(&market(0), OrderSide::Buy).is_ok());

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_uncorrelated_positions_count_as_one() {
        let (handle, _join) = spawn_position_tracker(100);

        // Fill 4 positions in unrelated markets (no groups)
        for idx in 10..14 {
            handle
                .fill(
                    market(idx),
                    OrderSide::Buy,
                    Price::new(dec!(100)),
                    Size::new(dec!(1)),
                    1000,
                    None,
                    None, // entry_edge_bps
                )
                .await;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        let groups = vec![make_group("metals", &[0, 1, 2], dec!(1.5))];
        let gate = CorrelationPositionGate::new(groups, handle.clone(), dec!(5));

        // 4 existing (1.0 each) + 1 proposed = 5.0 = max → pass (> not >=)
        assert!(gate.check(&market(20), OrderSide::Buy).is_ok());

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_correlated_same_direction_applies_weight() {
        let (handle, _join) = spawn_position_tracker(100);

        // Position in GOLD (idx 0) long
        handle
            .fill(
                market(0),
                OrderSide::Buy,
                Price::new(dec!(100)),
                Size::new(dec!(1)),
                1000,
                None,
                None, // entry_edge_bps
            )
            .await;
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        // Group: GOLD(0), SILVER(1), PLATINUM(2), weight=1.5
        let groups = vec![make_group("metals", &[0, 1, 2], dec!(1.5))];
        let gate = CorrelationPositionGate::new(groups, handle.clone(), dec!(5));

        // Propose SILVER(1) long → 2 same-direction in group → each 1.5
        // Total: 1.5 + 1.5 = 3.0 ≤ 5 → pass
        assert!(gate.check(&market(1), OrderSide::Buy).is_ok());

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_correlated_blocks_when_over_limit() {
        let (handle, _join) = spawn_position_tracker(100);

        // Fill GOLD(0) and SILVER(1) long
        for idx in [0, 1] {
            handle
                .fill(
                    market(idx),
                    OrderSide::Buy,
                    Price::new(dec!(100)),
                    Size::new(dec!(1)),
                    1000,
                    None,
                    None, // entry_edge_bps
                )
                .await;
        }
        // Fill two unrelated positions
        for idx in [10, 11] {
            handle
                .fill(
                    market(idx),
                    OrderSide::Buy,
                    Price::new(dec!(100)),
                    Size::new(dec!(1)),
                    1000,
                    None,
                    None, // entry_edge_bps
                )
                .await;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        // Group: metals {0,1,2} weight=1.5, max_weighted=5.0
        let groups = vec![make_group("metals", &[0, 1, 2], dec!(1.5))];
        let gate = CorrelationPositionGate::new(groups, handle.clone(), dec!(5));

        // Current: GOLD(0) + SILVER(1) same-dir → each 1.5 = 3.0
        //          mkt(10) + mkt(11) = 2.0
        //          Total existing weighted = 5.0
        // Propose PLATINUM(2) Buy → 3 in metals same-dir → each 1.5
        //   metals: 1.5*3 = 4.5, unrelated: 2.0, total = 6.5 > 5.0
        assert_eq!(
            gate.check(&market(2), OrderSide::Buy),
            Err(RejectReason::CorrelationPositionLimit)
        );

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_opposite_direction_not_correlated() {
        let (handle, _join) = spawn_position_tracker(100);

        // GOLD(0) long
        handle
            .fill(
                market(0),
                OrderSide::Buy,
                Price::new(dec!(100)),
                Size::new(dec!(1)),
                1000,
                None,
                None, // entry_edge_bps
            )
            .await;
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        let groups = vec![make_group("metals", &[0, 1, 2], dec!(1.5))];
        let gate = CorrelationPositionGate::new(groups, handle.clone(), dec!(5));

        // Propose SILVER(1) short → different direction → each counts 1.0
        // Total: 1.0 (GOLD long) + 1.0 (SILVER short) = 2.0 ≤ 5
        assert!(gate.check(&market(1), OrderSide::Sell).is_ok());

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_multiple_groups() {
        let (handle, _join) = spawn_position_tracker(100);

        // GOLD(0) long, EUR(10) long
        handle
            .fill(
                market(0),
                OrderSide::Buy,
                Price::new(dec!(100)),
                Size::new(dec!(1)),
                1000,
                None,
                None, // entry_edge_bps
            )
            .await;
        handle
            .fill(
                market(10),
                OrderSide::Buy,
                Price::new(dec!(100)),
                Size::new(dec!(1)),
                1000,
                None,
                None, // entry_edge_bps
            )
            .await;
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        let groups = vec![
            make_group("metals", &[0, 1, 2], dec!(1.5)),
            make_group("fx", &[10, 11, 12], dec!(2.0)),
        ];
        let gate = CorrelationPositionGate::new(groups, handle.clone(), dec!(5));

        // Propose JPY(11) long → fx group has EUR(10)+JPY(11) same-dir → each 2.0
        // metals: GOLD(0) alone → 1.0
        // fx: EUR(10) 2.0 + JPY(11) 2.0 = 4.0
        // Total: 1.0 + 4.0 = 5.0 → pass (not >)
        assert!(gate.check(&market(11), OrderSide::Buy).is_ok());

        // Propose DXY(12) long → fx group has 3 same-dir → each 2.0
        // metals: 1.0, fx: 2.0*3 = 6.0, total = 7.0 > 5.0 → block
        handle
            .fill(
                market(11),
                OrderSide::Buy,
                Price::new(dec!(100)),
                Size::new(dec!(1)),
                1000,
                None,
                None, // entry_edge_bps
            )
            .await;
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
        assert_eq!(
            gate.check(&market(12), OrderSide::Buy),
            Err(RejectReason::CorrelationPositionLimit)
        );

        handle.shutdown().await;
    }
}

// ============================================================================
// BurstSignalGate Tests
// ============================================================================

#[cfg(test)]
mod burst_signal_tests {
    use super::*;
    use hip3_core::{AssetId, DexId};

    fn market(idx: u32) -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(idx))
    }

    #[test]
    fn test_burst_gate_disabled_by_default() {
        let gate = BurstSignalGate::new(BurstSignalConfig::default());
        assert!(!gate.is_enabled());
        // Should always pass when disabled
        for _ in 0..5 {
            assert!(gate.check(&market(0)).is_ok());
            gate.record(&market(0));
        }
    }

    #[test]
    fn test_burst_gate_allows_within_limit() {
        let config = BurstSignalConfig {
            enabled: true,
            burst_window_secs: 1800,
            burst_max_signals: 3,
            burst_cooldown_secs: 300,
        };
        let gate = BurstSignalGate::new(config);

        // First 2 check+record cycles pass without triggering cooldown
        assert!(gate.check(&market(0)).is_ok());
        gate.record(&market(0));
        assert_eq!(gate.signal_count(&market(0)), 1);

        assert!(gate.check(&market(0)).is_ok());
        gate.record(&market(0));
        assert_eq!(gate.signal_count(&market(0)), 2);

        // 3rd check passes (count=2 < 3)
        assert!(gate.check(&market(0)).is_ok());
        // 3rd record triggers cooldown (count reaches 3) and clears signals
        gate.record(&market(0));
        assert!(gate.is_market_in_cooldown(&market(0)));
    }

    #[test]
    fn test_burst_gate_blocks_on_limit() {
        let config = BurstSignalConfig {
            enabled: true,
            burst_window_secs: 1800,
            burst_max_signals: 3,
            burst_cooldown_secs: 300,
        };
        let gate = BurstSignalGate::new(config);

        // Use up all 3 signals (3rd record triggers cooldown)
        for _ in 0..3 {
            assert!(gate.check(&market(0)).is_ok());
            gate.record(&market(0));
        }

        // 4th signal should be blocked (cooldown active after 3rd record)
        assert_eq!(gate.check(&market(0)), Err(RejectReason::BurstSignal));
        assert!(gate.is_market_in_cooldown(&market(0)));
    }

    #[test]
    fn test_burst_gate_per_market_isolation() {
        let config = BurstSignalConfig {
            enabled: true,
            burst_window_secs: 1800,
            burst_max_signals: 2,
            burst_cooldown_secs: 300,
        };
        let gate = BurstSignalGate::new(config);

        // Market 0: use up limit (2nd record triggers cooldown)
        for _ in 0..2 {
            assert!(gate.check(&market(0)).is_ok());
            gate.record(&market(0));
        }
        assert_eq!(gate.check(&market(0)), Err(RejectReason::BurstSignal));

        // Market 1: should still be allowed
        for _ in 0..2 {
            assert!(gate.check(&market(1)).is_ok());
            gate.record(&market(1));
        }

        // Market 1: now blocked too
        assert_eq!(gate.check(&market(1)), Err(RejectReason::BurstSignal));

        // Market 2: unaffected
        assert!(gate.check(&market(2)).is_ok());
        gate.record(&market(2));
        assert!(!gate.is_market_in_cooldown(&market(2)));
    }

    #[test]
    fn test_burst_gate_cooldown_blocks_all() {
        let config = BurstSignalConfig {
            enabled: true,
            burst_window_secs: 1800,
            burst_max_signals: 1,
            burst_cooldown_secs: 300,
        };
        let gate = BurstSignalGate::new(config);

        // First signal passes check; record triggers cooldown (1 >= 1)
        assert!(gate.check(&market(0)).is_ok());
        gate.record(&market(0));
        assert!(gate.is_market_in_cooldown(&market(0)));

        // During cooldown, all checks for this market blocked
        assert_eq!(gate.check(&market(0)), Err(RejectReason::BurstSignal));
        assert_eq!(gate.check(&market(0)), Err(RejectReason::BurstSignal));
    }

    #[test]
    fn test_burst_gate_config_defaults() {
        let config = BurstSignalConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.burst_window_secs, 1800);
        assert_eq!(config.burst_max_signals, 3);
        assert_eq!(config.burst_cooldown_secs, 300);
    }

    #[test]
    fn test_burst_gate_signal_count() {
        let config = BurstSignalConfig {
            enabled: true,
            burst_window_secs: 1800,
            burst_max_signals: 10,
            burst_cooldown_secs: 300,
        };
        let gate = BurstSignalGate::new(config);

        assert_eq!(gate.signal_count(&market(0)), 0);
        assert_eq!(gate.signal_count(&market(1)), 0);

        gate.record(&market(0));
        gate.record(&market(0));
        gate.record(&market(1));

        assert_eq!(gate.signal_count(&market(0)), 2);
        assert_eq!(gate.signal_count(&market(1)), 1);
    }

    #[test]
    fn test_burst_gate_check_is_read_only() {
        let config = BurstSignalConfig {
            enabled: true,
            burst_window_secs: 1800,
            burst_max_signals: 3,
            burst_cooldown_secs: 300,
        };
        let gate = BurstSignalGate::new(config);

        // Multiple checks should not change state
        for _ in 0..10 {
            assert!(gate.check(&market(0)).is_ok());
        }
        // No signals recorded (check is read-only)
        assert_eq!(gate.signal_count(&market(0)), 0);
        assert!(!gate.is_market_in_cooldown(&market(0)));
    }

    #[test]
    fn test_burst_gate_skipped_signals_dont_count() {
        let config = BurstSignalConfig {
            enabled: true,
            burst_window_secs: 1800,
            burst_max_signals: 3,
            burst_cooldown_secs: 300,
        };
        let gate = BurstSignalGate::new(config);

        // Simulate: 5 signals check, but only 2 pass downstream gates and record
        assert!(gate.check(&market(0)).is_ok());
        gate.record(&market(0)); // signal 1: passes all gates
        assert!(gate.check(&market(0)).is_ok()); // signal 2: check ok
                                                 // signal 2: rejected by downstream gate, no record()
        assert!(gate.check(&market(0)).is_ok()); // signal 3: check ok
        gate.record(&market(0)); // signal 3: passes all gates
        assert!(gate.check(&market(0)).is_ok()); // signal 4: check ok
                                                 // signal 4: rejected by downstream gate, no record()

        // Only 2 signals recorded (not 4)
        assert_eq!(gate.signal_count(&market(0)), 2);
        assert!(!gate.is_market_in_cooldown(&market(0)));
    }
}
