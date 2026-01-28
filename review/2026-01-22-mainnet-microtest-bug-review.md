# Mainnet Micro-Test Bug Review

## Executive Summary

| Bug ID | 概要 | 判定 | Severity |
|--------|------|------|----------|
| BUG-004 | READY-TRADING state not achieved | **FALSE POSITIVE** | N/A |
| BUG-005 | userFills parse failure warning | **CONFIRMED** | Low |

---

## BUG-004: READY-TRADING State Not Achieved

### 判定: FALSE POSITIVE（バグではない）

### 当初の仮説（bug fileより）

> orderUpdates messages are not notifying SubscriptionManager, preventing READY-TRADING state.

バグファイルでは `app.rs` で `subscriptions.handle_message()` が呼ばれていないことが原因と指摘されていた。

### 調査結果

**`subscriptions.handle_message()` は既に `connection.rs` で呼び出されている。**

`crates/hip3-ws/src/connection.rs:334-346`:
```rust
WsMessage::Channel(channel_msg) => {
    // Log subscription responses
    if channel_msg.channel == "subscriptionResponse" {
        debug!("Received subscription response");
    }
    // Decrement inflight on post response (transport responsibility)
    if channel_msg.channel == "post" {
        self.rate_limiter.record_post_response();
        debug!("Post response received, inflight decremented");
    }
    // Update subscription state for market data channels
    self.subscriptions.handle_message(&channel_msg.channel);  // <-- 全チャンネルで呼び出し
}
```

### データフロー分析

```
WebSocket メッセージ受信
        ↓
ConnectionManager.handle_text_message() (connection.rs:317-355)
        ↓
subscriptions.handle_message(&channel_msg.channel)  <-- READY-TRADING更新
        ↓
message_tx.send(msg)  <-- Appに転送
        ↓
App.handle_message() (app.rs:613-675)
        ↓
handle_order_update()  <-- 注文状態更新（別目的）
```

### マッチング条件

`crates/hip3-ws/src/subscription.rs:40-42`:
```rust
pub fn matches(&self, channel: &str) -> bool {
    channel.contains(self.channel_pattern())
}
```

`RequiredChannel::OrderUpdates.channel_pattern()` は `"orderUpdates"` を返すため、`"orderUpdates:0x..."` 形式のチャンネルは正しくマッチする。

### テストでの確認

`crates/hip3-ws/src/subscription.rs:486-491`:
```rust
manager.handle_message("bbo:BTC");
manager.handle_message("activeAssetCtx:perp:0");
manager.handle_message("orderUpdates:user:abc");

assert!(manager.is_ready());  // PASS
```

### READY-TRADINGが達成されない場合の実際の原因候補

| 候補 | 詳細 | 確認方法 |
|------|------|----------|
| 1. user_address未設定 | Trading subscriptionがスキップされる | configのuser_address確認 |
| 2. orderUpdates subscription失敗 | WebSocket subscription responseでエラー | ログで "subscriptionResponse" 確認 |
| 3. orderUpdatesメッセージ未受信 | 注文がない場合、初回メッセージが来ない可能性 | 手動で小さい注文を出してテスト |

### 重要な知見

**READY-TRADING達成には `orderUpdates` チャンネルから最低1回メッセージを受信する必要がある。**

注文がない場合、Hyperliquid WebSocketは `orderUpdates` メッセージを送信しない可能性がある。これは「バグ」ではなく「仕様」の可能性。

### 推奨アクション

- [ ] **調査**: メインネットログで `orderUpdates:` メッセージの有無を確認
- [ ] **調査**: `user_address` が正しく設定されているか確認
- [ ] **調査**: subscription response でエラーがないか確認
- [ ] **検討**: 初回orderUpdatesなしでもREADY-TRADING判定する仕様変更

---

## BUG-005: userFills Parse Failure Warning

### 判定: CONFIRMED BUG（確認済みバグ）

### 症状

```
2026-01-22T14:33:48.752037Z WARN hip3_bot::app: Failed to parse userFills message
```

### 根本原因

Hyperliquidの `userFills` サブスクリプションは初回接続時に **スナップショット形式** を送信する。

