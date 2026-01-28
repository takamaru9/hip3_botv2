# Mainnet Test Failure Fix Plan

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-01-24 |
| Source Bug Report | `bug/2026-01-24-mainnet-test-failure.md` |
| Priority | CRITICAL |
| Status | **IMPLEMENTED v1.5** |

## 参照した一次情報

| 項目 | ソース | URL | 確認日 |
|------|--------|-----|--------|
| WebSocket POST形式 | Hyperliquid GitBook | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/post-requests | 2026-01-24 |
| Exchange Endpoint | Hyperliquid GitBook | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint | 2026-01-24 |
| Python SDK signing.py | GitHub | https://github.com/hyperliquid-dex/hyperliquid-python-sdk | 2026-01-24 |

## 未確認事項（実測必須）

| 項目 | 理由 | 実測方法 |
|------|------|----------|
| 精度制限の正確な値 | ドキュメントに明記なし | 修正後にTestnetで確認 |
| CL市場のasset index | 設定値の正確性未確認 | API meta endpointで確認 |

---

## Executive Summary

Mainnetテストで注文が全て失敗した。根本原因は **Signature r/s フィールドに `0x` プレフィックスがない** こと。追加で **注文価格/サイズの精度制限未適用** も発見。

**Note**: 17:06 JST テストで発覚した SpecCache 初期化問題は別計画で対応:
→ `.claude/plans/2026-01-24-speccache-initialization-fix.md`

---

## Issue Summary

| ID | Severity | Issue | Impact |
|----|----------|-------|--------|
| P0 | CRITICAL | Signature r/s に 0x prefix なし | 全注文がJSON parseエラーで失敗 |
| P1 | HIGH | 価格/サイズ精度制限未適用 | 25桁のサイズ送信、取引所でリジェクトの可能性 |
| P2 | MEDIUM | コメント/テストが "no 0x prefix" と記載 | メンテナンスリスク |
| P3 | MEDIUM | ActionSignature::from_bytes で v 未正規化 | 0/1 入力時に無効な signature |
| P4 | LOW | BBO stale閾値が低流動性市場に不適切 | 不要なGate発火 |
| P5 | LOW | CL市場が動作していない | 設定ミスの可能性 |

---

## P0: Signature r/s に 0x prefix 追加 (CRITICAL)

### 問題

**現在のコード** (`crates/hip3-executor/src/executor_loop.rs:388-389`):
```rust
r: hex::encode(signature.r().to_be_bytes::<32>()),
s: hex::encode(signature.s().to_be_bytes::<32>()),
```

**出力**: `"r": "fddbcef86728..."`

**Python SDK** (`hyperliquid/utils/signing.py`):
```python
return {"r": to_hex(signed["r"]), "s": to_hex(signed["s"]), "v": signed["v"]}
```

**出力**: `"r": "0x609cb20c7379..."`

### 修正

**ファイル**: `crates/hip3-executor/src/executor_loop.rs`

**変更箇所** (L388-389):
```rust
// BEFORE
r: hex::encode(signature.r().to_be_bytes::<32>()),
s: hex::encode(signature.s().to_be_bytes::<32>()),

// AFTER
r: format!("0x{}", hex::encode(signature.r().to_be_bytes::<32>())),
s: format!("0x{}", hex::encode(signature.s().to_be_bytes::<32>())),
```

### 影響範囲

- `executor_loop.rs` のみ
- `ActionSignature::from_bytes` は別途 P3 で対応

### 根拠の確認

**一次情報の状況**:
- Hyperliquid公式ドキュメント: signature構造は示されているが `0x` 必須の明記なし
- Python SDK (`signing.py`): `to_hex()` を使用 → `0x` 付き
- サードパーティ実装例: `0x` 付き

**AB テスト計画 (Testnetで検証)**:

| テスト | r/s形式 | 期待結果 |
|--------|---------|----------|
| A | `fddbcef...` (0x なし) | JSON parse error |
| B | `0xfddbcef...` (0x あり) | 成功 |

