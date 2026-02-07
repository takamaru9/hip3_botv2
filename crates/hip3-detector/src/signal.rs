//! Dislocation signal types.

use crate::fee::FeeMetadata;
use chrono::{DateTime, Utc};
use hip3_core::{MarketKey, OrderSide, Price, Size};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Signal strength indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignalStrength {
    /// Edge just above threshold.
    Weak,
    /// Edge significantly above threshold.
    Medium,
    /// Edge very high (possible data issue or flash opportunity).
    Strong,
}

impl SignalStrength {
    /// Classify edge into signal strength.
    pub fn from_edge(edge_bps: Decimal, threshold_bps: Decimal) -> Option<Self> {
        if edge_bps < threshold_bps {
            return None;
        }

        let excess = edge_bps - threshold_bps;
        if excess < Decimal::from(5) {
            Some(Self::Weak)
        } else if excess < Decimal::from(15) {
            Some(Self::Medium)
        } else {
            Some(Self::Strong)
        }
    }
}

/// A detected dislocation opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DislocationSignal {
    /// Market where dislocation was detected.
    pub market_key: MarketKey,
    /// Trade side (buy when ask < oracle, sell when bid > oracle).
    pub side: OrderSide,
    /// Edge in basis points (raw, before costs).
    pub raw_edge_bps: Decimal,
    /// Edge after accounting for fees and slippage.
    pub net_edge_bps: Decimal,
    /// Signal strength classification.
    pub strength: SignalStrength,
    /// Suggested size.
    pub suggested_size: Size,
    /// Oracle price at detection.
    pub oracle_px: Price,
    /// Best price at detection (ask for buy, bid for sell).
    pub best_px: Price,
    /// Top-of-book size.
    pub book_size: Size,
    /// Detection timestamp.
    pub detected_at: DateTime<Utc>,
    /// Unique signal ID for tracking.
    pub signal_id: String,
    /// Fee calculation details for audit trail (P0-24).
    pub fee_metadata: FeeMetadata,
    /// Oracle velocity at detection time in bps (P2-1).
    /// Absolute change from previous tick. Used for velocity-based sizing.
    #[serde(default)]
    pub oracle_velocity_bps: Decimal,
    /// Multi-factor confidence score (P3-1).
    /// Range: 0.0-1.0. Higher = more reliable signal.
    /// Used for confidence-based sizing when enabled.
    #[serde(default)]
    pub confidence_score: Decimal,
    /// Structural oracle-quote gap for this market (Sprint 2).
    /// Signed bps: positive = oracle typically above mid, negative = below.
    /// Only populated when baseline_tracking is enabled.
    #[serde(default)]
    pub baseline_gap_bps: Decimal,
    /// Edge remaining after subtracting structural baseline (Sprint 2).
    /// `raw_edge_bps - baseline_adjustment`. Represents "genuine" edge.
    /// Only populated when baseline_tracking is enabled.
    #[serde(default)]
    pub edge_above_baseline_bps: Decimal,
}

impl DislocationSignal {
    /// Create a new dislocation signal.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        market_key: MarketKey,
        side: OrderSide,
        raw_edge_bps: Decimal,
        net_edge_bps: Decimal,
        strength: SignalStrength,
        suggested_size: Size,
        oracle_px: Price,
        best_px: Price,
        book_size: Size,
        fee_metadata: FeeMetadata,
        oracle_velocity_bps: Decimal,
        confidence_score: Decimal,
    ) -> Self {
        let now = Utc::now();
        let signal_id = format!("sig_{}_{}_{}", market_key, side, now.timestamp_millis());

        Self {
            market_key,
            side,
            raw_edge_bps,
            net_edge_bps,
            strength,
            suggested_size,
            oracle_px,
            best_px,
            book_size,
            detected_at: now,
            signal_id,
            fee_metadata,
            oracle_velocity_bps,
            confidence_score,
            baseline_gap_bps: Decimal::ZERO,
            edge_above_baseline_bps: Decimal::ZERO,
        }
    }

    /// Get expected PnL in basis points (simplified).
    pub fn expected_pnl_bps(&self) -> Decimal {
        self.net_edge_bps
    }

    /// Get notional value of suggested trade.
    pub fn notional(&self) -> Decimal {
        self.suggested_size.notional(self.oracle_px)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_signal_strength_classification() {
        let threshold = dec!(10);

        assert!(SignalStrength::from_edge(dec!(5), threshold).is_none());
        assert_eq!(
            SignalStrength::from_edge(dec!(12), threshold),
            Some(SignalStrength::Weak)
        );
        assert_eq!(
            SignalStrength::from_edge(dec!(20), threshold),
            Some(SignalStrength::Medium)
        );
        assert_eq!(
            SignalStrength::from_edge(dec!(30), threshold),
            Some(SignalStrength::Strong)
        );
    }
}
