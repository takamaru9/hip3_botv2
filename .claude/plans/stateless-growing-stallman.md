# BUG-004 再調査計画

## Overview

BUG-004（READY-TRADING state not achieved）の根本原因を解決する計画。

**問題の核心**: 現行実装は `orderUpdates` の**実データ受信**で READY-TRADING になるが、
注文がないアカウントでは `orderUpdates` メッセージが来ない可能性がある。

---

## 調査結果

### インスタンス共有: 正常

`Arc<SubscriptionManager>` により同一インスタンスが共有されている（問題なし）。

```
ConnectionManager.subscriptions: Arc<...> ──┬──► SubscriptionManager (単一)
WsWriteHandle.subscriptions: Arc<...> ──────┘     │
App.connection_manager ─────────────────────────► is_ready() で参照
```

### 問題箇所

| 箇所 | 内容 |
|------|------|
| `subscription.rs:317-326` | `orderUpdates:*` **実データ受信**で `order_updates_ready = true` |
| 問題 | 注文がなければ orderUpdates メッセージは来ない可能性 |

---

## 設計判断

### 採用: Option A-1（ACK で READY にする）

`subscriptionResponse` の**ACK受信**時点で `order_updates_ready = true` にする。

**理由**:
- 注文が無くても READY-TRADING に遷移可能
- Hyperliquid の実データ配信タイミングに依存しない

---

## エラー検知方針（Review 5/6/7 対応）

### 方針: 暫定ロジックを最小に留める

**公式Docsでエラー形式は未定義** → **実測で確定するまで**エラー判定ロジックは最小限。

### 想定される形式（実測で確定必須）

| 形式 | チャンネル | 検知方法 | 実装時期 |
|------|----------|----------|----------|
| 形式A | `subscriptionResponse` | `data.error` フィールド存在確認 | **実測後** |
| 形式B | `error` | `channel == "error"` をチェック | **実測後** |

### 実測前の実装（Step 2a）

```rust
// ログ出力のみ（エラー処理なし）
if channel_msg.channel == "subscriptionResponse" {
    debug!(?channel_msg.data, "Received subscription response");
    // 成功ケースのみ処理（orderUpdates ACK）
}

if channel_msg.channel == "error" {
    warn!(?channel_msg.data, "Received error channel message");
    // ⚠️ エラー処理は実測後に追加
}
```

### 実測後の実装（Step 2b）

エラー形式が確定した後、該当形式のエラー処理を追加。

---

## ACK 失敗の伝播経路（Review 5/6 対応）

### 問題

`drain_and_wait()` は `handle_text_message()` のエラーを**握りつぶす**（L515-517）:
```rust
if let Err(e) = self.handle_text_message(&text).await {
    warn!(?e, "Error handling message during drain");  // ← 伝播しない！
}
```

`handle_text_message()` でエラーを返しても `restore_subscriptions()` には届かない。

### 解決策: `drain_and_wait()` を変更してエラーを伝播

**Review 6 対応**: 経路を一本化するため、`drain_and_wait()` でエラーを上位に伝播させる。

**変更内容**:
1. `drain_and_wait()` 内で `handle_text_message()` のエラーを `return Err(e)` で伝播
2. `restore_subscriptions()` がエラーを受け取り、呼び出し元に伝播
3. `try_connect()` がエラーを受け取り、再接続ループに戻る

**影響範囲**:
- `drain_and_wait()` の戻り値の意味が変わる（成功/失敗）
- 呼び出し元（`restore_subscriptions()`）は既に `WsResult<()>` を返すため互換性あり

---

## 修正計画

### 変更ファイル

| ファイル | 変更内容 |
|----------|----------|
| `crates/hip3-ws/src/connection.rs` | (1) handle_text_message で ACK 解析・エラー返却, (2) drain_and_wait でエラー伝播, (3) mark_order_updates_ready() 呼び出し |
| `crates/hip3-ws/src/subscription.rs` | `mark_order_updates_ready()` メソッド追加 |
| `crates/hip3-ws/src/message.rs` | （任意・実測後）SubscriptionResponse 型追加 |

### Step 1: subscription.rs に専用メソッド追加

