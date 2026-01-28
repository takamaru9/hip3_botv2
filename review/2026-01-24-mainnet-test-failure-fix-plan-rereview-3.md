# Mainnet Test Failure Fix Plan 再レビュー (3)

対象: `.claude/plans/2026-01-24-mainnet-test-failure-fix.md`

## Findings

- [MEDIUM] `cancel` アクションに `grouping: Some("na")` を付与していますが、公式仕様では cancel は `type` と `cancels` のみで `grouping` はありません。既存実装も cancel では `grouping: None` です。スキーマ差異で拒否される可能性があるため、cancel では `grouping: None` に戻してください（order のみ `grouping: Some("na")`）。 (L351-368)

## Residual Risks / Gaps

- なし

## Change Summary

- 主要な整合性問題は解消しています。残るのは cancel アクションの `grouping` 付与のみです。
