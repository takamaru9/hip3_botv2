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

use std::collections::HashMap;

use crate::error::{RiskError, RiskResult};
use hip3_core::types::MarketSnapshot;
use hip3_core::{MarketKey, MarketSpec, Price, RejectReason, Size};
use hip3_position::PositionTrackerHandle;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{debug, trace, warn};

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
}

fn default_max_bbo_age_ms() -> i64 {
    2000
}

fn default_max_ctx_age_ms() -> i64 {
    8000
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
        let mut results = Vec::with_capacity(8);

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
    pub fn check_mark_mid_divergence(&self, snapshot: &MarketSnapshot) -> GateResult {
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
    pub fn check_bbo_update(&self, bbo_age_ms: i64) -> GateResult {
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
    /// BUG-002: oracle_fresh gate was removed; now 8 gates total.
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
            8,
            "Should have 8 gate results (BUG-002: oracle_fresh removed)"
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
