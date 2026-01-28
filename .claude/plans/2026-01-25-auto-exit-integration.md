# Auto Exit Integration Plan

**Version**: 1.1 DRAFT
**作成日**: 2026-01-25
**対象**: 自動決済システムの統合
**目的**: エントリー済みポジションの自動イグジット機能を実装

---

## 1. 概要

### 1.1 現状の問題

レビュー（`review/2026-01-25-implementation-status-review.md`）より：

| 機能 | 状態 | 影響 |
|------|------|------|
| TimeStopMonitor | コード存在、**未統合** | 30秒後の自動決済なし |
| Flattener | コード存在、**未統合** | 決済状態管理なし |
| HardStop flatten | **未実装** | 緊急停止時の自動決済なし |
| RiskMonitor | コード存在、**未統合** | 累積損失/連続失敗監視なし |

**結論**: 現状は「エントリーのみ可能、イグジットは手動」。本番運用には自動決済の統合が**必須**。

### 1.2 目標状態

```
Fill受信 → PositionTracker更新
    ↓
TimeStopMonitor (毎秒チェック)
    └─→ 30秒超過? → reduce-only注文 → BatchScheduler

RiskMonitor (イベント監視)
    ├─→ 累積損失 > $1000? → HardStop発火
    └─→ 連続失敗 >= 5? → HardStop発火

HardStop発火時
    → flatten_all_positions() → 全ポジション即時決済
```

---

## 2. 参照した一次情報

| 項目 | ソース | パス |
|------|--------|------|
| TimeStopMonitor実装 | 既存コード | `crates/hip3-position/src/time_stop.rs` |
| Flattener実装 | 既存コード | `crates/hip3-position/src/flatten.rs` |
| RiskMonitor実装 | 既存コード | `crates/hip3-risk/src/hard_stop.rs` |
| Application構造 | 既存コード | `crates/hip3-bot/src/app.rs` |
| MarketStateCache | 既存コード | `crates/hip3-executor/src/executor.rs` |
| BatchScheduler | 既存コード | `crates/hip3-executor/src/batch.rs` |
| ExecutorLoop | 既存コード | `crates/hip3-executor/src/executor_loop.rs` |
| Phase B計画 | 計画文書 | `.claude/plans/2026-01-19-phase-b-executor-implementation.md` |

### 2.1 設計決定事項

レビューの質問への回答：

| 質問 | 決定 | 理由 |
|------|------|------|
| RiskMonitor はどちらに寄せるか | `hip3-risk::RiskMonitor` を使用 | 既に `on_event()` API と `ExecutionEvent` enum が実装済み |
| 価格ソース | `MarketStateCache::get_mark_px()` | BBO は MarketStateCache に無い。mark_px で代用 |

---

## 3. アーキテクチャ

### 3.1 統合後のデータフロー

```
Application
  │
  ├── ExecutorLoop
  │     ├── Executor
  │     │     ├── PositionTrackerHandle ✓
  │     │     ├── BatchScheduler ✓ (enqueue_reduce_only 既存)
  │     │     ├── HardStopLatch ✓
  │     │     └── MarketStateCache ✓ (get_mark_px 使用)
  │     └── Signer/NonceManager ✓
  │
  ├── PositionTracker (Actor) ✓
  │
  ├── TimeStopMonitor ← [NEW INTEGRATION]
  │     ├── MarkPriceProvider (hip3-executor に配置)
  │     ├── PositionTrackerHandle
  │     └── flatten_tx → BatchScheduler.enqueue_reduce_only()
  │
  ├── RiskMonitor (hip3-risk) ← [NEW INTEGRATION]
  │     ├── on_event() 直接呼び出し（同期）
  │     └── HardStopLatch
  │
  └── Flattener ← [PHASE 4 統合]
        └── flatten_all_positions() + リトライ
```

### 3.2 新規チャネル

| チャネル | 型 | 送信元 | 受信先 |
|---------|-----|--------|--------|
| `flatten_tx/rx` | `mpsc::Sender<PendingOrder>` | TimeStopMonitor | BatchScheduler |

**注**: RiskMonitor は `on_event()` を直接呼び出し（同期 API）。非同期チャネル不要。

---

