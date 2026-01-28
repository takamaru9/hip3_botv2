# Position Tracker Sync Fix Implementation Spec

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-01-29 |
| Last Updated | 2026-01-29 |
| Status | `[COMPLETED]` + BUG-002 Hotfix |
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

---

## BUG-002: Race Condition in sync_positions

**発見日**: 2026-01-29
**重大度**: Critical (本番で $1,607 の意図しないポジションを発生)

### 問題

`max_notional_per_market = 50` の制限が効かず、TSLAポジションが $1,607 まで膨らんだ。

### 根本原因

`PositionTrackerHandle::sync_positions()` に競合状態があった：

```rust
// BUG: Handle でキャッシュを即座にクリア
pub async fn sync_positions(&self, positions: Vec<Position>) {
    self.positions_cache.clear();  // ← 即座にクリア
    self.positions_data.clear();   // ← 即座にクリア

    // Actor への非同期メッセージ送信
    let _ = self.tx.send(PositionTrackerMsg::SyncPositions(positions)).await;
}
```

**問題の流れ:**
1. 定期resync（60秒ごと）が実行される
2. Handle がキャッシュを即座にクリア
3. Actor がメッセージを処理する前に新しいシグナルが来る
4. Gate 3: `get_notional()` → キャッシュが空なので `0` を返す
5. Gate 6: `has_position()` → キャッシュが空なので `false` を返す
6. すべてのGateを通過し、注文が実行される
7. これが繰り返され、ポジションが無制限に蓄積

### 修正

1. **Handle からキャッシュクリアを削除** - Actor に任せる
2. **Actor の処理順序を変更** - clear-then-add から add-then-remove へ

```rust
// FIX: Handle ではキャッシュをクリアしない
pub async fn sync_positions(&self, positions: Vec<Position>) {
    // NOTE: Cache clearing removed to fix race condition (BUG-002)
    let _ = self.tx.send(PositionTrackerMsg::SyncPositions(positions)).await;
}

// FIX: Actor は add-then-remove で処理
fn on_sync_positions(&mut self, new_positions: Vec<Position>) {
    // Step 1: Add/update all new positions FIRST
    for pos in new_positions { ... }

    // Step 2: Remove positions that are NOT in the new list
    // (Only after new positions are added)
    for market in markets_to_remove { ... }
}
```

### 修正ファイル

| File | Changes |
|------|---------|
| `crates/hip3-position/src/tracker.rs` | Handle の sync_positions() からキャッシュクリアを削除、Actor の on_sync_positions() を add-then-remove に変更 |

### Commit

`00f0522` - fix(position): Fix race condition in sync_positions causing position limit bypass (BUG-002)

---

## BUG-003: Zero-Size Orders on Low-Liquidity Markets

**発見日**: 2026-01-29
**重大度**: Medium (取引所エラーが繰り返し発生、注文は実行されず)

### 問題

PLATINUM市場で "Order has zero size" エラーが繰り返し発生。シグナルは検出されるが、取引所で拒否される。

### 根本原因

`suggested_size` が `lot_size` より小さい場合、`round_to_lot()` で0に丸められる：

```
PLATINUM: sz_decimals=4 → lot_size=0.0001

1. Detector: suggested_size = 0.00005 (流動性が低い)
2. Signer: round_to_lot(0.00005, 0.0001)
   = floor(0.00005 / 0.0001) * 0.0001
   = floor(0.5) * 0.0001
   = 0 * 0.0001 = 0
3. 取引所: "Order has zero size" エラー
```

Detectorは `is_zero()` チェックをしているが、`lot_size` での丸め前の値をチェックしているため、丸め後に0になるケースを検出できなかった。

### 修正

App側で `on_signal()` を呼ぶ前にサイズをlot_sizeで丸め、0ならスキップするゲートを追加：

```rust
// Gate: Check if size rounds to zero after lot_size truncation
let lot_size = self.spec_cache.get(&signal.market_key)
    .map(|spec| spec.lot_size)
    .unwrap_or(Size::new(Decimal::new(1, 4)));
let rounded_size = signal.suggested_size.round_to_lot(lot_size);
if rounded_size.is_zero() {
    debug!(
        market = %signal.market_key,
        suggested_size = %signal.suggested_size,
        lot_size = %lot_size,
        "Signal dropped: size rounds to zero after lot_size truncation"
    );
    continue;
}

// Use rounded_size instead of suggested_size for execution
let result = executor_loop.executor().on_signal(
    &signal.market_key, signal.side, signal.best_px,
    rounded_size, // Rounded size
    current_time_ms(),
);
```

### 修正ファイル

| File | Changes |
|------|---------|
| `crates/hip3-bot/src/app.rs` | on_signal()呼び出し前にlot_sizeでの丸めチェック追加、丸めたサイズで注文実行 |

### Commit

`5770b24` - fix: Add lot_size truncation check to prevent zero-size orders
