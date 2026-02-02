# WSドリブン型Exit改善計画

## Metadata

| Item | Value |
|------|-------|
| Created | 2026-02-03 |
| Task | 利確ロジックのWSドリブン化 |
| Status | `[PLANNING]` |
| Priority | P0 - Trading Philosophy直結 |

## 問題分析

### 現在のアーキテクチャ（遅延あり）

```
WS Message → App.handle_market_event()
                    ↓
             market_state.update_bbo/ctx()
                    ↓ [データ更新のみ]

             [最大200ms後]
                    ↓
             MarkRegressionMonitor.ticker.tick().await
                    ↓
             check_exit() → flatten送信
```

### 問題点

| 問題 | 影響 | 詳細 |
|------|------|------|
| **ポーリング遅延** | 利確機会の逸失 | 乖離縮小は100ms未満で発生、200ms間隔では逃す |
| **平均遅延100ms** | 利益減少 | Exit時点で乖離がさらに縮小している |
| **CPU非効率** | 無駄なチェック | 変化がなくても200ms毎にポーリング |

### Trading Philosophyとの乖離

> **正しいエッジ**: オラクルが動いた後、マーケットメーカーの注文が追従していない「取り残された流動性」を取る

乖離は**短命**（通常100ms未満で解消）。ポーリングでは利確タイミングを逃す。

---

## 改善案: WSドリブン型Exit

### 理想のアーキテクチャ

```
WS Message → App.handle_market_event()
                    ↓
             market_state.update_bbo/ctx()
                    ↓
             exit_watcher.on_market_update(key, snapshot)
                    ↓ [即座にチェック]
             Exit条件満たす → flatten_tx.send()
```

### 設計: ExitWatcher

**新コンポーネント**: `ExitWatcher` - BBO/Oracle更新時に即座にExit判定

```rust
// crates/hip3-position/src/exit_watcher.rs

pub struct ExitWatcher {
    config: MarkRegressionConfig,
    position_handle: PositionTrackerHandle,
    flatten_tx: mpsc::Sender<PendingOrder>,
    local_flattening: RwLock<HashSet<MarketKey>>,
}

impl ExitWatcher {
    /// WS更新時にAppから呼ばれる（同期的に高速チェック）
    pub fn on_market_update(&self, key: MarketKey, snapshot: &MarketSnapshot) {
        // 1. このマーケットにポジションがあるか？
        let position = match self.position_handle.get_position(&key) {
            Some(p) => p,
            None => return, // ポジションなし、即リターン
        };

        // 2. 既にflatten中か？
        if self.local_flattening.read().contains(&key) {
            return;
        }
        if self.position_handle.is_flattening(&key) {
            return;
        }

        // 3. Exit条件チェック（MarkRegressionと同じロジック）
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        if let Some(edge_bps) = self.check_exit(&position, snapshot, now_ms) {
            // 4. 即座にflatten送信
            self.trigger_exit(&position, edge_bps, snapshot, now_ms);
        }
    }

    fn check_exit(&self, position: &Position, snapshot: &MarketSnapshot, now_ms: u64) -> Option<Decimal> {
        // MarkRegressionMonitor.check_exit()と同じロジック
    }

    fn trigger_exit(&self, ...) {
        // MarkRegressionMonitor.trigger_exit()と同じロジック（同期版）
    }
}
```

### 呼び出し箇所の変更

**File**: `crates/hip3-bot/src/app.rs`

```rust
// handle_market_event() 内

MarketEvent::BboUpdate { key, bbo } => {
    // ... 既存のメトリクス/state更新 ...
    self.market_state.update_bbo(key, bbo, None);

    // [NEW] WSドリブンExitチェック
    if let Some(snapshot) = self.market_state.get_snapshot(&key) {
        if let Some(exit_watcher) = &self.exit_watcher {
            exit_watcher.on_market_update(key, &snapshot);
        }
    }
}

MarketEvent::CtxUpdate { key, ctx } => {
    // ... 既存のメトリクス/state更新 ...
    self.market_state.update_ctx(key, ctx);

    // [NEW] WSドリブンExitチェック
    if let Some(snapshot) = self.market_state.get_snapshot(&key) {
        if let Some(exit_watcher) = &self.exit_watcher {
            exit_watcher.on_market_update(key, &snapshot);
        }
    }
}
```

