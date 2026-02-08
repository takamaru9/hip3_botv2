//! Inventory tracking for market making.
//!
//! Tracks net position per market and computes inventory ratio
//! for skew calculations.

use std::collections::HashMap;

use hip3_core::{MarketKey, OrderSide, Price, Size};
use rust_decimal::prelude::Signed;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Inventory state for a single market.
#[derive(Debug, Clone)]
pub struct MarketInventory {
    /// Net position size (positive = long, negative = short).
    pub net_size: Decimal,
    /// Average entry price of current inventory.
    pub avg_entry: Decimal,
    /// Total number of fills processed.
    pub fill_count: u64,
    /// Realized PnL in USD.
    pub realized_pnl: Decimal,
}

impl Default for MarketInventory {
    fn default() -> Self {
        Self {
            net_size: Decimal::ZERO,
            avg_entry: Decimal::ZERO,
            fill_count: 0,
            realized_pnl: Decimal::ZERO,
        }
    }
}

/// Manages inventory across all MM markets.
#[derive(Debug)]
pub struct InventoryManager {
    /// Per-market inventory.
    inventories: HashMap<MarketKey, MarketInventory>,
    /// Maximum allowed position per market in USD.
    max_position_usd: Decimal,
}

impl InventoryManager {
    /// Create a new inventory manager.
    pub fn new(max_position_usd: Decimal) -> Self {
        Self {
            inventories: HashMap::new(),
            max_position_usd,
        }
    }

    /// Record a fill and update inventory.
    pub fn record_fill(&mut self, market: MarketKey, side: OrderSide, price: Price, size: Size) {
        let inv = self.inventories.entry(market).or_default();
        let fill_size = size.inner();
        let fill_price = price.inner();

        let signed_size = match side {
            OrderSide::Buy => fill_size,
            OrderSide::Sell => -fill_size,
        };

        let old_size = inv.net_size;
        let new_size = old_size + signed_size;

        // Calculate realized PnL when reducing position
        if (old_size > Decimal::ZERO && signed_size < Decimal::ZERO)
            || (old_size < Decimal::ZERO && signed_size > Decimal::ZERO)
        {
            // Reducing position: realize PnL
            let reduce_amount = signed_size.abs().min(old_size.abs());
            let pnl = if old_size > Decimal::ZERO {
                // Was long, selling: pnl = (fill_price - avg_entry) * reduce_amount
                (fill_price - inv.avg_entry) * reduce_amount
            } else {
                // Was short, buying: pnl = (avg_entry - fill_price) * reduce_amount
                (inv.avg_entry - fill_price) * reduce_amount
            };
            inv.realized_pnl += pnl;
        }

        // Update average entry for the remaining or new position
        if new_size.is_zero() {
            inv.avg_entry = Decimal::ZERO;
        } else if new_size.signum() != old_size.signum() && !old_size.is_zero() {
            // Position flipped: new avg entry is fill price
            inv.avg_entry = fill_price;
        } else if new_size.signum() == signed_size.signum() || old_size.is_zero() {
            // Adding to position or new position: weighted average
            let old_notional = old_size.abs() * inv.avg_entry;
            let new_notional = fill_size * fill_price;
            let total_size = new_size.abs();
            if !total_size.is_zero() {
                inv.avg_entry = (old_notional + new_notional) / total_size;
            }
        }
        // else: reducing position, avg_entry stays the same

        inv.net_size = new_size;
        inv.fill_count += 1;
    }

    /// Get the inventory ratio for a market.
    ///
    /// Returns a value between -1.0 and 1.0 representing the inventory
    /// as a fraction of max_position_usd.
    ///
    /// Requires mark_px to convert size to USD notional.
    pub fn inventory_ratio(&self, market: &MarketKey, mark_px: Price) -> Decimal {
        let inv = match self.inventories.get(market) {
            Some(inv) => inv,
            None => return Decimal::ZERO,
        };

        if self.max_position_usd.is_zero() || mark_px.inner().is_zero() {
            return Decimal::ZERO;
        }

        let notional = inv.net_size * mark_px.inner();
        (notional / self.max_position_usd)
            .max(dec!(-1))
            .min(dec!(1))
    }

