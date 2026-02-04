# PR-5: Quote Lag Gate Implementation Spec

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-02-04 |
| Last Updated | 2026-02-04 |
| Status | `[COMPLETED]` |
| Source Plan | `.claude/plans/2026-02-04-pr5-quote-lag-gate.md` |

## Implementation Status

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P5-1 | config.rs: `min_quote_lag_ms` field | [x] DONE | Default 0 (disabled) |
| P5-2 | config.rs: `max_quote_lag_ms` field | [x] DONE | Default 0 (disabled) |
| P5-3 | config.rs: Default trait update | [x] DONE | Both fields added |
| P5-4 | detector.rs: `check()` signature | [x] DONE | Added `oracle_age_ms: Option<i64>` |
| P5-5 | detector.rs: `check_buy()` signature | [x] DONE | Added `oracle_age_ms` param |
| P5-6 | detector.rs: `check_sell()` signature | [x] DONE | Added `oracle_age_ms` param |
| P5-7 | detector.rs: Quote Lag Gate logic (buy) | [x] DONE | After consecutive filter |
| P5-8 | detector.rs: Quote Lag Gate logic (sell) | [x] DONE | After consecutive filter |
| P5-9 | detector.rs: Log oracle_age_ms in signals | [x] DONE | Added to info! macro |
| P5-10 | app.rs: Get oracle_age_ms | [x] DONE | `market_state.get_oracle_age_ms(&key)` |
| P5-11 | app.rs: Pass to detector.check() | [x] DONE | 5th argument |
| P5-12 | config/mainnet-trading-parallel.toml | [x] DONE | Added with comments |
| P5-13 | Test: gate disabled by default | [x] DONE | Passes |
| P5-14 | Test: min blocks too fresh | [x] DONE | Passes |
| P5-15 | Test: max blocks too stale | [x] DONE | Passes |
| P5-16 | Test: window works correctly | [x] DONE | Passes |
| P5-17 | Test: None allows (skip gate) | [x] DONE | Passes |
| P5-18 | Test: sell side works | [x] DONE | Passes |

## Deviations from Plan

None. Implementation follows plan exactly.

## Key Implementation Details

### Gate Position in Filter Sequence

```
1. is_tradeable() check      [既存]
2. Raw Edge Check            [既存]
3. Oracle Direction Filter   [既存]
4. Oracle Velocity Filter    [既存]
5. Oracle Consecutive Filter [既存]
6. Quote Lag Gate            [NEW] ← ここに追加
7. Liquidity Gate            [既存]
```

### Configuration

```toml
[detector]
# Quote Lag Gate (PR-5: True Stale Liquidity)
min_quote_lag_ms = 0   # 0=disabled, 推奨: 50ms
max_quote_lag_ms = 0   # 0=disabled, 推奨: 500ms
```

### Log Messages

Debug logs when blocked:
- `"Signal skipped: oracle moved too recently (noise filter)"`
- `"Signal skipped: oracle moved too long ago (MM caught up)"`

Signal logs now include `oracle_age_ms` field.

## Files Modified

| File | Changes |
|------|---------|
| `crates/hip3-detector/src/config.rs` | +20 lines: 2 fields, 2 defaults, Default trait |
| `crates/hip3-detector/src/detector.rs` | +70 lines: signatures, gate logic, 6 tests |
| `crates/hip3-bot/src/app.rs` | +3 lines: get oracle_age_ms, pass to check() |
| `config/mainnet-trading-parallel.toml` | +9 lines: config with comments |

## Test Results

```
cargo test -p hip3-detector quote_lag
running 6 tests
test detector::tests::test_quote_lag_gate_disabled_by_default ... ok
test detector::tests::test_quote_lag_gate_min_blocks_too_fresh ... ok
test detector::tests::test_quote_lag_gate_max_blocks_too_stale ... ok
test detector::tests::test_quote_lag_gate_window ... ok
test detector::tests::test_quote_lag_gate_none_allows ... ok
test detector::tests::test_quote_lag_gate_sell_side ... ok

test result: ok. 6 passed; 0 failed
```

## Backwards Compatibility

- Default values (0, 0) completely disable the gate
- Existing config files work without modification (serde defaults)
- No API changes (internal parameter addition only)
