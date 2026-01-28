# Position Tracker Sync Fix Implementation Spec

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-01-29 |
| Last Updated | 2026-01-29 |
| Status | `[COMPLETED]` |
| Source Plan | `.claude/plans/2026-01-29-position-tracker-sync-fix.md` |

## Implementation Status

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P0-1 | Post Response約定時にPositionTracker.fill()を呼び出す | [x] DONE | executor_loop.rs lines 656-704 |
| P0-2 | PositionTrackerMsg::FillにOption<ClientOrderId>を追加 | [x] DONE | tracker.rs |
| P0-3 | fill()メソッドにcloid引数を追加 | [x] DONE | tracker.rs |
| P0-4 | PositionTrackerTaskにrecent_fill_cloidsを追加 | [x] DONE | tracker.rs (HashSet<ClientOrderId>) |
| P0-5 | on_fill()でcloidベース重複検出を実装 | [x] DONE | tracker.rs (1000件超過時にクリア) |
| P0-6 | userFillsハンドラでcloidを渡すように修正 | [x] DONE | app.rs handle_user_fill() |
| P1-1 | position_resync_interval_secs設定を追加 | [x] DONE | config.rs (default: 60秒) |
| P1-2 | 定期resyncタスクをmain loopに追加 | [x] DONE | app.rs tokio::select! |
| TEST | 全テスト通過を確認 | [x] DONE | cargo test all passed |

## Deviations from Plan

None - 計画通りに実装完了。

## Key Implementation Details

### 1. Post Response Fill → Position更新 (executor_loop.rs)

```rust
OrderResponseStatus::Filled { oid, total_sz, avg_px } => {
    // 1. ORDER状態を更新 (terminal)
    self.executor.position_tracker()
        .order_update(cloid.clone(), OrderState::Filled, order.size, Some(*oid))
        .await;

    // 2. POSITION状態も更新 (userFillsに依存しない)
    let fill_price = avg_px.parse::<rust_decimal::Decimal>()
        .map(Price::new).unwrap_or(order.price);
    let fill_size = total_sz.parse::<rust_decimal::Decimal>()
        .map(Size::new).unwrap_or(order.size);

    self.executor.position_tracker()
        .fill(
            order.market,
            order.side,
            fill_price,
            fill_size,
            chrono::Utc::now().timestamp_millis() as u64,
            Some(cloid.clone()), // cloid for deduplication
        )
        .await;
}
```

### 2. Cloid-Based Deduplication (tracker.rs)

```rust
fn on_fill(&mut self, ..., cloid: Option<ClientOrderId>) {
    // Cloid-based deduplication
    if let Some(ref id) = cloid {
        if self.recent_fill_cloids.contains(id) {
            debug!("Skipping duplicate fill: cloid={}", id);
            return;
        }
        self.recent_fill_cloids.insert(id.clone());

        // Prevent memory leak: clear when size exceeds threshold
        if self.recent_fill_cloids.len() > 1000 {
            self.recent_fill_cloids.clear();
        }
    }
    // ... rest of position update logic
}
```

### 3. Periodic Position Resync (app.rs)

```rust
// Configuration: position_resync_interval_secs (default: 60)
let mut resync_interval = if resync_interval_secs > 0 {
    Some(tokio::time::interval(Duration::from_secs(resync_interval_secs)))
} else {
    None
};

// In main loop select!
Some(_) = async { ... } => {
    if self.config.mode == OperatingMode::Trading {
        match self.sync_positions_from_api(tracker, user_addr).await {
            Ok(()) => debug!("Periodic position resync completed"),
            Err(e) => warn!(?e, "Periodic position resync failed"),
        }
    }
}
```

## Modified Files

| File | Changes |
|------|---------|
| `crates/hip3-position/src/tracker.rs` | fill()にcloid引数追加、重複検出実装、HashSetフィールド追加 |
| `crates/hip3-executor/src/executor_loop.rs` | Filled時にposition_tracker.fill()呼び出し追加 |
| `crates/hip3-bot/src/app.rs` | userFillsでcloid渡す、定期resync追加 |
| `crates/hip3-bot/src/config.rs` | position_resync_interval_secs追加 |
| `crates/hip3-risk/src/gates.rs` | fill()呼び出しにNone追加（テスト） |
| `crates/hip3-executor/src/executor.rs` | fill()呼び出しにNone追加（テスト） |

## Verification

- [x] `cargo fmt` - OK
- [x] `cargo clippy -- -D warnings` - OK
- [x] `cargo test` - All passed
- [ ] VPSデプロイ後のログ確認（運用時）