    /// Get inventory for a market.
    pub fn get(&self, market: &MarketKey) -> Option<&MarketInventory> {
        self.inventories.get(market)
    }

    /// Get net size for a market.
    pub fn net_size(&self, market: &MarketKey) -> Decimal {
        self.inventories
            .get(market)
            .map(|inv| inv.net_size)
            .unwrap_or(Decimal::ZERO)
    }

    /// Get notional value of inventory for a market.
    pub fn notional_usd(&self, market: &MarketKey, mark_px: Price) -> Decimal {
        self.net_size(market) * mark_px.inner()
    }

    /// Check if adding a fill would exceed max position.
    pub fn would_exceed_max(
        &self,
        market: &MarketKey,
        side: OrderSide,
        size: Size,
        mark_px: Price,
    ) -> bool {
        let current = self.net_size(market);
        let delta = match side {
            OrderSide::Buy => size.inner(),
            OrderSide::Sell => -size.inner(),
        };
        let projected = (current + delta) * mark_px.inner();
        projected.abs() > self.max_position_usd
    }

    /// Reset inventory for all markets.
    pub fn reset(&mut self) {
        self.inventories.clear();
    }

    /// Iterate over all market inventories.
    pub fn iter(&self) -> impl Iterator<Item = (&MarketKey, &MarketInventory)> {
        self.inventories.iter()
    }

    /// Get total realized PnL across all markets.
    pub fn total_realized_pnl(&self) -> Decimal {
        self.inventories.values().map(|inv| inv.realized_pnl).sum()
    }

    /// Get total unrealized PnL across all markets.
    pub fn total_unrealized_pnl<F>(&self, get_mark_px: F) -> Decimal
    where
        F: Fn(&MarketKey) -> Option<Price>,
    {
        self.inventories
            .iter()
            .filter_map(|(market, inv)| {
                if inv.net_size.is_zero() {
                    return None;
                }
                let mark = get_mark_px(market)?;
                let unrealized = if inv.net_size > Decimal::ZERO {
                    (mark.inner() - inv.avg_entry) * inv.net_size
                } else {
                    (inv.avg_entry - mark.inner()) * inv.net_size.abs()
                };
                Some(unrealized)
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{AssetId, DexId};
    use rust_decimal_macros::dec;

    fn mk() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    #[test]
    fn test_buy_creates_long_inventory() {
        let mut mgr = InventoryManager::new(dec!(100));
        mgr.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(50)),
            Size::new(dec!(1)),
        );

        assert_eq!(mgr.net_size(&mk()), dec!(1));
        assert_eq!(mgr.get(&mk()).unwrap().avg_entry, dec!(50));
    }

    #[test]
    fn test_sell_creates_short_inventory() {
        let mut mgr = InventoryManager::new(dec!(100));
        mgr.record_fill(
            mk(),
            OrderSide::Sell,
            Price::new(dec!(50)),
            Size::new(dec!(1)),
        );

        assert_eq!(mgr.net_size(&mk()), dec!(-1));
        assert_eq!(mgr.get(&mk()).unwrap().avg_entry, dec!(50));
    }

    #[test]
    fn test_buy_then_sell_closes_position() {
        let mut mgr = InventoryManager::new(dec!(100));
        mgr.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(50)),
            Size::new(dec!(1)),
        );
        mgr.record_fill(
            mk(),
            OrderSide::Sell,
            Price::new(dec!(52)),
            Size::new(dec!(1)),
        );

