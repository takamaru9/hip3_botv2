# Mark Regression Exit Implementation Plan Re-Review

## Metadata

| Item | Value |
|------|-------|
| Plan File | `~/.claude/plans/cryptic-waddling-wilkes.md` |
| Review Date | 2026-01-28 |
| Previous Review | `2026-01-28-cryptic-waddling-wilkes-plan-review.md` |
| Status | **Approved** |

---

## Summary

| Category | Previous | Current | Notes |
|----------|----------|---------|-------|
| Overall Design | Good | **Good** | - |
| Exit Logic | Good | **Good** | - |
| Existing Code Compatibility | Issues | **Resolved** | All critical issues fixed |
| Test Plan | Adequate | **Adequate** | - |
| Documentation | Minor Gaps | **Good** | Details added |

**Verdict**: All critical issues resolved. Plan is ready for implementation.

---

## Critical Issues - Resolution Status

### Issue #1: FlattenReason Addition ✅ RESOLVED

**Previous**:
> FlattenReason::MarkRegression would be unused in current architecture

**Resolution**:
```
**Note**: FlattenReason enumの拡張は不要。TimeStopMonitorと同様に
PendingOrderを直接送信し、構造化ログでexit理由を記録。
```

Files to Modifyから`flatten.rs`が適切に除外されている。

---

### Issue #2: Redundant Provider Traits ✅ RESOLVED

**Previous**:
> New trait is unnecessary; Two providers are redundant

**Resolution**:
```rust
pub struct MarkRegressionMonitor {
    // ...
    market_state: Arc<MarketState>,  // 直接使用 (トレイト不要)
}
```

単一の`Arc<MarketState>`のみ使用。

---

## Minor Issues - Resolution Status

### Issue #3: min_holding_time_ms Logic ✅ RESOLVED

**Resolution**:
```rust
fn check_exit(&self, position: &Position, now_ms: u64) -> Option<Decimal> {
    // 1. Check minimum holding time
    let held_ms = now_ms.saturating_sub(position.entry_timestamp_ms);
    if held_ms < self.config.min_holding_time_ms {
        return None;
    }
    // ...
}
```

---

### Issue #4: edge_at_exit_bps Calculation ✅ RESOLVED

**Resolution**:
```rust
// Long: edge_bps = (bid - oracle) / oracle * 10000
// Short: edge_bps = (oracle - ask) / oracle * 10000
```

明示的な計算式がコード内に記載されている。

---

## New Minor Observations

### Observation #1: Missing Import

```rust
use hip3_core::{MarketKey, PendingOrder};
// Missing: OrderSide
```

**Impact**: Low - 実装時に自動補完される

**Fix**:
```rust
use hip3_core::{MarketKey, OrderSide, PendingOrder};
```

---

### Observation #2: Channel Close Handling

```rust
if self.flatten_tx.send(order).await.is_err() {
    tracing::info!("Flatten channel closed, stopping MarkRegressionMonitor");
    // Missing: return; to exit loop
}
```

**Impact**: Low - ループは継続するが、次のsend()でも同様にエラーになる

**Fix**:
```rust
if self.flatten_tx.send(order).await.is_err() {
    tracing::info!("Flatten channel closed, stopping MarkRegressionMonitor");
    return;  // Exit the run() loop
}
```

---

## Final Checklist

| Item | Status |
|------|--------|
| TimeStopMonitorパターン準拠 | ✅ |
| Arc<MarketState>直接使用 | ✅ |
| FlattenReason拡張なし | ✅ |
| min_holding_time_msチェック | ✅ |
| edge_bps計算式明記 | ✅ |
| 構造化ログ出力 | ✅ |
| flatten_tx共有 | ✅ |
| テストケース記載 | ✅ |

---

## Conclusion

全てのcritical issuesが修正されました。計画は実装準備完了です。

**実装時の注意**:
1. `OrderSide`のimport追加を忘れずに
2. チャネルclose時の`return`追加を検討

**Approved for implementation.**
