# Review Findings Fix Plan

## Metadata
| Item | Value |
|------|-------|
| Date | 2026-01-24 |
| Version | 1.6 |
| Status | `[DRAFT]` |
| Source Review | `review/stateless-growing-stallman-implementation-review.md` |

## 参照した一次情報

| 項目 | ソース | URL | 確認日 |
|------|--------|-----|--------|
| Order Status一覧 | Hyperliquid GitBook | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint | 2026-01-24 |
| WsOrder構造 | Hyperliquid GitBook | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions | 2026-01-24 |

## 未確認事項（実測必須）

| 項目 | 理由 | 実測方法 |
|------|------|----------|
| 部分約定時のstatus文字列 | ドキュメントに明記なし | Testnetで部分約定発生させて確認 |

---

## Summary

3つのFindingを修正する計画：

| ID | Severity | Issue | 修正方針 |
|----|----------|-------|----------|
| F1 | High | Mark price欠損時にGateがfail open | Fail closed: mark_px欠損時はReject |
| F2 | Medium | WS shutdown pathがtaskを終了しない | shutdown信号をmessage loopでチェック |
| F3 | Medium | orderUpdates statusマッピング不完全 | 公式docs基準で全statusをマッピング |

---

## F1: Mark Price欠損時のGate Fail Open

### 問題
- Gate 3 (per-market): `mark_px`が`None`の場合、チェックを完全スキップ
- Gate 4 (total): 既存ポジションの`mark_px`が`None`なら0として計算（過小評価）

### 修正方針: Fail Closed

**原則**: リスク管理Gateは「安全側にfail」すべき。mark price不明時は発注を拒否。

### 変更内容

#### F1-1: RejectReason追加
**File**: `crates/hip3-core/src/execution.rs`

```rust
pub enum RejectReason {
    // ... existing variants ...
    /// Required market data (mark price) is unavailable
    MarketDataUnavailable,
}
```

#### F1-2: Gate 3修正
**File**: `crates/hip3-executor/src/executor.rs:423-445`

```rust
// Gate 3: MaxPositionPerMarket
// MUST fail closed: if mark_px unavailable, reject order
let mark_px = match self.market_state_cache.get_mark_px(market) {
    Some(px) => px,
    None => {
        warn!(
            market = %market,
            "Gate 3: mark price unavailable, rejecting order"
        );
        return ExecutionResult::rejected(RejectReason::MarketDataUnavailable);
    }
};

let position_notional = self.position_tracker.get_notional(market, mark_px);
let pending_notional = self
    .position_tracker
    .get_pending_notional_excluding_reduce_only(market, mark_px);
let new_order_notional = size.inner() * mark_px.inner();
let total_notional =
    position_notional.inner() + pending_notional.inner() + new_order_notional;

if total_notional >= self.config.max_notional_per_market {
    debug!(...);
    return ExecutionResult::rejected(RejectReason::MaxPositionPerMarket);
}
```

#### F1-3: Gate 4修正
**File**: `crates/hip3-executor/src/executor.rs:447-467, 647-668`

`calculate_total_portfolio_notional`内でmark_pxが欠損しているポジションがあればエラーを返す：

```rust
fn calculate_total_portfolio_notional(&self) -> Result<Decimal, RejectReason> {
    let positions = self.position_tracker.positions_snapshot();
    let mut total = Decimal::ZERO;

    for pos in positions {
        let mark_px = self.market_state_cache.get_mark_px(&pos.market)
            .ok_or_else(|| {
                warn!(market = %pos.market, "Gate 4: mark price unavailable for position");
                RejectReason::MarketDataUnavailable
            })?;
        total += pos.notional(mark_px).inner();
    }

    // pending notional calculation also needs mark_px validation
    let cache = &self.market_state_cache;
    let pending_result = self
        .position_tracker
        .get_total_pending_notional_with_validation(|market| {
            cache.get_mark_px(market).ok_or(RejectReason::MarketDataUnavailable)
        });

    match pending_result {
        Ok(pending) => {
            total += pending;
            Ok(total)
        }
        Err(reason) => Err(reason),
    }
}
```