        assert_eq!(mgr.net_size(&mk()), dec!(0));
        // PnL = (52 - 50) * 1 = 2
        assert_eq!(mgr.get(&mk()).unwrap().realized_pnl, dec!(2));
    }

    #[test]
    fn test_short_then_buy_pnl() {
        let mut mgr = InventoryManager::new(dec!(100));
        mgr.record_fill(
            mk(),
            OrderSide::Sell,
            Price::new(dec!(52)),
            Size::new(dec!(1)),
        );
        mgr.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(50)),
            Size::new(dec!(1)),
        );

        assert_eq!(mgr.net_size(&mk()), dec!(0));
        // Short PnL = (52 - 50) * 1 = 2
        assert_eq!(mgr.get(&mk()).unwrap().realized_pnl, dec!(2));
    }

    #[test]
    fn test_losing_trade_pnl() {
        let mut mgr = InventoryManager::new(dec!(100));
        mgr.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(50)),
            Size::new(dec!(1)),
        );
        mgr.record_fill(
            mk(),
            OrderSide::Sell,
            Price::new(dec!(48)),
            Size::new(dec!(1)),
        );

        // PnL = (48 - 50) * 1 = -2
        assert_eq!(mgr.get(&mk()).unwrap().realized_pnl, dec!(-2));
    }

    #[test]
    fn test_inventory_ratio() {
        let mut mgr = InventoryManager::new(dec!(100));
        mgr.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(50)),
            Size::new(dec!(1)),
        );

        // Notional = 1 * 50 = 50 USD, ratio = 50/100 = 0.5
        let ratio = mgr.inventory_ratio(&mk(), Price::new(dec!(50)));
        assert_eq!(ratio, dec!(0.5));
    }

    #[test]
    fn test_inventory_ratio_clamped() {
        let mut mgr = InventoryManager::new(dec!(50));
        mgr.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(50)),
            Size::new(dec!(2)),
        );

        // Notional = 2 * 50 = 100, ratio = 100/50 = 2.0 → clamped to 1.0
        let ratio = mgr.inventory_ratio(&mk(), Price::new(dec!(50)));
        assert_eq!(ratio, dec!(1));
    }

    #[test]
    fn test_would_exceed_max() {
        let mut mgr = InventoryManager::new(dec!(100));
        mgr.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(50)),
            Size::new(dec!(1.5)),
        );

        // Current notional = 1.5 * 50 = 75
        // Adding 1 more at 50 = 75 + 50 = 125 > 100
        assert!(mgr.would_exceed_max(
            &mk(),
            OrderSide::Buy,
            Size::new(dec!(1)),
            Price::new(dec!(50))
        ));

        // Selling should reduce, not exceed
        assert!(!mgr.would_exceed_max(
            &mk(),
            OrderSide::Sell,
            Size::new(dec!(1)),
            Price::new(dec!(50))
        ));
    }

    #[test]
    fn test_avg_entry_weighted() {
        let mut mgr = InventoryManager::new(dec!(1000));
        mgr.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(100)),
            Size::new(dec!(1)),
        );
        mgr.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(110)),
            Size::new(dec!(1)),
        );

        // avg = (1*100 + 1*110) / 2 = 105
        assert_eq!(mgr.get(&mk()).unwrap().avg_entry, dec!(105));
        assert_eq!(mgr.net_size(&mk()), dec!(2));
    }

    #[test]
    fn test_total_realized_pnl() {
        let mk2 = MarketKey::new(DexId::XYZ, AssetId::new(1));
        let mut mgr = InventoryManager::new(dec!(1000));

        mgr.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(100)),
            Size::new(dec!(1)),
        );
        mgr.record_fill(
            mk(),
            OrderSide::Sell,
            Price::new(dec!(105)),
            Size::new(dec!(1)),
        );

        mgr.record_fill(
            mk2,
            OrderSide::Sell,
            Price::new(dec!(200)),
            Size::new(dec!(1)),
        );
        mgr.record_fill(
            mk2,
            OrderSide::Buy,
            Price::new(dec!(195)),
            Size::new(dec!(1)),
        );

        // Market 1: +5, Market 2: +5 = Total: +10
        assert_eq!(mgr.total_realized_pnl(), dec!(10));
    }
}
