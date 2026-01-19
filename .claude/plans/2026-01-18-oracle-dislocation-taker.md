# HIP-3 Oracle/Mark Dislocation Taker Bot 実装計画

## 概要

**プロジェクト**: hip3_botv2
**戦略**: Oracle/Mark Dislocation Taker（HIP-3市場でoraclePxとbestの乖離を収益化）
**言語**: Rust (tokio)
**開始フェーズ**: Phase A（観測のみ）
**対象DEX**: xyz（UNIT）のみ

## 戦略概要（Section 8.1, 8.8より）

- **狙い**: best bid/askがoraclePxを跨ぐ瞬間にIOCで踏む
- **勝ち筋**: ロジックより**Hard Risk Gate（停止品質）**と**市場選定**
- **執行**: IOC taker、短時間でフラット回帰（time stop + reduce-only）
- **参照価格**: `oraclePx`を一次参照、`markPx`を検算用

---

## 1. プロジェクト構造

```
hip3_botv2/
├── Cargo.toml                    # Workspace root
├── config/
│   ├── default.toml
│   ├── testnet.toml
│   └── mainnet.toml
├── docker/
│   ├── Dockerfile
│   └── docker-compose.yml
├── crates/
│   ├── hip3-core/                # ドメイン型（MarketKey, Price, Size）
│   ├── hip3-ws/                  # 自前WebSocketクライアント
│   ├── hip3-feed/                # マーケットデータ集約
│   ├── hip3-registry/            # Market Discovery & Spec同期
│   ├── hip3-risk/                # Hard Risk Gates（最重要）
│   ├── hip3-detector/            # Dislocation検知
│   ├── hip3-executor/            # IOC執行（Phase B以降）
│   ├── hip3-position/            # ポジション管理（Phase B以降）
│   ├── hip3-telemetry/           # Prometheus + 構造化ログ
│   ├── hip3-persistence/         # Parquet/ClickHouse
│   └── hip3-bot/                 # メインアプリケーション
├── analysis/                     # Python分析スクリプト
│   ├── requirements.txt
│   └── scripts/
└── tests/
    └── integration/
```

---

## 2. コアモジュール

### 2.1 hip3-core: ドメイン型

```rust
// MarketKey = HIP-3の二重構造（DEX + asset）
pub struct MarketKey {
    pub dex: DexId,
    pub asset: AssetId,
}

// 精度管理
pub struct Price(pub rust_decimal::Decimal);
pub struct Size(pub rust_decimal::Decimal);

// 市場仕様（spec_version付き：後解析の再現性確保）
// 注意: tick_size固定値ではなく、価格精度ルールを使用
pub struct MarketSpec {
    pub sz_decimals: u8,              // サイズ小数桁
    pub max_sig_figs: u8,             // 最大有効桁（常に5）
    pub max_price_decimals: u8,       // MAX_DECIMALS(6) - sz_decimals
    pub lot_size: Size,
    pub max_leverage: u8,
    pub base_taker_fee_bps: u16,
    pub base_maker_fee_bps: u16,
    pub hip3_fee_multiplier: f32,     // HIP-3は2.0
    pub oi_cap: Option<Size>,
    pub spec_version: u64,
}

// 価格/サイズの正規化（P0-23: hip3-coreに集約）
// P0-28追加: 文字列表現までテストベクタで仕様化
impl MarketSpec {
    /// 価格を正規化（5有効桁 + max decimals制約 + 末尾ゼロ除去）
    /// 戻り値は発注APIに送る文字列表現
    pub fn format_price(&self, price: Decimal) -> String {
        // 1. 5有効桁に丸め
        // 2. 小数桁を max_price_decimals 以下に制限
        // 3. 末尾ゼロを除去
        todo!()
    }

    /// サイズを正規化
    pub fn format_size(&self, size: Decimal) -> String {
        // sz_decimals桁に丸め + 末尾ゼロ除去
        todo!()
    }
}

// P0-28: テストベクタ（golden test）を先に固定
// format_price/size が曖昧だと注文reject / 解析再現性崩壊に直結
#[cfg(test)]
mod format_tests {
    // 価格 50123.456789 (sz_decimals=3) の正規化テスト
    // → 5有効桁: 50123
    // → max_decimals: 6-3=3
    // → 結果: "50123" (末尾ゼロなし)

    // 価格 0.00012345 (sz_decimals=0) の正規化テスト
    // → 5有効桁: 0.00012345 → 0.00012345 (5 sig figs)
    // → max_decimals: 6-0=6
    // → 結果: "0.00012345"
}

// Detector側: edge判定は丸め後（実際に発注される値）で必ず評価
pub fn evaluate_edge_with_rounding(
    raw_price: Decimal,
    spec: &MarketSpec,
) -> Decimal {
    let formatted = spec.format_price(raw_price);
    let rounded_price = Decimal::from_str(&formatted).unwrap();
    // 丸め後の価格でedgeを再計算
    calculate_edge(rounded_price)
}
```

### 2.2 hip3-ws: 自前WebSocketクライアント

| コンポーネント | 責務 |
|----------------|------|
| ConnectionManager | 指数バックオフ + jitter、再接続後の購読復元 |
| SubscriptionManager | 購読状態管理、READY条件 |
| HeartbeatManager | **45秒無受信**でping、pong未達で再接続 |
| Router | channel別ルーティング |
| RateLimiter | 2000msg/min（トークンバケット）、**100 inflight posts（別セマフォ）** |
| **PreflightChecker** | 起動時の静的制限チェック（P0-3追加） |
| **ActionBudget** | アドレス単位のアクション制限（P0-6追加） |

**アクション・バジェット（P0-6：address-based actions制限）**:

WS msg/minやinflight postに加え、**アドレス単位のアクション制限**を運用設計に組み込む。

| 制限対象 | ルール | 根拠 |
|----------|--------|------|
| **連続リトライ** | 指数バックオフ（1s→2s→4s…max 30s） | API負荷軽減 |
| **冗長キャンセル** | 既キャンセル済み注文の再キャンセル禁止 | 無駄なaction消費 |
| **不要post** | 同一cloidの重複post抑制 | 冪等性は担保だが消費は無駄 |
| **サーキットブレーカ** | 連続N回（例:5回）失敗で一時停止（60s） | 異常時の暴走防止 |

**ActionBudgetの責務拡張（P0-29：検知→制御アルゴリズム）**:

address-based limitsは「初期バッファ」「累積出来高比例」「制限中は10秒に1回」「キャンセル優遇」等の仕様あり。
Phase B/Cで取引頻度が上がるほど、**post/cancelの配分最適化**が必須。

```rust
pub struct ActionBudget {
    // 既存: 検知系
    pub retry_backoff: ExponentialBackoff,
    pub cancelled_orders: HashSet<String>,
    pub failure_count: u32,
    pub circuit_open_until: Option<Instant>,

    // P0-29追加: 制御系
    pub send_queue: PriorityQueue<QueuedAction>,  // 優先度付きキュー
    pub token_bucket: TokenBucket,                // 予算配賦
    pub address_limit_state: AddressLimitState,   // 通常/制限中
}

pub enum AddressLimitState {
    Normal,                          // 通常状態
    Limited { next_allowed: Instant }, // 10秒に1回モード
}

pub struct QueuedAction {
    pub priority: ActionPriority,
    pub action: Action,
    pub enqueued_at: Instant,
}
```

**過負荷時の制御ロジック**:
1. **ReducePosition/Cancel を優先的に通す**（token bucket から優先引き落とし）
2. **NewOrder を落とす**（キューに残すか、即時reject）
3. **制限中（10秒に1回）モードでは ReducePosition のみ通す**

**address-based limitヒット時の優先順位（P0-18：送信種別で差別化）**:

レート制限を踏んでも**安全側の行為を優先**する。

| 優先度 | 送信種別 | 理由 |
|--------|----------|------|
| **1（最優先）** | 保有ポジの縮小・クローズ（reduceOnly/flatten） | テール損失回避 |
| **2** | キャンセル（冗長キャンセル禁止は維持） | 意図しない約定回避 |
| **3（最も抑制）** | 新規post | 機会損失より事故回避 |

```rust
pub enum ActionPriority {
    ReducePosition = 0,  // 最優先
    Cancel = 1,
    NewOrder = 2,        // 最も抑制
}
```

**レート制限ガバナンス（P0-3対応）**:

| チェック | タイミング | 内容 |
|----------|-----------|------|
| **静的プリフライト** | 起動時 | 計画購読数が制限に収まるか検証、超過時は自動削減 or 起動拒否 |
| **ランタイムゲート** | 実行時 | トークンバケット（2000/min）+ semaphore（inflight ≤ 100） |
| **multi-process禁止** | 設計方針 | 単一インスタンス/アカウント前提。分散時はRedis等で集中管理 |

