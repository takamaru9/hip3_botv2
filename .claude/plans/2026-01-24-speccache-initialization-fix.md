# SpecCache Initialization Fix Plan

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-01-24 |
| Source Bug Report | `bug/2026-01-24-mainnet-test-failure.md` (Issue 5) |
| Related Plan | `2026-01-24-mainnet-test-failure-fix.md` (v1.5 - DONE) |
| Priority | CRITICAL |
| Status | DRAFT v1.3 |

## Changelog

| Version | Date | Changes |
|---------|------|---------|
| v1.0 | 2026-01-24 | Initial draft |
| v1.1 | 2026-01-24 | レビュー指摘反映: DEX index確定、tickSize型耐性、設定済み市場検証追加 |
| v1.2 | 2026-01-24 | 再レビュー指摘反映: API不整合修正、parse_spec()更新、テスト更新計画追加 |
| v1.3 | 2026-01-24 | 再々レビュー指摘反映: 冗長export削除、未使用import削除 |

## 参照した一次情報

| 項目 | ソース | URL | 確認日 |
|------|--------|-----|--------|
| Meta Info API | Hyperliquid GitBook | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint | 2026-01-24 |
| perpDexs レスポンス | コードベース調査 | `crates/hip3-registry/src/preflight.rs` | 2026-01-24 |
| SpecCache 実装 | コードベース調査 | `crates/hip3-registry/src/spec_cache.rs` | 2026-01-24 |

## 未確認事項（実測必須）

| 項目 | 理由 | 実測方法 |
|------|------|----------|
| tickSize フィールドの有無 | perpDexs レスポンスに含まれるか未確認 | API レスポンス実測 |
| tickSize の型 | String か Number か不明（docs に記載なし） | API レスポンス実測 |

---

## Executive Summary

Mainnetテスト (17:06 JST) で、0x prefix 修正後も全オーダーが `MarketSpec not found` で失敗。

**根本原因**: `spec_cache.update()` がコードベース内で一度も呼ばれておらず、SpecCache が常に空。

---

## Issue Summary

| ID | Severity | Issue | Impact |
|----|----------|-------|--------|
| P0.5 | CRITICAL | SpecCache が populate されていない | 全注文が MarketSpec not found で失敗 |

---

## 問題の詳細

### 症状

```
WARN hip3_executor::executor_loop: MarketSpec not found, failing batch, market: xyz:26, cloid: hip3_1769241993404_a307f1d3
WARN hip3_executor::executor_loop: Failed to build action from batch, error: MarketSpec not found for market: xyz:26
```

### 根本原因分析

**app.rs:108**: `SpecCache::default()` で空のキャッシュ作成
```rust
let spec_cache = Arc::new(SpecCache::default());
```

**app.rs:158-162**: `config.has_markets() == true` の場合、preflight をスキップ
```rust
if self.config.has_markets() {
    info!("Markets already configured, skipping preflight");
    self.initialize_daily_stats();
    return Ok(());  // ← SpecCache は空のまま!
}
```

**検索結果**: `spec_cache.update()` は一度も呼ばれていない
```bash
$ grep "spec_cache\.update" crates/hip3-bot/src/app.rs
(no matches)
```

### 影響範囲

- **Severity**: CRITICAL (P0)
- **Effect**: オーダー送信が完全にブロック
- **Scope**: Trading mode で markets を TOML 設定した場合に 100% 発生

---

## 修正アプローチ

**概要**: `run_preflight()` で perpDexs を取得し、SpecCache を populate する。

市場設定が TOML で明示されている場合でも、perpDexs から市場仕様を取得して SpecCache に登録する必要がある。

---

## Step 1: PerpMarketInfo に tick_size を追加（型耐性あり）

**ファイル**: `crates/hip3-registry/src/preflight.rs`

**レビュー指摘 [HIGH]**: `tickSize` が数値で返される可能性があるため、String/Number 両対応のデシリアライザが必要。

**現在の構造体** (L32-45):
```rust
pub struct PerpMarketInfo {
    pub name: String,
    #[serde(rename = "szDecimals")]
    pub sz_decimals: u8,
    #[serde(rename = "maxLeverage")]
    pub max_leverage: u8,
    #[serde(rename = "onlyIsolated", default)]
    pub only_isolated: bool,
}
```

