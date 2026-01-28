# BUG-004 再調査計画レビュー

## Findings (ordered)

1) High: 計画は「コード上は正しく動作する」と結論付けていますが、READY-TRADING 条件の根拠が **subscriptionResponse(購読ACK) ではなく実データ受信** に依存している点を明示していません。現行実装は `orderUpdates:*` を受信して初めて READY になるため、**注文が発生しない限り READY-TRADING が永遠に立たない**可能性が残ります。計画にはこの設計判断（ACKで良いのか/初回データ必須か）の是非と対策が必要です。  
   - 根拠コード: `crates/hip3-ws/src/subscription.rs:317-326` (orderUpdates 受信で ready フラグ) / `crates/hip3-ws/src/connection.rs:334-346` (全チャネルで handle_message)  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:44-66`

2) Medium: 検証手順に **subscriptionResponse の成功/失敗ログ確認**が含まれていません。WebSocket では購読成功時に `subscriptionResponse` が返るため、READY 以前に「購読そのものが成功しているか」を必ず確認する必要があります。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:60-79`

3) Medium: 「注文がないと初回メッセージが来ない可能性」は仮説であり、Docs で明示されていないため、**検証手順として明確に位置づける**必要があります。現状の計画では仮説が結論寄りに読めるため、誤った判断につながる恐れがあります。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:54-56`

4) Low: 「OrderUpdates channel ready」ログは **初回 data 受信時のみ出る**ため、ACKが成功していてもログが出ないケースを取りこぼします。ログが READY 判定の根拠になるなら、ACK時点のログを追加するか、チェック項目を分けるべきです。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:60-62`

## Doc-derived facts to incorporate

- WebSocket では購読成功時に `subscriptionResponse` が返り、その後に該当チャネルのデータが送られる。  
- 一部のユーザー系ストリーム（例: userFills など）は初回メッセージに `isSnapshot: true` が付く。  
- `orderUpdates` の購読形式は `{ "type": "orderUpdates", "user": "<address>" }`。

## Recommended plan updates

1) READY-TRADING の判定を明文化  
   - **ACKで可**: subscriptionResponse 受信で READY を立てるのか  
   - **初回データ必須**: orderUpdates の最初のデータ受信まで待つのか  
   - いずれの場合も「無更新時の扱い（タイムアウト/強制解除/テスト発注）」を計画に追加

2) 検証手順の拡充  
   - `subscriptionResponse` の成功/失敗確認（エラーがあれば READY 前に原因切り分け）  
   - `orderUpdates` 初回挙動の実測（少額注文で発火確認）

3) 仮説/仕様の区別を明確化  
   - 「注文がないとメッセージが来ない可能性」は **実測項目**として扱う

## Suggested verification steps (具体案)

1) WSログで `subscriptionResponse` を確認（orderUpdates/userFills/bbo/activeAssetCtx）  
2) `orderUpdates` が無いアカウントで起動し、READY-TRADING が立たない再現を確認  
3) 少額の注文を1回出し、`orderUpdates:*` 受信 → READY-TRADING 到達を確認  