#### WS状態機械（P0：明示化必須）

```
Disconnected → Connecting → Subscribing → Syncing → Ready
     ↑                                                 │
     └─────────────── (error/timeout) ←────────────────┘
```

| 状態 | 条件 | 次状態 |
|------|------|--------|
| Disconnected | connect() | Connecting |
| Connecting | WS open | Subscribing |
| Subscribing | 全subscriptionResponse受領 | Syncing |
| Syncing | 全snapshot適用完了 + 初回整合チェック | Ready |
| Ready | 切断/エラー | Disconnected |

**READY条件のPhase別プロファイル化（P0-4：観測 vs 取引の分離）**:

Phase A（観測のみ）とPhase B以降（取引あり）でREADY条件を分離し、不要な購読でmsg/subscription枠を消費しない設計。

**READY-MD（Phase A用：マーケットデータのみ）**:
1. subscriptionResponse（ACK）受領
2. 対象市場の **bbo** 初回受領（**かつ bestBid/bestAsk が非null**）
3. 対象市場の **activeAssetCtx**（oracle/mark含む）初回受領
4. **鮮度チェック**: `bbo_age_ms <= MAX_BBO_AGE_MS` かつ `ctx_age_ms <= MAX_CTX_AGE_MS`

**READY-TRADING（Phase B以降用）**:
1. **READY-MDを満たす**
2. `orderUpdates` / `userFills` の `isSnapshot:true` 受領
3. snapshot適用完了を確認
4. 必要なら `openOrders` / `clearinghouseState` 等のスナップショット適用完了

**整合性**:
5. 初回状態整合チェック（time巻き戻りなし）

**分離のメリット**:
- 観測だけしたいのにREADYにならない問題を回避
- 不要な購読でmsg/subscription枠を消費しない
- Phase Aの分析が遅延・欠損しない

**READY-MD「初回bbo未達」ポリシー（P0-7：初期状態の穴対策）**:

Hyperliquidの`bbo`は「ブロック上でbboが変化した場合のみ送信」仕様。購読ACK後に初回bboが長時間来ない可能性がある（静かな市場/板が動かない局面）。

| ポリシー | 条件 | アクション |
|----------|------|------------|
| **タイムアウト除外** | bbo初回が `BBO_INIT_TIMEOUT_MS`（例:10秒）未達 | **その市場を対象外化**（READY待ちで詰まらせない） |
| **代替シード（将来検討）** | l2Bookを短時間購読 | 初期板取得後に解除（購読枠/メッセージ枠とトレードオフ） |

```rust
pub struct MarketReadyState {
    pub subscription_ack: bool,
    pub bbo_received: bool,
    pub asset_ctx_received: bool,
    pub age_valid: bool,             // bbo_age/ctx_age が閾値内
    pub bbo_timeout_excluded: bool,  // true = 除外済み
    pub excluded_until: Option<Instant>,  // TTL付き再評価
}
```

**READY除外市場の再評価ポリシー（P0-20）**:

`BBO_INIT_TIMEOUT_MS` 等で一度除外した市場の扱い：

| Phase | ポリシー | TTL |
|-------|---------|-----|
| **Phase A（観測）** | **TTL付き再評価** | 5〜15分で再購読→再判定 |
| **Phase B（取引）** | **原則対象外** | ランキング上位のみ再評価検討 |

```rust
pub struct ExclusionPolicy {
    pub phase_a_ttl: Duration,    // e.g., 10 minutes
    pub phase_b_reevaluate: bool, // false = 永続除外
}
```

※マーケットデータについては「常に最初にsnapshotが来る」前提にしない設計が安全

**鮮度（age）ベースの時刻設計（P0-12：MAX_SKEW前提の撤回）**:

**問題**: 当初の「サーバtime同士で比較する MAX_SKEW」は**実装不可能**。
- `WsBbo` には `time: number` がある
- **`WsActiveAssetCtx` の `ctx`（PerpsAssetCtx）には時刻フィールドがない**

したがって、**monotonic鮮度（age）ベース**に設計を変更：

| 判定 | 定義 | 用途 |
|------|------|------|
| `bbo_age_ms` | `now_mono - last_bbo_recv_mono` | bbo鮮度判定 |
| `ctx_age_ms` | `now_mono - last_ctx_recv_mono` | assetCtx鮮度判定 |
| `MAX_BBO_AGE_MS` | 設定値（例: 2000ms） | bboが古すぎる場合に判定無効 |
| `MAX_CTX_AGE_MS` | 設定値（例: 8000ms） | ctxが古すぎる場合に判定無効 |

**クロス判定許可条件（修正後）**:
```
bbo_age_ms <= MAX_BBO_AGE_MS AND ctx_age_ms <= MAX_CTX_AGE_MS
```

**命名変更**: ~~MAX_SKEW_MS~~ → `MAX_BBO_AGE_MS` / `MAX_CTX_AGE_MS`（概念一致）

**将来検討（案B）**: `WsBook`（time fieldあり）を基準にする方法もあるが、購読・処理コストが上がるためPhase Aで必要性を検証してから導入

| 用途 | 使用する時刻 | 根拠 |
|------|-------------|------|
| **鮮度判定（クロス許可）** | ローカル受信時刻 (monotonic) | activeAssetCtxに時刻がないため |
| **遅延検知** | ローカル受信時刻 (monotonic) | ネットワーク遅延の測定 |
| **バックプレッシャ判定** | ローカル受信時刻 | キュー詰まり検知 |
| **再接続判定 (無受信閾値)** | ローカル受信時刻 | ハートビート監視 |

**Heartbeat仕様（P0-2対応：60秒ルールへの明示的マッピング）**:

Hyperliquid仕様: サーバは**60秒間クライアントへメッセージを送っていない接続を閉じる**

| 設定 | 値 | 根拠 |
|------|-----|------|
| 無受信検知閾値 | **45秒** | 60秒ルールの安全マージン（15秒余裕） |
| ping送信 | `{method:"ping"}` | 公式ドキュメント準拠 |
| pong期待 | `{channel:"pong"}` | 公式ドキュメント準拠 |
| pongタイムアウト | **10秒** | 未達で再接続トリガー |

### 2.3 hip3-risk: Hard Risk Gate（利益そのもの）

| Gate | 条件 | アクション（非保有時） | アクション（保有時） |
|------|------|------------------------|----------------------|
| OracleFresh | `ctx_age_ms > MAX_CTX_AGE_MS` | 新規禁止 | 新規禁止 |

**OracleFresh定義の修正（P0-13）**:
- ~~「oraclePxが変化した時刻」~~ではなく、**「ctxを最後に受信した時刻」**を基準にする
- 理由: 価格が横ばいの時間帯に誤ってstale扱いになるのを防ぐ
- `oracle_age_ms = now_mono - last_ctx_recv_mono`（ctx受信時刻ベース）
- Phase Aで oraclePx 更新頻度を観測し、必要に応じて定義を調整
| MarkMidDivergence | `abs(mark - mid)/mid > Y_bps`継続 | 新規禁止 | 新規禁止 |
| SpreadShock | `spread_now > k × EWMA(spread)` | サイズ1/5 or 禁止 | サイズ1/5 or 禁止 |
| OiCap | OI cap到達 | 新規禁止 | 新規禁止 |
| ParamChange | tick/lot/fee変更検知 | 全キャンセル + 停止 | 全キャンセル + 縮小→停止 |
| Halt | 取引停止検知 | 全キャンセル + 停止 | 全キャンセル + 縮小→停止 |
| BufferLow | 清算バッファ閾値割れ | 新規禁止 | 縮小→停止 |
| **NoBboUpdate** | bbo更新途絶（動的閾値） | **新規禁止のみ** | **縮小→停止** |
| **NoAssetCtxUpdate** | assetCtx更新途絶（動的閾値） | **新規禁止のみ** | **縮小→停止** |
| **TimeRegression** | 受信timeが巻き戻り | 全キャンセル + 停止 | 全キャンセル + 縮小→停止 |

**保有中の「縮小→停止」シーケンス（P0追加）**:
1. 全注文キャンセル
2. 成行/IOCで強制フラット化（reduce-only）
3. フラット化完了後に停止状態へ遷移

**FeedHealth設計（P0-1対応：接続健全性と市場健全性の分離）**:

bbo/assetCtxは「変化時のみ」送信されるため、静かな市場では更新途絶が正常。
誤停止を防ぐため、以下のように分離：

