# subscriptionResponse ACK パース修正計画

## Metadata

| 項目 | 値 |
|------|-----|
| 作成日 | 2026-01-24 |
| 最終更新 | 2026-01-24 (リレビュー 5 承認) |
| 対象レビュー | `review/stateless-growing-stallman-implementation-review.md` |
| 計画レビュー | `review/2026-01-24-subscriptionResponse-ack-fix-plan-review.md` |
| 計画リレビュー | `review/2026-01-24-subscriptionResponse-ack-fix-plan-rereview.md` |
| 計画リレビュー 2 | `review/2026-01-24-subscriptionResponse-ack-fix-plan-rereview-2.md` |
| 計画リレビュー 3 | `review/2026-01-24-subscriptionResponse-ack-fix-plan-rereview-3.md` |
| 計画リレビュー 4 | `review/2026-01-24-subscriptionResponse-ack-fix-plan-rereview-4.md` |
| 計画リレビュー 5 | `review/2026-01-24-subscriptionResponse-ack-fix-plan-rereview-5.md` (**承認**) |
| 影響範囲 | `crates/hip3-ws/src/connection.rs`, `crates/hip3-ws/src/message.rs`, `crates/hip3-bot/src/app.rs` |
| 優先度 | High (本番 READY-TRADING 遷移に影響) |

---

## 参照した一次情報

| 項目 | ソース | URL | 確認日 |
|------|--------|-----|--------|
| subscriptionResponse 形式 | Hyperliquid GitBook | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket | 2026-01-24 |
| Subscriptions 仕様 | Hyperliquid GitBook | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions | 2026-01-24 |
| Python SDK websocket_manager | GitHub (raw) | https://raw.githubusercontent.com/hyperliquid-dex/hyperliquid-python-sdk/master/hyperliquid/websocket_manager.py | 2026-01-24 |

### Python SDK 参照詳細

**ファイル**: `hyperliquid/websocket_manager.py`
**確認内容**:
- `ws_msg_to_identifier()` 関数でチャネル名からサブスクリプション識別子を抽出
- `orderUpdates` チャネルは `"orderUpdates"` 識別子にマップ
- `userFills` チャネルは `'userFills:{user.lower()}'` 形式で識別

**注**: SDK は subscriptionResponse の ACK パースを明示的に行っておらず、メッセージルーティングのみ。ACK 形式は公式 GitBook を優先参照。

### 公式ドキュメントからの引用

**subscriptionResponse 形式**:
```json
{"channel":"subscriptionResponse","data":{"method":"subscribe","subscription":{"type":"trades","coin":"SOL"}}}
```

**orderUpdates の場合の想定形式**:
```json
{
  "channel": "subscriptionResponse",
  "data": {
    "method": "subscribe",
    "subscription": {
      "type": "orderUpdates",
      "user": "<address>"
    }
  }
}
```

**データメッセージのチャネル名**:
> The server will then start sending messages with the channel property set to the corresponding subscription type (e.g. "allMids")

→ チャネル名は subscription type そのもの（例: `"orderUpdates"`）

---

## 問題の概要

### レビュー指摘 #1 [High]: ACK パースの仕様ズレ

**現状コード** (`connection.rs:341-345`):
```rust
let is_order_updates = channel_msg
    .data
    .get("type")  // ← data.type を見ている
    .and_then(|v| v.as_str())
    .is_some_and(|t| t == "orderUpdates");
```

**公式仕様では**:
- `type` は `data.subscription.type` にある
- `data.type` は存在しない

**結果**: orderUpdates の ACK を認識できず、注文がないアカウントで READY-TRADING に遷移できない。

### レビュー指摘 #2 [Medium]: 回帰テスト不在

- `handle_text_message` の subscriptionResponse 処理をテストするユニットテストが存在しない
- ACK 検知ロジックが変更された場合、検出できない

### 計画レビュー指摘 #1 [High]: チャネル名両対応を先行実装

**現状コード** (`message.rs:280-282`):
```rust
pub fn is_order_updates(&self) -> bool {
    matches!(self, Self::Channel(c) if c.channel.starts_with("orderUpdates:"))
}
```

**公式仕様**: チャネル名は `"orderUpdates"` 固定の可能性が高い

**問題**: `starts_with("orderUpdates:")` では `"orderUpdates"` 単体を取りこぼす

**対応**: 実測前に両対応を入れ、実測後に絞る

### 計画レビュー指摘 #2 [Medium]: method ガード不在

**問題**: `unsubscribe` や error 形式が同チャネルで来た場合、誤って Ready を立てる恐れ

