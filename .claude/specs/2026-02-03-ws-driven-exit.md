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
| 6 | EdgeTracker追加 | [x] DONE | 閾値最適化のためのedge分布監視 |

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

### EdgeTracker (NEW)

- **Location**: `crates/hip3-bot/src/edge_tracker.rs`
- **Purpose**: 閾値以下のedgeも含めた分布監視
- **Log Interval**: 60秒
- **Metrics Logged**:
  - max_buy_edge_bps, max_sell_edge_bps per market
  - threshold_pct (edge / threshold * 100)
  - update count per market

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

// check_dislocations - Edge tracking
let oracle = snapshot.ctx.oracle.oracle_px.inner();
if !oracle.is_zero() {
    let buy_edge = (oracle - ask) / oracle * 10000;
    let sell_edge = (bid - oracle) / oracle * 10000;
    self.edge_tracker.record_edge(key, buy_edge, sell_edge);
}
self.edge_tracker.maybe_log();
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

## EdgeTracker Sample Output

```
EdgeTracker: Market edge summary
  market: dex_1:110026 (SILVER)
  max_buy_edge_bps: 0
  max_sell_edge_bps: 24.47
  threshold_bps: 40
  threshold_pct: 61%
  updates: 905

EdgeTracker: Period summary
  global_max_edge_bps: 24.47
  threshold_bps: 40
  markets_tracked: 5
```

## Deviations from Plan

| Item | Original | Actual | Reason |
|------|----------|--------|--------|
| EdgeTracker | Not planned | Added | 閾値最適化のためのデータ収集が必要と判断 |

## Test Results

```
test result: ok. 59 passed; 0 failed
```

All existing tests pass. New components:
- `exit_watcher` tests: edge calculation, condition checks
- `edge_tracker` tests: edge tracking logic

## Trading Philosophy Validation

| Principle | Implementation | Status |
|-----------|---------------|--------|
| WS-driven exit detection | ExitWatcher < 1ms | ✅ |
| WS-driven entry detection | check_dislocations on each WS msg | ✅ |
| Edge monitoring | EdgeTracker 60s summaries | ✅ |
| Threshold calibration data | max_edge vs threshold_bps | ✅ |

## Next Steps

1. **Edge分布の長期監視**: 時間帯別のedge分布パターンを把握
2. **閾値最適化**: EdgeTrackerデータに基づく閾値調整検討
3. **市場追加検討**: より頻繁にedge機会がある市場の特定