| カテゴリ | Gate | 条件 | アクション |
|----------|------|------|------------|
| **接続健全性** | NoAnyMessage | 任意メッセージが45秒途絶 | ping送信→pong未達で再接続 |
| **市場健全性** | NoBboUpdate | bbo更新途絶（動的閾値） | **新規禁止のみ**（全停止しない） |
| **市場健全性** | NoAssetCtxUpdate | assetCtx更新途絶（動的閾値） | **新規禁止のみ** |
| **市場健全性** | **BboNull** | `bestBid == null` OR `bestAsk == null` | **判定無効**（当該市場をREADYから除外） |
| **整合性** | TimeRegression | 受信timeが巻き戻り（**timeフィールド有チャネルのみ**） | 全キャンセル + 停止 |

**TimeRegressionの適用範囲（P0-16：チャネル別固定）**:

| チャネル | time有無 | 検知方法 |
|----------|---------|---------|
| `WsBbo` | **有** | `time` フィールドの回帰（TimeRegression適用） |
| `activeAssetCtx` | **無** | **受信時刻ベースの age（Δt）**＋**更新停止検知** |
| `orderUpdates` | 要確認 | isSnapshot有、time有ならTimeRegression適用 |
| `userFills` | 要確認 | isSnapshot有、time有ならTimeRegression適用 |

```rust
pub enum TimestampPolicy {
    ServerTime,      // WsBbo等: time field を使用
    ReceiveTimeOnly, // activeAssetCtx等: monotonic受信時刻のみ
}

pub fn get_policy(channel: &str) -> TimestampPolicy {
    match channel {
        "bbo" | "l2Book" | "trades" => TimestampPolicy::ServerTime,
        "activeAssetCtx" => TimestampPolicy::ReceiveTimeOnly,
        _ => TimestampPolicy::ReceiveTimeOnly, // 安全側に倒す
    }
}
```

**Perps/Spot混在の例外封じ（P0-30：activeAssetCtxのspot型対応）**:

`activeAssetCtx` は perps/spot両方の型が返り得る（`WsActiveAssetCtx` / `WsActiveSpotAssetCtx`）。
本botは perps（HIP-3）限定のため、spot型が返った場合は購読対象から除外。

| 受信型 | 対応 |
|--------|------|
| `WsActiveAssetCtx`（perps） | **正常処理** |
| `WsActiveSpotAssetCtx`（spot） | **購読対象から除外** + ログ + メトリクス |

```rust
pub enum ActiveAssetCtxResponse {
    Perps(WsActiveAssetCtx),
    Spot(WsActiveSpotAssetCtx),
}

pub fn handle_active_asset_ctx(response: &ActiveAssetCtxResponse, coin: &str) -> Result<(), FeedError> {
    match response {
        ActiveAssetCtxResponse::Perps(ctx) => {
            // 正常処理
            Ok(())
        }
        ActiveAssetCtxResponse::Spot(_) => {
            // spot型は本botの対象外 → 除外
            metrics::counter!("hip3_spot_ctx_rejected_total", 1, "coin" => coin.to_string());
            log::warn!("Coin {} returned spot ctx, excluding from target", coin);
            Err(FeedError::SpotCoinRejected(coin.to_string()))
        }
    }
}
```

参照: https://github.com/hyperliquid-dex/hyperliquid-rust-sdk/issues/88

**BboNull（片側板なし）の扱い（P0-14 + P0-17修正）**:

`WsBbo.bbo` は `[WsLevel | null, WsLevel | null]` であり、best bid/ask の片側が null になり得る。

**注意**: 「両側 null = Halt」は一次情報では保証されない。Halt判定は別系（status/注文エラー/market停止イベント）に寄せる。

| 状態 | アクション |
|------|------------|
| `bestBid == null` OR `bestAsk == null` | **BboNull状態**（当該市場をREADYから除外、判定無効） |
| ~~両方 null = Halt~~ | **BboNull状態として扱う**（Halt判定は別系） |

```rust
pub enum BboState {
    Valid { bid: WsLevel, ask: WsLevel },
    BidNull { ask: WsLevel },
    AskNull { bid: WsLevel },
    BothNull,  // Halt扱いではない、判定無効のみ
}

pub fn classify_bbo(bbo: &WsBbo) -> BboState {
    match (&bbo.bbo.0, &bbo.bbo.1) {
        (Some(bid), Some(ask)) => BboState::Valid { bid: bid.clone(), ask: ask.clone() },
        (None, Some(ask)) => BboState::BidNull { ask: ask.clone() },
        (Some(bid), None) => BboState::AskNull { bid: bid.clone() },
        (None, None) => BboState::BothNull,
    }
}
```

**Halt判定の別系**:
- `perpDexStatus` による DEX status監視
- 注文送信時のエラーレスポンス（market halted等）
- WS subscription での market停止イベント（あれば）

**動的閾値の考え方**:
- Phase Aで市場ごとの更新間隔分布（p50/p95）を測定
- 閾値 = `p95 × 2` などデータに基づき設定
- 静かな市場では閾値を緩く、活発な市場では厳しく

### 2.4 hip3-detector: Dislocation検知

**エントリー条件**:
- Buy: `best_ask <= oraclePx × (1 - (FEE + SLIP + EDGE)/1e4)`
- Sell: `best_bid >= oraclePx × (1 + (FEE + SLIP + EDGE)/1e4)`
- **midの乖離だけで入らない**（bestが跨いだときのみ）
- **「跨いだ瞬間」の定義**: bbo更新イベント時点でbestがoracleを跨いでいること

**cross判定の鮮度条件（P0-12修正：age-based）**:

oracle（ctx）とbboは非同期に更新されるため、**古いデータで誤踏み**するリスクがある。
monotonic受信時刻による**鮮度（age）判定**で対策：

| 条件 | 判定 |
|------|------|
| `bbo_age_ms <= MAX_BBO_AGE_MS` AND `ctx_age_ms <= MAX_CTX_AGE_MS` | **有効**：cross判定を実行 |
| 上記を満たさない | **無効**：見送り（シグナル無効） |
| `bbo.bestBid == null` OR `bbo.bestAsk == null` | **無効**：判定不可（片側板なし） |

**注意**: 条件を満たさない場合は「新規禁止」ではなく「**その判定だけ無効**」。停止品質を悪化させない。

**閾値設定（初期値）**:
- `MAX_BBO_AGE_MS`: 2000ms（保守的に開始）
- `MAX_CTX_AGE_MS`: 8000ms（oracle fresh閾値と整合）
- Phase Aで受信間隔分布（P50/P95/P99）を実測し、市場別に調整

**サイズ**: `min(alpha × top_of_book_size, max_notional / mid)` where `alpha = 0.10`
- 丸め（tick/lot）適用後のedge劣化を計算に織り込む
- 丸めでedgeが死ぬ市場は除外

**Edge計算パラメータ（P0：構成要素として別フィールド保存）**:
```rust
pub struct EdgeParams {
    pub taker_fee_bps: Decimal,      // 実効taker fee（HIP-3 2x反映済み）
    pub slip_bps: Decimal,           // 想定スリッページ（top-of-book前提）
    pub safety_margin_bps: Decimal,  // 安全マージン
    pub total_bps: Decimal,          // FEE + SLIP + EDGE
    pub fee_metadata: FeeMetadata,   // 計算根拠（後から検証可能）
}

// HIP-3 Feeモデル（P0-24: 2倍手数料 + userFees反映）
pub struct FeeMetadata {
    pub base_maker_bps: Decimal,     // 基本maker fee
    pub base_taker_bps: Decimal,     // 基本taker fee
    pub volume_tier: String,         // 14d volume tier
    pub tier_discount_pct: Decimal,  // tier割引率
    pub referral_discount_pct: Decimal, // 紹介割引（あれば）
    pub hip3_multiplier: Decimal,    // HIP-3倍率（2.0）
    pub effective_maker_bps: Decimal, // 実効maker fee
    pub effective_taker_bps: Decimal, // 実効taker fee
}
```

**HIP-3手数料計算（P0-24必須）**:

HIP-3はユーザー手数料が**通常の2倍**。これを反映しないとedge計算が歪む。

1. 起動時に `info type="userFees"` を取得
2. 14d volume tier、各種discount から**実効fee（maker/taker）**を算出
3. **HIP-3倍率（2x）** を適用
4. `EdgeParams` に注入、`TriggerEvent` に計算根拠を残す

```rust
pub async fn calculate_effective_fees(user_fees: &UserFees) -> FeeMetadata {
    let base_maker = Decimal::from_str(&user_fees.maker_rate).unwrap();
    let base_taker = Decimal::from_str(&user_fees.taker_rate).unwrap();

    // tier割引等を適用
    let discounted_maker = apply_discounts(base_maker, user_fees);
    let discounted_taker = apply_discounts(base_taker, user_fees);

    // HIP-3 2x適用
    let hip3_mult = Decimal::from(2);
    FeeMetadata {
        effective_maker_bps: discounted_maker * hip3_mult,
        effective_taker_bps: discounted_taker * hip3_mult,
        hip3_multiplier: hip3_mult,
        // ... 他のフィールド
    }
}
```

