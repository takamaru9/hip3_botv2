# hip3-bot Core Module Re-Review

## Metadata
| Item | Value |
|------|-------|
| Date | 2026-01-22 |
| Reviewer | code-reviewer agent |
| Previous Review | `review/2026-01-22-hip3-bot-core-review.md` |
| Files Reviewed | `crates/hip3-bot/src/app.rs` |

## Quick Assessment
| Metric | Score |
|--------|-------|
| Code Quality | 8.5/10 (up from 7.5) |
| Test Coverage | 20% (estimated) |
| Risk Level | Green |

## Previous Review Issue Status

### P0-1: WS Handle `abort()` Resource Leak
**Previous Location**: app.rs:581

**Status**: FIXED

**Evidence**:
```rust
// app.rs:595-607
// Graceful shutdown of WebSocket connection (P0-1)
if let Some(ref cm) = self.connection_manager {
    cm.shutdown();
}
const WS_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
match tokio::time::timeout(WS_SHUTDOWN_TIMEOUT, ws_handle).await {
    Ok(Ok(())) => debug!("WebSocket task completed"),
    Ok(Err(e)) => warn!(?e, "WebSocket task panicked"),
    Err(_) => {
        warn!("WebSocket shutdown timed out (5s), aborting");
        // ws_handle is already consumed by timeout, no need to abort
    }
}
```

**Analysis**:
- `cm.shutdown()` now signals graceful shutdown via `AtomicBool` flag (connection.rs:147-150)
- Connection loop checks `is_shutdown()` at start and after disconnect (connection.rs:166-171, 185-190)
- Timeout-bounded join prevents indefinite blocking
- Comment correctly notes that `ws_handle` is consumed by timeout, no manual abort needed

**Verdict**: Properly fixed. Graceful shutdown with fallback timeout.

---

### P0-2: Position Tracker Join Handle Unprocessed
**Previous Location**: app.rs:338

**Status**: FIXED

**Evidence**:
```rust
// app.rs:85-86
/// Position tracker task join handle for graceful shutdown.
position_tracker_handle: Option<tokio::task::JoinHandle<()>>,

// app.rs:342-344
let (position_tracker, pos_join_handle) = spawn_position_tracker(100);
self.position_tracker = Some(position_tracker.clone());
self.position_tracker_handle = Some(pos_join_handle);

// app.rs:581-593
// Graceful shutdown of Position Tracker (P0-2)
if let Some(ref tracker) = self.position_tracker {
    debug!("Sending shutdown to position tracker");
    tracker.shutdown().await;
}
if let Some(handle) = self.position_tracker_handle.take() {
    const POSITION_TRACKER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
    match tokio::time::timeout(POSITION_TRACKER_SHUTDOWN_TIMEOUT, handle).await {
        Ok(Ok(())) => debug!("Position tracker task completed"),
        Ok(Err(e)) => warn!(?e, "Position tracker task panicked"),
        Err(_) => warn!("Position tracker shutdown timed out (5s)"),
    }
}
```

**Analysis**:
- Join handle is now stored in `position_tracker_handle` field
- Shutdown sequence: 1) Send `Shutdown` message, 2) Timeout-bounded join
- Position tracker actor handles `Shutdown` message correctly (tracker.rs:214-217)
- Timeout of 5s is reasonable for graceful shutdown

**Verdict**: Properly fixed. Actor receives shutdown signal and task is joined.

---

### P0-3: Preflight HTTP Timeout Missing
**Previous Location**: app.rs:160-163

**Status**: FIXED

**Evidence**:
```rust
// app.rs:159-167
// Fetch perpDexs from exchange with timeout
let client = MetaClient::new(&self.config.info_url)
    .map_err(|e| AppError::Preflight(format!("Failed to create HTTP client: {e}")))?;

const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(30);
let perp_dexs = tokio::time::timeout(PREFLIGHT_TIMEOUT, client.fetch_perp_dexs())
    .await
    .map_err(|_| AppError::Preflight("Preflight HTTP request timed out (30s)".to_string()))?
    .map_err(|e| AppError::Preflight(format!("Failed to fetch perpDexs: {e}")))?;
```