Gate 4呼び出し側：
```rust
// Gate 4: MaxPositionTotal
let total_portfolio_notional = match self.calculate_total_portfolio_notional() {
    Ok(total) => total,
    Err(reason) => return ExecutionResult::rejected(reason),
};
// ... rest of gate logic
```

#### F1-4: PositionTracker変更
**File**: `crates/hip3-position/src/tracker.rs`

新しいメソッド追加（`PositionTrackerHandle`に追加）：

**注意**: 実際のデータ構造は `pending_orders_data: Arc<DashMap<ClientOrderId, TrackedOrder>>`。
`TrackedOrder`のフィールドは `reduce_only: bool`, `market: MarketKey`, `size: Size`。

```rust
/// Get total pending notional with validation.
///
/// Unlike `get_total_pending_notional_excluding_reduce_only`, this method
/// returns an error if mark_px is unavailable for any pending order's market.
/// Used for Gate 4 fail-closed validation.
pub fn get_total_pending_notional_with_validation<F, E>(
    &self,
    get_mark_px: F,
) -> Result<Decimal, E>
where
    F: Fn(&MarketKey) -> Result<Price, E>,
{
    let mut total = Decimal::ZERO;
    for entry in self.pending_orders_data.iter() {
        let order = entry.value();
        if !order.reduce_only {
            let mark_px = get_mark_px(&order.market)?;
            let order_notional = order.size.inner() * mark_px.inner();
            total += order_notional;
        }
    }
    Ok(total)
}
```

### テスト

**注意**: 実際のAPIに合わせたテスト設計：
- `Executor::on_signal()` → `ExecutionResult::Rejected { reason }`
- **ポジション作成**: `position_tracker.fill(...).await` を使用（actorがキャッシュを更新）
- **pending追加**: `try_register_order()` (同期) を使用

**既存ヘルパー関数を再利用**（`crates/hip3-executor/src/executor.rs` テストモジュールに既存）：
- `setup_executor()` → `(Executor, PositionTrackerHandle)` を返す
- `sample_market()` → `MarketKey::new(DexId::XYZ, AssetId::new(0))`
- `sample_market_2()` → `MarketKey::new(DexId::XYZ, AssetId::new(1))`

**新規追加ヘルパー**（同テストモジュールに追加）：