**対応**: `data.method == "subscribe"` をチェックしてから Ready を立てる

### 計画レビュー指摘 #3 [Medium]: 統合テスト不足

**問題**: ユニットテストは `extract_subscription_type` のみ。`handle_text_message` 経由で `order_updates_ready` が更新されることを検証する統合テストが必要。

### リレビュー指摘 #1 [High]: `as_order_update` も両対応が必要

**現状コード** (`message.rs:285-292`):
```rust
pub fn as_order_update(&self) -> Option<OrderUpdatePayload> {
    match self {
        Self::Channel(c) if c.channel.starts_with("orderUpdates:") => {
            serde_json::from_value(c.data.clone()).ok()
        }
        _ => None,
    }
}
```

**問題**: `is_order_updates` だけ両対応にしても、`as_order_update` が旧フォーマットのままだと実データのパースが落ちる

**追加影響箇所**: `app.rs:640` も `starts_with("orderUpdates:")` を使用

### リレビュー指摘 #2 [Medium]: 統合テストがロジック再実装

**問題**: テスト内で処理ロジックを再実装しており、本番コードを変更してもテストが通る可能性がある

**対応**: 共通 helper を本体に抽出し、テストと本番コードの両方から使用

### リレビュー指摘 #3 [Medium/Low]: method ガードの挙動が曖昧

**問題**: `method != "subscribe"` の場合に `return Ok(())` と書いており、subscriptionResponse を downstream に転送しない分岐になり得る

**対応**: Ready 判定のみスキップし、メッセージ処理は継続することを明確化

### リレビュー指摘 #4 [Low]: 未確認事項の表現が不整合

**問題**: 「ドキュメントに明示なし」と書いているが、上段で「channel は subscription type と一致」と引用済み

**対応**: 「ドキュメントでは一致とされるが実測で確認」に修正

### リレビュー 2 指摘 #1 [High]: lib.rs の re-export が不足

**問題**: `use hip3_ws::is_order_updates_channel;` や `use hip3_ws::extract_subscription_type;` は `lib.rs` に `pub use` がないとコンパイルが通らない

**対応**: `crates/hip3-ws/src/lib.rs` に re-export を追加

### リレビュー 2 指摘 #2 [Medium]: 統合テストが実コード経路を検証していない

**問題**: JSON→WsMessage→helper までしかテストしておらず、`handle_text_message` や `SubscriptionManager::mark_order_updates_ready()` の配線ミスを検出できない

**対応**: ACK 処理を関数に切り出し、`connection.rs` 内の `#[tokio::test]` で直接テスト

### リレビュー 2 指摘 #3 [Medium/Low]: downstream 転送のフィルタリング方針が曖昧

**問題**: 「downstream 転送不要」と書いているが、実際に `message_tx.send()` をスキップするのか不明確

**対応**: `subscriptionResponse` は `message_tx.send()` に送らない（内部 ACK 専用）ことを明記し、フィルタリング実装を追加

### リレビュー 2 指摘 #4 [Low]: `as_order_update` exact match テスト不足

**問題**: `channel="orderUpdates"` 単体でのパースまで検証するテストがない

**対応**: P2 テストに exact match でのパース成功テストを追加

---

## 修正計画

### P0 [Critical]: subscriptionResponse ACK パース修正

**対象ファイル**: `crates/hip3-ws/src/connection.rs`

**変更内容**:

```rust
// Before (connection.rs:341-345)
let is_order_updates = channel_msg
    .data
    .get("type")
    .and_then(|v| v.as_str())
    .is_some_and(|t| t == "orderUpdates");

// After
// Handle subscriptionResponse: log and check for orderUpdates ACK
if channel_msg.channel == "subscriptionResponse" {
    debug!(?channel_msg.data, "Received subscription response");

    // Guard: only mark Ready for subscribe ACKs (not unsubscribe/error)
    let is_subscribe = channel_msg
        .data
        .get("method")
        .and_then(|v| v.as_str())
        .is_some_and(|m| m == "subscribe");

    if is_subscribe {
        // Extract subscription type using shared helper
        let subscription_type = extract_subscription_type(&channel_msg.data);

        if subscription_type.is_some_and(|t| t == "orderUpdates") {
            self.subscriptions.mark_order_updates_ready();
        }
    }

    // IMPORTANT: subscriptionResponse は内部 ACK 専用
    // → message_tx.send() に送らない（downstream 転送しない）
    // → return して以降の処理をスキップ
    return Ok(());
}
```

**共通 helper 関数**（`message.rs` または `connection.rs` に追加）:

