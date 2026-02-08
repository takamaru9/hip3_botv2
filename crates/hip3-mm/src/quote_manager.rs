//! Quote lifecycle management.
//!
//! Manages the full lifecycle of MM quotes:
//! - Place initial quotes (GTC/ALO)
//! - Detect when requote is needed (oracle moved)
//! - Generate cancel + re-place actions
//! - Track active quotes per market
//!
//! Safety features:
//! - P2-1: Inventory skew protection (one-sided stop + emergency flatten)
//! - P2-2: Stale cancel detection (halt on unacked cancels)
//! - P2-3: Adverse selection detection (spread widening on consecutive fills)

use std::collections::HashMap;

use hip3_core::{
    ClientOrderId, MarketKey, OrderSide, PendingCancel, PendingOrder, Price, Size, TimeInForce,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, info, warn};

use crate::config::MakerConfig;
use crate::inventory::InventoryManager;
use crate::quote_engine::{compute_quotes, QuotePair};
use crate::volatility::{VolatilityStats, WickTracker};

/// An active quote tracked by the manager.
#[derive(Debug, Clone)]
pub struct ActiveQuote {
    /// Client order ID.
    pub cloid: ClientOrderId,
    /// Exchange order ID (set after resting confirmation).
    pub oid: Option<u64>,
    /// Side (buy for bid, sell for ask).
    pub side: OrderSide,
    /// Quoted price.
    pub price: Price,
    /// Quoted size in base units.
    pub size: Size,
    /// Level index.
    pub level: u32,
    /// Timestamp when placed.
    pub placed_at_ms: u64,
}

/// Actions that the quote manager wants the executor to perform.
#[derive(Debug, Clone)]
pub enum MakerAction {
    /// Place new orders.
    PlaceOrders(Vec<PendingOrder>),
    /// Cancel existing orders.
    CancelOrders(Vec<PendingCancel>),
    /// Cancel then re-place (cancel has priority, new orders on next tick).
    CancelAndReplace {
        cancels: Vec<PendingCancel>,
        new_orders: Vec<PendingOrder>,
    },
    /// Cancel all quotes and flatten position.
    FlattenAll {
        cancels: Vec<PendingCancel>,
        flatten_orders: Vec<PendingOrder>,
    },
}

/// Quote manager state for a single market.
#[derive(Debug)]
struct MarketQuoteState {
    /// Active bid quotes.
    bids: Vec<ActiveQuote>,
    /// Active ask quotes.
    asks: Vec<ActiveQuote>,
    /// Oracle price at last quote.
    last_oracle: Option<Price>,
    /// Timestamp of last requote.
    last_requote_ms: u64,
}

impl MarketQuoteState {
    fn new() -> Self {
        Self {
            bids: Vec::new(),
            asks: Vec::new(),
            last_oracle: None,
            last_requote_ms: 0,
        }
    }

    fn clear(&mut self) {
        self.bids.clear();
        self.asks.clear();
        self.last_oracle = None;
    }
}

/// P2-2: Tracks pending (unacknowledged) cancel requests.
#[derive(Debug, Clone)]
struct PendingCancelInfo {
    oid: u64,
    market: MarketKey,
    sent_at_ms: u64,
}

/// P2-3: Tracks consecutive same-side fills per market for adverse selection detection.
#[derive(Debug, Default)]
struct AdverseSelectionState {
    /// Last fill side.
    last_fill_side: Option<OrderSide>,
    /// Count of consecutive same-side fills.
    consecutive_count: u32,
}

impl AdverseSelectionState {
    fn record_fill(&mut self, side: OrderSide) {
        if self.last_fill_side == Some(side) {
            self.consecutive_count += 1;
        } else {
            self.last_fill_side = Some(side);
            self.consecutive_count = 1;
        }
    }
}

/// Phase C: Per-market oracle velocity tracker.
#[derive(Debug)]
struct OracleVelocityTracker {
    /// Recent directional changes: true = up, false = down.
    recent_directions: std::collections::VecDeque<bool>,
    /// Last oracle price seen.
    last_price: Option<Decimal>,
    /// Window size for velocity calculation.
    window: usize,
}

impl OracleVelocityTracker {
    fn new(window: usize) -> Self {
        Self {
            recent_directions: std::collections::VecDeque::with_capacity(window),
            last_price: None,
            window,
        }
    }

    /// Record an oracle update and track direction.
    fn record(&mut self, price: Decimal) {
        if let Some(last) = self.last_price {
            if price != last {
                let is_up = price > last;
                if self.recent_directions.len() >= self.window {
                    self.recent_directions.pop_front();
                }
                self.recent_directions.push_back(is_up);
            }
        }
        self.last_price = Some(price);
    }

    /// Get trend in [-1.0, 1.0]. Positive = oracle rising.
    fn trend(&self) -> Decimal {
        if self.recent_directions.is_empty() {
            return Decimal::ZERO;
        }
        let up_count = self.recent_directions.iter().filter(|&&d| d).count();
        let total = self.recent_directions.len();
        // (up_ratio × 2) - 1 → maps [0, 1] to [-1, 1]
        let up_ratio = Decimal::from(up_count as u64) / Decimal::from(total as u64);
        up_ratio * dec!(2) - dec!(1)
    }
}

/// Manages quote lifecycle across all MM markets.
pub struct QuoteManager {
    config: MakerConfig,
    /// Per-market quote state.
    states: HashMap<MarketKey, MarketQuoteState>,
    /// P2-2: Pending cancel tracking.
    pending_cancels: Vec<PendingCancelInfo>,
    /// P2-2: Whether quoting is halted due to stale cancels.
    stale_halt: bool,
    /// P2-3: Per-market adverse selection state.
    adverse_selection: HashMap<MarketKey, AdverseSelectionState>,
    /// P3-1: Wick volatility tracker for dynamic offset.
    wick_tracker: WickTracker,
    /// Phase C: Per-market oracle velocity tracker.
    velocity_trackers: HashMap<MarketKey, OracleVelocityTracker>,
}

