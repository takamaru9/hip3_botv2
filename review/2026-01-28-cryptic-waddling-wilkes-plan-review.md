# Mark Regression Exit Implementation Plan Review

## Metadata

| Item | Value |
|------|-------|
| Plan File | `~/.claude/plans/cryptic-waddling-wilkes.md` |
| Review Date | 2026-01-28 |
| Reviewer | Claude Code |
| Status | **Requires Revision** |

---

## Summary

| Category | Rating | Notes |
|----------|--------|-------|
| Overall Design | **Good** | TimeStopMonitor pattern correctly chosen |
| Exit Logic | **Good** | BBO regression approach is sound |
| Existing Code Compatibility | **Issues** | Architecture mismatch with FlattenReason |
| Test Plan | **Adequate** | Covers main scenarios |
| Documentation | **Minor Gaps** | Some implementation details missing |

**Verdict**: Plan is fundamentally sound but requires revision on 2 critical issues before implementation.

---

## Critical Issues

### Issue #1: FlattenReason Addition - Architecture Mismatch

**Severity**: Critical

**Plan States**:
```rust
pub enum FlattenReason {
    TimeStop { elapsed_ms: u64 },
    HardStop,
    Manual,
    MarkRegression { edge_at_exit_bps: Decimal }, // NEW
}
```

**Actual Implementation**:
`TimeStopMonitor` uses `flatten_tx: mpsc::Sender<PendingOrder>` and sends `PendingOrder` directly, **not** `FlattenReason`:

```rust
// time_stop.rs:407
let order = FlattenOrderBuilder::create_flatten_order(
    position, price, self.slippage_bps, now_ms
);
self.flatten_tx.send(order).await  // PendingOrder, not FlattenRequest
```

**Impact**:
- Adding `FlattenReason::MarkRegression` would be unused in current architecture
- `Flattener` and `FlattenRequest` are separate subsystems not integrated with monitors

**Recommendation**:

| Option | Description | Effort |
|--------|-------------|--------|
| **A (Recommended)** | Remove FlattenReason addition; use structured logging only | Low |
| **B** | Unify architecture: change `flatten_tx` to `Sender<(PendingOrder, FlattenReason)>` | High |

---

### Issue #2: Redundant Provider Traits

**Severity**: Medium

**Plan States**:
```rust
pub trait MarketSnapshotProvider: Send + Sync {
    fn get_snapshot(&self, market: &MarketKey) -> Option<MarketSnapshot>;
}

pub struct MarkRegressionMonitor<S, P> {
    snapshot_provider: Arc<S>,  // MarketState adapter
    price_provider: Arc<P>,     // For flatten order pricing
}
```

**Actual Implementation**:
`MarketState` already has equivalent method:
```rust
// market_state.rs:159
pub fn get_snapshot(&self, key: &MarketKey) -> Option<MarketSnapshot>
```

**Impact**:
- New trait is unnecessary duplication
- Two providers are redundant (snapshot contains BBO for pricing)

**Recommendation**:
```rust
pub struct MarkRegressionMonitor {
    config: MarkRegressionConfig,
    position_handle: PositionTrackerHandle,
    flatten_tx: mpsc::Sender<PendingOrder>,
    market_state: Arc<MarketState>,  // Single unified provider
}
```

---

## Minor Issues

### Issue #3: min_holding_time_ms Logic Not Specified

**Severity**: Low

**Plan States**:
```toml
min_holding_time_ms = 1000  # minimum hold before exit check
```

**Missing**: How this is checked in `check_exit`

**Recommendation**: Add explicit logic:
```rust
fn check_exit(&self, position: &Position, snapshot: &MarketSnapshot, now_ms: u64) -> Option<Decimal> {
    // Check minimum holding time first
    let held_ms = now_ms.saturating_sub(position.entry_timestamp_ms);
    if held_ms < self.config.min_holding_time_ms {
        return None;  // Too early to exit
    }
    // ... rest of exit logic
}
```

