# Review Findings Fix Plan レビュー（再確認 5）

対象: `.claude/plans/2026-01-24-review-findings-fix.md`

## Findings (ordered)

1) Medium: F2 の統合テスト例で `read.next()` / `write.send()` を使っていますが、`StreamExt` / `SinkExt` のトレイトが import されていません。`crates/hip3-ws/tests/ws_shutdown_test.rs` に `use futures_util::{StreamExt, SinkExt};` を追加しないとコンパイルできません。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:463-486`

2) Low: F1 の `wait_for_position` で `Duration::from_millis` を使っていますが、`Duration` の import 指示がありません。`use std::time::Duration;`（または `tokio::time::Duration`）の追加を明記してください。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:207-219`

## Change Summary

- 主要なAPI互換性とテスト設計は整いました。残るのはテストコードの import 明記のみで、これを修正すれば計画として実装可能です。
