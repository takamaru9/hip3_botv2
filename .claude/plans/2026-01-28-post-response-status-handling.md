# Post Response Status Handling - Fix orderUpdate Not Received Issue

## Metadata

| Item | Value |
|------|-------|
| Created | 2026-01-28 |
| Updated | 2026-01-28 |
| Task | Post responseのstatusesを活用してorderUpdateが来ない問題を修正 |
| Status | `[PLANNING]` |

## 問題の概要

### 発生した問題
- GOLD注文がPost response OK（post_id: 1）を受け取った
- しかし、orderUpdateがWebSocketから受信されなかった
- pending_notionalが解放されず、後続の注文がすべて拒否された

### タイムライン
| 時刻 | イベント | 状態 |
|------|---------|------|
| 10:55:14.766 | OrderUpdates subscription ACKed | ✅ |
| 10:55:27.628 | GOLD Order queued | ✅ |
| 10:55:27.923 | Post response OK | ✅ |
| ❌ | Order update received | **なし** |

### 根本原因
orderUpdateメッセージが届かない場合、pending_notionalが解放されない。
原因は特定できていないが、以下が考えられる：
1. Hyperliquid側の一時的な問題
2. IOC注文が即座にfilled/canceledした場合のorderUpdate配信漏れ
3. WebSocketメッセージのドロップ

## 解決策

### 方針
Post responseの`statuses`配列を活用して、orderUpdateを待たずに注文状態を把握する。

### Hyperliquid API statuses format

```json
// Resting (オーダーブックに残っている)
{"resting": {"oid": 77738308}}

// Filled (即座に約定)
{"filled": {"totalSz": "0.02", "avgPx": "1891.4", "oid": 77747314}}

// Error (拒否)
{"error": "Order must have minimum value of $10."}
```

### 参照した一次情報

| 項目 | ソース | URL | 確認日 |
|------|--------|-----|--------|
| Order Response Format | Hyperliquid GitBook | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint | 2026-01-28 |

## 実装計画

### Step 1: OrderStatus型の定義 (hip3-ws/src/message.rs)

```rust
/// Order status from post response.
#[derive(Debug, Clone)]
pub enum OrderResponseStatus {
    /// Order is resting on order book.
    Resting { oid: u64 },
    /// Order was immediately filled.
    Filled {
        oid: u64,
        total_sz: String,
        avg_px: String,
    },
    /// Order was rejected.
    Error { message: String },
}

impl ActionResponsePayload {
    /// Parse statuses array from response.
    pub fn parse_statuses(&self) -> Vec<OrderResponseStatus> {
        // self.response.data contains {"statuses": [...]}
        // Parse and return Vec<OrderResponseStatus>
    }
}
```

### Step 2: ExecutorLoopにstatuses処理を追加 (hip3-executor/src/executor.rs)

```rust
impl ExecutorLoop {
    /// Handle post response with statuses.
    pub fn on_response_with_statuses(
        &self,
        post_id: u64,
        statuses: Vec<OrderResponseStatus>,
    ) {
        for status in statuses {
            match status {
                OrderResponseStatus::Filled { oid, total_sz, avg_px } => {
                    // 即座に約定 → pending解放、ポジション更新
                    self.handle_immediate_fill(post_id, oid, total_sz, avg_px);
                }
                OrderResponseStatus::Error { message } => {
                    // 拒否 → pending解放
                    self.handle_order_rejected(post_id, message);
                }
                OrderResponseStatus::Resting { oid } => {
                    // オープン → oid記録、orderUpdateを待つ
                    self.record_oid_mapping(post_id, oid);
                }
            }
        }
    }
}
```

### Step 3: app.rsのPost response処理を更新

```rust
// 現在
PostResponseBody::Action { .. } => {
    executor_loop.on_response_ok(resp.id);
    debug!(post_id = resp.id, "Post response OK");
}

// 修正後
PostResponseBody::Action { ref payload } => {
    let statuses = payload.parse_statuses();
    executor_loop.on_response_with_statuses(resp.id, statuses);
    debug!(
        post_id = resp.id,
        statuses = ?statuses,
        "Post response OK with statuses"
    );
}
```

### Step 4: タイムアウト機構（安全装置）

pending_orderに対して10秒のタイムアウトを設定：
- orderUpdateまたはPost responseでstatusが確認されなければ
- 10秒後にpending_orderを強制解放
- 警告ログを出力

```rust
// PositionTrackerに追加
pub fn check_order_timeouts(&mut self, timeout_duration: Duration) {
    let now = Instant::now();
    let timed_out: Vec<_> = self.pending_orders
        .iter()
        .filter(|(_, order)| now.duration_since(order.created_at) > timeout_duration)
        .map(|(cloid, _)| cloid.clone())
        .collect();

    for cloid in timed_out {
        warn!(cloid = %cloid, "Pending order timed out, releasing");
        self.remove_pending_order(&cloid);
    }
}
```

## 修正ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `crates/hip3-ws/src/message.rs` | OrderResponseStatus型、parse_statuses()追加 |
| `crates/hip3-executor/src/executor.rs` | on_response_with_statuses()追加 |
| `crates/hip3-bot/src/app.rs` | Post response処理更新 |
| `crates/hip3-position/src/tracker.rs` | タイムアウト機構追加 |

## 未確認事項（実測必須）

| 項目 | 理由 | 実測方法 |
|------|------|----------|
| IOC注文のstatuses形式 | IOCが即座にfillした場合のレスポンス | Testnetで小額注文を送信 |
| 複数注文のstatuses順序 | バッチ注文時の順序保証 | 使用していないため不要 |

## 検証計画

### 1. ユニットテスト
- parse_statuses()のテスト（resting, filled, error各パターン）
- on_response_with_statuses()のモックテスト

### 2. ローカル実行
```bash
cargo fmt && cargo clippy && cargo check
cargo run --bin hip3-bot -- -c config/default.toml
# ダッシュボードで注文フローを確認
```

### 3. VPSデプロイ
```bash
git add -A
git commit -m "fix: Handle order statuses from post response to prevent pending leak"
git push
# VPS で pull & rebuild
```

## 優先度

**高**: この問題により、1つの注文失敗で全取引が停止する