**変更後**:
```rust
use rust_decimal::Decimal;

/// Deserialize tickSize as Decimal, accepting both String and Number.
fn deserialize_tick_size<'de, D>(deserializer: D) -> Result<Option<Decimal>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct TickSizeVisitor;

    impl<'de> Visitor<'de> for TickSizeVisitor {
        type Value = Option<Decimal>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string, number, or null for tickSize")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            v.parse::<Decimal>().map(Some).map_err(de::Error::custom)
        }

        fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Decimal::try_from(v).map(Some).map_err(de::Error::custom)
        }

        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Some(Decimal::from(v)))
        }

        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Some(Decimal::from(v)))
        }
    }

    deserializer.deserialize_any(TickSizeVisitor)
}

pub struct PerpMarketInfo {
    pub name: String,
    #[serde(rename = "szDecimals")]
    pub sz_decimals: u8,
    #[serde(rename = "maxLeverage")]
    pub max_leverage: u8,
    #[serde(rename = "onlyIsolated", default)]
    pub only_isolated: bool,
    /// Tick size from exchange (if provided).
    /// Accepts both String ("0.01") and Number (0.01) from API.
    #[serde(rename = "tickSize", default, deserialize_with = "deserialize_tick_size")]
    pub tick_size: Option<Decimal>,
}
```

**依存関係追加**: `crates/hip3-registry/Cargo.toml`

注: `rust_decimal` は workspace で既に定義済みのため、`workspace = true` で統一。
```toml
[dependencies]
rust_decimal = { workspace = true }
```

**変更点**:
- `Option<String>` → `Option<Decimal>` に変更
- カスタムデシリアライザで String/Number/null すべてに対応
- `RawPerpSpec` も `Option<Decimal>` に合わせる必要あり

---

## Step 2: DiscoveredMarket に tick_size を追加

**ファイル**: `crates/hip3-registry/src/preflight.rs`

**現在の構造体** (L58-69):
```rust
pub struct DiscoveredMarket {
    pub key: MarketKey,
    pub name: String,
    pub sz_decimals: u8,
    pub max_leverage: u8,
}
```

**変更後**:
```rust
use rust_decimal::Decimal;

pub struct DiscoveredMarket {
    pub key: MarketKey,
    pub name: String,
    pub sz_decimals: u8,
    pub max_leverage: u8,
    /// Tick size from exchange (if provided).
    pub tick_size: Option<Decimal>,
}
```

**変更点**: `Option<Decimal>` で Step 1 と型を統一

---

## Step 3: PreflightChecker で tick_size を伝播

**ファイル**: `crates/hip3-registry/src/preflight.rs`

### 3a. build_market_list() で tick_size を伝播

**現在のコード** (L230-245):
```rust
fn build_market_list(
    &self,
    dex_id: DexId,
    markets: &[PerpMarketInfo],
) -> Vec<DiscoveredMarket> {
    markets
        .iter()
        .enumerate()
        .map(|(idx, market)| DiscoveredMarket {
            key: MarketKey::new(dex_id, AssetId::new(idx as u16)),
            name: market.name.clone(),
            sz_decimals: market.sz_decimals,
            max_leverage: market.max_leverage,
        })
        .collect()
}
```

**変更後**:
```rust
fn build_market_list(
    &self,
    dex_id: DexId,
    markets: &[PerpMarketInfo],
) -> Vec<DiscoveredMarket> {
    markets
        .iter()
        .enumerate()
        .map(|(idx, market)| DiscoveredMarket {
            key: MarketKey::new(dex_id, AssetId::new(idx as u16)),
            name: market.name.clone(),
            sz_decimals: market.sz_decimals,
            max_leverage: market.max_leverage,
            tick_size: market.tick_size,  // 追加
        })
        .collect()
}
```

### 3b. find_xyz_dex() を pub に変更

`populate_spec_cache()` から DEX 検索ロジックを再利用するため、`find_xyz_dex()` を public にエクスポート。

**現在のコード** (L131-132):
```rust
/// Find the xyz DEX by name pattern.
fn find_xyz_dex<'a>(&self, dexs: &'a [PerpDexInfo]) -> RegistryResult<(u16, &'a PerpDexInfo)> {
```

**変更後**:
```rust
/// Find the xyz DEX by name pattern.
/// Returns (dex_index, dex_info).
pub fn find_xyz_dex<'a>(&self, dexs: &'a [PerpDexInfo]) -> RegistryResult<(u16, &'a PerpDexInfo)> {
```

