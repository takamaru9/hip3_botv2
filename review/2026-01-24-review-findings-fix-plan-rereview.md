# Review Findings Fix Plan レビュー（再確認）

対象: `.claude/plans/2026-01-24-review-findings-fix.md`

## Findings (ordered)

1) High: F1 のテストが現行 API と一致していません。`Executor::execute` / `ExecutionOutcome` / `result.outcome` は実装に存在せず、`Executor::on_signal` の `ExecutionResult::Rejected { reason }` パターンが必要です。このままだと Gate 3/4 の fail-closed を検証できません。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:175-239`
   - 参考: `crates/hip3-executor/src/executor.rs:398-470`, `crates/hip3-core/src/execution.rs:257-336`

2) Medium: F1 のテストセットアップで `position_tracker.add_position` を使っていますが、`PositionTrackerHandle` に該当 API がありません。既存の `fill` を使った位置作成か、専用テストヘルパーの追加が必要です。さらに `register_order` は async なので `#[tokio::test]` + `await` か `try_register_order` へ置き換える必要があります。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:188-229`
   - 参考: `crates/hip3-position/src/tracker.rs:446-575`, `crates/hip3-core/src/execution.rs:130-178`

3) Low: F3 の `test_is_terminal` が `is_terminal("open")` のようなフリー関数前提になっていますが、実装は `OrderUpdatePayload::is_terminal()` メソッドです。ペイロードを生成して検証するか、テスト用ヘルパー関数を追加する方針を明記してください。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:589-614`
   - 参考: `crates/hip3-ws/src/message.rs:170-204`

## Change Summary

- 計画の方向性は妥当ですが、F1 と F3 のテストが現行 API と一致しておらず、このままでは検証不能です。API に合わせたテスト設計へ修正すれば、計画としては十分実装可能です。