```rust
/// Extract subscription type from subscriptionResponse data.
///
/// Handles both formats:
/// - Official: `data.subscription.type`
/// - Fallback: `data.type` (legacy compatibility)
pub fn extract_subscription_type(data: &serde_json::Value) -> Option<&str> {
    data.get("subscription")
        .and_then(|s| s.get("type"))
        .and_then(|v| v.as_str())
        .or_else(|| data.get("type").and_then(|v| v.as_str()))
}
```

**理由**:
1. `method == "subscribe"` ガードで unsubscribe/error を除外
2. 共通 helper でテストと本番コードの一貫性を確保
3. subscriptionResponse は内部 ACK → `return Ok()` で downstream 転送を明示的にスキップ

### P0.5 [Critical]: lib.rs re-export 追加

**対象ファイル**: `crates/hip3-ws/src/lib.rs`

**変更内容**:

```rust
// lib.rs に追加（既存の pub use の近くに）

// Subscription response helpers (for ACK parsing and channel matching)
pub use message::{extract_subscription_type, is_order_updates_channel};
```

**理由**:
1. `app.rs` から `use hip3_ws::is_order_updates_channel;` でアクセス可能にする
2. 統合テストから `use hip3_ws::{extract_subscription_type, is_order_updates_channel};` でアクセス可能にする
3. crate 内部では `use crate::message::...` または直接参照

### P1 [High]: チャネル名両対応（全メソッド統一）

**対象ファイル**: `crates/hip3-ws/src/message.rs`, `crates/hip3-bot/src/app.rs`

#### 共通 helper 関数を追加

```rust
// message.rs に追加

/// Check if channel name matches orderUpdates (both formats).
///
/// Supports:
/// - `"orderUpdates"` (per official docs: channel = subscription type)
/// - `"orderUpdates:<user>"` (legacy/alternative format)
#[inline]
pub fn is_order_updates_channel(channel: &str) -> bool {
    channel == "orderUpdates" || channel.starts_with("orderUpdates:")
}
```

#### `is_order_updates` を修正

```rust
// Before (message.rs:280-282)
pub fn is_order_updates(&self) -> bool {
    matches!(self, Self::Channel(c) if c.channel.starts_with("orderUpdates:"))
}

// After
pub fn is_order_updates(&self) -> bool {
    matches!(self, Self::Channel(c) if is_order_updates_channel(&c.channel))
}
```

#### `as_order_update` を修正

```rust
// Before (message.rs:285-292)
pub fn as_order_update(&self) -> Option<OrderUpdatePayload> {
    match self {
        Self::Channel(c) if c.channel.starts_with("orderUpdates:") => {
            serde_json::from_value(c.data.clone()).ok()
        }
        _ => None,
    }
}

// After
pub fn as_order_update(&self) -> Option<OrderUpdatePayload> {
    match self {
        Self::Channel(c) if is_order_updates_channel(&c.channel) => {
            serde_json::from_value(c.data.clone()).ok()
        }
        _ => None,
    }
}
```

#### `app.rs` を修正

```rust
// Before (app.rs:640)
if channel.starts_with("orderUpdates:") {

// After
use hip3_ws::is_order_updates_channel;
if is_order_updates_channel(channel) {
```

**理由**:
1. 共通 helper で判定ロジックを一元化
2. `is_order_updates` と `as_order_update` の整合性を保証
3. `app.rs` も同じ helper を使用して一貫性確保

### P2 [High]: 回帰テスト追加（共通 helper + 実コード経路）

**対象ファイル**:
- `crates/hip3-ws/src/message.rs` (ユニットテスト: helper 関数)
- `crates/hip3-ws/src/connection.rs` (ユニットテスト: ACK 処理)
- `crates/hip3-ws/tests/` (統合テスト)

#### ユニットテスト（message.rs 内）

```rust
#[cfg(test)]
mod subscription_response_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extract_subscription_type_official_format() {
        let data = json!({
            "method": "subscribe",
            "subscription": {
                "type": "orderUpdates",
                "user": "0x1234567890abcdef"
            }
        });

        assert_eq!(extract_subscription_type(&data), Some("orderUpdates"));
    }

    #[test]
    fn test_extract_subscription_type_fallback_format() {
        let data = json!({
            "method": "subscribe",
            "type": "orderUpdates",
            "user": "0x1234567890abcdef"
        });

        assert_eq!(extract_subscription_type(&data), Some("orderUpdates"));
    }

    #[test]
    fn test_extract_subscription_type_empty() {
        let data = json!({});
        assert_eq!(extract_subscription_type(&data), None);
    }

    #[test]
    fn test_is_order_updates_channel_exact_match() {
        assert!(is_order_updates_channel("orderUpdates"));
    }

    #[test]
    fn test_is_order_updates_channel_with_user() {
        assert!(is_order_updates_channel("orderUpdates:0x1234"));
    }

    #[test]
    fn test_is_order_updates_channel_other() {
        assert!(!is_order_updates_channel("userFills"));
        assert!(!is_order_updates_channel("allMids"));
        assert!(!is_order_updates_channel("orderUpdate")); // no 's'
    }
}
```

