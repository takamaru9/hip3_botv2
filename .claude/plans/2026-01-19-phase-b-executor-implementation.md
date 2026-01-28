# Phase B: 超小口IOC実弾 実装計画

**作成日**: 2026-01-19
**目的**: 滑り/手数料込みの実効EVを測定
**期間**: Week 13-16（約4週間）
**初期市場**: SNDK (xyz:28) ※Phase A 完了後に最終決定

---

## 1. 前提条件

### 1.1 Phase A 達成状況

| 条件 | 状態 | 詳細 |
|------|------|------|
| 24h連続稼働 | 🟡 部分達成 | 15h稼働、WS自律復旧1回確認 |
| EV正の市場特定 | ✅ 達成 | 6市場で高EV確認（HOOD, MSTR, NVDA, COIN, CRCL, SNDK） |
| Risk Gate停止品質 | ✅ 安定 | HeartbeatTimeout 1回、自律復旧 |
| API wallet準備 | ✅ 完了 | 取引用/観測用分離済み |

### 1.2 Phase B 初期パラメータ

| パラメータ | 値 | 理由 |
|-----------|-----|------|
| `SIZE_ALPHA` | 0.05 | Phase Aの半分（超保守的） |
| `MAX_NOTIONAL_PER_MARKET` | $50 | 初期は超小口 |
| `MAX_NOTIONAL_TOTAL` | $100 | 全市場合計 |
| `TIME_STOP_MS` | 30000 | 30秒でフラット化 |
| `REDUCE_ONLY_TIMEOUT_MS` | 60000 | 60秒でフラット化失敗アラート |

### 1.3 初期市場

| 優先度 | Market | Symbol | Mean Edge (bps) | シグナル数 |
|--------|--------|--------|-----------------|-----------|
| 1 | xyz:28 | **SNDK** | TBD | TBD |

※Phase A 完了後に最終決定。現時点では SNDK が有力候補。

---

## 2. アーキテクチャ

### 2.1 新規Crate構成

```
crates/
├── hip3-core/               # 共有型（循環依存回避）
│   ├── src/
│   │   ├── lib.rs
│   │   ├── types.rs         # PendingOrder, PendingCancel, ActionBatch
│   │   ├── order_id.rs      # ClientOrderId
│   │   └── market.rs        # MarketKey, MarketSpec, Price, Size
│   └── Cargo.toml
│
├── hip3-executor/           # IOC執行エンジン
│   ├── src/
│   │   ├── lib.rs
│   │   ├── nonce.rs         # NonceManager
│   │   ├── batch.rs         # BatchScheduler
│   │   ├── order.rs         # OrderBuilder、IOC発注
│   │   ├── signer.rs        # 署名処理
│   │   └── budget.rs        # ActionBudget拡張
│   └── Cargo.toml           # depends on: hip3-core
│
├── hip3-position/           # ポジション管理
│   ├── src/
│   │   ├── lib.rs
│   │   ├── tracker.rs       # PositionTracker
│   │   ├── flatten.rs       # フラット化ロジック
│   │   └── time_stop.rs     # TimeStop管理
│   └── Cargo.toml           # depends on: hip3-core
│
└── hip3-key/                # 鍵管理（セキュリティ）
    ├── src/
    │   ├── lib.rs
    │   ├── manager.rs       # KeyManager
    │   └── rotation.rs      # ローテーション
    └── Cargo.toml
```

#### 循環依存回避の設計方針

`hip3-executor` と `hip3-position` は相互に参照が必要だが、共有型を分離することで依存を単方向にする。

**解決策**: 共有型を `hip3-core` に配置、`hip3-executor` が `hip3-position` に依存

```
hip3-core（共有型）
    ↑               ↑
    │               │
hip3-executor ──→ hip3-position
    │
    └── hip3-position に依存（PositionTrackerHandle を直接使用）
```

**依存関係**:
- `hip3-core`: 共有型のみ、他crate に依存しない
- `hip3-position`: `hip3-core` に依存
- `hip3-executor`: `hip3-core` と `hip3-position` に依存

**hip3-core に配置する型**:
- `PendingOrder`, `PendingCancel`, `ActionBatch` - 注文の内部表現
- `TrackedOrder` - pending_orders 管理用（executor/position 共有）
- `ClientOrderId` - 注文 ID
- `MarketKey`, `MarketSpec`, `Price`, `Size`, `OrderSide` - 市場/価格型
- `TimeInForce`, `OrderWire`, `CancelWire` - wire format 型

**hip3-position が定義**:
- `PositionTrackerTask`, `PositionTrackerHandle`, `Position`, `TimeStop`, `Flattener`
- `PositionTrackerMsg` - actor メッセージ型

**hip3-executor が定義**:
- `NonceManager`, `BatchScheduler`, `Signer`, `OrderBuilder`
- `Executor`（`PositionTrackerHandle` を直接保持）
- `TradingReadyChecker`, `ExecutorLoop`

### 2.2 データフロー

```
[Detector] Signal
    │
    ▼
[Risk Gate] 再検証（保有時の追加Gate）
    │
    ▼
[Executor] IOC発注
    │  ├─ NonceManager: nonce採番
    │  ├─ BatchScheduler: 100ms周期
    │  └─ Signer: 署名
    │
    ▼
[Exchange] WS post
    │
    ▼
[Position] ポジション更新
    │  ├─ orderUpdates 監視
    │  └─ userFills 監視
    │
    ▼
[TimeStop] フラット化判定
    │
    ▼
[Executor] reduce-only IOC
```

---

## 3. 実装タスク

### 3.1 Week 1: hip3-executor基盤

#### P0-19a: NonceManager実装

##### 取引所制約
- 許容窓: (T-2days, T+1day) where T = ブロック時刻
- 高nonce上位100個: 最新100個のnonceのみ有効、古いものは無効化される
- 0起点禁止: 起動時に now_unix_ms へ fast-forward

```rust
/// Clock trait（テスト可能な時刻取得）
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }
}

pub struct NonceManager<C: Clock> {
    counter: AtomicU64,
    /// オフセット定義: server_time - local_time (正 = サーバが進んでいる)
    server_offset_ms: AtomicI64,
    last_sync_ms: AtomicU64,
    clock: C,
}

impl<C: Clock> NonceManager<C> {
    /// 起動時に now_unix_ms へ fast-forward（0起点禁止）
    pub fn new(clock: C) -> Self {
        let now_ms = clock.now_ms();
        Self {
            counter: AtomicU64::new(now_ms),
            server_offset_ms: AtomicI64::new(0),
            last_sync_ms: AtomicU64::new(now_ms),
            clock,
        }
    }

    /// サーバ時刻の近似値を計算
    /// approx_server_time = local_time + server_offset
    fn approx_server_time_ms(&self) -> u64 {
        let local = self.clock.now_ms();
        let offset = self.server_offset_ms.load(Ordering::SeqCst);
        if offset >= 0 {
            local + offset as u64
        } else {
            local.saturating_sub((-offset) as u64)
        }
    }

    /// nonce採番: max(last_nonce + 1, approx_server_time_ms())
    /// 「単調増加」と「時刻近傍」を両立
    pub fn next(&self) -> u64 {
        loop {
            let current = self.counter.load(Ordering::SeqCst);
            let server_approx = self.approx_server_time_ms();
            let next_nonce = current.saturating_add(1).max(server_approx);

            if self.counter
                .compare_exchange(current, next_nonce, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return next_nonce;
            }
        }
    }

    /// serverTime同期: counterもfast-forward
    pub fn sync_with_server(&self, server_time_ms: u64) -> Result<(), NonceError> {
        let local_ms = self.clock.now_ms();
        // offset = server - local (正 = サーバが進んでいる)
        let offset = server_time_ms as i64 - local_ms as i64;

        // ドリフト検知
        if offset.abs() > 5000 {
            return Err(NonceError::TimeDriftTooLarge(offset));
        }
        if offset.abs() > 2000 {
            tracing::warn!(offset_ms = offset, "Time drift detected (>2s)");
        }

        self.server_offset_ms.store(offset, Ordering::SeqCst);
        self.last_sync_ms.store(local_ms, Ordering::SeqCst);

        // counter も fast-forward（server_time_ms より低ければ追従）
        loop {
            let current = self.counter.load(Ordering::SeqCst);
            if current >= server_time_ms {
                break;
            }
            if self.counter
                .compare_exchange(current, server_time_ms, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                tracing::info!(
                    old = current,
                    new = server_time_ms,
                    "Counter fast-forwarded on sync"
                );
                break;
            }
        }

        Ok(())
    }
}
```

##### NonceManager テスト項目

| # | テスト | 期待動作 |
|---|--------|----------|
| 1 | 単調増加 | next() を連続呼出しで常に増加 |
| 2 | 並行呼び出し | 複数スレッドから呼び出しても重複なし |
| 3 | 時刻逆行 | MockClock で時刻を戻しても counter は減少しない |
| 4 | sync fast-forward | sync_with_server() で counter が server_time 以上になる |
| 5 | ドリフト 2s warn | offset 2001ms で warn ログ出力 |
| 6 | ドリフト 5s error | offset 5001ms で Err 返却 |
| 7 | 近傍維持 | next() が approx_server_time に追従 |

**タスク**:
- [ ] Clock trait 定義
- [ ] SystemClock 実装
- [ ] NonceManager<C: Clock> 構造体実装
- [ ] approx_server_time_ms() メソッド実装
- [ ] next() を max(last+1, approx_server_time) で実装
- [ ] serverTime同期 + counter fast-forward
- [ ] ドリフト検知（閾値: 2秒警告、5秒エラー）
- [ ] ユニットテスト（7項目）

#### P0-19b: BatchScheduler実装

##### 設計方針

| 項目 | 方針 |
|------|------|
| **バッチ単位** | **1 tick = 1 post = 1 L1 action**（複数 orders **または** cancels をまとめる、**同居しない**） |
| **nonce粒度** | 1 action = 1 nonce |
| **署名粒度** | 1 action = 1 署名 |
| **inflight消費** | **1 tick で最大 1 inflight 消費**（action 送信時にインクリメント） |
| **優先順位** | **cancel > reduce-only > new order**（3キュー構造） |
| **inflight上限** | 100、高水位(80)で新規注文のみ縮退 |
| **キュー上限** | cancel: 200、reduce_only: 500、new_order: 1000 |

##### 3キュー構造（優先順位の保証）

```
優先度高                                     優先度低
┌─────────────┐  ┌─────────────┐  ┌─────────────┐
│   cancel    │→ │ reduce_only │→ │  new_order  │
│  (cap:200)  │  │  (cap:500)  │  │  (cap:1000) │
└─────────────┘  └─────────────┘  └─────────────┘
     ↓                 ↓                 ↓
     └─────────────────┴─────────────────┘
                       ↓
              tick() で収集（優先順に）
```

##### キュー溢れ/縮退時の挙動（ActionBatch 仕様）

**tick() の ActionBatch 返却ルール**:
1. **cancel キューが空でない** → `ActionBatch::Cancels` を返す（orders は次 tick へ持ち越し）
2. **cancel キューが空** → `ActionBatch::Orders` を返す
   - 高水位時は reduce_only のみ、new_order は次 tick へ持ち越し

| 状態 | new_order | reduce_only | cancel | tick() 動作 |
|------|-----------|-------------|--------|-------------|
| 正常 (inflight < 80) | Queued | Queued | Queued | cancel あり→CancelBatch / なし→OrderBatch（全種） |
| 高水位 (80 ≤ inflight < 100) | **QueuedDegraded** | **Queued** | **Queued** | cancel あり→**CancelBatch** / なし→**OrderBatch（reduce_only のみ）** |
| 上限 (inflight = 100) | InflightFull | InflightFull | Queued | **None**（何も送れない、キューに残る） |
| キュー溢れ | QueueFull | QueueFull | QueueFull | 拒否 |

**重要（ActionBatch 仕様）**:
- **同一 tick で orders と cancels は同居しない**（SDK 仕様準拠）
- cancel が優先: cancel があれば CancelBatch を返し、orders は次 tick まで待機
- 高水位時: cancel → CancelBatch、cancel 空 → reduce_only のみの OrderBatch
- 上限時: **何も送れない**（cancel も含む）。inflight が減るまで待機。キューには残るので応答が来れば送信再開。

```rust
/// 3キュー構造: cancel > reduce_only > new_order
/// InflightTracker: 唯一の inflight ソース
///
/// 注意: crates/hip3-ws/src/rate_limiter.rs の RateLimiter とは別物。
/// - RateLimiter: WS 層のレート制限（リクエスト/秒など）
/// - InflightTracker: Executor 層の inflight post 管理（上限100）
///
/// 設計決定: RateLimiter と InflightTracker は責務が異なるため、
/// 二重会計ではなく「それぞれの層で管理」する。
/// ただし、将来的に RateLimiter に inflight 会計を統合する場合は、
/// InflightTracker を RateLimiter への参照に置き換える。
pub struct InflightTracker {
    count: AtomicU32,
    limit: u32, // 100
}

impl InflightTracker {
    pub fn new(limit: u32) -> Self {
        Self {
            count: AtomicU32::new(0),
            limit,
        }
    }

    pub fn current(&self) -> u32 {
        self.count.load(Ordering::SeqCst)
    }

    pub fn limit(&self) -> u32 {
        self.limit
    }

    /// increment: 上限を超えない（CASループで安全に加算）
    /// Returns: true if incremented, false if already at limit
    pub fn increment(&self) -> bool {
        loop {
            let current = self.count.load(Ordering::SeqCst);
            if current >= self.limit {
                return false; // 上限到達
            }
            if self.count
                .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return true;
            }
            // CAS失敗: リトライ
        }
    }

    /// decrement: 0未満にならない（saturating_sub 相当、CASループ）
    /// Returns: true if decremented, false if already at 0
    pub fn decrement(&self) -> bool {
        loop {
            let current = self.count.load(Ordering::SeqCst);
            if current == 0 {
                tracing::warn!("InflightTracker::decrement called at 0 (double decrement?)");
                return false; // underflow 防止
            }
            if self.count
                .compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return true;
            }
            // CAS失敗: リトライ
        }
    }

    /// 注意: reset() は使用しない。
    /// 切断時は ExecutorLoop::on_disconnect() で pending 数分 decrement する。
    #[deprecated(note = "Use decrement() in a loop instead")]
    pub fn reset(&self) {
        self.count.store(0, Ordering::SeqCst);
    }
}

pub struct BatchScheduler {
    interval: Duration,                                 // 100ms
    pending_cancels: Mutex<VecDeque<PendingCancel>>,    // 最優先
    pending_reduce_only: Mutex<VecDeque<PendingOrder>>, // 2番目（Flatten/TimeStop）
    pending_new_orders: Mutex<VecDeque<PendingOrder>>,  // 3番目（通常注文）
    inflight_tracker: Arc<InflightTracker>,
    config: BatchConfig,
    /// HardStop 参照（tick で new_order を skip するため、必須）
    /// new() の引数で受け取り、未設定を許容しない（安全装置）
    hard_stop_latch: Arc<HardStopLatch>,
}

pub struct BatchConfig {
    pub interval_ms: u64,               // 100
    pub max_orders_per_batch: usize,    // 50（1 action 内）
    pub max_cancels_per_batch: usize,   // 50（1 action 内）
    pub inflight_high_watermark: u32,   // 80（新規注文の縮退開始）
    pub cancel_queue_capacity: usize,   // 200
    pub reduce_only_queue_capacity: usize, // 500
    pub new_order_queue_capacity: usize,   // 1000
}

/// ActionBatch: 1 tick = 1 action type（SDK 仕様準拠）
///
/// **重要**: SDK は orders と cancels を **別々の action** として送信する。
/// - Order action: `{"type": "order", "orders": [...], "grouping": "na"}`
/// - Cancel action: `{"type": "cancel", "cancels": [...]}`
///
/// 同一 action に orders と cancels を同居させることは **しない**。
/// tick() は優先順位に従い、どちらか一方のみを返す。
#[derive(Debug, Clone)]
pub enum ActionBatch {
    /// 注文バッチ（type=order）
    Orders(Vec<PendingOrder>),
    /// キャンセルバッチ（type=cancel）
    Cancels(Vec<PendingCancel>),
}

/// enqueue の結果
pub enum EnqueueResult {
    Queued,           // 正常
    QueuedDegraded,   // キュー追加したが縮退中（新規注文のみ）
    QueueFull,        // キュー容量超過
    InflightFull,     // inflight上限（新規注文のみ）
}

impl BatchScheduler {
    /// 新規注文をキューに追加
    pub fn enqueue_new_order(&self, order: PendingOrder) -> EnqueueResult {
        let inflight = self.inflight_tracker.current();

        // inflight 上限時は新規注文を拒否
        if inflight >= 100 {
            return EnqueueResult::InflightFull;
        }

        let mut queue = self.pending_new_orders.lock();
        if queue.len() >= self.config.new_order_queue_capacity {
            return EnqueueResult::QueueFull;
        }

        queue.push_back(order);

        // 高水位時は縮退（キューに積むが送信遅延）
        if inflight >= self.config.inflight_high_watermark {
            return EnqueueResult::QueuedDegraded;
        }

        EnqueueResult::Queued
    }

    /// reduce-only 注文をキューに追加（高水位でも受付・送信）
    pub fn enqueue_reduce_only(&self, order: PendingOrder) -> EnqueueResult {
        debug_assert!(order.reduce_only, "Must be reduce_only order");

        let inflight = self.inflight_tracker.current();

        // inflight 上限時も受付（キューには積む）
        // tick() で cancel と一緒に送信される

        let mut queue = self.pending_reduce_only.lock();
        if queue.len() >= self.config.reduce_only_queue_capacity {
            tracing::error!("reduce_only queue full - CRITICAL");
            return EnqueueResult::QueueFull;
        }

        queue.push_back(order);

        if inflight >= 100 {
            return EnqueueResult::InflightFull; // キューには積んだ
        }

        EnqueueResult::Queued
    }

    /// キャンセルをキューに追加（常に受付）
    pub fn enqueue_cancel(&self, cancel: PendingCancel) -> EnqueueResult {
        let mut queue = self.pending_cancels.lock();

        if queue.len() >= self.config.cancel_queue_capacity {
            tracing::error!("cancel queue full - CRITICAL");
            return EnqueueResult::QueueFull;
        }

        queue.push_back(cancel);
        EnqueueResult::Queued
    }

    /// 100ms周期でバッチ収集（inflight increment は呼び出し元で行う）
    ///
    /// **SDK 仕様準拠**: 1 tick = 1 action type（orders と cancels は別々に送信）
    /// - 優先順位: cancel > orders（reduce_only + new_order）
    /// - cancel が pending の場合: CancelAction を返し、orders は次 tick へ持ち越し
    /// - cancel が空の場合: OrderAction を返す
    ///
    /// 注意: この関数は inflight を increment しない。
    /// 送信成功時に呼び出し元が `on_batch_sent()` を呼ぶこと。
    pub fn tick(&self) -> Option<ActionBatch> {
        let inflight = self.inflight_tracker.current();

        // inflight 上限時: 何も送れない（cancel も送れない）
        // → キューに残して inflight が減るまで待機
        if inflight >= 100 {
            tracing::debug!(inflight, "Inflight full, cannot send any batch");
            return None;
        }

        // 1. cancel を優先収集（pending があれば cancel のみ返す）
        let cancels = self.collect_cancels(self.config.max_cancels_per_batch);
        if !cancels.is_empty() {
            // cancel 優先: orders は次 tick へ持ち越し
            return Some(ActionBatch::Cancels(cancels));
        }

        // 2. cancel が空の場合のみ orders を収集
        // reduce_only を収集（高水位でも送信）
        let reduce_only = self.collect_reduce_only(self.config.max_orders_per_batch);

        // 高水位未満 かつ HardStop 中でなければ new_order も収集
        let new_orders = if inflight < self.config.inflight_high_watermark && !self.is_hard_stop() {
            let remaining = self.config.max_orders_per_batch.saturating_sub(reduce_only.len());
            self.collect_new_orders(remaining)
        } else {
            // 高水位時 or HardStop 中は新規注文 skip
            if self.is_hard_stop() {
                tracing::debug!("HardStop active, skipping new_orders in tick");
            }
            vec![]
        };

        // orders = reduce_only + new_orders
        let mut orders = reduce_only;
        orders.extend(new_orders);

        if orders.is_empty() {
            return None;
        }

        // 注意: ここでは increment しない（送信成功時に on_batch_sent() で行う）
        Some(ActionBatch::Orders(orders))
    }

    /// Batch 送信成功時に呼び出し（inflight increment）
    pub fn on_batch_sent(&self) {
        self.inflight_tracker.increment();
    }

    /// 失敗したバッチを先頭に再キュー（reduce_only のみ）
    pub fn requeue_reduce_only(&self, orders: Vec<PendingOrder>) {
        let mut queue = self.pending_reduce_only.lock();
        for order in orders.into_iter().rev() {
            if order.reduce_only {
                queue.push_front(order);
            }
        }
    }

    /// Batch 送信完了時に呼び出し
    pub fn on_batch_complete(&self) {
        self.inflight_tracker.decrement();
    }

    // 注意: on_disconnect() は BatchScheduler には実装しない。
    // 切断時の inflight 回収は ExecutorLoop::on_disconnect() で一元管理する。
    // これにより、reset() による一括ゼロ化と、pending 数分 decrement の二重管理を防ぐ。

    /// HardStop 発火時: new_order キューを全破棄して、drop した cloid リストを返す
    ///
    /// 呼び出し元は返された cloid に対して pending_markets_cache/pending_orders を cleanup する
    /// HardStop 発火時: new_order キューを全破棄して、drop した (cloid, market) を返す
    ///
    /// 呼び出し元は返された情報で pending_markets_cache/pending_orders を cleanup する
    /// NOTE: market は PendingOrder から直接取得するため、pending_orders_snapshot に依存しない（レース回避）
    pub fn drop_new_orders(&self) -> Vec<(ClientOrderId, MarketKey)> {
        let mut queue = self.pending_new_orders.lock();
        let dropped: Vec<(ClientOrderId, MarketKey)> = queue
            .iter()
            .map(|o| (o.cloid.clone(), o.market.clone()))
            .collect();
        queue.clear();
        tracing::warn!(count = dropped.len(), "Dropped new_order queue for HardStop");
        dropped
    }

    /// HardStop 中は new_order を返さない（tick 内で呼ばれる）
    fn is_hard_stop(&self) -> bool {
        self.hard_stop_latch.is_triggered()
    }
}
```

##### BatchScheduler テスト項目

| # | テスト | 期待動作 |
|---|--------|----------|
| 1 | 正常 enqueue | new_order/reduce_only/cancel が Queued |
| 2 | new_order キュー溢れ | 1001 件目で QueueFull |
| 3 | reduce_only キュー溢れ | 501 件目で QueueFull |
| 4 | cancel キュー溢れ | 201 件目で QueueFull |
| 5 | 高水位縮退 | new_order が QueuedDegraded、reduce_only は Queued |
| 6 | cancel 優先 | cancel pending 時は **CancelBatch のみ**返す、orders は次 tick |
| 7 | orders のみ | cancel 空の時は **OrderBatch**（reduce_only + new_order）を返す |
| 8 | inflight 上限時の tick | **None を返す**（cancel も送れない、キューに残る） |
| 9 | requeue_reduce_only | 失敗した reduce_only が先頭に戻る |
| 10 | tick は increment しない | tick() は inflight を変更しない（呼び出し元が管理） |
| 11 | InflightTracker 整合性 | increment/decrement が正しく動作、reset() は非推奨 |
| 12 | 1 tick = 1 action type | orders と cancels は **同居しない**（SDK 仕様） |

