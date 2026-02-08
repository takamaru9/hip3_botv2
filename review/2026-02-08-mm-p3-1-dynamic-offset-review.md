# MM P3-1 Dynamic Offset System (Phase A-C) Code Review

## Metadata
| Item | Value |
|------|-------|
| Date | 2026-02-08 |
| Reviewer | code-reviewer agent |
| Files Reviewed | `crates/hip3-mm/src/volatility.rs`, `crates/hip3-mm/src/config.rs`, `crates/hip3-mm/src/quote_engine.rs`, `crates/hip3-mm/src/quote_manager.rs`, `crates/hip3-bot/src/app.rs` |

## Quick Assessment
| Metric | Score |
|--------|-------|
| Code Quality | 8.2/10 |
| Test Coverage | 75% (estimated) |
| Risk Level | Yellow |

## Key Findings

### Strengths
1. **Clean feature toggle pattern**: All three phases (A: dynamic offset, B: exponential distribution, C: counter-order/velocity) follow the project convention of `enabled: bool` with `#[serde(default)]` and safe fallback to prior behavior. Disabling any feature reverts to the exact previous behavior.

2. **Well-designed breakpoint detection**: The `detect_breakpoint()` function in `volatility.rs` addresses a real v1 lesson (P99 shrinks as data accumulates) by scanning for distribution cliffs rather than using a fixed percentile. The fallback to P99 on no-jump-detected is safe.

3. **Defensive clamping throughout**: Wick values clamped at 500 bps (L184), inventory ratio clamped to [-1, 1] (L74 quote_engine), velocity trend clamped to [-1, 1] (L142), offset floor of 1 bps (L150-151), spread_multiplier floor of 1.0 (L80). This prevents pathological behavior from extreme inputs.

4. **Cache with TTL + invalidation**: The `CachedStats` approach in `WickTracker` avoids sorting on every call while invalidating when new data arrives. The dual check (time-based TTL + wick count change) is robust.

5. **Comprehensive test coverage for volatility.rs**: 13 unit tests covering empty state, single oracle, same-second high/low, rolling window eviction, min samples threshold, cache TTL, cache invalidation, multi-market independence, uniform data, and breakpoint detection edge cases.

6. **Solid test coverage for quote_engine.rs**: 20 tests covering all new features -- dynamic offset with valid/invalid stats, floor enforcement, exponential distribution (monotonicity, inner-dense property, single-level safety), convex sizing, velocity skew in both directions, and backward compatibility.

### Concerns

1. **`ActiveQuote.level` always set to 0**: Counter-order reversion per-level is broken.
   - Location: `crates/hip3-mm/src/quote_manager.rs:345`
   - Impact: When tracking new orders as `ActiveQuote`, the `level` field is hardcoded to `0`. The `PendingOrder` struct does not carry level information. This means `counter_reversion_per_level` config (default: 3% additional per level) has **zero effect** -- all fills are treated as L0 fills regardless of actual level. The test at L1716 even acknowledges this: _"Level stored as 0 in ActiveQuote (our make_orders doesn't set level)"_.
   - Suggestion: Either (a) add a `level: u32` field to `PendingOrder` and propagate from `QuoteLevel` through `make_orders`, or (b) add a separate `level` tracking map from `ClientOrderId` to `u32` in `QuoteManager`, or (c) infer level from price distance to oracle at fill time.

2. **`level_distribution` and `size_distribution` use string matching instead of enum**.
   - Location: `crates/hip3-mm/src/config.rs:129,146`
   - Impact: A typo like `"exponetial"` or `"Exponential"` would silently fall back to linear/uniform without any warning. No deserialization error would be raised.
   - Suggestion: Use `#[serde(rename_all = "lowercase")]` enum types with `#[serde(default)]` instead of `String`. This provides compile-time safety and clear error messages on invalid config.

3. **Decimal precision loss in `decimal_pow` via f64 round-trip**.
   - Location: `crates/hip3-mm/src/quote_engine.rs:38-43`
   - Impact: For the level distribution calculation, the `base` value is `Decimal::from(level) / Decimal::from(num_levels - 1)`, which is in [0, 1]. The `powf` operation on f64 introduces IEEE 754 rounding. For values like `t=0.5, exp=2.0`, `0.5^2 = 0.25` is exact in f64, but other fractions (e.g., 1/3) will have rounding. Given that the output is used to compute basis-point offsets (10s of bps on prices around $100), the precision impact is sub-cent and acceptable for MM quoting. The doc comment correctly notes this tradeoff.
   - Suggestion: No action needed -- this is an acceptable tradeoff documented in the code. Adding a comment about maximum expected error would improve clarity.

