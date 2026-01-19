# HIP-3 Oracle/Mark Dislocation Taker Implementation Spec

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-01-18 |
| Last Updated | 2026-01-18 (Session 4) |
| Status | `[IN_PROGRESS]` |
| Source Plan | `.claude/plans/2026-01-18-oracle-dislocation-taker.md` |

---

## Implementation Status Overview

### Phase A: 観測のみ（Week 1-12）

| Week | 予定項目 | 状態 | 備考 |
|------|---------|------|------|
| 1-2 | 基盤構築 | [x] DONE | Cargo workspace + hip3-core |
| 3-4 | WebSocket完成 | [x] DONE | 接続・購読・再接続 |
| 5-6 | Feed + Registry + Gate | [x] DONE | 全Gate実装完了 (P0-8, P0-12, P0-16) |
| 7-8 | 残りGate + Detector | [x] DONE | Detector + 全メトリクス実装完了 |
| 9-10 | 統合とテスト | [x] DONE | Testnet WS接続確認済 |
| 11-12 | 分析 | [ ] TODO | |

---

## P0項目 Implementation Status

### 完了済み P0項目

| ID | 項目 | 状態 | 実装ファイル |
|----|------|------|-------------|
| P0-4 | READY-MD/READY-TRADING分離 | [x] DONE | `hip3-ws/src/subscription.rs` |
| P0-7 | 初回bbo未達タイムアウト | [x] DONE | `hip3-ws/src/subscription.rs` |
| P0-12 | monotonic鮮度判定 | [x] DONE | `hip3-ws/src/subscription.rs`, `hip3-feed/src/market_state.rs` |
| P0-14 | BboNull判定 | [x] DONE | `hip3-core/src/types.rs` |
| P0-15 | xyz DEX同定（Preflight） | [x] DONE | `hip3-registry/src/preflight.rs` |
| P0-23 | format_price/format_size | [x] DONE | `hip3-core/src/market.rs` |
| P0-24 | HIP-3手数料2x + userFees | [x] DONE | `hip3-detector/src/fee.rs` |
| P0-26 | perpDexs API市場取得 | [x] DONE | `hip3-registry/src/client.rs` |
| P0-27 | Coin-AssetId一意性検証 | [x] DONE | `hip3-registry/src/preflight.rs` |
| P0-31 | Phase A DoD指標出力 | [x] DONE | `hip3-telemetry/src/daily_stats.rs`, `cross_tracker.rs` |
| P0-28 | format_price/sizeテストベクタ | [x] DONE | `hip3-core/src/market.rs` (tests) |
| P0-30 | Perps/Spot混在封じ | [x] DONE | `hip3-feed/src/parser.rs` |
| P0-8 | レート制限メトリクス | [x] DONE | `hip3-telemetry/src/metrics.rs` |
| P0-16 | TimeRegression検出 | [x] DONE | `hip3-risk/src/gates.rs` |

### 部分完了 P0項目

現在、部分完了のP0項目はありません。

### 未実装 P0項目

| ID | 項目 | 状態 | 優先度 | 備考 |
|----|------|------|--------|------|
| P0-29 | ActionBudget制御アルゴリズム | [ ] TODO | Phase B | 優先度キュー + token bucket |

---

## Module Implementation Status

### hip3-core

| 項目 | 状態 | ファイル | テスト |
|------|------|----------|--------|
| MarketKey | [x] DONE | `market.rs` | ✓ |
| DexId / AssetId | [x] DONE | `market.rs` | ✓ |
| Price / Size | [x] DONE | `decimal.rs` | ✓ |
| MarketSpec | [x] DONE | `market.rs` | ✓ |
| format_price | [x] DONE | `market.rs` | ✓ Golden tests |
| format_size | [x] DONE | `market.rs` | ✓ Golden tests |
| BboState | [x] DONE | `types.rs` | ✓ |
| Bbo | [x] DONE | `types.rs` | ✓ |
| OracleData | [x] DONE | `types.rs` | ✓ |
| AssetCtx | [x] DONE | `types.rs` | ✓ |
| MarketSnapshot | [x] DONE | `types.rs` | ✓ |
| OrderSide | [x] DONE | `order.rs` | ✓ |

### hip3-ws