```rust
// crates/hip3-ws/src/subscription.rs

impl SubscriptionManager {
    /// Mark orderUpdates as ready (ACK-based).
    /// Called when subscriptionResponse for orderUpdates is received.
    pub fn mark_order_updates_ready(&self) {
        let mut state = self.ready_state.write();
        if !state.order_updates_ready {
            info!("OrderUpdates subscription ACKed, marking ready");
            state.order_updates_ready = true;
        }
    }
}
```

### Step 2: connection.rs で ACK 解析（2段階）

#### Step 2a: 実測用（最小ロジック）

**実測完了まで**は以下の最小ロジックのみ実装:

```rust
// crates/hip3-ws/src/connection.rs の handle_text_message() 内
// WsMessage::Channel(channel_msg) => { ... } ブロック

// subscriptionResponse: ログ出力 + ACK成功判定のみ
if channel_msg.channel == "subscriptionResponse" {
    // ⚠️ 実測用: 形式を確認するためにログ出力
    debug!(?channel_msg.data, "Received subscription response");

    // ACK 成功: subscription 種別を判定（成功ケースのみ）
    // 公式Docs: "data は元の subscription を返す" → data 自体が subscription
    if let Some(sub_type) = channel_msg.data.get("type").and_then(|v| v.as_str()) {
        if sub_type == "orderUpdates" {
            self.subscriptions.mark_order_updates_ready();
        }
    }
}

// error チャンネル: ログ出力のみ（実測で存在確認後に処理追加）
if channel_msg.channel == "error" {
    warn!(?channel_msg.data, "Received error channel message");
    // ⚠️ 実測後: エラー処理を追加
}
```

#### Step 2b: 実測後（完全ロジック）

**実測でエラー形式が確定した後**に以下を追加:

```rust
// subscriptionResponse 内にエラーがある場合（形式Aが確認された場合のみ）
if channel_msg.channel == "subscriptionResponse" {
    // エラーチェック（実測で確認された形式に基づく）
    if let Some(error) = channel_msg.data.get("error") {
        warn!(?error, "Subscription failed");
        return Err(WsError::SubscriptionError(
            format!("Subscription error: {:?}", error)
        ));
    }

    // ACK 成功: data.type で判定（data 自体が subscription）
    if let Some(sub_type) = channel_msg.data.get("type").and_then(|v| v.as_str()) {
        if sub_type == "orderUpdates" {
            self.subscriptions.mark_order_updates_ready();
        }
    }
}

// error チャンネル（形式Bが確認された場合のみ）
if channel_msg.channel == "error" {
    warn!(?channel_msg.data, "Received error channel message");
    return Err(WsError::SubscriptionError(
        format!("Error channel: {:?}", channel_msg.data)
    ));
}
```

**重要**: Step 2b は**実測で形式が確定してから**実装する。

### Step 3: drain_and_wait() でエラー伝播

`drain_and_wait()` の L515-517 を変更し、エラーを上位に伝播させる:

```rust
// crates/hip3-ws/src/connection.rs の drain_and_wait() 内
// 変更前:
if let Err(e) = self.handle_text_message(&text).await {
    warn!(?e, "Error handling message during drain");
}

// 変更後:
self.handle_text_message(&text).await?;  // エラーを上位に伝播
```

**これにより**:
1. `handle_text_message()` で `WsError::SubscriptionError` を返すと
2. `drain_and_wait()` がそのエラーを伝播し
3. `restore_subscriptions()` がエラーを受け取り
4. `try_connect()` が再接続ループに戻る

**追加変更不要**: 既存の呼び出しチェーンが `WsResult<()>` を返すため、エラー伝播は自動的に機能する。

---

## subscriptionResponse の仕様（⚠️ 実測必須）

### 現状

Hyperliquid の `subscriptionResponse` の **正確な payload 形式は未確認**。

**公式Docs**: 「subscriptionResponse の data は元の subscription を返す」と記載。
**エラー形式は Docs に記載なし** → 実測で確定するまで未定義として扱う。

### 実装手順（順序重要）

| Phase | 内容 | 必須 |
|-------|------|------|
| Phase 0 | `debug!(?channel_msg.data)` のみ追加し Testnet で実測 | ✅ |
| Phase 1 | 成功時の形式を確認（`data` の構造） | ✅ |
| Phase 2 | 失敗時の形式を確認（`error` フィールド or `channel:"error"` or 別形式） | ✅ |
| Phase 3 | 確定した仕様に基づきパース/判定ロジックを実装 | ✅ |

