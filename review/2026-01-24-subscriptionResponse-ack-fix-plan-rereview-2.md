# subscriptionResponse ACK パース修正計画 リレビュー 2

対象: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md`

## Findings (ordered)

1) High: `extract_subscription_type` / `is_order_updates_channel` を **crate 直下で import する前提**になっていますが、現状の plan には `lib.rs` の re-export 追加がありません。`crates/hip3-bot/src/app.rs` の `use hip3_ws::is_order_updates_channel;` と、統合テストの `use hip3_ws::{WsMessage, extract_subscription_type, is_order_updates_channel};` は、そのままだとコンパイルが通りません。`pub use message::{...}` の追加か、`hip3_ws::message::...` へパス修正を計画に明記してください。
   - 該当箇所: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:271-280`, `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:355-356`, `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:465-490`

2) Medium: 「統合テスト」が `handle_text_message` や `SubscriptionManager::mark_order_updates_ready()` の **実コード経路**を検証していません。JSON→WsMessage→helper までしか見ていないため、`connection.rs` 側の配線ミス（`extract_subscription_type` の呼び忘れ、`method` ガードの条件違いなど）があってもテストが通ります。`connection.rs` 内テストで `handle_text_message` を叩く（`#[tokio::test]`）か、ACK 処理を関数に切り出してそれをテストする案を計画に追加してください。
   - 該当箇所: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:348-435`

3) Medium/Low: 「subscriptionResponse は内部 ACK なので downstream 転送不要」と書いていますが、実装方針が曖昧です。**送らないのか、送るが無視するのか**を明確化し、送らないなら `message_tx.send(msg)` のフィルタリング方針を計画に追記してください。現状だとコメントと実際の流れが食い違う可能性があります。
   - 該当箇所: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:167-189`, `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:499-529`

4) Low: `as_order_update` の新条件（`orderUpdates` 単体チャネル）を **パースまで含めて検証するテスト**がありません。既存の `orderUpdates:0x...` と同じ payload を `channel="orderUpdates"` で用意し、`as_order_update()` が成功することを追加で確認したほうが安全です。
   - 該当箇所: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:247-268`, `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:388-402`

## Change Summary

- 以前の指摘（`as_order_update` 両対応、method ガード明確化、未確認事項の表現）は反映済み。
- ただし **公開 API のパス整合** と **ACK→Ready の実コード経路テスト** が未解決です。