**重要**: `find_xyz_dex()` のロジック（`contains` + case-insensitive）を `populate_spec_cache()` で再利用することで、DEX 検索の不一致（レビュー指摘 [MEDIUM]）を解消。

---

## Step 4: App に populate_spec_cache メソッドを追加（DEX index 確定対応）

**ファイル**: `crates/hip3-bot/src/app.rs`

**レビュー指摘 [HIGH]**: `get_dex_id()` は `xyz_dex_id` 未設定時に `DexId::XYZ` (0) にフォールバックするため、perpDexs の xyz DEX が index 0 でない場合に SpecCache のキーがズレる。

**解決策**: `PreflightChecker::find_xyz_dex()` を再利用して DEX index を確定し、`xyz_dex_id` も設定。

```rust
use hip3_registry::preflight::PreflightChecker;
use hip3_registry::spec_cache::RawPerpSpec;
use rust_decimal::Decimal;

/// Populate SpecCache from perpDexs response.
///
/// Must be called during initialization to ensure ExecutorLoop has access
/// to market specifications for order formatting.
///
/// # Important
/// This method also sets `xyz_dex_id` on the config, ensuring that the correct
/// DEX index is used throughout the application.
fn populate_spec_cache(&mut self, perp_dexs: &PerpDexsResponse) -> AppResult<()> {
    // Use PreflightChecker to find xyz DEX (reuse same logic as validate())
    // This ensures consistent DEX detection: contains + case-insensitive
    let checker = PreflightChecker::new(&self.config.xyz_pattern);
    let (dex_idx, xyz_dex) = checker
        .find_xyz_dex(&perp_dexs.perp_dexs)
        .map_err(|e| AppError::Preflight(format!("Failed to find xyz DEX: {e}")))?;

    let dex_id = DexId::new(dex_idx);

    // Set xyz_dex_id on App to ensure get_dex_id() returns correct value
    // Note: xyz_dex_id is a field on App, not on config
    self.xyz_dex_id = Some(dex_id);

    tracing::info!(
        dex_name = %xyz_dex.name,
        dex_idx = dex_idx,
        market_count = xyz_dex.markets.len(),
        "Found xyz DEX for SpecCache initialization"
    );

    for (asset_idx, market) in xyz_dex.markets.iter().enumerate() {
        let raw = RawPerpSpec {
            name: market.name.clone(),
            sz_decimals: market.sz_decimals,
            max_leverage: market.max_leverage,
            only_isolated: market.only_isolated,
            tick_size: market.tick_size,  // Option<Decimal>
        };
        let spec = self.spec_cache.parse_spec(&raw);
        let key = MarketKey::new(dex_id, AssetId::new(asset_idx as u16));

        self.spec_cache
            .update(key, spec)
            .map_err(|e| AppError::Preflight(format!("Failed to update SpecCache: {e}")))?;

        tracing::debug!(
            market = %key,
            name = %market.name,
            sz_decimals = market.sz_decimals,
            tick_size = ?market.tick_size,
            "Populated SpecCache"
        );
    }

    tracing::info!(
        market_count = xyz_dex.markets.len(),
        dex_id = %dex_id,
        "SpecCache populated from perpDexs"
    );

    Ok(())
}
```

**注意点**:
1. `&self` → `&mut self` に変更（`self.xyz_dex_id` 設定のため）
2. `xyz_dex_id` は `App` のフィールド（`config` ではない）
3. `PreflightChecker::find_xyz_dex()` を再利用（Step 3b で pub 化）
4. `tick_size` は `Option<Decimal>` 型（Step 1 で変更）

---

## Step 5: run_preflight() で populate_spec_cache を呼ぶ（設定済み市場検証付き）

**ファイル**: `crates/hip3-bot/src/app.rs`

**レビュー指摘 [MEDIUM]**: markets 設定済み経路で preflight 検証が完全にスキップされるため、`asset_idx` の誤設定や coin 名の不整合を検知できない。

**現在のフロー** (L146-220):
```rust
pub async fn run_preflight(&mut self) -> AppResult<()> {
    // ...
    if self.config.has_markets() {
        info!("Markets already configured, skipping preflight");
        self.initialize_daily_stats();
        return Ok(());  // ← SpecCache は空!
    }
    // ...
    let perp_dexs = /* fetch */;
    // ...
    self.config.set_discovered_markets(markets);
    // ← SpecCache への登録がない!
}
```