#### 実コード経路テスト（connection.rs 内）

ACK 処理ロジックを関数に切り出し、直接テスト可能にする。

**注意**: `SubscriptionManager` は内部で `RwLock` を使用しており、`&self` で十分（`&mut` 不要）。

```rust
// connection.rs に追加

/// Process subscriptionResponse ACK and update subscription state.
///
/// Returns `true` if orderUpdates ACK was processed.
/// Extracted as separate function for testability.
///
/// Note: SubscriptionManager uses internal RwLock, so &self is sufficient.
fn process_subscription_response(
    data: &serde_json::Value,
    subscriptions: &SubscriptionManager,  // &self で十分（内部 RwLock）
) -> bool {
    // Guard: only mark Ready for subscribe ACKs
    let is_subscribe = data
        .get("method")
        .and_then(|v| v.as_str())
        .is_some_and(|m| m == "subscribe");

    if !is_subscribe {
        return false;
    }

    let subscription_type = extract_subscription_type(data);

    if subscription_type.is_some_and(|t| t == "orderUpdates") {
        subscriptions.mark_order_updates_ready();
        true
    } else {
        false
    }
}

#[cfg(test)]
mod subscription_response_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_process_subscription_response_official_format() {
        let subs = SubscriptionManager::new();
        let data = json!({
            "method": "subscribe",
            "subscription": {
                "type": "orderUpdates",
                "user": "0x1234"
            }
        });

        let result = process_subscription_response(&data, &subs);

        assert!(result, "Should return true for orderUpdates ACK");
        // ready_state() returns ReadyState struct, check order_updates_ready field
        assert!(subs.ready_state().order_updates_ready, "Should mark order_updates_ready");
    }

    #[test]
    fn test_process_subscription_response_unsubscribe_ignored() {
        let subs = SubscriptionManager::new();
        let data = json!({
            "method": "unsubscribe",
            "subscription": {
                "type": "orderUpdates",
                "user": "0x1234"
            }
        });

        let result = process_subscription_response(&data, &subs);

        assert!(!result, "Should return false for unsubscribe");
        assert!(!subs.ready_state().order_updates_ready, "Should NOT mark ready for unsubscribe");
    }

    #[test]
    fn test_process_subscription_response_other_type() {
        let subs = SubscriptionManager::new();
        let data = json!({
            "method": "subscribe",
            "subscription": {
                "type": "allMids"
            }
        });

        let result = process_subscription_response(&data, &subs);

        assert!(!result, "Should return false for non-orderUpdates");
        assert!(!subs.ready_state().order_updates_ready, "Should NOT mark ready for other types");
    }

    #[test]
    fn test_process_subscription_response_fallback_format() {
        let subs = SubscriptionManager::new();
        let data = json!({
            "method": "subscribe",
            "type": "orderUpdates",
            "user": "0x1234"
        });

        let result = process_subscription_response(&data, &subs);

        assert!(result, "Should handle fallback format");
        assert!(subs.ready_state().order_updates_ready);
    }
}
```

**理由**:
1. 実際の ACK 処理ロジック（`process_subscription_response`）を直接テスト
2. `SubscriptionManager::mark_order_updates_ready()` の呼び出しまで検証
3. method ガードや subscription type 判定のエッジケースをカバー
4. `&SubscriptionManager` で十分（内部 `RwLock` により可変性を確保）

#### 統合テスト（`crates/hip3-ws/tests/subscription_ack_integration.rs`）