手順:
1. 現在のコード（0xなし）でTestnet注文送信 → エラー確認
2. 0xプレフィックス追加 → Testnet注文送信 → 成功確認
3. 両方のレスポンスをログに記録

### テスト

1. 既存の単体テストが通ることを確認
2. Testnet ABテストで `0x` 有無の影響を確認
3. 成功パターンを本番適用

---

## P1: 価格/サイズ精度制限の適用 (HIGH)

### 問題

**現在のコード** (`crates/hip3-executor/src/signer.rs:258-261`):
```rust
pub fn from_pending_order(order: &hip3_core::PendingOrder) -> Self {
    Self {
        limit_px: order.price.inner().to_string(),  // 精度制限なし
        sz: order.size.inner().to_string(),         // 精度制限なし
        // ...
    }
}
```

**問題の出力**: `"s": "0.1928733304402333767298326824"` (25桁)

**期待される出力**: `"s": "0.19287"` (5有効数字、sz_decimals制限適用)

### 調査結果

**現在の構造** (2026-01-24調査):

| 構造体 | `SpecCache` を持つか |
|--------|---------------------|
| `ExecutorLoop` | **No** |
| `Executor` | **No** |
| `App` (`hip3-bot`) | **Yes** (`Arc<SpecCache>`) |

`ExecutorLoop` のフィールド:
- `executor: Arc<Executor>`
- `nonce_manager: Arc<NonceManager<SystemClock>>`
- `signer: Arc<Signer>`
- `ws_sender: Option<DynWsSender>`
- `post_request_manager: PostRequestManager`
- `vault_address: Option<Address>`
- `interval: Duration`

### 修正アプローチ

**方法C: `from_pending_order` のシグネチャ変更 + `ExecutorLoop` に `SpecCache` 追加**

依存関係を最小限にしつつ、必要な情報を渡せるようにする。

### Step 1: `ExecutorLoop` に `spec_cache` フィールドを追加

**ファイル**: `crates/hip3-executor/src/executor_loop.rs`

**現在の構造体** (L237付近):
```rust
pub struct ExecutorLoop {
    interval: Duration,
    executor: Arc<Executor>,
    nonce_manager: Arc<NonceManager<SystemClock>>,
    signer: Arc<Signer>,
    ws_sender: Option<DynWsSender>,
    post_request_manager: PostRequestManager,
    vault_address: Option<Address>,
}
```

**変更後**:
```rust
pub struct ExecutorLoop {
    interval: Duration,
    executor: Arc<Executor>,
    nonce_manager: Arc<NonceManager<SystemClock>>,
    signer: Arc<Signer>,
    ws_sender: Option<DynWsSender>,
    post_request_manager: PostRequestManager,
    vault_address: Option<Address>,
    spec_cache: Arc<SpecCache>,  // 追加
}
```

### Step 2: コンストラクタを更新

**ファイル**: `crates/hip3-executor/src/executor_loop.rs`

**現在のシグネチャ** (L254-270):
```rust
pub fn new(
    executor: Arc<Executor>,
    nonce_manager: Arc<NonceManager<SystemClock>>,
    signer: Arc<Signer>,
    timeout_ms: u64,
) -> Self
```

**変更後**:
```rust
pub fn new(
    executor: Arc<Executor>,
    nonce_manager: Arc<NonceManager<SystemClock>>,
    signer: Arc<Signer>,
    timeout_ms: u64,
    spec_cache: Arc<SpecCache>,  // 追加
) -> Self {
    Self {
        interval: executor.batch_scheduler().interval(),
        executor,
        nonce_manager,
        signer,
        ws_sender: None,
        post_request_manager: PostRequestManager::new(timeout_ms),
        vault_address: None,
        spec_cache,  // 追加
    }
}
```