```rust
// crates/hip3-executor/src/executor.rs (tests module に追加)

use std::time::Duration;  // wait_for_position で使用

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn sample_tracked_order(market: MarketKey, reduce_only: bool) -> TrackedOrder {
    TrackedOrder::from_pending(PendingOrder::new(
        ClientOrderId::new(),
        market,
        OrderSide::Buy,
        Price::new(dec!(50000)),
        Size::new(dec!(0.1)),
        reduce_only,
        now_ms(),
    ))
}

/// Wait for position to be reflected in cache (deterministic polling).
/// Avoids flaky tests from non-deterministic yield_now().
async fn wait_for_position(pt: &PositionTrackerHandle, market: &MarketKey) {
    const MAX_ATTEMPTS: usize = 100;
    const SLEEP_MS: u64 = 1;

    for _ in 0..MAX_ATTEMPTS {
        if pt.has_position(market) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(SLEEP_MS)).await;
    }
    panic!("Timed out waiting for position to be reflected in cache");
}

// === 新規テスト ===

#[tokio::test]
async fn test_gate3_rejects_when_mark_price_unavailable() {
    // 既存の setup_executor() を使用
    let (executor, _pt) = setup_executor();
    let market = sample_market();
    // Don't set mark price for the target market

    let result = executor.on_signal(
        &market,
        OrderSide::Buy,
        Price::new(dec!(50000)),
        Size::new(dec!(0.1)),
        now_ms(),
    );

    assert!(matches!(
        result,
        ExecutionResult::Rejected { reason: RejectReason::MarketDataUnavailable }
    ));
}

#[tokio::test]
async fn test_gate4_rejects_when_position_mark_price_unavailable() {
    let (executor, pt) = setup_executor();
    let market_a = sample_market();
    let market_b = sample_market_2();

    // Create position for market A via fill (actor updates caches)
    pt.fill(market_a, OrderSide::Buy, Price::new(dec!(50000)), Size::new(dec!(1.0)), now_ms()).await;
    // Wait for actor to process and update cache (deterministic polling)
    wait_for_position(&pt, &market_a).await;

    // Set mark price for target market B, but NOT for market A
    executor.market_state_cache().update(&market_b, Price::new(dec!(3000)), now_ms());

    let result = executor.on_signal(
        &market_b,
        OrderSide::Buy,
        Price::new(dec!(3000)),
        Size::new(dec!(0.1)),
        now_ms(),
    );

    // Should reject because mark price for existing position (market A) is unavailable
    assert!(matches!(
        result,
        ExecutionResult::Rejected { reason: RejectReason::MarketDataUnavailable }
    ));
}

#[tokio::test]
async fn test_gate4_rejects_when_pending_order_mark_price_unavailable() {
    let (executor, pt) = setup_executor();
    let market_a = sample_market();
    let market_b = sample_market_2();

    // Register pending order for market A using try_register_order (sync, updates caches directly)
    let pending_order = sample_tracked_order(market_a, false);
    pt.try_register_order(pending_order).unwrap();

    // Set mark price for target market B, but NOT for market A
    executor.market_state_cache().update(&market_b, Price::new(dec!(3000)), now_ms());

    let result = executor.on_signal(
        &market_b,
        OrderSide::Buy,
        Price::new(dec!(3000)),
        Size::new(dec!(0.1)),
        now_ms(),
    );

    // Should reject because mark price for pending order (market A) is unavailable
    assert!(matches!(
        result,
        ExecutionResult::Rejected { reason: RejectReason::MarketDataUnavailable }
    ));
}

#[tokio::test]
async fn test_gate4_passes_when_all_mark_prices_available() {
    let (executor, pt) = setup_executor();
    let market_a = sample_market();
    let market_b = sample_market_2();

    // Create position and pending order
    pt.fill(market_a, OrderSide::Buy, Price::new(dec!(50000)), Size::new(dec!(1.0)), now_ms()).await;
    wait_for_position(&pt, &market_a).await;
    let pending_order = sample_tracked_order(market_a, false);
    pt.try_register_order(pending_order).unwrap();

    // Set mark prices for ALL markets
    executor.market_state_cache().update(&market_a, Price::new(dec!(50000)), now_ms());
    executor.market_state_cache().update(&market_b, Price::new(dec!(3000)), now_ms());

    let result = executor.on_signal(
        &market_b,
        OrderSide::Buy,
        Price::new(dec!(3000)),
        Size::new(dec!(0.1)),
        now_ms(),
    );

    // Should proceed (not rejected for MarketDataUnavailable)
    assert!(!matches!(
        result,
        ExecutionResult::Rejected { reason: RejectReason::MarketDataUnavailable }
    ));
}
```

---

## F2: WebSocket Shutdown Path

### 問題
- `ConnectionManager::shutdown()`はフラグを設定するのみ
- Message loopはフラグをチェックしない → shutdown後もループ継続
- タイムアウト後に`ws_handle`をabortできない

### 修正方針

1. shutdown信号を`tokio::sync::watch`または`tokio_util::sync::CancellationToken`で伝播
2. Message loopの`select!`にshutdown待機ブランチを追加
3. Graceful close (WebSocket Close frame送信) 後にループ終了

### 変更内容

#### F2-1: CancellationToken導入
**File**: `crates/hip3-ws/src/connection.rs`