4. **`Decimal::from_f64_retain` can return `None` for NaN/Infinity**.
   - Location: `crates/hip3-mm/src/quote_engine.rs:42,86,105`
   - Impact: If `b.powf(e)` produces NaN or Infinity (e.g., 0.0^(-1.0)), `from_f64_retain` returns `None`, handled by `.unwrap_or(Decimal::ZERO)`. However, for the `optimal_wick_bps` conversion at L86, if the f64 value is NaN (e.g., from a corrupted WickTracker), the offset falls to ZERO, which then gets caught by `max(min_offset_bps).max(fee_buffer_bps)`. Similarly at L105, P100 falls to ZERO, caught by the floor computation. The fallback chain is safe.
   - Suggestion: Consider logging a warning when `from_f64_retain` returns `None`, as this indicates unexpected data corruption.

5. **`OracleVelocityTracker` tracks direction but not magnitude**.
   - Location: `crates/hip3-mm/src/quote_manager.rs:125-169`
   - Impact: The velocity tracker only records whether price went up or down (boolean), not by how much. A 0.01 bps tick and a 50 bps jump are treated identically. With the default `velocity_window: 5`, a sequence of 5 tiny upward ticks produces `trend = 1.0` (maximum), which applies 30% asymmetry. In low-volume weekend markets with small tick movements, this could create excessive skew from noise.
   - Suggestion: Consider weighting by magnitude (e.g., log return) or requiring a minimum price change to register as a direction. Alternatively, increase the default `velocity_window` or add a `min_velocity_tick_bps` threshold.

6. **Counter-order uses GTC without lifecycle tracking**.
   - Location: `crates/hip3-mm/src/quote_manager.rs:450-471`
   - Impact: Counter-orders are created with `TimeInForce::GoodTilCancelled` (L458) but are NOT added to the `MarketQuoteState.bids/asks` tracking. This means: (a) `is_mm_order()` will return false for counter-orders, so fills on counter-orders are treated as taker fills by P2-4 drawdown gate; (b) counter-orders are never cancelled during shutdown (`shutdown_all` only cancels tracked quotes); (c) they persist across requote cycles and can accumulate.
   - Suggestion: Either track counter-orders as active quotes (adding them to the MarketQuoteState), or use IOC for counter-orders to avoid orphans. If keeping GTC, add separate tracking and ensure shutdown logic cancels them.

7. **`all_stats()` performs N `get_stats()` calls, each potentially sorting**.
   - Location: `crates/hip3-mm/src/volatility.rs:229-237`
   - Impact: Called every 60 seconds in app.rs for logging. Each `get_stats()` clones the market key vector and individually queries. With caching, the second call within TTL returns cached data, but the first call after TTL sorts each market's wick array (O(n log n) for 3600 samples). For a small number of markets (currently 1-3), this is negligible.
   - Suggestion: Acceptable for current scale. If scaling to many markets, consider computing all stats in a single pass.

### Critical Issues

1. **Counter-order orphaning risk (GTC without tracking)**.
   - Location: `crates/hip3-mm/src/quote_manager.rs:450-471`
   - Must Fix: Counter-orders placed as GTC are not tracked in `MarketQuoteState`. During `shutdown_all()`, only tracked quotes are cancelled. This means GTC counter-orders survive shutdown and persist on the exchange, potentially filling during the next trading session when conditions have completely changed. This is a **real money risk** in production.

## Detailed Review

### volatility.rs

#### WickTracker
- L134: Guard against zero oracle price is correct.
- L144: Using `current_sec == 0` as "first observation" marker is safe because Unix timestamp 0 (1970) will never occur in practice.
- L172-194: `finalize_wick` correctly uses `(high + low) / 2` as mid, guards against zero mid, and caps wick at 500 bps. The `to_string().parse::<f64>()` pattern at L181 is an unusual way to convert `Decimal` to `f64` -- `ToPrimitive::to_f64()` would be more idiomatic and avoids string allocation, but this is a non-hot path (once per second per market).
- L260-261: Sorting with `partial_cmp(...).unwrap_or(Equal)` is correct for f64 (handles NaN safely by treating as equal).
- L264-269: Percentile calculation uses nearest-rank method with `round()`. For n=1, returns the single value. For edge cases where `p=100.0`, `idx = (100.0/100.0 * (n-1))` correctly returns the last element.

#### detect_breakpoint
- L308-342: Scans all adjacent pairs and picks the highest jump ratio >= threshold. Defaults to P99 when no cliff found. This is well-designed.
- Edge case: If all percentile values are 0.0, the `low_val > 0.0` guard prevents division issues, and the default `("P99", p99)` returns `("P99", 0.0)`.

### config.rs

- L93-185: All new fields follow `#[serde(default)]` pattern with `enabled: bool = false` for each feature. This ensures zero breaking changes for existing configs.
- L187-230: Default impl matches all serde defaults -- verified field by field.
- L329-384: Tests verify defaults and serde deserialization with minimal input. Good coverage.

### quote_engine.rs