---

## 2.5 実装開始前のGo条件（P0-32：着手判断基準）

Phase A着手前に以下を満たすこと：

### Go条件 1: 仕様TODOをゼロにする
- `MarketSpec::format_price/format_size` を実装し、**golden test**（拒否・丸め・境界）を固定
- `MarketSpec` の入力元（meta/perpDexs等のどのフィールドを採用するか）をコードで固定
- **プロダクションの変更検知**を仕込む（差分でfail fast）

### Go条件 2: Preflight が「落ちるべき時に必ず落ちる」
- **xyzがデプロイしているHIP-3銘柄に限定**（対象銘柄セットを起動時に確定）
- `perpDexs/meta` で **Coin–AssetId の一意性検証**を実施
- 曖昧（衝突/欠損/想定外dex混入）なら **起動拒否**
- "WSでdex指定できない購読" を使う場合は、上記一意性検証が **唯一の安全弁**

### Go条件 3: WS健全性・レート制限が「観測だけで証明できる」
- ping/pong、再接続（指数バックオフ＋jitter）、snapshot ack の扱い（`isSnapshot:true`）を実装
- 送信レート（msg/min）と inflight post を **会計**
- 上限接近で **段階的縮退**（購読削減→発注停止→全停止）

### Go条件 4: Perps/Spot混在の例外を封じる
- `activeAssetCtx` が spot型で返るケースがある（公式Docs上も spot型が返り得る仕様）
- 本botは perps（HIP-3）限定のため、対象coinがspot側に解決された場合は **購読対象から除外**
- ログ＋メトリクスで即時検知

---

## 3. Phase A 実装タスク

### 実装順序の推奨（P0-22：最小縦切りを先に通す）

ClickHouse/Parquet等の保存系より前に、以下を最小構成で動かす：

1. `hip3-ws`（subscribe: bbo + activeAssetCtx）
2. `hip3-feed`（正規化・欠損/回帰の検知まで）
3. `hip3-detector`（cross判定のみ、取引はしない）
4. `hip3-telemetry`（trigger eventを可視化）

→ ここまで通すと、以降の「Selector / Gate / Executor」の議論が実データで可能になる。

---

### Week 1-2: 基盤構築
- [ ] Cargo workspaceセットアップ
- [ ] hip3-core実装（MarketKey, Price, Size, MarketSpec）
- [ ] hip3-ws基本実装（ConnectionManager、基本接続）

### Week 3-4: WebSocket完成
- [ ] SubscriptionManager（購読管理、READY条件Phase分離）
- [ ] HeartbeatManager（45秒ping、pong監視）
- [ ] RateLimiter（トークンバケット、semaphore）
- [ ] Router（channel別ルーティング）
- [ ] 再接続シーケンス（バックオフ + jitter）
- [ ] **snapshot/重複処理ポリシー（チャネル別）**
- [ ] **READY-MD初回bbo未達ポリシー（タイムアウト→除外）**
- [ ] **レート制限"会計"メトリクス**

### Week 5-6: Feed と Registry + 最低限Gate（P0前倒し）
- [ ] MarketState（BBO、AssetCtx集約 + **bbo null判定**）
- [ ] ~~Oracle freshness tracking（oraclePx変化時刻）~~ → **ctx受信鮮度tracking（ctx_age_ms）**
- [ ] perpDexs同期（**xyz DEX dex name確定 + 空文字事故防止**）
- [ ] SpecCache（tick/lot/fee + spec_version）
- [ ] DiffDetector（仕様変更検知 + **30秒ポーリング**）
- [ ] **OracleFresh Gate（P0前倒し）**
- [ ] **FeedHealth Gate（NoBboUpdate/NoAssetCtxUpdate/TimeRegression）**
- [ ] **ParamChange Gate**
- [ ] **ActionBudget（backoff + サーキットブレーカ）**

### Week 7-8: 残りRisk Gate と Detector
- [ ] 残りのRiskGate実装（MarkMid/SpreadShock/OiCap/Halt/Buffer）
- [ ] DislocationDetector（edge計算、シグナル生成）
- [ ] **Phase Aログ形式確定（correlation_id + spec_version + EdgeParams）**
- [ ] イベント記録（Parquet）
- [ ] **Parquet書き込みバックプレッシャ設計（停止優先）**
- [ ] Prometheusメトリクス
- [ ] **停止→復帰状態機械（Gate別復帰条件）**
- [ ] **クロス判定機会損失メトリクス**

### Week 9-10: 統合とテスト
- [ ] Application統合
- [ ] Testnet接続テスト
- [ ] Phase A観測開始

### Week 11-12: 分析
- [ ] データ分析（Python/Polars）
- [ ] 市場ランキング作成
- [ ] Phase B準備

---

## 4. 依存クレート

```toml
[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
tokio-tungstenite = { version = "0.21", features = ["rustls-tls-webpki-roots"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rust_decimal = { version = "1", features = ["serde"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
prometheus = "0.13"
parquet = { version = "50", features = ["async"] }
thiserror = "1"
```

---

## 5. 監視・可観測性

### 主要メトリクス
- `hip3_ws_connected`: WS接続状態
- `hip3_ws_state{state}`: WS状態機械の状態
- `hip3_ws_reconnect_total`: 再接続回数
- `hip3_feed_latency_ms`: フィード遅延
- `hip3_triggers_total{market_key, side}`: トリガー回数
- `hip3_edge_bps{market_key}`: edge分布
- `hip3_gate_blocked_total{reason}`: ゲートブロック
- `hip3_oracle_stall_rate{market_key}`: Oracle stale率
- `hip3_mark_mid_gap_bps{market_key}`: Mark-Mid乖離
- `hip3_decision_latency_ms`: bbo受信→判定完了
- `hip3_exec_attempt_total{result}`: accepted/rejected/rate_limited/timeout

### レート制限"会計"メトリクス（P0-8：ボトルネック可視化）

Phase Aの時点でレート制限のボトルネックを潰すため、以下のメトリクスを導入：

| メトリクス | 型 | 用途 |
|------------|-----|------|
| `hip3_ws_msgs_sent_total{kind}` | Counter | 送信種別（subscribe/unsubscribe/ping/post/cancel） |
| `hip3_ws_msgs_blocked_total{reason,kind}` | Counter | 抑制された送信（rate_limit/inflight_full/circuit_open） |
| `hip3_post_inflight` | Gauge | 現在のinflight post数（0-100） |
| `hip3_post_rejected_total{reason}` | Counter | 拒否されたpost（rate_limit/error） |
| `hip3_action_budget_state{state}` | Gauge | サーキットブレーカ状態（open=1/closed=0） |
| `hip3_address_limit_hit_total` | Counter | address-based制限を踏んだ回数（1req/10sec状態） |
| `hip3_cross_skipped_total{reason}` | Counter | cross判定を見送った回数（skew_exceed/bbo_stale/oracle_stale） |

**address-based制限検知**:
- `1req/10sec`状態を踏んだ場合、メトリクス記録→自動降格（送信レートを下げる）
- Phase Aでこの状態が頻発する場合は、送信設計を見直し

### イベント相関ID（P0：検証の要）
```rust
pub struct TriggerEvent {
    pub correlation_id: String,  // 1 trigger = 1 correlation_id
    pub timestamp_ms: i64,
    pub market_key: String,
    pub spec_version: u64,       // 当時の仕様を再現可能に
    pub side: String,
    // P0-4対応: f64ではなくDecimal/Stringで保存（精度保持・再現可能なPnL/edge分析）
    pub oracle_px: Decimal,      // NOT f64
    pub best_px: Decimal,        // NOT f64
    pub mark_px: Decimal,        // NOT f64
    pub edge_params: EdgeParams, // FEE/SLIP/EDGE を分離保存（全てDecimal）
    pub edge_bps: Decimal,       // NOT f64
    pub spread_bps: Decimal,     // NOT f64
    pub depth_top: Decimal,      // NOT f64
    pub gate_result: String,
    pub duration_ms: i64,
}
```
- gate判定・想定注文・実注文・約定・フラット化まで同一correlation_idで紐付け
- **f64は派生フィールド（ダッシュボード用）としてのみ使用、正として使わない**

### Grafanaダッシュボード
1. 接続状態とREADY状態（WS状態機械）
2. フィード遅延分布
3. Risk Gateブロック理由
4. トリガー統計（MarketKey別）
5. Oracle健全性
6. スプレッド異常
7. **FeedHealth（bbo/assetCtx更新途絶）**

---

## 6. デプロイ

