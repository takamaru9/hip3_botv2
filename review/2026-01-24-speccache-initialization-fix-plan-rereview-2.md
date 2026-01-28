# SpecCache Initialization Fix Plan 再レビュー (2)

対象: `.claude/plans/2026-01-24-speccache-initialization-fix.md`

## Findings

- [LOW] `hip3-registry/src/lib.rs` の export 追加が冗長です。`PreflightChecker` は既に公開されており、`validate_market_keys` だけ追加すれば十分です。不要な再exportは避ける方針なら、ここは削除/縮小を明記してください。 (L538-546)
- [LOW] `use rust_decimal::Decimal;` を app.rs に追加していますが、コード例では `Decimal` を直接使っていません。未使用 import になるため、必要な箇所に限定するか削除を推奨します。 (L532-539)

## Residual Risks / Gaps

- なし

## Change Summary

- 主要な不整合はすべて解消されています。残るのは export/unused import の整理だけです。