```rust
use tokio_util::sync::CancellationToken;

pub struct ConnectionManager {
    // ... existing fields ...
    shutdown_token: CancellationToken,
}

impl ConnectionManager {
    pub fn new(...) -> Self {
        Self {
            // ... existing ...
            shutdown_token: CancellationToken::new(),
        }
    }

    pub fn shutdown(&self) {
        info!("ConnectionManager shutdown requested");
        self.shutdown_token.cancel();
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown_token.is_cancelled()
    }
}
```

#### F2-2: Message Loop修正
**File**: `crates/hip3-ws/src/connection.rs:235-314`

```rust
loop {
    let outbound_recv = async { self.outbound_rx.lock().await.recv().await };

    tokio::select! {
        // Shutdown signal - highest priority
        _ = self.shutdown_token.cancelled() => {
            info!("Shutdown signal received in message loop");
            // Send WebSocket Close frame for graceful disconnect
            if let Err(e) = write.send(Message::Close(None)).await {
                warn!(?e, "Failed to send Close frame");
            }
            *self.state.write() = ConnectionState::Disconnected;
            return Ok(());
        }

        // Incoming message
        msg = read.next() => { ... }

        // Outbound message
        outbound = outbound_recv => { ... }

        // Heartbeat check
        _ = self.heartbeat.wait_for_check() => { ... }
    }
}
```

#### F2-3: Reconnect Loop修正
**File**: `crates/hip3-ws/src/connection.rs:162-216`

backoff sleepをshutdown信号と並行待機：

```rust
// Calculate backoff delay
let delay = self.calculate_backoff_delay(attempt);
warn!(attempt, delay_ms = delay.as_millis(), "Reconnecting");

// Wait for delay OR shutdown signal
tokio::select! {
    _ = tokio::time::sleep(delay) => {}
    _ = self.shutdown_token.cancelled() => {
        info!("Shutdown requested during backoff, exiting");
        *self.state.write() = ConnectionState::Disconnected;
        return Ok(());
    }
}
```

#### F2-4: App側タイムアウト処理
**File**: `crates/hip3-bot/src/app.rs:595-606`

```rust
// Graceful shutdown of WebSocket connection
if let Some(ref cm) = self.connection_manager {
    cm.shutdown();
}

const WS_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

// Use tokio::select! instead of timeout to keep handle for abort
tokio::select! {
    result = &mut ws_handle => {
        match result {
            Ok(Ok(())) => debug!("WebSocket task completed"),
            Ok(Err(e)) => warn!(?e, "WebSocket task error"),
            Err(e) => warn!(?e, "WebSocket task panicked"),
        }
    }
    _ = tokio::time::sleep(WS_SHUTDOWN_TIMEOUT) => {
        warn!("WebSocket shutdown timed out (5s), aborting task");
        ws_handle.abort();
    }
}
```

#### F2-5: Cargo.toml依存追加
**File**: `crates/hip3-ws/Cargo.toml`

```toml
[dependencies]
tokio-util = { version = "0.7", features = ["sync"] }
```

### テスト

**注意**: ConnectionManagerのテストには実際のWebSocket接続またはモックサーバーが必要。

**配置場所**: `crates/hip3-ws/tests/ws_shutdown_test.rs`
（ワークスペース直下の`tests/`はpackageがなく実行対象にならないため、対象crateのtestsディレクトリに配置）

**所有権設計**: `ConnectionManager`は`Clone`未実装のため、`Arc<ConnectionManager>`でラップして共有する。