```rust
//! Integration tests for subscriptionResponse ACK handling.
//!
//! Tests that the shared helpers work correctly with real message parsing.

use hip3_ws::{WsMessage, extract_subscription_type, is_order_updates_channel};
use serde_json::json;

/// Test full message flow: JSON -> WsMessage -> subscription type extraction
#[test]
fn test_subscription_response_parsing_official_format() {
    let raw = r#"{
        "channel": "subscriptionResponse",
        "data": {
            "method": "subscribe",
            "subscription": {
                "type": "orderUpdates",
                "user": "0x1234567890abcdef"
            }
        }
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    if let WsMessage::Channel(channel_msg) = msg {
        assert_eq!(channel_msg.channel, "subscriptionResponse");

        let method = channel_msg.data.get("method").and_then(|v| v.as_str());
        assert_eq!(method, Some("subscribe"));

        // Use the shared helper (same as production code)
        let sub_type = extract_subscription_type(&channel_msg.data);
        assert_eq!(sub_type, Some("orderUpdates"));
    } else {
        panic!("Expected Channel message");
    }
}

/// Test orderUpdates data message with exact channel name
#[test]
fn test_order_updates_data_message_exact_channel() {
    let raw = r#"{
        "channel": "orderUpdates",
        "data": [{"order": {"oid": 123}}]
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    // Use shared helper
    assert!(msg.is_order_updates());
}

/// Test as_order_update with exact channel match (not just starts_with)
///
/// Note: OrderUpdatePayload is a single object with:
/// - order: { cloid?, oid, coin, side, px, sz, origSz }
/// - status: String
/// - statusTimestamp: u64
#[test]
fn test_as_order_update_exact_channel_match() {
    // Exact channel name "orderUpdates" (per official docs)
    // data is a SINGLE object (not an array)
    let raw = r#"{
        "channel": "orderUpdates",
        "data": {
            "order": {
                "cloid": "hip3_test_001",
                "oid": 12345,
                "coin": "ETH",
                "side": "B",
                "px": "3000.0",
                "sz": "0.1",
                "origSz": "0.1"
            },
            "status": "open",
            "statusTimestamp": 1700000000000
        }
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    assert!(msg.is_order_updates(), "is_order_updates should match exact channel");

    // as_order_update should successfully parse the payload
    let payload = msg.as_order_update();
    assert!(payload.is_some(), "as_order_update should parse exact channel match");

    // Verify parsed fields
    let payload = payload.unwrap();
    assert_eq!(payload.order.coin, "ETH");
    assert_eq!(payload.order.oid, 12345);
    assert_eq!(payload.status, "open");
}

/// Test as_order_update with user suffix (legacy format)
#[test]
fn test_as_order_update_with_user_suffix() {
    // data is a SINGLE object (not an array)
    let raw = r#"{
        "channel": "orderUpdates:0x1234567890abcdef",
        "data": {
            "order": {
                "oid": 99999,
                "coin": "BTC",
                "side": "A",
                "px": "50000.0",
                "sz": "0.5",
                "origSz": "0.5"
            },
            "status": "filled",
            "statusTimestamp": 1700000000000
        }
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    assert!(msg.is_order_updates());
    let payload = msg.as_order_update();
    assert!(payload.is_some(), "as_order_update should parse user suffix format");

    // Verify parsed fields
    let payload = payload.unwrap();
    assert_eq!(payload.order.coin, "BTC");
    assert!(payload.is_terminal(), "filled status should be terminal");
}

/// Test orderUpdates data message with user suffix
#[test]
fn test_order_updates_data_message_with_user() {
    let raw = r#"{
        "channel": "orderUpdates:0x1234567890abcdef",
        "data": [{"order": {"oid": 123}}]
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    assert!(msg.is_order_updates());
}

/// Test channel name helper directly
#[test]
fn test_channel_name_helper() {
    // Exact match (official docs format)
    assert!(is_order_updates_channel("orderUpdates"));

    // With user suffix (legacy format)
    assert!(is_order_updates_channel("orderUpdates:0xabc"));

    // Other channels
    assert!(!is_order_updates_channel("userFills"));
    assert!(!is_order_updates_channel("subscriptionResponse"));
}
```

**理由**:
1. 共通 helper をテストで直接使用（ロジック再実装を回避）
2. JSON パースから helper 呼び出しまでの全フローをテスト
3. **`as_order_update` の exact match テストを追加**（リレビュー 2 指摘 #4 対応）
4. 本番コードと同じ helper を使うため、実装変更時にテストも失敗する

### P3 [Medium]: チャネル名実測と最適化

実装完了後、Testnet で実測:

```bash
RUST_LOG=hip3_ws=debug cargo run --bin hip3-bot -- --config config/testnet.toml
```

**確認ログパターン**:
```
DEBUG hip3_ws::connection: Received channel message channel="orderUpdates" ...
# または
DEBUG hip3_ws::connection: Received channel message channel="orderUpdates:0x..." ...
```

**結果に基づく最適化**:
- `"orderUpdates"` のみ → `is_order_updates_channel` から `starts_with` 分岐を削除
- `"orderUpdates:<user>"` のみ → `== "orderUpdates"` 分岐を削除
- 両方来る → 現状維持

