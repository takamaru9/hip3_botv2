# ethereal-sauteeing-galaxy 実装レビュー（hip3-ws WsSender 統合）

確認日: 2026-01-22  
対象: hip3-ws / hip3-executor / hip3-bot（WsSender統合まわり）

---

## 結論

**部分承諾**。`hip3-ws` の送信API（`WsWriteHandle`）と `hip3-executor` の `RealWsSender` 実装は概ね計画どおりで、ユニットテスト/Clippyも通っています。  
ただし **bot側の結線（post応答・orderUpdates/userFills→ExecutorLoop/PositionTracker）が未実装**のため、E2Eで「実弾」が流れる状態にはまだ到達していません。

---

## 実装できている点（OK）

- `hip3-ws` に post送信/受信の型を追加（`PostRequest`/`PostResponseData`/`PostResponseBody`、`OrderUpdatePayload`、`FillPayload`）: `crates/hip3-ws/src/message.rs`
- `WsWriteHandle`（`post()` + `send_text()`）と outbound キューを追加し、接続詳細を隠蔽: `crates/hip3-ws/src/ws_write_handle.rs` `crates/hip3-ws/src/connection.rs`
- post応答受信時の inflight デクリメント、および切断/再接続時の inflight リセット: `crates/hip3-ws/src/connection.rs` `crates/hip3-ws/src/rate_limiter.rs`
- `hip3-executor` に `RealWsSender` を追加し、`WsSender` trait を実装（fire-and-forgetでWSへ投入）: `crates/hip3-executor/src/real_ws_sender.rs`
- `WsMessage::Response` を廃止し、bot側が `WsMessage::{Channel,Pong}` 前提になるよう更新: `crates/hip3-bot/src/app.rs`

---

## 未完了（ブロッカー）

1) **bot側の結線が未実装**  
`post` 応答を `ExecutorLoop::on_response_ok/on_response_rejected` に流す処理、`orderUpdates/userFills` を `PositionTrackerHandle::order_update/fill` に流す処理がまだ入っていません（Trading mode も `not yet implemented` のまま）。  
→ この部分がないと、WS統合は「基盤のみ」で止まります。

2) **orderUpdates/userFills の購読トリガが無い**  
`SubscriptionManager::order_updates_subscription_request()` 等は追加されていますが、どこから `WsWriteHandle::send_text()` で送るか（接続後/READY-MD後/ユーザーアドレス取得後）が未実装です。  
→ これが無いと `READY-TRADING` に到達できず、`WsWriteHandle::post()` が `NotReady` になり続けます。

3) **`WsWriteHandle::is_ready()` と `PostError::RateLimited` の意味が重複**  
`is_ready()` が `rate_limiter.can_send_post()` を含むため、`post()` 内の `PostError::RateLimited` 分岐が実質到達しづらいです（多くは `NotReady` 扱い）。  
→ ログ/原因切り分けを明確にしたいなら、`is_ready()` は「接続 + READY-TRADING + channel open」のみに寄せ、rate limit は `post()` 側で返すのがおすすめです。

---

## テスト結果

```
cargo test -p hip3-ws -p hip3-executor -p hip3-bot
cargo clippy -p hip3-ws -p hip3-executor -p hip3-bot -- -D warnings
```