```rust
// crates/hip3-ws/tests/ws_shutdown_test.rs
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use futures_util::{SinkExt, StreamExt};  // read.next(), write.send() で使用
use hip3_ws::{ConnectionConfig, ConnectionManager};

/// Setup mock WebSocket server and Arc-wrapped ConnectionManager.
/// Returns (Arc<ConnectionManager>, server shutdown handle).
async fn setup_with_mock_server() -> (Arc<ConnectionManager>, tokio::task::JoinHandle<()>) {
    // Start mock WebSocket server on random port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_handle = tokio::spawn(async move {
        // Simple mock: accept connection and echo messages
        if let Ok((stream, _)) = listener.accept().await {
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let (mut write, mut read) = ws.split();
            while let Some(Ok(msg)) = read.next().await {
                if msg.is_close() { break; }
                // Echo back
                let _ = write.send(msg).await;
            }
        }
    });

    let config = ConnectionConfig {
        url: format!("ws://{}", addr),
        ..Default::default()
    };
    let (tx, _rx) = mpsc::channel(100);
    let cm = Arc::new(ConnectionManager::new(config, tx));

    (cm, server_handle)
}

#[tokio::test]
async fn test_shutdown_terminates_message_loop() {
    let (cm, _server) = setup_with_mock_server().await;

    // Clone Arc for shutdown call
    let cm_for_shutdown = Arc::clone(&cm);

    // Start connection in background
    let connect_handle = tokio::spawn(async move {
        cm.connect().await
    });

    // Wait for connection to establish
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Request shutdown via Arc reference
    cm_for_shutdown.shutdown();

    // Connection should terminate within 1 second
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        connect_handle
    ).await;

    assert!(result.is_ok(), "Shutdown should complete within 1 second");
}

#[tokio::test]
async fn test_shutdown_during_backoff_exits_promptly() {
    // Use invalid URL to trigger reconnection loop
    let config = ConnectionConfig {
        url: "ws://127.0.0.1:1".to_string(), // Will fail to connect
        max_reconnect_attempts: 10,
        ..Default::default()
    };
    let (tx, _rx) = mpsc::channel(100);
    let cm = Arc::new(ConnectionManager::new(config, tx));
    let cm_for_shutdown = Arc::clone(&cm);

    let connect_handle = tokio::spawn(async move {
        cm.connect().await
    });

    // Let it enter backoff
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Request shutdown during backoff
    cm_for_shutdown.shutdown();

    // Should exit promptly, not wait for full backoff
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        connect_handle
    ).await;

    assert!(result.is_ok(), "Shutdown during backoff should exit promptly");
}
```

---

## F3: orderUpdates Status Mapping

### 問題
- 現在: `open`, `filled`, `canceled`, `rejected` のみマッピング
- 公式ドキュメント: 20以上のstatus値が存在
- 未知statusは`warn`ログ後に`return` → PositionTrackerに更新が届かない

### 公式Status一覧（2026-01-24確認）

**Active (non-terminal):**
| Status | 意味 |
|--------|------|
| `open` | 正常にポスト |
| `triggered` | トリガー注文が発火 |

**Filled (terminal):**
| Status | 意味 |
|--------|------|
| `filled` | 完全約定 |

**Rejected (terminal):**
| Status | 意味 |
|--------|------|
| `rejected` | ポスト時に拒否 |
| `tickRejected` | tick size不正 |
| `minTradeNtlRejected` | 最小取引額未満 |
| `perpMarginRejected` | 証拠金不足 |
| `reduceOnlyRejected` | reduce only制約違反 |
| `badAloPxRejected` | post-only即時マッチ |
| `iocCancelRejected` | IOCマッチ不可 |
| `badTriggerPxRejected` | TP/SL価格不正 |
| `marketOrderNoLiquidityRejected` | 流動性不足 |
| `positionIncreaseAtOpenInterestCapRejected` | OI上限 |
| `positionFlipAtOpenInterestCapRejected` | OI上限でflip |
| `tooAggressiveAtOpenInterestCapRejected` | OI上限で価格aggressive |
| `openInterestIncreaseRejected` | OI上限違反 |
| `insufficientSpotBalanceRejected` | spot残高不足 |
| `oracleRejected` | oracle価格乖離 |
| `perpMaxPositionRejected` | margin tier上限 |

