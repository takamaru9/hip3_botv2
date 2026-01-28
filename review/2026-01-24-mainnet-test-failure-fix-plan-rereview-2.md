# Mainnet Test Failure Fix Plan 再レビュー (2)

対象: `.claude/plans/2026-01-24-mainnet-test-failure-fix.md`

## Findings

- [HIGH] `batch_to_action()` の実装例が現行 `Action`/`CancelWire` の型と一致していません。`Action` には `action_type` と `grouping: Option<String>` が必須ですが、計画では `Grouping::Na` を使い `action_type` を省略しています。また `CancelWire::from_pending_cancel` は存在しません。実際の構造体に合わせて `action_type: "order"/"cancel"` を明記し、`grouping: Some("na".to_string())` へ戻すなど、コンパイル可能な形に修正してください。 (L318-356)
- [HIGH] `tick()` のエラーハンドリング例が `return;` になっており、戻り値が `Option<u64>` のため不整合です。加えて `create_request()` 済みの `post_request_manager` エントリを削除しないと、タイムアウトで二重処理される恐れがあります。`return None;` への修正と、`post_request_manager.remove(post_id)` もしくは `create_request()` を `batch_to_action()` 成功後に移動する方針を明記してください。 (L360-372)
- [MEDIUM] `handle_batch_conversion_failure` の例が既存APIと合っていません。`self.executor.cleanup_dropped_orders` / `self.executor.requeue_order` / `self.executor.requeue_cancel` は存在しないため、`ExecutorLoop::cleanup_dropped_orders` と `batch_scheduler().enqueue_reduce_only/enqueue_cancel` を使う形に合わせてください。 (L387-412)

## Residual Risks / Gaps

- `handle_batch_conversion_failure` の追加と `handle_send_failure` との重複が残っています。`post_id` が不要な分岐を共通化するなら、既存の `handle_send_failure` のシグネチャ変更と全呼び出し元の更新が必要です。 (L387-418)

## Change Summary

- 主要な安全性方針（SpecCache未充足時はバッチ失敗）が明確になった一方、`Action` 構造体・`tick()` 返り値・再キューAPIの整合がまだ取れていません。
