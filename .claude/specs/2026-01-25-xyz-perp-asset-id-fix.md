# xyz Perp Asset ID Fix Specification

## Metadata

| Item | Value |
|------|-------|
| Date | 2026-01-25 |
| Status | `[COMPLETED]` |
| Related Plan | N/A (hotfix) |

## Summary

xyz perp銘柄のasset ID計算に重大なバグを発見・修正。`perpDexs` APIと`meta(dex=xyz)` APIでマーケットの順序が異なり、間違ったasset IDで注文が送信されていた。

## Problem Statement

### Symptom
- xyz:SILVERに注文を出そうとすると、xyz:RIVNに注文が出る
- RIVNは$15前後で0.2 × $15 = $3 < $10 minimumのためエラー

### Root Cause
**`perpDexs` APIと`meta(dex=xyz)` APIでマーケット順序が異なる**

| Asset | perpDexs (アルファベット順) | meta(dex=xyz) (正しい) |
|-------|---------------------------|----------------------|
| xyz:CL | [5] = 110005 | [29] = **110029** |
| xyz:RIVN | [26] = 110026 | [27] = **110027** |
| xyz:SILVER | [27] = 110027 | [26] = **110026** |

### Official Documentation
> `index_in_meta` comes from the position of your asset in the metadata response from **`meta(dex=xyz)`**

Source: [Asset IDs | Hyperliquid Docs](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/asset-ids)

## Changes Made

### 1. config/mainnet-test.toml
```diff
-asset_idx = 110027  # xyz:SILVER
+asset_idx = 110026  # xyz:SILVER (index 26 in meta)

-asset_idx = 110005  # xyz:CL
+asset_idx = 110029  # xyz:CL (index 29 in meta)
```

### 2. crates/hip3-registry/src/preflight.rs

**PerpMarketInfo構造体に`asset_index`フィールドを追加:**
```rust
pub struct PerpMarketInfo {
    pub name: String,
    pub sz_decimals: u8,
    pub max_leverage: u8,
    pub only_isolated: bool,
    pub tick_size: Option<Decimal>,
    /// Asset index from meta(dex=xyz) API for asset ID calculation.
    /// NOTE: perpDexs API uses different ordering.
    #[serde(skip)]
    pub asset_index: Option<u32>,  // NEW
}
```

**build_market_list()を修正:**
```rust
// Use asset_index from meta(dex=xyz) if available
let asset_idx = market.asset_index.unwrap_or_else(|| {
    warn!("Using fallback enumerate index (may be incorrect for xyz perps)");
    fallback_idx as u32
});
let full_asset_id = 100000 + perp_dex_id * 10000 + asset_idx;
```

### 3. crates/hip3-registry/src/client.rs

**新規メソッド `fetch_dex_meta_indices()` を追加:**
```rust
/// Fetch asset indices from meta(dex=xyz) API.
async fn fetch_dex_meta_indices(&self, dex_name: &str)
    -> RegistryResult<HashMap<String, u32>>
{
    let request = InfoRequestWithDex {
        request_type: "meta".to_string(),
        dex: dex_name.to_string(),
    };
    // ... API call and parse universe array
}
```

**fetch_perp_dexs()を修正:**
```rust
// Fetch correct asset indices from meta(dex=xyz) for each xyz DEX
for dex in &mut perp_dexs {
    if let Ok(index_map) = self.fetch_dex_meta_indices(&dex.name).await {
        for market in &mut dex.markets {
            let full_name = format!("{}:{}", dex.name, market.name);
            if let Some(&idx) = index_map.get(&full_name) {
                market.asset_index = Some(idx);
            }
        }
    }
}
```

### 4. crates/hip3-bot/src/app.rs

**populate_spec_cache()を修正:**
```rust
let asset_idx = market.asset_index.unwrap_or_else(|| {
    warn!("Using fallback enumerate index for SpecCache");
    fallback_idx as u32
});
let full_asset_id = 100000 + (dex_idx as u32) * 10000 + asset_idx;
```

## Test Results

### Before Fix
```
Error: Order must have minimum value of $10. asset=110027
(Actually sent to RIVN instead of SILVER)
```

### After Fix
```json
{
  "status": "ok",
  "response": {
    "type": "order",
    "data": {
      "statuses": [{
        "filled": {
          "totalSz": "0.2",
          "avgPx": "104.63",
          "oid": 302228651101
        }
      }]
    }
  }
}
```

## API Reference

### Get correct asset indices
```bash
curl -X POST https://api.hyperliquid.xyz/info \
  -H "Content-Type: application/json" \
  -d '{"type": "meta", "dex": "xyz"}'
```

Response `universe` array index = asset_index for ID calculation.

### Asset ID Formula
```
asset_id = 100000 + perp_dex_id * 10000 + index_in_meta
```

For xyz (perp_dex_id = 1):
- xyz:SILVER (index 26) = 100000 + 10000 + 26 = **110026**
- xyz:RIVN (index 27) = 100000 + 10000 + 27 = **110027**
- xyz:CL (index 29) = 100000 + 10000 + 29 = **110029**

## Lessons Learned

1. **APIの違いに注意**: Hyperliquidには複数のmeta系APIがあり、それぞれ異なる順序でデータを返す
2. **公式ドキュメント確認**: asset IDの計算には`meta(dex=xyz)`を使うと明記されている
3. **テスト重要**: 実際に注文を送信してUIで確認することで問題を発見できた

## Files Modified

| File | Change |
|------|--------|
| `config/mainnet-test.toml` | asset_idx値を修正 |
| `crates/hip3-registry/src/preflight.rs` | `asset_index`フィールド追加、`build_market_list`修正 |
| `crates/hip3-registry/src/client.rs` | `fetch_dex_meta_indices`追加、`fetch_perp_dexs`修正 |
| `crates/hip3-bot/src/app.rs` | `populate_spec_cache`修正 |
| `crates/hip3-bot/src/bin/test_order.rs` | SILVER_ASSET_IDXを110026に修正 |