**重要**: Phase 0-2 を完了するまで、エラー判定ロジックは**最小限**（ログ出力のみ）に留める。

### 想定形式（仮・実測で確定すること）

```json
// 成功時（公式Docsに基づく: "data は元の subscription を返す"）
{
  "channel": "subscriptionResponse",
  "data": { "type": "orderUpdates", "user": "0x..." }
}

// 失敗時（⚠️ 推測のみ - 実測で確定必須）
// パターンA: subscriptionResponse 内にエラー（未確認）
{
  "channel": "subscriptionResponse",
  "data": {
    "error": "Invalid user address"
  }
}

// パターンB: 専用エラーチャンネル（未確認）
{
  "channel": "error",
  "data": {
    "message": "Subscription failed: Invalid user address"
  }
}
```

---

## ACK 成功/失敗の判定基準（実測後）

### 成功判定

- `channel == "subscriptionResponse"` かつ `data.error` が存在しない
- `data.type == "orderUpdates"` の場合、`mark_order_updates_ready()` 呼び出し
  - 公式Docs: 「data は元の subscription を返す」→ `data` 自体が subscription

### 失敗判定

- `channel == "subscriptionResponse"` かつ `data.error` が存在
- または `channel == "error"`

### 失敗時の動作

| 失敗種別 | 動作 |
|----------|------|
| orderUpdates 失敗 | `WsError::SubscriptionError` を返す → 再接続 |
| bbo/activeAssetCtx 失敗 | `WsError::SubscriptionError` を返す → 再接続 |
| userFills 失敗 | warn ログのみ（READY に影響しない） |

**重要**: エラーバリアントは既存の `WsError::SubscriptionError(String)` を使用。

---

## Verification

### Step 1: subscriptionResponse の確認

1. Testnet で起動
2. ログで `subscriptionResponse` の内容を確認
3. エラー時の形式を特定（`data.error` か `channel:"error"` か）

### Step 2: orderUpdates 初回挙動の実測

| 手順 | 確認項目 |
|------|----------|
| 注文が無いアカウントで起動 | `subscriptionResponse` で ACK が来るか |
| ACK 受信後 | `OrderUpdates subscription ACKed` ログ確認 |
| READY-TRADING 表示 | 遷移するか確認 |

### Step 3: エラーハンドリング確認

| 手順 | 確認項目 |
|------|----------|
| 無効な user_address で subscription | エラー検知 → 再接続されるか |
| ログに `Subscription error` or `Error channel` | 表示されるか |

---

## 関連ファイル

- `crates/hip3-ws/src/connection.rs:334-346` - handle_message 呼び出し箇所
- `crates/hip3-ws/src/connection.rs:485-535` - drain_and_wait（エラー握りつぶし箇所）
- `crates/hip3-ws/src/subscription.rs:317-326` - READY-TRADING 判定ロジック
- `crates/hip3-ws/src/error.rs` - WsError 定義（SubscriptionError を使用）
- `crates/hip3-bot/src/app.rs:509` - is_ready() 呼び出し箇所

---

## Review 5/6/7/10 対応まとめ

| 指摘 | 対応 |
|------|------|
| エラーバリアント選択 | `WsError::SubscriptionError(String)` を使用 |
| `drain_and_wait` がエラーを握りつぶす | **drain_and_wait() を変更してエラー伝播**（経路一本化） |
| エラーが `channel:"error"` で来る可能性 | 両形式をチェック（**実測後**に実装） |
| コード例の不整合 | 統一したコード例を記載 |
| エラー伝播経路が不明確（Review 6） | `drain_and_wait()` → `restore_subscriptions()` → `try_connect()` の経路を明確化 |
| subscriptionResponse のエラー形式は Docs に記載なし（Review 6/7） | **実測後に確定**、暫定ロジックは最小限 |
| 暫定ロジックを最小に留める（Review 7） | **Step 2 を 2a/2b に分割**: 2a=実測用（ログのみ）、2b=実測後（エラー処理） |
| data 形状の前提不一致（Review 10） | **`data.type` ベースに統一**: 公式Docs「data は元の subscription を返す」に準拠 |
