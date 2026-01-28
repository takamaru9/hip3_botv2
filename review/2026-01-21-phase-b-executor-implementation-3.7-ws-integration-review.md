# Phase B Executor Implementation - Code Review 3 指摘対応完了レビュー

**作成日**: 2026-01-21
**対象**: コードレビュー3の全3指摘への対応

---

## 指摘対応サマリー

| 指摘 | 問題 | 対応状態 | 変更ファイル |
|------|------|----------|--------------|
| 1 | ExecutorLoop WS送信未実装 | ✅ 完了 | ws_sender.rs, executor_loop.rs, signer.rs |
| 2 | MaxPositionTotal 不備 | ✅ 完了 | tracker.rs, executor.rs |
| 3 | pending_markets_cache 二重減算 | ✅ 完了 | executor_loop.rs, executor.rs, tracker.rs |

---

## 指摘1: ExecutorLoop WS送信未実装

### 実装内容

#### 1.1 WsSender trait (ws_sender.rs - 新規)

```rust
pub type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

pub trait WsSender: Send + Sync {
    fn send(&self, action: SignedAction) -> BoxFuture<'_, SendResult>;
    fn is_ready(&self) -> bool;
}

pub enum SendResult {
    Sent,
    Disconnected,
    RateLimited,
    Error(String),
}

pub struct SignedAction {
    pub action: Action,
    pub nonce: u64,
    pub signature: ActionSignature,
    pub post_id: u64,
}

pub struct ActionSignature {
    pub r: String,
    pub s: String,
    pub v: u8,
}
```

#### 1.2 署名統合 (executor_loop.rs)

```rust
// tick() 内の署名フロー
let nonce = self.nonce_manager.next();
let action = self.batch_to_action(&batch);
let signing_input = SigningInput { action: action.clone(), nonce, vault_address: None, expires_after: None };
let signature = self.signer.sign_action(signing_input).await?;

let signed_action = SignedAction {
    action, nonce,
    signature: ActionSignature {
        r: hex::encode(signature.r().to_be_bytes::<32>()),
        s: hex::encode(signature.s().to_be_bytes::<32>()),
        v: signature.v() as u8,
    },
    post_id,
};
```

#### 1.3 sent状態管理 (executor_loop.rs)

送信成功時のみ `mark_sent()` を呼び出す:
```rust
match ws_sender.send(signed_action).await {
    SendResult::Sent => {
        // Only mark as sent AFTER successful send
        for cloid in cloids {
            self.post_request_manager.mark_sent(cloid);
        }
        self.executor.batch_scheduler().on_batch_sent();
    }
    // ... error handling
}
```

#### 1.4 timeout/失敗時の再キュー (executor_loop.rs)

```rust
async fn handle_timeouts(&self, now_ms: u64) {
    // ...
    match batch {
        ActionBatch::Orders(orders) => {
            let (reduce_only, new_orders) = orders.into_iter().partition(|o| o.reduce_only);
            self.cleanup_dropped_orders(new_orders).await;
            for order in reduce_only {
                self.executor.batch_scheduler().enqueue_reduce_only(order);
            }
        }
        ActionBatch::Cancels(cancels) => {
            for cancel in cancels {
                self.executor.batch_scheduler().enqueue_cancel(cancel);
            }
        }
    }
}
```

### 追加ファイル

- `crates/hip3-executor/src/ws_sender.rs` (約220行)
  - `WsSender` trait
  - `SendResult` enum
  - `SignedAction`, `ActionSignature` structs
  - `MockWsSender` for testing
  - `SignedActionBuilder` helper

### 追加メソッド

- `OrderWire::from_pending_order(&PendingOrder) -> Self` (signer.rs)
- `ExecutorLoop::batch_to_action(&ActionBatch) -> Action` (executor_loop.rs)
- `ExecutorLoop::handle_send_failure(post_id, batch)` (executor_loop.rs)

---

## 指摘2: MaxPositionTotal 不備

### 修正内容

1. `ExecutorConfig`: `max_notional_per_market`/`max_notional_total` を `f64` → `Decimal` に変更
2. `get_pending_notional_excluding_reduce_only`: `_mark_px` → `mark_px` で実際に使用
3. 新規追加: `get_total_pending_notional_excluding_reduce_only<F>` (全マーケット合計用)
4. `calculate_total_portfolio_notional`: pending notional を加算
5. Gate 3/4: 比較を `to_f64()...>=` から Decimal 直接比較に変更

### 変更箇所

- `tracker.rs`: `get_pending_notional_excluding_reduce_only`, `get_total_pending_notional_excluding_reduce_only`
- `executor.rs`: `ExecutorConfig`, Gate 3/4 比較ロジック, `calculate_total_portfolio_notional`

---

## 指摘3: pending_markets_cache 二重減算

### 修正内容

選択肢B採用: cleanup から `unmark_pending_market()` を撤去、`remove_order()` に一本化

### 変更箇所

- `executor_loop.rs:cleanup_dropped_orders`: `unmark_pending_market` 呼び出し削除
- `executor.rs:on_hard_stop`: `unmark_pending_market` 呼び出し削除
- `tracker.rs:unmark_pending_market`: ドキュメント更新（register前ロールバック専用と明記）

---

## テスト結果

```
cargo test -p hip3-executor -p hip3-position
test result: ok. 45 passed; 0 failed; 0 ignored

cargo clippy -p hip3-executor -p hip3-position -- -D warnings
Finished
```

---

## Production Readiness

| 項目 | 状態 |
|------|------|
| WS送信統合 | ✅ trait定義完了、実装はhip3-wsに委譲 |
| 署名フロー | ✅ EIP-712署名統合済み |
| sent状態管理 | ✅ 送信成功後のみ更新 |
| 失敗時再キュー | ✅ reduce_only必達再キュー実装 |
| MaxPosition計算 | ✅ pending含む、mark_px統一、Decimal比較 |
| pending cache整合性 | ✅ remove_orderに一本化 |

**結論**: コードレビュー3の全指摘に対応完了。本番稼働に向けて再レビュー可能。
