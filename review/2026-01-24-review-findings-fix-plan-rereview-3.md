# Review Findings Fix Plan レビュー（再確認 3）

対象: `.claude/plans/2026-01-24-review-findings-fix.md`

## Findings (ordered)

1) High: F1 のテスト用 `setup_executor_with_tracker` が現行 API と不整合です。`PositionTracker::spawn()` は存在せず、`Executor::new(config, handle.clone(), ...)` の形も実装にありません。既存の `spawn_position_tracker` + `BatchScheduler` + `TradingReadyChecker` などと同じ構成に合わせるか、既存の `setup_executor()` を拡張する前提を明記してください。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:203-232`
   - 参考: `crates/hip3-position/src/tracker.rs:773`, `crates/hip3-executor/src/executor.rs:709-742`

2) Medium: F2 の統合テストで `cm.clone()` を使用していますが、`ConnectionManager` は `Clone` を実装していません。`Arc<ConnectionManager>` にするか、`connect()` と `shutdown()` の所有権設計を見直す必要があります。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:447-472`
   - 参考: `crates/hip3-ws/src/connection.rs:71-140`

3) Low: F1 のテストコードで `sample_market()` / `sample_market_2()` を再定義していますが、`executor.rs` のテストモジュールに既に同名関数があります。重複定義になるため、既存関数の再利用か名称変更の指示を明記してください。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:180-199`
   - 参考: `crates/hip3-executor/src/executor.rs:709-716`

## Change Summary

- F1/F3 のテスト整合性は改善されていますが、F1 テストのセットアップが実装 API と一致していない点が残っています。F2 のテストも `ConnectionManager` の所有権モデルに合わせた修正が必要です。これらを直せば実装可能な計画になります。