## 4. 実装フェーズ

### Phase 1: MarkPriceProvider 実装

**目的**: TimeStopMonitor が価格情報を取得できるようにする

#### 4.1.1 PriceProvider の実態確認

**既存 API** (`crates/hip3-position/src/time_stop.rs:273-278`):

```rust
pub trait PriceProvider: Send + Sync {
    /// Get the current price for a market.
    fn get_price(&self, market: &MarketKey) -> Option<Price>;
}
```

**MarketStateCache の実態** (`crates/hip3-executor/src/executor.rs`):

```rust
impl MarketStateCache {
    pub fn get_mark_px(&self, market: &MarketKey) -> Option<Price> { ... }
    pub fn get(&self, market: &MarketKey) -> Option<MarketState> { ... }
}
```

#### 4.1.2 MarkPriceProvider 実装

**ファイル**: `crates/hip3-executor/src/price_provider.rs`（新規）

**理由**: `hip3-position` → `hip3-executor` の依存は許容されない。`hip3-executor` に Adapter を配置。

```rust
use hip3_core::market::MarketKey;
use hip3_core::Price;
use hip3_position::time_stop::PriceProvider;
use crate::executor::MarketStateCache;
use std::sync::Arc;

/// MarketStateCache を PriceProvider としてラップ
///
/// Mark price を返す（BBO は MarketStateCache に無いため）
pub struct MarkPriceProvider {
    market_state_cache: Arc<MarketStateCache>,
}

impl MarkPriceProvider {
    pub fn new(market_state_cache: Arc<MarketStateCache>) -> Self {
        Self { market_state_cache }
    }
}

impl PriceProvider for MarkPriceProvider {
    fn get_price(&self, market: &MarketKey) -> Option<Price> {
        self.market_state_cache.get_mark_px(market)
    }
}
```

#### 4.1.3 hip3-executor/src/lib.rs への追加

```rust
mod price_provider;
pub use price_provider::MarkPriceProvider;
```

#### 4.1.4 テスト

| テスト | 説明 |
|--------|------|
| `test_mark_price_provider_returns_price` | mark_px が正しく返る |
| `test_mark_price_provider_none_for_unknown` | 未知の market で None |

---

### Phase 2: TimeStopMonitor 統合

**目的**: 30秒超過ポジションを自動決済

#### 4.2.1 flatten チャネル作成と接続

**ファイル**: `crates/hip3-bot/src/app.rs`

```rust
// Application::run() 内、Trading mode 初期化時

// 1. flatten チャネル作成
let (flatten_tx, mut flatten_rx) = mpsc::channel::<PendingOrder>(100);

// 2. BatchScheduler への flatten_rx 接続タスク
let batch_scheduler_clone = batch_scheduler.clone();
let flatten_receiver_handle = tokio::spawn(async move {
    while let Some(order) = flatten_rx.recv().await {
        batch_scheduler_clone.enqueue_reduce_only(order);
    }
});
```

**注**: `BatchScheduler::enqueue_reduce_only()` は**既に実装済み**。新規追加不要。

#### 4.2.2 TimeStopMonitor 起動

**ファイル**: `crates/hip3-bot/src/app.rs`

```rust
// MarkPriceProvider 作成（hip3-executor から）
let price_provider = Arc::new(MarkPriceProvider::new(
    executor.market_state_cache().clone()
));

// TimeStopConfig（設定から読み込み）
let time_stop_config = TimeStopConfig {
    threshold_ms: self.config.time_stop.threshold_ms,
    reduce_only_timeout_ms: self.config.time_stop.reduce_only_timeout_ms,
};

// TimeStopMonitor 作成
let time_stop_monitor = TimeStopMonitor::with_defaults(
    time_stop_config,
    position_tracker.clone(),
    flatten_tx,
    price_provider,
);

// タスク起動
let time_stop_handle = tokio::spawn(async move {
    time_stop_monitor.run().await;
});
```

#### 4.2.3 設定構造体追加

**ファイル**: `crates/hip3-bot/src/config.rs`

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct TimeStopConfig {
    #[serde(default = "default_threshold_ms")]
    pub threshold_ms: u64,
    #[serde(default = "default_reduce_only_timeout_ms")]
    pub reduce_only_timeout_ms: u64,
    #[serde(default = "default_check_interval_ms")]
    pub check_interval_ms: u64,
    #[serde(default = "default_slippage_bps")]
    pub slippage_bps: u64,
}

