# subscriptionResponse ACK パース修正計画 リレビュー 3

対象: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md`

## Findings (ordered)

1) High: `process_subscription_response` のシグネチャが `&mut SubscriptionManager` になっており、`ConnectionManager` 側の `self.subscriptions` は `Arc<SubscriptionManager>` なので **そのままでは借用できません**。加えてテストが `subs.order_updates_ready()` を呼んでいますが、そのメソッドは現状存在しません。`SubscriptionManager` は内部可変（`RwLock`）なので `&SubscriptionManager` で十分です。テストも `subs.ready_state().order_updates_ready` など既存 API に合わせる必要があります。
   - 該当箇所: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:396-500`, `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:766-790`

2) Medium: 統合テストの `orderUpdates` データのペイロードが `OrderUpdatePayload` の実型と整合していません。`limitPx` / `timestamp` など実構造にないフィールドが含まれ、`data` も配列になっています。現在の `OrderUpdatePayload` は **単一オブジェクト**（`order`, `status`, `statusTimestamp`）を期待するため、このテストは失敗します。既存の `message.rs` のパーステストと同じフィールド構成に合わせるべきです。
   - 該当箇所: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:564-621`

## Change Summary

- 以前の指摘（re-export、downstream フィルタ方針、`as_order_update` exact match テスト追加）は反映済み。
- ただし **`process_subscription_response` の型整合** と **テストデータの構造不一致** が残っています。
