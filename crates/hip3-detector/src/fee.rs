//! HIP-3 fee calculation (P0-24).
//!
//! Implements HIP-3-specific 2x taker fee multiplier and fee metadata tracking.
//! All fee calculations are performed with explicit audit trail for transparency.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// HIP-3 fee multiplier (2x standard perp fees).
pub const HIP3_FEE_MULTIPLIER: Decimal = Decimal::TWO;

/// Default HIP-3 base taker fee in basis points (before 2x multiplier).
pub const DEFAULT_BASE_TAKER_FEE_BPS: Decimal = Decimal::TWO; // 2 bps base

/// User fee tier from exchange API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserFees {
    /// Account's taker fee in basis points.
    pub taker_bps: Decimal,
    /// Account's maker fee in basis points (can be negative for rebates).
    pub maker_bps: Decimal,
    /// Whether user has VIP status.
    pub is_vip: bool,
    /// Fee tier name (e.g., "tier1", "vip1").
    pub tier: String,
}

impl Default for UserFees {
    fn default() -> Self {
        Self {
            taker_bps: DEFAULT_BASE_TAKER_FEE_BPS,
            maker_bps: Decimal::ONE,
            is_vip: false,
            tier: "default".to_string(),
        }
    }
}

impl UserFees {
    /// P0-4: Create UserFees from effective taker bps (HIP-3 2x already applied).
    ///
    /// This is used when config specifies effective fees (post-multiplier).
    /// The base fee is derived by dividing by HIP3_FEE_MULTIPLIER.
    ///
    /// # Example
    /// ```
    /// use hip3_detector::fee::UserFees;
    /// use rust_decimal_macros::dec;
    ///
    /// // Config has effective_taker_bps = 4 (which is 2 bps base * 2x)
    /// let fees = UserFees::from_effective_taker_bps(dec!(4));
    /// assert_eq!(fees.taker_bps, dec!(2)); // Base is 2 bps
    /// ```
    pub fn from_effective_taker_bps(effective_taker_bps: Decimal) -> Self {
        let base_taker_bps = effective_taker_bps / HIP3_FEE_MULTIPLIER;
        Self {
            taker_bps: base_taker_bps,
            maker_bps: Decimal::ONE, // Default maker fee
            is_vip: false,
            tier: "config".to_string(),
        }
    }
}

/// Fee calculation metadata for audit trail (P0-24).
///
/// Records all components of fee calculation for transparency:
/// - Base fees from exchange/user tier
/// - HIP-3 2x multiplier application
/// - Final effective fee used in edge calculation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeMetadata {
    /// Base taker fee from user tier (bps).
    pub base_taker_fee_bps: Decimal,
    /// HIP-3 fee multiplier applied.
    pub hip3_multiplier: Decimal,
    /// Effective taker fee after multiplier (bps).
    pub effective_taker_fee_bps: Decimal,
    /// Slippage estimate (bps).
    pub slippage_bps: Decimal,
    /// Minimum required edge (bps).
    pub min_edge_bps: Decimal,
    /// Total cost = effective_taker_fee + slippage + min_edge (bps).
    pub total_cost_bps: Decimal,
}

impl FeeMetadata {
    /// Create fee metadata from user fees and config.
    ///
    /// Applies HIP-3 2x multiplier to base taker fee.
    pub fn new(base_taker_fee_bps: Decimal, slippage_bps: Decimal, min_edge_bps: Decimal) -> Self {
        let effective_taker_fee_bps = base_taker_fee_bps * HIP3_FEE_MULTIPLIER;
        let total_cost_bps = effective_taker_fee_bps + slippage_bps + min_edge_bps;

        Self {
            base_taker_fee_bps,
            hip3_multiplier: HIP3_FEE_MULTIPLIER,
            effective_taker_fee_bps,
            slippage_bps,
            min_edge_bps,
            total_cost_bps,
        }
    }

    /// Create from UserFees and config parameters.
    pub fn from_user_fees(
        user_fees: &UserFees,
        slippage_bps: Decimal,
        min_edge_bps: Decimal,
    ) -> Self {
        Self::new(user_fees.taker_bps, slippage_bps, min_edge_bps)
    }

