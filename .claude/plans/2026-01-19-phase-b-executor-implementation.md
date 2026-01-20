# Phase B: 超小口IOC実弾 実装計画

**作成日**: 2026-01-19
**目的**: 滑り/手数料込みの実効EVを測定
**期間**: Week 13-16（約4週間）
**初期市場**: COIN (xyz:5)

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
| 1 | xyz:5 | **COIN** | 33.04 | 22,828 |

---

## 2. アーキテクチャ

### 2.1 新規Crate構成

```
crates/
├── hip3-executor/           # IOC執行エンジン
│   ├── src/
│   │   ├── lib.rs
│   │   ├── nonce.rs         # NonceManager
│   │   ├── batch.rs         # BatchScheduler
│   │   ├── order.rs         # OrderBuilder、IOC発注
│   │   ├── signer.rs        # 署名処理
│   │   └── budget.rs        # ActionBudget拡張
│   └── Cargo.toml
│
├── hip3-position/           # ポジション管理
│   ├── src/
│   │   ├── lib.rs
│   │   ├── tracker.rs       # PositionTracker
│   │   ├── flatten.rs       # フラット化ロジック
│   │   └── time_stop.rs     # TimeStop管理
│   └── Cargo.toml
│
└── hip3-key/                # 鍵管理（セキュリティ）
    ├── src/
    │   ├── lib.rs
    │   ├── manager.rs       # KeyManager
    │   └── rotation.rs      # ローテーション
    └── Cargo.toml
```

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
| **バッチ単位** | **1 tick = 1 post = 1 L1 action**（複数 orders/cancels をまとめる） |
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

##### キュー溢れ/縮退時の挙動

| 状態 | new_order | reduce_only | cancel | tick() 動作 |
|------|-----------|-------------|--------|-------------|
| 正常 (inflight < 80) | Queued | Queued | Queued | 全キューから収集 |
| 高水位 (80 ≤ inflight < 100) | **QueuedDegraded** | **Queued** | **Queued** | **cancel/reduce_only のみ送信** |
| 上限 (inflight = 100) | InflightFull | InflightFull | Queued | **None**（何も送れない、キューに残る） |
| キュー溢れ | QueueFull | QueueFull | QueueFull | 拒否 |

