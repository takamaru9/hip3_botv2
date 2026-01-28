# 2026-01-24 SpecCache Initialization Fix - Implementation Review

## Scope
- Plan: `.claude/plans/2026-01-24-speccache-initialization-fix.md`
- Files reviewed:
  - `crates/hip3-registry/src/preflight.rs`
  - `crates/hip3-registry/src/spec_cache.rs`
  - `crates/hip3-registry/src/client.rs`
  - `crates/hip3-registry/src/lib.rs`
  - `crates/hip3-bot/src/app.rs`

## Findings
1) **MEDIUM**: `max_price_decimals` が `tick_size` 由来のため、`tickSize` が未提供の場合の精度が公式仕様とズレる可能性
   - 実装では `tick_size` が `None` の場合に `0.01` を採用し、`max_price_decimals` を `tick_size.scale()` で決定しています。
     - 参照: `crates/hip3-registry/src/spec_cache.rs:96-115`
   - 公式ドキュメントでは価格の最大小数桁は `MAX_DECIMALS - szDecimals`（perpsなら `6 - szDecimals`）とされており、`tickSize` の存在は明記されていません。`tickSize` が実際に返らない場合、`0.01` の固定値は市場によって過剰に粗い丸めになり、価格精度が不要に落ちる可能性があります。citeturn0search0
   - 対応案: `tickSize` が取得できない場合は `max_price_decimals = 6 - szDecimals` を使う、または API 実測で `tickSize` の存在/型を確定して仕様化する。

2) **LOW**: `tickSize` が JSON 数値の場合、`f64 -> Decimal` 変換の丸め誤差リスク
   - `deserialize_tick_size()` は数値型を `f64` 経由で `Decimal::try_from()` しているため、極小の tickSize で丸め誤差が発生する可能性があります。
     - 参照: `crates/hip3-registry/src/preflight.rs:16-59`
   - 影響は限定的ですが、厳密性が必要なら `serde_json::Number` から文字列経由で `Decimal` へ変換するほうが安全です。

## Open Questions / Assumptions
- `perpDexs` または `metaAndAssetCtxs` に `tickSize` が実際に含まれるかは未検証のままです。API 実測で確認し、仕様として固定しておくのが望ましいです。citeturn0search0

## Tests
- 実行結果はユーザー報告（`373 passed, 0 failed, 1 ignored`）の通り。ローカルでは未実行。
