# BUG-004: Duplicate Flatten Orders Fix

## Metadata

| Item | Value |
|------|-------|
| Bug ID | BUG-004 |
| Date | 2026-01-29 |
| Status | `[COMPLETED]` |
| Severity | Medium |
| Component | hip3-executor/batch.rs, MarkRegression/TimeStop |

## Problem Description

### Symptom
- Orders rejected with error: `"Reduce only order would increase position. asset=110026"`
- Close orders submitted when position is already flat
- Multiple flatten orders sent for the same position

### Root Cause
MarkRegressionMonitor runs every 200ms and checks all open positions. When the exit condition is met:
1. First check (T=0ms): Exit triggered, flatten order sent to BatchScheduler
2. Second check (T=200ms): Position still exists (first order not yet filled), exit triggered AGAIN
3. Two flatten orders are now queued/in-flight for the same position
4. First order fills (position closed)
5. Second order is rejected by exchange ("Reduce only would increase position")

### Log Evidence
```
08:35:26.277 MarkRegression exit triggered, cloid=0xd285c327...
08:35:26.477 MarkRegression exit triggered, cloid=0xeabcf7d4... (200ms later, DUPLICATE)
08:35:26.655 Position updated (fill), cloid=0xd285c327... (first order filled)
08:35:27.030 Order rejected: "Reduce only order would increase position" (second order)
```

## Solution

### Approach
Add deduplication in `BatchScheduler.enqueue_reduce_only()` to prevent multiple flatten orders for the same market.

### Implementation

**File:** `crates/hip3-executor/src/batch.rs`

```rust
pub fn enqueue_reduce_only(&self, order: PendingOrder) -> EnqueueResult {
    // ... existing assertions ...

    let mut queue = self.pending_reduce_only.lock();

    // BUG-004: Check for duplicate reduce_only order for same market
    let has_pending_for_market = queue.iter().any(|o| o.market == order.market);
    if has_pending_for_market {
        debug!(
            cloid = %order.cloid,
            market = %order.market,
            "Skipping duplicate reduce_only order (market already has pending flatten)"
        );
        return EnqueueResult::Queued; // Return Queued to not break caller
    }

    // ... rest of the method ...
}
```

### Why This Works
- When MarkRegression triggers a flatten at T=0, the order is added to `pending_reduce_only` queue
- When MarkRegression triggers again at T=200ms for the same market, it's rejected as duplicate
- The first order will handle closing the position
- Queue is cleared when order is sent, so new flatten can be enqueued if needed later

### Edge Cases Handled
| Case | Behavior |
|------|----------|
| Same market, duplicate order | Skipped, first order handles it |
| Different markets | Both accepted (no deduplication) |
| First order sent, new trigger | New order accepted (queue is clear) |
| First order fails/rejected | Manual intervention needed (position resync) |

## Tests Added

| Test | Purpose |
|------|---------|
| `test_reduce_only_deduplication_same_market` | Verify duplicate is skipped |
| `test_reduce_only_different_markets_not_deduplicated` | Verify different markets both accepted |
| `test_reduce_only_dedup_after_drain` | Verify new order accepted after queue drain |

## Files Modified

| File | Changes |
|------|---------|
| `crates/hip3-executor/src/batch.rs` | Added deduplication logic, helper function, tests |

## Verification

```bash
# Run batch tests
cargo test -p hip3-executor batch::

# Result: 18 passed, 0 failed
```

## Deployment
Deploy to VPS after this fix to resolve production "Reduce only would increase position" errors.