**変更後**:
```rust
use hip3_registry::preflight::validate_market_keys;

pub async fn run_preflight(&mut self) -> AppResult<()> {
    // Always fetch perpDexs for SpecCache (even if markets are configured)
    info!(
        info_url = %self.config.info_url,
        "Fetching perpDexs for SpecCache initialization"
    );

    let client = MetaClient::new(&self.config.info_url)
        .map_err(|e| AppError::Preflight(format!("Failed to create HTTP client: {e}")))?;

    const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(30);
    let perp_dexs = tokio::time::timeout(PREFLIGHT_TIMEOUT, client.fetch_perp_dexs())
        .await
        .map_err(|_| AppError::Preflight("Preflight HTTP request timed out (30s)".to_string()))?
        .map_err(|e| AppError::Preflight(format!("Failed to fetch perpDexs: {e}")))?;

    // ★ Always populate SpecCache (sets xyz_dex_id too)
    self.populate_spec_cache(&perp_dexs)?;

    // Safety check for Trading mode
    if self.config.mode == OperatingMode::Trading && !self.config.has_markets() {
        return Err(AppError::Preflight(
            "Trading mode requires explicit [[markets]] in config (auto-discovery disabled for safety)"
                .to_string(),
        ));
    }

    // If markets already configured, validate against perpDexs then skip discovery
    if self.config.has_markets() {
        // ★ Validate configured markets exist in perpDexs (NEW)
        self.validate_configured_markets(&perp_dexs)?;

        info!("Markets already configured and validated, skipping market discovery");
        self.initialize_daily_stats();
        return Ok(());
    }

    // Continue with market discovery for non-configured case...
    // Validate and discover markets
    let checker = PreflightChecker::new(&self.config.xyz_pattern);
    let result = checker
        .validate(&perp_dexs)
        .map_err(|e| AppError::Preflight(format!("Preflight validation failed: {e}")))?;

    // ... (existing discovery logic, unchanged)
}

/// Validate that configured markets exist in perpDexs.
///
/// This catches configuration errors like:
/// - Invalid asset_idx (market doesn't exist)
/// - Coin name mismatch
fn validate_configured_markets(&self, perp_dexs: &PerpDexsResponse) -> AppResult<()> {
    let checker = PreflightChecker::new(&self.config.xyz_pattern);
    let result = checker
        .validate(perp_dexs)
        .map_err(|e| AppError::Preflight(format!("Preflight validation failed: {e}")))?;

    let dex_id = self.get_dex_id();

    // Build configured market keys from asset_idx
    // Note: MarketConfig has asset_idx, not key field
    let configured_keys: Vec<MarketKey> = self
        .config
        .get_markets()
        .iter()
        .map(|m| MarketKey::new(dex_id, AssetId::new(m.asset_idx)))
        .collect();

    // Validate all configured keys exist in discovered markets
    validate_market_keys(&configured_keys, &result.markets)
        .map_err(|e| AppError::Preflight(format!("Configured market validation failed: {e}")))?;

    // Optional: Warn if coin names don't match
    for configured in self.config.get_markets() {
        let key = MarketKey::new(dex_id, AssetId::new(configured.asset_idx));
        if let Some(discovered) = result.markets.iter().find(|m| m.key == key) {
            if !configured.coin.ends_with(&discovered.name) {
                tracing::warn!(
                    configured_coin = %configured.coin,
                    discovered_name = %discovered.name,
                    key = %key,
                    "Configured coin name doesn't match perpDexs - verify configuration"
                );
            }
        }
    }

    tracing::info!(
        market_count = configured_keys.len(),
        "Configured markets validated against perpDexs"
    );

    Ok(())
}
```

**重要な変更点**:
1. perpDexs フェッチを最初に移動（markets 設定の有無に関わらず実行）
2. `populate_spec_cache()` を呼び出し（`xyz_dex_id` も設定）
3. Trading mode のセーフティチェックを perpDexs フェッチ後に移動
4. **markets 設定済みの場合も `validate_configured_markets()` で検証**（NEW）
5. discovery をスキップするが、SpecCache は populate 済み + 設定検証済み

---

## Step 6: import 追加と export 確認

### 6a. hip3-bot/src/app.rs に import 追加

```rust
use hip3_registry::preflight::{PreflightChecker, validate_market_keys};
use hip3_registry::spec_cache::RawPerpSpec;
// Note: rust_decimal::Decimal は app.rs では未使用のため追加不要
```