| 項目 | 状態 | ファイル | テスト |
|------|------|----------|--------|
| ConnectionManager | [x] DONE | `connection.rs` | ✓ |
| HeartbeatManager (45秒) | [x] DONE | `heartbeat.rs` | ✓ |
| RateLimiter | [x] DONE | `rate_limiter.rs` | ✓ |
| SubscriptionManager | [x] DONE | `subscription.rs` | ✓ |
| ReadyPhase (MD/Trading) | [x] DONE | `subscription.rs` | ✓ |
| MarketReadyState | [x] DONE | `subscription.rs` | ✓ |
| BBO timeout policy | [x] DONE | `subscription.rs` | ✓ |
| Freshness check | [x] DONE | `subscription.rs` | ✓ |

### hip3-feed

| 項目 | 状態 | ファイル | テスト |
|------|------|----------|--------|
| MessageParser | [x] DONE | `parser.rs` | ✓ |
| Spot rejection (P0-30) | [x] DONE | `parser.rs` | ✓ |
| SpotRejectionStats | [x] DONE | `parser.rs` | ✓ |
| MarketState | [x] DONE | `market_state.rs` | ✓ |
| MarketStateEntry | [x] DONE | `market_state.rs` | ✓ |
| Oracle tracking | [x] DONE | `market_state.rs` | ✓ |
| BBO monotonic freshness (P0-12) | [x] DONE | `market_state.rs` | ✓ bbo_recv_mono |
| Ctx monotonic freshness (P0-12) | [x] DONE | `market_state.rs` | ✓ ctx_recv_mono |
| BBO server_time tracking (P0-16) | [x] DONE | `market_state.rs` | ✓ bbo_server_time |

### hip3-registry

| 項目 | 状態 | ファイル | テスト |
|------|------|----------|--------|
| SpecCache | [x] DONE | `spec_cache.rs` | ✓ |
| PreflightChecker | [x] DONE | `preflight.rs` | ✓ |
| xyz DEX識別 | [x] DONE | `preflight.rs` | ✓ |
| Coin一意性検証 | [x] DONE | `preflight.rs` | ✓ |
| MetaClient | [x] DONE | `client.rs` | - |
| perpDexs API取得 | [x] DONE | `client.rs` | ✓ Mainnet確認 |
| UserState | [x] DONE | `user_state.rs` | - |

### hip3-risk

| 項目 | 状態 | ファイル | テスト |
|------|------|----------|--------|
| RiskGate trait | [x] DONE | `gates.rs` | ✓ |
| OracleFresh | [x] DONE | `gates.rs` | ✓ |
| MarkMidDivergence | [x] DONE | `gates.rs` | ✓ |
| SpreadShock | [x] DONE | `gates.rs` | ✓ |
| OiCap | [x] DONE | `gates.rs` | ✓ |
| ParamChange | [x] DONE | `gates.rs` | ✓ |
| Halt | [x] DONE | `gates.rs` | ✓ |
| NoBboUpdate (P0-12) | [x] DONE | `gates.rs` | ✓ max_bbo_age_ms=2000ms |
| NoAssetCtxUpdate (P0-12) | [x] DONE | `gates.rs` | ✓ max_ctx_age_ms=8000ms |
| TimeRegression (P0-16) | [x] DONE | `gates.rs` | ✓ server_time比較 |

### hip3-detector

| 項目 | 状態 | ファイル | テスト |
|------|------|----------|--------|
| DislocationDetector | [x] DONE | `detector.rs` | - |
| DetectorConfig | [x] DONE | `config.rs` | - |
| Signal | [x] DONE | `signal.rs` | - |
| FeeCalculator | [x] DONE | `fee.rs` | ✓ |
| FeeMetadata | [x] DONE | `fee.rs` | ✓ |
| UserFees | [x] DONE | `fee.rs` | ✓ |

### hip3-telemetry