### P4 [Low]: Spec ファイル作成

**対象**: `.claude/specs/2026-01-24-subscriptionResponse-ack-fix.md`

---

## 実装詳細

### Step 1: 共通 helper 追加

**ファイル**: `crates/hip3-ws/src/message.rs`

```rust
/// Extract subscription type from subscriptionResponse data.
///
/// Handles both formats:
/// - Official: `data.subscription.type`
/// - Fallback: `data.type` (legacy compatibility)
pub fn extract_subscription_type(data: &serde_json::Value) -> Option<&str> {
    data.get("subscription")
        .and_then(|s| s.get("type"))
        .and_then(|v| v.as_str())
        .or_else(|| data.get("type").and_then(|v| v.as_str()))
}

/// Check if channel name matches orderUpdates (both formats).
///
/// Supports:
/// - `"orderUpdates"` (per official docs: channel = subscription type)
/// - `"orderUpdates:<user>"` (legacy/alternative format)
#[inline]
pub fn is_order_updates_channel(channel: &str) -> bool {
    channel == "orderUpdates" || channel.starts_with("orderUpdates:")
}
```

### Step 1.5: lib.rs re-export 追加

**ファイル**: `crates/hip3-ws/src/lib.rs`

```rust
// 既存の pub use の近くに追加
pub use message::{extract_subscription_type, is_order_updates_channel};
```

**確認事項**:
- `hip3_ws::extract_subscription_type` で外部からアクセス可能
- `hip3_ws::is_order_updates_channel` で外部からアクセス可能

### Step 2: ACK パース修正 (P0)

**ファイル**: `crates/hip3-ws/src/connection.rs`

**変更箇所**: L334-355 付近

#### 2a. ACK 処理関数を抽出（テスト可能にする）

**注意**: `SubscriptionManager` は内部で `RwLock` を使用しており、`&self` で十分（`&mut` 不要）。

```rust
use crate::message::extract_subscription_type;

/// Process subscriptionResponse ACK and update subscription state.
///
/// Returns `true` if orderUpdates ACK was processed.
/// Extracted as separate function for testability.
///
/// Note: SubscriptionManager uses internal RwLock, so &self is sufficient.
fn process_subscription_response(
    data: &serde_json::Value,
    subscriptions: &SubscriptionManager,  // &self で十分（内部 RwLock）
) -> bool {
    // Guard: only mark Ready for subscribe ACKs
    let is_subscribe = data
        .get("method")
        .and_then(|v| v.as_str())
        .is_some_and(|m| m == "subscribe");

    if !is_subscribe {
        return false;
    }

    let subscription_type = extract_subscription_type(data);

    if subscription_type.is_some_and(|t| t == "orderUpdates") {
        subscriptions.mark_order_updates_ready();
        true
    } else {
        false
    }
}
```

#### 2b. handle_text_message での呼び出し

**注意**: `self.subscriptions` は `Arc<SubscriptionManager>` なので、`&*self.subscriptions` で `&SubscriptionManager` を取得。

```rust
WsMessage::Channel(channel_msg) => {
    // Handle subscriptionResponse: internal ACK only
    if channel_msg.channel == "subscriptionResponse" {
        debug!(?channel_msg.data, "Received subscription response");

        // Process ACK using extracted function
        // Note: Arc<SubscriptionManager> から &SubscriptionManager を取得
        //   - &self.subscriptions → &Arc<SubscriptionManager> (型不一致)
        //   - &*self.subscriptions → &SubscriptionManager (正しい)
        process_subscription_response(&channel_msg.data, &*self.subscriptions);

        // IMPORTANT: subscriptionResponse は内部 ACK 専用
        // → message_tx.send() には送らない（downstream 転送しない）
        // → return して以降の処理をスキップ
        return Ok(());
    }

    // Handle error channel (log only, but still forward downstream)
    if channel_msg.channel == "error" {
        warn!(?channel_msg.data, "Received error channel message");
        // Note: error は forward する（アプリ側で処理する可能性あり）
    }

    // ... rest of channel handling (orderUpdates data, userFills, etc.)
    // These messages ARE forwarded downstream via message_tx.send()
}
```

**Downstream フィルタリング方針**:
| チャネル | 転送 | 理由 |
|----------|------|------|
| `subscriptionResponse` | ❌ 送らない | 内部 ACK、アプリロジック不要 |
| `error` | ✅ 送る | アプリ側でエラーハンドリング可能 |
| `orderUpdates` | ✅ 送る | メインのデータフロー |
| `userFills` | ✅ 送る | メインのデータフロー |

