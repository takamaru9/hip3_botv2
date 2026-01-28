# SpecCache Initialization Fix Plan 再レビュー

対象: `.claude/plans/2026-01-24-speccache-initialization-fix.md`

## Findings

- [HIGH] `populate_spec_cache()` が `self.config.set_xyz_dex_id(...)` を呼んでいますが、その API は存在しません。`xyz_dex_id` は `App` のフィールドで、`config` には保持されていないため、このままではコンパイルできません。`self.xyz_dex_id = Some(dex_id)` に修正し、`get_dex_id()` が正しい値を返す流れに合わせてください。 (L280-318)
- [HIGH] `validate_configured_markets()` が `self.config.markets()` と `m.key` を参照していますが、`AppConfig` に `markets()` はなく `MarketConfig` に `key` もありません。`get_markets()` から `asset_idx` を取り、`MarketKey::new(self.get_dex_id(), AssetId::new(m.asset_idx))` を構築する形に修正してください。 (L356-407)
- [MEDIUM] `RawPerpSpec.tick_size` を `Option<Decimal>` に変更するのに対して、`spec_cache::parse_spec()` の既存実装は `Option<String>` 前提で `parse::<Decimal>()` を実行しています。型変更に合わせた具体的な修正手順が不足しています。`raw.tick_size.map(Price::new)` に置き換える等、明示的に計画へ追記してください。 (L458-472)
- [MEDIUM] `spec_cache.rs` のテストは `RawPerpSpec { tick_size: Some("0.01".to_string()) }` 形式で多数構築しています。型変更後はテストが一斉に壊れるため、`Decimal::from_str("0.01")` へ更新する計画を追加してください。 (L458-472)
- [LOW] `hip3-registry/Cargo.toml` への `rust_decimal` 追加は既に workspace で依存定義済みのため冗長です。不要なら省略し、追加する場合でも `workspace = true` で統一する旨を記載してください。 (L162-175, L633-639)

## Residual Risks / Gaps

- なし

## Change Summary

- DEX index 確定や tickSize の型耐性など主要方針は改善されていますが、API不整合（`set_xyz_dex_id` / `markets()` / `m.key`）と `RawPerpSpec` 型変更に伴う実装・テスト更新が未整理です。
