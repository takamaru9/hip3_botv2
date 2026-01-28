# BUG-004 再調査計画レビュー（更新版）

## Findings (ordered)

1) High: Option A（ACKでREADY）の実装案は、`subscriptionResponse` の **payload仕様が未定義**のため現状のままでは実装できません。`parse_subscription_response()` の具体的な戻り値スキーマ（成功/失敗、subscription種別、対象coin/userなど）を計画に明記しないと、`orderUpdates` のACK判定が **誤判定 or 未実装** になります。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:90-121`  
   - 関連実装: `crates/hip3-ws/src/message.rs:248`（subscriptionResponseの型なし）

2) Medium: Verification の `activeAssetCtx` 期待件数が **1件（全市場共通）** になっていますが、実装は **市場数分** 送信しています（`bbo` と同数）。このままだと検証が **常にズレる**ため、`activeAssetCtx` の件数を `bbo` と同じ市場数に修正すべきです。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:188-199`  
   - 関連実装: `crates/hip3-ws/src/connection.rs:380-417`

3) Medium: ACK確認のログ強化は「中身を出力」とありますが、**何を成功/失敗として判定するか** が未定義です。`subscriptionResponse` にエラーが入る場合の扱い（READY判定の抑止/即エラー停止）を計画内で決めておく必要があります。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:171-187`

4) Low: Option A のスニペットは `sub.subscription.contains("orderUpdates")` としていますが、`subscription` は **オブジェクト形式である可能性が高い**ため、`type` フィールド等の構造に合わせた判定ロジックを記載した方が安全です。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:96-111`

## Open Questions

- `subscriptionResponse` の payload 形式（成功/失敗・subscription 内容）を **どの構造体で表現するか**。  
- ACK失敗時の運用方針（即停止/リトライ/READY抑止）をどうするか。  

## Suggested plan edits

- Option A 実装に `SubscriptionResponse` の **明確な構造体/パース方針**を追記。  
- Verification の `activeAssetCtx` 期待件数を **市場数分**に修正。  
- `subscriptionResponse` の **成功/失敗判定基準**と、失敗時の動作（READY抑止 or 即エラー）を追加。  

## Change Summary

- 設計オプションの具体化が進んだ一方で、ACK判定に必要な **payload仕様と検証条件**がまだ不足しています。

