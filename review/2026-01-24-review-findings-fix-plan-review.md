# Review Findings Fix Plan レビュー

対象: `.claude/plans/2026-01-24-review-findings-fix.md`

## Findings (ordered)

1) High: `get_total_pending_notional_with_validation` が既存構造と一致していません。計画では `self.pending_orders` を MarketKey で回し `order.is_reduce_only` を参照していますが、実装は `pending_orders_data: DashMap<ClientOrderId, TrackedOrder>` で、`TrackedOrder` のフィールドは `reduce_only` です。このままではコンパイル不能・集計不能になります。`pending_orders_data.iter()` で `order.market` を使って合算し、`order.reduce_only` を参照する形に修正が必要です。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:139-160`
   - 参考: `crates/hip3-position/src/tracker.rs:185`, `crates/hip3-position/src/tracker.rs:710`, `crates/hip3-core/src/execution.rs:145`

2) Medium: `scheduledCancel` が計画上は「既知の Cancel」ですが、`map_order_status` は `ends_with("Canceled")` のみなので `scheduledCancel` が未知扱いになります。ログが常時 warn になり、運用ノイズが出るため、`"scheduledCancel"` を明示マップするか `ends_with("Cancel")` も許容する設計に調整が必要です。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:380-392`, `.claude/plans/2026-01-24-review-findings-fix.md:427-446`

3) Medium: F1 のテストが Gate 3 しかありません。Gate 4 の `calculate_total_portfolio_notional` と新しい pending 検証経路（missing mark price）をテストで担保しないと、fail closed の重要な分岐が未検証になります。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:92-126`, `.claude/plans/2026-01-24-review-findings-fix.md:164-177`

4) Low: F3 で `is_terminal` を変更していますが、テストは `map_order_status` のみです。`triggered` / `scheduledCancel` / unknown status の `is_terminal()` 期待値を追加して、未知扱いの fail-safe が意図通りか確認するのが安全です。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:472-515`

## Change Summary

- 方向性は妥当ですが、F1 の pending 集計ロジックが現行データ構造と合っていない点が重大です。F3 の `scheduledCancel` は既知ステータスとして明示対応し、F1/F3 のテストを補強すると計画の確度が上がります。