### 6b. hip3-registry/src/lib.rs で export 確認

**ファイル**: `crates/hip3-registry/src/lib.rs`

`PreflightChecker` は既に公開済み。`validate_market_keys` のみ追加。

```rust
// validate_market_keys のみ追加（PreflightChecker は既存）
pub use preflight::validate_market_keys;
// RawPerpSpec, SpecCache 等は既に export 済みなら追加不要
```

### 6c. RawPerpSpec の tick_size 型を変更

**ファイル**: `crates/hip3-registry/src/spec_cache.rs`

`RawPerpSpec` の `tick_size` を `Option<Decimal>` に変更して `PerpMarketInfo` と型を統一。

**現在**:
```rust
pub struct RawPerpSpec {
    pub name: String,
    pub sz_decimals: u8,
    pub max_leverage: u8,
    pub only_isolated: bool,
    pub tick_size: Option<String>,
}
```

**変更後**:
```rust
use rust_decimal::Decimal;

pub struct RawPerpSpec {
    pub name: String,
    pub sz_decimals: u8,
    pub max_leverage: u8,
    pub only_isolated: bool,
    pub tick_size: Option<Decimal>,
}
```

### 6d. parse_spec() の修正

**ファイル**: `crates/hip3-registry/src/spec_cache.rs`

`tick_size` が `Option<String>` → `Option<Decimal>` に変更されるため、`parse_spec()` 内の処理を修正。

**現在** (L103-108):
```rust
// P1-1: Parse tick_size from exchange if provided
let tick_size = raw
    .tick_size
    .as_ref()
    .and_then(|s| s.parse::<Decimal>().ok())
    .map(Price::new)
    .unwrap_or_else(|| Price::new(Decimal::new(1, 2))); // Default: 0.01
```

**変更後**:
```rust
// P1-1: Use tick_size directly (already Decimal from deserialization)
let tick_size = raw
    .tick_size
    .map(Price::new)
    .unwrap_or_else(|| Price::new(Decimal::new(1, 2))); // Default: 0.01
```

**変更点**: `.as_ref().and_then(|s| s.parse::<Decimal>().ok())` を削除し、直接 `.map(Price::new)` で変換。

### 6e. spec_cache.rs のテスト更新

**ファイル**: `crates/hip3-registry/src/spec_cache.rs`

`RawPerpSpec` の `tick_size` 型変更に伴い、テストを更新。

**現在** (例: L295):
```rust
tick_size: Some("0.001".to_string()),
```

**変更後**:
```rust
use rust_decimal_macros::dec;

tick_size: Some(dec!(0.001)),
```

**更新対象テスト**:
- `test_parse_spec_with_tick_size` (L285-324)
- その他 `RawPerpSpec` を生成するテスト

---

## 依存関係の確認

`RawPerpSpec` と `PerpMarketInfo` のフィールドを統一（両方とも `Option<Decimal>`）。

**RawPerpSpec** (`spec_cache.rs`):
```rust
pub struct RawPerpSpec {
    pub name: String,
    pub sz_decimals: u8,
    pub max_leverage: u8,
    pub only_isolated: bool,
    pub tick_size: Option<Decimal>,  // ← Step 6c で変更
}
```

**PerpMarketInfo** (`preflight.rs`):
```rust
pub struct PerpMarketInfo {
    pub name: String,
    pub sz_decimals: u8,
    pub max_leverage: u8,
    pub only_isolated: bool,
    pub tick_size: Option<Decimal>,  // ← Step 1 で追加
}
```

フィールドが一致（両方 `Option<Decimal>`）しているので、`PerpMarketInfo` から `RawPerpSpec` への変換が可能。

---

## 代替案: 設定ファイルから SpecCache を初期化

perpDexs をフェッチせずに、TOML 設定から SpecCache を初期化する方法もある。

```toml
[[markets]]
asset_idx = 26
coin = "xyz:SILVER"
sz_decimals = 3
tick_size = "0.01"
max_leverage = 10
```

**メリット**:
- ネットワークリクエスト不要
- 起動が速い

**デメリット**:
- 設定が古くなるリスク
- 取引所との不整合の可能性

**推奨**: perpDexs からフェッチする方法 (取引所が正の情報源)

---

## Implementation Order

