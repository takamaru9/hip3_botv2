# ethereal-sauteeing-galaxy 実装 再レビュー（hip3-ws WsSender 統合）

確認日: 2026-01-21  
対象: `hip3-ws` / `hip3-executor` / `hip3-bot`（WsSender統合まわり + READY/HardStop 影響範囲）

---

## 結論

**部分承諾**です。

- ✅ `hip3-ws` の送信基盤（`WsWriteHandle` + outbound queue）と `hip3-executor` の `RealWsSender` / `ExecutorLoop` は、ユニットテスト/Clippy まで含めて安定しています。
- ❌ ただし **E2E で「実弾が流れる」状態（bot→executor→ws→post→post応答→position更新）には未到達**です。下記ブロッカーが残っています。

検証:
```bash
cargo test -p hip3-ws -p hip3-executor -p hip3-bot
cargo clippy -p hip3-ws -p hip3-executor -p hip3-bot -- -D warnings
```

---

## ✅ OK（改善確認できた点）

- `ExecutorLoop` の `v` 値変換（`0/1` → `27/28`）が反映されている（署名 wire の互換性向上）: `crates/hip3-executor/src/executor_loop.rs`
- `WsWriteHandle::is_ready()` から rate limit 判定が分離され、`NotReady` と `RateLimited` が意味的に分かれた: `crates/hip3-ws/src/ws_write_handle.rs`
- `ConnectionManager` 側で post 応答受信時に inflight をデクリメントし、切断/再接続/heartbeat timeout 時に inflight をリセットする方針が入っている: `crates/hip3-ws/src/connection.rs` `crates/hip3-ws/src/rate_limiter.rs`

---

## ❌ ブロッカー（未承諾 / 実運用・testnet実弾に進めない要因）

### 1) bot側がまだ「統合層」になっていない（結線が TODO のまま）

`post` 応答を `ExecutorLoop::on_response_ok/on_response_rejected` に流す処理、`orderUpdates/userFills` を `PositionTrackerHandle::order_update/fill` に流す処理が未実装です。`Trading mode not yet implemented` も残っています。  
→ これが無いと WsSender 統合は「基盤のみ」で止まり、Phase B のループが閉じません。

対象: `crates/hip3-bot/src/app.rs`

### 2) Trading subscriptions が起動時に有効化されない（`user_address=None` のまま）

`ConnectionManager` は `ConnectionConfig.user_address` が `Some` のときだけ `orderUpdates/userFills` を購読しますが、bot側設定で `user_address: None` のままです。  
→ その結果 `READY-TRADING` に到達できず、`WsWriteHandle::post()` が常に `NotReady` になり得ます（= RealWsSender が常に retryable 扱い）。

対象: `crates/hip3-bot/src/config.rs`（`impl From<WsConfig> for ConnectionConfig`） / `crates/hip3-bot/src/app.rs`（`ws_config.user_address` の設定が無い）

### 3) `drain_and_wait()` が SubscriptionManager/Heartbeat の状態更新をしない（READYが詰む可能性）

`restore_subscriptions()` 内で使っている `drain_and_wait()` は、受信した `Text` を `message_tx` へ流すだけで、
- `self.subscriptions.handle_message(...)` による READY 更新
- `self.heartbeat.record_message()` 等の更新
- `post` 応答時の inflight デクリメント（今回は無関係だが一貫性）

を実施していません。  
特に `orderUpdates` の初回スナップショットが `drain_and_wait()` で吸われると、以後 `orderUpdates` が流れない限り `READY-TRADING` が永遠に立たず、**`WsWriteHandle::is_ready()` が false のままになり得ます（postできないデッドロック）**。

対象: `crates/hip3-ws/src/connection.rs`（`drain_and_wait`）

---

## ⚠️ 追加の問題（Phase B 停止品質/整合性に直結。WsSender統合とは別タスクだが、未解消のまま実弾は危険）

### A) HardStop の停止シーケンスが計画の「全cancel + 全flatten」になっていない

現状 `Executor::on_hard_stop()` は new_orders の drop までで、**「pending_orders 走査→cancel enqueue」「positions 走査→flatten enqueue」** が未実装です。  
`TrackedOrder` に `oid` が無いので cancel を生成できない設計制約も残っています（plan上は orderUpdates で `oid` を反映して cancel 可能にする前提）。

対象: `crates/hip3-executor/src/executor.rs` / `crates/hip3-core/src/execution.rs` / `crates/hip3-position/src/tracker.rs`

### B) `HardStopLatch/RiskMonitor` が `hip3-risk` と `hip3-executor` に二重定義されている

同名概念が別実装で共存しており、どれが「本番で使う正」なのか読み手が迷います（将来の事故源）。  
→ 片側へ寄せる/依存関係を整理する方針を決めたいです。

対象: `crates/hip3-risk/src/hard_stop.rs` と `crates/hip3-executor/src/risk.rs`

### C) READY の定義が二重化している（`SubscriptionManager` と `TradingReadyChecker`）

`Executor::on_signal()` は `TradingReadyChecker::is_ready()` を要求（4フラグ: md/orderSnapshot/fillsSnapshot/positionSynced）しますが、現状 bot/WS 側からこれらをセットする結線が見当たりません。  
一方 `WsWriteHandle::is_ready()` は `SubscriptionManager::is_ready()`（bbo/assetCtx/orderUpdates）です。  
→ 「何を READY とするか」を一本化しないと、実運用で “READYのはずなのに発注できない/発注してしまう” が起きます。

対象: `crates/hip3-executor/src/ready.rs` / `crates/hip3-ws/src/subscription.rs`

### D) `userFills` のスキーマ/スナップショット（`isSnapshot`）未対応の可能性

docs/メモでは `userFills` 初回に `isSnapshot:true` が来る前提がありますが、現状 `FillPayload` はそのフィールドを持たず、`as_fill()` は単発 fill としてパースします。  
→ 再接続後の整合（position 初期化）に影響するため、実データでの確認が必要です。

対象: `crates/hip3-ws/src/message.rs` / `about_ws.md`

---

## 次のステップ（推奨順）

1. `crates/hip3-ws/src/connection.rs` の `drain_and_wait()` が内部状態（READY/heartbeat）も更新するようにする（READY詰み回避）
2. `crates/hip3-bot/src/config.rs` / `crates/hip3-bot/src/app.rs` で `user_address` を設定し、起動時に trading subscriptions が確実に有効化されるようにする
3. bot側の結線（post→ExecutorLoop、orderUpdates/userFills→PositionTracker）を実装し、E2Eで「postが成功し続ける」まで到達させる
4. HardStop 完全実装（TrackedOrderへoid反映 + cancel/flatten シーケンス）を進める