### Step 3: チャネル名両対応 (P1)

**ファイル**: `crates/hip3-ws/src/message.rs`

```rust
pub fn is_order_updates(&self) -> bool {
    matches!(self, Self::Channel(c) if is_order_updates_channel(&c.channel))
}

pub fn as_order_update(&self) -> Option<OrderUpdatePayload> {
    match self {
        Self::Channel(c) if is_order_updates_channel(&c.channel) => {
            serde_json::from_value(c.data.clone()).ok()
        }
        _ => None,
    }
}
```

**ファイル**: `crates/hip3-bot/src/app.rs`

```rust
// L640 付近
use hip3_ws::is_order_updates_channel;

if is_order_updates_channel(channel) {
    // ... existing logic
}
```

### Step 4: テスト追加 (P2)

上記「回帰テスト追加」セクション参照。

**テスト構成**:

| ファイル | テスト内容 | 目的 |
|----------|------------|------|
| `message.rs` | helper 関数のユニットテスト | `extract_subscription_type`, `is_order_updates_channel` の単体動作確認 |
| `connection.rs` | `process_subscription_response` のユニットテスト | ACK 処理ロジック（method ガード、Ready 設定）の検証 |
| `tests/subscription_ack_integration.rs` | 統合テスト | JSON パース → helper → `as_order_update` の全フロー |

**注意点**:
- `connection.rs` のテストでは `SubscriptionManager` の状態変化まで検証
- 統合テストでは `as_order_update` の exact match (`channel="orderUpdates"`) パースを含める

---

## 未確認事項（実測必須）

| 項目 | 理由 | 実測方法 |
|------|------|----------|
| orderUpdates データチャネル名 | ドキュメントでは subscription type と一致とされるが、実測で確認が必要 | Testnet で実際の受信メッセージを確認 |
| subscriptionResponse エラー形式 | ドキュメントに記載なし | Testnet で無効なユーザーアドレスを購読 |
| フォールバック形式の必要性 | 実際の API が `data.subscription.type` のみか不明 | Testnet での実測 |

---

## 検証手順

### 自動テスト

```bash
# 1. 単体テスト（共通 helper）
cargo test -p hip3-ws -- subscription_response

# 2. 統合テスト
cargo test -p hip3-ws --test subscription_ack_integration

# 3. 全テスト
cargo test --workspace
```

### 手動検証

1. **Testnet 接続テスト**
   - 新規アカウント（注文なし）で接続
   - READY-TRADING に遷移することを確認

2. **ACK ログ確認**
   ```bash
   RUST_LOG=hip3_ws=debug cargo run ...
   # "OrderUpdates subscription ACKed, marking ready" が出力されることを確認
   ```

3. **既存アカウント（注文あり）テスト**
   - 従来通り動作することを確認（リグレッションなし）

4. **チャネル名形式確認**
   - DEBUG ログで `channel=` の値を確認
   - P3 の最適化に反映

---

## リスク評価

| リスク | 影響 | 軽減策 |
|--------|------|--------|
| フォールバック不要で無駄なコード | 低 | 実測後に不要なら削除 |
| チャネル名両対応で予期しない動作 | 低 | テストでカバー |
| method ガードで正常 ACK を弾く | 中 | 実測で method 存在を確認 |
| 実測結果と異なる形式が本番で来る | 中 | ログ出力 + 監視 |
| downstream 非転送で将来 ACK が必要になる | 低 | 現時点では不要。必要時に再検討 |

---

## 完了条件

- [ ] P0: ACK パース修正完了（`process_subscription_response` 関数抽出 + method ガード + 共通 helper 使用）
- [ ] P0: downstream フィルタリング実装（`subscriptionResponse` は `return Ok()` で転送スキップ）
- [ ] P0.5: lib.rs re-export 追加（`extract_subscription_type`, `is_order_updates_channel`）
- [ ] P1: チャネル名両対応完了（`is_order_updates`, `as_order_update`, `app.rs` すべて統一）
- [ ] P2: 回帰テスト追加完了
  - [ ] `message.rs`: helper 関数ユニットテスト
  - [ ] `connection.rs`: `process_subscription_response` ユニットテスト（実コード経路）
  - [ ] `tests/`: 統合テスト（`as_order_update` exact match 含む）
- [ ] P3: チャネル名形式の実測と最適化
- [ ] P4: Spec ファイル作成
- [ ] 全テスト pass
- [ ] Testnet で手動検証完了

---

## レビュー対応履歴