| Priority | Task | Description | Depends On |
|----------|------|-------------|------------|
| 1 | Step 1 | PerpMarketInfo に tick_size 追加（Decimal型、型耐性デシリアライザ） | - |
| 2 | Step 2 | DiscoveredMarket に tick_size 追加（Decimal型） | Step 1 |
| 3 | Step 3a | build_market_list() で tick_size を伝播 | Step 2 |
| 4 | Step 3b | find_xyz_dex() を pub に変更 | - |
| 5 | Step 6c | RawPerpSpec の tick_size を Decimal に変更 | Step 1 |
| 6 | Step 6d | parse_spec() の修正（.parse() 削除） | Step 6c |
| 7 | Step 6e | spec_cache.rs のテスト更新 | Step 6c |
| 8 | Step 6b | hip3-registry/lib.rs で export 追加 | Step 3b |
| 9 | Step 4 | App::populate_spec_cache() 追加（xyz_dex_id 設定） | Step 3b, Step 6c |
| 10 | Step 5 | run_preflight() 修正 + validate_configured_markets() 追加 | Step 4 |
| 11 | Step 6a | app.rs に import 追加 | Step 6b |
| 12 | Cargo | hip3-registry/Cargo.toml に rust_decimal 追加（workspace = true） | - |
| 13 | Tests | 統合テスト追加・実行 | Step 11 |

---

## Verification Checklist

### SpecCache 初期化
- [ ] PerpMarketInfo に tick_size フィールド追加（`Option<Decimal>`）
- [ ] tick_size のカスタムデシリアライザ（String/Number 両対応）
- [ ] DiscoveredMarket に tick_size フィールド追加（`Option<Decimal>`）
- [ ] PreflightChecker::build_market_list() で tick_size を伝播
- [ ] PreflightChecker::find_xyz_dex() を pub に変更
- [ ] RawPerpSpec の tick_size を `Option<Decimal>` に変更
- [ ] parse_spec() から `.parse::<Decimal>()` 削除、直接 `.map(Price::new)` に変更
- [ ] spec_cache.rs のテストを `Decimal` 型に更新
- [ ] hip3-registry/lib.rs で `validate_market_keys` のみ export 追加
- [ ] App::populate_spec_cache() メソッド追加（`&mut self`）
- [ ] populate_spec_cache() 内で find_xyz_dex() を再利用
- [ ] populate_spec_cache() 内で `self.xyz_dex_id = Some(dex_id)` を設定

### 設定済み市場の検証（レビュー指摘対応）
- [ ] validate_configured_markets() メソッド追加
- [ ] `self.config.get_markets()` を使用（`markets()` ではない）
- [ ] `MarketKey::new(dex_id, AssetId::new(m.asset_idx))` でキー構築（`m.key` ではない）
- [ ] 設定された asset_idx が perpDexs に存在するか確認
- [ ] coin 名の不一致に対する警告ログ

### run_preflight() フロー
- [ ] 必ず perpDexs フェッチ（markets 設定時も）
- [ ] populate_spec_cache() 呼び出し
- [ ] markets 設定済みの場合も validate_configured_markets() で検証
- [ ] app.rs に必要な import 追加（`rust_decimal::Decimal` は未使用のため不要）

### Cargo 依存関係
- [ ] hip3-registry/Cargo.toml に rust_decimal 追加（`workspace = true`）

### 動作確認
- [ ] cargo build 成功
- [ ] 起動時に "SpecCache populated from perpDexs" ログ出力
- [ ] 起動時に "Configured markets validated against perpDexs" ログ出力
- [ ] ExecutorLoop::batch_to_action() で MarketSpec not found が発生しない
- [ ] Testnet でオーダー送信成功
- [ ] 不正な asset_idx を設定した場合にエラーで起動失敗

---

## Files to Modify

| File | Changes |
|------|---------|
| `crates/hip3-registry/Cargo.toml` | rust_decimal 依存追加（`workspace = true`） |
| `crates/hip3-registry/src/preflight.rs` | Step 1,2,3: tick_size 追加 + 型耐性デシリアライザ + find_xyz_dex pub 化 |
| `crates/hip3-registry/src/spec_cache.rs` | Step 6c,6d,6e: RawPerpSpec 型変更 + parse_spec() 修正 + テスト更新 |
| `crates/hip3-registry/src/lib.rs` | Step 6b: export 追加 |
| `crates/hip3-bot/src/app.rs` | Step 4,5,6a: populate_spec_cache() + validate_configured_markets() + run_preflight() 修正 |