**`with_ws_sender` も更新** (L273付近):
```rust
pub fn with_ws_sender(
    executor: Arc<Executor>,
    nonce_manager: Arc<NonceManager<SystemClock>>,
    signer: Arc<Signer>,
    ws_sender: DynWsSender,
    timeout_ms: u64,
    spec_cache: Arc<SpecCache>,  // 追加
) -> Self {
    Self {
        interval: executor.batch_scheduler().interval(),
        executor,
        nonce_manager,
        signer,
        ws_sender: Some(ws_sender),
        post_request_manager: PostRequestManager::new(timeout_ms),
        vault_address: None,
        spec_cache,  // 追加
    }
}
```

### Step 3: `OrderWire::from_pending_order` のシグネチャを変更

**ファイル**: `crates/hip3-executor/src/signer.rs`

```rust
// BEFORE
pub fn from_pending_order(order: &hip3_core::PendingOrder) -> Self

// AFTER
pub fn from_pending_order(order: &hip3_core::PendingOrder, spec: &hip3_core::MarketSpec) -> Self {
    use hip3_core::OrderSide;
    let is_buy = matches!(order.side, OrderSide::Buy);
    Self {
        asset: order.market.asset.0 as u32,
        is_buy,
        limit_px: spec.format_price(order.price, is_buy),  // tick丸め + 5 sig figs
        sz: spec.format_size(order.size),                  // lot丸め + 5 sig figs
        reduce_only: order.reduce_only,
        order_type: OrderTypeWire::ioc(),
        cloid: Some(order.cloid.to_string()),
    }
}
```

### Step 4: `batch_to_action` の型変更とエラーハンドリング

**ファイル**: `crates/hip3-executor/src/executor_loop.rs`

**重要**: `SpecCache::get()` は `Option<MarketSpec>` を返す。起動直後や市場追加時に `None` になる可能性があるため、**パニックしない設計**が必要。

#### 4.1: ExecutorError にバリアント追加

**ファイル**: `crates/hip3-executor/src/error.rs`

```rust
#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("Order submission failed: {0}")]
    SubmissionFailed(String),

    #[error("Order rejected: {0}")]
    OrderRejected(String),

    #[error("Connection error: {0}")]
    ConnectionError(String),

    #[error("Rate limited")]
    RateLimited,

    // 追加
    #[error("MarketSpec not found for market: {0}")]
    MarketSpecNotFound(hip3_core::MarketKey),
}
```

#### 4.2: `batch_to_action` の戻り値型を変更

**現在のシグネチャ** (L434):
```rust
fn batch_to_action(&self, batch: &ActionBatch) -> Action
```

**変更後**:
```rust
fn batch_to_action(&self, batch: &ActionBatch) -> Result<Action, ExecutorError>
```

#### 4.3: 実装 (バッチ全体失敗方式)

**方針**: SpecCache 未充足時は**バッチ全体を失敗**とし、`handle_send_failure` で適切にクリーンアップ/再キューする。

理由:
- 部分スキップは `reduce_only` 注文の消失リスクがある
- `handle_send_failure` は既に reduce_only の再キューをサポート
- シンプルで予測可能な挙動

```rust
fn batch_to_action(&self, batch: &ActionBatch) -> Result<Action, ExecutorError> {
    match batch {
        ActionBatch::Orders(orders) => {
            let mut order_wires = Vec::with_capacity(orders.len());
            for order in orders {
                let spec = self.spec_cache.get(&order.market)
                    .ok_or_else(|| {
                        tracing::warn!(
                            market = %order.market,
                            cloid = %order.cloid,
                            "MarketSpec not found, failing batch"
                        );
                        ExecutorError::MarketSpecNotFound(order.market.clone())
                    })?;
                order_wires.push(OrderWire::from_pending_order(order, &spec));
            }
            Ok(Action {
                action_type: "order".to_string(),  // 必須フィールド
                orders: Some(order_wires),
                cancels: None,
                grouping: Some("na".to_string()),  // Option<String>
                builder: None,
            })
        }
        ActionBatch::Cancels(cancels) => {
            // Cancels は MarketSpec 不要
            // CancelWire::from_pending_cancel は存在しないため手動マッピング
            let cancel_wires: Vec<CancelWire> = cancels
                .iter()
                .map(|c| CancelWire {
                    asset: c.market.asset.0 as u32,
                    oid: c.oid,
                })
                .collect();
            Ok(Action {
                action_type: "cancel".to_string(),  // 必須フィールド
                orders: None,
                cancels: Some(cancel_wires),
                grouping: None,  // cancel には grouping 不要（公式仕様）
                builder: None,
            })
        }
    }
}
```