fn default_threshold_ms() -> u64 { 30_000 }
fn default_reduce_only_timeout_ms() -> u64 { 60_000 }
fn default_check_interval_ms() -> u64 { 1_000 }
fn default_slippage_bps() -> u64 { 50 }

// AppConfig に追加
#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    // ... 既存フィールド ...

    #[serde(default)]
    pub time_stop: TimeStopConfig,
}
```

#### 4.2.4 テスト

| テスト | 説明 |
|--------|------|
| `test_time_stop_triggers_after_threshold` | 30秒後に flatten 発火 |
| `test_flatten_order_reaches_batch_scheduler` | チャネル経由で enqueue_reduce_only() 到達 |

---

### Phase 3: RiskMonitor 統合

**目的**: 累積損失/連続失敗で HardStop 発火

#### 4.3.1 RiskMonitor API 確認

**既存 API** (`crates/hip3-risk/src/hard_stop.rs`):

```rust
impl RiskMonitor {
    pub fn new(hard_stop: Arc<HardStopLatch>, config: RiskMonitorConfig) -> Self;
    pub fn on_event(&self, event: ExecutionEvent);  // 同期呼び出し
}

pub enum ExecutionEvent {
    OrderSubmitted { cloid: ClientOrderId },
    OrderFilled { cloid: ClientOrderId, pnl: Price },
    OrderRejected { cloid: ClientOrderId, reason: String },
    OrderTimeout { cloid: ClientOrderId },
    PositionClosed { market: MarketKey, pnl: Price },
}
```

#### 4.3.2 Application への RiskMonitor 追加

**ファイル**: `crates/hip3-bot/src/app.rs`

```rust
use hip3_risk::{RiskMonitor, RiskMonitorConfig, ExecutionEvent};

pub struct Application {
    // ... 既存フィールド ...

    /// RiskMonitor（Trading mode のみ）
    risk_monitor: Option<Arc<RiskMonitor>>,
}

// Application::run() 内、Trading mode 初期化時
let risk_monitor_config = RiskMonitorConfig {
    max_consecutive_failures: self.config.risk_monitor.max_consecutive_failures,
    max_loss_usd: self.config.risk_monitor.max_loss_usd,
    max_flatten_failed: self.config.risk_monitor.max_flatten_failed,
    window_seconds: self.config.risk_monitor.window_seconds,
};

let risk_monitor = Arc::new(RiskMonitor::new(
    hard_stop_latch.clone(),
    risk_monitor_config,
));

self.risk_monitor = Some(risk_monitor);
```

#### 4.3.3 ExecutionEvent 送信ポイント

**ファイル**: `crates/hip3-bot/src/app.rs`

**handle_order_update() 内** (status は String 型):

```rust
fn handle_order_update(&self, payload: &OrderUpdatePayload) {
    // ... 既存処理 ...

    // RiskMonitor へイベント送信
    if let Some(ref risk_monitor) = self.risk_monitor {
        let status = payload.status.as_str();

        // rejected パターン検出
        if status == "rejected" || status.ends_with("Rejected") {
            if let Some(cloid) = self.parse_cloid(&payload.order.cloid) {
                risk_monitor.on_event(ExecutionEvent::OrderRejected {
                    cloid,
                    reason: format!("status={}", status),
                });
            }
        }
    }
}
```

**handle_user_fill() 内**:

```rust
fn handle_user_fill(&self, fill: &UserFill) {
    // ... 既存処理（PnL 計算含む）...

    // RiskMonitor へイベント送信
    if let Some(ref risk_monitor) = self.risk_monitor {
        if let Some(cloid) = self.parse_cloid(&fill.cloid) {
            risk_monitor.on_event(ExecutionEvent::OrderFilled {
                cloid,
                pnl: calculated_pnl,
            });
        }
    }
}
```

#### 4.3.4 設定構造体追加

**ファイル**: `crates/hip3-bot/src/config.rs`

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct RiskMonitorConfig {
    #[serde(default = "default_max_consecutive_failures")]
    pub max_consecutive_failures: u32,
    #[serde(default = "default_max_loss_usd")]
    pub max_loss_usd: f64,
    #[serde(default = "default_max_flatten_failed")]
    pub max_flatten_failed: u32,
    #[serde(default = "default_window_seconds")]
    pub window_seconds: u64,
}

fn default_max_consecutive_failures() -> u32 { 5 }
fn default_max_loss_usd() -> f64 { 1000.0 }
fn default_max_flatten_failed() -> u32 { 3 }
fn default_window_seconds() -> u64 { 3600 }

// AppConfig に追加
#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    // ... 既存フィールド ...

    #[serde(default)]
    pub risk_monitor: RiskMonitorConfig,
}
```

