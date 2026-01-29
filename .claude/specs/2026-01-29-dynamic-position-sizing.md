# Dynamic Position Sizing Implementation Spec

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-01-29 |
| Last Updated | 2026-01-29 |
| Status | `[COMPLETED]` |
| Source Plan | `.claude/plans/2026-01-29-dynamic-position-sizing.md` |

## Implementation Status

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P1 | DynamicSizingConfig in config.rs | [x] DONE | Added enabled + risk_per_market_pct fields |
| P2-1 | balance_cache (AtomicU64) in PositionTrackerHandle | [x] DONE | Lock-free, stores cents (USD*100) |
| P2-2 | get_balance() method | [x] DONE | Returns Decimal from atomic |
| P2-3 | update_balance() method | [x] DONE | Fixed: uses trunc().to_u64() |
| P2-4 | spawn_position_tracker update | [x] DONE | Initializes balance_cache |
| P2-5 | Balance unit tests | [x] DONE | test_balance_get_and_update, test_balance_precision |
| P3 | sync_positions_from_api balance sync | [x] DONE | Extracts account_value from margin_summary |
| P4-1 | ExecutorConfig extension | [x] DONE | Added dynamic_sizing_enabled + risk_per_market_pct |
| P4-2 | effective_max_notional_per_market() | [x] DONE | min(config_max, balance * risk_pct) |
| P4-3 | Gate 3 update | [x] DONE | Uses effective_max, logs dynamic values |
| P5 | Config connection in app.rs | [x] DONE | Passes dynamic_sizing to ExecutorConfig |
| P6 | Config file update | [x] DONE | Enabled with 10% risk, 100 hard cap |

## Deviations from Plan

### No Actor Message for Balance

**Original Plan:** Add `UpdateBalance(Decimal)` message to `PositionTrackerMsg`
**Actual Implementation:** Direct atomic update via Handle method only
**Reason:** Balance is only read (get_balance), never needs Actor state consistency. Actor already has read-only relationship with Handle caches for positions/orders. Adding message would add unnecessary complexity and latency.

### Decimal to u64 Conversion

**Original Plan:** `(balance * 100).to_string().parse::<u64>()`
**Actual Implementation:** `(balance * 100).trunc().to_u64()`
**Reason:** Decimal::to_string() returns "18650.00" which fails u64 parse. Fixed to use proper numeric conversion.

## Key Implementation Details

### Balance Storage Architecture

```
PositionTrackerHandle
  └── balance_cache: Arc<AtomicU64>  // Stores cents (USD * 100)
       ├── get_balance() -> Decimal   // cents / 100
       └── update_balance(Decimal)    // balance * 100 -> u64
```

- Lock-free atomic for high-frequency reads in Gate 3
- $0.01 precision (sufficient for position sizing)
- Max representable: ~$184 quadrillion (u64::MAX / 100)

### Dynamic Sizing Calculation

```rust
effective_max = if dynamic_sizing_enabled && balance > 0 {
    min(config.max_notional_per_market, balance * risk_per_market_pct)
} else {
    config.max_notional_per_market  // Static fallback
}
```

### Balance Update Flow

```
1. sync_positions_from_api() (startup + every 60s)
2. fetch clearinghouseState API
3. Extract margin_summary.account_value
4. Call position_tracker.update_balance(balance)
5. Gate 3 uses effective_max on next signal
```

## Test Results

| Test | Result |
|------|--------|
| test_balance_get_and_update | PASS |
| test_balance_precision | PASS |
| All hip3-position tests | 56 passed |
| All hip3-executor tests | 100 passed |

## Example Behavior

| Balance | Risk % | Dynamic Max | Config Max | Effective Max |
|---------|--------|-------------|------------|---------------|
| $0 | 10% | N/A | $100 | $100 (fallback) |
| $186 | 10% | $18.60 | $100 | $18.60 |
| $500 | 10% | $50.00 | $100 | $50.00 |
| $1000 | 10% | $100.00 | $100 | $100.00 |
| $2000 | 10% | $200.00 | $100 | $100.00 (capped) |

## Files Modified

| File | Changes |
|------|---------|
| crates/hip3-bot/src/config.rs | +DynamicSizingConfig, +PositionConfig.dynamic_sizing |
| crates/hip3-position/src/tracker.rs | +balance_cache, +get_balance(), +update_balance(), tests |
| crates/hip3-bot/src/app.rs | Balance sync in sync_positions_from_api() |
| crates/hip3-executor/src/executor.rs | +ExecutorConfig fields, +effective_max_notional_per_market(), Gate 3 |
| config/mainnet-trading-parallel.toml | +[position.dynamic_sizing] section |