```yaml
# docker/docker-compose.yml
services:
  hip3-bot:
    build: .
    restart: unless-stopped
    environment:
      - RUST_LOG=info,hip3=debug
    volumes:
      - ./config:/config:ro
      - ./data:/data
    deploy:
      resources:
        limits:
          cpus: '2.0'
          memory: 1G

  prometheus:
    image: prom/prometheus:v2.45.0
    ports:
      - "9090:9090"

  grafana:
    image: grafana/grafana:10.1.5
    ports:
      - "3000:3000"
```

---

## 7. 全フェーズロードマップ

### Phase A: 観測のみ（紙トレ）- Week 1-12

**目的**: EV見込みのある市場を特定、Gate停止品質の検証

**成果物**:
- トリガー条件成立回数（MarketKey別）
- edge分布（手数料込みで正のEVが出るか）
- Oracle stale率、spread shock率
- **EV見込みのある市場ランキング**

**Phase A 追加観測指標（P0-12対応）**:
- `ctx_recv_interval_ms`: activeAssetCtx受信間隔の分布（P50/P95/P99）
- `bbo_recv_interval_ms`: bbo受信間隔の分布（P50/P95/P99）
- `ctx_age_ms` / `bbo_age_ms`: クロス許可時の鮮度分布
- `bbo_null_rate`: bid/ask null の発生率（市場別）
- `post_inflight_max/avg`: inflight post数（上限100に対する状況）
- `rate_limit_hit`: IP/Address別のヒット回数

**Phase A DoD（Definition of Done）（P0-31：Phase B移行前に必須）**:

Phase Bへ進む前に、以下を満たすこと：

| 項目 | 判定基準 |
|------|---------|
| **連続稼働** | 24時間以上の連続稼働で、WS再接続が自律復旧し続ける（手動介入なし） |
| **レート制限観測** | msg/min / inflight post が常時観測され、上限接近時に縮退が機能する |
| **日次出力指標** | 対象銘柄ごとに日次で以下が出力される |

**日次出力必須指標**:
- `cross_count`: oracle跨ぎ検出回数
- `bbo_null_rate`: BBO欠損率
- `ctx_age_ms`: activeAssetCtx遅延分布（P50/P95/P99）
- `best_age_ms`: bbo遅延分布（P50/P95/P99）
- `cross_duration_ticks`: "跨ぎの持続時間" の分布（1tick/複数tick）

**Phase B移行条件**:
- **Phase A DoDを全て満たす**
- 2〜3市場でEV正の兆候
- Risk Gateの停止品質が安定
- 超小口IOCでの滑り/手数料の実測準備完了
- **ctx/bboの受信間隔分布（P50/P95/P99）が把握済み**（age閾値調整完了）
- **cross判定が鮮度条件を満たしたサンプルで十分に発生している**（統計的検証可能なイベント数）
- **bbo_null_rate が許容範囲**（null多発市場は除外対象として確定）

---

### Phase B: 超小口IOC（実弾）- Week 13-16

**目的**: 滑り/手数料込みの実効EVを測定

**実装タスク**:
- [ ] hip3-executor実装（IOC発注、cloid冪等性）
- [ ] hip3-position実装（PositionTracker、TimeStop）
- [ ] 署名機能（Exchange endpoint用）
- [ ] **セキュリティ/鍵管理実装（P0-11）**
- [ ] Testnet実弾テスト
- [ ] Mainnet超小口テスト（$10-50/注文）

**Executor nonce/バッチング（P0-19：一次情報推奨の運用）**:

一次情報（nonces-and-api-wallets）が明示的に推奨する運用：

| 要件 | 実装 |
|------|------|
| **nonce一意性** | atomic counter（u64）、**起動時にnow_unix_msへfast-forward** |
| **バッチング周期** | **0.1秒**（100ms）周期のバッチングタスク |
| **IOC/GTC と ALO 分離** | ALO バッチを優先処理（maker優先） |

**NonceManager初期化（P0-25: 0起点禁止）**:

公式制約: nonceはブロック時刻に対する範囲制約あり `(T-2d, T+1d)`。0起点は即座に不正nonceになる。

```rust
pub struct NonceManager {
    counter: AtomicU64,
    server_time_offset_ms: AtomicI64,  // local - server 差分
}

impl NonceManager {
    /// 起動時に now_unix_ms へ fast-forward
    pub fn new() -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        Self {
            counter: AtomicU64::new(now_ms),
            server_time_offset_ms: AtomicI64::new(0),
        }
    }

    /// serverTime との同期（webData3.userState.serverTime）
    pub fn sync_with_server(&self, server_time_ms: u64) {
        let local_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let offset = local_ms as i64 - server_time_ms as i64;

        // 閾値超え（例: 2秒）で警告/停止
        if offset.abs() > 2000 {
            log::warn!("Time drift detected: {}ms", offset);
        }
        self.server_time_offset_ms.store(offset, Ordering::SeqCst);
    }

    pub fn next(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::SeqCst)
    }
}
```

**serverTime同期**: `webData3.userState.serverTime` を購読し、ローカル時刻ドリフトを監視（閾値超えで停止/再同期）

