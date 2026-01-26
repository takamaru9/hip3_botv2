//! Price provider implementation for TimeStopMonitor.
//!
//! Adapts `MarketStateCache` to the `PriceProvider` trait required by
//! hip3_position's TimeStopMonitor.

use std::sync::Arc;

use hip3_core::{MarketKey, Price};
use hip3_position::time_stop::PriceProvider;

use crate::executor::MarketStateCache;

/// Adapts `MarketStateCache` to `PriceProvider` for TimeStopMonitor.
///
/// Returns mark price for the requested market. BBO is not available
/// in `MarketStateCache`, so mark price is used as the best available
/// approximation for flatten order pricing.
///
/// # Note
///
/// Mark price may differ from BBO mid price by up to MarkMidDivergence
/// threshold (typically 50-110 bps). This is acceptable for flatten
/// orders since they use slippage tolerance anyway.
pub struct MarkPriceProvider {
    market_state_cache: Arc<MarketStateCache>,
}

impl MarkPriceProvider {
    /// Create a new `MarkPriceProvider`.
    #[must_use]
    pub fn new(market_state_cache: Arc<MarketStateCache>) -> Self {
        Self { market_state_cache }
    }
}

impl PriceProvider for MarkPriceProvider {
    fn get_price(&self, market: &MarketKey) -> Option<Price> {
        self.market_state_cache.get_mark_px(market)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetId, DexId};
    use rust_decimal_macros::dec;

    fn sample_market() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    fn sample_market_2() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(1))
    }

    #[test]
    fn test_mark_price_provider_returns_price() {
        let cache = Arc::new(MarketStateCache::new());
        let market = sample_market();

        // Set mark price
        cache.update(&market, Price::new(dec!(50000)), 1234567890);

        let provider = MarkPriceProvider::new(cache);

        // Should return the mark price
        let price = provider.get_price(&market);
        assert_eq!(price, Some(Price::new(dec!(50000))));
    }

    #[test]
    fn test_mark_price_provider_none_for_unknown() {
        let cache = Arc::new(MarketStateCache::new());
        let provider = MarkPriceProvider::new(cache);

        // Unknown market should return None
        let price = provider.get_price(&sample_market());
        assert!(price.is_none());
    }

    #[test]
    fn test_mark_price_provider_multiple_markets() {
        let cache = Arc::new(MarketStateCache::new());
        let market1 = sample_market();
        let market2 = sample_market_2();

        // Set different prices for different markets
        cache.update(&market1, Price::new(dec!(50000)), 1234567890);
        cache.update(&market2, Price::new(dec!(3000)), 1234567890);

        let provider = MarkPriceProvider::new(cache);

        assert_eq!(provider.get_price(&market1), Some(Price::new(dec!(50000))));
        assert_eq!(provider.get_price(&market2), Some(Price::new(dec!(3000))));
    }
}