#### 4.4: `tick()` 内での呼び出し元を更新

**tick() のシグネチャ**: `pub async fn tick(&self, now_ms: u64) -> Option<u64>`

**現在のフロー** (L356-363):
```rust
// 4. Create request (not yet sent)
let (post_id, _rx) = self
    .post_request_manager
    .create_request(batch.clone(), now_ms);

// 5. Convert batch to action
let action = self.batch_to_action(&batch);
```

**変更後** (post_id 生成を batch_to_action 成功後に移動):
```rust
// 4. Convert batch to action (may fail if SpecCache not ready)
let action = match self.batch_to_action(&batch) {
    Ok(action) => action,
    Err(e) => {
        tracing::error!(error = %e, "Failed to build action from batch");
        // SpecCache 未充足 → post_id 生成前なので remove 不要
        // handle_send_failure と同等のクリーンアップを実行
        self.handle_batch_conversion_failure(batch).await;
        return None;  // Option<u64> を返す
    }
};

// 5. Create request (only after action build succeeds)
let (post_id, _rx) = self
    .post_request_manager
    .create_request(batch.clone(), now_ms);
```

**ポイント**:
- `batch_to_action()` を `create_request()` より前に移動
- 失敗時は post_id が生成されていないため `post_request_manager.remove()` 不要
- 戻り値は `return None;` (Option<u64>)

#### 4.5: `handle_batch_conversion_failure` の追加

既存の `handle_send_failure` (L469-497) と同じロジックを使用。post_id 削除が不要な点のみ異なる。

**方法A: 新規メソッド追加** (推奨 - シンプル)

```rust
/// Handle batch conversion failure (SpecCache not ready).
/// Similar to handle_send_failure but without post_id removal.
async fn handle_batch_conversion_failure(&self, batch: ActionBatch) {
    match batch {
        ActionBatch::Orders(orders) => {
            let (reduce_only, new_orders): (Vec<_>, Vec<_>) =
                orders.into_iter().partition(|o| o.reduce_only);

            // 新規注文はドロップ (position_tracker から削除)
            self.cleanup_dropped_orders(new_orders).await;

            // reduce_only は再キュー
            for order in reduce_only {
                let _ = self.executor.batch_scheduler().enqueue_reduce_only(order);
            }
        }
        ActionBatch::Cancels(cancels) => {
            // Cancels は idempotent なので再キュー
            for cancel in cancels {
                let _ = self.executor.batch_scheduler().enqueue_cancel(cancel);
            }
        }
    }
}
```

**方法B: handle_send_failure を共通化** (既存コード変更)

```rust
/// Handle send failure with optional post_id.
/// If post_id is Some, removes it from post_request_manager.
async fn handle_send_failure(&self, post_id: Option<u64>, batch: ActionBatch) {
    // Remove from pending requests if post_id exists
    if let Some(id) = post_id {
        self.post_request_manager.remove(id);
    }

    // 残りは同じ
    match batch {
        ActionBatch::Orders(orders) => {
            // ...
        }
        ActionBatch::Cancels(cancels) => {
            // ...
        }
    }
}
```