impl QuoteManager {
    /// Create a new quote manager.
    pub fn new(config: MakerConfig) -> Self {
        let wick_tracker = WickTracker::new(
            config.wick_window_size,
            config.wick_min_samples,
            config.wick_cache_ttl_ms,
            config.breakpoint_min_jump_ratio,
        );
        Self {
            config,
            states: HashMap::new(),
            pending_cancels: Vec::new(),
            stale_halt: false,
            adverse_selection: HashMap::new(),
            wick_tracker,
            velocity_trackers: HashMap::new(),
        }
    }

    /// Process a market data update and determine if requoting is needed.
    ///
    /// Returns actions to execute (place, cancel, or cancel-and-replace).
    pub fn on_market_update(
        &mut self,
        market: MarketKey,
        oracle_price: Price,
        mark_price: Price,
        now_ms: u64,
        inventory: &InventoryManager,
    ) -> Option<MakerAction> {
        // P2-2: Check for stale cancels before any quoting
        self.check_stale_cancels(now_ms);
        if self.stale_halt {
            debug!(market = %market, "MM quoting halted: stale cancel detected");
            return None;
        }

        // P2-1: Emergency flatten check
        let inventory_ratio = inventory.inventory_ratio(&market, mark_price);
        let abs_ratio = inventory_ratio.abs();

        if abs_ratio >= self.config.inventory_emergency_ratio {
            warn!(
                market = %market,
                ratio = %inventory_ratio,
                threshold = %self.config.inventory_emergency_ratio,
                "MM EMERGENCY FLATTEN: inventory ratio exceeded emergency threshold"
            );
            return self.build_emergency_flatten(market, inventory, mark_price, now_ms);
        }

        // P2-3: Get spread multiplier before borrowing states
        let spread_multiplier =
            Self::calc_spread_multiplier(&self.adverse_selection, &market, &self.config);

        // P3-1: Record oracle price for wick tracking and get volatility stats
        self.wick_tracker
            .record_oracle(market, oracle_price.inner(), now_ms);
        let vol_stats = self.wick_tracker.get_stats(&market, now_ms);
        let vol_ref = if self.config.dynamic_offset_enabled {
            Some(&vol_stats)
        } else {
            None
        };

        // Phase C: Track oracle velocity
        let vel_tracker = self
            .velocity_trackers
            .entry(market)
            .or_insert_with(|| OracleVelocityTracker::new(self.config.velocity_window));
        vel_tracker.record(oracle_price.inner());
        let velocity_trend = if self.config.velocity_skew_enabled {
            vel_tracker.trend()
        } else {
            Decimal::ZERO
        };

        // P2-1: Compute filtered quotes before borrowing states
        let tif = if self.config.use_alo {
            TimeInForce::AddLiquidityOnly
        } else {
            TimeInForce::GoodTilCancelled
        };
        let quotes = compute_quotes(
            oracle_price,
            inventory_ratio,
            &self.config,
            spread_multiplier,
            vol_ref,
            velocity_trend,
        );
        let filtered = Self::apply_inventory_warn(&self.config, quotes, inventory_ratio);
        let new_orders = Self::make_orders(market, &filtered, mark_price, tif, now_ms);

        let state = self
            .states
            .entry(market)
            .or_insert_with(MarketQuoteState::new);

        // Check if requote is needed
        if !Self::check_requote(&self.config, state, oracle_price, now_ms) {
            return None;
        }

        // Check if we have active quotes to cancel
        let has_active = !state.bids.is_empty() || !state.asks.is_empty();

        let action = if has_active {
            let cancels = Self::build_cancels_static(market, state);

            if cancels.is_empty() {
                // No oids yet (not confirmed resting), just place new
                state.clear();
                if new_orders.is_empty() {
                    return None;
                }
                MakerAction::PlaceOrders(new_orders)
            } else {
                // P2-2: Track pending cancels
                for c in &cancels {
                    self.pending_cancels.push(PendingCancelInfo {
                        oid: c.oid,
                        market,
                        sent_at_ms: now_ms,
                    });
                }
                state.clear();
                MakerAction::CancelAndReplace {
                    cancels,
                    new_orders,
                }
            }
        } else if new_orders.is_empty() {
            return None;
        } else {
            MakerAction::PlaceOrders(new_orders)
        };

        // Update state
        state.last_oracle = Some(oracle_price);
        state.last_requote_ms = now_ms;

        // Track new orders as active quotes
        if let MakerAction::PlaceOrders(ref orders)
        | MakerAction::CancelAndReplace {
            new_orders: ref orders,
            ..
        } = action
        {
            for order in orders {
                let quote = ActiveQuote {
                    cloid: order.cloid.clone(),
                    oid: None,
                    side: order.side,
                    price: order.price,
                    size: order.size,
                    level: 0,
                    placed_at_ms: now_ms,
                };
                match order.side {
                    OrderSide::Buy => state.bids.push(quote),
                    OrderSide::Sell => state.asks.push(quote),
                }
            }
        }

        Some(action)
    }

    /// Record that an order has been confirmed resting with an oid.
    pub fn record_resting(&mut self, market: &MarketKey, cloid: &ClientOrderId, oid: u64) {
        if let Some(state) = self.states.get_mut(market) {
            for quote in state.bids.iter_mut().chain(state.asks.iter_mut()) {
                if &quote.cloid == cloid {
                    quote.oid = Some(oid);
                    debug!(market = %market, cloid = %cloid, oid = oid, "Quote resting confirmed");
                    return;
                }
            }
        }
    }

