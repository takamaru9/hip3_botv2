# HIP-3 実装継続計画

## 設定情報

| 項目 | 値 |
|------|-----|
| WebSocket URL | `wss://api.hyperliquid.xyz/ws` |
| DEX name | `xyz` |
| 対象 | HIP-3 perps only |

---

## 現状評価サマリー

**ビルド状態**: ✅ 成功（`cargo check` パス）

### 完了済み（Week 1-6相当）

| モジュール | 完了項目 |
|-----------|---------|
| hip3-core | MarketKey, Price, Size, MarketSpec, Bbo, AssetCtx, OracleData |
| hip3-ws | ConnectionManager, HeartbeatManager (45秒), SubscriptionManager, RateLimiter |
| hip3-feed | MarketState, MessageParser (bbo/assetCtx) |
| hip3-registry | SpecCache (パラメータ変更検知) |
| hip3-risk | RiskGate (Oracle/MarkMid/Spread/OiCap/ParamChange/Halt) |
| hip3-detector | DislocationDetector (edge計算、サイズ計算) |
| hip3-persistence | ParquetWriter |
| hip3-telemetry | Prometheus基本メトリクス |
| hip3-bot | Application (イベントループ) |

### 未実装P0項目（優先度順）

| 項目 | 内容 | 影響範囲 |
|------|------|----------|
| P0-23 | format_price/format_size（5有効桁+max decimals） | hip3-core |
| P0-28 | format_price/sizeテストベクタ | hip3-core |
| P0-15 | xyz DEX同定（Preflight） | hip3-registry |
| P0-27 | Coin-AssetId一意性検証 | hip3-registry |
| P0-30 | Perps/Spot混在封じ | hip3-feed |
| P0-4 | READY-MD/READY-TRADING分離 | hip3-ws |
| P0-14 | BboNull判定 | hip3-feed, hip3-core |
| P0-7 | 初回bbo未達タイムアウト | hip3-ws |
| P0-12 | monotonic鮮度判定完全化 | hip3-feed |
| P0-24 | HIP-3手数料2x + userFees | hip3-detector |
| P0-8 | レート制限メトリクス | hip3-ws, hip3-telemetry |

---

## 実装タスク（詳細）

### Task 1: MarketSpec精度ルール実装（P0-23, P0-28）

**ファイル**: `crates/hip3-core/src/market.rs`

```rust
pub struct MarketSpec {
    // 既存フィールド...

    // 追加フィールド
    pub sz_decimals: u8,           // サイズ小数桁（例: 3）
    pub max_sig_figs: u8,          // 最大有効桁（常に5）
    pub max_price_decimals: u8,    // MAX_DECIMALS(6) - sz_decimals
}

impl MarketSpec {
    /// 価格を正規化（5有効桁 + max decimals制約 + 末尾ゼロ除去）
    pub fn format_price(&self, price: Decimal) -> String { ... }

    /// サイズを正規化（sz_decimals桁 + 末尾ゼロ除去）
    pub fn format_size(&self, size: Decimal) -> String { ... }
}
```

**テストベクタ**:
- 50123.456789 (sz_decimals=3) → "50123"
- 0.00012345 (sz_decimals=0) → "0.00012345"

### Task 2: Preflight実装（P0-15, P0-27）

**新規ファイル**: `crates/hip3-registry/src/preflight.rs`

```rust
pub struct PreflightChecker {
    pub target_dex_name: String,
    pub target_coin_universe: HashSet<String>,
}

impl PreflightChecker {
    pub async fn validate_dex_universe(&mut self) -> Result<(), PreflightError> {
        // 1. perpDexs取得
        // 2. xyz DEX同定
        // 3. Coin-AssetId一意性検証
        // 4. 衝突あれば起動拒否
    }
}
```

### Task 3: Perps/Spot混在封じ（P0-30）

**ファイル**: `crates/hip3-feed/src/parser.rs`

```rust
pub enum ActiveAssetCtxResponse {
    Perps(RawAssetCtx),
    Spot(RawSpotAssetCtx),
}

// parse_asset_ctx内でspot型を検出→除外+メトリクス
```

### Task 4: READY条件強化（P0-4, P0-7, P0-14）

**ファイル**: `crates/hip3-ws/src/subscription.rs`