#### 4.3.5 テスト

| テスト | 説明 |
|--------|------|
| `test_risk_monitor_triggers_on_consecutive_failures` | 連続失敗で HardStop |
| `test_risk_monitor_triggers_on_max_loss` | 損失上限で HardStop |

---

### Phase 4: HardStop Flatten 統合

**目的**: HardStop 発火時に全ポジションを即時決済

#### 4.4.1 flatten_all_positions API 確認

**既存 API** (`crates/hip3-position/src/flatten.rs`):

```rust
/// Convert all positions to flatten requests (e.g., for HardStop).
pub fn flatten_all_positions(
    positions: &[Position],
    reason: FlattenReason,
    now_ms: u64
) -> Vec<FlattenRequest>

pub struct FlattenRequest {
    pub market: MarketKey,
    pub side: OrderSide,
    pub size: Size,
    pub reason: FlattenReason,
    pub requested_at: u64,
}
```

#### 4.4.2 HardStop 発火時の処理

**ファイル**: `crates/hip3-bot/src/app.rs` (または `crates/hip3-executor/src/executor_loop.rs`)

```rust
impl Application {
    /// HardStop 発火時の全ポジション決済
    async fn on_hard_stop_triggered(&self) {
        tracing::warn!("🛑 HardStop triggered, flattening all positions");

        let executor_loop = match &self.executor_loop {
            Some(el) => el,
            None => return,
        };

        let position_tracker = match &self.position_tracker {
            Some(pt) => pt,
            None => return,
        };

        // 全ポジション取得
        let positions: Vec<Position> = position_tracker
            .positions_snapshot()
            .into_values()
            .collect();

        if positions.is_empty() {
            tracing::info!("No positions to flatten");
            return;
        }

        // FlattenRequest 生成
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let flatten_requests = flatten_all_positions(
            &positions,
            FlattenReason::HardStop,
            now_ms,
        );

        // 各 FlattenRequest を PendingOrder に変換して enqueue
        let executor = executor_loop.executor();
        let batch_scheduler = executor.batch_scheduler();
        let market_state_cache = executor.market_state_cache();
        let slippage_bps = self.config.time_stop.slippage_bps;

        for request in flatten_requests {
            // 価格取得（mark_px）
            let price = match market_state_cache.get_mark_px(&request.market) {
                Some(p) => p,
                None => {
                    tracing::error!(
                        "Cannot flatten {}: no mark price available",
                        request.market
                    );
                    continue;
                }
            };

            // PendingOrder 生成（reduce-only）
            let pending_order = self.build_reduce_only_order(
                &request,
                price,
                slippage_bps,
            );

            // BatchScheduler に enqueue（優先キュー）
            batch_scheduler.enqueue_reduce_only(pending_order);
        }

        tracing::info!(
            "Enqueued {} flatten orders",
            flatten_requests.len()
        );
    }

    /// reduce-only 注文を構築
    fn build_reduce_only_order(
        &self,
        request: &FlattenRequest,
        mark_price: Price,
        slippage_bps: u64,
    ) -> PendingOrder {
        // 反対側の注文を作成
        let order_side = request.side.opposite();

        // スリッページ適用価格
        let limit_price = if order_side == OrderSide::Buy {
            // Buy (close short): mark * (1 + slippage)
            mark_price * (10000 + slippage_bps as i64) / 10000
        } else {
            // Sell (close long): mark * (1 - slippage)
            mark_price * (10000 - slippage_bps as i64) / 10000
        };

        PendingOrder {
            market: request.market.clone(),
            side: order_side,
            size: request.size,
            limit_price,
            time_in_force: TimeInForce::Ioc,
            reduce_only: true,
            cloid: ClientOrderId::generate(),
            created_at: request.requested_at,
        }
    }
}
```

