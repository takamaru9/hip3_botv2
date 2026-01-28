# BUG-004 再調査計画レビュー（正しいパス確認版）

対象: `/Users/taka/crypto_trading_bot/hip3_botv2/.claude/plans/stateless-growing-stallman.md`

## Findings (ordered)

1) High: `WsError::SubscriptionFailed` が計画内に残っており、現行 `WsError` に該当バリアントがありません（`SubscriptionError` のみ）。このままだと **実装がコンパイル不能**です。  
   - 計画該当箇所: `/Users/taka/crypto_trading_bot/hip3_botv2/.claude/plans/stateless-growing-stallman.md:352` `/Users/taka/crypto_trading_bot/hip3_botv2/.claude/plans/stateless-growing-stallman.md:360`  
   - 関連実装: `crates/hip3-ws/src/error.rs`

2) High: ACK失敗→再接続の **伝播経路が未定義**です。計画は `restore_subscriptions()` で `Err(...)` を返して再接続する前提ですが、`drain_and_wait()` が `handle_text_message()` のエラーを握り潰す現状だと、`restore_subscriptions()` へ失敗が届きません。  
   - いずれかに統一が必要: (a) `drain_and_wait()` でエラーを上位へ伝播 / (b) ACK解析を `restore_subscriptions()` に寄せる。  
   - 計画該当箇所: `/Users/taka/crypto_trading_bot/hip3_botv2/.claude/plans/stateless-growing-stallman.md:356` `/Users/taka/crypto_trading_bot/hip3_botv2/.claude/plans/stateless-growing-stallman.md:368`  
   - 関連実装: `crates/hip3-ws/src/connection.rs:485-535`

3) Medium: `subscriptionResponse` の形式は「未確認」と明記しつつ、Step 2 と判定基準では `method/subscription/error` が存在する前提の実装例を提示しています。実測前ロジックを **ログ出力のみ**に限定するか、パース/判定例を **実測後**セクションへ移動してください。  
   - 計画該当箇所: `/Users/taka/crypto_trading_bot/hip3_botv2/.claude/plans/stateless-growing-stallman.md:268` `/Users/taka/crypto_trading_bot/hip3_botv2/.claude/plans/stateless-growing-stallman.md:287` `/Users/taka/crypto_trading_bot/hip3_botv2/.claude/plans/stateless-growing-stallman.md:340`

## Open Questions

- 実測で `channel:"error"` が確認された場合、どの層で検知し `WsError::SubscriptionError` に変換する想定ですか。  

## Suggested plan edits

- `WsError::SubscriptionFailed` を **既存の `SubscriptionError`** に統一するか、新バリアント追加を明記。  
- ACK失敗の **エラー伝播経路**を一本化（`drain_and_wait()` 伝播 or `restore_subscriptions()` 解析）。  
- 実測前はログのみ、パース/判定は **実測後**に限定する旨を追記。  

## Change Summary

- 正しいパスの計画を確認したところ、**WsErrorの不整合**と**エラー伝播経路の欠落**が依然としてブロッカーです。