**方法B を採用する場合の呼び出し元変更**:
- `tick()` 内の既存呼び出し: `self.handle_send_failure(Some(post_id), batch).await;`
- `process_timeout()` の呼び出し: 同様に `Some(post_id)` を渡す
- 新規 (batch_to_action 失敗時): `self.handle_send_failure(None, batch).await;`

**推奨**: 方法A（新規メソッド追加）。既存の `handle_send_failure` を変更せず、影響範囲を限定。

### Step 5: `ExecutorLoop` 生成箇所を更新

**呼び出し元一覧**:

| ファイル | 行 | 現在のコード |
|---------|-----|-------------|
| `crates/hip3-bot/src/app.rs` | 422-424 | `ExecutorLoop::new(executor.clone(), nonce_manager, signer, 5000)` |

**ファイル**: `crates/hip3-bot/src/app.rs`

```rust
// BEFORE (L422-424)
let mut executor_loop =
    ExecutorLoop::new(executor.clone(), nonce_manager, signer, 5000);
executor_loop.set_vault_address(trading_vault_address);

// AFTER
let mut executor_loop = ExecutorLoop::new(
    executor.clone(),
    nonce_manager,
    signer,
    5000,
    spec_cache.clone(),  // 追加
);
executor_loop.set_vault_address(trading_vault_address);
```

**注意**: `App` は既に `spec_cache: Arc<SpecCache>` を保持しているため、追加の変更は不要。

### 依存関係の確認

`hip3-executor` crate が `SpecCache` を使うために必要な依存:

**ファイル**: `crates/hip3-executor/Cargo.toml`

```toml
[dependencies]
hip3-registry = { path = "../hip3-registry" }  # SpecCache がここにある場合
# または
hip3-core = { path = "../hip3-core" }  # MarketSpec はここ
```

`SpecCache` の定義場所を確認し、適切な依存を追加する。

### 代替案: `MarketSpec` を直接マップで渡す

`SpecCache` を使わず、`HashMap<MarketKey, MarketSpec>` を渡す方法もある:

```rust
spec_map: Arc<HashMap<MarketKey, MarketSpec>>,
```

これにより `hip3-registry` への依存を避けられる。

### テスト

#### 1. 単体テスト: `OrderWire::from_pending_order` の精度検証

**ファイル**: `crates/hip3-executor/src/signer.rs` (テストモジュールに追加)

