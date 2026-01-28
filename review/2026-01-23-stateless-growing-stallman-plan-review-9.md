# BUG-004 再調査計画レビュー（最新版・再々々々）

## Findings (ordered)

1) High: `WsError::SubscriptionFailed` を返す記述が残っていますが、現行 `WsError` に該当バリアントは存在しません（`SubscriptionError` のみ）。このままだと **実装がコンパイル不能**です。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:372-400`  
   - 関連実装: `crates/hip3-ws/src/error.rs`

2) High: ACK失敗→再接続の **伝播経路が未統一**のままです。`handle_text_message()` で ACK を解析する一方、再接続トリガーは `restore_subscriptions()` で `resp.error` を見て `Err(...)` を返す構図になっています。しかし `drain_and_wait()` は `handle_text_message()` のエラーを握りつぶすため、`restore_subscriptions()` 側に失敗が届きません。  
   - いずれかに統一が必要: (a) `restore_subscriptions()` 内でACKを直接解析する / (b) `drain_and_wait()` からエラーを上位へ伝播させる。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:103-120` `.claude/plans/stateless-growing-stallman.md:352-400`  
   - 関連実装: `crates/hip3-ws/src/connection.rs:485-535`

3) Medium: `subscriptionResponse.data.error` を前提にした失敗判定が残っていますが、公式Docsでは `subscriptionResponse` は **元の subscription を返す**とされており、エラーフィールド仕様は未定義です。**error形式は実測で確定するまで未定義扱い**にし、暫定ロジックは最小に留めるべきです。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:103-120` `.claude/plans/stateless-growing-stallman.md:297-338`

4) Low: Step 2 の ACK 解析スニペットは error 判定がなく、前半の例と **挙動が一致しません**。どちらかに統一した方が実装時の迷いが減ります。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:103-120` `.claude/plans/stateless-growing-stallman.md:268-283`

## Open Questions

- `subscriptionResponse` の失敗通知形式（`error`/`message`/別チャネル等）を **実測で確定後**、どの構造体にどう反映するか。  
- 再接続トリガーを **どのレイヤーで発火**させるか（restore_subscriptions 直解析 or drain_and_wait 伝播 or 外側ループ）。  

## Suggested plan edits

- `WsError::SubscriptionFailed` を **既存の `SubscriptionError`** に合わせるか、バリアント追加を明記。  
- ACK失敗の **エラー伝播経路**を一本化（restore_subscriptions で解析 / drain_and_wait で伝播）。  
- `subscriptionResponse` のエラー形状は **実測後に確定**する旨をより強く明記し、暫定ロジックは最小に留める。  

## Change Summary

- 直近の修正で大きな構造変更は見られず、**再接続エラー伝播**と **WsError整合**の課題は残っています。