### 現在の実装の問題

`crates/hip3-ws/src/message.rs:300-307`:
```rust
pub fn as_fill(&self) -> Option<FillPayload> {
    match self {
        Self::Channel(c) if c.channel == "userFills" => {
            serde_json::from_value(c.data.clone()).ok()  // 単一オブジェクト期待
        }
        _ => None,
    }
}
```

`FillPayload` は以下のフィールドを持つ単一のfillオブジェクトを期待:
```rust
pub struct FillPayload {
    pub coin: String,
    pub side: String,
    pub px: String,
    pub sz: String,
    pub time: u64,
    // ...
}
```

### 想定されるHyperliquid初回レスポンス

**形式A: スナップショット**
```json
{
  "channel": "userFills",
  "data": {
    "isSnapshot": true,
    "fills": []
  }
}
```

**形式B: 空配列**
```json
{
  "channel": "userFills",
  "data": []
}
```

どちらの形式も `FillPayload` の構造と一致しないため、パースに失敗する。

### 既存ドキュメントでの記載

`about_ws.md:40`:
> `userFills` は初回に `isSnapshot:true` が来る

`review/2026-01-21-ethereal-sauteeing-galaxy-implementation-rereview.md:85-90`:
> `userFills` のスキーマ/スナップショット（`isSnapshot`）未対応の可能性

### 影響度

| 項目 | 評価 |
|------|------|
| Severity | **Low** |
| Functional Impact | なし（warning only） |
| User Impact | ログノイズ |
| 修正緊急度 | **Low** |

### 修正方針

#### Option A: 最小限の修正（推奨）

`crates/hip3-bot/src/app.rs` でwarningをdebugに変更し、空配列/スナップショットを無視:

```rust
// Handle userFills (Trading mode)
if channel == "userFills" {
    if let Some(fill) = msg.as_fill() {
        self.handle_user_fill(&fill);
    } else {
        // Initial subscription may return snapshot/empty format
        debug!("userFills message not a fill (possibly initial/empty)");
    }
    return Ok(());
}
```

**Pros:**
- 最小限のコード変更
- ログノイズ解消
- リスクが低い

**Cons:**
- スナップショットデータは無視される

#### Option B: 完全対応

1. `FillPayload` を拡張して `isSnapshot` フィールド対応
2. `as_fills()` メソッド追加で配列対応
3. スナップショット受信でREADY状態更新

```rust
// message.rs
#[derive(Debug, Clone, Deserialize)]
pub struct UserFillsSnapshot {
    #[serde(rename = "isSnapshot")]
    pub is_snapshot: bool,
    pub fills: Vec<FillPayload>,
}

impl WsMessage {
    pub fn as_fills_snapshot(&self) -> Option<UserFillsSnapshot> {
        match self {
            Self::Channel(c) if c.channel == "userFills" => {
                serde_json::from_value(c.data.clone()).ok()
            }
            _ => None,
        }
    }
}
```

**Pros:**
- 正式なスナップショット対応
- 将来の機能拡張に有利

**Cons:**
- コード変更量が多い
- テストケース追加必要

### 推奨

**Option A（最小限の修正）を推奨。**

理由:
1. 現状warningのみで機能的影響なし
2. スナップショットデータは現状使用していない
3. リスク最小化優先

---

## Summary

| Bug ID | 判定 | アクション |
|--------|------|-----------|
| BUG-004 | FALSE POSITIVE | 追加調査（ログ確認、user_address確認） |
| BUG-005 | CONFIRMED | Option A修正を推奨（warn → debug変更） |

## Related Files

- `bug/BUG-004-ready-trading-not-achieved.md` - 元バグレポート
- `bug/BUG-005-userfills-parse-failure.md` - 元バグレポート
- `crates/hip3-ws/src/connection.rs:334-346` - handle_message呼び出し箇所
- `crates/hip3-ws/src/subscription.rs:324-327` - READY-TRADING判定ロジック
- `crates/hip3-ws/src/message.rs:300-307` - as_fill()実装
- `crates/hip3-bot/src/app.rs:649-657` - userFills処理

---

## Review Date

2026-01-22
