# BUG-004 再調査計画レビュー（最新版・再々）

## Findings (ordered)

1) High: 再接続トリガーで `WsError::SubscriptionFailed` を返す計画ですが、現行 `WsError` にそのバリアントは存在しません（`SubscriptionError` のみ）。このままだと **コンパイル不能**です。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:372-400`  
   - 関連実装: `crates/hip3-ws/src/error.rs`

2) High: ACK失敗→再接続の経路がまだ不明確です。計画は `handle_text_message()` で `subscriptionResponse` を解析しつつ、再接続トリガーは `restore_subscriptions()` で `resp.error` を見て `Err(...)` を返す流れになっています。しかし `drain_and_wait()` は `handle_text_message()` のエラーを **握りつぶして継続**するため、`restore_subscriptions()` 側に失敗が届きません。  
   - 対策として、(a) `restore_subscriptions()` 内で `subscriptionResponse` を直接解析する、または (b) `drain_and_wait()` でエラーを上位に伝播させる、のいずれかに統一する必要があります。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:103-120` `.claude/plans/stateless-growing-stallman.md:352-400`  
   - 関連実装: `crates/hip3-ws/src/connection.rs:485-535`

3) Medium: `subscriptionResponse` に `error` フィールドがある前提は **公式Docsに記載がありません**。Docsは「`subscriptionResponse` の data は元の subscription を返す」と明記しているため、エラー形式は **実測で確定するまで未定義**として扱うべきです。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:103-120` `.claude/plans/stateless-growing-stallman.md:297-338`  
   - 公式Docs: `subscriptionResponse` の data は元の subscription を返すciteturn0search0

4) Low: Step 2 の ACK 解析スニペットは error 判定がなく、前半の例とは **挙動が一致しません**。エラー判定を入れるなら両方に統一した方が実装の迷いが減ります。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:103-120` `.claude/plans/stateless-growing-stallman.md:268-283`

## Open Questions

- `subscriptionResponse` の **失敗通知形式**（`error`/`message`/別チャネル）を実測で確定後、どの構造体にどう反映するか。citeturn0search0  
- 再接続トリガーは **どのレイヤーで発火**させるか（restore_subscriptions 直解析 or drain_and_wait 経由 or 外側ループ）。  

## Suggested plan edits

- `WsError::SubscriptionFailed` を **既存の `SubscriptionError`** に合わせるか、新バリアント追加を明記。  
- ACK失敗の **エラー伝播経路**を一本化（restore_subscriptions で解析 / drain_and_wait で伝播）。  
- `subscriptionResponse` のエラー形状は **実測後に確定**する旨をより強く明記し、暫定ロジックは最小に留める。citeturn0search0  

## Change Summary

- 計画は具体化されていますが、**再接続エラー伝播**と **WsError整合**がまだ詰め切れていません。