pub struct BatchConfig {
    pub interval_ms: u64,        // 100ms
    pub alo_priority: bool,       // ALO優先
    pub max_batch_size: usize,    // 上限（inflight考慮）
}
```

**「短時間でフラット化」では注文密度が上がるため、P0として組み込み必須**。

**セキュリティ/鍵管理（P0-11：Phase B必須）**:

| 項目 | 設計 |
|------|------|
| **API wallet / agent key分離** | 観測用（読み取りのみ）と取引用（署名権限）を分離 |
| **secret配置** | docker env直書き回避 → ファイルマウント or secret store（vault等） |
| **ローテーション手順** | 定期ローテーション + 漏洩時の緊急ローテーション手順を文書化 |
| **漏洩時停止手順** | 検知→全注文キャンセル→取引停止→key無効化→新key発行 |
| **署名nonce/timestamp** | サーバ時刻との同期チェック（時刻ズレ検知と整合） |

```rust
pub struct KeyManager {
    pub read_only_key: Option<ApiKey>,  // Phase A: 観測用
    pub trading_key: ApiKey,             // Phase B: 取引用
    pub key_rotation_interval: Duration,
    pub last_rotation: Instant,
}
```

**パラメータ**:
- `SIZE_ALPHA = 0.05`（Phase Aの半分）
- `MAX_NOTIONAL_PER_MARKET = $100`（超保守的）
- top-of-book連動の小サイズで入り、即reduce-onlyで戻す

**成果物**:
- 実効スリッページ（expected vs actual）
- fill率（accepted/rejected/timeout）
- フラット化品質（flat_time_ms）
- **手数料+滑り込みでedgeが残るか否かの判定**

**Phase C移行条件**:
- 100回以上の実弾トレード完了
- 手数料+滑り込みでedge正を確認
- 重大な停止漏れなし

---

### Phase C: 停止品質の改善 - Week 17-20

**目的**: テール対策の強化、例外ケースの網羅

**実装タスク**:
- [ ] OI cap検知の強化（`perpsAtOpenInterestCap` REST polling）
- [ ] DEX status監視（`perpDexStatus` 定期取得）
- [ ] ParamChange検知の精度向上（tick/lot/leverage/fee全て）
- [ ] SpreadShock Gate閾値のAdaptive化（市場別EWMA）
- [ ] 異常検知時のGraceful Degradation実装
- [ ] アラート設定（Slack/Discord通知）

**追加Gate検討**:
- `VolumeAnomaly`: 出来高急変時の警戒
- `PriceJump`: 価格急変時の一時停止
- `CorrelationBreak`: 関連市場との乖離検知

**停止→復帰の状態機械（P0-10：Gate別復帰条件）**:

Gate種別によって復帰条件が異なるため、状態遷移表を明文化：

| 停止理由 | 自動復帰条件 | 手動復帰条件 | cooldown |
|----------|-------------|-------------|----------|
| OracleFresh | oracle更新受信 | - | 0s |
| NoBboUpdate | bbo更新受信 | - | 0s |
| NoAssetCtxUpdate | assetCtx更新受信 | - | 0s |
| SpreadShock | spreadがEWMA×k以下に戻る | - | 5s |
| ParamChange | re-fetch成功 + 整合チェックOK | 複数回連続変更時 | 30s |
| Halt | DEX status正常化 | - | 60s |
| BufferLow | buffer回復 | - | 0s |
| TimeRegression | 再接続成功 | - | 0s |
| RateLimited | backoff完了 | 連続発生時 | 10-60s |
| CircuitOpen | cooldown経過 | - | 60s |

**復帰シーケンス（共通）**:
1. 復帰条件を満たす
2. cooldown待機
3. 状態チェック（READY条件再確認）
4. 新規エントリ許可

**成果物**:
- Gate発火統計（reason別、誤検知率）
- 停止→復帰シーケンスの安定性検証
- **テール損失の発生頻度・規模の把握**

**Phase D移行条件**:
- 1000回以上の実弾トレード完了
- テール損失が想定内に収まる
- Gate誤検知率が許容範囲

---

### Phase D: 対象市場の自動入れ替え - Week 21-24

**目的**: 運用の自動化、スケール

**実装タスク**:
- [ ] 市場ランキングの自動計算（rolling統計）
- [ ] 上位N市場のみ稼働（動的切り替え）
- [ ] ブラックリスト管理（oracle stall多発・halt多発等）
- [ ] 資金配分の自動調整（市場別max_notional）
- [ ] 複数MarketKeyの並行運用
- [ ] ダウンタイム最小化（設定変更のホットリロード）

**追加機能検討**:
- `l2Book`併用による検知高速化
- 複数DEXへの拡張（xyz以外）
- Position skew管理（在庫偏り検知）

**成果物**:
- 自動運用の安定稼働実績
- 市場入れ替え時のPnL影響分析
- **運用負荷の削減（手動介入頻度）**

---

### Phase E（将来）: 収益最大化

**検討事項**:
- サイズ拡大（`SIZE_ALPHA`、`MAX_NOTIONAL`の段階的引き上げ）
- 低遅延化（RTT最小リージョンへの移行）
- Spread Shock Harvest戦略の追加（8.2）
- 在庫管理の高度化（MM要素の導入）

---

## 8. フェーズ間マイルストーン

| Phase | 期間 | 目標 | Go/No-Go判定 |
|-------|------|------|-------------|
| A | Week 1-12 | 観測・市場ランキング | EV正の市場が2-3個 |
| B | Week 13-16 | 超小口実弾・滑り測定 | edge残存確認 |
| C | Week 17-20 | 停止品質改善 | テール損失許容内 |
| D | Week 21-24 | 自動運用・スケール | 安定稼働実績 |

**総期間**: 約6ヶ月（24週間）

---

## 9. リスクと撤退基準

### 撤退判断基準
- Phase A: 12週間観測してEV正の市場がゼロ → 戦略見直し or 撤退
- Phase B: 100トレードでedge負 → Phase Aに戻り閾値見直し
- Phase C: テール損失が資金の10%超 → 運用停止・Gate見直し

### 継続判断基準
- 各フェーズで明確なGo/No-Go判定
- データに基づく意思決定（感覚ではなくメトリクス）
- 損失許容額を事前に設定

---

## 10. 重要ファイル参照

| ファイル | 用途 |
|----------|------|
| `about_hip3.md` Section 8.8 | Oracle/Mark Dislocation Takerの詳細実装定義 |
| `about_ws.md` | 自前WebSocket実装の要件 |
| `.claude/rules/typescript/websocket.md` | WS再接続パターン参照 |

---

## 11. 非交渉ライン

1. **冪等性**: cloid（client order id）必須、再送で二重発注しない
2. **停止優先**: 例外時は「継続」ではなく「縮小/停止」に倒す
3. **READY条件Phase分離**: READY-MD（観測用）とREADY-TRADING（取引用）を分離、Phaseに応じた購読管理
4. **可観測性**: Day 1からメトリクス・ログを計測
5. **仕様変更検知**: tick/lot/fee変更は「異常」として即停止
6. **spec_version付与**: 全イベントにspec_versionを刻む（後解析の再現性）
7. **FeedHealth分離**: 接続健全性（再接続）と市場健全性（新規禁止/保有時は縮小→停止）を区別
8. **post inflight分離**: メッセージ数とinflight postは別セマフォで管理
9. **Heartbeat無受信基準**: 45秒無受信でping（60秒ルールの安全マージン）
10. **静的プリフライトチェック**: 起動時に購読数/制限を検証、超過時は起動拒否
11. **Decimal精度保持**: TriggerEvent等でf64ではなくDecimal使用、PnL/edge分析の再現性確保
12. **single-instance方針**: 1アカウント1インスタンス、分散時は集中レート管理必須
13. ~~MAX_SKEW時刻整合~~（撤回）→ **鮮度（age）ベース判定**: `bbo_age_ms`/`ctx_age_ms` が閾値超の場合はcross判定を見送り
14. **monotonic鮮度ベース**: activeAssetCtxに時刻フィールドがないため、全ての鮮度判定はローカル受信時刻（monotonic）ベース
15. **アクション・バジェット**: Executorにbackoff + サーキットブレーカを必須導入（address-based制限対応）
16. **初回bbo未達ポリシー**: タイムアウト（例:10秒）で市場除外、READY待ちで詰まらせない
17. **レート制限会計**: 送信/抑制/inflight/rejected/circuitをメトリクスで可視化
18. **停止→復帰状態機械**: Gate別の復帰条件と cooldown を明文化
19. **セキュリティ/鍵管理**: Phase B前に権限分離・ローテーション・漏洩時手順を確定
20. **OracleFresh定義**: 「oraclePx変化時刻」ではなく「ctx受信鮮度」中心に判定
21. **BboNull判定**: bestBid/bestAsk が null の場合はBboNull状態（READY除外）、Halt判定は別系
22. **xyz DEX同定**: Preflightで perpDexs から dex name を確定、空文字送信禁止
23. **TimeRegressionチャネル別**: timeフィールド有チャネルのみ適用、無は受信時刻age
24. **レート制限優先順位**: reduceOnly > cancel > new post（安全側優先）
25. **nonce/バッチング**: 100ms周期、IOC/GTCとALO分離
26. **READY除外再評価**: Phase A はTTL再評価、Phase B は原則対象外
27. **ID体系固定**: post.id（monotonic u64）と cloid（correlation_id由来）を分離
28. **価格精度ルール**: 固定tick禁止、5有効桁 + max decimals制約を使用
29. **HIP-3手数料2x**: userFees取得 + HIP-3倍率反映、FeeMetadataで計算根拠を残す
30. **nonce fast-forward**: 0起点禁止、now_unix_msで初期化、serverTimeでドリフト監視
31. **coin衝突検知**: xyz DEXとデフォルトDEXで同名coin衝突があれば起動拒否
32. **WS購読dexスコープ**: bbo/activeAssetCtxはdex指定不可、Coin-AssetId一意性を起動条件として検証
33. **format_price/sizeテストベクタ**: 文字列表現まで固定、edge判定は丸め後で評価
34. **ActionBudget制御アルゴリズム**: 優先度付きキュー + token bucket、過負荷時はReducePosition優先・NewOrder抑制
35. **Perps/Spot混在封じ**: activeAssetCtxがspot型なら購読対象から除外、ログ＋メトリクス即時検知
36. **Phase A DoD必須**: 24h連続稼働・レート制限観測・日次指標出力をPhase B移行前に達成
37. **Go条件4項目**: 仕様TODO=0、Preflight堅牢性、WS健全性証明、Perps/Spot封じ
38. **Nonce制約遵守**: (T-2d, T+1d)範囲 + 最大100個の高nonce保持を前提に設計

---

## 12. P1項目（改善すべき）

### P1-1: 起動時のxyz HIP-3市場ID自己テスト
- perpDexs / metaAndAssetCtxs でxyz DEX universeを取得
- 購読予定のcoinシンボルがxyz-deployed HIP-3市場に正しくマッピングされるか検証
- WS `bbo` が同一assetのmark/oracleと対応しているか確認

### ~~P1-2~~ → P0-21: post.id と cloid のID体系を固定

**ID体系（P0-21：運用ルール固定）**:

| ID | 用途 | 生成方法 |
|----|------|----------|
| `post.id` | WS post request/response相関 | **プロセス内 monotonic（u64）** |
| `cloid` | 注文冪等性 | **correlation_id から決定的生成**（hash → 128-bit hex） |

**重要な違い**:
- 再送しても **cloid は同一**（冪等性維持）
- 再送しても **post.id は別**（inflight追跡は別）

```rust
pub fn generate_cloid(correlation_id: &str) -> String {
    // correlation_id からハッシュ生成 → 128-bit hex
    let hash = blake3::hash(correlation_id.as_bytes());
    hex::encode(&hash.as_bytes()[..16])  // 128-bit = 16 bytes
}

pub struct PostIdGenerator {
    counter: AtomicU64,
}