#### 4.4.3 HardStopLatch 監視タスク（リトライ付き）

```rust
// Application::run() 内

let hard_stop_latch_clone = hard_stop_latch.clone();
let app_clone = self.clone(); // Application を Arc で包む必要あり

let hard_stop_watch_handle = tokio::spawn(async move {
    let mut triggered = false;
    let mut retry_count = 0;
    const MAX_RETRIES: u32 = 3;
    const RETRY_INTERVAL_MS: u64 = 1000;

    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;

        if hard_stop_latch_clone.is_triggered() && !triggered {
            triggered = true;
            tracing::warn!("HardStop detected, initiating flatten sequence");
        }

        if triggered {
            // flatten 実行
            app_clone.on_hard_stop_triggered().await;

            // ポジションが残っているかチェック
            let remaining = app_clone.position_tracker
                .as_ref()
                .map(|pt| pt.positions_snapshot().len())
                .unwrap_or(0);

            if remaining == 0 {
                tracing::info!("All positions flattened successfully");
                break;
            }

            retry_count += 1;
            if retry_count >= MAX_RETRIES {
                tracing::error!(
                    "⚠️  CRITICAL: {} positions remain after {} retries. Manual intervention required.",
                    remaining,
                    MAX_RETRIES
                );
                break;
            }

            tracing::warn!(
                "Retry {}/{}: {} positions remaining",
                retry_count,
                MAX_RETRIES,
                remaining
            );
            tokio::time::sleep(Duration::from_millis(RETRY_INTERVAL_MS)).await;
        }
    }
});
```

#### 4.4.4 テスト

| テスト | 説明 |
|--------|------|
| `test_hard_stop_triggers_flatten_all` | HardStop で全ポジション決済 |
| `test_hard_stop_flatten_retry` | 失敗時にリトライ |
| `test_hard_stop_flatten_order_priority` | reduce-only が通常注文より優先 |

---

### Phase 5: Flattener 状態管理統合（オプション）

**目的**: 決済注文の状態追跡（InProgress/Completed/Failed）

**注**: Phase 1-4 完了後に実装。Phase 4 のリトライロジックで最低限の堅牢性は確保済み。

#### 4.5.1 Flattener インスタンス追加

```rust
pub struct Application {
    // ... 既存フィールド ...
    flattener: Option<Arc<Mutex<Flattener>>>,
}
```

#### 4.5.2 状態追跡フロー

1. `TimeStopMonitor` / `HardStop` が flatten 発火: `flattener.start_flatten()`
2. `handle_order_update()` で reduce-only 約定: `flattener.mark_completed()`
3. タイムアウト検出: `flattener.check_timeouts()` → リトライ

---

## 5. 実装順序

| 順序 | Phase | 依存関係 | 推定作業量 |
|------|-------|----------|-----------|
| 1 | Phase 1: MarkPriceProvider | なし | 小 |
| 2 | Phase 2: TimeStopMonitor | Phase 1 | 中 |
| 3 | Phase 3: RiskMonitor | なし | 小 |
| 4 | Phase 4: HardStop Flatten | Phase 1 | 中 |
| 5 | Phase 5: Flattener 状態管理 | Phase 4 | 中（オプション） |

---

## 6. 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `crates/hip3-executor/src/price_provider.rs` | **新規**: MarkPriceProvider 実装 |
| `crates/hip3-executor/src/lib.rs` | price_provider モジュール追加 |
| `crates/hip3-bot/src/app.rs` | TimeStopMonitor, RiskMonitor, HardStop flatten 統合 |
| `crates/hip3-bot/src/config.rs` | TimeStopConfig, RiskMonitorConfig 追加 |
| `config/default.toml` | time_stop, risk_monitor セクション追加 |

---

## 7. 設定追加

### config/default.toml