---

## Rollback Plan

修正がさらなる問題を引き起こした場合:

1. 変更をリバート (`git revert`)
2. 前回の動作バージョンに戻す
3. 根本原因を再調査

---

## Test Plan

### 単体テスト

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    #[test]
    fn test_deserialize_tick_size_string() {
        // tickSize が文字列の場合
        let json = r#"{"name":"BTC","szDecimals":3,"maxLeverage":50,"tickSize":"0.01"}"#;
        let info: PerpMarketInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.tick_size, Some(Decimal::from_str("0.01").unwrap()));
    }

    #[test]
    fn test_deserialize_tick_size_number() {
        // tickSize が数値の場合
        let json = r#"{"name":"BTC","szDecimals":3,"maxLeverage":50,"tickSize":0.01}"#;
        let info: PerpMarketInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.tick_size, Some(Decimal::from_str("0.01").unwrap()));
    }

    #[test]
    fn test_deserialize_tick_size_null() {
        // tickSize が null の場合
        let json = r#"{"name":"BTC","szDecimals":3,"maxLeverage":50,"tickSize":null}"#;
        let info: PerpMarketInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.tick_size, None);
    }

    #[test]
    fn test_deserialize_tick_size_missing() {
        // tickSize フィールドがない場合
        let json = r#"{"name":"BTC","szDecimals":3,"maxLeverage":50}"#;
        let info: PerpMarketInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.tick_size, None);
    }

    #[test]
    fn test_populate_spec_cache() {
        // Mock PerpDexsResponse を作成
        // populate_spec_cache() を呼び出し
        // spec_cache.get() が Some を返すことを確認
        // xyz_dex_id が正しく設定されていることを確認
    }

    #[test]
    fn test_validate_configured_markets_success() {
        // 有効な設定の場合は成功
    }

    #[test]
    fn test_validate_configured_markets_invalid_asset_idx() {
        // 存在しない asset_idx を設定した場合はエラー
    }
}
```

### 統合テスト

1. `cargo build --release`
2. Testnet で起動
3. ログに "SpecCache populated from perpDexs" が出力されることを確認
4. ログに "Configured markets validated against perpDexs" が出力されることを確認
5. シグナル発生時にオーダー送信が成功することを確認 (MarketSpec not found が出ない)

### エラーケーステスト

1. 不正な `asset_idx` を TOML に設定して起動
2. エラーで起動失敗することを確認
3. エラーメッセージに "not found in perpDexs" が含まれることを確認

---

## Notes

- この修正は `2026-01-24-mainnet-test-failure-fix.md` (v1.5) の P1 (精度制限) の前提条件
- SpecCache が populate されていないと P1 の `batch_to_action()` も正常に動作しない
- P1 の実装は本修正完了後に行う

## レビュー対応履歴

| Version | レビュー指摘 | 対応 |
|---------|-------------|------|
| v1.1 | [HIGH] DEX index確定問題 | `PreflightChecker::find_xyz_dex()` を再利用、`xyz_dex_id` を設定 |
| v1.1 | [HIGH] tickSize型耐性 | String/Number両対応のカスタムデシリアライザ追加 |
| v1.1 | [MEDIUM] DEX検索ロジック不一致 | `find_xyz_dex()` を pub 化して再利用 |
| v1.1 | [MEDIUM] 設定済み市場の検証 | `validate_configured_markets()` 追加 |
| v1.2 | [HIGH] `set_xyz_dex_id` API不存在 | `self.xyz_dex_id = Some(dex_id)` に修正（App フィールド） |
| v1.2 | [HIGH] `markets()` / `m.key` API不存在 | `get_markets()` / `MarketKey::new()` に修正 |
| v1.2 | [MEDIUM] parse_spec() 修正手順不足 | `.parse()` 削除、直接 `.map(Price::new)` に変更 |
| v1.2 | [MEDIUM] テスト更新計画不足 | Step 6e でテスト更新計画追加 |
| v1.2 | [LOW] rust_decimal 依存冗長 | `workspace = true` で統一 |
| v1.3 | [LOW] lib.rs export 冗長 | `validate_market_keys` のみ追加（既存export確認） |
| v1.3 | [LOW] app.rs 未使用 import | `rust_decimal::Decimal` 削除 |