    /// Create with default fees.
    pub fn with_defaults(slippage_bps: Decimal, min_edge_bps: Decimal) -> Self {
        Self::new(DEFAULT_BASE_TAKER_FEE_BPS, slippage_bps, min_edge_bps)
    }
}

impl Default for FeeMetadata {
    fn default() -> Self {
        Self::new(
            DEFAULT_BASE_TAKER_FEE_BPS,
            Decimal::from(2), // Default slippage: 2 bps
            Decimal::from(5), // Default min edge: 5 bps
        )
    }
}

/// HIP-3 fee calculator.
///
/// Manages fee calculation with HIP-3 2x multiplier and user-specific rates.
#[derive(Debug, Clone)]
pub struct FeeCalculator {
    /// User's fee tier information.
    user_fees: UserFees,
    /// Slippage estimate in basis points.
    slippage_bps: Decimal,
    /// Minimum required edge in basis points.
    min_edge_bps: Decimal,
}

impl FeeCalculator {
    /// Create with user fees.
    pub fn new(user_fees: UserFees, slippage_bps: Decimal, min_edge_bps: Decimal) -> Self {
        Self {
            user_fees,
            slippage_bps,
            min_edge_bps,
        }
    }

    /// Create with default fees.
    pub fn with_defaults() -> Self {
        Self {
            user_fees: UserFees::default(),
            slippage_bps: Decimal::from(2),
            min_edge_bps: Decimal::from(5),
        }
    }

    /// Update user fees (e.g., after REST fetch).
    pub fn update_user_fees(&mut self, user_fees: UserFees) {
        self.user_fees = user_fees;
    }

    /// Get current user fees.
    pub fn user_fees(&self) -> &UserFees {
        &self.user_fees
    }

    /// Calculate effective taker fee with HIP-3 2x multiplier.
    pub fn effective_taker_fee_bps(&self) -> Decimal {
        self.user_fees.taker_bps * HIP3_FEE_MULTIPLIER
    }

    /// Calculate total cost (fee + slippage + min edge).
    pub fn total_cost_bps(&self) -> Decimal {
        self.effective_taker_fee_bps() + self.slippage_bps + self.min_edge_bps
    }

    /// Generate fee metadata for audit trail.
    pub fn metadata(&self) -> FeeMetadata {
        FeeMetadata::from_user_fees(&self.user_fees, self.slippage_bps, self.min_edge_bps)
    }

    /// Calculate buy threshold multiplier.
    /// Buy when: ask <= oracle * (1 - threshold/10000)
    pub fn buy_threshold(&self) -> Decimal {
        Decimal::ONE - self.total_cost_bps() / Decimal::from(10000)
    }

    /// Calculate sell threshold multiplier.
    /// Sell when: bid >= oracle * (1 + threshold/10000)
    pub fn sell_threshold(&self) -> Decimal {
        Decimal::ONE + self.total_cost_bps() / Decimal::from(10000)
    }

    /// Calculate net edge after costs.
    pub fn net_edge_bps(&self, raw_edge_bps: Decimal) -> Decimal {
        raw_edge_bps - self.total_cost_bps()
    }
}

impl Default for FeeCalculator {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_hip3_fee_multiplier() {
        let metadata = FeeMetadata::default();
        // Base: 2 bps, HIP-3 2x = 4 bps effective
        assert_eq!(metadata.base_taker_fee_bps, dec!(2));
        assert_eq!(metadata.hip3_multiplier, dec!(2));
        assert_eq!(metadata.effective_taker_fee_bps, dec!(4));
    }

    #[test]
    fn test_total_cost_calculation() {
        let metadata = FeeMetadata::new(
            dec!(2), // base taker: 2 bps
            dec!(2), // slippage: 2 bps
            dec!(5), // min edge: 5 bps
        );

        // Effective taker = 2 * 2 = 4 bps
        // Total = 4 + 2 + 5 = 11 bps
        assert_eq!(metadata.effective_taker_fee_bps, dec!(4));
        assert_eq!(metadata.total_cost_bps, dec!(11));
    }