**既存のテストヘルパーを活用**:
- `MarketSpec::default()` - `crates/hip3-core/src/market.rs:341-358`
- `sample_pending_order()` - `crates/hip3-executor/src/executor_loop.rs:608-618`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use hip3_core::{MarketKey, MarketSpec, OrderSide, Price, Size, DexId, AssetId, ClientOrderId};
    use rust_decimal_macros::dec;

    fn sample_market() -> MarketKey {
        MarketKey::new(DexId::XYZ, AssetId::new(0))
    }

    fn sample_pending_order_with_price_size(
        price: Price,
        size: Size,
        side: OrderSide,
    ) -> hip3_core::PendingOrder {
        hip3_core::PendingOrder::new(
            ClientOrderId::new(),
            sample_market(),
            side,
            price,
            size,
            false, // reduce_only
            1234567890, // created_at
        )
    }

    #[test]
    fn test_from_pending_order_limits_price_precision() {
        // MarketSpec::default() を使用
        // tick_size: 0.01, max_price_decimals: 2, sz_decimals: 3
        let spec = MarketSpec::default();

        let order = sample_pending_order_with_price_size(
            Price::new(dec!(103.123456789)),  // 過剰精度
            Size::new(dec!(0.1928733304402333767298326824)),  // 25桁
            OrderSide::Buy,
        );

        let wire = OrderWire::from_pending_order(&order, &spec);

        // 価格: tick丸め + 5 sig figs + max_price_decimals=2
        assert_eq!(wire.limit_px, "103.13"); // ceil for buy

        // サイズ: lot丸め (0.001) + 5 sig figs + sz_decimals=3
        assert_eq!(wire.sz, "0.192"); // 3 decimals
    }

    #[test]
    fn test_from_pending_order_sell_rounds_down() {
        let spec = MarketSpec::default();

        let order = sample_pending_order_with_price_size(
            Price::new(dec!(103.125)),
            Size::new(dec!(0.1)),
            OrderSide::Sell,
        );

        let wire = OrderWire::from_pending_order(&order, &spec);

        // Sell は floor
        assert_eq!(wire.limit_px, "103.12");
    }
}
```

#### 2. 統合テスト: Testnet で精度確認

手順:
1. `meta` API で `szDecimals` / `maxPriceDecimals` を取得
2. `MarketSpec` の設定値と比較
3. 精度制限された注文を送信 → 成功確認

#### 3. シグネチャ変更の影響確認

- `ExecutorLoop::new()` のテストがあれば更新
- `with_ws_sender()` のテストがあれば更新

---

## P2: コメント/ドキュメントの修正 (MEDIUM)

### 問題

`crates/hip3-ws/src/message.rs:61-64`:
```rust
pub struct SignaturePayload {
    /// Signature r component (hex without 0x prefix).
    pub r: String,
    /// Signature s component (hex without 0x prefix).
    pub s: String,
```

`crates/hip3-executor/src/ws_sender.rs:60-63`:
```rust
pub struct ActionSignature {
    /// Hex-encoded r value of the ECDSA signature (no 0x prefix).
    pub r: String,
    /// Hex-encoded s value of the ECDSA signature (no 0x prefix).
    pub s: String,
```

### 修正

**ファイル**: `crates/hip3-ws/src/message.rs`

```rust
pub struct SignaturePayload {
    /// Signature r component (hex with 0x prefix, e.g., "0x1a2b...").
    pub r: String,
    /// Signature s component (hex with 0x prefix, e.g., "0x3c4d...").
    pub s: String,
```

**ファイル**: `crates/hip3-executor/src/ws_sender.rs`

```rust
pub struct ActionSignature {
    /// Hex-encoded r value of the ECDSA signature (with 0x prefix).
    pub r: String,
    /// Hex-encoded s value of the ECDSA signature (with 0x prefix).
    pub s: String,
```

---

## P3: ActionSignature::from_bytes で v を正規化 (MEDIUM)

### 問題

**現在のコード** (`crates/hip3-executor/src/ws_sender.rs:69-75`):
```rust
pub fn from_bytes(bytes: &[u8; 65]) -> Self {
    Self {
        r: hex::encode(&bytes[0..32]),
        s: hex::encode(&bytes[32..64]),
        v: bytes[64],  // 0/1 がそのまま使われる可能性
    }
}
```

### 使用箇所

**この関数は実際に使用されています**:

| ファイル | 行 | 使用箇所 |
|---------|-----|----------|
| `crates/hip3-executor/src/ws_sender.rs` | 180 | `SignedActionBuilder::with_signature()` 内 |

### 修正

```rust
pub fn from_bytes(bytes: &[u8; 65]) -> Self {
    let v_raw = bytes[64];
    // Normalize v: if 0/1 (EIP-2098), convert to 27/28 (EIP-155)
    let v = if v_raw < 27 { v_raw + 27 } else { v_raw };
    Self {
        r: format!("0x{}", hex::encode(&bytes[0..32])),
        s: format!("0x{}", hex::encode(&bytes[32..64])),
        v,
    }
}
```

### テストの更新

**既存テスト** (`crates/hip3-executor/src/ws_sender.rs:262-272`):
```rust
#[test]
fn test_signature_from_bytes() {
    let mut bytes = [0u8; 65];
    bytes[0..32].copy_from_slice(&[0xab; 32]);
    bytes[32..64].copy_from_slice(&[0xcd; 32]);
    bytes[64] = 28;

    let sig = ActionSignature::from_bytes(&bytes);
    assert_eq!(sig.r.len(), 64); // ← 0x追加後は 66 になる
    assert_eq!(sig.s.len(), 64); // ← 0x追加後は 66 になる
    assert_eq!(sig.v, 28);
}
```

**更新後のテスト**:
```rust
#[test]
fn test_signature_from_bytes() {
    let mut bytes = [0u8; 65];
    bytes[0..32].copy_from_slice(&[0xab; 32]);
    bytes[32..64].copy_from_slice(&[0xcd; 32]);
    bytes[64] = 28;

    let sig = ActionSignature::from_bytes(&bytes);
    assert_eq!(sig.r.len(), 66); // "0x" + 64 hex chars
    assert!(sig.r.starts_with("0x"));
    assert_eq!(sig.s.len(), 66);
    assert!(sig.s.starts_with("0x"));
    assert_eq!(sig.v, 28);
}

#[test]
fn test_signature_from_bytes_normalizes_v() {
    let mut bytes = [0u8; 65];
    bytes[0..32].copy_from_slice(&[0xab; 32]);
    bytes[32..64].copy_from_slice(&[0xcd; 32]);
    bytes[64] = 1; // EIP-2098 形式

    let sig = ActionSignature::from_bytes(&bytes);
    assert_eq!(sig.v, 28); // 1 + 27 = 28 に正規化
}
```

---

## P4: BBO stale 閾値の設定調整 (LOW)

### 問題

```
WARN hip3_bot::app: Gate block started, market: xyz:26, gate: bbo_update,
reason: BBO stale: 2137ms > 2000ms max (P0-12)
```

SILVER市場は流動性が低く、BBO更新が2秒以上遅れることがある。

### 修正案

**Option A**: 市場ごとの閾値設定

`config/mainnet-test.toml`:
```toml
[markets.xyz_26]
bbo_stale_threshold_ms = 5000  # 5秒
```

**Option B**: グローバル閾値の引き上げ

```toml
[risk]
bbo_stale_threshold_ms = 5000
```

### 実装の検討

設定読み込みロジックに市場ごとの閾値オーバーライドを追加する必要があるかもしれない。

---

## P5: CL市場の調査 (LOW)

### 問題

CL市場 (xyz:4) からシグナルが検出されなかった。

### 調査項目

1. `xyz:4` が正しいアセットインデックスか確認
2. サブスクリプションが成功しているかログ確認
3. Oracle価格データが配信されているか確認

### 調査方法

```bash
# API で meta 情報を取得
curl -X POST https://api.hyperliquid.xyz/info \
  -H "Content-Type: application/json" \
  -d '{"type": "meta"}'
```

レスポンスから CL のインデックスを確認。

---

## Implementation Order

| Priority | Task | Description | Depends On |
|----------|------|-------------|------------|
| 1 | P0: 0x prefix 追加 | executor_loop.rs L388-389 の修正 | - |
| 2 | P0: Testnet ABテスト | 0x有無の動作確認 | P0 |
| 3 | P2: コメント修正 | message.rs, ws_sender.rs のドキュメント | P0確認後 |
| 4 | P3: from_bytes 修正 | v正規化 + 0x prefix追加 + テスト更新 | P0確認後 |
| 5 | P1-Step1 | ExecutorLoop に spec_cache フィールド追加 | - |
| 6 | P1-Step2 | ExecutorLoop::new/with_ws_sender シグネチャ変更 | P1-Step1 |
| 7 | P1-Step3 | from_pending_order にspec引数追加 | P1-Step2 |
| 8 | P1-Step4a | ExecutorError::MarketSpecNotFound 追加 | - |
| 9 | P1-Step4b | batch_to_action を Result 型に変更 | P1-Step3, P1-Step4a |
| 10 | P1-Step4c | tick() の呼び出し元を更新、handle_batch_conversion_failure 追加 | P1-Step4b |
| 11 | P1-Step5 | app.rs で spec_cache 渡し | P1-Step4c |
| 12 | P1-Tests | from_pending_order の精度テスト追加 | P1-Step3 |
| 11 | Testnet精度検証 | meta API と MarketSpec 突合せ | P1 |
| 12 | P4, P5 調査 | 設定調整、CL市場確認 | - |

### クリティカルパス

**P0のみで注文送信は成功する可能性が高い** (0x prefix が根本原因)。

P1 は精度の問題であり、取引所がリジェクトしなければ動作する。

**推奨手順**:
1. P0 修正 → Testnet ABテストで `0x` 有無の影響を確認
2. 成功確認後 → P2, P3 を適用
3. 精度問題が発生した場合 → P1 を実装

---

## Verification Checklist

### P0: Signature修正
- [ ] 0x prefix が r, s に追加されている
- [ ] Testnet ABテスト: 0xなし → エラー確認
- [ ] Testnet ABテスト: 0xあり → 成功確認

### P1: 精度制限
- [ ] ExecutorLoop に spec_cache フィールド追加
- [ ] new() / with_ws_sender() シグネチャ更新
- [ ] ExecutorError::MarketSpecNotFound 追加
- [ ] batch_to_action が Result 型を返す
- [ ] Action に action_type ("order"/"cancel") を設定、grouping は order のみ Some("na")、cancel は None
- [ ] batch_to_action を create_request より前に移動
- [ ] tick() で batch_to_action エラー時に return None と handle_batch_conversion_failure 呼び出し
- [ ] handle_batch_conversion_failure が cleanup_dropped_orders / enqueue_reduce_only / enqueue_cancel を使用
- [ ] app.rs の呼び出し元更新
- [ ] 価格/サイズが 5 有効数字以内
- [ ] tick/lot サイズで丸められている
- [ ] from_pending_order のテスト追加 (既存ヘルパー活用)
- [ ] Testnet: meta API と MarketSpec 突合せ

### P2/P3: ドキュメント/from_bytes
- [ ] コメントが "with 0x prefix" に更新
- [ ] from_bytes が 0x prefix を付加
- [ ] from_bytes が v を正規化 (0/1 → 27/28)
- [ ] test_signature_from_bytes が更新 (len=66)
- [ ] test_signature_from_bytes_normalizes_v 追加

### P4/P5: 設定/調査
- [ ] 低流動性市場でGateが不要に発火しない
- [ ] CL市場のasset indexが正しい

---

## Rollback Plan

修正がさらなる問題を引き起こした場合:

1. 変更をリバート (`git revert`)
2. 前回の動作バージョンに戻す
3. 根本原因を再調査

---

## Files to Modify

| File | Changes |
|------|---------|
| `crates/hip3-executor/src/executor_loop.rs` | P0: 0x prefix, P1: spec_cache フィールド追加 + batch_to_action Result型変更 + tick() エラーハンドリング |
| `crates/hip3-executor/src/error.rs` | P1: ExecutorError::MarketSpecNotFound 追加 |
| `crates/hip3-executor/src/signer.rs` | P1: from_pending_order シグネチャ変更 + テスト追加 |
| `crates/hip3-executor/src/ws_sender.rs` | P2: コメント修正, P3: from_bytes修正 + テスト更新 |
| `crates/hip3-ws/src/message.rs` | P2: コメント修正 |
| `crates/hip3-executor/Cargo.toml` | P1: hip3-registry 依存追加 (SpecCache 使用のため) |
| `crates/hip3-bot/src/app.rs` | P1: ExecutorLoop::new() に spec_cache 追加 (L422-424) |
| `config/mainnet-test.toml` | P4: 閾値調整 (optional) |

### テストファイル

| File | Changes |
|------|---------|
| `crates/hip3-executor/src/ws_sender.rs` | P3: test_signature_from_bytes 更新, v正規化テスト追加 |
| `crates/hip3-executor/src/signer.rs` | P1: from_pending_order 精度テスト追加 (既存ヘルパー活用) |
