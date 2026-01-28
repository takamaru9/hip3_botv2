# orderUpdates 配列形式対応 修正計画

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-01-24 |
| Last Updated | 2026-01-24 |
| Status | `[DRAFT]` |
| Source Review | `review/stateless-growing-stallman-implementation-review.md` |

## Problem Statement

**High Severity**: `orderUpdates` の payload が配列形式 `WsOrder[]` で届く可能性があるが、現在の `as_order_update()` は単一オブジェクトのみをパースしようとする。配列形式で届いた場合、パースが失敗し **注文状態更新を取り逃がす**。

**追加問題**: 公式ドキュメントは `limitPx` / `timestamp` フィールドを示すが、現在の実装は `px` のみ。フィールド名不整合があると要素単位でパース失敗→空配列扱いになる。

## 参照した一次情報

| 項目 | ソース | URL | 確認日 |
|------|--------|-----|--------|
| orderUpdates 仕様 | Hyperliquid GitBook | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions | 2026-01-24 |

### 一次情報からの抜粋

```typescript
// orderUpdates の data フォーマットは WsOrder[]（配列）
interface WsOrder {
  order: WsBasicOrder;
  status: string;
  statusTimestamp: number;
}

interface WsBasicOrder {
  coin: string;
  side: string;
  limitPx: string;  // ← 注意: 現在の実装は `px`
  sz: string;
  oid: number;
  timestamp: number;  // ← 現在の実装にはない
  origSz: string;
  cloid?: string;
}
```

## 未確認事項（実測必須）

| 項目 | 理由 | 実測方法 |
|------|------|----------|
| 空配列の発生 | 初期スナップショットで届くか不明 | Testnet で subscription 直後をログ |

**注**: `px` vs `limitPx` および `timestamp` は P0 で alias/optional 対応するため、実測待ちではなく両対応で進める。

## 現状の実装

### message.rs (L311-318)
```rust
/// Try to parse as order update payload.
pub fn as_order_update(&self) -> Option<OrderUpdatePayload> {
    match self {
        Self::Channel(c) if is_order_updates_channel(&c.channel) => {
            serde_json::from_value(c.data.clone()).ok()
        }
        _ => None,
    }
}
```

**問題**: `c.data` が配列の場合、`OrderUpdatePayload`（単一オブジェクト）へのデシリアライズは失敗する。

### app.rs (L639-647)
```rust
if is_order_updates_channel(channel) {
    if let Some(update) = msg.as_order_update() {
        self.handle_order_update(&update);
    } else {
        warn!(channel = %channel, "Failed to parse orderUpdates message");
    }
    return Ok(());
}
```

**問題**: 配列で届いた場合 `as_order_update()` は None を返し、warning ログのみで処理されない。

---

## Implementation Plan

### P0: 配列形式対応 + フィールド互換 [High]

#### P0-1: `OrderInfo` にフィールド互換対応を追加

**ファイル**: `crates/hip3-ws/src/message.rs`

**理由**: 公式ドキュメントは `limitPx` / `timestamp` を示しており、現在の `px` 固定のままだと要素単位でパース失敗→空配列扱いになる。実測を待たず両対応で進める。

```rust
/// Order information in orderUpdates.
#[derive(Debug, Clone, Deserialize)]
pub struct OrderInfo {
    /// Client order ID (our cloid).
    #[serde(default)]
    pub cloid: Option<String>,
    /// Exchange order ID.
    pub oid: u64,
    /// Coin symbol.
    pub coin: String,
    /// Side: "B" for buy, "A" for sell.
    pub side: String,
    /// Price.
    /// Official docs use "limitPx", but some responses may use "px".
    /// Accept both via alias.
    #[serde(alias = "limitPx")]
    pub px: String,
    /// Size.
    pub sz: String,
    /// Original size.
    #[serde(rename = "origSz")]
    pub orig_sz: String,
    /// Timestamp (optional - present in official docs but may be missing in some responses).
    #[serde(default)]
    pub timestamp: Option<u64>,
}
```

#### P0-2: `as_order_updates()` メソッド追加（失敗数カウント付き）

**ファイル**: `crates/hip3-ws/src/message.rs`

**変更点**: パース失敗数をカウントして返し、呼び出し側でエラー可視性を維持。