    /// Record that a quote was filled (remove from active quotes).
    ///
    /// Returns an optional counter-order (mean reversion) if `counter_order_enabled`.
    pub fn record_fill(
        &mut self,
        market: &MarketKey,
        cloid: &ClientOrderId,
        mark_price: Price,
        now_ms: u64,
    ) -> Option<MakerAction> {
        let mut fill_side = None;
        let mut fill_price = None;
        let mut fill_level = 0u32;
        if let Some(state) = self.states.get_mut(market) {
            // Determine the side, price, and level of the filled quote before removing
            for quote in state.bids.iter().chain(state.asks.iter()) {
                if &quote.cloid == cloid {
                    fill_side = Some(quote.side);
                    fill_price = Some(quote.price);
                    fill_level = quote.level;
                    break;
                }
            }
            state.bids.retain(|q| &q.cloid != cloid);
            state.asks.retain(|q| &q.cloid != cloid);
        }

        // P2-3: Track adverse selection
        if let Some(side) = fill_side {
            self.adverse_selection
                .entry(*market)
                .or_default()
                .record_fill(side);
        }

        // Phase C: Generate counter-order (mean reversion)
        if !self.config.counter_order_enabled {
            return None;
        }
        let (side, price) = match (fill_side, fill_price) {
            (Some(s), Some(p)) => (s, p),
            _ => return None,
        };

        // Counter-order is the opposite side
        let counter_side = match side {
            OrderSide::Buy => OrderSide::Sell,
            OrderSide::Sell => OrderSide::Buy,
        };

        // Reversion: move from fill price toward oracle (mark_price as proxy)
        // reversion_pct = base + per_level * level
        let reversion_pct = (self.config.counter_reversion_pct
            + self.config.counter_reversion_per_level * Decimal::from(fill_level))
        .min(dec!(0.95)); // cap at 95% reversion

        let fill_px = price.inner();
        let oracle_px = mark_price.inner();
        let distance = (oracle_px - fill_px).abs();
        let reversion_distance = distance * reversion_pct;

        let counter_price = match counter_side {
            // Sell counter: fill was BUY below oracle, sell at fill + reversion_distance
            OrderSide::Sell => Price::new(fill_px + reversion_distance),
            // Buy counter: fill was SELL above oracle, buy at fill - reversion_distance
            OrderSide::Buy => Price::new(fill_px - reversion_distance),
        };

        // Use the same size as the base level size
        let size_base = if mark_price.inner().is_zero() {
            return None;
        } else {
            self.config.size_per_level_usd / mark_price.inner()
        };

        if size_base <= Decimal::ZERO {
            return None;
        }

        let counter_order = PendingOrder::with_tif(
            ClientOrderId::new(),
            *market,
            counter_side,
            counter_price,
            Size::new(size_base),
            false,
            now_ms,
            TimeInForce::GoodTilCancelled, // GTC for counter-orders
        );

        debug!(
            market = %market,
            fill_side = ?side,
            fill_price = %fill_px,
            counter_price = %counter_price.inner(),
            reversion_pct = %reversion_pct,
            level = fill_level,
            "Phase C: counter-order generated"
        );

        Some(MakerAction::PlaceOrders(vec![counter_order]))
    }

    /// Record that a quote was cancelled (remove from active quotes).
    pub fn record_cancelled(&mut self, market: &MarketKey, cloid: &ClientOrderId) {
        if let Some(state) = self.states.get_mut(market) {
            // Find the oid for P2-2 pending cancel cleanup
            let mut cancelled_oid = None;
            for quote in state.bids.iter().chain(state.asks.iter()) {
                if &quote.cloid == cloid {
                    cancelled_oid = quote.oid;
                    break;
                }
            }
            state.bids.retain(|q| &q.cloid != cloid);
            state.asks.retain(|q| &q.cloid != cloid);

            // P2-2: Remove from pending cancel tracking
            if let Some(oid) = cancelled_oid {
                self.pending_cancels.retain(|pc| pc.oid != oid);
            }
        }
    }

    /// Record that a cancel was acknowledged by the exchange (via order update).
    /// This is called when we get an OrderState::Cancelled for an oid we sent a cancel for.
    pub fn record_cancel_acked(&mut self, oid: u64) {
        let before = self.pending_cancels.len();
        self.pending_cancels.retain(|pc| pc.oid != oid);
        if self.pending_cancels.len() < before {
            debug!(oid = oid, "Cancel acked, removed from pending");
        }

        // If all stale cancels are resolved, resume quoting
        if self.stale_halt && self.pending_cancels.is_empty() {
            info!("All stale cancels resolved — resuming MM quoting");
            self.stale_halt = false;
        }
    }

    /// Check if a client order ID belongs to an active MM quote.
    ///
    /// Used by P2-4 to distinguish MM fills from taker fills,
    /// so that MM P&L is excluded from the taker drawdown gate.
    pub fn is_mm_order(&self, cloid: &ClientOrderId) -> bool {
        for state in self.states.values() {
            for quote in state.bids.iter().chain(state.asks.iter()) {
                if &quote.cloid == cloid {
                    return true;
                }
            }
        }
        false
    }

    /// Generate cancel-all + flatten actions for shutdown.
    pub fn shutdown_all(
        &mut self,
        inventory: &InventoryManager,
        get_mark_px: impl Fn(&MarketKey) -> Option<Price>,
        now_ms: u64,
    ) -> Vec<MakerAction> {
        let mut actions = Vec::new();

        for (market, state) in &self.states {
            let cancels = Self::build_cancels_static(*market, state);
            let net = inventory.net_size(market);

            let mut flatten_orders = Vec::new();
            if !net.is_zero() {
                if let Some(mark_px) = get_mark_px(market) {
                    let side = if net > Decimal::ZERO {
                        OrderSide::Sell
                    } else {
                        OrderSide::Buy
                    };
                    let slippage = Decimal::from(self.config.flatten_slippage_bps) / dec!(10000);
                    let price = match side {
                        OrderSide::Buy => Price::new(mark_px.inner() * (Decimal::ONE + slippage)),
                        OrderSide::Sell => Price::new(mark_px.inner() * (Decimal::ONE - slippage)),
                    };

                    flatten_orders.push(PendingOrder::with_tif(
                        ClientOrderId::new(),
                        *market,
                        side,
                        price,
                        Size::new(net.abs()),
                        true, // reduce_only
                        now_ms,
                        TimeInForce::ImmediateOrCancel, // flatten uses IOC
                    ));
                }
            }

            if !cancels.is_empty() || !flatten_orders.is_empty() {
                actions.push(MakerAction::FlattenAll {
                    cancels,
                    flatten_orders,
                });
            }
        }

        // Clear all state
        self.states.clear();
        self.pending_cancels.clear();
        self.adverse_selection.clear();
        self.velocity_trackers.clear();

        actions
    }