**Analysis**:
- `tokio::time::timeout` with 30s limit wraps the async HTTP call
- Timeout error is converted to `AppError::Preflight` with clear message
- Inner error from `fetch_perp_dexs()` is also properly mapped

**Verdict**: Properly fixed. HTTP request cannot hang indefinitely.

---

### P1-1: `std::sync::Mutex<FollowupWriter>` Blocking
**Previous Location**: app.rs:100-103

**Status**: FIXED

**Evidence**:
```rust
// app.rs:67
/// Followup writer for signal validation snapshots.
followup_writer: Arc<tokio::sync::Mutex<FollowupWriter>>,

// app.rs:102-105
let followup_writer = Arc::new(tokio::sync::Mutex::new(FollowupWriter::new(
    &config.persistence.data_dir,
    config.persistence.buffer_size,
)));

// app.rs:1014 (in capture_followup)
followup_writer: Arc<tokio::sync::Mutex<FollowupWriter>>,

// app.rs:1123-1124
{
    let mut writer = followup_writer.lock().await;
    // ...
}
```

**Analysis**:
- Changed from `std::sync::Mutex` to `tokio::sync::Mutex`
- All lock operations now use `.lock().await` (async-aware)
- Concurrent T+1s, T+3s, T+5s followup tasks will not block each other's tokio tasks

**Verdict**: Properly fixed. Async mutex prevents runtime thread blocking.

---

## New Observations

### Strengths (Improvements Since Last Review)

1. **Shutdown Sequence Order**: Correct order: Position tracker first (async message), then WS (flag-based), then joins with timeout.

2. **Tick Handle Abort**: Tick handle is aborted before position tracker shutdown (app.rs:577-579), which is correct since tick task has no cleanup needs.

3. **Comment Quality**: Clear comments marking P0-1 and P0-2 fixes aid code navigation.

### Minor Remaining Concerns

1. **Tick Handle Abort Asymmetry**
   - Location: app.rs:577-579
   - Observation: Tick handle is `abort()`ed but not joined
   - Impact: Low - infinite loop with no state, abort is safe
   - Note: For consistency, could use the same timeout-join pattern, but not necessary

2. **Followup Tasks Not Cancelled on Shutdown**
   - Location: app.rs:997-1000
   - Observation: Followup capture tasks spawned via `tokio::spawn` are fire-and-forget
   - Impact: Low - they will complete naturally (max 5s sleep) or be cancelled when runtime drops
   - Note: Not a bug, but worth noting for shutdown timing expectations

3. **WS Shutdown Timeout Comment**
   - Location: app.rs:604-606
   - Observation: Comment says "no need to abort" but if timeout fires, the task continues running until runtime drops
   - Impact: Low - task will exit on next shutdown flag check in reconnect loop
   - Suggestion: Could explicitly abort after timeout for deterministic cleanup

## Shutdown Sequence Summary

```
1. ctrl_c received
2. Exit main loop (break)
3. Output final statistics
4. Close Parquet writer (writer.close())
5. Close followup writer (lock + close)
6. Abort tick handle (no wait)
7. Send Shutdown to position tracker actor
8. Join position tracker task (5s timeout)
9. Signal WS shutdown (flag)
10. Join WS task (5s timeout)
11. Return Ok(())
```

This sequence is correct for graceful shutdown.

## Verdict
APPROVED

**Summary**: All four P0/P1 issues from the previous review have been properly fixed. The shutdown sequence is now correct with appropriate timeout guards. No new critical issues were introduced.

**Remaining Work** (optional, not blocking):
1. Add `JoinSet` for followup tasks if explicit cancellation is desired
2. Consider explicit abort after WS timeout for deterministic behavior
3. Add integration tests for shutdown sequence
