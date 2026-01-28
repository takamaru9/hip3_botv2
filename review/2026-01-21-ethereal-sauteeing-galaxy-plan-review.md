# hip3-ws WsSender 統合計画レビュー（ethereal-sauteeing-galaxy.md）

確認日: 2026-01-21  
対象: `/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md`

---

## 結論

**承諾**。`/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md` は、このまま実装に入って良いレベルです。  
（以降はレビュー履歴として残します。）

---

## 指摘（ブロッカー: 3点）

### 1) 依存関係が循環する（RealWsSenderをhip3-wsに置く案は現状NG）

- 現状の依存: `hip3-executor` → `hip3-ws`（`crates/hip3-executor/Cargo.toml`に既に依存あり）
- 計画の方針: `hip3-ws` に `RealWsSender` を置き、`hip3-executor::WsSender` trait を実装するため `hip3-ws` → `hip3-executor` 依存を追加  
  → **循環依存でビルド不能**

**修正案（どちらかに確定）**
- **A推奨**: `RealWsSender` は `hip3-executor`（または `hip3-bot`）側に置く（`hip3-ws` は `WsWriteHandle`/送信APIを提供するだけ）
- **B**: `WsSender` trait と送信用DTOを `hip3-core` か新規crateへ移動し、双方から依存する（影響範囲が大きい）

---

### 2) request correlation の設計が二重化/不整合（PostRequestManagerの置き場所を確定すべき）

計画は `hip3-ws` に `PostRequestManager`（oneshotでpost応答待ち）を新設していますが、現状 `hip3-executor` 側にも `PostRequestManager`（`crates/hip3-executor/src/executor_loop.rs`）が既に存在し、timeout/requeue/inflight制御と結びついています。

このまま `RealWsSender::send()` が「post応答待ち」になると、`ExecutorLoop` の `on_response_ok/on_response_rejected` が呼ばれず、**timeout扱いで誤再送**になり得ます（特に reduce_only は再キューされる）。

**修正案（どちらかに確定）**
- **A推奨（最小変更）**: 相関管理は **executor側に残す**  
  - `RealWsSender::send()` は「WSへ投入できたか」だけ返す（応答待ちはしない）
  - `hip3-ws` は `post` 応答（id/reason）を message stream に流す
  - `hip3-bot`（統合層）が `post` 応答を受けて `ExecutorLoop::on_response_ok/rejected` を呼ぶ
- **B（大改修）**: 相関管理を **hip3-wsへ移し**、`ExecutorLoop` の `PostRequestManager` と timeout/requeue設計を置き換える

---

### 3) message/serde schema がそのままだと実装で詰まる（WsMessageの曖昧さ + フィールド名）

- `crates/hip3-ws/src/message.rs` の `WsMessage` は `#[serde(untagged)]` で `ChannelMessage` と `WsResponse` が同形（`channel + data`）のため、拡張しても判別が不安定になります。
- `post` リクエスト/レスポンスのJSONは、SDK互換の **field名（`type` や `vaultAddress` 等）** が重要です。計画の `request_type` / `vault_address` のままだとズレやすいので、serde rename前提を明記しておくべきです。

**修正案（例）**
- `WsMessage` は「channel+data」を単一型に寄せ、`channel` の値で上位層が振り分ける（`pong` だけ別型でOK）
- PostRequestは `type_` + `#[serde(rename="type")]`、`vault_address` は `#[serde(rename="vaultAddress")]` など、**rename方針を計画に明記**

---

## 承諾条件（この計画で実装に入ってよい状態）

- [x] `RealWsSender` の配置とcrate依存が循環しない設計に確定している
- [x] request correlation の責務が **executor側/WS側どちらか**に確定し、`ExecutorLoop` のtimeout/requeueと整合している
- [x] `post` と `orderUpdates/userFills` を流すための `hip3-ws` 側メッセージ表現（serde戦略含む）が確定している

---

## 再レビュー（2026-01-21, 修正版反映後）

対象: `/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md`（修正版）

### 結論

**未承諾（あと少し）**。前回の3ブロッカー（循環依存 / correlation二重化 / serde方針不明確）は解消されています。  
ただし、現状の計画のままだと「接続断や未READY時にpostが成功扱いになる」「inflightがリークする」などの事故パスが残るため、以下3点を計画に反映してください。

### 前回ブロッカーの解消状況

- ✅ `RealWsSender` を `hip3-executor` 側に配置（循環依存回避）
- ✅ request correlation を executor側に残す（`ExecutorLoop` のtimeout/requeueと整合）
- ✅ serde rename方針を明記（`type` / `vaultAddress` 等）

---

## 指摘（ブロッカー: 3点）

### 1) `WsWriteHandle::is_ready()` が「接続状態/READY-TRADING」を見ていない

計画の `WsWriteHandle::is_ready()` は「rate limit + channel open」しか見ていません。  
このままだと **WSがDisconnectedでも送信キュー投入が成功扱い**になり、`hip3-executor` 側が `mark_sent()` → timeout/requeue を誤作動させます（特に reduce_only の無限再送リスク）。

**計画へ追記して確定してほしいこと**
- `WsWriteHandle` が「接続state」と「subscriptions ready phase」を参照できる形にする  
  例: `WsWriteHandle { state: Arc<RwLock<ConnectionState>>, subscriptions: Arc<SubscriptionManager>, ... }`
- `is_ready()` は最低限 `state == Connected && subscriptions.is_ready()` を含める（READY-TRADINGのみpost許可）

---

### 2) inflight管理がリークする/責務が曖昧（`record_post_send` と `record_post_response`）