**注意**: `on_disconnect()` は BatchScheduler には実装しない。切断時の回収は ExecutorLoop で一元管理。

**タスク**:
- [ ] InflightTracker 構造体実装（唯一の inflight ソース）
- [ ] 3キュー構造 (cancel/reduce_only/new_order)
- [ ] enqueue_new_order() / enqueue_reduce_only() / enqueue_cancel()
- [ ] tick() で優先順位に従った収集（inflight increment しない）
- [ ] inflight >= 100 では何も返さない（cancel も含む）
- [ ] on_batch_sent() で inflight increment
- [ ] requeue_reduce_only() for 再キュー
- [ ] on_disconnect() は BatchScheduler に実装しない（ExecutorLoop で一元管理）
- [ ] ユニットテスト（10項目）

### 3.2 Week 1-2: 署名・発注

#### Signer実装

##### 設計方針

| 項目 | 方針 |
|------|------|
| **責務** | Action の署名のみ（post_id は署名対象外、WsSender層で付与） |
| **API** | `sign_action(&Action, nonce, vault_address, expires_after) -> Signature` |
| **鍵供給** | 環境変数 or config file から読み込み |
| **API wallet** | Observation用 / Trading用を分離（Trading用のみ署名に使用） |
| **検証** | address は秘密鍵から導出、不一致なら起動失敗 |
| **秘匿** | ログに秘密鍵/署名を出さない、`zeroize` でメモリクリア |

##### 鍵管理（KeyManager）

```rust
use alloy::signers::local::PrivateKeySigner;
use zeroize::Zeroizing;

/// 鍵の供給元
///
/// **フォーマット（EnvVar / File 共通）**:
/// - hex 文字列（64文字、32 bytes）
/// - `0x` prefix あり/なし両対応
/// - 前後の空白・改行は自動 trim
/// - 例: `0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80`
///
/// 32 bytes 生バイナリは **非対応**（誤ってログに混入するリスク回避）
pub enum KeySource {
    /// 環境変数から読み込み（開発用）
    EnvVar { var_name: String },
    /// ファイルから読み込み（本番用、パーミッション 0600 推奨）
    File { path: PathBuf },
    // 将来: HSM, AWS KMS, etc.
}

/// API wallet 種別
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WalletRole {
    Observation,  // 読み取り専用（WS購読、REST query）
    Trading,      // 取引用（署名必要）
}

pub struct KeyManager {
    trading_signer: Option<PrivateKeySigner>,
    observation_address: Address,
    trading_address: Option<Address>,
}

impl KeyManager {
    /// 鍵をロードし、address との一致を検証
    pub fn load(
        trading_source: Option<KeySource>,
        expected_trading_address: Option<Address>,
    ) -> Result<Self, KeyError> {
        let trading_signer = if let Some(source) = trading_source {
            // 秘密鍵を Zeroizing でラップして、スコープ外で自動クリア
            // hex 文字列からバイト列へ変換（共通ロジック）
            fn parse_hex_key(hex_str: &str) -> Result<Zeroizing<Vec<u8>>, KeyError> {
                let trimmed = hex_str.trim().trim_start_matches("0x");
                Ok(Zeroizing::new(hex::decode(trimmed)?))
            }

            let secret_bytes: Zeroizing<Vec<u8>> = match source {
                KeySource::EnvVar { var_name } => {
                    let hex = std::env::var(&var_name)
                        .map_err(|_| KeyError::EnvVarNotFound(var_name.clone()))?;
                    parse_hex_key(&hex)?
                }
                KeySource::File { path } => {
                    let content = std::fs::read_to_string(&path)?;
                    parse_hex_key(&content)?
                }
            };

            let signer = PrivateKeySigner::from_slice(&secret_bytes)?;

            // address 一致検証
            if let Some(expected) = expected_trading_address {
                if signer.address() != expected {
                    return Err(KeyError::AddressMismatch {
                        expected,
                        actual: signer.address(),
                    });
                }
            }

            Some(signer)
        } else {
            None
        };

        Ok(Self {
            trading_address: trading_signer.as_ref().map(|s| s.address()),
            trading_signer,
            observation_address: Address::ZERO, // TODO: 別途設定
        })
    }

    /// バイト列から直接ロード（テスト用、環境変数非依存）
    #[cfg(test)]
    pub fn from_bytes(
        secret_bytes: &[u8],
        expected_address: Option<Address>,
    ) -> Result<Self, KeyError> {
        let signer = PrivateKeySigner::from_slice(secret_bytes)?;

        // address 一致検証
        if let Some(expected) = expected_address {
            if signer.address() != expected {
                return Err(KeyError::AddressMismatch {
                    expected,
                    actual: signer.address(),
                });
            }
        }

        Ok(Self {
            trading_address: Some(signer.address()),
            trading_signer: Some(signer),
            observation_address: Address::ZERO,
        })
    }

    pub fn trading_signer(&self) -> Option<&PrivateKeySigner> {
        self.trading_signer.as_ref()
    }

    pub fn trading_address(&self) -> Option<Address> {
        self.trading_address
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("Environment variable not found: {0}")]
    EnvVarNotFound(String),
    #[error("Failed to decode hex: {0}")]
    HexDecode(#[from] hex::FromHexError),
    #[error("Invalid private key: {0}")]
    InvalidKey(#[from] alloy::signers::Error),
    #[error("Address mismatch: expected {expected}, got {actual}")]
    AddressMismatch { expected: Address, actual: Address },
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

##### 署名仕様（Hyperliquid L1 Action）

**重要**: Hyperliquid の署名仕様は公式 Python SDK (`hyperliquid-python-sdk/hyperliquid/utils/signing.py`) に**厳密に準拠**する。
SDK は **2段階の署名方式**（action_hash 計算 → phantom_agent EIP-712 署名）を採用している。

**署名対象データ構造**:

```rust
/// L1 Action（署名対象の一部）
/// ref: hyperliquid-python-sdk/hyperliquid/utils/signing.py - order_wires_to_order_action()
///
/// ⚠️ msgpack互換性: Option<T> フィールドは `skip_serializing_if` 必須。
/// Python SDK は存在しないキーを省略するが、serde のデフォルトは
/// `None` を `nil` としてシリアライズするため、hash が不一致になる。
#[derive(Debug, Clone, Serialize)]
pub struct Action {
    /// アクションタイプ: "order", "cancel", "batchModify", etc.
    #[serde(rename = "type")]
    pub action_type: String,

    /// orders が None の場合、キー自体を省略（Python SDK 互換）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orders: Option<Vec<OrderWire>>,

    /// cancels が None の場合、キー自体を省略（Python SDK 互換）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancels: Option<Vec<CancelWire>>,

    /// 注文グルーピング（type=order 時は必須）
    /// SDK: order_wires_to_order_action() で "na" を設定
    /// "na" = not applicable（単発注文）, "normalTpsl" = TP/SL連動, etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grouping: Option<String>,

    /// ビルダー情報（省略可）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builder: Option<BuilderInfo>,
    // 他のフィールドは action_type に依存（追加時も skip_serializing_if 必須）
}

/// ビルダー情報（オプション）
#[derive(Debug, Clone, Serialize)]
pub struct BuilderInfo {
    #[serde(rename = "b")]
    pub address: String,
    #[serde(rename = "f")]
    pub fee: u64,
}

/// 注文ワイヤ形式（SDK の order_spec_to_order_wire に対応）
/// ref: hyperliquid-python-sdk/hyperliquid/utils/types.py - OrderWire
#[derive(Debug, Clone, Serialize)]
pub struct OrderWire {
    #[serde(rename = "a")]
    pub asset: u32,
    #[serde(rename = "b")]
    pub is_buy: bool,
    #[serde(rename = "p")]
    pub limit_px: String,
    #[serde(rename = "s")]
    pub sz: String,
    #[serde(rename = "r")]
    pub reduce_only: bool,
    #[serde(rename = "t")]
    pub order_type: OrderTypeWire,
    #[serde(rename = "c", skip_serializing_if = "Option::is_none")]
    pub cloid: Option<String>,
}

/// 注文タイプのワイヤ形式（SDK の order type wire に対応）
/// ref: hyperliquid-python-sdk/hyperliquid/exchange.py - order()
///
/// SDK例:
/// - Limit IOC: {"limit": {"tif": "Ioc"}}
/// - Limit GTC: {"limit": {"tif": "Gtc"}}
/// - Limit ALO: {"limit": {"tif": "Alo"}}
/// - Trigger: {"trigger": {"triggerPx": "...", "isMarket": true, "tpsl": "tp"}}
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum OrderTypeWire {
    /// Limit order: {"limit": {"tif": "Gtc"|"Ioc"|"Alo"}}
    Limit { limit: LimitOrderType },
    /// Trigger order: {"trigger": {...}}
    Trigger { trigger: TriggerOrderType },
}

impl OrderTypeWire {
    /// IOC (Immediate or Cancel) 注文
    pub fn ioc() -> Self {
        Self::Limit {
            limit: LimitOrderType { tif: "Ioc".to_string() },
        }
    }

    /// GTC (Good Till Cancel) 注文
    pub fn gtc() -> Self {
        Self::Limit {
            limit: LimitOrderType { tif: "Gtc".to_string() },
        }
    }

