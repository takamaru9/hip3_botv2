# subscriptionResponse ACK パース修正計画 リレビュー

対象: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md`

## Findings (ordered)

1) High: `is_order_updates` だけ両対応にしており、`as_order_update` が旧フォーマット（`starts_with("orderUpdates:")`）のままです。もし公式仕様どおり `channel == "orderUpdates"` でデータが来る場合、`is_order_updates` は true でも `as_order_update` が `None` になり、実データのパースが落ちます。**両方を同じ条件で更新**する方針を計画に追加すべきです。
   - 該当箇所: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:165-182`, `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:455-471`

2) Medium: 「統合テスト」が実際の `handle_text_message` を通さず、テスト内でロジックを再実装しています。これだと本番コードを変更してもテストが通る可能性が高く、回帰検出になりません。**接続ハンドラの実処理を呼ぶテスト**（`connection.rs` 内の `#[tokio::test]` で `handle_text_message` を叩く or 共通 helper を本体に抽出して両方から使用）に修正が必要です。
   - 該当箇所: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:293-381`

3) Medium/Low: `method != "subscribe"` の場合に `return Ok(())` と書いており、**subscriptionResponse を downstream に転送しない**分岐になり得ます（コメントも「or continue」と曖昧）。Ready 判定だけをスキップするのか、メッセージ転送も止めるのかを明確化してください。
   - 該当箇所: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:134-143`

4) Low: 未確認事項の「orderUpdates データチャネル名 | ドキュメントに明示なし」は、上段で「channel は subscription type と一致」と引用済みのため整合が取れていません。**「ドキュメントでは一致とされるが実測で確認」**の表現に直す方が一貫します。
   - 該当箇所: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:55-59`, `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:479-484`

## Open Questions / Assumptions

- 公式ドキュメントの「channel は subscription type に一致」を前提に `orderUpdates` 単体チャネルが来る想定で良いか。実測で異なる場合のみ `orderUpdates:<user>` を優先する方針でよいか。

## Change Summary

- 主要方針（ACK の `data.subscription.type` 優先、`method` ガード、両形式対応）は妥当。
- ただし **orderUpdates データのパース経路**と **統合テストの実効性**にギャップが残っています。