```rust
pub enum ReadyPhase {
    MarketDataOnly,  // Phase A用
    Trading,         // Phase B用
}

pub struct MarketReadyState {
    pub bbo_received: bool,
    pub ctx_received: bool,
    pub bbo_age_valid: bool,
    pub ctx_age_valid: bool,
    pub bbo_timeout_excluded: bool,
}
```

**ファイル**: `crates/hip3-core/src/types.rs`

```rust
pub enum BboState {
    Valid { bid: Level, ask: Level },
    BidNull { ask: Level },
    AskNull { bid: Level },
    BothNull,
}
```

### Task 5: monotonic鮮度判定（P0-12）

**ファイル**: `crates/hip3-feed/src/market_state.rs`

```rust
pub struct MarketStateEntry {
    // 既存...
    pub bbo_recv_mono: Instant,    // monotonic受信時刻
    pub ctx_recv_mono: Instant,    // monotonic受信時刻
}

impl MarketStateEntry {
    pub fn bbo_age_ms(&self) -> u64 {
        self.bbo_recv_mono.elapsed().as_millis() as u64
    }

    pub fn ctx_age_ms(&self) -> u64 {
        self.ctx_recv_mono.elapsed().as_millis() as u64
    }
}
```

### Task 6: HIP-3手数料（P0-24）

**新規ファイル**: `crates/hip3-detector/src/fees.rs`

```rust
pub struct FeeMetadata {
    pub base_taker_bps: Decimal,
    pub tier_discount_pct: Decimal,
    pub hip3_multiplier: Decimal,  // 2.0
    pub effective_taker_bps: Decimal,
}

pub async fn fetch_user_fees() -> Result<FeeMetadata, Error> {
    // info type="userFees" REST取得
}
```

### Task 7: レート制限メトリクス（P0-8）

**ファイル**: `crates/hip3-telemetry/src/metrics.rs`

```rust
// 追加メトリクス
pub static WS_MSGS_SENT_TOTAL: Lazy<CounterVec>
pub static WS_MSGS_BLOCKED_TOTAL: Lazy<CounterVec>
pub static POST_INFLIGHT: Lazy<Gauge>
pub static CROSS_SKIPPED_TOTAL: Lazy<CounterVec>
```

---

## 実装順序

1. **Task 1** (MarketSpec) - 他タスクの基盤
2. **Task 2** (Preflight) - 起動条件
3. **Task 3** (Perps/Spot) - データ品質
4. **Task 5** (monotonic鮮度) - cross判定の正確性
5. **Task 4** (READY条件) - 状態管理
6. **Task 6** (手数料) - edge計算精度
7. **Task 7** (メトリクス) - 可観測性

---

## 検証手順

1. **単体テスト**: 各モジュールの`cargo test`
2. **ビルド確認**: `cargo check && cargo clippy`
3. **Testnet接続**: config/testnet.tomlでWS接続確認
4. **観測確認**: bbo/assetCtx受信ログ確認
5. **メトリクス確認**: Prometheus endpoint確認

---

## 修正対象ファイル一覧

| ファイルパス | 変更種別 |
|-------------|---------|
| crates/hip3-core/src/market.rs | 改修（format_price/size追加） |
| crates/hip3-core/src/types.rs | 改修（BboState追加） |
| crates/hip3-registry/src/preflight.rs | 新規 |
| crates/hip3-registry/src/lib.rs | 改修（preflight追加） |
| crates/hip3-feed/src/parser.rs | 改修（spot除外） |
| crates/hip3-feed/src/market_state.rs | 改修（monotonic時刻） |
| crates/hip3-ws/src/subscription.rs | 改修（ReadyPhase） |
| crates/hip3-detector/src/fees.rs | 新規 |
| crates/hip3-detector/src/lib.rs | 改修（fees追加） |
| crates/hip3-telemetry/src/metrics.rs | 改修（メトリクス追加） |
| crates/hip3-bot/src/app.rs | 改修（Preflight呼び出し） |
| config/default.toml | 改修（ws_url更新） |

---

## 設定ファイル更新

### config/default.toml

```toml
ws_url = "wss://api.hyperliquid.xyz/ws"
```

### config/testnet.toml

```toml
ws_url = "wss://api.hyperliquid-testnet.xyz/ws"
```

---

## 参照元プラン

オリジナルプラン: `/Users/taka/crypto_trading_bot/hip3_botv2/.claude/plans/2026-01-18-oracle-dislocation-taker.md`

本計画はオリジナルプランのPhase A Week 5-8相当の実装を継続する。
