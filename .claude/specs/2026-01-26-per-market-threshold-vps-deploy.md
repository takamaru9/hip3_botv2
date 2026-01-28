# Per-Market Threshold Configuration & VPS Deployment Spec

## Metadata

| Item | Value |
|------|-------|
| Date | 2026-01-26 |
| Status | `[COMPLETED]` |
| Last Updated | 2026-01-26 |

## Overview

銘柄ごとに異なる `threshold_bps` を設定できる機能を実装し、Phase A分析結果に基づく最適閾値でVPSにデプロイした。

## Implementation Status

### 1. Per-Market Threshold Configuration

| ID | Item | Status | Notes |
|----|------|--------|-------|
| 1.1 | MarketConfig に threshold_bps フィールド追加 | [x] DONE | `Option<u32>` 型 |
| 1.2 | DetectorConfig の check() にオーバーライド対応 | [x] DONE | `threshold_override_bps: Option<Decimal>` |
| 1.3 | Application に market_threshold_map 追加 | [x] DONE | `HashMap<u32, Decimal>` |
| 1.4 | シグナル検出時に銘柄別閾値を適用 | [x] DONE | check() 呼び出し時に渡す |
| 1.5 | テストコード修正 | [x] DONE | threshold_bps: None を追加 |
| 1.6 | mainnet-optimal-test.toml 作成 | [x] DONE | Phase A分析結果の閾値 |

### 2. VPS Deployment

| ID | Item | Status | Notes |
|----|------|--------|-------|
| 2.1 | Dockerfile を Rust 1.85 に更新 | [x] DONE | edition2024 対応 |
| 2.2 | docker-compose.yml に HIP3_TRADING_KEY 追加 | [x] DONE | 環境変数経由 |
| 2.3 | .env.example 作成 | [x] DONE | キー設定例 |
| 2.4 | VPS にリポジトリをクローン | [x] DONE | /root/hip3_botv2 |
| 2.5 | Docker イメージビルド | [x] DONE | 2m 43s |
| 2.6 | .env にトレーディングキー設定 | [x] DONE | |
| 2.7 | コンテナ起動・動作確認 | [x] DONE | healthy |

## Code Changes

### crates/hip3-bot/src/config.rs

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketConfig {
    pub asset_idx: u32,
    pub coin: String,
    /// Per-market threshold in basis points. If None, uses global detector config.
    #[serde(default)]
    pub threshold_bps: Option<u32>,
}
```

### crates/hip3-detector/src/detector.rs

```rust
pub fn check(
    &self,
    key: MarketKey,
    snapshot: &MarketSnapshot,
    threshold_override_bps: Option<Decimal>,
) -> Option<DislocationSignal> {
    // ...
}

fn check_buy(..., threshold_override_bps: Option<Decimal>) -> Option<DislocationSignal> {
    let total_cost = threshold_override_bps.unwrap_or_else(|| self.fee_calculator.total_cost_bps());
    // ...
}
```

### crates/hip3-bot/src/app.rs

```rust
// Application struct に追加
market_threshold_map: HashMap<u32, Decimal>,

// Application::new() で初期化
let market_threshold_map: HashMap<u32, Decimal> = config
    .markets
    .as_ref()
    .map(|markets| {
        markets.iter()
            .filter_map(|m| m.threshold_bps.map(|t| (m.asset_idx, Decimal::from(t))))
            .collect()
    })
    .unwrap_or_default();

// シグナル検出時
let threshold_override = self.market_threshold_map.get(&key.asset.0).copied();
if let Some(signal) = self.detector.check(key, &snapshot, threshold_override) {
```

### config/mainnet-optimal-test.toml

```toml
[[markets]]
asset_idx = 110003
coin = "xyz:GOLD"
threshold_bps = 30

[[markets]]
asset_idx = 110026
coin = "xyz:SILVER"
threshold_bps = 30

[[markets]]
asset_idx = 110012
coin = "xyz:GOOGL"
threshold_bps = 45

[[markets]]
asset_idx = 110001
coin = "xyz:TSLA"
threshold_bps = 40

[[markets]]
asset_idx = 110002
coin = "xyz:NVDA"
threshold_bps = 25
```

### Dockerfile

```dockerfile
# Rust 1.83 -> 1.85 (edition2024 対応)
FROM rust:1.85-bookworm AS builder
```

## VPS Deployment Details

| Item | Value |
|------|-------|
| VPS IP | 5.104.81.76 |
| Provider | Contabo |
| Container Name | hip3-bot |
| Config | /app/config/mainnet-optimal-test.toml |
| Data Directory | /app/data/mainnet/signals |
| Health Status | healthy |

### Per-Market Thresholds (Phase A Analysis Results)

| Market | Threshold (bps) | Rationale |
|--------|-----------------|-----------|
| GOLD | 30 | 流動性高、スプレッド安定 |
| SILVER | 30 | GOLDと同等の特性 |
| GOOGL | 45 | 株式xyz、ボラティリティ高め |
| TSLA | 40 | 流動性良好、適度な閾値 |
| NVDA | 25 | 流動性最良、低閾値で捕捉 |

### Management Commands

```bash
# ログ確認
ssh root@5.104.81.76 "docker-compose -f /root/hip3_botv2/docker-compose.yml logs -f"

# ステータス確認
ssh root@5.104.81.76 "docker ps"

# 再起動
ssh root@5.104.81.76 "cd /root/hip3_botv2 && docker-compose restart"

# 停止
ssh root@5.104.81.76 "cd /root/hip3_botv2 && docker-compose down"

# コード更新
ssh root@5.104.81.76 "cd /root/hip3_botv2 && git pull && docker-compose build && docker-compose up -d"
```

## Issues Resolved

### 1. Rust edition2024 Error

**Problem**: ruint-1.17.2 requires Cargo feature `edition2024` not available in Rust 1.83

**Solution**: Updated Dockerfile from `rust:1.83-bookworm` to `rust:1.85-bookworm`

### 2. SSH Key Passphrase

**Problem**: SSH key requires passphrase, blocking automated deployment

**Solution**: Used `expect` command to handle password authentication

### 3. HIP3_TRADING_KEY Missing

**Problem**: Container failed with "Invalid private key: signature error"

**Solution**: Created .env file on VPS with the trading key

## Notes

- HIP-3 xyz markets (including TSLA, NVDA, GOOGL) trade 24/7
- threshold_bps = taker_fee + slippage + min_edge (total cost basis)
- Local monitoring process (PID 35988) stopped after VPS deployment
- VPS container set to `restart: unless-stopped` for persistence