| 項目 | 状態 | ファイル | テスト |
|------|------|----------|--------|
| WS_CONNECTED | [x] DONE | `metrics.rs` | - |
| WS_RECONNECT_TOTAL | [x] DONE | `metrics.rs` | - |
| FEED_LATENCY_MS | [x] DONE | `metrics.rs` | - |
| TRIGGERS_TOTAL | [x] DONE | `metrics.rs` | - |
| EDGE_BPS | [x] DONE | `metrics.rs` | - |
| GATE_BLOCKED_TOTAL | [x] DONE | `metrics.rs` | - |
| ORACLE_STALE_RATE | [x] DONE | `metrics.rs` | - |
| ORACLE_AGE_MS | [x] DONE | `metrics.rs` | - |
| SPREAD_BPS | [x] DONE | `metrics.rs` | - |
| WS_MSGS_SENT_TOTAL | [x] DONE | `metrics.rs` | P0-8 |
| WS_MSGS_BLOCKED_TOTAL | [x] DONE | `metrics.rs` | P0-8 |
| POST_INFLIGHT | [x] DONE | `metrics.rs` | P0-8 |
| POST_REJECTED_TOTAL | [x] DONE | `metrics.rs` | P0-8 |
| ACTION_BUDGET_CIRCUIT_OPEN | [x] DONE | `metrics.rs` | P0-8 |
| ADDRESS_LIMIT_HIT_TOTAL | [x] DONE | `metrics.rs` | P0-8 |
| CROSS_SKIPPED_TOTAL | [x] DONE | `metrics.rs` | P0-8 Cross判定Skip理由 |
| BBO_AGE_MS | [x] DONE | `metrics.rs` | P0-8 monotonic |
| CTX_AGE_MS | [x] DONE | `metrics.rs` | P0-8 monotonic |
| CROSS_COUNT_TOTAL | [x] DONE | `metrics.rs` | P0-31 |
| BBO_NULL_RATE | [x] DONE | `metrics.rs` | P0-31 |
| CrossTracker | [x] DONE | `cross_tracker.rs` | P0-31 |
| DailyStatsReporter | [x] DONE | `daily_stats.rs` | P0-31 |

### hip3-persistence

| 項目 | 状態 | ファイル | テスト |
|------|------|----------|--------|
| ParquetWriter | [x] DONE | `writer.rs` | - |

### hip3-bot

| 項目 | 状態 | ファイル | テスト |
|------|------|----------|--------|
| Application | [x] DONE | `app.rs` | - |
| Config | [x] DONE | `config.rs` | - |
| main | [x] DONE | `main.rs` | - |

### hip3-executor (Phase B)

| 項目 | 状態 | ファイル | テスト |
|------|------|----------|--------|
| スケルトン | [x] DONE | `lib.rs` | - |
| IOC執行 | [ ] TODO | - | Phase B |
| NonceManager | [ ] TODO | - | P0-25 |
| Batching | [ ] TODO | - | P0-19 |

### hip3-position (Phase B)

| 項目 | 状態 | ファイル | テスト |
|------|------|----------|--------|
| スケルトン | [x] DONE | `lib.rs` | - |
| PositionTracker | [ ] TODO | - | Phase B |
| TimeStop | [ ] TODO | - | Phase B |

---

## Deviations from Plan

### 計画からの逸脱（なし）
現時点で計画からの大きな逸脱はありません。

---

## Key Implementation Details

### format_price/format_size (P0-23, P0-28)

```rust
// 実装場所: hip3-core/src/market.rs
// 5有効桁 + max_price_decimals制約 + 末尾ゼロ除去
pub fn format_price(&self, price: Price, is_buy: bool) -> String
pub fn format_size(&self, size: Size) -> String
```

Golden tests実装済み:
- 50123.456789 (sz_decimals=3) → "50123"
- 0.00012345 (sz_decimals=0) → "0.00012345"
- Buy方向: ceil rounding
- Sell方向: floor rounding

### READY条件分離 (P0-4)

```rust
// 実装場所: hip3-ws/src/subscription.rs
pub enum ReadyPhase {
    NotReady,      // 初期化中
    ReadyMD,       // Phase A観測モード (bbo + assetCtx)
    ReadyTrading,  // Phase B取引モード (+ orderUpdates)
}
```

### Perps/Spot混在封じ (P0-30)

```rust
// 実装場所: hip3-feed/src/parser.rs
pub enum ChannelType {
    Perp,    // 許可
    Spot,    // 拒否 → FeedError::SpotRejected
    Unknown, // 無視
}
```

SpotRejectionStats でメトリクス追跡。

### Hyperliquid WebSocket Format

```rust
// 実装場所: hip3-feed/src/parser.rs

// Subscription format:
// {"method": "subscribe", "subscription": {"type": "bbo", "coin": "BTC"}}
// {"method": "subscribe", "subscription": {"type": "activeAssetCtx", "coin": "BTC"}}

// BBO response: {"channel": "bbo", "data": {"coin": "BTC", "bbo": [[px,sz,n], [px,sz,n]]}}
pub struct HyperliquidBbo {
    pub coin: String,
    pub time: Option<i64>,
    pub bbo: (Option<HyperliquidLevel>, Option<HyperliquidLevel>),
}

// AssetCtx response: {"channel": "activeAssetCtx", "data": {"coin": "BTC", "ctx": {...}}}
pub struct HyperliquidAssetCtx {
    pub coin: String,
    pub ctx: HyperliquidCtxData,  // All fields as String (not f64)
}
```

