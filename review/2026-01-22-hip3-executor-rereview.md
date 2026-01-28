# hip3-executor Re-Review

## Metadata
| Item | Value |
|------|-------|
| Date | 2026-01-22 |
| Reviewer | code-reviewer agent |
| Files Reviewed | executor.rs, signer.rs, ready.rs, batch.rs, executor_loop.rs, risk.rs, nonce.rs, ws_sender.rs, lib.rs |
| Previous Review | 2026-01-22-hip3-executor-comprehensive-review.md |
| Focus | 前回指摘P1/P2問題の修正状況確認 |

## Quick Assessment

| Metric | Score | Change |
|--------|-------|--------|
| Code Quality | 8.8/10 | +0.3 |
| Thread Safety | 9.0/10 | +0.5 |
| Error Handling | 8.5/10 | +1.0 |
| Risk Level | Green | - |

---

## 前回指摘事項の修正状況

### P1-1: ActionBudget check/consume非アトミック問題

**前回指摘 (executor.rs:L110-131)**:
> `can_send_new_order_at()` と `consume_at()` は別々の操作であり、間にインターバルリセットが入ると予算オーバーの可能性がある

**修正状況: FIXED**

`executor.rs:L147-197` の `consume_at()` が完全に書き直され、CASループで以下を原子的に処理:

```rust
// L155-197: consume_at() - FIXED
pub fn consume_at(&self, now_ms: u64) -> bool {
    // Single loop handles both interval reset and consumption atomically
    loop {
        let interval_start = self.interval_start_ms.load(Ordering::Acquire);
        let current = self.current_count.load(Ordering::Acquire);

        // Check if interval has expired
        if now_ms.saturating_sub(interval_start) > self.interval_ms {
            // Try to atomically reset the interval (only one thread wins)
            match self.interval_start_ms.compare_exchange_weak(
                interval_start,
                now_ms,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    // We won the race - reset counter and consume one
                    self.current_count.store(1, Ordering::Release);
                    return true;
                }
                Err(_) => {
                    // Another thread reset the interval - retry with new values
                    continue;
                }
            }
        }

        // Interval still valid - try to consume from existing budget
        if current >= self.max_orders {
            return false;
        }

        match self.current_count.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(_) => continue,
        }
    }
}
```

**評価**:
- インターバルリセットとカウンタ消費が単一ループ内でCAS操作
- レースコンディションで他スレッドがリセットした場合はリトライ
- `compare_exchange_weak` 使用で高頻度環境でも効率的
- 適切なメモリオーダリング (AcqRel/Acquire)

**呼び出し側の変更 (executor.rs:L481-498)**:

```rust
// Gate 7: ActionBudget
if !self.action_budget.can_send_new_order() {
    // Rollback: unmark pending market since we won't queue the order
    self.position_tracker.unmark_pending_market(market);
    // ...
    return ExecutionResult::skipped(SkipReason::BudgetExhausted);
}

// Consume budget
if !self.action_budget.consume() {
    // Race condition: budget exhausted between check and consume
    self.position_tracker.unmark_pending_market(market);
    return ExecutionResult::skipped(SkipReason::BudgetExhausted);
}
```

check後にconsumeを呼び出し、レース時も適切にロールバック。これは意図的な設計であり、頻度の低いレースケースでの二重拒否は許容範囲。

---

### P1-2: signer.rs の expect() パニック問題

**前回指摘 (signer.rs:L391)**:
> `expect("Action serialization should not fail")` がパニックを引き起こす可能性がある

**修正状況: FIXED**

`signer.rs:L388-396` が `Result` を返すように変更:

```rust
// SigningInput::action_hash() - FIXED (L388-396)
pub fn action_hash(&self) -> Result<B256, SignerError> {
    let mut data = Vec::new();

    // 1. Serialize Action with msgpack (named/map format)
    let action_bytes = rmp_serde::to_vec_named(&self.action)
        .map_err(|e| SignerError::SerializationFailed(e.to_string()))?;
    data.extend_from_slice(&action_bytes);
    // ...
    Ok(keccak256(&data))
}
```

**SignerError への追加 (signer.rs:L513)**:

```rust
#[derive(Debug, Error)]
pub enum SignerError {
    #[error("No trading key available")]
    NoTradingKey,

    #[error("Signing failed: {0}")]
    SigningFailed(#[from] alloy::signers::Error),

    #[error("Action serialization failed: {0}")]
    SerializationFailed(String),  // NEW
}
```

**呼び出し側の対応 (signer.rs:L560-561)**:

```rust
pub async fn sign_action(&self, input: SigningInput) -> Result<PrimitiveSignature, SignerError> {
    // ...
    // Step 1: Calculate action_hash (returns Result now)
    let action_hash = input.action_hash()?;
    // ...
}
```

