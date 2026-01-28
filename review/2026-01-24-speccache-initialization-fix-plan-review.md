# SpecCache Initialization Fix Plan レビュー

対象: `.claude/plans/2026-01-24-speccache-initialization-fix.md`

## Findings

- [HIGH] `populate_spec_cache()` が `get_dex_id()` に依存しており、`run_preflight()` の「markets 設定済み」経路では `xyz_dex_id` が未設定のまま `DexId::XYZ` (0) にフォールバックします。もし perpDexs の xyz dex が index 0 でない場合、SpecCache のキーがズレて `MarketSpec not found` が再発します。perpDexs 取得後に必ず xyz dex index を確定し（PreflightChecker 相当のロジックを再利用）、`xyz_dex_id` を設定してから `populate_spec_cache()` で使用するように計画へ明記してください。 (L179-199, L247-279)
- [HIGH] `tick_size: Option<String>` は API が `tickSize` を数値で返した場合にデシリアライズが失敗します。公式 docs の `perpDexs`/`meta` レスポンスに `tickSize` の型や存在が明記されていないため、型不一致は現実的リスクです。`String`/`number` 両対応のデシリアライザ（例: `serde_with::DisplayFromStr` や `deserialize_with` で `Decimal` に吸収）を計画に含めてください。 (L92-121, L21-25) citeturn4view0
- [MEDIUM] DEX の検索が `starts_with` かつ大小文字区別になっており、preflight の `contains` + case-insensitive と一致しません。設定が `XYZ` などのケースで一致せず、SpecCache が未初期化になる可能性があります。preflight と同一ロジックへ統一するか、共通ヘルパー化してください。 (L182-187)
- [MEDIUM] markets 設定済み経路で preflight 検証が完全にスキップされるため、`asset_idx` の誤設定や coin 名の不整合を検知できません。SpecCache を populate するだけでは防げないので、設定済みの場合でも `perpDexs` と突合する検証（最低でも asset_idx の存在確認）を追加してください。 (L274-279)

## Residual Risks / Gaps

- `perpDexs` のレスポンススキーマが docs に掲載されていないため、`tickSize` の実測確認は必須です。型が判明するまではデシリアライズを耐性化しておくのが安全です。 (L21-25) citeturn4view0

## Change Summary

- SpecCache の自動初期化方針は妥当ですが、DEX index 確定・tickSize の型耐性・設定済み市場の検証が不足しています。これらを追加すれば、`MarketSpec not found` の再発リスクを大きく減らせます。
