# BUG-004 再調査計画レビュー（追加）

## Findings (ordered)

1) High: `WsError::SubscriptionFailed` が再登場していますが、現行 `WsError` に該当バリアントはありません。`SubscriptionError` に寄せるか、新バリアント追加を計画に明記しないと **コンパイル不能**です。  
   - 計画該当箇所: `/Users/taka/.claude/plans/stateless-growing-stallman.md:352` `/Users/taka/.claude/plans/stateless-growing-stallman.md:360`  
   - 関連実装: `crates/hip3-ws/src/error.rs`

2) High: ACK失敗→再接続の **伝播経路が未定義**です。計画は `restore_subscriptions()` でエラーを返して再接続する前提ですが、`drain_and_wait()` で `handle_text_message()` のエラーが握り潰される現状だと失敗が届きません。  
   - いずれかに統一が必要: (a) `drain_and_wait()` でエラーを上位に伝播させる / (b) ACK解析を `restore_subscriptions()` に寄せる。  
   - 計画該当箇所: `/Users/taka/.claude/plans/stateless-growing-stallman.md:356` `/Users/taka/.claude/plans/stateless-growing-stallman.md:368`  
   - 関連実装: `crates/hip3-ws/src/connection.rs:485-535`

3) Medium: `subscriptionResponse` の形式は「実測必須」と書きつつ、Step 2 と判定基準で `method/subscription/error` が存在する前提の例が提示されています。実測前のロジックは最小化するか、Step 2 を「実測後」へ明確に移してください。  
   - 計画該当箇所: `/Users/taka/.claude/plans/stateless-growing-stallman.md:268` `/Users/taka/.claude/plans/stateless-growing-stallman.md:287` `/Users/taka/.claude/plans/stateless-growing-stallman.md:340`

## Open Questions

- `channel:"error"` 等の別形式が実測で確認された場合、どこで検出して `WsError::SubscriptionError` に変換する想定ですか。  

## Suggested plan edits

- `WsError::SubscriptionFailed` を **既存の `SubscriptionError`** に統一するか、新バリアント追加を明記。  
- ACK失敗の **エラー伝播経路**を一本化（`drain_and_wait()` で伝播 or `restore_subscriptions()` で解析）。  
- 実測前の例はログ出力のみとし、パース/判定は **実測後に限定**する旨を明記。  

## Change Summary

- 以前解消したはずの `SubscriptionFailed` が復活し、エラー伝播経路の記述も不足しているため、再びブロッカーが出ています。