**Canceled (terminal):**
| Status | 意味 |
|--------|------|
| `canceled` | ユーザーキャンセル |
| `marginCanceled` | 証拠金不足でキャンセル |
| `vaultWithdrawalCanceled` | vault出金でキャンセル |
| `openInterestCapCanceled` | OI上限でキャンセル |
| `selfTradeCanceled` | 自己取引防止 |
| `reduceOnlyCanceled` | reduce only無効化 |
| `siblingFilledCanceled` | TP/SL sibling約定 |
| `delistedCanceled` | 上場廃止 |
| `liquidatedCanceled` | 清算でキャンセル |
| `scheduledCancel` | 予定キャンセル期限超過 |

### 修正方針

1. **パターンマッチで分類**:
   - `*Rejected` → `OrderState::Rejected`
   - `*Canceled` → `OrderState::Cancelled`
2. **明示的マッピング**: `open`, `filled`, `triggered`
3. **未知statusはterminal扱い**: 安全側にfail（pending残留を防ぐ）

### 変更内容

#### F3-1: Status分類ヘルパー
**File**: `crates/hip3-bot/src/app.rs`

```rust
/// Map Hyperliquid order status to internal OrderState.
///
/// Status classification based on:
/// https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint
fn map_order_status(status: &str) -> OrderState {
    match status {
        // Active states (non-terminal)
        "open" => OrderState::Open,
        "triggered" => OrderState::Open, // Trigger order activated, treat as open

        // Filled state
        "filled" => OrderState::Filled,

        // Explicit cancel
        "canceled" => OrderState::Cancelled,

        // Explicit reject
        "rejected" => OrderState::Rejected,

        // Pattern matching for *Rejected statuses
        s if s.ends_with("Rejected") => {
            debug!(status = %s, "Order rejected by exchange");
            OrderState::Rejected
        }

        // Pattern matching for *Canceled statuses
        s if s.ends_with("Canceled") => {
            debug!(status = %s, "Order canceled by exchange");
            OrderState::Cancelled
        }

        // Special case: scheduledCancel (ends with "Cancel", not "Canceled")
        "scheduledCancel" => {
            debug!("Order canceled by scheduled cancel deadline");
            OrderState::Cancelled
        }

        // Unknown status - treat as terminal to avoid pending order leak
        other => {
            warn!(
                status = %other,
                "Unknown order status, treating as cancelled to prevent pending leak"
            );
            OrderState::Cancelled
        }
    }
}
```

#### F3-2: 呼び出し箇所修正
**File**: `crates/hip3-bot/src/app.rs:724-732`

```rust
// Before:
let state = match status.as_str() {
    "open" => OrderState::Open,
    "filled" => OrderState::Filled,
    "canceled" => OrderState::Cancelled,
    "rejected" => OrderState::Rejected,
    other => {
        warn!(status = %other, "Unknown order status");
        return;  // ← 問題: PositionTrackerに更新が届かない
    }
};

// After:
let state = map_order_status(&status);
// No early return - always update PositionTracker
```

#### F3-3: is_terminal修正
**File**: `crates/hip3-ws/src/message.rs:184-191`

```rust
impl OrderUpdatePayload {
    pub fn is_terminal(&self) -> bool {
        let status = self.status.as_str();
        matches!(status, "filled" | "canceled" | "rejected" | "scheduledCancel")
            || status.ends_with("Rejected")
            || status.ends_with("Canceled")
            // Unknown status is also terminal (fail safe)
            || !matches!(status, "open" | "triggered")
    }
}
```

### テスト

