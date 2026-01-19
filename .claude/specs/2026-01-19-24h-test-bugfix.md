# 24h Test Bugfix Implementation Spec

## Metadata
| Item | Value |
|------|-------|
| Plan Date | 2026-01-19 |
| Last Updated | 2026-01-19 |
| Status | `[COMPLETED]` |
| Source Plan | `.claude/plans/2026-01-19-24h-test-bugfix-plan.md` |

## Implementation Status

### BUG-001: Parquet File Not Written

| ID | Item | Status | Notes |
|----|------|--------|-------|
| B1-1 | Add `flush()` after `write()` in `ParquetWriter::flush()` | [x] DONE | L186-190 in `writer.rs` |
| B1-2 | Add public `close()` API to `ParquetWriter` | [x] DONE | L203-210 in `writer.rs` |
| B1-3 | Call `close()` in `app.rs` shutdown | [x] DONE | L284-286 in `app.rs` |

### BUG-002: oracle_age Measuring "Price Change" Not "Update Received"

| ID | Item | Status | Notes |
|----|------|--------|-------|
| B2-1 | Remove `oracle_age_ms` from `check_all()` signature | [x] DONE | 7 args -> 6 args |
| B2-2 | Remove `oracle_fresh` gate from `check_all()` | [x] DONE | 9 gates -> 8 gates |
| B2-3 | Mark `check_oracle_fresh()` as deprecated | [x] DONE | `#[allow(dead_code)]` |
| B2-4 | Update gate numbering in `check_all()` | [x] DONE | 1-8 renumbered |
| B2-5 | Remove `oracle_age_ms` from `app.rs` | [x] DONE | L392-396 removed |
| B2-6 | Update tests for new signature | [x] DONE | 4 tests updated |

## Deviations from Plan

None. Implementation followed the plan exactly.

## Key Implementation Details

### BUG-001 Changes

1. **`writer.rs:186-190`**: Added `active.writer.flush()?` after `write()` to force row group to disk
2. **`writer.rs:203-210`**: New public `close()` method that calls `flush()` then `close_active_writer()`
3. **`app.rs:284-286`**: Changed `self.writer.flush()?` to `self.writer.close()?` in shutdown

### BUG-002 Changes

1. **Gate Order** (after fix):
   - Gate 1: `bbo_update` (was Gate 2)
   - Gate 2: `ctx_update` (was Gate 3) - now covers oracle freshness
   - Gate 3: `time_regression` (was Gate 4)
   - Gate 4: `mark_mid_divergence` (was Gate 5)
   - Gate 5: `spread_shock` (was Gate 6)
   - Gate 6: `oi_cap` (was Gate 7)
   - Gate 7: `param_change` (was Gate 8)
   - Gate 8: `halt` (was Gate 9)

2. **`check_oracle_fresh()`**: Kept but marked `#[allow(dead_code)]` for backwards compatibility

3. **Metrics**: `oracle_age` gauge metric still collected for observability (in `apply_market_event`)

## Test Results

```
cargo test --workspace
   130+ tests passed, 0 failed

cargo clippy -- -D warnings
   No warnings
```

### BUG-003: Excessive Gate Block WARN Logs

| ID | Item | Status | Notes |
|----|------|--------|-------|
| B3-1 | Change `warn!` to `trace!` in `gates.rs` check_all() | [x] DONE | All 8 gates use trace! |
| B3-2 | Add state-change-only logging in `app.rs` | [x] DONE | HashMap tracks (market, gate) block state |
| B3-3 | Pass specific gate name to `Metrics::gate_blocked()` | [x] DONE | Was "combined", now uses actual gate name |
| B3-4 | Add `gate_block_state` field to Application | [x] DONE | HashMap<(MarketKey, String), bool> |

### BUG-003 Changes

1. **`gates.rs`**: Changed all `warn!()` to `trace!()` in `check_all()` for gates 1-8
2. **`app.rs`**: Added `gate_block_state: HashMap<(MarketKey, String), bool>` for state tracking
3. **`app.rs`**: State-change-only logging - only logs WARN when gate first blocks, not every tick
4. **`app.rs`**: Extract gate name from `RiskError::GateBlocked` for specific metrics
5. **`app.rs`**: Clear block state when all gates pass (allows re-logging when block resumes)

**Impact**: Reduces WARN log volume from ~250k/5min to only state changes (few hundred per day max)

## Files Modified

| File | Changes |
|------|---------|
| `crates/hip3-persistence/src/writer.rs` | Added `flush()` call, new `close()` API |
| `crates/hip3-risk/src/gates.rs` | Removed oracle_fresh gate, renumbered gates, warn->trace, updated tests |
| `crates/hip3-bot/src/app.rs` | Removed oracle_age_ms, use `close()`, state-change logging |