#### compute_quotes
- L73-96: Dynamic offset computation chain: `optimal_wick * multiplier`, floored by `min_offset_bps` and `fee_buffer_bps`. The three-way max ensures offset is never below either the fixed floor or the fee protection floor.
- L98-113: Range upper bound for exponential distribution: correctly falls back when volatility stats are unavailable or distribution is linear.
- L118-129: Division by `(num_levels - 1)` is guarded by `use_exponential` check which requires `num_levels > 1` (L99). No division-by-zero risk.
- L133-147: Inventory skew and velocity skew are multiplicative. The interaction between them is additive on the multiplier: `bid_offset = base * (1 + inv_skew) * (1 + vel_skew)`. When both are active at maximum values (inv=0.3, vel=0.3), bid could be `base * 1.3 * 1.3 = base * 1.69`, while ask could be `base * 0.7 * 0.7 = base * 0.49`. The 1 bps floor at L150-151 prevents negative offsets.
- L157-165: Convex size distribution uses `t / (num_levels - 1)`, same division-by-zero guard via `use_convex` requiring `num_levels > 1` (L116).

### quote_manager.rs

#### on_market_update
- L211-356: The method is large but well-structured with associated functions to avoid borrow conflicts. The flow is: stale check -> emergency flatten -> spread multiplier -> wick record -> velocity track -> compute quotes -> inventory filter -> requote check -> build action.
- L245-252: Wick tracker always records oracle updates regardless of `dynamic_offset_enabled` (observation mode). The `vol_ref` is only passed to `compute_quotes` when enabled. Good design.
- L255-264: Velocity tracker is created per-market on first access. The `or_insert_with` uses `self.config.velocity_window`, which is safe since config is immutable after construction.

#### record_fill (counter-order generation)
- L374-472: Counter-order logic is sound mathematically. The `reversion_pct` is capped at 95% (L425). The counter-price direction is correct: BUY fill -> SELL counter at `fill_px + reversion_distance`; SELL fill -> BUY counter at `fill_px - reversion_distance`.
- L440-448: Guards against zero mark_price and non-positive size_base.
- L450-458: Counter-order uses `ClientOrderId::new()` for unique cloid (idempotent). However, as noted in Critical Issues, this order is not tracked.

#### OracleVelocityTracker
- L136-169: Simple direction-based tracker. `VecDeque::with_capacity(window)` pre-allocates. The `trend()` calculation maps [0, 1] up-ratio to [-1, 1] correctly: all ups = `1*2-1=1`, all downs = `0*2-1=-1`, half and half = `0.5*2-1=0`.
- L147: Price equality check (`price != last`) avoids recording stale ticks as direction changes.
- L149: `len() >= window` correctly evicts old entries.

### app.rs Integration

- L158: `mm_wick_log_ms` field added for periodic logging interval.
- L2197-2206: Counter-order execution path is correct -- calls `qm.record_fill()`, gets optional `MakerAction`, executes via `on_mm_quote`. The executor is accessed via `executor_loop.executor()` which returns a reference.
- L2390-2407: Periodic wick stats logging every 60 seconds. Only logs markets with `is_valid` stats. Updates `mm_wick_log_ms` after logging.

## Suggestions

| Priority | Location | Current | Suggested | Reason |
|----------|----------|---------|-----------|--------|
| P0 | `quote_manager.rs:450-458` | Counter-orders are GTC, not tracked in state | Track counter-orders in MarketQuoteState or use IOC instead of GTC | Counter-orders survive shutdown and can fill at stale prices; real money risk |
| P0 | `quote_manager.rs:345` | `level: 0` hardcoded for all ActiveQuotes | Propagate level from QuoteLevel through make_orders | `counter_reversion_per_level` config has zero effect currently |
| P1 | `config.rs:129,146` | `level_distribution: String`, `size_distribution: String` | Use enums with `#[serde(rename_all)]` | Typos silently fall back to default behavior without warning |
| P1 | `quote_manager.rs:127-169` | Direction-only velocity tracker | Add minimum tick threshold or magnitude weighting | Noise from tiny ticks causes excessive skew in low-volume markets |
| P2 | `volatility.rs:181` | `to_string().parse::<f64>()` for Decimal-to-f64 | Use `ToPrimitive::to_f64()` | Avoids unnecessary string allocation |
| P2 | `quote_engine.rs:86-87` | `from_f64_retain(...).unwrap_or(ZERO)` silently | Add `tracing::warn` when None | Helps diagnose data corruption in production |

## Verdict
**CONDITIONAL**

**Summary**: The P3-1 dynamic offset system (Phase A-C) is well-designed with solid test coverage and defensive coding. However, two issues must be addressed before production deployment: (1) GTC counter-orders are not tracked and will survive shutdown, creating orphaned orders on the exchange; (2) the `level` field is always 0, rendering the `counter_reversion_per_level` feature non-functional.

**Next Steps**:
1. Fix P0: Track counter-orders in MarketQuoteState or switch to IOC TIF, and ensure shutdown_all cancels them.
2. Fix P0: Propagate level information from QuoteLevel to ActiveQuote (requires adding level to PendingOrder or using a side-channel).
3. Fix P1: Replace string-based distribution selectors with enums.
4. Consider P1: Add minimum tick threshold to OracleVelocityTracker to filter noise.
5. Deploy in observation mode first (`dynamic_offset_enabled: false`, `counter_order_enabled: false`, `velocity_skew_enabled: false`) to validate wick statistics before activating.
