# WSドリブン型Exit Implementation Spec

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-02-03 |
| Last Updated | 2026-02-03 |
| Status | `[COMPLETED]` |
| Source Plan | `.claude/plans/2026-02-03-ws-driven-exit.md` |

## Implementation Status

| ID | Item | Status | Notes |
|----|------|--------|-------|
| 1 | ExitWatcher作成 | [x] DONE | `crates/hip3-position/src/exit_watcher.rs` |
| 2 | get_position追加 | [x] DONE | `tracker.rs` に追加 |
| 3 | lib.rsモジュール追加 | [x] DONE | `exit_watcher` モジュール + re-export |
| 4 | App統合 | [x] DONE | BBO/Ctx更新時にon_market_update()呼び出し |
| 5 | parking_lot依存追加 | [x] DONE | Cargo.tomlに追加 |

## Key Implementation Details

### ExitWatcher

- **Location**: `crates/hip3-position/src/exit_watcher.rs`
- **Type**: `ExitWatcherHandle = Arc<ExitWatcher>`
- **Main Entry Point**: `on_market_update(key, snapshot)`

```rust
pub fn on_market_update(&self, key: MarketKey, snapshot: &MarketSnapshot) {
    // 1. Fast path: Check if we have a position
    // 2. Check if already flattening
    // 3. Check exit condition (same logic as MarkRegressionMonitor)
    // 4. Non-blocking try_send to flatten channel
}
```

### Integration in App

**File**: `crates/hip3-bot/src/app.rs`

```rust
// BboUpdate handler
self.market_state.update_bbo(key, bbo, None);
if let Some(ref exit_watcher) = self.exit_watcher {
    if let Some(snapshot) = self.market_state.get_snapshot(&key) {
        exit_watcher.on_market_update(key, &snapshot);
    }
}

// CtxUpdate handler
self.market_state.update_ctx(key, ctx);
if let Some(ref exit_watcher) = self.exit_watcher {
    if let Some(snapshot) = self.market_state.get_snapshot(&key) {
        exit_watcher.on_market_update(key, &snapshot);
    }
}
```

### Architecture

```
WS Message → App.apply_market_event()
                    ↓
             market_state.update_bbo/ctx()
                    ↓
             exit_watcher.on_market_update(key, snapshot)
                    ↓ [immediate check, < 1ms]
             Exit condition met → flatten_tx.try_send()
                    ↓
             BatchScheduler.enqueue_reduce_only()
```

### Dual-Path Exit (Primary + Backup)

| Path | Latency | Trigger |
|------|---------|---------|
| **ExitWatcher** (Primary) | < 1ms | WS message arrival |
| **MarkRegressionMonitor** (Backup) | ~100ms avg | 200ms polling |

Both paths use `local_flattening` + `is_flattening()` to prevent duplicate flatten orders.

## Deviations from Plan

None. Implementation followed the plan exactly.

## Test Results

```
test result: ok. 59 passed; 0 failed
```

All existing tests pass. New exit_watcher tests:
- `test_long_exit_condition`
- `test_short_exit_condition`
- `test_edge_calculation`