    #[test]
    fn test_fee_calculator_thresholds() {
        let calc = FeeCalculator::with_defaults();

        // Total cost: 4 + 2 + 5 = 11 bps
        assert_eq!(calc.total_cost_bps(), dec!(11));

        // Buy threshold: 1 - 0.0011 = 0.9989
        assert_eq!(calc.buy_threshold(), dec!(0.9989));

        // Sell threshold: 1 + 0.0011 = 1.0011
        assert_eq!(calc.sell_threshold(), dec!(1.0011));
    }

    #[test]
    fn test_fee_calculator_with_vip_fees() {
        let vip_fees = UserFees {
            taker_bps: dec!(1.5), // VIP rate: 1.5 bps
            maker_bps: dec!(0.5),
            is_vip: true,
            tier: "vip1".to_string(),
        };

        let calc = FeeCalculator::new(vip_fees, dec!(2), dec!(5));

        // Effective taker = 1.5 * 2 = 3 bps
        // Total = 3 + 2 + 5 = 10 bps
        assert_eq!(calc.effective_taker_fee_bps(), dec!(3));
        assert_eq!(calc.total_cost_bps(), dec!(10));
    }

    #[test]
    fn test_net_edge_calculation() {
        let calc = FeeCalculator::with_defaults();

        // Total cost: 11 bps
        // Raw edge: 20 bps
        // Net edge: 20 - 11 = 9 bps
        assert_eq!(calc.net_edge_bps(dec!(20)), dec!(9));

        // Raw edge: 10 bps (below total cost)
        // Net edge: 10 - 11 = -1 bps (not profitable)
        assert_eq!(calc.net_edge_bps(dec!(10)), dec!(-1));
    }

    #[test]
    fn test_fee_metadata_audit_trail() {
        let user_fees = UserFees {
            taker_bps: dec!(3),
            maker_bps: dec!(1),
            is_vip: false,
            tier: "tier2".to_string(),
        };

        let metadata = FeeMetadata::from_user_fees(&user_fees, dec!(2), dec!(5));

        // Verify all components are captured
        assert_eq!(metadata.base_taker_fee_bps, dec!(3));
        assert_eq!(metadata.hip3_multiplier, dec!(2));
        assert_eq!(metadata.effective_taker_fee_bps, dec!(6)); // 3 * 2
        assert_eq!(metadata.slippage_bps, dec!(2));
        assert_eq!(metadata.min_edge_bps, dec!(5));
        assert_eq!(metadata.total_cost_bps, dec!(13)); // 6 + 2 + 5
    }

    #[test]
    fn test_user_fees_default() {
        let fees = UserFees::default();
        assert_eq!(fees.taker_bps, dec!(2));
        assert_eq!(fees.tier, "default");
        assert!(!fees.is_vip);
    }

    /// P0-4: Test from_effective_taker_bps derives correct base fee.
    #[test]
    fn test_user_fees_from_effective_taker_bps() {
        // Effective 4 bps = base 2 bps * 2x
        let fees = UserFees::from_effective_taker_bps(dec!(4));
        assert_eq!(fees.taker_bps, dec!(2));
        assert_eq!(fees.tier, "config");

        // Effective 6 bps = base 3 bps * 2x
        let fees2 = UserFees::from_effective_taker_bps(dec!(6));
        assert_eq!(fees2.taker_bps, dec!(3));

        // Effective 10 bps = base 5 bps * 2x
        let fees3 = UserFees::from_effective_taker_bps(dec!(10));
        assert_eq!(fees3.taker_bps, dec!(5));
    }

    /// P0-4: Test effective -> base -> effective round trip.
    #[test]
    fn test_user_fees_effective_roundtrip() {
        let effective_bps = dec!(8);
        let fees = UserFees::from_effective_taker_bps(effective_bps);

        // Create FeeCalculator and verify effective fee matches original
        let calc = FeeCalculator::new(fees, dec!(0), dec!(0));
        assert_eq!(calc.effective_taker_fee_bps(), effective_bps);
    }
}