**評価**:
- パニックの可能性が完全に除去された
- エラーは `SignerError::SerializationFailed` として適切に伝播
- 呼び出し側 (`ExecutorLoop::tick()`) で正しくハンドリング

---

### P2: TradingReadyChecker未使用問題

**前回指摘 (executor.rs:L389-393)**:
> TradingReadyCheckerがExecutorに渡されているが、Gate 2は「botが担当」とコメントされている。責任の所在が不明確。

**修正状況: DOCUMENTED (設計決定として記録)**

`executor.rs:L417-421` にコメントで設計決定が明記:

```rust
// Gate 2: READY-TRADING - Handled by bot via connection_manager.is_ready()
// TradingReadyChecker's 4 flags are not wired in current implementation.
// The bot checks WS READY-TRADING (bbo + assetCtx + orderUpdates subscriptions)
// before calling on_signal, so we skip this check here to avoid duplication.
// To restore: if !self.ready_checker.is_ready() { return Rejected(NotReady); }
```

**Executor構造体 (L337-351)**:

```rust
pub struct Executor {
    // ...
    /// READY-TRADING checker.
    ready_checker: Arc<TradingReadyChecker>,  // 保持されているが未使用
    // ...
}
```

**評価**:
- コメントで設計意図が明確化された
- 将来の復元方法も記載
- `ready_checker()` アクセサが提供されており、botレベルでの使用を想定
- 不要なフィールドだが、将来の拡張性を考慮して保持は妥当

**推奨**: アーキテクチャドキュメントに「Gate 2はbot層で処理」と明記すべき。コードコメントだけでなく、`.claude/specs/` などに設計決定を記録することを推奨。

---

## 新規発見事項

### 良い改善点

1. **テスト追加** (executor.rs:L1043-1057)
   - `test_action_budget_interval_reset` が追加され、インターバルリセット動作を検証

2. **コメント充実** (executor.rs:L147-154)
   ```rust
   /// Consume one order from the budget at the given timestamp.
   ///
   /// This method atomically handles:
   /// 1. Interval expiration check and reset (via CAS)
   /// 2. Budget consumption (via CAS)
   ///
   /// The two operations are performed in a single loop to avoid race conditions
   /// where multiple threads could both reset the interval.
   ```

3. **ドキュメントヘッダー改善** (executor.rs:L6-17)
   - Gate Check Order が明確に文書化

### 軽微な懸念点 (P3)

1. **PostRequestManager の check_timeouts パフォーマンス**
   - 前回指摘: 二重ループ問題
   - 現状: 未修正（executor_loop.rs:L161-183）
   - 影響: 低（pending requests数が限定的な想定）
   - 対応: P3として継続監視

2. **error.rs の構造化**
   - 前回指摘: エラー型に構造化データがない
   - 現状: 未修正
   - 影響: デバッグ時の情報不足
   - 対応: P3として将来改善

---

## Non-Negotiable Line Compliance (再確認)

| Non-Negotiable | Status | Evidence |
|----------------|--------|----------|
| cloid Idempotency | COMPLIANT | ClientOrderId::new() generates UUIDs (executor.rs:L501) |
| Exception Halt Priority | COMPLIANT | HardStop rejects new, allows reduce_only (batch.rs:L403-428) |
| Decimal Precision | COMPLIANT | All notional uses Decimal (executor.rs:L425-433) |
| Monotonic Freshness | COMPLIANT | CAS loop in nonce.rs:L101-118 |
| reduce_only Priority | COMPLIANT | Queued even at inflight limit (batch.rs:L304-334) |
| Cancel Priority | COMPLIANT | Processed before orders (batch.rs:L390-398) |

---

## Suggestions Summary

| Priority | Location | Issue | Status |
|----------|----------|-------|--------|
| P1 | executor.rs:L110-131 | ActionBudget非アトミック | FIXED |
| P1 | signer.rs:L391 | expect()パニック | FIXED |
| P2 | executor.rs:L389-393 | TradingReadyChecker未使用 | DOCUMENTED |
| P3 | executor_loop.rs:L161-183 | check_timeouts二重ループ | OPEN |
| P3 | error.rs | 構造化エラーデータ | OPEN |

---

## Verdict

**APPROVED**

**Summary**: 前回指摘の全P1問題が修正され、P2問題は設計決定として文書化された。ActionBudgetのCASループ実装は正しく、signer.rsのパニック可能性も除去された。コードベースは本番運用に適した状態。

**Remaining Items**:
1. [P3] check_timeouts のパフォーマンス最適化（必要に応じて）
2. [P3] error.rs の構造化データ追加（将来改善）
3. [DOC] Gate 2 のアーキテクチャ決定を specs/ に記録

**Risk Assessment**:
- 前回指摘のレースコンディションリスクは解消
- シリアライゼーションエラーによるクラッシュリスクは解消
- 残存するP3項目は運用に影響なし