| 日付 | レビュー | 対応 |
|------|----------|------|
| 2026-01-24 | `review/2026-01-24-subscriptionResponse-ack-fix-plan-review.md` | 指摘 #1-4 を反映 |
| 2026-01-24 | `review/2026-01-24-subscriptionResponse-ack-fix-plan-rereview.md` | 指摘 #1-4 を反映 |
| 2026-01-24 | `review/2026-01-24-subscriptionResponse-ack-fix-plan-rereview-2.md` | 指摘 #1-4 を反映 |
| 2026-01-24 | `review/2026-01-24-subscriptionResponse-ack-fix-plan-rereview-3.md` | 指摘 #1-2 を反映 |
| 2026-01-24 | `review/2026-01-24-subscriptionResponse-ack-fix-plan-rereview-4.md` | 指摘 #1 を反映 |
| 2026-01-24 | `review/2026-01-24-subscriptionResponse-ack-fix-plan-rereview-5.md` | **承認** (指摘なし) |

### 初回レビュー対応詳細

1. **[High] チャネル名両対応を P1 に昇格**: P2 → P1 に変更、実測前に両対応実装
2. **[Medium] 統合テスト追加**: P2 に統合テストセクション追加
3. **[Medium/Low] method ガード追加**: P0 に `is_subscribe` チェック追加
4. **[Low] Python SDK 参照元明記**: 「参照した一次情報」セクションに詳細追加

### リレビュー対応詳細

1. **[High] `as_order_update` も両対応**: P1 に `as_order_update` と `app.rs:640` の修正を追加、共通 helper `is_order_updates_channel` を導入
2. **[Medium] 統合テストの実効性改善**: 共通 helper を本体に抽出し、テストと本番コードの両方から使用する方式に変更
3. **[Medium/Low] method ガードの挙動明確化**: コメントで「Ready 判定のみスキップ、subscriptionResponse は内部 ACK なので downstream 転送不要」と明記
4. **[Low] 未確認事項の表現修正**: 「ドキュメントに明示なし」→「ドキュメントでは subscription type と一致とされるが、実測で確認が必要」に修正

### リレビュー 2 対応詳細

1. **[High] lib.rs re-export 追加**: P0.5 を新設し、`crates/hip3-ws/src/lib.rs` に `pub use message::{extract_subscription_type, is_order_updates_channel};` を追加する計画を追記。これにより `use hip3_ws::is_order_updates_channel;` が有効になる
2. **[Medium] 実コード経路テスト追加**: ACK 処理を `process_subscription_response()` 関数に切り出し、`connection.rs` 内の `#[cfg(test)]` で `SubscriptionManager` の状態変化まで検証するテストを追加。これにより配線ミス（helper 呼び忘れ、method ガード条件違い）を検出可能
3. **[Medium/Low] downstream フィルタリング明確化**: `subscriptionResponse` は `return Ok();` で明示的に転送スキップ。フィルタリング方針表を追加（subscriptionResponse: 送らない、error: 送る、orderUpdates/userFills: 送る）
4. **[Low] `as_order_update` exact match テスト追加**: 統合テストに `test_as_order_update_exact_channel_match()` を追加。`channel="orderUpdates"` でのパース成功を検証

### リレビュー 3 対応詳細

1. **[High] `process_subscription_response` の型修正**:
   - `&mut SubscriptionManager` → `&SubscriptionManager` に変更
   - `SubscriptionManager` は内部で `RwLock` を使用しており、`&self` で十分
   - テストで `subs.order_updates_ready()` → `subs.ready_state().order_updates_ready` に修正（既存 API に合わせる）
   - `Arc<SubscriptionManager>` からの参照取得は `&*self.subscriptions` で可能（`&self.subscriptions` は `&Arc<...>` になるため不可）

2. **[Medium] 統合テストのペイロード修正**:
   - `data` は**配列ではなく単一オブジェクト**に修正
   - フィールド名を実際の `OrderUpdatePayload` 構造に合わせる:
     - `limitPx` → `px`
     - `timestamp` → 削除（`OrderInfo` に存在しない）
     - `origSz` はそのまま（正しい）
   - `OrderInfo` のフィールド: `cloid?`, `oid`, `coin`, `side`, `px`, `sz`, `origSz`
   - テストでパース結果のフィールド値も検証するよう追加

### リレビュー 4 対応詳細

1. **[High] Arc 参照の渡し方修正**:
   - `&self.subscriptions` → `&*self.subscriptions` に修正
   - `&self.subscriptions` は `&Arc<SubscriptionManager>` を返す（型不一致）
   - `&*self.subscriptions` で `Arc` を deref して `&SubscriptionManager` を取得
   - リレビュー 3 対応詳細の誤った記述も修正