```rust
#[test]
fn test_map_order_status() {
    // Active
    assert_eq!(map_order_status("open"), OrderState::Open);
    assert_eq!(map_order_status("triggered"), OrderState::Open);

    // Filled
    assert_eq!(map_order_status("filled"), OrderState::Filled);

    // Explicit cancel/reject
    assert_eq!(map_order_status("canceled"), OrderState::Cancelled);
    assert_eq!(map_order_status("rejected"), OrderState::Rejected);

    // Pattern: *Rejected
    assert_eq!(map_order_status("perpMarginRejected"), OrderState::Rejected);
    assert_eq!(map_order_status("oracleRejected"), OrderState::Rejected);

    // Pattern: *Canceled
    assert_eq!(map_order_status("marginCanceled"), OrderState::Cancelled);
    assert_eq!(map_order_status("liquidatedCanceled"), OrderState::Cancelled);

    // Unknown → Cancelled (fail safe)
    assert_eq!(map_order_status("unknownFutureStatus"), OrderState::Cancelled);

    // Special case: scheduledCancel (ends with "Cancel", not "Canceled")
    assert_eq!(map_order_status("scheduledCancel"), OrderState::Cancelled);
}

/// Helper to create OrderUpdatePayload for testing is_terminal()
///
/// OrderInfo fields (from message.rs):
/// - cloid: Option<String> (#[serde(default)])
/// - oid: u64
/// - coin: String
/// - side: String ("B" or "A")
/// - px: String (#[serde(alias = "limitPx")])
/// - sz: String
/// - orig_sz: String (#[serde(rename = "origSz")])
/// - timestamp: Option<u64> (#[serde(default)])
fn make_order_update(status: &str) -> OrderUpdatePayload {
    OrderUpdatePayload {
        order: OrderInfo {
            cloid: Some("test-cloid".to_string()),
            oid: 12345,
            coin: "BTC".to_string(),
            side: "B".to_string(),
            px: "50000".to_string(),  // NOT limit_px
            sz: "0.1".to_string(),
            orig_sz: "0.1".to_string(),
            timestamp: Some(1234567890),  // Option<u64>
        },
        status: status.to_string(),
        status_timestamp: 1234567890,
    }
}

#[test]
fn test_is_terminal() {
    // Non-terminal statuses
    assert!(!make_order_update("open").is_terminal());
    assert!(!make_order_update("triggered").is_terminal());

    // Terminal: explicit statuses
    assert!(make_order_update("filled").is_terminal());
    assert!(make_order_update("canceled").is_terminal());
    assert!(make_order_update("rejected").is_terminal());

    // Terminal: *Rejected pattern
    assert!(make_order_update("perpMarginRejected").is_terminal());
    assert!(make_order_update("oracleRejected").is_terminal());
    assert!(make_order_update("tickRejected").is_terminal());

    // Terminal: *Canceled pattern
    assert!(make_order_update("marginCanceled").is_terminal());
    assert!(make_order_update("liquidatedCanceled").is_terminal());
    assert!(make_order_update("selfTradeCanceled").is_terminal());

    // Terminal: scheduledCancel (special case - ends with "Cancel")
    assert!(make_order_update("scheduledCancel").is_terminal());

    // Terminal: unknown status (fail safe)
    assert!(make_order_update("unknownFutureStatus").is_terminal());
}
```

---

## Implementation Order

| Order | ID | Task | Estimated Complexity |
|-------|-----|------|---------------------|
| 1 | F3 | orderUpdates status mapping | Low - 単純な関数追加 |
| 2 | F1 | Mark price Gate fail closed | Medium - 複数ファイル変更 |
| 3 | F2 | WS shutdown path | Medium - 非同期ロジック変更 |

### 理由
- F3は独立性が高く、すぐにテスト可能
- F1はリスク管理の最重要修正
- F2は既存動作への影響が最も大きいため最後

---

## Non-Negotiable Lines

1. **F1**: Gate fail closed は必須。`mark_px`欠損時の注文許可は絶対禁止
2. **F3**: 未知statusをterminal扱いにする（pending残留防止）
3. **F2**: shutdown後5秒以内にタスク終了を保証

---

## Residual Risks

| Risk | Mitigation |
|------|------------|
| 部分約定statusが不明 | Testnetで実測、または`sz < origSz && status == "open"`で判定 |
| CancellationToken依存追加 | tokio-utilは広く使われており安定 |
| Unknown statusをCancelledにする副作用 | ログで可視化、実運用で新statusを検知したら追加 |