```toml
[time_stop]
threshold_ms = 30000          # 30秒でフラット化
reduce_only_timeout_ms = 60000  # 60秒でタイムアウト
check_interval_ms = 1000      # 1秒ごとにチェック
slippage_bps = 50             # 0.5% スリッページ許容

[risk_monitor]
max_consecutive_failures = 5  # 連続失敗閾値
max_loss_usd = 1000.0         # 累積損失閾値 ($1,000)
max_flatten_failed = 3        # flatten失敗閾値
window_seconds = 3600         # 1時間ウィンドウ
```

---

## 8. テスト計画

### 8.1 ユニットテスト

| テスト | ファイル | 内容 |
|--------|----------|------|
| `test_mark_price_provider` | `price_provider.rs` | PriceProvider 実装 |
| `test_time_stop_trigger` | `time_stop.rs` | 30秒超過検出 |
| `test_risk_monitor_events` | `hard_stop.rs` | イベント処理 |

### 8.2 統合テスト

| テスト | 内容 |
|--------|------|
| `test_time_stop_integration` | TimeStopMonitor → BatchScheduler |
| `test_hard_stop_flatten_integration` | HardStop → 全ポジション決済 |
| `test_risk_monitor_hard_stop_integration` | RiskMonitor → HardStop 発火 |

### 8.3 手動検証（Mainnet 少額）

| 検証項目 | 手順 |
|----------|------|
| TimeStop 動作確認 | ポジション保持 → 30秒後に自動決済 |
| HardStop 動作確認 | 手動 HardStop → 全ポジション決済 |

---

## 9. リスク評価

### 9.1 実装リスク

| リスク | 影響度 | 対策 |
|--------|--------|------|
| reduce-only 注文失敗 | 高 | Phase 4 でリトライロジック実装（最大3回）|
| 価格取得失敗時の flatten | 高 | エラーログ + 手動介入アラート |
| チャネル詰まり | 中 | bounded channel (100)、バッファサイズ調整 |

### 9.2 運用リスク

| リスク | 影響度 | 対策 |
|--------|--------|------|
| flatten 注文の約定失敗 | 高 | リトライ + CRITICAL ログ + 手動介入 |
| 長時間ダウン後の再起動 | 中 | position 復元 → 即時 TimeStop 発火（意図通り）|

---

## 10. 非交渉ライン

| 項目 | 要件 |
|------|------|
| TimeStop 閾値 | 30秒以下で自動決済開始 |
| HardStop 即時性 | 発火から 1 秒以内に flatten 開始 |
| reduce-only 優先度 | 通常注文より優先して処理 |
| 累積損失監視 | $1,000 超過で HardStop |
| 連続失敗監視 | 5回連続失敗で HardStop |
| flatten リトライ | 最大3回、残ポジションあれば CRITICAL アラート |

---

## 11. Changelog

| Version | Date | Changes |
|---------|------|---------|
| 1.0 | 2026-01-25 | 初版作成 |
| 1.1 | 2026-01-25 | Review #1 対応: API 整合性修正、設計決定事項追加 |

---

## 12. Review History

| # | Date | Reviewer | Findings | Status |
|---|------|----------|----------|--------|
| 1 | 2026-01-25 | code-reviewer | HIGH 4件, MEDIUM 2件, LOW 1件 | ✅ v1.1 で対応 |

### Review #1 対応内容

| Finding | 対応 |
|---------|------|
| [HIGH] PriceProviderAdapter API 不一致 | `get_price()` のみ使用、`hip3-executor` に配置、`get_mark_px()` 使用 |
| [HIGH] RiskMonitor 型混在 | `hip3-risk::RiskMonitor` に統一、`on_event()` 直接呼び出し |
| [HIGH] handle_order_update() API 不一致 | status を String として扱い、パターンマッチで判定 |
| [HIGH] HardStop flatten API 不一致 | `flatten_all_positions()` + `executor().xxx()` 経由アクセス |
| [MEDIUM] 設定構造体 wiring 不足 | `TimeStopConfig`, `RiskMonitorConfig` を `AppConfig` に追加 |
| [MEDIUM] HardStop flatten リトライなし | 最大3回リトライ、残ポジションで CRITICAL アラート |
| [LOW] reduce-only キュー重複 | 「既に実装済み」と明記、重複作業削除 |