---

### Issue #4: edge_at_exit_bps Calculation Not Specified

**Severity**: Low

**Plan States**:
```rust
pub fn check_exit(...) -> Option<Decimal> // Returns edge_at_exit_bps if exit triggered
```

**Missing**: Calculation formula for return value

**Recommendation**: Add explicit formula:
```rust
// Long position: edge = (bid - oracle) / oracle * 10000
// Short position: edge = (oracle - ask) / oracle * 10000
let edge_bps = match position.side {
    OrderSide::Buy => {
        (snapshot.bbo.bid_price.inner() - snapshot.ctx.oracle.oracle_px.inner())
            / snapshot.ctx.oracle.oracle_px.inner() * dec!(10000)
    }
    OrderSide::Sell => {
        (snapshot.ctx.oracle.oracle_px.inner() - snapshot.bbo.ask_price.inner())
            / snapshot.ctx.oracle.oracle_px.inner() * dec!(10000)
    }
};
```

---

## Verification: Exit Condition Logic

**Plan States**:
```
Long Exit:  bid >= oracle * (1 - exit_threshold_bps/10000)
Short Exit: ask <= oracle * (1 + exit_threshold_bps/10000)
```

**Analysis**:
- Long: When `bid` rises to near `oracle`, the divergence has closed
- Short: When `ask` drops to near `oracle`, the divergence has closed

**Verdict**: Logic is correct.

---

## Suggestions

### 1. Simplify File Structure

| Original Plan | Suggested |
|---------------|-----------|
| New `mark_regression.rs` | Consider adding to `time_stop.rs` or new `monitors.rs` |
| New `MarketSnapshotProvider` trait | Use `Arc<MarketState>` directly |

### 2. Logging Instead of FlattenReason

If FlattenReason tracking isn't critical, use structured logging:
```rust
info!(
    market = %position.market,
    side = ?position.side,
    edge_bps = %edge_at_exit_bps,
    held_ms = position_held_ms,
    "MarkRegression exit triggered"
);
```

### 3. Metrics Alignment

Plan's metrics are good. Ensure they match existing patterns:
```rust
// Existing pattern in codebase
metrics::counter!("time_stop_triggered_total").increment(1);

// Suggested for MarkRegression
metrics::counter!("mark_regression_exits_total", "market" => market.to_string()).increment(1);
metrics::histogram!("mark_regression_edge_bps").record(edge_at_exit_bps.to_f64().unwrap_or(0.0));
```

---

## Files to Modify - Revised

| File | Original Plan | Revised Recommendation |
|------|---------------|------------------------|
| `mark_regression.rs` | NEW | NEW (or add to `time_stop.rs`) |
| `flatten.rs` | Add FlattenReason variant | **Skip** (Option A) |
| `lib.rs` | Export module | Export module |
| `config.rs` | Add MarkRegressionConfig | Add MarkRegressionConfig |
| `app.rs` | Spawn monitor | Spawn monitor |
| `default.toml` | Add section | Add section |

---

## Action Items

| Priority | Item | Owner |
|----------|------|-------|
| **P0** | Decide on FlattenReason approach (Option A vs B) | User |
| **P0** | Remove redundant provider design | Plan Author |
| **P1** | Add min_holding_time_ms check logic | Plan Author |
| **P1** | Add edge_bps calculation formula | Plan Author |
| **P2** | Consider file structure simplification | Plan Author |

---

## Conclusion

The plan correctly identifies TimeStopMonitor as the reference pattern and proposes a sound exit condition based on BBO-Oracle regression. However, the FlattenReason extension doesn't align with the current architecture where monitors send `PendingOrder` directly without reason tracking.

**Recommended Next Steps**:
1. Choose Option A (logging only) or Option B (architecture unification)
2. Simplify to use `Arc<MarketState>` directly
3. Add missing implementation details
4. Re-review before implementation