impl PostIdGenerator {
    pub fn next(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::SeqCst)
    }
}
```

### P1-3: Oracle stale閾値の市場別設定
- 固定8000msではなく、市場ごとにオーバーライド可能
- Phase Aでoracle更新間隔分布（p50/p95）を測定し、データから閾値設定

### P1-4: Prometheusのmarket_keyラベル方針
- `{market_key}` を広く付けるとカーディナリティ爆発でPrometheus/Grafanaが重くなる
- **推奨**:
  - 原因別カウンタ/ヒストグラムはmarket_keyを外す、または上位N市場のみ
  - 市場別の深掘りはログ/ClickHouse側に寄せる

### P1-5: ParamChangeの「停止→復帰」シーケンス定義
- 停止後の復帰フローが未定義 → 明文化必須
- **自動復帰条件**:
  - 再fetch → 整合チェック → cooldown（例: 30秒）
- **人手介入条件**:
  - 複数回連続でparamが変わる、または整合が取れない
- **復帰時シーケンス**:
  1. 全注文キャンセル
  2. 新仕様でspec_version更新
  3. 再クォート開始

### P1-6: WSメッセージの重複・並べ替え耐性
- `TimeRegression` に加えて、「同一時刻」「軽微な順序入替」「再接続での再送」に対応
- **推奨**:
  - チャネル別に `last_processed_time` を持ち、同値・逆行をdrop/acceptどちらにするか明文化
  - userFills/orderUpdatesは `isSnapshot` と併せて重複排除キーを固定

**WS snapshot/重複処理ポリシー（P0-9：チャネル別固定）**:

| チャネル | isSnapshot=true時 | 重複/再接続時 | 基準 |
|----------|-------------------|---------------|------|
| bbo | N/A（snapshotなし） | 同一time→accept（上書き）、逆行→drop+warn | `last_bbo_time` |
| assetCtx | N/A | 同一time→accept、逆行→drop+warn | `last_ctx_time` |
| orderUpdates | **状態リセット→再構築** | 重複orderIdはaccept（最新を採用） | `orderId` |
| userFills | **状態リセット→再構築** | 重複fillIdはdrop | `fillId` |

**再接続時のシーケンス**:
1. 全state（bbo/assetCtx/orders/fills）をクリア
2. READY状態をリセット
3. 再購読→snapshot待ち→READY遷移

### ~~P1-7~~ → P0-15: xyz限定のDEX同定をPreflightで一意化

**重要**: HIP-3 は builder-deployed perp DEX を扱うため、dex 指定の取り扱いを誤ると「別DEXを観測・取引」する事故に直結。

**必須対応（P0に格上げ）**:
1. 起動時に `info(type="perpDexs")` で DEX 一覧を取得
2. 対象（xyz）DEXを**dex name**で確定
3. 以後の `info` / `subscribe` で**確定した dex name を常に明示的に付与**
4. **空文字事故の防止**: `dex` は "空文字=first perp dex" というデフォルト挙動があるため、空文字のまま送信しない

```rust
pub struct PreflightChecker {
    pub target_dex_name: String,     // "xyz" を perpDexs から取得
    pub target_coin_universe: HashSet<String>,  // xyz DEXのcoin集合
}

impl PreflightChecker {
    /// P0-26: dex取り違え事故防止（同名coin衝突で起動拒否）
    pub async fn validate_dex_universe(&mut self) -> Result<(), PreflightError> {
        // 1. xyz DEXのuniverse取得（dex="xyz"を明示）
        let xyz_universe = fetch_universe_with_dex("xyz").await?;

        // 2. デフォルトDEX（空文字）のuniverse取得
        let default_universe = fetch_universe_with_dex("").await?;

        // 3. 同名coin衝突チェック
        let xyz_coins: HashSet<_> = xyz_universe.iter().map(|m| &m.coin).collect();
        let default_coins: HashSet<_> = default_universe.iter().map(|m| &m.coin).collect();

        let collision: Vec<_> = xyz_coins.intersection(&default_coins).collect();
        if !collision.is_empty() {
            // 同名coinが存在 → 取り違えリスク → 起動拒否
            return Err(PreflightError::CoinNameCollision {
                coins: collision.into_iter().cloned().cloned().collect(),
            });
        }

        self.target_coin_universe = xyz_coins.into_iter().cloned().collect();
        Ok(())
    }