計画では:
- `post()` 内で `record_post_send()` を呼んだ後に `tx.send(...)` しているため、`ChannelClosed` で **inflightが増えたまま**になります。
- `record_post_response()` を bot 側で呼ぶ方針ですが、現状の計画だと bot が同じ `RateLimiter` にアクセスできずコンパイルしません（`WsWriteHandle` もメソッド未定義）。

**計画へ追記して確定してほしいこと（推奨）**
- `record_post_send()` は `tx.send(...)` が成功した後に呼ぶ（ChannelClosed時のリーク防止）
- inflight decrement は transport責務として `hip3-ws::ConnectionManager::handle_text_message()` で `channel == \"post\"` を検出して `record_post_response()` する（bot依存を無くす）
  - どうしてもbot側でやるなら、`WsWriteHandle::record_post_response()` 等のAPIを明記

---

### 3) 既存APIと計画の呼び出しがズレている（実装時に詰まる）

計画スニペット上で以下が現状コードと一致していません（このまま実装に入ると手戻りになります）。
- `ExecutorLoop::on_response_ok/on_response_rejected` は既に存在し、現状は **sync** かつ `ok` は `post_id` のみ（payload不要）
- `PositionTracker` への取り込みは `orderUpdates/userFills` のJSONを **cloid/oid/state/filled** にパースして `PositionTrackerHandle::order_update(...)` 等へ流す必要があるが、計画に具体がない

**計画へ追記して確定してほしいこと**
- bot側のpost応答処理の呼び出し形（引数/async有無）を現行`ExecutorLoop`に合わせて記述
- orderUpdates/userFills の「どのフィールドを見て、どのHandle APIを呼ぶか」を最低限1パターンで確定（TrackedOrderへoidを入れる前提に直結するため）

---

## 再々レビュー（2026-01-21, 再修正後）

対象: `/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md`

### 結論

**未承諾（残り3点）**。接続状態/READY-TRADINGの導入やinflight扱いの方針は良くなりました。  
ただし、現状の計画スニペットのままだと「実装時にコンパイル不整合」や「positionsが更新されない」などが残ります。

### 解消済み（前回ブロッカー）

- ✅ `WsWriteHandle::is_ready()` が ConnectionState + READY-TRADING を見る
- ✅ `record_post_send()` を `tx.send()` 成功後に移動（inflightリーク抑制）
- ✅ `record_post_response()` を hip3-ws 側で完結させる方針（bot依存排除）
- ✅ bot側のpost応答→`ExecutorLoop::on_response_*` の方向性（sync呼び出し）を明記

---

## 指摘（今回のブロッカー: 3点）

### 1) `RealWsSender` 側のエラー分岐が計画と不整合（`PostError::NotReady` が未ハンドル）

計画の `WsWriteHandle::post()` は `PostError::NotReady` を返し得ますが、`RealWsSender::send()` の `match` で扱っていません（このままだと実装時に漏れます）。

**計画に追記して確定**
- `Err(PostError::NotReady)` を `SendResult::Disconnected`（retryable）等にマップする

あわせて、`ConnectionManager` の rate limiter フィールド名/共有方法（現状コードは `_rate_limiter`）も、計画上の表記を実装に合わせて統一してください。

---

### 2) bot側の `orderUpdates` パース例が `hip3-core/hip3-position` の既存APIとズレている

スニペット内に以下の齟齬があります（このまま実装すると詰まります）。
- `ClientOrderId::new(update.cloid)` は存在しない（`ClientOrderId::from_string(update.cloid)` などが必要）
- `oid.map(OrderId::new)` の `OrderId` 型が存在しない（現状は `Option<u64>` のまま渡す）
- `PositionTrackerHandle::order_update(...).await?` は戻り値が `()` のため `?` が使えない
- `OrderState::from_str(...)` が未実装（どこでどう変換するか要確定）

**計画に追記して確定**
- cloid/oid/state の変換・エラーハンドリング方針を「実在するAPI」に合わせた形で記述する

---

### 3) `userFills` をログのみ扱いにすると PositionTracker の positions が更新されない

現状の `hip3-position` 実装では、positions 更新は `PositionTrackerMsg::Fill`（= `PositionTrackerHandle::fill()`）で行われます。  
`orderUpdates` の `filled_size` だけでは **価格/約定情報が不足**し、PositionTrackerの `positions_snapshot()` が正しく育ちません。

**計画に追記して確定**
- `userFills` をパースして `PositionTrackerHandle::fill(market, side, price, size, ts)` を呼ぶ（少なくともポジション更新はここで行う）
- `orderUpdates` は TrackedOrder の state/filled/oid 更新（oid追跡）に使う、と役割分担を明確化する

---

## 修正確認（2026-01-21）

`/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md` で、前回指摘の3点は ✅ 修正確認できました。

- `PostError::NotReady` のマッピング追加: `/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md:312`
- `orderUpdates` パース例の主要齟齬解消（`ClientOrderId::new`/`OrderId`/`.await?`/`OrderState::from_str` の撤去）: `/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md:355`
- `userFills` で `PositionTrackerHandle::fill()` を呼ぶ方針に変更: `/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md:417`

---

## 再々々レビュー（2026-01-21, 最新）

### 結論

**承諾**。再々々レビューの3点も修正確認できたため、この計画で実装に入ってOKです。

### 修正確認

- bot側サンプルの制御フロー修正（`ok()?`/`unwrap_or_else(return)` 撤去）: `/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md:416`
- `orderUpdates` 購読用の送信API（`WsWriteHandle::send_text()`）追加: `/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md:191`
- 切断/再接続時の inflight リセット方針（`RateLimiter::reset_inflight()`）明記: `/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md:298`