---

## 実装計画

### Step 1: ExitWatcher作成

**File**: `crates/hip3-position/src/exit_watcher.rs`

| 項目 | 内容 |
|------|------|
| 構造体 | `ExitWatcher` |
| メソッド | `on_market_update()`, `check_exit()`, `trigger_exit()` |
| 依存 | `PositionTrackerHandle`, `flatten_tx`, `MarkRegressionConfig` |

### Step 2: PositionTrackerHandle拡張

**File**: `crates/hip3-position/src/tracker.rs`

| 追加メソッド | 目的 |
|-------------|------|
| `get_position(&MarketKey) -> Option<Position>` | 特定マーケットのポジション取得 |

### Step 3: App統合

**File**: `crates/hip3-bot/src/app.rs`

| 変更箇所 | 内容 |
|---------|------|
| `App` struct | `exit_watcher: Option<Arc<ExitWatcher>>` 追加 |
| `run_trading_mode()` | ExitWatcher初期化 |
| `handle_market_event()` | BBO/Ctx更新時にExitWatcher呼び出し |

### Step 4: MarkRegressionMonitorとの共存

**選択肢**:

| Option | メリット | デメリット |
|--------|---------|----------|
| A: 両方有効 | バックアップ機能 | 重複チェック |
| B: ExitWatcherのみ | シンプル | 障害時のバックアップなし |
| C: check_interval_ms=0で無効化 | 既存コード変更なし | 設定で制御 |

**推奨**: Option A（両方有効）
- ExitWatcherが主系（即座）
- MarkRegressionMonitorがバックアップ（200ms間隔）
- local_flatteningで重複防止済み

---

## 変更ファイル一覧

| ファイル | 変更内容 |
|---------|---------|
| `crates/hip3-position/src/exit_watcher.rs` | **新規作成** |
| `crates/hip3-position/src/lib.rs` | `exit_watcher` モジュール追加 |
| `crates/hip3-position/src/tracker.rs` | `get_position()` 追加 |
| `crates/hip3-bot/src/app.rs` | ExitWatcher統合 |

---

## リスク評価

| リスク | 対策 |
|-------|------|
| flatten重複送信 | `local_flattening` + `is_flattening()` で防止 |
| WSハンドラ遅延 | check_exit()は軽量、flatten送信はtry_send() |
| ポジション取得競合 | PositionTrackerはDashMapベースで競合なし |

---

## 期待効果

| 指標 | Before | After |
|------|--------|-------|
| Exit検知遅延 | 平均100ms | < 1ms |
| 利確精度 | 乖離縮小後 | 乖離縮小直後 |
| CPU使用 | 200ms毎ポーリング | イベント駆動のみ |

---

## 検証計画

### 1. 単体テスト

```rust
#[test]
fn test_exit_watcher_triggers_on_regression() {
    // Setup: position exists, BBO returns to oracle
    // Assert: flatten order sent
}

#[test]
fn test_exit_watcher_no_duplicate() {
    // Setup: position already flattening
    // Assert: no new flatten order
}
```

### 2. 統合テスト

```bash
# VPSログでExit発火タイミング確認
docker logs hip3-bot 2>&1 | grep -E "MarkRegression exit|ExitWatcher"
```

### 3. メトリクス確認

```
# Exit発火からflatten送信までの遅延
exit_trigger_to_flatten_ms histogram
```

---

## 参照した一次情報

| 項目 | ソース |
|------|--------|
| 現在のMarkRegression実装 | `crates/hip3-position/src/mark_regression.rs` |
| MarketState更新 | `crates/hip3-feed/src/market_state.rs` |
| WS更新ハンドラ | `crates/hip3-bot/src/app.rs:1680-1737` |

## 未確認事項

| 項目 | 確認方法 |
|------|----------|
| flatten_tx.send()の非同期性 | コードレビューで確認済み（mpsc channel） |