**重要**:
- 高水位時: cancel と reduce_only は送信（TimeStop/Flatten を確実に処理）
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
    /// 注意: この関数は inflight を increment しない。
    /// 送信成功時に呼び出し元が `on_batch_sent()` を呼ぶこと。
    pub fn tick(&self) -> Option<Batch> {
        let inflight = self.inflight_tracker.current();

        // inflight 上限時: 何も送れない（cancel も送れない）
        // → キューに残して inflight が減るまで待機
        if inflight >= 100 {
            tracing::debug!(inflight, "Inflight full, cannot send any batch");
            return None;
        }

        // 1. cancel を優先収集
        let cancels = self.collect_cancels(self.config.max_cancels_per_batch);

        // 2. reduce_only を収集（高水位でも送信）
        let reduce_only = self.collect_reduce_only(self.config.max_orders_per_batch);

        // 3. 高水位未満なら new_order も収集
        let new_orders = if inflight < self.config.inflight_high_watermark {
            let remaining = self.config.max_orders_per_batch.saturating_sub(reduce_only.len());
            self.collect_new_orders(remaining)
        } else {
            vec![] // 高水位時は新規注文 skip
        };

        // orders = reduce_only + new_orders
        let mut orders = reduce_only;
        orders.extend(new_orders);

        if orders.is_empty() && cancels.is_empty() {
            return None;
        }

        // 注意: ここでは increment しない（送信成功時に on_batch_sent() で行う）
        Some(Batch { orders, cancels })
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
| 6 | 高水位時の tick | cancel + reduce_only のみ収集、new_order は skip |
| 7 | inflight 上限時の tick | **None を返す**（cancel も送れない、キューに残る） |
| 8 | requeue_reduce_only | 失敗した reduce_only が先頭に戻る |
| 9 | tick は increment しない | tick() は inflight を変更しない（呼び出し元が管理） |
| 10 | InflightTracker 整合性 | increment/decrement が正しく動作、reset() は非推奨 |

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

```rust
pub struct Signer {
    private_key: SigningKey,
    address: Address,
}

impl Signer {
    pub fn sign_order(&self, order: &Order, nonce: u64) -> SignedOrder {
        // L1 action署名
        // ref: exchange-endpoint docs
    }
}
```

**タスク**:
- [ ] ethers-rs / alloy 署名実装
- [ ] L1 action構造体定義
- [ ] 署名テスト（Testnetで検証）
- [ ] nonce/vault連携

#### OrderBuilder実装

```rust
pub struct OrderBuilder {
    market: MarketKey,
    spec: MarketSpec,
}

impl OrderBuilder {
    /// IOC注文を構築
    pub fn build_ioc(
        &self,
        side: Side,
        price: Decimal,
        size: Decimal,
        reduce_only: bool,
        cloid: &str,
    ) -> Order {
        let formatted_price = self.spec.format_price(price);
        let formatted_size = self.spec.format_size(size);

        Order {
            asset: self.market.asset.0 as u32,
            is_buy: matches!(side, Side::Buy),
            limit_px: formatted_price,
            sz: formatted_size,
            reduce_only,
            order_type: OrderType::Ioc,
            cloid: Some(cloid.to_string()),
        }
    }
}
```

**タスク**:
- [ ] OrderBuilder実装
- [ ] format_price/format_size統合
- [ ] cloid生成（correlation_id由来）
- [ ] reduce_onlyフラグ
- [ ] IOC/GTC切り替え

### 3.3 Week 2: hip3-position

#### PositionTracker実装

```rust
pub struct PositionTracker {
    positions: DashMap<MarketKey, Position>,
    pending_orders: DashMap<String, PendingOrder>,  // cloid -> order
}

pub struct Position {
    pub market: MarketKey,
    pub side: Side,
    pub size: Decimal,
    pub entry_price: Decimal,
    pub entry_time: Instant,
    pub unrealized_pnl: Decimal,
}
```

**タスク**:
- [ ] PositionTracker構造体
- [ ] orderUpdates購読・処理
- [ ] userFills購読・処理
- [ ] isSnapshot処理（READY-TRADING条件）
- [ ] 約定→ポジション更新ロジック

#### TimeStop実装

```rust
pub struct TimeStop {
    timeout: Duration,        // 30秒
    reduce_only_timeout: Duration,  // 60秒
}

impl TimeStop {
    pub fn check(&self, position: &Position) -> TimeStopAction {
        let age = position.entry_time.elapsed();

        if age > self.reduce_only_timeout {
            TimeStopAction::AlertAndFlatten
        } else if age > self.timeout {
            TimeStopAction::Flatten
        } else {
            TimeStopAction::Hold
        }
    }
}
```

**タスク**:
- [ ] TimeStop構造体
- [ ] タイムアウト判定ロジック
- [ ] フラット化トリガー
- [ ] アラート（60秒超過時）

#### Flatten実装

```rust
pub struct Flattener {
    executor: Arc<Executor>,
}

impl Flattener {
    /// 成行IOCでフラット化
    pub async fn flatten(&self, position: &Position) -> FlattenResult {
        let order = self.build_flatten_order(position);
        self.executor.submit_reduce_only(order).await
    }

    fn build_flatten_order(&self, position: &Position) -> Order {
        OrderBuilder::new(position.market, &position.spec)
            .build_ioc(
                position.side.opposite(),
                self.calculate_aggressive_price(position),
                position.size,
                true,  // reduce_only
                &self.generate_flatten_cloid(position),
            )
    }
}
```

**タスク**:
- [ ] Flattener構造体
- [ ] 成行相当の価格計算（aggressive price）
- [ ] reduce_only IOC発注
- [ ] 部分約定時のリトライ

### 3.4 Week 2-3: 統合・READY-TRADING

#### READY-TRADING条件

```rust
pub struct TradingReadyChecker {
    md_ready: bool,           // READY-MD達成
    order_snapshot: bool,     // orderUpdates isSnapshot受領
    fills_snapshot: bool,     // userFills isSnapshot受領
    position_synced: bool,    // ポジション同期完了
}

impl TradingReadyChecker {
    pub fn is_ready(&self) -> bool {
        self.md_ready
            && self.order_snapshot
            && self.fills_snapshot
            && self.position_synced
    }
}
```

**タスク**:
- [ ] READY-TRADING状態機械
- [ ] orderUpdates isSnapshot処理
- [ ] userFills isSnapshot処理
- [ ] clearinghouseState同期（オプション）

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
  │  ├─ Risk Gate 再検証                   │  └─ enqueue_reduce_only()
  │  ├─ ActionBudget 確認                  │       ↓ (優先キュー)
  │  └─ enqueue_new_order()                │
  │       ↓                                 │
  ▼───────┴─────────────────────────────────┘
BatchScheduler (3キュー: cancel > reduce_only > new_order)
  │
  ▼ (100ms 周期)
ExecutorLoop::tick()
  │  ├─ check_timeouts()
  │  ├─ batch_scheduler.tick() → Option<Batch>
  │  ├─ post_manager.register(nonce, batch)
  │  ├─ nonce_manager.next() → u64
  │  ├─ signer.sign_action(batch, nonce)
  │  └─ ws_sender.post(signed_action)
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
    position_tracker: Arc<PositionTracker>,
    action_budget: Arc<ActionBudget>,
}