```rust
/// Result of parsing order updates.
#[derive(Debug, Clone)]
pub struct OrderUpdatesResult {
    /// Successfully parsed order updates.
    pub updates: Vec<OrderUpdatePayload>,
    /// Number of elements that failed to parse.
    pub failed_count: usize,
}

impl WsMessage {
    /// Try to parse as order update payloads.
    /// Returns OrderUpdatesResult containing parsed updates and failure count.
    ///
    /// # Official format (WsOrder[])
    /// ```json
    /// {"channel": "orderUpdates", "data": [{"order": {...}, "status": "open", ...}]}
    /// ```
    ///
    /// # Legacy format (single object)
    /// ```json
    /// {"channel": "orderUpdates", "data": {"order": {...}, "status": "open", ...}}
    /// ```
    pub fn as_order_updates(&self) -> OrderUpdatesResult {
        match self {
            Self::Channel(c) if is_order_updates_channel(&c.channel) => {
                match &c.data {
                    serde_json::Value::Array(arr) => {
                        // Official format: WsOrder[]
                        let mut updates = Vec::with_capacity(arr.len());
                        let mut failed_count = 0;

                        for v in arr {
                            match serde_json::from_value::<OrderUpdatePayload>(v.clone()) {
                                Ok(payload) => updates.push(payload),
                                Err(e) => {
                                    tracing::debug!(
                                        error = %e,
                                        element = ?v,
                                        "Failed to parse orderUpdate element"
                                    );
                                    failed_count += 1;
                                }
                            }
                        }

                        OrderUpdatesResult { updates, failed_count }
                    }
                    serde_json::Value::Object(_) => {
                        // Legacy format: single object
                        match serde_json::from_value::<OrderUpdatePayload>(c.data.clone()) {
                            Ok(p) => OrderUpdatesResult { updates: vec![p], failed_count: 0 },
                            Err(e) => {
                                tracing::debug!(
                                    error = %e,
                                    "Failed to parse orderUpdate single object"
                                );
                                OrderUpdatesResult { updates: vec![], failed_count: 1 }
                            }
                        }
                    }
                    other => {
                        // Unexpected data type (not Array or Object)
                        tracing::warn!(
                            data_type = ?other,
                            "orderUpdates data is neither Array nor Object"
                        );
                        OrderUpdatesResult { updates: vec![], failed_count: 1 }
                    }
                }
            }
            // Not an orderUpdates channel - not a failure, just not applicable
            _ => OrderUpdatesResult { updates: vec![], failed_count: 0 },
        }
    }
}
```

#### P0-3: lib.rs で re-export

**ファイル**: `crates/hip3-ws/src/lib.rs`

```rust
pub use message::{OrderUpdatesResult, /* 既存のexport */};
```

#### P0-4: app.rs 呼び出し側修正（エラー可視性維持）

**ファイル**: `crates/hip3-bot/src/app.rs`

**変更点**: `failed_count > 0` の場合は warn を出力し、エラー可視性を維持。

```rust
// Handle orderUpdates (Trading mode)
if is_order_updates_channel(channel) {
    let result = msg.as_order_updates();

    // Log parse failures at warn level for visibility
    if result.failed_count > 0 {
        warn!(
            channel = %channel,
            failed_count = result.failed_count,
            parsed_count = result.updates.len(),
            "Some orderUpdate elements failed to parse"
        );
    }

    if result.updates.is_empty() {
        // Empty array (initial snapshot) or all elements failed
        debug!(channel = %channel, "orderUpdates: no updates to process");
    } else {
        for update in result.updates {
            self.handle_order_update(&update);
        }
    }
    return Ok(());
}
```

### P1: テスト追加 [Medium]

**ファイル**: `crates/hip3-ws/tests/subscription_ack_integration.rs`

#### P1-1: 配列形式パーステスト（`limitPx` / `timestamp` 版 = 公式スキーマ）

```rust
/// Test as_order_updates with array format using official schema (limitPx, timestamp)
#[test]
fn test_as_order_updates_array_format_official_schema() {
    let raw = r#"{
        "channel": "orderUpdates",
        "data": [
            {
                "order": {
                    "cloid": "order_001",
                    "oid": 1001,
                    "coin": "ETH",
                    "side": "B",
                    "limitPx": "3000.0",
                    "sz": "0.1",
                    "origSz": "0.1",
                    "timestamp": 1700000000000
                },
                "status": "open",
                "statusTimestamp": 1700000000000
            },
            {
                "order": {
                    "cloid": "order_002",
                    "oid": 1002,
                    "coin": "BTC",
                    "side": "A",
                    "limitPx": "50000.0",
                    "sz": "0.5",
                    "origSz": "0.5",
                    "timestamp": 1700000001000
                },
                "status": "filled",
                "statusTimestamp": 1700000001000
            }
        ]
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");
    assert!(msg.is_order_updates());

    let result = msg.as_order_updates();
    assert_eq!(result.failed_count, 0, "No parse failures expected");
    assert_eq!(result.updates.len(), 2, "Should parse both orders");

    // Verify limitPx was mapped to px field
    assert_eq!(result.updates[0].order.px, "3000.0");
    assert_eq!(result.updates[0].order.coin, "ETH");
    assert_eq!(result.updates[0].order.timestamp, Some(1700000000000));

    assert_eq!(result.updates[1].order.px, "50000.0");
    assert!(result.updates[1].is_terminal());
}
```

#### P1-2: 配列形式パーステスト（`px` 版 = 後方互換）

```rust
/// Test as_order_updates with array format using px (backward compatibility)
#[test]
fn test_as_order_updates_array_format_px_compat() {
    let raw = r#"{
        "channel": "orderUpdates",
        "data": [
            {
                "order": {
                    "cloid": "order_001",
                    "oid": 1001,
                    "coin": "ETH",
                    "side": "B",
                    "px": "3000.0",
                    "sz": "0.1",
                    "origSz": "0.1"
                },
                "status": "open",
                "statusTimestamp": 1700000000000
            }
        ]
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    let result = msg.as_order_updates();
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.updates.len(), 1);
    assert_eq!(result.updates[0].order.px, "3000.0");
    // timestamp should be None when not provided
    assert_eq!(result.updates[0].order.timestamp, None);
}
```

#### P1-3: 空配列テスト

```rust
/// Test as_order_updates with empty array (initial snapshot)
#[test]
fn test_as_order_updates_empty_array() {
    let raw = r#"{
        "channel": "orderUpdates",
        "data": []
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");
    assert!(msg.is_order_updates());

    let result = msg.as_order_updates();
    assert!(result.updates.is_empty(), "Empty array should return empty vec");
    assert_eq!(result.failed_count, 0, "Empty array is not a failure");
}
```

#### P1-4: 単一オブジェクト（後方互換）テスト

```rust
/// Test as_order_updates with single object (backward compatibility)
#[test]
fn test_as_order_updates_single_object() {
    let raw = r#"{
        "channel": "orderUpdates",
        "data": {
            "order": {
                "oid": 9999,
                "coin": "SOL",
                "side": "B",
                "limitPx": "100.0",
                "sz": "1.0",
                "origSz": "1.0",
                "timestamp": 1700000000000
            },
            "status": "canceled",
            "statusTimestamp": 1700000000000
        }
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    let result = msg.as_order_updates();
    assert_eq!(result.updates.len(), 1, "Single object should return vec of one");
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.updates[0].order.coin, "SOL");
    assert_eq!(result.updates[0].order.px, "100.0"); // limitPx mapped to px
    assert!(result.updates[0].is_terminal());
}
```

#### P1-5: 一部パース失敗テスト（failed_count 検証）

```rust
/// Test as_order_updates with partially invalid array - verify failed_count
#[test]
fn test_as_order_updates_partial_failure() {
    let raw = r#"{
        "channel": "orderUpdates",
        "data": [
            {
                "order": {"oid": 1001, "coin": "ETH", "side": "B", "limitPx": "3000.0", "sz": "0.1", "origSz": "0.1"},
                "status": "open",
                "statusTimestamp": 1700000000000
            },
            {"invalid": "data"},
            {
                "order": {"oid": 1003, "coin": "BTC", "side": "A", "limitPx": "50000.0", "sz": "0.5", "origSz": "0.5"},
                "status": "filled",
                "statusTimestamp": 1700000002000
            }
        ]
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    let result = msg.as_order_updates();
    // Should parse 2 valid orders, skip the invalid one
    assert_eq!(result.updates.len(), 2, "Should skip invalid element and parse valid ones");
    assert_eq!(result.failed_count, 1, "Should report 1 failed element");
}
```

#### P1-6: 単一オブジェクトパース失敗テスト

```rust
/// Test as_order_updates with invalid single object - verify failed_count
#[test]
fn test_as_order_updates_single_object_failure() {
    let raw = r#"{
        "channel": "orderUpdates",
        "data": {"invalid": "object"}
    }"#;

    let msg: WsMessage = serde_json::from_str(raw).expect("parse WsMessage");

    let result = msg.as_order_updates();
    assert!(result.updates.is_empty());
    assert_eq!(result.failed_count, 1, "Single invalid object should report 1 failure");
}
```

### P2: 既存 `as_order_update()` の扱い

**方針**: Deprecate するが削除はしない

```rust
/// Try to parse as order update payload (single object).
///
/// **Deprecated**: Use `as_order_updates()` instead, which handles both
/// array format (official) and single object (legacy).
#[deprecated(since = "0.2.0", note = "Use as_order_updates() which handles array format")]
pub fn as_order_update(&self) -> Option<OrderUpdatePayload> {
    self.as_order_updates().updates.into_iter().next()
}
```

---

## Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| `limitPx` vs `px` 不整合 | Low | P0 で serde alias 両対応済み |
| 配列が常に1要素の可能性 | Low | 両対応しているので問題なし |
| パフォーマンス（Vec アロケーション） | Low | 通常1-2要素、無視できるオーバーヘッド |
| 空配列の正常/異常判定 | Low | failed_count で区別可能、Residual Risk として記録 |

## Residual Risks

| Risk | Status | Notes |
|------|--------|-------|
| 空配列が実際に届くか | 未確認 | 初期スナップショットで届く可能性あり。現在は正常扱い（failed_count=0）。実測後に方針確定。 |

## Review History

| Date | Version | Reviewer | Changes |
|------|---------|----------|---------|
| 2026-01-24 | 1.0 | - | Initial plan |
| 2026-01-24 | 1.1 | Review #1 | P1（`limitPx`/`timestamp`対応）を P0 に統合、`OrderUpdatesResult` で failed_count を返しエラー可視性維持、テストに公式スキーマ版を追加 |
| 2026-01-24 | 1.2 | Rereview | Array/Object 以外の場合に warn + failed_count=1 で可視性維持、コードフェンス閉じ忘れ修正（P0-4, P1-6） |