    /// ALO (Add Liquidity Only) 注文
    pub fn alo() -> Self {
        Self::Limit {
            limit: LimitOrderType { tif: "Alo".to_string() },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LimitOrderType {
    pub tif: String, // "Gtc", "Ioc", "Alo"
}

/// **注意: Phase B スコープ外（未対応）**
///
/// Trigger order は Phase B では使用しない（IOC のみ使用）。
/// 将来対応する場合は、フィールド順を SDK に合わせること:
/// SDK 順序: isMarket → triggerPx → tpsl
/// （現状のフィールド順だと msgpack の map 順がズレて action_hash が不一致になる）
#[derive(Debug, Clone, Serialize)]
pub struct TriggerOrderType {
    // SDK 順序に合わせたフィールド定義
    #[serde(rename = "isMarket")]
    pub is_market: bool,
    #[serde(rename = "triggerPx")]
    pub trigger_px: String,
    pub tpsl: String, // "tp" or "sl"
}

/// キャンセルワイヤ形式（SDK の cancel wire に対応）
/// ref: hyperliquid-python-sdk/hyperliquid/exchange.py - cancel() / bulk_cancel()
///
/// SDK例: {"a": 5, "o": 123456789}
#[derive(Debug, Clone, Serialize)]
pub struct CancelWire {
    #[serde(rename = "a")]
    pub asset: u32,
    #[serde(rename = "o")]
    pub oid: u64, // exchange order ID
}

/// 署名入力パラメータ
#[derive(Debug, Clone)]
pub struct SigningInput {
    pub action: Action,
    pub nonce: u64,
    pub vault_address: Option<Address>,  // None = 通常取引, Some = vault経由
    pub expires_after: Option<u64>,      // 署名有効期限（オプション）
}
```

**署名手順（SDK 2段階方式）**:

SDK の `sign_l1_action()` は以下の2段階で署名を生成する:

1. **action_hash の計算** (`action_hash()` 関数)
2. **phantom_agent の EIP-712 署名** (`sign_inner()` 関数)

```rust
use alloy::primitives::{keccak256, B256, Address, U256};
use alloy::signers::Signer as AlloySigner;
use alloy::sol_types::{eip712_domain, SolStruct};

/// ===== Step 1: action_hash の計算 =====
///
/// SDK signing.py の action_hash() に準拠:
/// ```python
/// def action_hash(action, vault_address, nonce, expires_after=None):
///     data = msgpack.packb(action) + nonce.to_bytes(8, "big") + (b"\x00" if vault_address is None else b"\x01" + bytes.fromhex(vault_address[2:]))
///     if expires_after is not None:
///         data += b"\x00" + expires_after.to_bytes(8, "big")
///     # else: 何も追加しない（タグ自体が存在しない）
///     return keccak256(data)
/// ```

impl SigningInput {
    /// action_hash を計算（SDK action_hash() 関数に準拠）
    pub fn action_hash(&self) -> B256 {
        let mut data = Vec::new();

        // 1. Action を msgpack でシリアライズ
        //    ⚠️ rmp_serde::to_vec_named を使用（キー名付き map 形式）
        //    Python SDK: msgpack.packb(action)
        let action_bytes = rmp_serde::to_vec_named(&self.action)
            .expect("Action serialization should not fail");
        data.extend_from_slice(&action_bytes);

        // 2. nonce を **big-endian** 8 bytes に変換
        //    ⚠️ Python SDK: nonce.to_bytes(8, "big")
        data.extend_from_slice(&self.nonce.to_be_bytes());

        // 3. vault_address タグ
        //    None の場合: 0x00 (1 byte)
        //    Some の場合: 0x01 + address (21 bytes)
        //    ⚠️ None でも 0x00 の 1 byte が必ず入る
        match &self.vault_address {
            None => data.push(0x00),
            Some(addr) => {
                data.push(0x01);
                data.extend_from_slice(addr.as_slice());
            }
        }

        // 4. expires_after タグ（SDK 準拠）
        //    None の場合: 何も追加しない（タグ自体が存在しない）
        //    Some の場合: 0x00 + expires_after (9 bytes total)
        //    ⚠️ SDK と異なり vault_address と挙動が違う点に注意
        if let Some(expires) = self.expires_after {
            data.push(0x00);
            data.extend_from_slice(&expires.to_be_bytes());
        }
        // None の場合は何も追加しない

        keccak256(&data)
    }
}

/// ===== Step 2: phantom_agent EIP-712 署名 =====
///
/// SDK signing.py の construct_phantom_agent() と sign_inner() に準拠:
/// - phantom_agent = {"source": source, "connectionId": action_hash}
/// - source = "a" (mainnet) or "b" (testnet)
/// - EIP-712 domain: {name: "Exchange", version: "1", chainId: 1337, verifyingContract: 0x0}
/// - primaryType: "Agent"

/// Phantom Agent 構造（EIP-712 署名対象）
#[derive(Debug, Clone)]
pub struct PhantomAgent {
    pub source: String,         // "a" (mainnet) or "b" (testnet)
    pub connection_id: B256,    // action_hash の結果
}

/// EIP-712 ドメイン定数
pub const EIP712_DOMAIN_NAME: &str = "Exchange";
pub const EIP712_DOMAIN_VERSION: &str = "1";
pub const EIP712_CHAIN_ID: u64 = 1337;
pub const EIP712_VERIFYING_CONTRACT: Address = Address::ZERO;

/// EIP-712 型定義（alloy sol! マクロで定義）
sol! {
    #[derive(Debug)]
    struct Agent {
        string source;
        bytes32 connectionId;
    }
}

impl PhantomAgent {
    pub fn new(action_hash: B256, is_mainnet: bool) -> Self {
        Self {
            source: if is_mainnet { "a".to_string() } else { "b".to_string() },
            connection_id: action_hash,
        }
    }

    /// EIP-712 TypedData のハッシュを計算し署名
    pub async fn sign<S: AlloySigner>(
        &self,
        signer: &S,
    ) -> Result<alloy::primitives::Signature, alloy::signers::Error> {
        let domain = eip712_domain! {
            name: EIP712_DOMAIN_NAME,
            version: EIP712_DOMAIN_VERSION,
            chain_id: EIP712_CHAIN_ID,
            verifying_contract: EIP712_VERIFYING_CONTRACT,
        };

        let agent = Agent {
            source: self.source.clone(),
            connectionId: self.connection_id,
        };

        // EIP-712 signing_hash = keccak256(0x1901 || domain_separator || struct_hash)
        let signing_hash = agent.eip712_signing_hash(&domain);

        signer.sign_hash(&signing_hash).await
    }
}
```

**署名フロー（まとめ）**:

```
1. Action を構築（type, orders, grouping, etc.）
2. SigningInput を作成（action, nonce, vault_address, expires_after）
3. action_hash = SigningInput.action_hash()
   └─ keccak256(msgpack(action) || nonce_be || vault_tag || expires_tag)
4. PhantomAgent を作成（source="a"/"b", connectionId=action_hash）
5. EIP-712 署名 = PhantomAgent.sign(signer)
   └─ domain: {name:"Exchange", version:"1", chainId:1337, verifyingContract:0x0}
   └─ primaryType: "Agent"
6. 署名を {r, s, v} 形式で返却
```

**重要な仕様ポイント**:

| 項目 | SDK 仕様 | 注意点 |
|------|----------|--------|
| nonce エンディアン | **big-endian** | `to_be_bytes()` を使用 |
| vault_address=None | **0x00 タグ 1 byte** | 空ではなく 0x00 が入る |
| expires_after=None | **何も追加しない** | タグ自体が存在しない（vault_address と挙動が異なる） |
| expires_after=Some | **0x00 + 8 bytes** | 0x00 タグ + big-endian 8 bytes |
| source | "a" (mainnet) / "b" (testnet) | 静的定数ではない |
| EIP-712 chainId | **1337** | Mainnet/Testnet 共通 |
| grouping (type=order) | **必須** ("na" など) | 省略不可 |
```

##### Signer 構造体

```rust
pub struct Signer {
    key_manager: Arc<KeyManager>,
    is_mainnet: bool,
}

impl Signer {
    pub fn new(key_manager: Arc<KeyManager>, is_mainnet: bool) -> Result<Self, SignerError> {
        // Trading 鍵が存在することを確認
        if key_manager.trading_signer().is_none() {
            return Err(SignerError::NoTradingKey);
        }
        Ok(Self {
            key_manager,
            is_mainnet,
        })
    }

    /// Action に署名（SDK 2段階方式: action_hash → phantom_agent EIP-712）
    ///
    /// 注意: post_id は WS 層の相関 ID であり、署名対象ではない。
    /// ExecutorLoop が post_id を付与し、WsSender が JSON に含める。
    pub async fn sign_action(
        &self,
        action: &Action,
        nonce: u64,
        vault_address: Option<Address>,
        expires_after: Option<u64>,
    ) -> Result<Signature, SignerError> {
        let signer = self.key_manager.trading_signer()
            .ok_or(SignerError::NoTradingKey)?;

        // Step 1: action_hash を計算
        let input = SigningInput {
            action: action.clone(),
            nonce,
            vault_address,
            expires_after,
        };
        let action_hash = input.action_hash();

        // Step 2: phantom_agent を作成して EIP-712 署名
        let phantom_agent = PhantomAgent::new(action_hash, self.is_mainnet);

        // 注意: 署名は秘密情報を含むのでログに出さない
        let signature = phantom_agent.sign(signer).await?;

        Ok(signature)
    }

    /// ActionBatch から Action を構築して署名
    ///
    /// **SDK 仕様準拠**: orders と cancels は別々の action として送信
    /// - OrderBatch → `{"type": "order", "orders": [...], "grouping": "na"}`
    /// - CancelBatch → `{"type": "cancel", "cancels": [...]}`
    ///
    /// **注意**: `market_specs` は `PendingOrder::to_wire()` で価格/サイズの文字列化に使用
    pub async fn build_and_sign(
        &self,
        batch: &ActionBatch,
        nonce: u64,
        market_specs: &DashMap<MarketKey, MarketSpec>,
    ) -> Result<SignedAction, SignerError> {
        // ActionBatch -> Action 変換
        let action = match batch {
            ActionBatch::Orders(orders) => {
                // PendingOrder → OrderWire（各注文の market から spec を取得）
                let wires: Vec<OrderWire> = orders
                    .iter()
                    .map(|o| {
                        let spec = market_specs.get(&o.market)
                            .expect("MarketSpec not found for market");
                        o.to_wire(spec.value())
                    })
                    .collect();

                Action {
                    action_type: "order".to_string(),
                    orders: Some(wires),
                    cancels: None,
                    grouping: Some("na".to_string()), // type=order 時は必須
                    builder: None,
                }
            },
            ActionBatch::Cancels(cancels) => Action {
                action_type: "cancel".to_string(),
                orders: None,
                cancels: Some(cancels.iter().map(|c| c.to_wire()).collect()),
                grouping: None, // type=cancel 時は不要
                builder: None,
            },
        };

        let signature = self.sign_action(
            &action,
            nonce,
            None,  // vault_address: 通常取引
            None,  // expires_after: 有効期限なし
        ).await?;

        Ok(SignedAction {
            action,
            nonce,
            signature,
        })
    }

    pub fn trading_address(&self) -> Option<Address> {
        self.key_manager.trading_address()
    }
}

/// 署名済み Action（内部表現）
#[derive(Debug, Clone)]
pub struct SignedAction {
    pub action: Action,
    pub nonce: u64,
    pub signature: Signature,  // v は 0/1（alloy 内部表現）
    // post_id はここには含まない（WsSender が付与）
}

/// WS wire payload（SDK `_post_action()` 準拠）
///
/// WS `post` リクエストの `request.payload` として送信する形式。
/// SDK の `_post_action(action, signature, nonce, vault_address, expires_after)` を参照。
///
/// **重要: `v` の変換**
/// - Rust 内部（alloy）: 0 or 1
/// - WS wire（SDK準拠）: 27 or 28
/// → SignedAction → ActionWirePayload 変換時に `v + 27` を行う
///
/// 変換責務: **WsSender 層**（Signer は内部表現のまま返す）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionWirePayload {
    pub action: Action,
    pub nonce: u64,
    pub signature: SignatureWire,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault_address: Option<Address>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_after: Option<u64>,
}

/// 署名の wire 表現（SDK準拠）
#[derive(Debug, Clone, Serialize)]
pub struct SignatureWire {
    pub r: String,  // "0x..." (64 hex chars, 32 bytes)
    pub s: String,  // "0x..." (64 hex chars, 32 bytes)
    pub v: u8,      // 27 or 28（SDK準拠、alloy の 0/1 から +27）
}

impl SignedAction {
    /// WS wire payload に変換（v: 0/1 → 27/28）
    pub fn to_wire_payload(
        &self,
        vault_address: Option<Address>,
        expires_after: Option<u64>,
    ) -> ActionWirePayload {
        ActionWirePayload {
            action: self.action.clone(),
            nonce: self.nonce,
            signature: SignatureWire {
                r: format!("0x{}", hex::encode(self.signature.r().to_be_bytes::<32>())),
                s: format!("0x{}", hex::encode(self.signature.s().to_be_bytes::<32>())),
                v: self.signature.v().to_u64() as u8 + 27,  // 0/1 → 27/28
            },
            vault_address,
            expires_after,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SignerError {
    #[error("No trading key configured")]
    NoTradingKey,
    #[error("Signing failed: {0}")]
    SigningFailed(#[from] alloy::signers::Error),
}
```

##### Golden Test（オフライン署名検証）

**テストベクトル（環境非依存、固定値）**:

> **重要**: Golden test の期待値（action_hash / signature）は **実装前に Python SDK で計算** し、
> Rust コードに**リテラルとして埋め込む**。計画段階ではプレースホルダー値を記載しているが、
> 実装時に必ず以下のスクリプトを実行して実値に置き換えること。

**実装時フロー**:
1. 下記 Python スクリプトを実行
2. 出力された `expected_action_hash` と `expected_signature` をコピー
3. Rust テストコードのプレースホルダーを実値に置き換え
4. `cargo test` で検証

```python
# 期待値計算スクリプト（実装前に必ず実行）
# ファイル名: scripts/generate_golden_test_vectors.py
#
# SDK API: sign_l1_action(wallet, action, active_pool, nonce, expires_after, is_mainnet)
# 戻り値: {"r": hex, "s": hex, "v": int} (return_hash オプションは存在しない)

from hyperliquid.utils.signing import (
    sign_l1_action,
    action_hash,  # action_hash を別途呼び出して期待 hash を取得
)
from eth_account import Account
import msgpack

# テスト用固定秘密鍵（Foundry/Hardhat デフォルト #0）
# これは公開されたテスト用鍵であり、本番では使用しない
TEST_PRIVATE_KEY = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
EXPECTED_ADDRESS = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"

# 固定入力（Rust テストと完全一致させる）
# ⚠️ type=order の場合、grouping は必須
action = {
    "type": "order",
    "orders": [{
        "a": 5,
        "b": True,
        "p": "100.0",
        "s": "0.1",
        "r": False,
        "t": {"limit": {"tif": "Ioc"}},
        "c": "test-cloid-001",
    }],
    "grouping": "na",  # 必須: "na" = not applicable
}
nonce = 1705000000000
vault_address = None  # active_pool とも呼ばれる
expires_after = None
is_mainnet = False

wallet = Account.from_key(TEST_PRIVATE_KEY)

# Step 1: action_hash を計算（SDK の action_hash 関数を使用）
expected_action_hash = action_hash(action, vault_address, nonce, expires_after)

# Step 2: 署名を取得（SDK の sign_l1_action を使用）
# 戻り値は {"r": hex, "s": hex, "v": int}
sig_result = sign_l1_action(wallet, action, vault_address, nonce, expires_after, is_mainnet)

# 署名を 65 bytes hex に変換 (r[32] || s[32] || v[1])
# ⚠️ r/s は先頭ゼロが省略され得るため、必ず 32 bytes にパディング
r = int(sig_result["r"], 16).to_bytes(32, "big")
s = int(sig_result["s"], 16).to_bytes(32, "big")

# v の表現（SDK は 27/28、alloy は 0/1 を使用）
# alloy::primitives::Signature は recovery_id (0/1) を期待するため変換
v_raw = sig_result["v"]
v = (v_raw - 27).to_bytes(1, "big") if v_raw >= 27 else v_raw.to_bytes(1, "big")
signature_bytes = r + s + v

print("=== Golden Test Vectors ===")
print(f"Address: {wallet.address}")
print(f"Expected action_hash: 0x{expected_action_hash.hex()}")
print(f"Expected signature (r||s||v): {signature_bytes.hex()}")
print()
print("// Rust テストコードにコピー:")
print(f'let expected_action_hash: B256 = "0x{expected_action_hash.hex()}".parse().unwrap();')
print(f'let expected_sig_hex = "{signature_bytes.hex()}";')
```

> **注意**: SDK の `action_hash()` 関数が import できない場合は、SDK ソースから
> `action_hash()` の実装をコピーして使用すること。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用固定秘密鍵（Foundry/Hardhat デフォルト #0）
    /// 本番環境では絶対に使用しないこと
    const TEST_PRIVATE_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const TEST_ADDRESS: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

    /// テスト用 KeyManager を作成（環境変数非依存）
    fn test_key_manager() -> KeyManager {
        let secret_bytes = hex::decode(TEST_PRIVATE_KEY).unwrap();
        KeyManager::from_bytes(
            &secret_bytes,
            Some(TEST_ADDRESS.parse().unwrap()),
        ).unwrap()
    }

    /// テスト用固定 Action を作成
    fn test_action() -> Action {
        Action {
            action_type: "order".to_string(),
            orders: Some(vec![OrderWire {
                asset: 5,
                is_buy: true,
                limit_px: "100.0".to_string(),
                sz: "0.1".to_string(),
                reduce_only: false,
                order_type: OrderTypeWire::ioc(), // {"limit": {"tif": "Ioc"}}
                cloid: Some("test-cloid-001".to_string()),
            }]),
            cancels: None,
            grouping: Some("na".to_string()),  // 必須
            builder: None,
        }
    }

    /// Golden test: action_hash 計算の検証
    #[test]
    fn test_action_hash_golden_vector() {
        let input = SigningInput {
            action: test_action(),
            nonce: 1705000000000,
            vault_address: None,
            expires_after: None,
        };

        let hash = input.action_hash();

        // 期待される action_hash（Python SDK で事前計算した実値）
        // ⚠️ 実装時: scripts/generate_golden_test_vectors.py を実行し、
        //    出力された値でこのプレースホルダーを置き換えること
        let expected_action_hash: B256 = "0x_REPLACE_WITH_PYTHON_SDK_OUTPUT_"
            .parse()
            .expect("Invalid hash - run Python script to get actual value");

        assert_eq!(hash, expected_action_hash, "action_hash mismatch - SDK互換性を確認せよ");
    }

    /// Golden test: 署名の検証（2段階: action_hash → EIP-712）
    ///
    /// 署名形式: r[32] || s[32] || v[1] = 65 bytes
    /// - r, s: 署名の楕円曲線成分（各32 bytes、big-endian）
    /// - v: recovery id（alloy は 0/1 を使用、SDK/Ethereum は 27/28 を使用）
    ///
    /// Python SDK → Rust 変換時の注意:
    /// - SDK の v が 27/28 の場合、27 を引いて 0/1 に変換
    /// - alloy::primitives::Signature は recovery_id (0/1) を期待
    #[tokio::test]
    async fn test_signature_golden_vector() {
        let key_manager = Arc::new(test_key_manager());
        let signer = Signer::new(key_manager, /* is_mainnet */ false).unwrap();

        let action = test_action();
        let signature = signer.sign_action(
            &action,
            1705000000000,    // nonce
            None,             // vault_address
            None,             // expires_after
        ).await.unwrap();

        // 期待される署名（Python SDK で事前計算した実値、65 bytes = 130 hex chars）
        // ⚠️ 実装時: scripts/generate_golden_test_vectors.py を実行し、
        //    出力された値でこのプレースホルダーを置き換えること
        // ⚠️ v は 0 or 1 に変換済みであること（27/28 ではない）
        let expected_sig_hex = "_REPLACE_WITH_PYTHON_SDK_OUTPUT_130_HEX_CHARS_";

        assert_eq!(
            hex::encode(signature.as_bytes()),
            expected_sig_hex,
            "Signature mismatch - SDK互換性を確認せよ"
        );
    }

    /// アドレス導出の検証
    #[test]
    fn test_address_derivation() {
        let key_manager = test_key_manager();
        let expected: Address = TEST_ADDRESS.parse().unwrap();

        assert_eq!(key_manager.trading_address(), Some(expected));
    }

    /// アドレス不一致でエラー
    #[test]
    fn test_address_mismatch_fails() {
        let secret_bytes = hex::decode(TEST_PRIVATE_KEY).unwrap();
        let wrong_address: Address = "0x0000000000000000000000000000000000000001".parse().unwrap();

        let result = KeyManager::from_bytes(&secret_bytes, Some(wrong_address));

        assert!(matches!(result, Err(KeyError::AddressMismatch { .. })));
    }

    /// post_id が署名対象に含まれないことを検証
    #[test]
    fn test_post_id_not_in_signature() {
        // SigningInput に post_id フィールドがないことを型レベルで保証
        // post_id は WsSender 層で付与される
        let input = SigningInput {
            action: Action {
                action_type: "order".to_string(),
                orders: None,
                cancels: None,
                grouping: Some("na".to_string()),
                builder: None,
            },
            nonce: 123,
            vault_address: None,
            expires_after: None,
            // post_id: 789,      // コンパイルエラー: フィールドが存在しない
            // timestamp_ms: 456, // コンパイルエラー: フィールドが存在しない（SDK準拠）
        };

        // action_hash 計算は post_id に影響されない
        let _hash = input.action_hash();
    }

    /// nonce エンディアンの検証（big-endian）
    #[test]
    fn test_nonce_big_endian() {
        let input = SigningInput {
            action: test_action(),
            nonce: 0x0102030405060708,  // 明確な値でエンディアンを確認
            vault_address: None,
            expires_after: None,
        };

        // action_hash の計算（内部で nonce.to_be_bytes() が使われる）
        let hash = input.action_hash();

        // 同じ入力で常に同じ hash が出ることを確認
        let hash2 = input.action_hash();
        assert_eq!(hash, hash2);
    }

    /// vault_address=None でも 0x00 タグが入ることを検証
    #[test]
    fn test_vault_address_none_has_tag() {
        let input_none = SigningInput {
            action: test_action(),
            nonce: 123,
            vault_address: None,
            expires_after: None,
        };

        let input_some = SigningInput {
            action: test_action(),
            nonce: 123,
            vault_address: Some(Address::ZERO),
            expires_after: None,
        };

        // None と Some(0x0) で hash が異なることを確認
        // （None は 0x00 タグ 1byte、Some は 0x01 + 20bytes）
        assert_ne!(input_none.action_hash(), input_some.action_hash());
    }
}
```

##### Signer テスト項目

| # | テスト | 期待動作 |
|---|--------|----------|
| 1 | Golden test (action_hash) | 固定入力に対して Python SDK `action_hash()` と同一ハッシュ |
| 2 | Golden test (signature) | 固定入力に対して Python SDK `sign_l1_action()` と同一署名 |
| 3 | Address 導出 | 秘密鍵からアドレスが正しく導出される |
| 4 | Address 不一致 | 不一致時に `KeyError::AddressMismatch` |
| 5 | 鍵なしエラー | Trading 鍵なしで `SignerError::NoTradingKey` |
| 6 | ActionBatch→Action 変換 | ActionBatch が正しく Action に変換される（orders→grouping="na"、cancels→grouping 省略） |
| 7 | msgpack シリアライズ | Action が SDK 互換の msgpack 形式に（Option::None はキー省略） |
| 8 | nonce big-endian | nonce が big-endian でエンコードされる |
| 9 | vault_address=None タグ | None でも 0x00 タグ 1 byte が入る |
| 10 | post_id 非含有 | post_id は署名対象に含まれない（型で保証） |
| 11 | EIP-712 domain | chainId=1337, name="Exchange", version="1" |
| 12 | phantom_agent source | mainnet="a", testnet="b" |

**タスク**:
- [ ] Python SDK (hyperliquid-python-sdk) の `signing.py` から署名仕様を最終確認
- [ ] **P0**: `scripts/generate_golden_test_vectors.py` を作成・実行し、期待値を取得
- [ ] **P0**: Golden test の `_REPLACE_WITH_...` プレースホルダーを実値に置き換え
- [ ] KeyManager 実装（鍵ロード、address 検証、zeroize）
- [ ] KeySource enum（EnvVar / File）
- [ ] Action 構造体定義（`grouping` 必須、`skip_serializing_if` 徹底、msgpack 互換）
- [ ] OrderWire / CancelWire 構造体定義（SDK wire format 準拠）
- [ ] SigningInput と action_hash() 実装（nonce big-endian、vault_tag、expires_tag）
- [ ] PhantomAgent と EIP-712 署名実装（alloy sol! マクロ使用）
- [ ] Signer::sign_action() 実装（2段階: action_hash → phantom_agent EIP-712）
- [ ] Signer::build_and_sign() 実装（ActionBatch → Action 変換含む）
- [ ] Golden test（action_hash / signature）作成（環境非依存）
- [ ] 秘密鍵誤出力防止（Debug trait の impl 確認）
- [ ] ユニットテスト（12項目）
- [ ] Testnet で実際の署名が受理されることを確認

#### OrderBuilder実装

##### 型の責務分離

| 型 | 責務 | 生成場所 |
|----|------|----------|
| `PendingOrder` | 内部表現（キュー保持用） | OrderBuilder |
| `OrderWire` | WS送信用 wire format | `PendingOrder::to_wire()` |
| `ClientOrderId` | 注文ID（idempotency保証） | OrderBuilder（生成時に付与） |

##### PendingOrder（内部表現）

```rust
use hip3_core::{ClientOrderId, MarketKey, MarketSpec, Price, Size};

/// 内部注文表現（キュー保持用）
///
/// **cloid の idempotency 保証**:
/// - `cloid` は OrderBuilder で生成時に付与
/// - 再キュー/再送でも **同一の cloid を使用**（重複注文防止）
/// - `post_id` は WS 相関ID であり、cloid とは別物（再送のたびに変わる）
#[derive(Debug, Clone)]
pub struct PendingOrder {
    pub cloid: ClientOrderId,       // 注文ID（idempotency キー）
    pub market: MarketKey,
    pub side: OrderSide,            // Buy/Sell（TrackedOrder と統一）
    pub price: Price,               // 内部表現（Decimal）
    pub size: Size,                 // 内部表現（Decimal）
    pub reduce_only: bool,
    pub tif: TimeInForce,           // IOC / GTC
    pub created_at: Instant,        // タイムアウト計算用
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeInForce {
    Ioc,  // Immediate-or-Cancel
    Gtc,  // Good-til-Cancel
}

impl PendingOrder {
    /// OrderWire に変換（送信直前に MarketSpec を使って文字列化）
    pub fn to_wire(&self, spec: &MarketSpec) -> OrderWire {
        let is_buy = matches!(self.side, OrderSide::Buy);
        OrderWire {
            asset: self.market.asset.0 as u32,
            is_buy,
            // format_price は is_buy で丸め方向を決定
            // - Buy: ceil（不利な方向に丸め）
            // - Sell: floor（不利な方向に丸め）
            limit_px: spec.format_price(self.price, is_buy),
            sz: spec.format_size(self.size),
            reduce_only: self.reduce_only,
            order_type: self.tif.to_wire(),
            cloid: Some(self.cloid.to_string()),
        }
    }
}

impl TimeInForce {
    /// OrderTypeWire に変換（SDK wire format）
    /// `ioc()` → `{"limit": {"tif": "Ioc"}}`
    /// `gtc()` → `{"limit": {"tif": "Gtc"}}`
    pub fn to_wire(&self) -> OrderTypeWire {
        match self {
            TimeInForce::Ioc => OrderTypeWire::ioc(),
            TimeInForce::Gtc => OrderTypeWire::gtc(),
        }
    }
}
```

##### OrderBuilder

```rust
pub struct OrderBuilder {
    market: MarketKey,
    spec: MarketSpec,  // format_price/format_size 用
}

impl OrderBuilder {
    pub fn new(market: MarketKey, spec: MarketSpec) -> Self {
        Self { market, spec }
    }

    /// IOC注文を構築
    ///
    /// - `cloid` は呼び出し元で生成（通常は `ClientOrderId::new()`）
    /// - `price`/`size` は内部表現（Decimal）で保持
    /// - 文字列化は `to_wire()` で行う（送信直前）
    pub fn build_ioc(
        &self,
        side: OrderSide,
        price: Price,
        size: Size,
        reduce_only: bool,
        cloid: ClientOrderId,
    ) -> PendingOrder {
        PendingOrder {
            cloid,
            market: self.market.clone(),
            side,  // OrderSide をそのまま保持（to_wire で is_buy に変換）
            price,
            size,
            reduce_only,
            tif: TimeInForce::Ioc,
            created_at: Instant::now(),
        }
    }

    /// GTC注文を構築（TimeStop用など）
    pub fn build_gtc(
        &self,
        side: OrderSide,
        price: Price,
        size: Size,
        reduce_only: bool,
        cloid: ClientOrderId,
    ) -> PendingOrder {
        PendingOrder {
            cloid,
            market: self.market.clone(),
            side,  // OrderSide をそのまま保持
            price,
            size,
            reduce_only,
            tif: TimeInForce::Gtc,
            created_at: Instant::now(),
        }
    }

    /// MarketSpec への参照を取得（to_wire 用）
    pub fn spec(&self) -> &MarketSpec {
        &self.spec
    }
}
```

##### ActionBatch → Action 変換

**`Signer::build_and_sign()` で実行**（3.2 Signer 節参照）:
- `PendingOrder::to_wire(spec)` で `OrderWire` に変換
- `PendingCancel::to_wire()` で `CancelWire` に変換
- `market_specs: &DashMap<MarketKey, MarketSpec>` から各注文の spec を取得

##### cloid 生成規約（idempotency）

| 項目 | 規約 |
|------|------|
| **生成タイミング** | OrderBuilder 呼び出し時（シグナル処理時） |
| **生成方法** | `ClientOrderId::new()`（UUID v4 ベース） |
| **保持場所** | `PendingOrder.cloid` |
| **再キュー時** | **同一の cloid を維持**（新規生成しない） |
| **再送時** | **同一の cloid を維持**（post_id は変わるが cloid は変わらない） |
| **post_id との関係** | 独立（post_id は WS 相関ID、cloid は注文ID） |

**重要**: `cloid` を `post_id` から生成してはならない。`post_id` は再送のたびに変わるため、同一注文を重複発注してしまう。

**タスク**:
- [ ] PendingOrder 構造体定義（cloid: ClientOrderId、price: Price、size: Size）
- [ ] `PendingOrder::to_wire()` 実装（MarketSpec 参照、format_price に is_buy を渡す）
- [ ] TimeInForce enum と `to_wire()` 実装
- [ ] OrderBuilder 実装（build_ioc / build_gtc）
- [ ] Signer::build_and_sign() に market_specs 参照を追加
- [ ] cloid 生成規約のテスト（再キュー時に同一 cloid 維持）

### 3.3 Week 2: hip3-position

#### PositionTracker実装

##### 並行モデル: Actor 方式

`PositionTracker` は WS 受信タスクから並行に呼ばれるため、**actor 方式**を採用。
内部状態は単一タスクで管理し、外部からは mpsc で orderUpdates/userFills を投入する。

```rust
use hip3_core::{ClientOrderId, MarketKey, OrderSide, Price, Size};
use tokio::sync::mpsc;

/// 外部からのメッセージ
pub enum PositionTrackerMsg {
    /// 注文登録（enqueue 成功時に Executor から呼ばれる）
    RegisterOrder(TrackedOrder),
    /// 注文削除（送信失敗時に ExecutorLoop から呼ばれる）
    RemoveOrder(ClientOrderId),
    /// 注文更新（WS orderUpdates から）
    OrderUpdate(OrderUpdate),
    OrderSnapshot(Vec<OrderUpdate>),
    /// 約定（WS userFills から）
    UserFill(UserFill),
    FillsSnapshot(Vec<UserFill>),
    /// 状態問い合わせ（oneshot で返信）
    GetPosition { market: MarketKey, reply: oneshot::Sender<Option<Position>> },
    GetPendingOrder { cloid: ClientOrderId, reply: oneshot::Sender<Option<TrackedOrder>> },
    /// 同期的なポジション確認（キャッシュから即座に返す）
    HasPosition { market: MarketKey, reply: oneshot::Sender<bool> },
}

/// PositionTracker の actor タスク
pub struct PositionTrackerTask {
    rx: mpsc::Receiver<PositionTrackerMsg>,
    // 同期読み取り用キャッシュへの参照（Handle と共有）
    positions_cache: Arc<DashMap<MarketKey, bool>>,
    /// pending_markets キャッシュへの参照（Handle と共有、terminal 状態で解除）
    pending_markets_cache: Arc<DashMap<MarketKey, u32>>,
    /// 全ポジションスナップショットへの参照（Handle と共有）
    positions_snapshot: Arc<DashMap<MarketKey, Position>>,
    /// 全 pending orders スナップショットへの参照（Handle と共有）
    pending_orders_snapshot: Arc<DashMap<ClientOrderId, TrackedOrder>>,
    // 内部状態（単一タスクなので Mutex 不要）
    positions: HashMap<MarketKey, Position>,
    pending_orders: HashMap<ClientOrderId, TrackedOrder>,
    // isSnapshot 前のバッファ
    order_buffer: Vec<OrderUpdate>,
    fills_buffer: Vec<UserFill>,
    order_snapshot_received: bool,
    fills_snapshot_received: bool,
}

/// PositionTracker へのハンドル（クローン可、Send + Sync）
#[derive(Clone)]
pub struct PositionTrackerHandle {
    tx: mpsc::Sender<PositionTrackerMsg>,
    /// 同期読み取り用キャッシュ（has_position の高速判定用）
    /// PositionTrackerTask が更新を反映（eventual consistency）
    positions_cache: Arc<DashMap<MarketKey, bool>>,
    /// 未約定注文がある市場のキャッシュ（has_pending_order の高速判定用）
    /// Executor が enqueue 前に **同期的に** 更新（race 回避）
    /// PositionTrackerTask が約定/キャンセル時に削除
    pending_markets_cache: Arc<DashMap<MarketKey, u32>>,  // market -> 未約定注文数

    // --- 4.1/4.2 MaxPosition/HardStop 用の同期キャッシュ ---
    // PositionTrackerTask が更新を反映（eventual consistency）

    /// 全ポジションのスナップショット（read-only）
    /// MaxPosition の notional 計算、HardStop の flatten 対象取得に使用
    positions_snapshot: Arc<DashMap<MarketKey, Position>>,
    /// 全 pending orders のスナップショット（read-only）
    /// MaxPosition の pending notional 計算、HardStop の cancel/cleanup に使用
    pending_orders_snapshot: Arc<DashMap<ClientOrderId, TrackedOrder>>,
}

impl PositionTrackerHandle {
    /// 注文を pending_orders に登録（async 版、通常は try_register_order を使用）
    pub async fn register_order(&self, tracked: TrackedOrder) {
        let _ = self.tx.send(PositionTrackerMsg::RegisterOrder(tracked)).await;
    }

    /// 注文を pending_orders に登録（同期版、enqueue 成功後に呼ぶ）
    /// try_send() を使用し、チャネルが詰まっている場合はエラーを返す
    pub fn try_register_order(&self, tracked: TrackedOrder) -> Result<(), mpsc::error::TrySendError<PositionTrackerMsg>> {
        self.tx.try_send(PositionTrackerMsg::RegisterOrder(tracked))
    }

    /// 注文を pending_orders から削除（送信失敗時に呼ぶ）
    /// NOTE: pending_markets_cache は別途 unmark_pending_market() で解除済み
    pub async fn remove_order(&self, cloid: ClientOrderId) {
        let _ = self.tx.send(PositionTrackerMsg::RemoveOrder(cloid)).await;
    }

    pub async fn on_order_update(&self, update: OrderUpdate) {
        let _ = self.tx.send(PositionTrackerMsg::OrderUpdate(update)).await;
    }

    pub async fn on_order_snapshot(&self, snapshot: Vec<OrderUpdate>) {
        let _ = self.tx.send(PositionTrackerMsg::OrderSnapshot(snapshot)).await;
    }

    pub async fn get_position(&self, market: &MarketKey) -> Option<Position> {
        let (tx, rx) = oneshot::channel();
        let _ = self.tx.send(PositionTrackerMsg::GetPosition {
            market: market.clone(),
            reply: tx,
        }).await;
        rx.await.ok().flatten()
    }

    /// 同期的にポジション有無を確認（キャッシュから即座に返す）
    /// Risk Gate / on_signal での高頻度チェック用
    pub fn has_position(&self, market: &MarketKey) -> bool {
        self.positions_cache.get(market).map(|v| *v).unwrap_or(false)
    }

    // --- PendingOrder Gate 用の同期 API ---

    /// 原子的に gate + mark を行う（存在しなければ insert して true、存在すれば false）
    /// check→mark の非原子性による二重 enqueue を防止
    ///
    /// 戻り値:
    /// - true: mark 成功（この市場に未約定注文がなかった → 発注可能）
    /// - false: mark 失敗（この市場に既に未約定注文がある → 発注不可）
    pub fn try_mark_pending_market(&self, market: &MarketKey) -> bool {
        use dashmap::mapref::entry::Entry;

        match self.pending_markets_cache.entry(market.clone()) {
            Entry::Vacant(vacant) => {
                // 存在しない → insert して true
                vacant.insert(1);
                true
            }
            Entry::Occupied(occupied) => {
                // 既に存在する → false（mark しない）
                // ※ 同一市場での複数注文を禁止する設計
                if *occupied.get() > 0 {
                    false
                } else {
                    // カウントが 0 の場合は mark 可能（cleanup 漏れの場合など）
                    *occupied.into_ref() = 1;
                    true
                }
            }
        }
    }

    /// 同期的に未約定注文有無を確認（キャッシュから即座に返す）
    /// 情報取得のみ - gate には try_mark_pending_market() を使用
    pub fn has_pending_order(&self, market: &MarketKey) -> bool {
        self.pending_markets_cache.get(market).map(|v| *v > 0).unwrap_or(false)
    }

    /// 市場の未約定注文マークを解除（enqueue 失敗時に呼ぶ）
    pub fn unmark_pending_market(&self, market: &MarketKey) {
        if let Some(mut entry) = self.pending_markets_cache.get_mut(market) {
            if *entry > 0 {
                *entry -= 1;
            }
            if *entry == 0 {
                drop(entry);
                self.pending_markets_cache.remove(market);
            }
        }
    }

    /// 市場の未約定注文カウントをデクリメント（約定/キャンセル完了時に呼ぶ）
    /// PositionTrackerTask から呼ばれる
    pub fn decrement_pending_market(&self, market: &MarketKey) {
        self.unmark_pending_market(market);
    }

    // --- 4.1 MaxPosition Gate 用の同期 API ---

    /// 指定市場の position notional を取得（同期、キャッシュから）
    /// notional = abs(size) × mark_px
    pub fn get_notional(&self, market: &MarketKey, mark_px: Price) -> Decimal {
        self.positions_snapshot
            .get(market)
            .map(|pos| pos.size.abs() * mark_px)
            .unwrap_or(Decimal::ZERO)
    }

    /// 指定市場の pending notional を取得（reduce_only 除外、同期、キャッシュから）
    /// MaxPosition gate では reduce_only は除外（解消中の注文はカウントしない）
    pub fn get_pending_notional_excluding_reduce_only(&self, market: &MarketKey, mark_px: Price) -> Decimal {
        let mut total = Decimal::ZERO;
        for entry in self.pending_orders_snapshot.iter() {
            let order = entry.value();
            if &order.market == market && !order.reduce_only {
                // unfilled size × mark_px
                let unfilled = order.original_size - order.filled_size;
                total += unfilled.abs() * mark_px;
            }
        }
        total
    }

    /// 全ポジションを取得（同期、キャッシュから）
    /// HardStop の flatten 対象取得に使用
    pub fn get_all_positions(&self) -> HashMap<MarketKey, Position> {
        self.positions_snapshot
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }

    /// pending order がある市場一覧を取得（同期、キャッシュから）
    /// MaxPosition の total notional 計算に使用
    pub fn get_markets_with_pending_orders(&self) -> HashSet<MarketKey> {
        self.pending_orders_snapshot
            .iter()
            .map(|entry| entry.value().market.clone())
            .collect()
    }

    /// 全 pending order の cloid を取得（同期、キャッシュから）
    /// HardStop の全キャンセルに使用
    pub fn get_all_pending_cloids(&self) -> Vec<ClientOrderId> {
        self.pending_orders_snapshot
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// cloid から market を取得（同期、キャッシュから）
    /// HardStop の new_order purge 時の cleanup に使用
    pub fn get_market_for_cloid(&self, cloid: &ClientOrderId) -> Option<MarketKey> {
        self.pending_orders_snapshot
            .get(cloid)
            .map(|entry| entry.market.clone())
    }
}
```

##### 注文追跡: `TrackedOrder` に統一

`pending_orders` は **`TrackedOrder` で統一**（`PendingOrder` は保持しない）。
enqueue 時に `PendingOrder -> TrackedOrder` に変換して登録。

```rust
/// 注文追跡用の統一構造体
/// - enqueue 時: PendingOrder から変換して登録
/// - orderUpdates: exchange_oid/status/filled_size を更新
/// - 再起動時: orderUpdates.isSnapshot から復元可能
pub struct TrackedOrder {
    pub cloid: ClientOrderId,
    pub exchange_oid: Option<u64>,      // orderUpdates: open で設定
    pub market: MarketKey,
    pub side: OrderSide,
    pub original_size: Size,            // 発注時のサイズ
    pub filled_size: Size,              // 約定済みサイズ
    pub price: Price,                   // 指値価格（IOC でも設定）
    pub reduce_only: bool,
    pub status: OrderStatus,
    pub created_at_ms: u64,             // enqueue 時刻（復元可能）
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    Pending,        // enqueue 済み、未送信 or 送信中
    Open,           // 取引所で受理（exchange_oid 設定済み）
    PartialFilled,  // 部分約定
    Filled,         // 全約定（削除予定）
    Canceled,       // キャンセル完了（削除予定）
    Rejected,       // 拒否（削除予定）
}

impl TrackedOrder {
    /// PendingOrder から TrackedOrder を生成（enqueue 時）
    pub fn from_pending(order: &PendingOrder, now_ms: u64) -> Self {
        Self {
            cloid: order.cloid.clone(),
            exchange_oid: None,
            market: order.market.clone(),
            side: order.side,
            original_size: order.size,
            filled_size: Size::ZERO,
            price: order.price,
            reduce_only: order.reduce_only,
            status: OrderStatus::Pending,
            created_at_ms: now_ms,
        }
    }
}
```

##### Position 構造体

```rust
/// Position は Price/Size 型を使用（OrderBuilder と一貫性を保つ）
/// entry_timestamp_ms は再起動時に復元可能（userFills 由来）
pub struct Position {
    pub market: MarketKey,
    pub side: OrderSide,
    pub size: Size,                     // hip3_core::Size
    pub entry_price: Price,             // 平均約定価格
    pub entry_timestamp_ms: u64,        // 最初の約定時刻（fills 由来、復元可能）
    pub last_fill_timestamp_ms: u64,    // 最後の約定時刻
    pub unrealized_pnl: Price,
}

impl Position {
    /// ポジションが空かどうか（size == 0）
    pub fn is_flat(&self) -> bool {
        self.size.is_zero()
    }

    /// エントリーからの経過時間（ミリ秒）
    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.entry_timestamp_ms)
    }
}
```

**タスク**:
- [ ] PositionTrackerTask actor 実装
- [ ] PositionTrackerHandle API 実装
- [ ] TrackedOrder 構造体と `from_pending()` 実装
- [ ] orderUpdates購読・処理（状態遷移）
- [ ] userFills購読・処理（Position 更新）
- [ ] isSnapshot処理（バッファ→適用→リアルタイム）

#### pending_orders ライフサイクル・整合ルール

##### pending_orders: `TrackedOrder` に統一

`pending_orders` は **`HashMap<ClientOrderId, TrackedOrder>`** で統一。
`PendingOrder` は BatchScheduler のキュー内のみで使用し、enqueue 時に `TrackedOrder` に変換して `pending_orders` に登録。

```rust
// 最終仕様: mark → enqueue → register の順序（on_signal 内で実行）
//
// 1. PendingOrder Gate + mark（原子的）
if !position_tracker.try_mark_pending_market(&market) {
    return ExecutionResult::Skipped(SkipReason::PendingOrderExists);
}

// 2. ActionBudget 確認
if !action_budget.can_send_new_order() {
    position_tracker.unmark_pending_market(&market);  // rollback
    return ExecutionResult::Skipped(SkipReason::BudgetExhausted);
}

// 3. enqueue（mark 済みの状態で実行）
let pending_order = OrderBuilder::build_ioc(...);
match batch_scheduler.enqueue_new_order(pending_order.clone()) {
    EnqueueResult::Queued => { /* success */ }
    EnqueueResult::QueueFull | EnqueueResult::InflightFull => {
        position_tracker.unmark_pending_market(&market);  // rollback
        return ExecutionResult::Skipped(SkipReason::QueueFull);
    }
}

// 4. enqueue 成功 → TrackedOrder を actor に登録
let tracked = TrackedOrder::from_pending(&pending_order, now_ms());
if let Err(e) = position_tracker.try_register_order(tracked.clone()) {
    // try_send 失敗時は fallback で spawn（登録は必ず届ける）
    let handle = position_tracker.clone();
    tokio::spawn(async move { handle.register_order(tracked).await; });
}
```

##### 登録・状態遷移・削除タイミング

| イベント | アクション | TrackedOrder の変化 |
|----------|------------|---------------------|
| **enqueue時** | `register_order(tracked)` | `status: Pending`, `exchange_oid: None` |
| **orderUpdates: open** | `update_order_status()` | `status: Open`, `exchange_oid: Some(...)` |
| **orderUpdates: partial** | `update_order_status()` | `status: PartialFilled`, `filled_size` 更新 |
| **orderUpdates: filled** | `remove_order()` | Position 更新後に削除 |
| **orderUpdates: canceled** | `remove_order()` | 削除（リトライ判断は上流） |
| **orderUpdates: rejected** | `remove_order()` | 削除（ログ/アラート出力） |
| **userFills** | `update_position()` | `pending_orders` は変更しない |

##### 再起動時の整合

```
[再起動]
    │
    ▼
pending_orders は空（メモリ上）
    │
    ▼ (WS 接続)
orderUpdates isSnapshot=true 受領
    │  ├─ open/partial 状態 → TrackedOrder として復元
    │  │    └─ orderUpdates から取得可能な情報のみ（price/reduce_only は復元不可の場合あり）
    │  └─ filled/canceled は復元不要
    │
    ▼
userFills isSnapshot=true 受領
    │  ├─ fills から Position を再構築
    │  └─ entry_timestamp_ms は最初の fill timestamp
    │
    ▼
clearinghouseState 照合（オプション）
    │  └─ Position の整合性検証
    │
    ▼
READY-TRADING
```

**復元時の注意**:
- `TrackedOrder` は orderUpdates から復元可能だが、`price`/`reduce_only` は取引所が返さない場合がある
- 復元不可のフィールドは `Option` にするか、センチネル値（`Price::ZERO`）を使用
- **再起動後の未約定注文は手動キャンセル推奨**（reduce_only 不明のため安全側に倒す）

##### isSnapshot 処理方針（Actor 内部）

| 状態 | 処理 |
|------|------|
| **isSnapshot 前の増分** | バッファに蓄積（破棄しない） |
| **isSnapshot 受領** | snapshot を適用後、バッファ内の増分を時系列順に適用 |
| **isSnapshot 後** | リアルタイム処理に移行 |

**理由**: isSnapshot 前の増分を捨てると、その間に発生した約定やキャンセルを見逃す。

```rust
impl PositionTrackerTask {
    /// メインループ
    async fn run(&mut self) {
        while let Some(msg) = self.rx.recv().await {
            match msg {
                PositionTrackerMsg::RegisterOrder(tracked) => {
                    // enqueue 時に呼ばれる - pending_orders に登録
                    let cloid = tracked.cloid.clone();
                    self.pending_orders.insert(cloid.clone(), tracked.clone());
                    // スナップショットキャッシュにも反映
                    self.pending_orders_snapshot.insert(cloid, tracked);
                }
                PositionTrackerMsg::OrderUpdate(update) => {
                    if !self.order_snapshot_received {
                        self.order_buffer.push(update);
                    } else {
                        self.apply_order_update(update);
                    }
                }
                PositionTrackerMsg::OrderSnapshot(snapshot) => {
                    // 1. snapshot を適用（open/partial → TrackedOrder 復元）
                    for order in snapshot {
                        self.apply_order_update(order);
                    }
                    // 2. バッファを時系列順に適用
                    let buffered = std::mem::take(&mut self.order_buffer);
                    for update in buffered {
                        self.apply_order_update(update);
                    }
                    self.order_snapshot_received = true;
                }
                PositionTrackerMsg::UserFill(fill) => {
                    if !self.fills_snapshot_received {
                        self.fills_buffer.push(fill);
                    } else {
                        self.apply_fill(fill);
                    }
                }
                PositionTrackerMsg::FillsSnapshot(fills) => {
                    // fills から Position を再構築
                    for fill in fills {
                        self.apply_fill(fill);
                    }
                    let buffered = std::mem::take(&mut self.fills_buffer);
                    for fill in buffered {
                        self.apply_fill(fill);
                    }
                    self.fills_snapshot_received = true;
                }
                PositionTrackerMsg::GetPosition { market, reply } => {
                    let _ = reply.send(self.positions.get(&market).cloned());
                }
                PositionTrackerMsg::GetPendingOrder { cloid, reply } => {
                    let _ = reply.send(self.pending_orders.get(&cloid).cloned());
                }
                PositionTrackerMsg::HasPosition { market, reply } => {
                    let _ = reply.send(self.positions.contains_key(&market));
                }
                PositionTrackerMsg::RemoveOrder(cloid) => {
                    // 送信失敗時に呼ばれる - pending_orders から削除
                    self.pending_orders.remove(&cloid);
                    // スナップショットキャッシュからも削除
                    self.pending_orders_snapshot.remove(&cloid);
                }
            }
        }
    }

    /// Position 更新時にキャッシュも同期
    fn update_position(&self, market: &MarketKey, position: Option<Position>) {
        match position {
            Some(pos) => {
                self.positions.insert(market.clone(), pos.clone());
                self.positions_cache.insert(market.clone(), true);
                self.positions_snapshot.insert(market.clone(), pos);
            }
            None => {
                self.positions.remove(market);
                self.positions_cache.remove(market);
                self.positions_snapshot.remove(market);
            }
        }
    }

    /// pending_order 更新時にスナップショットも同期
    fn update_pending_order(&self, cloid: &ClientOrderId, tracked: Option<TrackedOrder>) {
        match tracked {
            Some(order) => {
                self.pending_orders.insert(cloid.clone(), order.clone());
                self.pending_orders_snapshot.insert(cloid.clone(), order);
            }
            None => {
                self.pending_orders.remove(cloid);
                self.pending_orders_snapshot.remove(cloid);
            }
        }
    }
}
```

**タスク（追加）**:
- [ ] `TrackedOrder::from_pending()` と復元ロジック実装
- [ ] pending_orders 登録/状態遷移/削除の PositionTrackerTask 実装
- [ ] isSnapshot バッファリング実装（actor 内部）
- [ ] 再起動時の clearinghouseState からの Position 検証
- [ ] READY-TRADING 条件との整合テスト

#### TimeStop実装

```rust
pub struct TimeStop {
    timeout_ms: u64,              // 30_000 (30秒)
    reduce_only_timeout_ms: u64,  // 60_000 (60秒)
}

impl TimeStop {
    /// ポジションの経過時間を評価
    ///
    /// `now_ms`: 現在時刻（Unix ms）- 外部から注入でテスト容易
    /// `entry_timestamp_ms`: Position.entry_timestamp_ms（fills 由来、復元可能）
    pub fn check(&self, position: &Position, now_ms: u64) -> TimeStopAction {
        let age_ms = position.age_ms(now_ms);

        if age_ms > self.reduce_only_timeout_ms {
            TimeStopAction::AlertAndFlatten
        } else if age_ms > self.timeout_ms {
            TimeStopAction::Flatten
        } else {
            TimeStopAction::Hold
        }
    }
}

pub enum TimeStopAction {
    Hold,               // 保持継続
    Flatten,            // TimeStop 発動、フラット化
    AlertAndFlatten,    // 60秒超過、アラート + フラット化
}
```

##### 再起動時の TimeStop 整合

| シナリオ | 方針 |
|----------|------|
| **正常再起動** | `entry_timestamp_ms` は fills から復元されるため、TimeStop は継続動作 |
| **fills なしで Position がある** | clearinghouseState から Position を復元、`entry_timestamp_ms` は **現在時刻**（安全側） |
| **長時間ダウン後の再起動** | 復元直後に TimeStop が発動する可能性あり（意図通り） |

**重要**: `entry_timestamp_ms` は **fills の timestamp** から取得するため再起動で復元可能。
`Instant` は使用しない（プロセス終了で失われる）。

**タスク**:
- [ ] TimeStop構造体（ミリ秒ベース）
- [ ] `check(position, now_ms)` で経過時間判定
- [ ] フラット化トリガー
- [ ] アラート（60秒超過時）
- [ ] 再起動後の TimeStop 継続動作テスト

#### Flatten実装

```rust
use hip3_core::{ClientOrderId, OrderSide, MarketKey, MarketSpec, Price, Size, PendingOrder};

pub struct Flattener {
    /// MarketSpec は Executor.market_specs を共有参照
    market_specs: Arc<DashMap<MarketKey, MarketSpec>>,
}

impl Flattener {
    /// フラット化用の PendingOrder を構築
    ///
    /// MarketSpec は market_specs から取得（Position には持たせない）
    pub fn build_flatten_order(&self, position: &Position) -> PendingOrder {
        // cloid は生成時に付与（再送でも同一値を使用）
        let cloid = ClientOrderId::new();

        // MarketSpec を取得
        let spec = self.market_specs
            .get(&position.market)
            .expect("MarketSpec must exist for open position");

        OrderBuilder::new(position.market.clone(), spec.value().clone())
            .build_ioc(
                position.side.opposite(),  // OrderSide::opposite()
                self.calculate_aggressive_price(position, spec.value()),
                position.size,
                true,  // reduce_only
                cloid,
            )
    }

    /// 成行相当の aggressive price を計算
    ///
    /// - Buy でクローズ（売り玉をフラット化）→ 高めの価格（mid + slippage%）
    /// - Sell でクローズ（買い玉をフラット化）→ 低めの価格（mid - slippage%）
    fn calculate_aggressive_price(&self, position: &Position, spec: &MarketSpec) -> Price {
        // 実装時: mid price に対して 1-2% の slippage を加味
        // 例: mid=100, slippage=1% → Buy:101, Sell:99
        // MarketSpec.format_price() で tick size に丸める
        todo!()
    }
}
```

**注意**: `Flattener` は `Executor` を直接保持しない（循環参照回避）。
代わりに `market_specs` を共有し、`submit_reduce_only()` は呼び出し側（TimeStop loop 等）で行う。

##### Side 型について

**方針**: `hip3_core::OrderSide` を使用（新規定義しない）

```rust
// hip3_core で定義済み
pub enum OrderSide {
    Buy,
    Sell,
}

impl OrderSide {
    pub fn opposite(&self) -> Self {
        match self {
            OrderSide::Buy => OrderSide::Sell,
            OrderSide::Sell => OrderSide::Buy,
        }
    }
}
```

**タスク**:
- [ ] Flattener構造体（`PendingOrder` を返す）
- [ ] 成行相当の価格計算（aggressive price）
- [ ] reduce_only IOC発注
- [ ] 部分約定時のリトライ
- [ ] `ClientOrderId` を生成時に付与（再送時も同一値維持）

### 3.4 Week 2-3: 統合・READY-TRADING

#### READY-TRADING条件

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::watch;

/// READY-TRADING 状態管理
///
/// **設計方針**:
/// - 各フラグは AtomicBool で高頻度チェックに対応
/// - 全条件達成時に watch channel で通知（ExecutorLoop 起動用）
/// - on_signal() は is_ready() を同期的に呼び出し
pub struct TradingReadyChecker {
    md_ready: AtomicBool,           // READY-MD達成
    order_snapshot: AtomicBool,     // orderUpdates isSnapshot受領
    fills_snapshot: AtomicBool,     // userFills isSnapshot受領
    position_synced: AtomicBool,    // ポジション同期完了
    ready_tx: watch::Sender<bool>,  // 準備完了通知
    ready_rx: watch::Receiver<bool>,
}

impl TradingReadyChecker {
    pub fn new() -> Self {
        let (ready_tx, ready_rx) = watch::channel(false);
        Self {
            md_ready: AtomicBool::new(false),
            order_snapshot: AtomicBool::new(false),
            fills_snapshot: AtomicBool::new(false),
            position_synced: AtomicBool::new(false),
            ready_tx,
            ready_rx,
        }
    }

    /// 同期的に READY-TRADING 判定（on_signal から呼ばれる）
    pub fn is_ready(&self) -> bool {
        self.md_ready.load(Ordering::SeqCst)
            && self.order_snapshot.load(Ordering::SeqCst)
            && self.fills_snapshot.load(Ordering::SeqCst)
            && self.position_synced.load(Ordering::SeqCst)
    }

    /// 準備完了通知の Receiver を取得（clone 可能）
    /// ExecutorLoop 起動前に `mut rx = subscribe()` して待機する
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.ready_rx.clone()
    }

    // --- 各条件のセッター（条件達成時に呼ばれる） ---

    pub fn set_md_ready(&self) {
        self.md_ready.store(true, Ordering::SeqCst);
        self.check_and_notify();
    }

    pub fn set_order_snapshot_received(&self) {
        self.order_snapshot.store(true, Ordering::SeqCst);
        self.check_and_notify();
    }

    pub fn set_fills_snapshot_received(&self) {
        self.fills_snapshot.store(true, Ordering::SeqCst);
        self.check_and_notify();
    }

    pub fn set_position_synced(&self) {
        self.position_synced.store(true, Ordering::SeqCst);
        self.check_and_notify();
    }

    fn check_and_notify(&self) {
        if self.is_ready() {
            let _ = self.ready_tx.send(true);
            tracing::info!("🚀 READY-TRADING achieved");
        }
    }
}
```

#### 各条件の設定タイミング

| 条件 | 設定者 | タイミング |
|------|--------|----------|
| `md_ready` | MarketDataTask（hip3-ws） | READY-MD 達成時（ws/detector から callback） |
| `order_snapshot` | PositionTrackerTask | orderUpdates `isSnapshot=true` 適用完了時 |
| `fills_snapshot` | PositionTrackerTask | userFills `isSnapshot=true` 適用完了時 |
| `position_synced` | PositionTrackerTask | order_snapshot + fills_snapshot 両方適用後、ポジション再計算完了時 |

#### PositionTrackerTask から snapshot 適用完了を通知

`PositionTrackerHandle` に `TradingReadyChecker` の参照を渡し、snapshot 適用完了時にセッターを呼ぶ。

```rust
// PositionTrackerTask の構築時
pub struct PositionTrackerTask {
    // ... 既存フィールド ...
    ready_checker: Arc<TradingReadyChecker>,  // READY-TRADING 通知用
    /// pending_markets_cache への参照（terminal 状態で解除するため）
    pending_markets_cache: Arc<DashMap<MarketKey, u32>>,
}

// snapshot 適用完了時
impl PositionTrackerTask {
    async fn handle_order_snapshot(&mut self, orders: Vec<OrderUpdate>) {
        // ... snapshot 適用処理 ...
        self.order_snapshot_received = true;
        self.ready_checker.set_order_snapshot_received();
        self.try_set_position_synced();
    }

    async fn handle_fills_snapshot(&mut self, fills: Vec<UserFill>) {
        // ... snapshot 適用処理 ...
        self.fills_snapshot_received = true;
        self.ready_checker.set_fills_snapshot_received();
        self.try_set_position_synced();
    }

    fn try_set_position_synced(&self) {
        if self.order_snapshot_received && self.fills_snapshot_received {
            // ポジション同期完了
            self.ready_checker.set_position_synced();
        }
    }

    /// orderUpdates 処理: terminal 状態（filled/canceled/rejected）で pending 解除
    fn handle_order_update(&mut self, update: &OrderUpdate) {
        if let Some(tracked) = self.pending_orders.get_mut(&update.cloid) {
            // 状態更新
            tracked.status = update.status;
            if let Some(filled) = update.filled_size {
                tracked.filled_size = filled;
            }

            // terminal 状態で pending 解除
            if update.status.is_terminal() {
                let market = tracked.market.clone();
                self.pending_orders.remove(&update.cloid);
                self.decrement_pending_market(&market);
                tracing::debug!(cloid = %update.cloid, status = ?update.status, "Order removed (terminal)");
            }
        }
    }

    /// pending_markets_cache をデクリメント
    fn decrement_pending_market(&self, market: &MarketKey) {
        if let Some(mut entry) = self.pending_markets_cache.get_mut(market) {
            if *entry > 0 {
                *entry -= 1;
            }
            if *entry == 0 {
                drop(entry);
                self.pending_markets_cache.remove(market);
            }
        }
    }
}

/// OrderStatus の terminal 判定
impl OrderStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Filled | Self::Canceled | Self::Rejected)
    }
}
```

#### PendingOrder Gate の解除タイミング

| イベント | 解除方法 | 責務 |
|---------|---------|------|
| enqueue 失敗（QueueFull/InflightFull） | `unmark_pending_market()` | Executor |
| orderUpdates: filled/canceled/rejected | `decrement_pending_market()` | PositionTrackerTask |
| WS 送信失敗（new_order）| `unmark_pending_market()` + actor に `RemoveOrder(cloid)` 送信 | ExecutorLoop |
| WS 送信タイムアウト | 応答待ち→orderUpdates で解除 | （自動） |

**タスク（READY-TRADING）**:
- [ ] TradingReadyChecker 実装（AtomicBool + watch channel）
- [ ] PositionTrackerTask に ready_checker 参照を追加
- [ ] orderUpdates isSnapshot 処理で set_order_snapshot_received() 呼び出し
- [ ] userFills isSnapshot 処理で set_fills_snapshot_received() 呼び出し
- [ ] MarketDataTask から set_md_ready() 呼び出し（callback/channel）
- [ ] ExecutorLoop 起動前に `subscribe()` で Receiver を取得して待機（下記参照）
- [ ] on_signal() で is_ready() チェック追加（READY-TRADING 未達 → NotReady 拒否）

**ExecutorLoop 起動時の待機方法**:
```rust
// オーケストレータ側
let ready_checker = Arc::new(TradingReadyChecker::new());
let mut ready_rx = ready_checker.subscribe();

// 各タスクを起動（ready_checker を渡す）
let position_tracker = PositionTrackerTask::spawn(ready_checker.clone(), ...);
let market_data = MarketDataTask::spawn(ready_checker.clone(), ...);

// READY-TRADING 達成まで待機
while !*ready_rx.borrow() {
    ready_rx.changed().await.ok();
}

// ExecutorLoop 起動
let executor_loop = ExecutorLoop::new(executor, ...).spawn();
```

**タスク（PendingOrder Gate）**:
- [ ] PositionTrackerHandle に pending_markets_cache 追加
- [ ] has_pending_order() / mark_pending_market() / unmark_pending_market() 実装
- [ ] on_signal() で has_pending_order() チェック追加
- [ ] enqueue 成功前に mark_pending_market() 呼び出し
- [ ] enqueue 失敗時に unmark_pending_market() 呼び出し
- [ ] PositionTrackerTask で約定/キャンセル完了時に decrement_pending_market() 呼び出し
- [ ] 単体テスト: race condition 回避の確認

#### Executor統合

##### 型エイリアス（API整合）

```rust
/// NonceManager の具体型（テスト時は MockClock を注入）
pub type NonceManagerImpl = NonceManager<SystemClock>;
```

##### Executor フロー図

```
Signal (new order)                    TimeStop/Flatten (reduce_only)
  │                                           │
  ▼                                           ▼
Executor::on_signal()                 Executor::submit_reduce_only()
  │  ├─ [Gate 0] HardStop 確認                │  └─ (HardStop でも通す)
  │  │      └─ 発火中 → Rejected(HardStop)    │  └─ mark_pending_market()
  │  ├─ [Gate 1] READY-TRADING 確認           │  └─ enqueue_reduce_only()
  │  │      └─ 未達 → Rejected(NotReady)      │       ↓ (優先キュー)
  │  ├─ [Gate 2] MaxPosition 確認             │
  │  │      └─ 超過 → Rejected(MaxPosition*)  │
  │  ├─ [Gate 3] has_position 確認            │
  │  │      └─ あり → Skipped                 │
  │  ├─ [Gate 4] has_pending_order 確認       │
  │  │      └─ あり → Skipped                 │
  │  ├─ [Gate 5] ActionBudget 確認            │
  │  │      └─ 切れ → Skipped                 │
  │  ├─ mark_pending_market() ← **同期更新**  │
  │  └─ enqueue_new_order()                   │
  │       ↓                                   │
  ▼───────┴───────────────────────────────────┘
BatchScheduler (3キュー: cancel > reduce_only > new_order)
  │
  ▼ (100ms 周期)
ExecutorLoop::tick()
  │  ├─ check_timeouts()
  │  ├─ batch_scheduler.tick() → Option<ActionBatch>
  │  ├─ nonce_manager.next() → u64
  │  ├─ signer.build_and_sign(&batch, nonce, &market_specs) → SignedAction
  │  ├─ post_manager.register(post_id, nonce, batch)
  │  └─ ws_sender.post(signed_action, post_id)
  │
  ▼
Exchange WS
  │
  ▼ (応答 or タイムアウト or 切断)
ExecutorLoop::on_ws_message() / handle_timeouts() / on_disconnect()
  │  ├─ post_manager.on_response()
  │  ├─ on_batch_complete()
  │  └─ handle_send_failure() → reduce_only 再キュー
```

##### Executor 構造体

```rust
pub struct Executor {
    nonce_manager: Arc<NonceManagerImpl>,
    batch_scheduler: Arc<BatchScheduler>,
    signer: Arc<Signer>,
    /// PositionTrackerHandle（actor 方式）
    /// - has_position() は同期キャッシュから即座に返す
    /// - has_pending_order() は同期キャッシュから即座に返す
    /// - register_order() は async
    position_tracker: PositionTrackerHandle,
    action_budget: Arc<ActionBudget>,
    market_specs: Arc<DashMap<MarketKey, MarketSpec>>,  // PendingOrder → OrderWire 変換用
    /// READY-TRADING チェッカー
    ready_checker: Arc<TradingReadyChecker>,
    /// HardStop latch（4.2 参照）
    hard_stop_latch: Arc<HardStopLatch>,
    /// MarketState キャッシュ（markPx 取得用、4.1 MaxPosition で使用）
    market_state_cache: Arc<MarketStateCache>,
    /// 設定（max_notional 等）
    config: ExecutorConfig,
    /// Flattener（HardStop 時の flatten 発火用）
    flattener: Arc<Flattener>,
    /// アラートサービス（HardStop 時の通知用）
    alert_service: Arc<AlertService>,
}

/// Executor 設定
pub struct ExecutorConfig {
    pub max_notional_per_market: Decimal,  // $50
    pub max_notional_total: Decimal,       // $100
}

/// MarketState キャッシュ（WS subscription で更新）
pub struct MarketStateCache {
    /// market → (mark_px, update_time)
    cache: DashMap<MarketKey, (Price, Instant)>,
}

impl MarketStateCache {
    pub fn get_mark_price(&self, market: &MarketKey) -> Option<Price> {
        self.cache.get(market).map(|v| v.0)
    }

    pub fn update(&self, market: &MarketKey, mark_px: Price) {
        self.cache.insert(market.clone(), (mark_px, Instant::now()));
    }
}

impl Executor {
    /// Signal 受信時（新規注文、同期メソッド）
    ///
    /// **Gate チェック順序**:
    /// 0. HardStop → reject（緊急停止中）
    /// 1. READY-TRADING 未達 → reject（システム未準備）
    /// 2. MaxPosition → reject（ポジション上限超過）
    /// 3. has_position → skip（既にポジションあり）
    /// 4. has_pending_order → skip（同一市場に未約定注文あり）
    /// 5. ActionBudget → skip（予算切れ）
    ///
    /// **登録順序**（リーク防止）:
    /// 1. mark_pending_market() - 同期キャッシュ更新
    /// 2. enqueue_new_order() - キュー追加
    /// 3. register_order() - actor に TrackedOrder 登録（enqueue 成功後のみ）
    ///
    /// NOTE: has_position/has_pending_order はキャッシュから同期読み取り（eventual consistency）
    pub fn on_signal(&self, signal: &Signal) -> ExecutionResult {
        // 0. HardStop チェック（最優先、緊急停止中は新規禁止）
        if self.hard_stop_latch.is_triggered() {
            return ExecutionResult::Rejected(RejectReason::HardStop);
        }

        // 1. READY-TRADING チェック
        if !self.ready_checker.is_ready() {
            return ExecutionResult::Rejected(RejectReason::NotReady);
        }

        // 2. MaxPosition チェック（per-market / total）
        if let Err(reason) = self.check_max_position(&signal.market, signal.size) {
            return ExecutionResult::Rejected(reason);
        }

        // 3. Risk Gate再検証（保有時）
        // has_position() は同期キャッシュから即座に返す
        if self.position_tracker.has_position(&signal.market) {
            return ExecutionResult::Skipped(SkipReason::AlreadyHasPosition);
        }

        // 4. PendingOrder Gate + mark（原子的）
        // try_mark_pending_market() は「存在しなければ insert して true、存在すれば false」
        // check→mark の非原子性による二重 enqueue を防止
        if !self.position_tracker.try_mark_pending_market(&signal.market) {
            return ExecutionResult::Skipped(SkipReason::PendingOrderExists);
        }

        // 5. ActionBudget確認
        if !self.action_budget.can_send_new_order() {
            // mark 済みなので rollback
            self.position_tracker.unmark_pending_market(&signal.market);
            return ExecutionResult::Skipped(SkipReason::BudgetExhausted);
        }

        // 6. 注文構築（reduce_only = false）
        let order = self.build_order(signal, false);
        let cloid = order.cloid.clone();
        let market = order.market.clone();

        // 5. 新規注文キューに追加
        let enqueue_result = self.batch_scheduler.enqueue_new_order(order.clone());

        match enqueue_result {
            EnqueueResult::Queued | EnqueueResult::QueuedDegraded => {
                // 6. enqueue 成功 → TrackedOrder を actor に登録
                let tracked = TrackedOrder::from_pending(&order, now_ms());
                if let Err(e) = self.position_tracker.try_register_order(tracked.clone()) {
                    // try_send 失敗時は fallback で spawn（登録は必ず届ける）
                    // NOTE: HardStop 中は register しない（state 汚染防止）
                    //       HardStop 発火時に drop_new_orders() + on_hard_stop() で
                    //       pending_orders_snapshot に反映される前の注文を cleanup するが、
                    //       fallback spawn が HardStop 後に届くと再登録されて state が汚れる
                    tracing::warn!(cloid = %cloid, "try_register_order failed, fallback to spawn: {}", e);
                    let handle = self.position_tracker.clone();
                    let hard_stop_latch = self.hard_stop_latch.clone();
                    tokio::spawn(async move {
                        // HardStop 中は register をスキップ（既に cleanup 対象）
                        if hard_stop_latch.is_triggered() {
                            tracing::debug!(cloid = %tracked.cloid, "Skipping register_order: HardStop triggered");
                            return;
                        }
                        handle.register_order(tracked).await;
                    });
                }

                if matches!(enqueue_result, EnqueueResult::QueuedDegraded) {
                    tracing::info!("Order queued but system degraded");
                    ExecutionResult::QueuedDegraded
                } else {
                    ExecutionResult::Queued
                }
            }
            EnqueueResult::QueueFull => {
                // キュー溢れ → キャッシュから削除（actor に TrackedOrder は送っていない）
                self.position_tracker.unmark_pending_market(&market);
                ExecutionResult::Rejected(RejectReason::QueueFull)
            }
            EnqueueResult::InflightFull => {
                // inflight 上限 → キャッシュから削除（actor に TrackedOrder は送っていない）
                self.position_tracker.unmark_pending_market(&market);
                ExecutionResult::Rejected(RejectReason::InflightFull)
            }
        }
    }

    /// reduce_only 注文（TimeStop/Flatten 用、優先キュー）
    ///
    /// NOTE: reduce_only は READY-TRADING / PendingOrder Gate をスキップ
    ///       （既存ポジションの決済なので常に受け付ける）
    ///
    /// **登録順序**（リーク防止）:
    /// 1. mark_pending_market() - 同期キャッシュ更新（reduce_only は gate なしで常に mark）
    /// 2. enqueue_reduce_only() - キュー追加
    /// 3. register_order() - actor に TrackedOrder 登録（enqueue 成功後のみ、必ず届ける）
    pub fn submit_reduce_only(&self, order: PendingOrder) -> ExecutionResult {
        debug_assert!(order.reduce_only);

        let cloid = order.cloid.clone();
        let market = order.market.clone();

        // pending_markets キャッシュを更新（reduce_only は gate なし、常に mark）
        // NOTE: reduce_only は既存ポジションの決済なので、同一市場に複数の reduce_only が
        //       並行して走ることは許容（例: TimeStop と Flatten が同時に発火）
        self.position_tracker.mark_pending_market(&market);

        // reduce_only は優先キューへ（高水位でも受付）
        let enqueue_result = self.batch_scheduler.enqueue_reduce_only(order.clone());

        match enqueue_result {
            EnqueueResult::Queued | EnqueueResult::InflightFull | EnqueueResult::QueuedDegraded => {
                // enqueue 成功 → TrackedOrder を actor に登録（必ず届ける）
                let tracked = TrackedOrder::from_pending(&order, now_ms());
                if let Err(e) = self.position_tracker.try_register_order(tracked.clone()) {
                    // try_send 失敗時は fallback で spawn
                    // NOTE: reduce_only は HardStop 中も許可されるため、HardStop チェックは不要
                    //       （HardStop の cleanup 対象は new_order のみ）
                    tracing::warn!(cloid = %cloid, "try_register_order failed, fallback to spawn: {}", e);
                    let handle = self.position_tracker.clone();
                    tokio::spawn(async move {
                        handle.register_order(tracked).await;
                    });
                }
                ExecutionResult::Queued
            }
            EnqueueResult::QueueFull => {
                // reduce_only キュー溢れは CRITICAL
                self.position_tracker.unmark_pending_market(&market);
                tracing::error!("CRITICAL: reduce_only queue full");
                ExecutionResult::Rejected(RejectReason::QueueFull)
            }
        }
    }

    /// キャンセル（最優先キュー）
    pub fn submit_cancel(&self, cancel: PendingCancel) -> ExecutionResult {
        match self.batch_scheduler.enqueue_cancel(cancel) {
            EnqueueResult::Queued => ExecutionResult::Queued,
            EnqueueResult::QueueFull => {
                tracing::error!("CRITICAL: cancel queue full");
                ExecutionResult::Rejected(RejectReason::QueueFull)
            }
            _ => ExecutionResult::Queued,
        }
    }
}

/// 実行結果
pub enum ExecutionResult {
    Queued,
    QueuedDegraded,
    Skipped(SkipReason),
    Rejected(RejectReason),
}

pub enum SkipReason {
    AlreadyHasPosition,     // 既にポジションあり
    PendingOrderExists,     // 同一市場に未約定注文あり（PendingOrder Gate）
    BudgetExhausted,        // ActionBudget 切れ
}

pub enum RejectReason {
    HardStop,              // 緊急停止中（4.2 参照）
    NotReady,              // READY-TRADING 未達（システム準備中）
    MaxPositionPerMarket,  // per-market notional 上限超過
    MaxPositionTotal,      // total notional 上限超過
    QueueFull,             // キュー溢れ
    InflightFull,          // inflight 上限
}
```

##### PostRequestManager（応答追跡・タイムアウト）

**目的**: 応答が来ない/切断/再接続で inflight が戻らず `tick()` が止まるデッドロックを防止

**相関キー**: WS の post 応答は `id` フィールドで相関される（nonce ではない）。
計画では `post_id`（クライアント生成の UUID/連番）を使用。

```rust
/// post_id 生成（連番）
pub struct PostIdGenerator {
    next_id: AtomicU64,
}

impl PostIdGenerator {
    pub fn next(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }
}

/// post リクエストの応答を追跡
///
/// 状態遷移:
///   register() → PendingRequest { sent: false }
///   mark_as_sent() → PendingRequest { sent: true }
///   on_response() → 削除 (sent: true なら inflight decrement)
///   check_timeouts() → 削除 (sent: true のみ inflight decrement)
///   on_disconnect() → 全削除 (sent: true の数だけ inflight decrement)
///
/// これにより、「pending に入っているが inflight には入っていない」状態を
/// 正しく処理できる（送信失敗時は inflight decrement 不要）。
pub struct PostRequestManager {
    pending: DashMap<u64, PendingRequest>, // post_id -> request
    timeout: Duration,                      // 5秒
}

pub struct PendingRequest {
    batch: ActionBatch,  // ActionBatch（Orders or Cancels）
    nonce: u64,          // 付随情報として保持
    sent_at: Instant,
    sent: bool,          // true = WS 送信成功済み（inflight increment 済み）
    tx: Option<oneshot::Sender<PostResult>>,  // Option: take() で move 可能にする
}

pub enum PostResult {
    Accepted,
    Rejected { reason: String },
    Timeout,
    Disconnected,
    SendError,  // WS送信自体が失敗
}

impl PostRequestManager {
    /// リクエスト登録（post_id で管理、sent: false で登録）
    pub fn register(
        &self,
        post_id: u64,
        nonce: u64,
        batch: ActionBatch,
    ) -> oneshot::Receiver<PostResult> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(post_id, PendingRequest {
            batch,
            nonce,
            sent_at: Instant::now(),
            sent: false, // 送信前
            tx: Some(tx),  // Option でラップ
        });
        rx
    }

    /// WS 送信成功時に呼び出し（sent: true にマーク）
    /// これ以降、タイムアウト/切断時に inflight decrement が必要になる
    pub fn mark_as_sent(&self, post_id: u64) {
        if let Some(mut entry) = self.pending.get_mut(&post_id) {
            entry.sent = true;
        }
    }

    /// 応答受信時（post_id で検索）
    /// Returns: (batch, sent) - sent が true なら inflight decrement が必要
    pub fn on_response(&self, post_id: u64, result: PostResult) -> Option<(ActionBatch, bool)> {
        self.pending.remove(&post_id).map(|(_, mut req)| {
            // Option::take() で Sender を move out して send
            if let Some(tx) = req.tx.take() {
                let _ = tx.send(result);
            }
            (req.batch, req.sent)
        })
    }

    /// タイムアウトチェック（定期実行）
    /// Returns: Vec<(post_id, batch, sent)> - sent が true なら inflight decrement が必要
    ///
    /// NOTE: `retain()` のクロージャ内で `oneshot::Sender::send()` を呼ぶには
    /// `tx: Option<...>` にして `take()` で move out する必要がある
    pub fn check_timeouts(&self) -> Vec<(u64, ActionBatch, bool)> {
        let now = Instant::now();
        let mut timed_out = vec![];

        self.pending.retain(|post_id, req| {
            if now.duration_since(req.sent_at) > self.timeout {
                // Option::take() で Sender を move out して send
                if let Some(tx) = req.tx.take() {
                    let _ = tx.send(PostResult::Timeout);
                }
                timed_out.push((*post_id, req.batch.clone(), req.sent));
                false // remove
            } else {
                true // keep
            }
        });

        timed_out
    }

    /// WS切断時: 全リクエストを Disconnected で完了
    /// Returns: (batches, sent_count) - sent_count 分だけ inflight decrement が必要
    pub fn on_disconnect(&self) -> (Vec<ActionBatch>, usize) {
        let mut batches = vec![];
        let mut sent_count = 0;

        // DashMap::drain() は Iterator<Item = (K, V)> を返す
        // drain() で所有権を取得するので req は move 可能
        for (_post_id, mut req) in self.pending.drain() {
            // Option::take() で Sender を move out して send
            if let Some(tx) = req.tx.take() {
                let _ = tx.send(PostResult::Disconnected);
            }
            if req.sent {
                sent_count += 1;
            }
            batches.push(req.batch);
        }

        (batches, sent_count)
    }
}
```

##### 送信失敗時の再キュー方針

| 失敗種別 | reduce_only | new_order | cancel |
|----------|-------------|-----------|--------|
| WS送信エラー | **再キュー** | **ログ+アラート** | 再キュー |
| 応答 Rejected | ログのみ | ログのみ | ログのみ |
| タイムアウト | **再キュー** | **ログ+アラート** | 再キュー |

**注意**: new_order の送信失敗は enqueue 後（`on_signal()` が既に `Queued` を返した後）に発生するため、
戻り値として「上流へ返す」ことは不可能。代わりに以下で対応:
- **ログ**: `tracing::warn!` で詳細を出力
- **メトリクス**: `order_send_failure_total` カウンタをインクリメント
- **アラート**: 連続失敗時は `tracing::error!` + 監視システムへ通知

| WS切断 | **再キュー** | **ログ+アラート** | 再キュー |

**方針**:
- **reduce_only/cancel**: 失敗時は `requeue_reduce_only()` で先頭に再キュー（Flatten 必達）
- **new_order**: 失敗時はログ/メトリクス/アラートで対応（pending 解除を忘れずに）

##### ExecutorLoop（tick ループ）

**並行性前提: 直列実行を保証**

`ExecutorLoop` は **1 タスクで `select!` を使って直列実行**する設計。
これにより `PostRequestManager` への並行アクセスが発生せず、inflight 会計のレースを回避:

- `tick()`: 100ms 周期で `mark_as_sent()` / `on_batch_sent()` を呼ぶ
- `on_ws_message()`: WS メッセージ受信時に `on_response()` / `on_batch_complete()` を呼ぶ
- 両者は **同一タスク内で直列に実行**されるため、`mark_as_sent()` と `on_response()` の順序が入れ替わるレースは発生しない

```rust
// メインループ例（select! で直列化）
pub async fn run(&self) {
    let mut interval = tokio::time::interval(self.interval);
    loop {
        tokio::select! {
            _ = interval.tick() => {
                self.tick().await;
            }
            msg = self.ws_receiver.recv() => {
                if let Some(msg) = msg {
                    self.on_ws_message(&msg);
                }
            }
            // ... 他のイベント（切断検知など）
        }
    }
}
```

**inflight 管理ルール**:
- `on_batch_sent()`: 送信成功時に increment
- `on_batch_complete()`: 応答受信/タイムアウト時に decrement
- 送信失敗時: increment しないので decrement も不要
- 切断時: `on_disconnect()` で全回収（pending 数分を decrement）

```rust
pub struct ExecutorLoop {
    ws_receiver: mpsc::Receiver<WsMessage>,  // WS メッセージ受信用
    executor: Arc<Executor>,
    ws_sender: Arc<WsSender>,
    post_manager: Arc<PostRequestManager>,
    post_id_gen: Arc<PostIdGenerator>,
    interval: Duration, // 100ms
    /// RiskMonitor へのイベント送信用
    risk_event_tx: mpsc::Sender<ExecutionEvent>,
}

impl ExecutorLoop {
    /// 100ms 周期で実行
    ///
    /// **SDK 仕様準拠**: 1 tick = 1 action type（orders か cancels のどちらか）
    ///
    /// **HardStop 対応**:
    /// - キューから取り出した後に HardStop が発火した場合でも安全
    /// - ActionBatch::Orders の場合、署名/送信前に HardStop をチェック
    /// - HardStop 中は reduce_only のみにフィルタし、new_order は drop + cleanup
    /// - フィルタ後に空なら何も送らない（nonce/post_id を消費しない）
    pub async fn tick(&self) {
        // 0. タイムアウトチェック
        self.handle_timeouts();

        // 1. ActionBatch 収集（inflight increment はまだしない）
        //    tick() は cancel 優先で、orders と cancels を同時には返さない
        let Some(action_batch) = self.executor.batch_scheduler.tick() else {
            return;
        };

        // 2. **送信直前の HardStop ガード**
        //    batch_scheduler.tick() でキューから取り出した後に HardStop が発火した場合、
        //    new_order が送信されてしまうのを防ぐ（cancel/reduce_only は常に許可）
        let action_batch = match action_batch {
            ActionBatch::Orders(orders) => {
                if self.executor.hard_stop_latch.is_triggered() {
                    // HardStop 中: reduce_only のみ許可、new_order は drop + cleanup
                    let (reduce_only, new_orders): (Vec<_>, Vec<_>) =
                        orders.into_iter().partition(|o| o.reduce_only);

                    // drop された new_order の pending 解除（drop_new_orders と同等の扱い）
                    for order in &new_orders {
                        self.executor.position_tracker.unmark_pending_market(&order.market);
                        let cloid = order.cloid.clone();
                        let handle = self.executor.position_tracker.clone();
                        tokio::spawn(async move {
                            handle.remove_order(cloid).await;
                        });
                        tracing::warn!(
                            cloid = %order.cloid,
                            market = %order.market,
                            "new_order dropped by HardStop guard (post-dequeue)"
                        );
                    }

                    if reduce_only.is_empty() {
                        // reduce_only も無い → 何も送らない（nonce/post_id を消費しない）
                        tracing::debug!("HardStop: all orders dropped, skipping tick");
                        return;
                    }

                    ActionBatch::Orders(reduce_only)
                } else {
                    ActionBatch::Orders(orders)
                }
            }
            // cancel は HardStop でも常に許可
            batch @ ActionBatch::Cancels(_) => batch,
        };

        // 3. post_id 生成（応答相関用）
        let post_id = self.post_id_gen.next();

        // 4. nonce 払い出し
        let nonce = self.executor.nonce_manager.next();

        // 5. 署名（post_id は署名対象外）
        //    ActionBatch → Action 変換（PendingOrder → OrderWire）+ EIP-712 署名
        let signed_action = self.executor.signer
            .build_and_sign(&action_batch, nonce, &self.executor.market_specs)
            .await
            .expect("Signing should not fail with valid key");

        // 6. 応答追跡に登録（sent: false で登録）
        let _rx = self.post_manager.register(post_id, nonce, action_batch.clone());

        // 7. WS 送信（post_id は WsSender 層で付与）
        if let Err(e) = self.ws_sender.post(signed_action, post_id).await {
            tracing::error!(error = %e, post_id, "Failed to post action");

            // 送信失敗: PostManager から除去し、reduce_only を再キュー
            // sent: false なので inflight decrement 不要
            if let Some((batch, _sent)) = self.post_manager.on_response(post_id, PostResult::SendError) {
                self.handle_send_failure(batch);
            }
            return;
        }

        // 8. 送信成功:
        //    (a) sent: true にマーク
        //    (b) inflight increment
        self.post_manager.mark_as_sent(post_id);
        self.executor.batch_scheduler.on_batch_sent();
        tracing::debug!(post_id, nonce, "Action sent successfully");
    }

    /// WS メッセージ受信時
    pub fn on_ws_message(&self, msg: &WsMessage) {
        if let Some(action_response) = msg.as_action_response() {
            // post_id で相関
            let post_id = action_response.id;
            let result = match &action_response.status {
                ActionStatus::Ok => PostResult::Accepted,
                ActionStatus::Err(reason) => PostResult::Rejected { reason: reason.clone() },
            };

            if let Some((batch, sent)) = self.post_manager.on_response(post_id, result.clone()) {
                // sent: true の場合のみ inflight デクリメント
                // （送信成功していた場合のみ inflight にカウントされている）
                if sent {
                    self.executor.batch_scheduler.on_batch_complete();
                }

                match result {
                    PostResult::Accepted => {
                        tracing::debug!(post_id, "Action accepted");
                    }
                    PostResult::Rejected { reason } => {
                        // Rejected は terminal 扱い: pending_markets_cache と pending_orders を解除
                        // orderUpdates が来ない可能性があるため、ここで明示的に cleanup
                        tracing::warn!(post_id, %reason, "Action rejected, cleaning up pending state");

                        // ActionBatch 内の全注文について pending 解除
                        // NOTE: ActionBatch::Orders の場合のみ pending 解除が必要
                        //       ActionBatch::Cancels の場合は pending_markets_cache に影響しない
                        if let ActionBatch::Orders(orders) = batch {
                            for order in &orders {
                                // pending_markets_cache 解除
                                self.executor.position_tracker.unmark_pending_market(&order.market);
                                // pending_orders 解除（actor 側）
                                let cloid = order.cloid.clone();
                                let handle = self.executor.position_tracker.clone();
                                tokio::spawn(async move {
                                    handle.remove_order(cloid).await;
                                });

                                // reduce_only の場合はアラート（Flatten 必達のため）
                                if order.reduce_only {
                                    tracing::error!(
                                        cloid = %order.cloid,
                                        market = %order.market,
                                        "ALERT: reduce_only order rejected - manual intervention may be required"
                                    );
                                }
                            }
                        }
                        // Rejected は再キューしない（取引所が明示的に拒否）
                    }
                    _ => {}
                }
            }
        }
    }

    /// WS 切断時
    ///
    /// inflight 回収の唯一の正（BatchScheduler::on_disconnect() は使わない）
    /// - sent: true のリクエストのみ inflight decrement
    /// - sent: false のリクエストは inflight にカウントされていない
    pub fn on_disconnect(&self) {
        // 1. 全 pending リクエストを Disconnected で完了
        let (batches, sent_count) = self.post_manager.on_disconnect();

        // 2. reduce_only/cancel を再キュー
        for batch in batches {
            self.handle_send_failure(batch);
        }

        // 3. sent: true だった分のみ inflight decrement
        for _ in 0..sent_count {
            self.executor.batch_scheduler.on_batch_complete();
        }

        tracing::warn!(
            sent_count,
            total_pending = sent_count, // batches.len() was moved
            "Disconnected: inflight recovered (sent requests only)"
        );
    }

    /// タイムアウトハンドリング
    fn handle_timeouts(&self) {
        let timed_out = self.post_manager.check_timeouts();
        for (post_id, batch, sent) in timed_out {
            tracing::warn!(post_id, sent, "Post request timed out");

            // sent: true の場合のみ inflight decrement
            if sent {
                self.executor.batch_scheduler.on_batch_complete();
            }

            self.handle_send_failure(batch);
        }
    }

    /// 送信失敗時のハンドリング
    ///
    /// ActionBatch の種類に応じて再キュー/解除
    fn handle_send_failure(&self, batch: ActionBatch) {
        match batch {
            ActionBatch::Orders(orders) => {
                for order in orders {
                    if order.reduce_only {
                        // reduce_only は再キュー（TimeStop/Flatten 必達）
                        tracing::warn!(cloid = %order.cloid, "Re-queuing reduce_only order after failure");
                        let _ = self.executor.batch_scheduler.enqueue_reduce_only(order);
                    } else {
                        // new_order は pending 解除（リーク防止）
                        // 1. pending_markets_cache を解除
                        self.executor.position_tracker.unmark_pending_market(&order.market);
                        // 2. actor 側の pending_orders を解除
                        let handle = self.executor.position_tracker.clone();
                        let cloid = order.cloid.clone();
                        tokio::spawn(async move {
                            handle.remove_order(cloid).await;
                        });
                        tracing::warn!(cloid = %order.cloid, "New order dropped after send failure");
                        // メトリクス更新
                        // metrics::counter!("order_send_failure_total", 1);
                        // 連続失敗時はアラート（監視システムで検知）
                    }
                }
            }
            ActionBatch::Cancels(cancels) => {
                // cancel は再キュー
                for cancel in cancels {
                    let _ = self.executor.batch_scheduler.enqueue_cancel(cancel);
                }
            }
        }
    }
}
```

##### ExecutorLoop テスト項目

| # | テスト | 期待動作 |
|---|--------|----------|
| 1 | 正常フロー | tick → post 成功 → mark_as_sent() → on_batch_sent() → on_ws_message → on_batch_complete |
| 2 | 送信失敗 | sent: false のまま、inflight 不変、reduce_only/cancel が再キュー |
| 3 | タイムアウト (sent: true) | 5秒後に timeout、on_batch_complete + reduce_only 再キュー |
| 4 | タイムアウト (sent: false) | timeout、inflight 不変、reduce_only 再キュー |
| 5 | WS切断 | 全 pending が Disconnected、**sent: true の数だけ** decrement |
| 6 | Rejected 応答 | sent: true なら on_batch_complete、再キューなし |
| 7 | post_id 相関 | 応答は post_id で正しく相関される |
| 8 | sent フラグ状態遷移 | register() で false → mark_as_sent() で true |
| 9 | inflight 整合性 | sent数 - complete数 = inflight が常に成立 |

**タスク**:
- [ ] PostIdGenerator 実装
- [ ] PostRequestManager 実装（post_id ベース、sent フラグ付き）
- [ ] PendingRequest に sent: bool フィールド追加
- [ ] mark_as_sent() 実装（送信成功時に呼び出し）
- [ ] タイムアウト設計（5秒）
- [ ] on_batch_sent() で inflight increment（送信成功時のみ）
- [ ] on_disconnect() で **sent: true の数だけ** decrement + 再キュー（唯一の正）
- [ ] handle_send_failure() で reduce_only/cancel を再キュー
- [ ] handle_timeouts() を tick() 内で定期実行（sent フラグで条件分岐）
- [ ] ユニットテスト（9項目）

### 3.5 Week 3: Testnet検証

#### 起動 Runbook（Testnet）

##### 1. 設定ファイル確認

```bash
# Testnet 専用設定で起動
export HIP3_CONFIG=config/testnet.toml

# 設定内容を事前確認
cat $HIP3_CONFIG | grep -E "(ws_url|info_url|mode)"
# 期待出力:
#   ws_url = "wss://api.hyperliquid-testnet.xyz/ws"
#   info_url = "https://api.hyperliquid-testnet.xyz/info"
#   mode = "trading"  # Phase B 実取引検証なので trading モード
```

##### 2. Trading Key 供給

```toml
# config/testnet.toml
[signer]
# 方法 A: 環境変数から供給（推奨）
key_source = { type = "env_var", var_name = "HIP3_TESTNET_PRIVATE_KEY" }

# 方法 B: ファイルから供給
# key_source = { type = "file", path = "secrets/testnet_key.hex" }
```

```bash
# 環境変数で供給する場合（hex 文字列、0x prefix 許容）
export HIP3_TESTNET_PRIVATE_KEY="0x..."

# 鍵の address は起動時ログで確認（Testnet wallet と一致するか）
# 起動時に [INFO] Signer address: 0x1234... が出力される
```

##### 3. 安全装置（Testnet 限定ガード）

| ガード | 値 | 説明 |
|--------|-----|------|
| `testnet_only` | `true` | Mainnet URL で起動時に panic |
| `max_notional_usd` | `100` | 1 ポジションの最大想定元本 |
| `allowed_markets` | `["BTC", "ETH", "SOL"]` | 検証対象を限定 |
| `max_daily_trades` | `50` | 1 日の最大トレード数 |
| `auto_stop_loss_usd` | `20` | 累積損失で自動停止 |

```toml
# config/testnet.toml
[safety]
testnet_only = true
max_notional_usd = 100
allowed_markets = ["BTC", "ETH", "SOL"]
max_daily_trades = 50
auto_stop_loss_usd = 20
```

##### 4. 起動コマンド

```bash
# 起動（設定は起動時ログで確認）
cargo run --release -- --config $HIP3_CONFIG

# 起動時に以下のログを確認:
# [INFO] Config loaded: config/testnet.toml
# [INFO] Network: TESTNET (wss://api.hyperliquid-testnet.xyz/ws)
# [INFO] Mode: trading
# [INFO] Signer address: 0x1234...
# [INFO] Safety guards: testnet_only=true, max_notional=100 USD
```

##### 5. 停止条件

| 条件 | アクション |
|------|-----------|
| 累積損失 > $20 | 自動停止 + アラート |
| 連続失敗 > 5 回 | 自動停止 + 手動確認 |
| Flatten 失敗 > 3 回 | 自動停止 + 手動介入 |
| 手動停止 | `Ctrl+C` → graceful shutdown（全ポジション Flatten 後に終了） |

---

#### Testnet検証項目（詳細）

##### #1 WS接続・購読

| 項目 | 内容 |
|------|------|
| **手順** | (1) 起動して WS 接続を確認 (2) orderUpdates/userFills 購読を送信 (3) isSnapshot=true を受信 |
| **期待ログ** | `[INFO] WS connected to wss://...testnet...` / `[INFO] Subscribed: orderUpdates` / `[INFO] Subscribed: userFills` / `[INFO] Received orderUpdates snapshot` |
| **合否** | isSnapshot を受信し、READY-TRADING 状態に遷移すれば **PASS** |

##### #2 署名検証

| 項目 | 内容 |
|------|------|
| **手順** | (1) 手動で 1 件の new_order を発行（CLI or テストコード） (2) post 応答を確認 |
| **期待ログ** | `[DEBUG] Action signed: nonce=..., action_hash=...` / `[INFO] Post response: Ok` |
| **合否** | post 応答が `Ok` なら **PASS**。`Err(reason)` なら reason を記録して **FAIL** |

##### #3 IOC発注

| 項目 | 内容 |
|------|------|
| **手順** | (1) BTC 市場で IOC buy $10 を発注 (2) orderUpdates を監視 |
| **期待ログ** | `[INFO] Order enqueued: cloid=...` / `[INFO] orderUpdates: status=open, oid=...` |
| **合否** | orderUpdates で `open` または `filled` を受信すれば **PASS** |

##### #4 約定確認

| 項目 | 内容 |
|------|------|
| **手順** | (1) #3 の注文が約定するのを待つ (2) userFills を監視 |
| **期待ログ** | `[INFO] userFills received: cloid=..., price=..., size=...` / `[INFO] Position updated: BTC size=...` |
| **合否** | userFills を受信し、Position が更新されれば **PASS** |

##### #5 ポジション同期

| 項目 | 内容 |
|------|------|
| **手順** | (1) clearinghouseState API でポジションを取得 (2) PositionTracker の内部状態と比較 |
| **期待ログ** | `[DEBUG] Position reconcile: tracker=..., exchange=..., diff=0` |
| **合否** | 差分が 0（または許容誤差内）なら **PASS** |

##### #6 フラット化（reduce-only）

| 項目 | 内容 |
|------|------|
| **手順** | (1) ポジションを持った状態で Flatten を発火 (2) reduce_only IOC が優先キューで送信されることを確認 |
| **期待ログ** | `[INFO] Flatten triggered: market=BTC` / `[INFO] reduce_only enqueued (priority)` / `[INFO] Position closed: BTC size=0` |
| **合否** | ポジションが 0 になれば **PASS**。60 秒以内に Flatten 完了しなければ **FAIL (CRITICAL)** |

##### #7 TimeStop

| 項目 | 内容 |
|------|------|
| **手順** | (1) TimeStop 閾値を短く設定（例: 30 秒） (2) ポジションを保持して閾値経過を待つ (3) 自動 Flatten を確認 |
| **期待ログ** | `[WARN] TimeStop triggered: market=BTC, age=30001ms` / `[INFO] Flatten triggered: market=BTC` |
| **合否** | TimeStop 発火後に Flatten が完了すれば **PASS**。Flatten 失敗は **FAIL (CRITICAL)** |

##### #8 nonce 連続性

| 項目 | 内容 |
|------|------|
| **手順** | (1) 連続で N 回（例: 20 回）tick を回す (2) 各 tick の nonce をログで確認 |
| **期待ログ** | `[DEBUG] Nonce issued: 1705000001` / `[DEBUG] Nonce issued: 1705000002` / ...（単調増加） |
| **合否** | 全 nonce が単調増加（重複・逆行なし）なら **PASS** |

##### #9 レート制限・inflight

| 項目 | 内容 |
|------|------|
| **手順** | (1) 高頻度でシグナルを送り、inflight 上限（80/100）付近まで負荷をかける (2) `on_batch_sent`/`on_batch_complete` のカウントを監視 |
| **期待ログ** | `[DEBUG] Inflight: 78/100` / `[WARN] Inflight high watermark reached: 80` / `[DEBUG] on_batch_complete: inflight=77` |
| **メトリクス** | `inflight_current`, `batch_sent_total`, `batch_complete_total` |
| **合否（運転中）** | `batch_sent_total - batch_complete_total == inflight_current` が常に成立（ドリフトなし）なら **PASS** |
| **合否（終了時）** | キュー枯渇後に `inflight_current == 0` かつ `batch_sent_total == batch_complete_total` なら **PASS** |

##### #10 エラー処理・リトライ（3.4 整合）

| 項目 | 内容 |
|------|------|
| **手順** | (1) 送信失敗/タイムアウトを意図的に発生させる（例: WS 切断をシミュレート） (2) reduce_only/cancel の再キュー動作を確認 (3) Rejected 応答時の cleanup を確認 |
| **期待ログ（送信失敗）** | `[WARN] Send failure: requeue reduce_only` / `[INFO] Retry succeeded: cloid=...` |
| **期待ログ（Rejected）** | `[WARN] Action rejected: reason=...` / `[DEBUG] Cleanup: unmark_pending_market, remove_order` |
| **合否** | (a) reduce_only/cancel は再キューして成功すれば **PASS** (b) Rejected は再キューせず cleanup されれば **PASS** (c) new_order は再キューしない（ログ+アラートのみ）なら **PASS** |

**注意**: 3.4 の方針に基づき、`Rejected` は terminal 扱いで**再キューしない**。
リトライ対象は `reduce_only`/`cancel` の「送信失敗/タイムアウト/切断」のみ。

---

#### 検証結果サマリテンプレート

| # | 検証項目 | 結果 | 備考 |
|---|----------|------|------|
| 1 | WS接続・購読 | PASS/FAIL | |
| 2 | 署名検証 | PASS/FAIL | |
| 3 | IOC発注 | PASS/FAIL | |
| 4 | 約定確認 | PASS/FAIL | |
| 5 | ポジション同期 | PASS/FAIL | |
| 6 | フラット化 | PASS/FAIL | |
| 7 | TimeStop | PASS/FAIL | |
| 8 | nonce | PASS/FAIL | |
| 9 | レート制限 | PASS/FAIL | |
| 10 | エラー処理 | PASS/FAIL | |

**Mainnet 移行条件**: 全項目 PASS、かつ 10-20 トレード完了

---

**目標トレード数**: 10-20トレード

**タスク**:
- [ ] Testnet 設定ファイル作成（`config/testnet.toml`）
- [ ] Trading key の準備と address 確認
- [ ] 安全装置の設定確認
- [ ] #1〜#10 の検証実施
- [ ] 検証結果サマリ作成
- [ ] 問題点の修正
- [ ] Mainnet 移行判定

### 3.6 Week 4: Mainnet超小口テスト

#### Mainnet検証パラメータ

| パラメータ | 値 |
|-----------|-----|
| 対象市場 | SNDK-PERP（`asset_idx = 28`, `coin = "SNDK"`）※Phase A 完了後に最終決定 |
| 注文サイズ | $10-50/注文 |
| 最大ポジション | $50 |
| 目標トレード数 | 100 |
| 監視期間 | 1週間 |

#### Mainnet 起動 Runbook

##### 1. 設定ファイル確認

```toml
# config/mainnet_micro.toml

# トップレベル設定（既存実装と整合）
ws_url = "wss://api.hyperliquid.xyz/ws"
info_url = "https://api.hyperliquid.xyz/info"
mode = "trading"

# 対象市場（SNDK-PERP のみ）※Phase A 完了後に最終決定
[[markets]]
asset_idx = 28
coin = "SNDK"

# 署名鍵設定
[signer]
key_source = { type = "env_var", var_name = "HIP3_MAINNET_PRIVATE_KEY" }

# 安全装置
[safety]
mainnet = true
max_notional_usd = 50
allowed_markets = ["SNDK"]
max_daily_trades = 100
auto_stop_loss_usd = 20
```

##### 2. Trading Key 供給

```bash
# 環境変数で供給（mainnet 用ウォレット）
export HIP3_MAINNET_PRIVATE_KEY="0x..."

# 鍵の address は起動時ログで確認（mainnet wallet と一致するか）
# 起動時に [INFO] Signer address: 0xABCD... が出力される
# ⚠️ Testnet wallet と混同しないこと（address が異なることを確認）
```

##### 3. Preflight チェック

```bash
# 1. perpDexs から対象市場の存在を確認（SNDK = asset_idx 28）
curl -s https://api.hyperliquid.xyz/info -d '{"type":"meta"}' | jq '.universe[28]'
# 期待出力: {"name":"SNDK","szDecimals":...}
# 出力が空 or name が異なれば中止（asset_idx は Phase A 完了後に最終確認）

# 2. 残高確認（十分な margin があること）
curl -s https://api.hyperliquid.xyz/info -d '{"type":"clearinghouseState","user":"0xABCD..."}' | jq '.marginSummary'
```

##### 4. 起動コマンド

```bash
export HIP3_CONFIG=config/mainnet_micro.toml

# 起動
cargo run --release -- --config $HIP3_CONFIG

# 起動時に以下のログを確認:
# [INFO] Config loaded: config/mainnet_micro.toml
# [INFO] Network: MAINNET (wss://api.hyperliquid.xyz/ws)
# [INFO] Mode: trading
# [INFO] Signer address: 0xABCD...
# [INFO] Allowed markets: ["SNDK"]
# [INFO] Safety guards: mainnet=true, max_notional=50 USD
```

##### 5. 緊急停止手順（4.2 の具体化）

| 手順 | 実行者 | 方法 |
|------|--------|------|
| **1. 即時停止** | オペレータ | `Ctrl+C` でプロセス終了（graceful shutdown） |
| **2. 強制停止** | オペレータ | `kill -9 <pid>`（graceful shutdown 失敗時） |
| **3. 手動 Flatten** | オペレータ | Hyperliquid Web UI で全ポジションを market close |
| **4. 手動キャンセル** | オペレータ | Hyperliquid Web UI で全 open orders をキャンセル |
| **5. 状態確認** | オペレータ | `clearinghouseState` API で position=0 を確認 |

**緊急連絡**: 停止後に Slack/Discord で状況報告（タイムスタンプ、停止理由、ポジション状態）

#### 成果物メトリクス

| メトリクス | 定義 | 収集方法 |
|-----------|------|----------|
| `expected_edge_bps` | シグナル時点のedge | ログ: `[INFO] Signal: expected_edge=...` |
| `actual_edge_bps` | 実約定ベースのedge | DB: `fills` テーブルから計算 |
| `slippage_bps` | expected - actual | 上記2つから算出 |
| `fill_rate` | accepted / (accepted + rejected + timeout) | メトリクス: `order_accepted_total`, `order_rejected_total`, `order_timeout_total` |
| `flat_time_ms` | エントリー→フラット完了 | ログ: `[INFO] Flat completed: duration_ms=...` |
| `pnl_per_trade` | 1トレードあたりPnL | DB: `trades` テーブルの `realized_pnl` |
| `pnl_cumulative` | 累積PnL | DB: `SUM(realized_pnl)` |

#### 合否基準（Go/No-Go）

##### チェックポイント別判定

| トレード数 | 継続条件 | 停止条件 | アクション |
|------------|----------|----------|-----------|
| **10** | `fill_rate >= 0.8` AND `slippage_bps < 20` | 上記未達 | パラメータ見直し後に再開 |
| **50** | `pnl_cumulative >= -$5` AND `fill_rate >= 0.7` | `pnl_cumulative < -$10` OR `fill_rate < 0.5` | Phase A に戻る |
| **100** | `actual_edge_bps > 0` | `actual_edge_bps <= 0` | 長期分析後に判断 |

##### 即時停止条件（チェックポイント間でも適用）

| 条件 | 閾値 | アクション |
|------|------|-----------|
| 累積損失 | > $20 | 自動停止 + アラート |
| 連続損失 | > 5 回 | 自動停止 + アラート |
| FlattenFailed | > 3 回 | 自動停止 + 手動介入 |
| Rejected 多発 | > 10 回/時間 | 一時停止 + 原因調査 |
| slippage 異常 | > 50 bps 連続 3 回 | 一時停止 + 流動性確認 |

##### edge 算出方法

```sql
-- expected_edge_bps: シグナルログから抽出
-- actual_edge_bps: fills から計算
WITH edge_calc AS (
    SELECT
        expected_edge_bps,
        CASE WHEN side = 'buy'
            THEN (exit_price - entry_price) / entry_price * 10000
            ELSE (entry_price - exit_price) / entry_price * 10000
        END AS actual_edge_bps
    FROM trades
    WHERE created_at >= NOW() - INTERVAL '1 day'
)
SELECT
    AVG(expected_edge_bps) AS avg_expected,
    AVG(actual_edge_bps) AS avg_actual,
    AVG(expected_edge_bps) - AVG(actual_edge_bps) AS avg_slippage
FROM edge_calc;
```

**タスク**:
- [ ] Mainnet 設定ファイル作成（`config/mainnet_micro.toml`）
- [ ] Preflight チェック実行
- [ ] 超小口テスト開始
- [ ] 10 トレード時点で Go/No-Go 判定
- [ ] 50 トレード時点で Go/No-Go 判定
- [ ] メトリクス収集・日次レビュー
- [ ] 100 トレード達成・最終分析

---

## 4. リスク管理

### 4.1 Phase B固有のRisk Gate

#### Gate 一覧

| Gate | 条件 | アクション | 実装位置 |
|------|------|-----------|----------|
| MaxPositionPerMarket | `notional(market) >= MAX_NOTIONAL_PER_MARKET` | 新規禁止 | `Executor::on_signal()` 同期gate |
| MaxPositionTotal | `Σ notional(all) >= MAX_NOTIONAL_TOTAL` | 新規禁止 | `Executor::on_signal()` 同期gate |
| PendingOrder | 同一市場に未約定注文あり | 新規禁止 | `try_mark_pending_market()` 原子gate |
| HardStop | `hard_stop_latch == true` | 新規禁止（reduce_only/cancel は通す） | `Executor::on_signal()` 同期gate |
| FlattenFailed | フラット化60秒超過 | アラート + HardStop発火 | `PositionTrackerTask` |

#### MaxPosition 定義

| 項目 | 仕様 |
|------|------|
| **per-market 上限** | `MAX_NOTIONAL_PER_MARKET = $50`（1.2 参照） |
| **total 上限** | `MAX_NOTIONAL_TOTAL = $100`（1.2 参照） |
| **notional 算出** | `notional = abs(size) × mark_px`（各市場の mark price で評価） |
| **mark_px 取得元** | `MarketState`（WS subscription で更新、`Executor` が `Arc<MarketStateCache>` を保持） |
| **pending 含有** | **含む**（`position_size + pending_order_size` の合計で判定、**reduce_only pending は除外**） |
| **reduce_only** | **gate 除外**（常に通す、ポジション解消は最優先）、**notional 計算でも除外** |
| **USD 換算** | USDC建てのため換算不要（将来の非USD建て市場は `oracle_px` で換算） |

```rust
/// MaxPosition gate の実装（Executor::on_signal() 内）
///
/// mark_px 取得: MarketStateCache から各市場の最新 mark を取得
/// pending notional: reduce_only pending は除外（解消中の注文はカウントしない）
fn check_max_position(&self, market: &MarketKey, order_size: Size) -> Result<(), RejectReason> {
    // この市場の mark price を MarketState から取得
    let mark_px = self.market_state_cache.get_mark_price(market)
        .ok_or(RejectReason::NotReady)?;  // mark 未取得なら準備未完了

    // 現在ポジション + pending（reduce_only 除外） + 今回の注文
    let current_notional = self.position_tracker.get_notional(market, mark_px);
    let pending_notional = self.position_tracker.get_pending_notional_excluding_reduce_only(market, mark_px);
    let order_notional = order_size.abs() * mark_px;
    let total_market_notional = current_notional + pending_notional + order_notional;

    // per-market チェック
    if total_market_notional > self.config.max_notional_per_market {
        return Err(RejectReason::MaxPositionPerMarket);
    }

    // total チェック（全市場の notional を各市場の mark_px で計算）
    let total_notional = self.calculate_total_notional()? + order_notional;
    if total_notional > self.config.max_notional_total {
        return Err(RejectReason::MaxPositionTotal);
    }

    Ok(())
}

/// 全市場の notional 合計を計算
///
/// 各市場の (position + pending_excluding_reduce_only) × mark_px を合算
fn calculate_total_notional(&self) -> Result<Decimal, RejectReason> {
    let mut total = Decimal::ZERO;

    // position_tracker から全市場の position を取得
    let positions = self.position_tracker.get_all_positions();
    for (market, position) in &positions {
        // 各市場の mark_px を取得
        let mark_px = self.market_state_cache.get_mark_price(market)
            .ok_or(RejectReason::NotReady)?;

        let pos_notional = position.size.abs() * mark_px;
        let pending_notional = self.position_tracker
            .get_pending_notional_excluding_reduce_only(market, mark_px);

        total += pos_notional + pending_notional;
    }

    // pending があるが position がない市場も考慮
    let pending_markets = self.position_tracker.get_markets_with_pending_orders();
    for market in pending_markets {
        if positions.contains_key(&market) {
            continue;  // 既に上で計算済み
        }

        let mark_px = self.market_state_cache.get_mark_price(&market)
            .ok_or(RejectReason::NotReady)?;

        let pending_notional = self.position_tracker
            .get_pending_notional_excluding_reduce_only(&market, mark_px);

        total += pending_notional;
    }

    Ok(total)
}
```

### 4.2 緊急停止（HardStop）

#### 自動停止トリガー（3.6 即時停止条件と統一）

| トリガー | 閾値 | スコープ |
|----------|------|----------|
| 累積損失 | > $20 | Mainnet/Testnet 共通 |
| 連続損失 | > 5 回 | Mainnet/Testnet 共通 |
| FlattenFailed | > 3 回 | Mainnet/Testnet 共通 |
| Rejected 多発 | > 10 回/時間 | Mainnet 専用 |
| slippage 異常 | > 50 bps 連続 3 回 | Mainnet 専用 |

#### HardStop latch

```rust
/// HardStop: 発火後は新規発注を完全停止
pub struct HardStopLatch {
    triggered: AtomicBool,
    trigger_reason: Mutex<Option<String>>,
    trigger_time: Mutex<Option<Instant>>,
}

impl HardStopLatch {
    /// HardStop を発火（一度発火すると手動リセットまで解除不可）
    pub fn trigger(&self, reason: &str) {
        if !self.triggered.swap(true, Ordering::SeqCst) {
            *self.trigger_reason.lock() = Some(reason.to_string());
            *self.trigger_time.lock() = Some(Instant::now());
            tracing::error!(reason, "🛑 HARD STOP TRIGGERED");
        }
    }

    pub fn is_triggered(&self) -> bool {
        self.triggered.load(Ordering::SeqCst)
    }

    /// 手動リセット（運用確認後のみ）
    pub fn reset(&self) {
        self.triggered.store(false, Ordering::SeqCst);
        *self.trigger_reason.lock() = None;
        *self.trigger_time.lock() = None;
        tracing::warn!("HardStop reset by operator");
    }
}
```

**HardStop 発火後の動作**:
- `new_order`: **拒否**（`RejectReason::HardStop`）
- `reduce_only`: **許可**（ポジション解消は最優先）
- `cancel`: **許可**（未約定注文の解消）

#### 停止シーケンス（責務分解）

| ステップ | 責務 | 実行者 | 完了条件 |
|----------|------|--------|----------|
| 1. HardStop 発火 | `hard_stop_latch.trigger(reason)` | `RiskMonitor` | latch == true |
| 2. 新規発注停止 | `on_signal()` で `HardStop` gate が弾く | `Executor` | 自動（gate通過不可） |
| 3. 全 cancel enqueue | `pending_orders` を走査して cancel 生成 | `Executor::on_hard_stop()` | cancel が全て enqueue |
| 4. 全 flatten 発火 | `positions` を走査して reduce_only 生成 | `Flattener::flatten_all()` | reduce_only が全て enqueue |
| 5. 完了待機 | `pending_orders.is_empty() && positions.is_empty()` | `Executor` | position=0, pending=0 |
| 6. アラート送信 | Slack/Discord 通知 | `AlertService` | 通知完了 |
| 7. 手動確認待ち | ログ/メトリクス確認、原因調査 | オペレータ | `HardStopLatch::reset()` |

**WS 切断中の扱い**:
- 切断検知時に `HardStop` を発火（安全側に倒す）
- 再接続後、`clearinghouseState` で position/pending を同期してから flatten 再試行
- 再接続しても position が残っていれば Hyperliquid Web UI で手動 flatten

```rust
/// on_hard_stop: HardStop 発火時の処理
impl Executor {
    pub async fn on_hard_stop(&self, reason: &str) {
        self.hard_stop_latch.trigger(reason);

        // 0. new_order キューを purge（HardStop 後に送信されないように）
        //    drop された (cloid, market) の pending_markets_cache/pending_orders を cleanup
        //    NOTE: market は PendingOrder から直接取得（pending_orders_snapshot に依存しない）
        let dropped = self.batch_scheduler.drop_new_orders();
        for (cloid, market) in &dropped {
            // pending_markets_cache を解除（market は drop_new_orders() から取得済み）
            self.position_tracker.unmark_pending_market(market);
            // pending_orders から削除
            self.position_tracker.remove_order(cloid.clone()).await;
        }
        if !dropped.is_empty() {
            tracing::warn!(count = dropped.len(), "Dropped new_orders on HardStop");
        }

        // 1. 全 pending order をキャンセル（既に送信済みの注文）
        let pending_cloids = self.position_tracker.get_all_pending_cloids();
        for cloid in pending_cloids {
            let cancel = PendingCancel::new(cloid);
            self.batch_scheduler.enqueue_cancel(cancel);
        }

        // 2. 全 position を flatten
        let positions = self.position_tracker.get_all_positions();
        for (market, position) in positions {
            if let Some(reduce_only) = self.flattener.build_flatten_order(&market, &position) {
                self.batch_scheduler.enqueue_reduce_only(reduce_only);
            }
        }

        // 3. アラート送信
        self.alert_service.send_hard_stop_alert(reason).await;
    }
}
```

#### RiskMonitor アクター

`RiskMonitor` は独立タスクとして動作し、`ExecutionEvent` ストリームを監視して HardStop トリガー条件を判定する。

**入力**: `ExecutionEvent` ストリーム（`mpsc::Receiver<ExecutionEvent>`）

```rust
/// 実行イベント（PositionTracker/ExecutorLoop から送信）
pub enum ExecutionEvent {
    /// 約定（fills_snapshot または orderUpdates の fill）
    Fill {
        market: MarketKey,
        cloid: ClientOrderId,
        price: Price,
        size: Size,
        pnl: Option<Decimal>,  // realized PnL（close 時のみ）
    },
    /// ポジションクローズ完了
    PositionClosed {
        market: MarketKey,
        realized_pnl: Decimal,
    },
    /// Flatten 失敗
    FlattenFailed {
        market: MarketKey,
        reason: String,
    },
    /// Post 拒否（PostResult::Rejected）
    Rejected {
        cloid: ClientOrderId,
        reason: String,
    },
    /// slippage 計測（expected vs actual）
    SlippageMeasured {
        market: MarketKey,
        expected_edge_bps: f64,
        actual_edge_bps: f64,
    },
}
```

**RiskMonitor 構造体**:

```rust
pub struct RiskMonitor {
    event_rx: mpsc::Receiver<ExecutionEvent>,
    hard_stop_latch: Arc<HardStopLatch>,
    executor_handle: ExecutorHandle,  // on_hard_stop() 呼び出し用

    // カウンタ
    cumulative_pnl: Decimal,
    consecutive_losses: u32,
    flatten_failed_count: u32,
    rejected_count_hourly: u32,
    rejected_reset_time: Instant,
    slippage_history: VecDeque<f64>,  // 直近 N 回の slippage

    // 閾値（config から）
    config: RiskMonitorConfig,
}

pub struct RiskMonitorConfig {
    pub max_cumulative_loss: Decimal,      // $20
    pub max_consecutive_losses: u32,       // 5
    pub max_flatten_failed: u32,           // 3
    pub max_rejected_per_hour: u32,        // 10
    pub max_slippage_bps: f64,             // 50
    pub slippage_consecutive_threshold: u32, // 3
}
```

**RiskMonitor タスク**:

```rust
impl RiskMonitor {
    pub async fn run(mut self) {
        while let Some(event) = self.event_rx.recv().await {
            if let Some(reason) = self.process_event(event) {
                // HardStop 発火
                self.executor_handle.on_hard_stop(&reason).await;
                // 発火後もイベントは受信し続ける（ログ/メトリクス用）
            }
        }
    }

    fn process_event(&mut self, event: ExecutionEvent) -> Option<String> {
        match event {
            ExecutionEvent::PositionClosed { realized_pnl, .. } => {
                self.cumulative_pnl += realized_pnl;

                if realized_pnl < Decimal::ZERO {
                    self.consecutive_losses += 1;
                } else {
                    self.consecutive_losses = 0;
                }

                // 閾値チェック
                if self.cumulative_pnl < -self.config.max_cumulative_loss {
                    return Some(format!(
                        "Cumulative loss exceeded: {}",
                        self.cumulative_pnl
                    ));
                }
                if self.consecutive_losses > self.config.max_consecutive_losses {
                    return Some(format!(
                        "Consecutive losses exceeded: {}",
                        self.consecutive_losses
                    ));
                }
            }

            ExecutionEvent::FlattenFailed { market, reason } => {
                self.flatten_failed_count += 1;
                tracing::error!(?market, reason, "Flatten failed");

                if self.flatten_failed_count > self.config.max_flatten_failed {
                    return Some(format!(
                        "Flatten failed count exceeded: {}",
                        self.flatten_failed_count
                    ));
                }
            }

            ExecutionEvent::Rejected { cloid, reason } => {
                // 1時間でリセット
                if self.rejected_reset_time.elapsed() > Duration::from_secs(3600) {
                    self.rejected_count_hourly = 0;
                    self.rejected_reset_time = Instant::now();
                }

                self.rejected_count_hourly += 1;
                tracing::warn!(?cloid, reason, "Order rejected");

                if self.rejected_count_hourly > self.config.max_rejected_per_hour {
                    return Some(format!(
                        "Rejected count exceeded: {}/hour",
                        self.rejected_count_hourly
                    ));
                }
            }

            ExecutionEvent::SlippageMeasured { expected_edge_bps, actual_edge_bps, .. } => {
                let slippage = expected_edge_bps - actual_edge_bps;
                self.slippage_history.push_back(slippage);

                // 直近 N 回のみ保持
                while self.slippage_history.len() > 10 {
                    self.slippage_history.pop_front();
                }

                // 連続して閾値超過かチェック
                let consecutive_high = self.slippage_history
                    .iter()
                    .rev()
                    .take(self.config.slippage_consecutive_threshold as usize)
                    .all(|&s| s > self.config.max_slippage_bps);

                if consecutive_high && self.slippage_history.len() >= self.config.slippage_consecutive_threshold as usize {
                    return Some(format!(
                        "Slippage exceeded {}bps for {} consecutive trades",
                        self.config.max_slippage_bps,
                        self.config.slippage_consecutive_threshold
                    ));
                }
            }

            _ => {}
        }

        None  // 閾値未達、HardStop 不要
    }
}
```

**WS 切断時の HardStop 発火**:

WS 切断は `RiskMonitor` ではなく `ExecutorLoop` が検知し、直接 HardStop を発火する。

```rust
impl ExecutorLoop {
    async fn on_ws_disconnect(&self, reason: &str) {
        tracing::error!(reason, "WebSocket disconnected");

        // 切断検知時は安全側に倒す
        self.executor.on_hard_stop(&format!("WS disconnect: {}", reason)).await;
    }
}
```

**イベント送信元の責務**:

| イベント | 送信元 | タイミング |
|----------|--------|------------|
| `Fill` | `PositionTrackerTask` | orderUpdates で fill 検知時 |
| `PositionClosed` | `PositionTrackerTask` | position が 0 になった時 |
| `FlattenFailed` | `Flattener` | reduce_only タイムアウト時 |
| `Rejected` | `ExecutorLoop` | `PostResult::Rejected` 受信時 |
| `SlippageMeasured` | `PositionTrackerTask` | ポジションクローズ時に expected/actual を計算 |
| (WS 切断) | `ExecutorLoop` | 切断検知時に直接 `on_hard_stop()` |

### 4.3 ロールバック基準

**注**: 具体的な Go/No-Go 判定は **3.6 のチェックポイント別判定**に統一。4.3 はその参照先を示すのみ。

| 評価タイミング | 参照先 | 概要 |
|----------------|--------|------|
| 10 トレード時点 | 3.6 Go/No-Go | `fill_rate >= 0.8` AND `slippage_bps < 20` |
| 50 トレード時点 | 3.6 Go/No-Go | `pnl_cumulative >= -$5` AND `fill_rate >= 0.7` |
| 100 トレード時点 | 3.6 Go/No-Go | `actual_edge_bps > 0` |
| 重大バグ発見 | 即時 | HardStop 発火 + 修正後に再評価 |

**edge/slippage の定義**（3.6 と統一）:
- `expected_edge_bps`: シグナル時点の理論 edge（ログから抽出）
- `actual_edge_bps`: 実約定ベースの edge（`(exit_price - entry_price) / entry_price × 10000`）
- `slippage_bps`: `expected_edge_bps - actual_edge_bps`
- 評価窓: 直近 N トレード（チェックポイント到達時点の全トレード）

---

## 5. スケジュール

### 5.1 週次マイルストーン

| Week | 目標 | 成果物 |
|------|------|--------|
| 1 | NonceManager + BatchScheduler | 基盤実装完了 |
| 2 | Signer + PositionTracker | 署名・ポジション管理 |
| 3 | Testnet検証 | 10-20トレード成功 |
| 4 | Mainnet超小口 | 100トレード + edge分析 |

### 5.2 日次チェックリスト（Mainnet稼働後）

- [ ] 前日のトレードレビュー
- [ ] PnL確認
- [ ] エラーログ確認
- [ ] メトリクス異常チェック
- [ ] パラメータ調整（必要時）

---

## 6. Phase C移行条件

| 条件 | 基準 |
|------|------|
| トレード数 | 100回以上完了 |
| edge残存 | 手数料+滑り込みでedge正 |
| 停止品質 | 重大な停止漏れなし |
| fill率 | > 80% |
| フラット化成功率 | > 95% |

---

## 7. 実装優先順位（P0タスク）

| ID | タスク | 依存 | 状態 |
|----|--------|------|------|
| P0-19a | NonceManager（0起点禁止） | - | ⏳ |
| P0-19b | BatchScheduler（100ms周期） | P0-19a | ⏳ |
| P0-19c | **InflightTracker / RateLimiter 統合** | P0-19b | ⏳ |
| P0-25 | serverTime同期 | P0-19a | ⏳ |
| P0-11 | セキュリティ/鍵管理 | - | ⏳ |
| P0-29 | ActionBudget制御 | - | ⏳ |

### P0-19c: InflightTracker / RateLimiter 統合

**背景**: 現状 `crates/hip3-ws/src/rate_limiter.rs` が inflight を会計している。
`InflightTracker` を追加すると二重会計になり、ドリフトするとデバッグ不能になる。

**設計決定**: 以下のいずれかを実装時に確定する。

| 方針 | 内容 | メリット | デメリット |
|------|------|----------|-----------|
| **A. RateLimiter を唯一の inflight ソースにする** | InflightTracker を削除、RateLimiter を Arc で共有 | 会計が一元化 | RateLimiter の責務が増加 |
| **B. RateLimiter の inflight 会計を外す** | RateLimiter はレート制限のみ、InflightTracker が inflight 管理 | 責務分離 | 既存コード変更が必要 |
| **C. InflightTracker を RateLimiter への参照にする** | InflightTracker は RateLimiter.inflight() を呼ぶだけのラッパー | 会計が一元化、変更最小 | 層が増える |

**推奨**: **方針 B**（責務分離）

**タスク**:
- [ ] `crates/hip3-ws/src/rate_limiter.rs` の inflight 会計コードを確認
- [ ] 方針を確定（A/B/C）
- [ ] 実装: RateLimiter から inflight 会計を削除 or InflightTracker を削除
- [ ] テスト: inflight が正しく会計されることを確認

---

## 8. 参照ドキュメント

| ドキュメント | パス |
|-------------|------|
| メイン計画 | `.claude/plans/2026-01-18-oracle-dislocation-taker.md` |
| Phase A分析 | `.claude/specs/2026-01-19-phase-a-analysis.md` |
| ロードマップ | `.claude/roadmap.md` |
| Hyperliquid Exchange API | [公式ドキュメント](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint) |
| Nonce制約 | [公式ドキュメント](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/nonces-and-api-wallets) |

---

## 9. 更新履歴

| 日付 | 内容 |
|------|------|
| 2026-01-19 | 初版作成 |
| 2026-01-19 | 3.1 レビュー対応: NonceManager に Clock trait 注入、next() を max(last+1, approx_server_time) に変更、BatchScheduler にバックプレッシャ制御追加 |
| 2026-01-19 | 追加レビュー対応: (1) 1 tick = 1 post = 1 L1 action に確定、(2) cancel にも queue 上限追加、EnqueueResult を 4 種に分離、高水位時は送信 skip、(3) Executor API 整合 - NonceManagerImpl 型エイリアス、on_signal() 同期化、ExecutorLoop 分離でフロー明確化 |
| 2026-01-19 | 再レビュー対応: (1) PostRequestManager + タイムアウト(5s) + 切断時 inflight リセット、(2) 送信失敗時の再キュー方針（reduce_only/cancel は再キュー、new_order は上流通知）、(3) 3キュー構造（cancel > reduce_only > new_order）で優先順位保証、高水位時も reduce_only/cancel は送信 |
| 2026-01-20 | 再々レビュー対応: (1) inflight=100 時は cancel も送れない（None を返す）、(2) tick() は inflight increment しない → 送信成功時に on_batch_sent() で increment、(3) 応答相関キーを nonce から post_id に変更（PostIdGenerator 追加） |
| 2026-01-20 | 追補レビュー対応: (1) PendingRequest に `sent` フラグ追加、register() で false → mark_as_sent() で true、タイムアウト/切断時は sent: true のみ inflight decrement、(2) InflightTracker を明示定義、RateLimiter との責務分離を明記、(3) BatchScheduler::on_disconnect() を削除、切断時の回収は ExecutorLoop で一元管理 |
| 2026-01-20 | 追補2レビュー対応: (1) InflightTracker の increment/decrement を CAS ループで安全に実装（underflow/overflow 防止）、(2) PostRequestManager::on_disconnect() の drain() の分解を DashMap の正しい返り値に修正、(3) RateLimiter との inflight 会計統合を P0-19c タスクとして追加（方針 B: 責務分離を推奨） |
| 2026-01-20 | **3.2 Signer レビュー対応**: (1) API を `sign_action()` に統一、post_id は署名対象外（WsSender 層で付与）、(2) 署名仕様を固定（Action JSON スキーマ、SigningPayload、hash 手順）、golden test 方針追加、(3) 鍵管理を追加（KeyManager、KeySource、address 検証、zeroize、API wallet 分離） |
| 2026-01-20 | **3.2 Signer 再レビュー対応**: (1) ExecutorLoop の旧 API `sign_action(&batch, nonce, post_id)` を新 API `build_and_sign(&batch, nonce)` に統一、WsSender.post() に post_id を別引数で渡す形に変更、(2) 署名仕様から "EIP-712" 文言を削除、SDK 準拠の msgpack + keccak256 形式を明確化、(3) Golden test を環境非依存に修正 - `from_bytes()` で直接鍵注入、`with_timestamp()` で timestamp 固定、Python スクリプトで期待値を事前計算するフローを明記、プレースホルダーを `_REPLACE_WITH_PYTHON_SDK_OUTPUT_` 形式に変更 |
| 2026-01-20 | **3.2 Signer 追補レビュー対応**: (1) `connection_id` 定数を SDK 実値取得フローで確定（プレースホルダー形式に変更、取得方法を明記）、(2) msgpack 互換性を強化 - Action 構造体の `Option<T>` フィールドに `skip_serializing_if = "Option::is_none"` を追加、`rmp_serde::to_vec_named` の設定説明を追記、(3) `timestamp_ms` を SigningPayload と Signer から削除（SDK 準拠 - 署名対象に含まれないため） |
| 2026-01-20 | **3.2 Signer SDKソース突合対応（大幅改訂）**: (1) 署名方式を **2段階構造に全面改訂** - action_hash 計算（`keccak(msgpack || nonce_be || vault_tag || expires_tag)`）→ phantom_agent EIP-712 署名（domain: chainId=1337, name="Exchange"）、(2) `MAINNET/TESTNET_CONNECTION_ID` 定数を**削除**（静的定数ではなく action_hash から動的生成）、(3) nonce エンディアンを **big-endian** に修正（`to_be_bytes()`）、(4) vault_address=None でも **0x00 タグ 1 byte** 必須に修正、(5) Action に **grouping フィールド追加**（type=order 時は必須）、(6) Golden test スクリプトを SDK API に合わせて修正（`action_hash()` と `sign_l1_action()` を別々に呼び出し）、(7) SigningPayload → SigningInput に名称変更、hash() → action_hash() に変更、(8) テスト項目を 12 項目に拡充 |
| 2026-01-20 | **3.2 Signer 修正後レビュー対応**: (1) `expires_after` エンコードを SDK 準拠に修正 - **None は何も追加しない**（タグ自体が存在しない）、**Some は 0x00 + 8 bytes**（vault_address と挙動が異なる）、(2) Golden test スクリプトの r/s パディングを修正 - `int(sig["r"],16).to_bytes(32,"big")` で 32 bytes に揃える、(3) **v 表現を明確化** - SDK は 27/28、alloy は 0/1 を使用、変換処理を追記、(4) **API 表を更新** - `sign_action(action, nonce)` → `sign_action(action, nonce, vault_address, expires_after)` に修正、(5) **OrderTypeWire / CancelWire 定義を追加** - SDK wire format 準拠（`{"limit":{"tif":"Ioc"}}`、`{"a":5,"o":123}` など） |
| 2026-01-20 | **3.2 Signer 再修正後レビュー対応**: (1) **`Batch` → `ActionBatch` enum に変更** - SDK 仕様準拠で **orders と cancels は別々の action として送信**（同居しない）、`tick()` は cancel 優先で 1 action type のみ返す、(2) **`TriggerOrderType` を Phase B スコープ外（未対応）と明記** - フィールド順を SDK に合わせて修正（isMarket→triggerPx→tpsl）、(3) **3.4 フロー図を更新** - `signer.sign_action(batch, nonce)` → `signer.build_and_sign(&batch, nonce)` に修正、(4) `handle_send_failure()` を `ActionBatch` 対応に修正、テスト項目を 12 項目に拡充 |
| 2026-01-20 | **3.2 Signer 再々修正後レビュー対応（型整合）**: (1) **`PostRequestManager` を `ActionBatch` に統一** - `PendingRequest.batch`、`register()`、`on_response()`、`check_timeouts()`、`on_disconnect()` の全 API で `Batch` → `ActionBatch` に変更、(2) **3.1 バッチ単位説明を修正** - 「複数 orders/cancels をまとめる」→「複数 orders **または** cancels をまとめる、**同居しない**」、(3) **3.2 テスト/タスク文言を修正** - `Batch→Action 変換` → `ActionBatch→Action 変換` に変更 |
| 2026-01-20 | **3.2 Signer 型整合修正後レビュー対応**: (1) **WS wire payload スキーマを追加** - `ActionWirePayload` / `SignatureWire` 構造体を定義、`v` 変換（0/1→27/28）の責務を WsSender 層に明確化、`to_wire_payload()` メソッド追加、(2) **KeySource::File フォーマットを明確化** - hex 文字列形式（`0x` prefix 許容、改行 trim）を断定、生バイナリは非対応、EnvVar/File で共通パーサ使用、(3) **3.1 高水位説明を ActionBatch 仕様に更新** - tick() の ActionBatch 返却ルールを明記（cancel あり→CancelBatch / なし→OrderBatch）、同一 tick で同居しないことを強調 |
| 2026-01-20 | **3.2 OrderBuilder レビュー対応**: (1) **型の責務分離を明確化** - `PendingOrder`（内部表現）と `OrderWire`（wire format）を分離、`PendingOrder::to_wire()` で変換、(2) **`format_price(price, is_buy)` に対応** - hip3-core の API に合わせて `is_buy` 引数を追加（丸め方向決定）、(3) **cloid 生成規約を断定** - `ClientOrderId` 型を使用、生成タイミング・保持場所・再キュー/再送時の同一維持を明記、`post_id` との独立性を強調、(4) **PositionTracker.pending_orders を `ClientOrderId` キーに変更** - 型安全性向上 |
| 2026-01-20 | **3.2 OrderBuilder 再レビュー対応**: (1) **`build_and_sign` API 統一** - Signer 節の `build_and_sign(batch, nonce, market_specs)` に一本化、OrderBuilder 節の重複定義を削除、3.4 フロー図/ExecutorLoop 疑似コードも更新、(2) **`OrderTypeWire` コンストラクタ名統一** - `ioc()` / `gtc()` に統一（`limit_ioc()` / `limit_gtc()` を削除）、(3) **`Side` → `OrderSide` に統一** - `hip3_core::OrderSide` を使用、Flatten も `PendingOrder` 返却 + `ClientOrderId` 引数に更新、(4) **Executor に `market_specs` 追加** - `PendingOrder → OrderWire` 変換用 |
| 2026-01-20 | **3.3 hip3-position レビュー対応**: (1) **循環依存回避** - 共有型（`PendingOrder`, `PendingCancel`, `ActionBatch`, `ClientOrderId` 等）を `hip3-core` に配置、`hip3-executor` と `hip3-position` は両方 `hip3-core` を参照（直接依存なし）、(2) **Position 型を Price/Size に統一** - `size: Size`, `entry_price: Price`, `unrealized_pnl: Price` に変更、OrderBuilder との一貫性確保、(3) **Flattener から `position.spec` 参照を削除** - `market_specs: Arc<DashMap>` を共有、MarketSpec は Flattener 側で取得、(4) **pending_orders ライフサイクル・整合ルールを追加** - 登録/状態遷移/削除タイミング、再起動時の復元（`TrackedOrder` 構造体）、isSnapshot 前のバッファリング方針を明記 |
| 2026-01-20 | **3.3 hip3-position 再レビュー対応**: (1) **`pending_orders` を `TrackedOrder` に統一** - `HashMap<ClientOrderId, TrackedOrder>` に一本化、`PendingOrder` は BatchScheduler キュー内のみ使用、enqueue 時に `TrackedOrder::from_pending()` で変換、`exchange_oid`/`status`/`filled_size` を TrackedOrder で管理、(2) **actor 方式を採用（スレッド安全）** - `PositionTrackerTask` + mpsc でメッセージ駆動、内部状態は単一タスクで管理（Mutex 不要）、`PositionTrackerHandle` で外部 API 提供、(3) **`entry_time: Instant` → `entry_timestamp_ms: u64` に変更** - fills 由来で復元可能、`Position::age_ms(now_ms)` で経過時間計算、TimeStop も `now_ms` を外部注入（テスト容易）、再起動後も TimeStop 継続動作 |
| 2026-01-20 | **3.3 hip3-position 再々レビュー対応**: (1) **`PendingOrder.side: OrderSide` に変更** - `is_buy: bool` から `side: OrderSide` に変更、`to_wire()` で `is_buy` を導出、`TrackedOrder::from_pending()` との整合を確保、(2) **`register_order()` API を追加** - `PositionTrackerMsg::RegisterOrder(TrackedOrder)` バリアントを追加、`PositionTrackerHandle::register_order()` を実装、(3) **3.4 Executor を actor handle 方式に更新** - `position_tracker: PositionTrackerHandle` に変更、`has_position()` は同期キャッシュ（`positions_cache: Arc<DashMap>`）から即座に返す、`register_order()` は `tokio::spawn` で fire-and-forget |
| 2026-01-20 | **3.4 統合レビュー対応**: (1) **循環依存回避の方針を統一** - `PositionQuery` trait object 前提を削除、`hip3-executor` が `hip3-position` に直接依存して `PositionTrackerHandle` を使用する設計に統一、依存関係図を更新、(2) **READY-TRADING ゲートを実行フローに接続** - `TradingReadyChecker` を AtomicBool + watch channel で実装、`on_signal()` で `is_ready()` チェック追加、各条件（md_ready/order_snapshot/fills_snapshot/position_synced）の設定タイミングを明記、`PositionTrackerTask` から snapshot 適用完了を通知する仕組み追加、(3) **PendingOrder Gate を追加（race 回避）** - `PositionTrackerHandle` に `pending_markets_cache: Arc<DashMap>` 追加、`has_pending_order()` / `mark_pending_market()` / `unmark_pending_market()` を実装、`on_signal()` で enqueue **前に** 同期的にキャッシュ更新（race 回避）、enqueue 失敗時は `unmark_pending_market()` でロールバック、(4) **SkipReason/RejectReason を拡張** - `SkipReason::PendingOrderExists`、`RejectReason::NotReady` を追加 |
| 2026-01-20 | **3.4 統合 再レビュー対応**: (1) **`register_order()` の順序を修正（リーク防止）** - enqueue **成功後**に `try_register_order()` で同期的に送信（`tokio::spawn` をやめて `try_send()` 使用）、enqueue 失敗時は actor に TrackedOrder が送られないため `unmark_pending_market()` だけで済む、(2) **PendingOrder Gate の解除フローを追加** - `PositionTrackerTask` が orderUpdates の terminal 状態（filled/canceled/rejected）で `decrement_pending_market()` を呼ぶ、`handle_send_failure()` で new_order を落とす場合に `unmark_pending_market()` + `RemoveOrder(cloid)` で pending 解除、`PositionTrackerMsg::RemoveOrder` と `PositionTrackerHandle::remove_order()` を追加、(3) **`wait_ready(&mut self)` を削除** - `Arc<TradingReadyChecker>` との整合のため削除、代わりに `subscribe()` で `watch::Receiver` を取得し、オーケストレータ側で `mut rx` を待つ形に統一、起動時の待機コード例を追加 |
| 2026-01-21 | **3.4 統合 再々レビュー対応**: (1) **`try_register_order()` 失敗時のフォールバック追加** - `try_send()` が `TrySendError` で失敗した場合、`tokio::spawn` でフォールバック送信（登録は必ず届ける、enqueue 後なのでリーク無し）、(2) **PendingOrder Gate を原子化** - `has_pending_order()` + `mark_pending_market()` の非原子的呼び出しを **`try_mark_pending_market()`** に統一、DashMap の entry API で原子的に check + mark を実行（並行 `on_signal()` による二重 enqueue を防止）、(3) **`PostResult::Rejected` で pending 解除を追加** - Rejected は terminal 扱いで `unmark_pending_market()` + `remove_order()` を実行、orderUpdates が来ない可能性を考慮して明示的に cleanup、reduce_only の場合はアラート出力、(4) **pending_orders ライフサイクル例を最終仕様に更新** - mark→enqueue→register の順序、各段階での rollback/fallback 処理を明記 |
| 2026-01-21 | **3.4 統合 再々々レビュー対応**: (1) **「上流通知」記述を削除** - new_order 送信失敗は enqueue 後に発生するため `ExecutionResult::Failed` として返せない、代わりにログ/メトリクス/アラートで対応（`order_send_failure_total` カウンタ）、(2) **`PendingRequest.tx` を `Option<oneshot::Sender>` に変更** - `check_timeouts()` の retain クロージャ内で `tx.send()` するには `take()` で move out が必要、`on_response()`/`on_disconnect()` も `take()` を使用するよう修正、(3) **`ExecutorLoop` の並行性前提を明記** - 1 タスクで `select!` を使って tick と ws メッセージ処理を直列化、`PostRequestManager` への並行アクセスが発生しない設計に統一、メインループ例（`run()` メソッド）を追加 |
| 2026-01-21 | **3.5 Testnet検証 レビュー対応**: (1) **起動 Runbook を追加** - Testnet 設定ファイル確認手順（`HIP3_CONFIG=config/testnet.toml`）、trading mode への切り替え、Trading key 供給方法（EnvVar/File）、安全装置（`testnet_only`/`max_notional_usd`/`allowed_markets`/`max_daily_trades`/`auto_stop_loss_usd`）、起動コマンドと期待ログ、停止条件を明記、(2) **検証項目 #1〜#10 を詳細化** - 各項目に「手順」「期待ログ/メトリクス」「合否基準」を追加、検証結果サマリテンプレートを追加、(3) **#10 を 3.4 方針と整合** - `Rejected` は terminal 扱いで**再キューしない**（cleanup のみ）、リトライ対象は `reduce_only`/`cancel` の「送信失敗/タイムアウト/切断」のみ、new_order は再キューせずログ+アラートで対応 |
| 2026-01-21 | **3.5 Testnet検証 再レビュー対応**: (1) **存在しないコマンドを削除** - `hip3-cli check-key` と `--dry-run` フラグを削除、代わりに起動時ログで `Signer address` を確認する方式に統一（実装タスク不要）、(2) **#9 合否基準を修正** - 運転中は `batch_sent_total - batch_complete_total == inflight_current`（不変条件）、テスト終了時は `inflight_current == 0` かつ `batch_sent_total == batch_complete_total` に修正 |
| 2026-01-21 | **3.6 Mainnet超小口テスト レビュー対応**: (1) **Mainnet 起動 Runbook を追加** - `config/mainnet_micro.toml` 設定例、起動時確認ログ（ws_url/Signer address/allowed_markets/max_notional）、Trading key 供給方法（mainnet wallet 確認手順）、緊急停止手順（Ctrl+C/kill/手動Flatten/手動キャンセル/状態確認）を明記、(2) **対象市場を明確化** - `asset_idx = 28` / `coin = "SNDK"` で SNDK-PERP を指定（Phase A 完了後に最終決定）、Preflight チェック手順（perpDexs API で市場存在確認、残高確認）を追加、(3) **Go/No-Go 合否基準を追加** - 10/50/100 トレード時点のチェックポイント判定（fill_rate/slippage_bps/pnl_cumulative の閾値）、即時停止条件（累積損失/連続損失/FlattenFailed/Rejected多発/slippage異常）、edge 算出 SQL クエリを追加 |
| 2026-01-21 | **3.6 Mainnet超小口テスト 再レビュー対応**: (1) **config スキーマを 3.5 と整合** - `[network]`/`[trading]` セクションを削除し、`ws_url`/`info_url`/`mode` をトップレベルに配置、`[signer] key_source = { type = "env_var", var_name = "HIP3_MAINNET_PRIVATE_KEY" }` を追加、(2) **冒頭/1.3 の初期市場を SNDK に統一** - 冒頭「初期市場: SNDK (xyz:28)」、1.3 表「xyz:28 / SNDK」に変更（Phase A 完了後に最終決定の注釈付き）、(3) **edge 算出 SQL を修正** - `AVG(actual_edge_bps)` を直接参照していたのを CTE で `actual_edge_bps` を事前定義する形に修正 |
| 2026-01-21 | **4. リスク管理 レビュー対応**: (1) **4.1 MaxPosition を詳細定義** - per-market/total 両方を gate 化、notional 算出（`abs(size) × mark_px`）、pending 含む、reduce_only 除外、実装位置（`Executor::on_signal()` 同期 gate）、疑似コード追加、(2) **4.2 HardStop を定義** - トリガー一覧を 3.6 即時停止条件と統一、`HardStopLatch` 構造体（発火後は new_order 拒否、reduce_only/cancel は許可）、停止シーケンスの責務分解（7ステップ）、WS 切断中の扱い、`on_hard_stop()` 疑似コード追加、(3) **4.3 を 3.6 Go/No-Go に統合** - 具体的な判定基準は 3.6 参照、edge/slippage 定義を明記 |
| 2026-01-21 | **4. リスク管理 再レビュー対応**: (1) **3.4 Gate 順序を 4.1 と整合** - Gate 0: HardStop、Gate 2: MaxPosition を追加、`on_signal()` の gate チェック順序を更新（HardStop → READY-TRADING → MaxPosition → has_position → PendingOrder → ActionBudget）、フロー図・`RejectReason` enum（`HardStop`/`MaxPositionPerMarket`/`MaxPositionTotal`）を追加、(2) **MaxPosition notional 計算を修正** - 単一 `mark_px` ではなく `MarketStateCache` から各市場の mark を取得、`calculate_total_notional()` ヘルパー追加（全市場の position + pending を各 mark_px で評価）、(3) **RiskMonitor アクターを定義** - 入力: `ExecutionEvent` ストリーム（Fill/PositionClosed/FlattenFailed/Rejected/SlippageMeasured）、カウンタ更新（cumulative_pnl/consecutive_losses/flatten_failed_count/rejected_count_hourly/slippage_history）、`process_event()` で閾値判定、発火時の呼び出し経路（`executor_handle.on_hard_stop()`）、WS 切断時は `ExecutorLoop` が直接発火 |
| 2026-01-21 | **4. リスク管理 再々レビュー対応**: (1) **HardStop で new_order キュー purge** - `BatchScheduler::drop_new_orders()` 追加、`tick()` で HardStop 中は new_order skip、`on_hard_stop()` で drop した cloid の pending_markets_cache/pending_orders を cleanup、BatchScheduler に `hard_stop_latch` 参照を追加、(2) **Executor 構造体に 4章の依存を追加** - `hard_stop_latch: Arc<HardStopLatch>`、`market_state_cache: Arc<MarketStateCache>`、`config: ExecutorConfig`、`flattener: Arc<Flattener>`、`alert_service: Arc<AlertService>` を追加、`ExecutorConfig`/`MarketStateCache` 構造体を定義、(3) **PositionTrackerHandle に同期 API 追加（方針 A: 同期キャッシュ拡張）** - `positions_snapshot`/`pending_orders_snapshot` を追加、`get_notional()`/`get_pending_notional_excluding_reduce_only()`/`get_all_positions()`/`get_markets_with_pending_orders()`/`get_all_pending_cloids()`/`get_market_for_cloid()` を追加、PositionTrackerTask でスナップショットキャッシュを同期更新 |
| 2026-01-21 | **4. リスク管理 再々々レビュー対応**: (1) **HardStop purge のレース対策** - `drop_new_orders()` の返り値を `Vec<(ClientOrderId, MarketKey)>` に変更（market info を直接返す）、`on_hard_stop()` は `pending_orders_snapshot` を参照せず確実に cleanup、fallback spawn 内で `hard_stop_latch.is_triggered()` を確認し HardStop 中は `register_order()` をスキップ、(2) **ExecutorLoop::tick() に送信直前 HardStop ガード追加** - `batch_scheduler.tick()` で取得した後に HardStop が発火した場合の対策、ActionBatch::Orders の場合は署名/送信前に HardStop チェック、HardStop 中は reduce_only のみにフィルタし new_order は drop + cleanup、フィルタ後に空なら何も送らない（nonce/post_id を消費しない）、(3) **BatchScheduler の HardStop 注入を必須化** - `hard_stop_latch: Option<Arc<HardStopLatch>>` → `hard_stop_latch: Arc<HardStopLatch>` に変更（安全装置は未設定を許容しない）、`set_hard_stop_latch()` setter を削除、`new()` の引数で必須受け取り |
