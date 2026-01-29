# Phase A分析と実トレード乖離の改善 - Implementation Spec

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-01-29 |
| Last Updated | 2026-01-29 |
| Status | `[COMPLETED]` |
| Source Plan | `.claude/plans/2026-01-29-trade-winrate-improvement.md` |

## Implementation Status

### P0: 設定変更（コード変更なし）

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P0-1 | Exit閾値調整 (3→10 bps) | [x] DONE | `mark_regression.exit_threshold_bps = 10` |
| P0-2 | TimeStop延長 (5s→15s) | [x] DONE | `time_stop.threshold_ms = 15000` |
| P0-3 | Detector Slippage現実化 (2→10 bps) | [x] DONE | `detector.slippage_bps = 10` |

### P1-1: Gate 6 (FlattenInProgress) 実装

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P1-1a | `is_flattening()` メソッド追加 | [x] DONE | `tracker.rs` |
| P1-1b | `SkipReason::FlattenInProgress` 追加 | [x] DONE | `execution.rs` |
| P1-1c | Gate 6 実装 | [x] DONE | `executor.rs` - Gate番号を5.5→6に変更 |
| P1-1d | ドキュメント更新 | [x] DONE | Gate番号をリナンバリング（5.5→6, 6→7, etc.） |

### P1-2: Reduce-Only Timeout リトライ

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P1-2 | Reduce-only リトライロジック | [-] SKIPPED | 既存の`TimeStopMonitor`がこの機能を持つ（警告ログ出力）。フル実装は今後の課題。 |

## Deviations from Plan

### Gate番号のリナンバリング

**Original Plan:**
> Gate 5.5 として追加

**Actual Implementation:**
- `5.5` はRustdocのリスト解析でエラーとなるため、Gate番号をリナンバリング
- 5.5 → 6 (FlattenInProgress)
- 6 → 7 (has_position)
- 7 → 8 (PendingOrder)
- 8 → 9 (ActionBudget)
- 9 → 10 (all passed)

**Reason:** Clippy `doc_lazy_continuation` lint対応

### P1-2 Reduce-Only リトライ

**Original Plan:**
> `time_stop.rs` でリトライロジックを追加

**Actual Implementation:**
- 既存の `TimeStopMonitor::check_reduce_only_timeout_alerts()` が警告ログを出力する機能を持つ
- 完全なリトライ実装は複雑度が高いため、今回はスキップ
- 今後の改善課題として残す

**Reason:** 既存機能で一定のカバレッジがあり、完全実装は追加計画が必要

## Key Implementation Details

### 1. is_flattening() メソッド

```rust
// crates/hip3-position/src/tracker.rs:910-914
pub fn is_flattening(&self, market: &MarketKey) -> bool {
    self.pending_orders_snapshot.iter().any(|entry| {
        let (m, is_reduce_only) = entry.value();
        m == market && *is_reduce_only
    })
}
```

- `pending_orders_snapshot` を走査して reduce-only フラグを確認
- O(n) だが pending orders は通常少数のため許容範囲

### 2. Gate 6 (FlattenInProgress)

```rust
// crates/hip3-executor/src/executor.rs
// Gate 6: Flatten in progress
if self.position_tracker.is_flattening(market) {
    trace!(market = %market, "Signal skipped: Flatten in progress");
    return ExecutionResult::skipped(SkipReason::FlattenInProgress);
}
```

- Gate 5 (MaxConcurrentPositions) の直後、Gate 7 (has_position) の前に配置
- Flatten中の市場への新規エントリーをブロック

### 3. 設定変更の根拠

| 設定 | Before | After | 根拠 |
|------|--------|-------|------|
| `exit_threshold_bps` | 3 | 10 | エントリー20bpsの50%で対称性改善 |
| `threshold_ms` | 5000 | 15000 | MarkRegressionに発火時間を確保 |
| `slippage_bps` | 2 | 10 | 実際のslippageに近い値でフィルタリング |

## Files Changed

| File | Changes |
|------|---------|
| `config/mainnet-trading-parallel.toml` | P0設定値変更 |
| `crates/hip3-position/src/tracker.rs` | `is_flattening()` 追加 |
| `crates/hip3-core/src/execution.rs` | `SkipReason::FlattenInProgress` 追加 |
| `crates/hip3-executor/src/executor.rs` | Gate 6追加、ドキュメント更新 |

## Test Results

- `cargo fmt`: OK
- `cargo clippy -- -D warnings`: OK
- `cargo test -p hip3-position -p hip3-executor -p hip3-core`: 200 tests passed

## Next Steps

1. VPSにデプロイして設定変更の効果を確認（1時間テスト）
2. `trade_history.csv` を再分析して勝率改善を確認
3. P1-2 (Reduce-only リトライ) の完全実装を検討
