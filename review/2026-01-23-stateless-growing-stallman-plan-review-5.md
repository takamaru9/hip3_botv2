# BUG-004 再調査計画レビュー（最新版・再）

## Findings (ordered)

1) High: 再接続トリガー例で `WsError::SubscriptionFailed` を返していますが、現行 `WsError` にそのバリアントは存在しません。`SubscriptionError` へ合わせるか、新バリアント追加を明記しないと **実装がコンパイル不能**になります。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:352-372`  
   - 関連実装: `crates/hip3-ws/src/error.rs`

2) High: ACK失敗→再接続の経路が実装上つながっていません。`subscriptionResponse` は `handle_text_message()` で処理されますが、`drain_and_wait()` は **handle_text_messageのErrを握りつぶす**ため、`restore_subscriptions()` へエラーが戻りません。再接続を確実に起こすには、  
   - ① `restore_subscriptions()` 内でACKを直接解析する、または  
   - ② `handle_text_message()` のエラーを `drain_and_wait()` から伝播させる  
のどちらかを計画に明示する必要があります。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:352-372` `.claude/plans/stateless-growing-stallman.md:268-283`  
   - 関連実装: `crates/hip3-ws/src/connection.rs:485-535`

3) Medium: `subscriptionResponse` に `error` フィールドが含まれる前提は、公式Docsでは確認できません。公式Docsは **subscriptionResponse は元の subscription を返す**と明記しており、エラーフィールド仕様は不明です。別資料ではエラーは `channel:"error"` で来る例もあるため、**error検知の入口を `subscriptionResponse` に固定しない設計**が必要です。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:108-120` `.claude/plans/stateless-growing-stallman.md:354-372`

4) Low: Option A のコード例（冒頭）では `error` を扱っていますが、後半の「Step 2: connection.rs で ACK 解析」では **error判定が欠落**しています。実装の一貫性のため、片方に統一するのが安全です。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:103-120` `.claude/plans/stateless-growing-stallman.md:268-283`

## Open Questions

- 公式Docsで未定義な **subscription失敗の通知形式**（`channel:"error"` か、`subscriptionResponse.data.error` か）をどう検知するか。  
- 再接続トリガーを **どのレイヤーで発火**させるか（restore_subscriptions内解析/handle_text_message経由/外側ループ）。  

## Suggested plan edits

- `WsError::SubscriptionFailed` の名称を **既存バリアントに整合**させるか、追加する旨を明記。  
- ACK失敗の **伝播経路**（restore_subscriptions 直解析 or drain_and_wait 伝播）を決めて明文化。  
- エラー通知の入口は `subscriptionResponse` に限定せず、`channel:"error"` も含めた検出方針を追記。  

## Change Summary

- 仕様前提の明文化は進みましたが、**再接続の実装経路**と **エラー通知形式**がまだ詰め切れていません。