Coin→AssetId マッピング: BTC=0, ETH=1, SOL=2 (config/testnet.toml)

### HIP-3手数料 (P0-24)

```rust
// 実装場所: hip3-detector/src/fee.rs
pub const HIP3_FEE_MULTIPLIER: Decimal = Decimal::TWO;

pub struct FeeMetadata {
    pub base_taker_fee_bps: Decimal,
    pub hip3_multiplier: Decimal,
    pub effective_taker_fee_bps: Decimal,  // base × 2
    pub slippage_bps: Decimal,
    pub min_edge_bps: Decimal,
    pub total_cost_bps: Decimal,
}
```

---

## Next Steps

### 優先度1: Phase A完了に必要

1. ~~**P0-8完了** - レート制限メトリクス追加~~ ✅ DONE
   - `WS_MSGS_SENT_TOTAL` ✅
   - `WS_MSGS_BLOCKED_TOTAL` ✅
   - `POST_INFLIGHT` ✅
   - `CROSS_SKIPPED_TOTAL` ✅
   - `BBO_AGE_MS` / `CTX_AGE_MS` ✅

2. ~~**Risk Gate追加**~~ ✅ DONE
   - NoBboUpdate (max_bbo_age_ms=2000ms) ✅
   - NoAssetCtxUpdate (max_ctx_age_ms=8000ms) ✅
   - TimeRegression (server_time比較) ✅

3. ~~**Testnet接続テスト**~~ ✅ DONE
   - WS接続確認 ✅ (api.hyperliquid-testnet.xyz/ws)
   - bbo/assetCtx受信確認 ✅ パース成功確認
   - Hyperliquidフォーマット対応 ✅ (bbo, activeAssetCtx)
   - READY-MD状態遷移確認 - 要長時間テスト

4. ~~**P0-31: 日次指標出力**~~ ✅ DONE
   - cross_count (CROSS_COUNT_TOTAL ✅)
   - bbo_null_rate (BBO_NULL_RATE ✅)
   - ctx_age_ms (P50/P95/P99) ✅
   - cross_duration_ticks ✅
   - DailyStatsReporter実装完了

### 優先度2: Phase B準備

1. P0-25: NonceManager (now_unix_ms初期化)
2. P0-19: Executor batching (100ms周期)
3. P0-29: ActionBudget制御アルゴリズム
4. セキュリティ/鍵管理 (P0-11)

---

## Testing Status

| テスト種別 | 状態 | 備考 |
|-----------|------|------|
| Unit tests | [x] DONE | cargo test 113テスト通過 |
| Integration tests | [x] DONE | Testnet WS接続確認 |
| 24h連続稼働 | [~] IN_PROGRESS | VPSでテスト中 |

### 24h連続稼働テスト環境

| 項目 | 値 |
|------|-----|
| 環境 | VPS (Ubuntu/Debian) |
| IP | 5.104.81.76 |
| デプロイ方法 | Docker Compose |
| 開始日時 | 2026-01-18 |
| 対象 | Mainnet xyz DEX (32 markets) |
| 設定ファイル | config/mainnet.toml |

---

## Build Status

```
cargo check: ✅ Pass
cargo clippy: ✅ Pass (--workspace)
cargo test: ✅ Pass (113 tests)
```

---

## Version History

| Date | Change |
|------|--------|
| 2026-01-18 | Initial spec creation |
| 2026-01-18 (Session 2) | P0-8 metrics完了, Risk Gate (NoBboUpdate, NoAssetCtxUpdate, TimeRegression) 実装完了, MarketState monotonic freshness追加, Testnet WS接続確認, TLS ring crypto provider追加 |
| 2026-01-18 (Session 3) | Hyperliquid bbo/activeAssetCtx購読・パース実装完了, MarketConfig(coin+asset_idx)追加, clap CLI引数パーサー追加 |
| 2026-01-18 (Session 4) | P0-26 perpDexs API自動市場取得完了, MetaClient実装, P0-31 DailyStatsReporter完了確認, Mainnet xyz DEX 32マーケット発見成功 |