    pub fn is_valid_coin(&self, coin: &str) -> bool {
        self.target_coin_universe.contains(coin)
    }
}
```

**WS購読のdexスコープ（P0-27：dex指定できないチャネルの扱い）**:

一次情報によると、`bbo` / `activeAssetCtx` の購読メッセージは**coin指定のみ**で、**dexを明示できない**。

| チャネル | dex指定 | 対策 |
|----------|---------|------|
| `bbo` | **不可**（coinのみ） | **Coin-AssetId一意性で担保** |
| `activeAssetCtx` | **不可**（coinのみ） | **Coin-AssetId一意性で担保** |
| `allMids` | 可（dexパラメータあり） | dex明示必須 |
| info REST系 | 可 | dex明示必須、空文字禁止 |

**仕様として固定**:
1. **WS購読でdex指定できないチャネルはcoin一意性で担保**
   - 起動時に `perpDexs/meta` で `Coin-AssetId` の一意性を検証
   - **衝突がある場合は起動拒否**（これが生命線）
2. **dex指定できるinfo系RESTは常にdexを明示**
   - dex省略/空文字は**first perp dexを参照し得る**ため禁止

### P1-8: ParamChange検知のポーリング仕様
- `meta` / `metaAndAssetCtxs` の取得周期: **30秒**（初期値）
- 失敗時バックオフ: 1s→2s→4s→…max 60s
- 変更検知時の「停止→復帰」手順（P1-5参照）

### P1-9: `expiresAfter` 使用時の時刻同期
`expiresAfter`を使う場合は以下をセットで導入：
- **NTP/時刻同期**: サーバ時刻とローカル時刻のズレを定期チェック
- **期限切れ多発検知**: 一定期間内にexpired多発で自動無効化
- **TTL既定値**: 5秒（conservative）
- **注意**: 誤って古い時刻で送ると拒否されるだけでなくコスト増の可能性

### P1-10: クロス判定の機会損失メトリクス
cross判定を「bbo更新時のみ」に固定する場合、以下を計測して機会損失を評価：
- `hip3_cross_skipped_total{reason=oracle_moved_bbo_stale}`: oracleが動いたがbboが動かず判定不可だった回数
- `hip3_cross_skipped_total{reason=skew_exceed}`: skew条件で見送った回数
- これらが多い場合、判定トリガーの見直しや閾値調整を検討

### P1-11: Parquet書き込みのバックプレッシャ設計
- **ログ/イベントキュー**と**執行キュー**を分離
- 書き込み遅延時の方針：
  - **停止優先**（推奨）: 書き込み遅延が閾値超→新規エントリ禁止
  - **観測ドロップ**（代替）: 古いイベントを捨てて最新を保持
- **推奨**: Taker戦略では「停止優先」を採用（分析再現性を損なわない）

### P1-12: `activeAssetCtx` の数値型（number）をログ上で再現性確保
- `bbo` は `px: string` でDecimal化しやすい
- `activeAssetCtx` の `oraclePx/markPx` は **number**（JavaScript number）
- 解析再現性のため「受信表現をto_stringで保存→Decimal化」を推奨
- Parquet/JSONLに **raw と normalized を両方保存**

### P1-13: l2Book初期シード戦略（bbo未達銘柄対策）
- `WsBook (l2Book)` はブロック単位のスナップショットで、初期シードに使える
- Phase Aでは「bbo未達銘柄は除外」で良いが、以下のオプションを検討：
  - **短時間だけl2Bookでシード→以後bboのみ**（観測対象を増やしたい場合）
  - 購読枠/メッセージ枠とのトレードオフを考慮

### P1-14: ローカルinfo server（--serve-info）の検討
- info pollingが重くなる場合、ローカルinfo serverの活用で外部依存/レート制限の緩和余地
- 参照: Hyperliquid node docs

### P1-15: ユーザー系WS（orderUpdates / userFills等）のdex混在を前提にフィルタ設計
- ユーザー系購読でdex引数が見えない場合に備え、coin/assetIdでフィルタ
- dex指定できるsnapshotと突合して整合性を担保

### P1-16: API wallet pruningを踏まえた鍵運用
- API walletのnonce状態pruningがあり得るため、**API walletの再利用は避ける**
- ローテ時は新規生成し、運用手順として固定
- 参照: Hyperliquid docs — Nonces

---

## 13. 追加推奨事項（設計強化）

### バックプレッシャポリシー
- 内部キューがバックアップした場合の方針を明示
- Taker戦略（停止品質重視）では「drop to latest」より**「新規禁止/停止」**を推奨
- 無言でメッセージを落とすのは分析の再現性を損なう

### Record/Replayテスト
- WSストリームをキャプチャし、以下を検証：
  - READYゲーティング
  - 再接続復旧
  - cloid冪等性
  - メッセージ並べ替え/バーストでのRisk Gate正確性

---

## 14. レビュー反映済みP0項目チェックリスト

**第1回レビュー（2026-01-18）**:
- [x] Heartbeatを「無受信」基準に修正
- [x] READY条件を「ACK＋snapshot適用完了」まで形式化
- [x] FeedHealth系Hard Gate追加（購読途絶、time巻き戻り）
- [x] spec_versionを全イベントに刻む
- [x] post inflight上限を別セマフォで管理
- [x] WS状態機械を明示（Disconnected→Connecting→Subscribing→Syncing→Ready）
- [x] Edge計算パラメータをFEE/SLIP/EDGEとして別フィールド保存
- [x] イベント相関ID（correlation_id）の導入
- [x] Week 5-6でGate（OracleFresh/FeedHealth/ParamChange）を先に通す

**第2回レビュー（2026-01-18）**:
- [x] P0-1: FeedHealthを「接続健全性」と「市場健全性」に分離（bbo途絶は新規禁止のみ）
- [x] P0-2: Heartbeat 45秒を60秒ルールの安全マージンとして明文化
- [x] P0-3: 静的プリフライトチェック + ランタイムゲート + single-instance方針
- [x] P0-4: TriggerEventでDecimal使用（f64は派生フィールドのみ）
- [x] P1-1〜P1-3: 追加（起動時self-test、post.id分離、閾値市場別設定）
- [x] Record/Replayテスト、バックプレッシャポリシーの追加

**第3回レビュー（2026-01-18）**:
- [x] NoBboUpdate/NoAssetCtxUpdateの矛盾解消（新規禁止のみ、ただし保有中は縮小→停止）
- [x] READY条件に「bbo/assetCtx初回受信」+ 「oracle/bbo時刻整合（MAX_SKEW）」追加
- [x] cross判定の時系列定義（MAX_SKEW方針）固定
- [x] P1-4〜P1-6追加（Prometheusラベル方針、ParamChange復帰シーケンス、WS重複耐性）
- [x] Phase B移行条件に「skew分布が許容範囲」「cross判定の統計的検証可能なイベント数」追加

**第4回レビュー（2026-01-18）**:
- [x] P0-4: READY条件をPhase別に分離（READY-MD / READY-TRADING）
- [x] P0-5: timeソースの統一ルール固定（サーバtime同士でskew計算、ローカル時刻は遅延/再接続用）
- [x] P0-6: Executorに「アクション・バジェット」導入（backoff、サーキットブレーカ、冗長キャンセル抑制）
- [x] P1-7: DEX名の正規化とxyz限定の厳密化（perpDexs.nameを正規形として使用）
- [x] P1-8: ParamChange検知のポーリング仕様固定（30秒周期、失敗時バックオフ）
- [x] P1-9: expiresAfter使用時のNTP/時刻同期要件追加

**第5回レビュー（2026-01-18）**:
- [x] P0-7: READY-MD「初回bbo未達」ポリシー（タイムアウト→市場除外）
- [x] P0-8: レート制限の"会計"メトリクス追加（sent/blocked/inflight/rejected/circuit）
- [x] P0-9: WSのsnapshot/重複処理方針をチャネル別に固定
- [x] P0-10: 停止→復帰の状態機械統一（Gate別復帰条件の状態遷移表）
- [x] P0-11: セキュリティ/鍵管理/署名運用（Phase B必須、権限分離・ローテーション・漏洩時手順）
- [x] P1-10: クロス判定の機会損失メトリクス追加
- [x] P1-11: Parquet書き込みのバックプレッシャ設計

**第6回レビュー（2026-01-18）**:
- [x] P0-12: **MAX_SKEW前提の撤回** → monotonic鮮度（age）ベースに設計変更
  - activeAssetCtxに時刻フィールドがないため、サーバtime同士の比較は不可能
  - `bbo_age_ms` / `ctx_age_ms` による鮮度判定に置き換え
  - 命名変更: ~~MAX_SKEW_MS~~ → `MAX_BBO_AGE_MS` / `MAX_CTX_AGE_MS`
- [x] P0-13: **OracleFresh定義の修正** → 「oraclePx変化時刻」ではなく「ctx受信鮮度」中心
- [x] P0-14: **bbo null（片側板なし）時の判定無効ルール追加**
- [x] P0-15: **xyz限定のDEX同定をPreflightで一意化**（P1-7から格上げ）
  - perpDexsからdex nameを確定、空文字事故防止
- [x] Phase A観測指標追加（ctx_recv_interval、bbo_recv_interval、bbo_null_rate等）

**第7回レビュー（2026-01-18）**:
- [x] P0-16: TimeRegressionの適用範囲をチャネル別に固定（timeフィールド有無で分岐）
- [x] P0-17: 「bbo両側null=Halt」を断定しない → BboNull状態として扱い、Halt判定は別系
- [x] P0-18: address-based limitヒット時の優先順位（reduceOnly > cancel > new post）
- [x] P0-19: Executor nonce/バッチング（100ms周期、IOC/GTCとALO分離）をP0に格上げ
- [x] P0-20: READY除外市場の再評価ポリシー（Phase A: TTL再評価、Phase B: 原則除外）
- [x] P0-21: post.id と cloid のID体系を固定（P1-2から格上げ）
- [x] P0-22: 実装順序の推奨（最小縦切りを先に通す）

**第8回レビュー（2026-01-18）**:
- [x] P0-23: `tick_size`固定値設計を撤回 → 価格精度ルール（5有効桁 + max decimals）に変更
  - `MarketSpec` に `sz_decimals`、`max_sig_figs`、`max_price_decimals` を追加
  - `format_price()`/`format_size()` を `hip3-core` に集約
- [x] P0-24: Feeモデルに「HIP-3は2倍」+ `userFees`反映を必須化
  - 起動時に `info type="userFees"` を取得
  - 14d volume tier、discount、HIP-3 2x を反映した実効feeを算出
  - `TriggerEvent` に計算根拠（`FeeMetadata`）を残す
- [x] P0-25: NonceManager初期化を0起点禁止 → `now_unix_ms`へfast-forward
  - `webData3.userState.serverTime` でローカル時刻ドリフトを監視
- [x] P0-26: dex取り違え事故防止を強化
  - info endpoint で dex="xyz" を明示
  - デフォルトDEXとの同名coin衝突で起動拒否
- [x] P1-12: `activeAssetCtx` の number型をログ上でraw保存（再現性）
- [x] P1-13: l2Book初期シード戦略を検討オプションとして追加

**第9回レビュー（2026-01-18）**:
- [x] P0-27: WS購読のdexスコープ（bbo/activeAssetCtxはcoin指定のみ）
  - dex指定できないチャネルはCoin-AssetId一意性で担保
  - 衝突がある場合は起動拒否（生命線）
  - dex指定できるinfo系RESTは常にdexを明示、空文字禁止
- [x] P0-28: format_price/format_sizeの「文字列表現」までテストベクタで仕様化
  - tick/lotの制約（最大有効桁、decimals上限、末尾ゼロ除去等）を先に固定
  - Detector側のedge判定は丸め後（実際に発注される値）で必ず評価
- [x] P0-29: ActionBudgetを「検知」から「制御アルゴリズム」へ責務拡張
  - send_queue（優先度付きキュー）+ token_bucket（予算配賦）
  - 過負荷時: ReducePosition/Cancelを優先、NewOrderを落とす
  - address_limit_state: Normal / Limited（10秒に1回モード）
- [x] P1-14: ローカルinfo server（--serve-info）の検討
- [x] P1-15: ユーザー系WS dex混在を前提にフィルタ設計
- [x] P1-16: API wallet pruningを踏まえた鍵運用（再利用回避）

**第10回レビュー（2026-01-18）- 実装着手判断メモ**:
- [x] P0-30: Perps/Spot混在の例外封じ
  - `activeAssetCtx` がspot型（`WsActiveSpotAssetCtx`）で返るケースへの対応
  - spot型なら購読対象から除外 + ログ + メトリクス
- [x] P0-31: Phase A DoD（Definition of Done）の明文化
  - 24h連続稼働、レート制限観測、日次指標出力をPhase B移行条件に
  - 日次出力必須指標: cross_count, bbo_null_rate, ctx_age_ms, best_age_ms, cross_duration_ticks
- [x] P0-32: 実装開始前のGo条件4項目を追加
  - Go条件1: 仕様TODOをゼロ（format_price/format_size golden test固定）
  - Go条件2: Preflightが「落ちるべき時に必ず落ちる」
  - Go条件3: WS健全性・レート制限が「観測だけで証明できる」
  - Go条件4: Perps/Spot混在の例外を封じる
- [x] 一次情報URLの明記（WebSocket、Rate limits、Nonces、Subscriptions）
- [x] 着手判断：**実装着手可**（Phase Aから開始、Phase BへはDoD達成後）