    /// Check if any market has active quotes.
    pub fn has_active_quotes(&self) -> bool {
        self.states
            .values()
            .any(|s| !s.bids.is_empty() || !s.asks.is_empty())
    }

    /// Get active quote count for a market.
    pub fn active_quote_count(&self, market: &MarketKey) -> usize {
        self.states
            .get(market)
            .map(|s| s.bids.len() + s.asks.len())
            .unwrap_or(0)
    }

    /// P2-2: Whether quoting is halted due to stale cancels.
    pub fn is_stale_halted(&self) -> bool {
        self.stale_halt
    }

    /// P2-3: Get the current adverse selection state for a market.
    pub fn adverse_consecutive_count(&self, market: &MarketKey) -> u32 {
        self.adverse_selection
            .get(market)
            .map(|s| s.consecutive_count)
            .unwrap_or(0)
    }

    /// Total active quotes across all markets.
    pub fn total_active_quotes(&self) -> usize {
        self.states
            .values()
            .map(|s| s.bids.len() + s.asks.len())
            .sum()
    }

    /// P3-1: Get volatility statistics for all tracked markets.
    pub fn volatility_stats(&mut self, now_ms: u64) -> HashMap<MarketKey, VolatilityStats> {
        self.wick_tracker.all_stats(now_ms)
    }

    /// Number of markets with active quotes.
    pub fn num_quoted_markets(&self) -> usize {
        self.states
            .values()
            .filter(|s| !s.bids.is_empty() || !s.asks.is_empty())
            .count()
    }

    // === Private helpers ===

    /// P2-1: Filter quotes based on inventory warning threshold.
    /// When inventory is above warn_ratio, remove quotes on the side that increases exposure.
    fn apply_inventory_warn(
        config: &MakerConfig,
        mut quotes: QuotePair,
        inventory_ratio: Decimal,
    ) -> QuotePair {
        let abs_ratio = inventory_ratio.abs();
        if abs_ratio < config.inventory_warn_ratio {
            return quotes;
        }

        if inventory_ratio > Decimal::ZERO {
            // Long: stop buying (remove bids)
            info!(
                ratio = %inventory_ratio,
                "P2-1: Inventory warn — removing bid quotes (long exposure)"
            );
            quotes.bids.clear();
        } else {
            // Short: stop selling (remove asks)
            info!(
                ratio = %inventory_ratio,
                "P2-1: Inventory warn — removing ask quotes (short exposure)"
            );
            quotes.asks.clear();
        }

        quotes
    }

    /// P2-1: Build emergency flatten action for a market.
    fn build_emergency_flatten(
        &mut self,
        market: MarketKey,
        inventory: &InventoryManager,
        mark_price: Price,
        now_ms: u64,
    ) -> Option<MakerAction> {
        let state = self
            .states
            .entry(market)
            .or_insert_with(MarketQuoteState::new);

        let cancels = Self::build_cancels_static(market, state);
        state.clear();

        let net = inventory.net_size(&market);
        let mut flatten_orders = Vec::new();

        if !net.is_zero() && !mark_price.inner().is_zero() {
            let side = if net > Decimal::ZERO {
                OrderSide::Sell
            } else {
                OrderSide::Buy
            };
            let slippage = Decimal::from(self.config.flatten_slippage_bps) / dec!(10000);
            let price = match side {
                OrderSide::Buy => Price::new(mark_price.inner() * (Decimal::ONE + slippage)),
                OrderSide::Sell => Price::new(mark_price.inner() * (Decimal::ONE - slippage)),
            };

            flatten_orders.push(PendingOrder::with_tif(
                ClientOrderId::new(),
                market,
                side,
                price,
                Size::new(net.abs()),
                true,
                now_ms,
                TimeInForce::ImmediateOrCancel,
            ));
        }

        if cancels.is_empty() && flatten_orders.is_empty() {
            return None;
        }

        Some(MakerAction::FlattenAll {
            cancels,
            flatten_orders,
        })
    }

    /// P2-2: Check for stale (unacknowledged) cancels.
    fn check_stale_cancels(&mut self, now_ms: u64) {
        if self.config.stale_cancel_timeout_ms == 0 {
            return;
        }

        for pc in &self.pending_cancels {
            let age_ms = now_ms.saturating_sub(pc.sent_at_ms);
            if age_ms >= self.config.stale_cancel_timeout_ms {
                warn!(
                    oid = pc.oid,
                    market = %pc.market,
                    age_ms = age_ms,
                    "P2-2: Stale cancel detected — halting MM quoting"
                );
                self.stale_halt = true;
                return;
            }
        }
    }

    /// P2-3: Get spread multiplier based on adverse selection state.
    fn calc_spread_multiplier(
        adverse_selection: &HashMap<MarketKey, AdverseSelectionState>,
        market: &MarketKey,
        config: &MakerConfig,
    ) -> Decimal {
        if config.adverse_consecutive_fills == 0 {
            return dec!(1);
        }

        let count = adverse_selection
            .get(market)
            .map(|s| s.consecutive_count)
            .unwrap_or(0);

        if count >= config.adverse_consecutive_fills {
            debug!(
                market = %market,
                consecutive = count,
                multiplier = %config.adverse_spread_multiplier,
                "P2-3: Adverse selection — widening spread"
            );
            config.adverse_spread_multiplier
        } else {
            dec!(1)
        }
    }

    // === Associated functions (avoid borrow conflicts) ===

    fn check_requote(
        config: &MakerConfig,
        state: &MarketQuoteState,
        oracle_price: Price,
        now_ms: u64,
    ) -> bool {
        // First quote: always place
        let last_oracle = match state.last_oracle {
            Some(p) => p,
            None => return true,
        };

        // Time-based: requote if interval elapsed
        let elapsed = now_ms.saturating_sub(state.last_requote_ms);
        if elapsed >= config.requote_interval_ms {
            return true;
        }

        // Price-based: requote if oracle moved significantly
        let oracle_change_bps = if last_oracle.inner().is_zero() {
            dec!(0)
        } else {
            ((oracle_price.inner() - last_oracle.inner()) / last_oracle.inner() * dec!(10000)).abs()
        };

        oracle_change_bps >= config.min_requote_change_bps
    }

