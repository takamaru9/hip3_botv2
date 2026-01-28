# BUG-004 再調査計画レビュー（再レビュー）

## Findings (ordered)

1) High: Option A の実装は現状の API では成立しません。`SubscriptionManager::handle_message()` は `channel` 文字列しか受け取らないため、`subscriptionResponse` の **対象 subscription / 成否** を判定できず、`order_updates_ready = true` を正しく立てられません。`subscriptionResponse` の payload を渡す経路追加（例: `handle_message_with_payload(channel, data)`）や、`ConnectionManager` 側で ACK 判定を済ませて `SubscriptionManager` に専用メソッドで通知する設計が必要です。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:90` `.claude/plans/stateless-growing-stallman.md:182`  
   - 関連実装: `crates/hip3-ws/src/connection.rs:334` `crates/hip3-ws/src/subscription.rs:287` `crates/hip3-ws/src/message.rs:248`

2) Medium: 検証手順の「4チャンネル成功」は具体性が不足しています。`bbo`/`activeAssetCtx` は市場数分購読されるため、**期待件数 or 具体的な subscription 名**に基づく確認が必要です。現行ログは `"Received subscription response"` のみで、対象/成否が追えないため、**ACK の中身をログに出す or 解析ロジックを追加**しないと検証になりません。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:153`  
   - 関連実装: `crates/hip3-ws/src/connection.rs:336`

3) Low: 原因候補「チャンネル名マッチング失敗」は現行実装の `contains("orderUpdates")` 前提だと起きにくいので、優先度を下げるか「どの形式が不一致になり得るか」を具体化した方が焦点が合います。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:119`  
   - 関連実装: `crates/hip3-ws/src/subscription.rs:317`

## Open Questions

- `subscriptionResponse` の成否と対象 subscription の判定を **どの層で行うか**（ConnectionManager で判定→SubscriptionManager へ通知 / SubscriptionManager で payload 解釈）。  
- READY-TRADING を **ACK ベース**にする判断基準（運用上のメリット/リスク）をどこに明記するか。  

## Suggested plan edits

- Option A の実装詳細に **payload 受け渡しの具体策**を追記（API 追加 or 役割分担の明示）。  
- Verification に **期待購読数/具体 subscription 名**の確認と、`subscriptionResponse` の **成否ログ or 解析**の追加。  

## Change Summary

- 設計判断の選択肢や仮説/仕様の区別は明確化されていますが、ACK で READY にする場合の **実装経路と検証の具体性**がまだ不足しています。