impl Executor {
    /// Signal 受信時（新規注文、同期メソッド）
    pub fn on_signal(&self, signal: &Signal) -> ExecutionResult {
        // 1. Risk Gate再検証（保有時）
        if self.position_tracker.has_position(&signal.market) {
            return ExecutionResult::Skipped(SkipReason::AlreadyHasPosition);
        }

        // 2. ActionBudget確認
        if !self.action_budget.can_send_new_order() {
            return ExecutionResult::Skipped(SkipReason::BudgetExhausted);
        }

        // 3. 注文構築（reduce_only = false）
        let order = self.build_order(signal, false);

        // 4. 新規注文キューに追加
        match self.batch_scheduler.enqueue_new_order(order) {
            EnqueueResult::Queued => ExecutionResult::Queued,
            EnqueueResult::QueuedDegraded => {
                tracing::info!("Order queued but system degraded");
                ExecutionResult::QueuedDegraded
            }
            EnqueueResult::QueueFull => {
                ExecutionResult::Rejected(RejectReason::QueueFull)
            }
            EnqueueResult::InflightFull => {
                ExecutionResult::Rejected(RejectReason::InflightFull)
            }
        }
    }

    /// reduce_only 注文（TimeStop/Flatten 用、優先キュー）
    pub fn submit_reduce_only(&self, order: PendingOrder) -> ExecutionResult {
        debug_assert!(order.reduce_only);

        // reduce_only は優先キューへ（高水位でも受付）
        match self.batch_scheduler.enqueue_reduce_only(order) {
            EnqueueResult::Queued => ExecutionResult::Queued,
            EnqueueResult::InflightFull => {
                // キューには積んだが inflight 上限
                // 次の tick で送信される
                ExecutionResult::Queued
            }
            EnqueueResult::QueueFull => {
                // reduce_only キュー溢れは CRITICAL
                tracing::error!("CRITICAL: reduce_only queue full");
                ExecutionResult::Rejected(RejectReason::QueueFull)
            }
            EnqueueResult::QueuedDegraded => ExecutionResult::Queued,
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
    AlreadyHasPosition,
    BudgetExhausted,
}

pub enum RejectReason {
    QueueFull,
    InflightFull,
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
    batch: Batch,
    nonce: u64,        // 付随情報として保持
    sent_at: Instant,
    sent: bool,        // true = WS 送信成功済み（inflight increment 済み）
    tx: oneshot::Sender<PostResult>,
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
        batch: Batch,
    ) -> oneshot::Receiver<PostResult> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(post_id, PendingRequest {
            batch,
            nonce,
            sent_at: Instant::now(),
            sent: false, // 送信前
            tx,
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
    pub fn on_response(&self, post_id: u64, result: PostResult) -> Option<(Batch, bool)> {
        self.pending.remove(&post_id).map(|(_, req)| {
            let _ = req.tx.send(result);
            (req.batch, req.sent)
        })
    }

    /// タイムアウトチェック（定期実行）
    /// Returns: Vec<(post_id, batch, sent)> - sent が true なら inflight decrement が必要
    pub fn check_timeouts(&self) -> Vec<(u64, Batch, bool)> {
        let now = Instant::now();
        let mut timed_out = vec![];

        self.pending.retain(|post_id, req| {
            if now.duration_since(req.sent_at) > self.timeout {
                let _ = req.tx.send(PostResult::Timeout);
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
    pub fn on_disconnect(&self) -> (Vec<Batch>, usize) {
        let mut batches = vec![];
        let mut sent_count = 0;

        for (_, (_, req)) in self.pending.drain() {
            let _ = req.tx.send(PostResult::Disconnected);
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
| WS送信エラー | **再キュー** | **上流通知** | 再キュー |
| 応答 Rejected | ログのみ | ログのみ | ログのみ |
| タイムアウト | **再キュー** | **上流通知** | 再キュー |
| WS切断 | **再キュー** | **上流通知 + HardStop検討** | 再キュー |

**方針**:
- **reduce_only/cancel**: 失敗時は `requeue_reduce_only()` で先頭に再キュー（Flatten 必達）
- **new_order**: 失敗時は上流へ `ExecutionResult::Failed` を返し、シグナルは黙って落ちない

##### ExecutorLoop（tick ループ）

**inflight 管理ルール**:
- `on_batch_sent()`: 送信成功時に increment
- `on_batch_complete()`: 応答受信/タイムアウト時に decrement
- 送信失敗時: increment しないので decrement も不要
- 切断時: `on_disconnect()` で全回収（pending 数分を decrement）

```rust
pub struct ExecutorLoop {
    executor: Arc<Executor>,
    ws_sender: Arc<WsSender>,
    post_manager: Arc<PostRequestManager>,
    post_id_gen: Arc<PostIdGenerator>,
    interval: Duration, // 100ms
}

impl ExecutorLoop {
    /// 100ms 周期で実行
    pub async fn tick(&self) {
        // 0. タイムアウトチェック
        self.handle_timeouts();

        // 1. バッチ収集（inflight increment はまだしない）
        let Some(batch) = self.executor.batch_scheduler.tick() else {
            return;
        };

        // 2. post_id 生成（応答相関用）
        let post_id = self.post_id_gen.next();

        // 3. nonce 払い出し
        let nonce = self.executor.nonce_manager.next();

        // 4. 署名
        let signed_action = self.executor.signer.sign_action(&batch, nonce, post_id);

        // 5. 応答追跡に登録（sent: false で登録）
        let _rx = self.post_manager.register(post_id, nonce, batch.clone());

        // 6. WS 送信
        if let Err(e) = self.ws_sender.post(signed_action).await {
            tracing::error!(error = %e, post_id, "Failed to post action");

            // 送信失敗: PostManager から除去し、reduce_only を再キュー
            // sent: false なので inflight decrement 不要
            if let Some((batch, _sent)) = self.post_manager.on_response(post_id, PostResult::SendError) {
                self.handle_send_failure(batch);
            }
            return;
        }

        // 7. 送信成功:
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

            if let Some((_batch, sent)) = self.post_manager.on_response(post_id, result.clone()) {
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
                        tracing::warn!(post_id, reason, "Action rejected");
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
    fn handle_send_failure(&self, batch: Batch) {
        // reduce_only のみ再キュー（TimeStop/Flatten 必達）
        let reduce_only_orders: Vec<_> = batch.orders
            .into_iter()
            .filter(|o| o.reduce_only)
            .collect();

        if !reduce_only_orders.is_empty() {
            tracing::warn!(
                count = reduce_only_orders.len(),
                "Re-queuing reduce_only orders after failure"
            );
            self.executor.batch_scheduler.requeue_reduce_only(reduce_only_orders);
        }

        // cancel も再キュー
        for cancel in batch.cancels {
            let _ = self.executor.batch_scheduler.enqueue_cancel(cancel);
        }

        // new_order は上流へ通知（シグナルが黙って落ちないように）
        // → 上流で ExecutionResult::Failed を受け取り、必要ならアラート
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

#### Testnet検証項目

| # | 検証項目 | 成功基準 |
|---|----------|----------|
| 1 | WS接続・購読 | orderUpdates/userFills購読成功 |
| 2 | 署名検証 | L1 action署名がTestnetで受理される |
| 3 | IOC発注 | 注文が正常に受理される |
| 4 | 約定確認 | userFillsで約定を受信 |
| 5 | ポジション同期 | PositionTrackerが正しく更新 |
| 6 | フラット化 | reduce-only IOCが正常に動作 |
| 7 | TimeStop | タイムアウト時に自動フラット化 |
| 8 | nonce | 連続発注でnonce衝突なし |
| 9 | レート制限 | inflight上限内で動作 |
| 10 | エラー処理 | reject時のリトライ動作 |

**目標トレード数**: 10-20トレード

**タスク**:
- [ ] Testnet接続設定
- [ ] 各検証項目の実施
- [ ] 問題点の修正
- [ ] Mainnet移行判定

### 3.6 Week 4: Mainnet超小口テスト

#### Mainnet検証パラメータ

| パラメータ | 値 |
|-----------|-----|
| 対象市場 | COIN (xyz:5) のみ |
| 注文サイズ | $10-50/注文 |
| 最大ポジション | $50 |
| 目標トレード数 | 100 |
| 監視期間 | 1週間 |

#### 成果物メトリクス

| メトリクス | 定義 |
|-----------|------|
| `expected_edge_bps` | シグナル時点のedge |
| `actual_edge_bps` | 実約定ベースのedge |
| `slippage_bps` | expected - actual |
| `fill_rate` | accepted / (accepted + rejected + timeout) |
| `flat_time_ms` | エントリー→フラット完了 |
| `pnl_per_trade` | 1トレードあたりPnL |
| `pnl_cumulative` | 累積PnL |

**タスク**:
- [ ] Mainnet設定切り替え
- [ ] 超小口テスト開始
- [ ] メトリクス収集
- [ ] 日次レビュー
- [ ] 100トレード達成

---

## 4. リスク管理

### 4.1 Phase B固有のRisk Gate

| Gate | 条件 | アクション |
|------|------|-----------|
| MaxPosition | ポジション > MAX_NOTIONAL | 新規禁止 |
| PendingOrder | 同一市場に未約定注文あり | 新規禁止 |
| FlattenFailed | フラット化60秒超過 | アラート + 手動介入 |

### 4.2 緊急停止手順

1. **自動停止トリガー**
   - 累積損失 > $20
   - 連続損失 > 5回
   - フラット化失敗 > 3回

2. **停止シーケンス**
   ```
   1. 新規発注停止
   2. 全未約定注文キャンセル
   3. 全ポジションフラット化
   4. アラート送信
   5. 手動確認待ち
   ```

### 4.3 ロールバック基準

| 条件 | アクション |
|------|-----------|
| 10トレードでedge負 | パラメータ見直し |
| 50トレードでedge負 | Phase Aに戻る |
| 重大バグ発見 | 即時停止・修正 |

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
| P0-25 | serverTime同期 | P0-19a | ⏳ |
| P0-11 | セキュリティ/鍵管理 | - | ⏳ |
| P0-29 | ActionBudget制御 | - | ⏳ |

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