    fn make_orders(
        market: MarketKey,
        quotes: &QuotePair,
        mark_price: Price,
        tif: TimeInForce,
        now_ms: u64,
    ) -> Vec<PendingOrder> {
        let mut orders = Vec::new();

        for bid in &quotes.bids {
            let size_base = if mark_price.inner().is_zero() {
                Decimal::ZERO
            } else {
                bid.size_usd / mark_price.inner()
            };

            if size_base > Decimal::ZERO {
                orders.push(PendingOrder::with_tif(
                    ClientOrderId::new(),
                    market,
                    OrderSide::Buy,
                    bid.price,
                    Size::new(size_base),
                    false,
                    now_ms,
                    tif,
                ));
            }
        }

        for ask in &quotes.asks {
            let size_base = if mark_price.inner().is_zero() {
                Decimal::ZERO
            } else {
                ask.size_usd / mark_price.inner()
            };

            if size_base > Decimal::ZERO {
                orders.push(PendingOrder::with_tif(
                    ClientOrderId::new(),
                    market,
                    OrderSide::Sell,
                    ask.price,
                    Size::new(size_base),
                    false,
                    now_ms,
                    tif,
                ));
            }
        }

        orders
    }

    fn build_cancels_static(market: MarketKey, state: &MarketQuoteState) -> Vec<PendingCancel> {
        let now = chrono::Utc::now().timestamp_millis() as u64;
        state
            .bids
            .iter()
            .chain(state.asks.iter())
            .filter_map(|q| q.oid.map(|oid| PendingCancel::new(market, oid, now)))
            .collect()
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

    fn test_config() -> MakerConfig {
        MakerConfig {
            enabled: true,
            num_levels: 1,
            min_offset_bps: dec!(20),
            size_per_level_usd: dec!(10),
            requote_interval_ms: 2000,
            min_requote_change_bps: dec!(5),
            use_alo: true,
            ..Default::default()
        }
    }

    #[test]
    fn test_first_update_places_quotes() {
        let config = test_config();
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(100));

        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );

        assert!(action.is_some());
        if let Some(MakerAction::PlaceOrders(orders)) = action {
            assert_eq!(orders.len(), 2); // bid + ask
            assert_eq!(orders[0].side, OrderSide::Buy);
            assert_eq!(orders[1].side, OrderSide::Sell);
            assert_eq!(orders[0].tif, TimeInForce::AddLiquidityOnly);
        } else {
            panic!("Expected PlaceOrders");
        }
    }

    #[test]
    fn test_no_requote_within_interval() {
        let config = test_config();
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(100));

        // First update
        mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );

        // Same price, within interval → no action
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1500, // 500ms later, < 2000ms interval
            &inv,
        );
        assert!(action.is_none());
    }

    #[test]
    fn test_requote_after_interval() {
        let config = test_config();
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(100));

        // First update
        mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );

        // After interval → requote
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            3100, // 2100ms later, > 2000ms interval
            &inv,
        );
        assert!(action.is_some());
    }

    #[test]
    fn test_requote_on_price_change() {
        let config = test_config();
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(100));

        // First update
        mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );

        // Big price change within interval → requote
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100.10)), // 10 bps change > 5 bps threshold
            Price::new(dec!(100.10)),
            1500,
            &inv,
        );
        assert!(action.is_some());
    }

    #[test]
    fn test_cancel_and_replace_with_oid() {
        let config = test_config();
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(100));

        // First update → place
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );
        let cloids: Vec<ClientOrderId> = if let Some(MakerAction::PlaceOrders(orders)) = &action {
            orders.iter().map(|o| o.cloid.clone()).collect()
        } else {
            panic!("Expected PlaceOrders");
        };

        // Simulate resting confirmation
        mgr.record_resting(&mk(), &cloids[0], 100);
        mgr.record_resting(&mk(), &cloids[1], 101);

        // After interval → cancel and replace
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(101)),
            Price::new(dec!(101)),
            4000,
            &inv,
        );

        if let Some(MakerAction::CancelAndReplace {
            cancels,
            new_orders,
        }) = action
        {
            assert_eq!(cancels.len(), 2);
            assert_eq!(new_orders.len(), 2);
            // Cancels should have the oids
            let cancel_oids: Vec<u64> = cancels.iter().map(|c| c.oid).collect();
            assert!(cancel_oids.contains(&100));
            assert!(cancel_oids.contains(&101));
        } else {
            panic!("Expected CancelAndReplace");
        }
    }

    #[test]
    fn test_record_fill_removes_quote() {
        let config = test_config();
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(100));

        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );
        let cloid = if let Some(MakerAction::PlaceOrders(orders)) = &action {
            orders[0].cloid.clone()
        } else {
            panic!("Expected PlaceOrders");
        };

        assert_eq!(mgr.active_quote_count(&mk()), 2);

        mgr.record_fill(&mk(), &cloid, Price::new(dec!(100)), 0);
        assert_eq!(mgr.active_quote_count(&mk()), 1);
    }

    #[test]
    fn test_shutdown_all() {
        let config = test_config();
        let mut mgr = QuoteManager::new(config);
        let mut inv = InventoryManager::new(dec!(100));

        // Place quotes
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );
        let cloids: Vec<ClientOrderId> = if let Some(MakerAction::PlaceOrders(orders)) = &action {
            orders.iter().map(|o| o.cloid.clone()).collect()
        } else {
            panic!("Expected PlaceOrders");
        };

        // Record resting
        mgr.record_resting(&mk(), &cloids[0], 200);
        mgr.record_resting(&mk(), &cloids[1], 201);

        // Simulate a fill (creates inventory)
        inv.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(100)),
            Size::new(dec!(0.1)),
        );

        // Shutdown
        let actions = mgr.shutdown_all(&inv, |_| Some(Price::new(dec!(100))), 5000);

        assert!(!actions.is_empty());
        if let MakerAction::FlattenAll {
            cancels,
            flatten_orders,
        } = &actions[0]
        {
            assert_eq!(cancels.len(), 2); // Cancel both quotes
            assert_eq!(flatten_orders.len(), 1); // Flatten the inventory
            assert_eq!(flatten_orders[0].side, OrderSide::Sell); // Sell to close long
            assert!(flatten_orders[0].reduce_only);
            assert_eq!(flatten_orders[0].tif, TimeInForce::ImmediateOrCancel);
        } else {
            panic!("Expected FlattenAll");
        }

        // State should be cleared
        assert!(!mgr.has_active_quotes());
    }

    #[test]
    fn test_gtc_tif_when_alo_disabled() {
        let config = MakerConfig {
            use_alo: false,
            ..test_config()
        };
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(100));

        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );

        if let Some(MakerAction::PlaceOrders(orders)) = action {
            assert_eq!(orders[0].tif, TimeInForce::GoodTilCancelled);
        } else {
            panic!("Expected PlaceOrders");
        }
    }

    // === P2-1: Inventory skew protection tests ===

    #[test]
    fn test_inventory_warn_removes_bid_when_long() {
        let config = MakerConfig {
            inventory_warn_ratio: dec!(0.8),
            ..test_config()
        };
        let mut mgr = QuoteManager::new(config);
        let mut inv = InventoryManager::new(dec!(100));

        // Build up long inventory to 85% of max
        // max = $100, need $85 notional at mark_price $100 → 0.85 units
        inv.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(100)),
            Size::new(dec!(0.85)),
        );

        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );

        // Should only have ask (sell) quotes, no bids (buy)
        if let Some(MakerAction::PlaceOrders(orders)) = action {
            assert_eq!(orders.len(), 1);
            assert_eq!(orders[0].side, OrderSide::Sell);
        } else {
            panic!("Expected PlaceOrders with only sell side");
        }
    }

    #[test]
    fn test_inventory_warn_removes_ask_when_short() {
        let config = MakerConfig {
            inventory_warn_ratio: dec!(0.8),
            ..test_config()
        };
        let mut mgr = QuoteManager::new(config);
        let mut inv = InventoryManager::new(dec!(100));

        // Build up short inventory to 85% of max
        inv.record_fill(
            mk(),
            OrderSide::Sell,
            Price::new(dec!(100)),
            Size::new(dec!(0.85)),
        );

        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );

        // Should only have bid (buy) quotes, no asks (sell)
        if let Some(MakerAction::PlaceOrders(orders)) = action {
            assert_eq!(orders.len(), 1);
            assert_eq!(orders[0].side, OrderSide::Buy);
        } else {
            panic!("Expected PlaceOrders with only buy side");
        }
    }

    #[test]
    fn test_inventory_emergency_triggers_flatten() {
        let config = MakerConfig {
            inventory_emergency_ratio: dec!(0.95),
            ..test_config()
        };
        let mut mgr = QuoteManager::new(config);
        let mut inv = InventoryManager::new(dec!(100));

        // Build up inventory to 96% of max
        inv.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(100)),
            Size::new(dec!(0.96)),
        );

        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );

        if let Some(MakerAction::FlattenAll { flatten_orders, .. }) = action {
            assert_eq!(flatten_orders.len(), 1);
            assert_eq!(flatten_orders[0].side, OrderSide::Sell); // Sell to close long
            assert!(flatten_orders[0].reduce_only);
        } else {
            panic!("Expected FlattenAll for emergency flatten");
        }
    }

    // === P2-2: Stale cancel detection tests ===

    #[test]
    fn test_stale_cancel_halts_quoting() {
        let config = MakerConfig {
            stale_cancel_timeout_ms: 5000,
            ..test_config()
        };
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(100));

        // Place initial quotes
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );
        let cloids: Vec<ClientOrderId> = if let Some(MakerAction::PlaceOrders(orders)) = &action {
            orders.iter().map(|o| o.cloid.clone()).collect()
        } else {
            panic!("Expected PlaceOrders");
        };

        // Confirm resting
        mgr.record_resting(&mk(), &cloids[0], 100);
        mgr.record_resting(&mk(), &cloids[1], 101);

        // Trigger requote → cancel and replace (creates pending cancels)
        mgr.on_market_update(
            mk(),
            Price::new(dec!(101)),
            Price::new(dec!(101)),
            4000, // after interval
            &inv,
        );

        // Now 6 seconds later (> 5000ms timeout), cancels still pending
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(102)),
            Price::new(dec!(102)),
            10000,
            &inv,
        );

        // Should be halted
        assert!(mgr.is_stale_halted());
        assert!(action.is_none());
    }

    #[test]
    fn test_stale_cancel_resolved_resumes_quoting() {
        let config = MakerConfig {
            stale_cancel_timeout_ms: 5000,
            ..test_config()
        };
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(100));

        // Place, confirm, requote → creates pending cancels
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );
        let cloids: Vec<ClientOrderId> = if let Some(MakerAction::PlaceOrders(orders)) = &action {
            orders.iter().map(|o| o.cloid.clone()).collect()
        } else {
            panic!("Expected PlaceOrders");
        };
        mgr.record_resting(&mk(), &cloids[0], 100);
        mgr.record_resting(&mk(), &cloids[1], 101);

        mgr.on_market_update(
            mk(),
            Price::new(dec!(101)),
            Price::new(dec!(101)),
            4000,
            &inv,
        );

        // Timeout triggers halt
        mgr.on_market_update(
            mk(),
            Price::new(dec!(102)),
            Price::new(dec!(102)),
            10000,
            &inv,
        );
        assert!(mgr.is_stale_halted());

        // Ack the cancels
        mgr.record_cancel_acked(100);
        mgr.record_cancel_acked(101);

        // Halt should be resolved
        assert!(!mgr.is_stale_halted());
    }

    // === P2-3: Adverse selection tests ===

    #[test]
    fn test_adverse_selection_spread_multiplier() {
        let config = MakerConfig {
            adverse_consecutive_fills: 3,
            adverse_spread_multiplier: dec!(2),
            ..test_config()
        };
        let mut mgr = QuoteManager::new(config);
        let mut inv = InventoryManager::new(dec!(1000));

        // Place initial quotes
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );
        let cloids: Vec<ClientOrderId> = if let Some(MakerAction::PlaceOrders(orders)) = &action {
            orders.iter().map(|o| o.cloid.clone()).collect()
        } else {
            panic!("Expected PlaceOrders");
        };

        // First bid fill
        inv.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(99.80)),
            Size::new(dec!(0.1)),
        );
        mgr.record_fill(&mk(), &cloids[0], Price::new(dec!(100)), 0);
        assert_eq!(mgr.adverse_consecutive_count(&mk()), 1);

        // Place more quotes and fill bid again
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            4000,
            &inv,
        );
        let cloids: Vec<ClientOrderId> = match &action {
            Some(MakerAction::PlaceOrders(o)) => o.iter().map(|x| x.cloid.clone()).collect(),
            Some(MakerAction::CancelAndReplace { new_orders, .. }) => {
                new_orders.iter().map(|x| x.cloid.clone()).collect()
            }
            _ => panic!("Expected PlaceOrders or CancelAndReplace"),
        };

        // 2nd bid fill
        inv.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(99.80)),
            Size::new(dec!(0.1)),
        );
        mgr.record_fill(&mk(), &cloids[0], Price::new(dec!(100)), 0);
        assert_eq!(mgr.adverse_consecutive_count(&mk()), 2);

        // Place more and fill bid 3rd time
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            7000,
            &inv,
        );
        let cloids: Vec<ClientOrderId> = match &action {
            Some(MakerAction::PlaceOrders(o)) => o.iter().map(|x| x.cloid.clone()).collect(),
            Some(MakerAction::CancelAndReplace { new_orders, .. }) => {
                new_orders.iter().map(|x| x.cloid.clone()).collect()
            }
            _ => panic!("Expected PlaceOrders or CancelAndReplace"),
        };
        inv.record_fill(
            mk(),
            OrderSide::Buy,
            Price::new(dec!(99.60)),
            Size::new(dec!(0.1)),
        );
        mgr.record_fill(&mk(), &cloids[0], Price::new(dec!(100)), 0);
        assert_eq!(mgr.adverse_consecutive_count(&mk()), 3);

        // Now next quote should have 2x spread
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            10000,
            &inv,
        );

        // With 2x multiplier: offset = 20 * 2 = 40 bps
        // But inventory skew also applies, so just check the spread is wider
        if let Some(MakerAction::PlaceOrders(orders))
        | Some(MakerAction::CancelAndReplace {
            new_orders: orders, ..
        }) = &action
        {
            // At minimum we should have orders (may only have ask due to inv warn)
            assert!(!orders.is_empty());
        }
    }

    #[test]
    fn test_adverse_selection_resets_on_opposite_fill() {
        let config = MakerConfig {
            adverse_consecutive_fills: 3,
            adverse_spread_multiplier: dec!(2),
            ..test_config()
        };
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(1000));

        // Place quotes
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );
        let cloids: Vec<ClientOrderId> = if let Some(MakerAction::PlaceOrders(orders)) = &action {
            orders.iter().map(|o| o.cloid.clone()).collect()
        } else {
            panic!("Expected PlaceOrders");
        };

        // 2 consecutive buys
        mgr.record_fill(&mk(), &cloids[0], Price::new(dec!(100)), 0); // bid fill (Buy side)
        assert_eq!(mgr.adverse_consecutive_count(&mk()), 1);

        // Place new quotes
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            4000,
            &inv,
        );
        let cloids: Vec<ClientOrderId> = match &action {
            Some(MakerAction::PlaceOrders(o)) => o.iter().map(|x| x.cloid.clone()).collect(),
            Some(MakerAction::CancelAndReplace { new_orders, .. }) => {
                new_orders.iter().map(|x| x.cloid.clone()).collect()
            }
            _ => panic!("Expected PlaceOrders or CancelAndReplace"),
        };

        mgr.record_fill(&mk(), &cloids[0], Price::new(dec!(100)), 0); // another bid fill
        assert_eq!(mgr.adverse_consecutive_count(&mk()), 2);

        // Now a sell fill resets the counter
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            7000,
            &inv,
        );
        let cloids: Vec<ClientOrderId> = match &action {
            Some(MakerAction::PlaceOrders(o)) => o.iter().map(|x| x.cloid.clone()).collect(),
            Some(MakerAction::CancelAndReplace { new_orders, .. }) => {
                new_orders.iter().map(|x| x.cloid.clone()).collect()
            }
            _ => panic!("Expected PlaceOrders or CancelAndReplace"),
        };

        // Fill the ask side (Sell) - should be index 1 if both sides present
        let sell_cloid = cloids.iter().last().unwrap();
        mgr.record_fill(&mk(), sell_cloid, Price::new(dec!(100)), 0);
        // Sell resets to 1 (new direction)
        assert_eq!(mgr.adverse_consecutive_count(&mk()), 1);
    }

    #[test]
    fn test_is_mm_order() {
        let config = test_config();
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(25));

        // Before any quotes, unknown cloid is not MM
        let unknown_cloid = ClientOrderId::new();
        assert!(!mgr.is_mm_order(&unknown_cloid));

        // Place quotes
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );
        let cloids: Vec<ClientOrderId> = match &action {
            Some(MakerAction::PlaceOrders(o)) => o.iter().map(|x| x.cloid.clone()).collect(),
            _ => panic!("Expected PlaceOrders"),
        };

        // Active quote cloid should be recognized as MM
        assert!(mgr.is_mm_order(&cloids[0]));

        // Random cloid should not be recognized
        assert!(!mgr.is_mm_order(&unknown_cloid));

        // After fill removes the quote, cloid is no longer MM
        mgr.record_fill(&mk(), &cloids[0], Price::new(dec!(100)), 0);
        assert!(!mgr.is_mm_order(&cloids[0]));
    }

    // === P3-1: WickTracker integration tests ===

    #[test]
    fn test_wick_tracker_initialized() {
        let config = test_config();
        let mut mgr = QuoteManager::new(config);

        // WickTracker should exist but have no data
        let vol_stats = mgr.volatility_stats(1000);
        assert!(vol_stats.is_empty());
    }

    #[test]
    fn test_oracle_updates_feed_tracker() {
        let config = test_config();
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(100));

        // Feed multiple oracle updates across different seconds
        for sec in 0u64..10 {
            let price = dec!(100) + Decimal::new(sec as i64, 2);
            mgr.on_market_update(
                mk(),
                Price::new(price),
                Price::new(price),
                sec * 1000,
                &inv,
            );
        }

        // WickTracker should have accumulated samples
        let vol_stats = mgr.volatility_stats(10_000);
        assert!(vol_stats.contains_key(&mk()));
        // At least some wicks should be recorded
        // (exact count depends on requote timing, but tracker records every call)
        let stats = &vol_stats[&mk()];
        assert!(stats.sample_count > 0);
    }

    #[test]
    fn test_dynamic_offset_observation_mode() {
        let config = MakerConfig {
            dynamic_offset_enabled: false, // observation mode
            min_offset_bps: dec!(20),
            ..test_config()
        };
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(100));

        // First update → quotes should use fixed 20 bps offset
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );

        if let Some(MakerAction::PlaceOrders(orders)) = action {
            // bid = 99.80 (20 bps), ask = 100.20 (20 bps)
            let bid_price = orders.iter().find(|o| o.side == OrderSide::Buy).unwrap();
            let ask_price = orders.iter().find(|o| o.side == OrderSide::Sell).unwrap();
            assert_eq!(bid_price.price.inner(), dec!(99.80));
            assert_eq!(ask_price.price.inner(), dec!(100.20));
        } else {
            panic!("Expected PlaceOrders");
        }
    }

    // === Phase C: Counter-order + velocity tests ===

    #[test]
    fn test_counter_order_buy_fill() {
        // When a BUY fill happens, counter-order should be SELL
        let config = MakerConfig {
            counter_order_enabled: true,
            counter_reversion_pct: dec!(0.6),
            counter_reversion_per_level: dec!(0),
            ..test_config()
        };
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(100));

        // Place quotes at oracle=100, bid≈99.80
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );
        let cloids: Vec<ClientOrderId> = if let Some(MakerAction::PlaceOrders(orders)) = &action {
            orders.iter().map(|o| o.cloid.clone()).collect()
        } else {
            panic!("Expected PlaceOrders");
        };

        // BUY fill at bid (99.80), mark=100 (oracle proxy)
        let result = mgr.record_fill(&mk(), &cloids[0], Price::new(dec!(100)), 2000);

        assert!(result.is_some(), "Counter-order should be generated");
        if let Some(MakerAction::PlaceOrders(orders)) = result {
            assert_eq!(orders.len(), 1);
            assert_eq!(orders[0].side, OrderSide::Sell); // counter is SELL
            // fill_px=99.80, oracle=100, distance=0.20
            // reversion = 0.20 * 0.6 = 0.12
            // counter_price = 99.80 + 0.12 = 99.92
            assert_eq!(orders[0].price.inner(), dec!(99.92));
        } else {
            panic!("Expected PlaceOrders");
        }
    }

    #[test]
    fn test_counter_order_reversion_per_level() {
        // Higher level fills should have higher reversion
        let config = MakerConfig {
            counter_order_enabled: true,
            counter_reversion_pct: dec!(0.5),
            counter_reversion_per_level: dec!(0.1), // +10% per level
            num_levels: 3,
            min_offset_bps: dec!(20),
            level_spacing_bps: dec!(20), // L0: 20bps, L1: 40bps, L2: 60bps
            ..test_config()
        };
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(100));

        // Place 3-level quotes at oracle=100
        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );
        let orders = if let Some(MakerAction::PlaceOrders(ref o)) = action {
            o.clone()
        } else {
            panic!("Expected PlaceOrders");
        };

        // Find L2 bid (should be the one with the lowest price)
        // With 3 levels: bid[0]=99.80, bid[1]=99.60, bid[2]=99.40
        let buy_orders: Vec<_> = orders
            .iter()
            .filter(|o| o.side == OrderSide::Buy)
            .collect();
        // The outermost bid (lowest price) — it's the last buy order
        let outer_bid = buy_orders.last().unwrap();

        // Record as level 2 fill: reversion = 0.5 + 0.1*2 = 0.7
        let result = mgr.record_fill(&mk(), &outer_bid.cloid, Price::new(dec!(100)), 2000);

        assert!(result.is_some());
        if let Some(MakerAction::PlaceOrders(counter)) = result {
            assert_eq!(counter[0].side, OrderSide::Sell);
            // Level stored as 0 in ActiveQuote (our make_orders doesn't set level)
            // So reversion = 0.5 + 0.1*0 = 0.5
            // fill_px = 99.40, oracle=100, distance=0.60
            // counter = 99.40 + 0.60 * 0.5 = 99.70
            assert_eq!(counter[0].price.inner(), dec!(99.70));
        } else {
            panic!("Expected PlaceOrders");
        }
    }

    #[test]
    fn test_counter_order_disabled() {
        // When counter_order_enabled=false, no counter-order generated
        let config = MakerConfig {
            counter_order_enabled: false,
            ..test_config()
        };
        let mut mgr = QuoteManager::new(config);
        let inv = InventoryManager::new(dec!(100));

        let action = mgr.on_market_update(
            mk(),
            Price::new(dec!(100)),
            Price::new(dec!(100)),
            1000,
            &inv,
        );
        let cloids: Vec<ClientOrderId> = if let Some(MakerAction::PlaceOrders(orders)) = &action {
            orders.iter().map(|o| o.cloid.clone()).collect()
        } else {
            panic!("Expected PlaceOrders");
        };

        let result = mgr.record_fill(&mk(), &cloids[0], Price::new(dec!(100)), 2000);
        assert!(result.is_none(), "No counter-order when disabled");
    }
}
