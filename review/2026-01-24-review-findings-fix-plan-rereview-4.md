# Review Findings Fix Plan レビュー（再確認 4）

対象: `.claude/plans/2026-01-24-review-findings-fix.md`

## Findings (ordered)

1) Medium: F2 の統合テスト配置が曖昧です。ワークスペース直下は package が無いので `tests/` は実行対象になりません。`crates/hip3-ws/tests/ws_shutdown_test.rs` のように、対象 crate の tests ディレクトリ配下に置く前提を明記してください。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:455-464`

2) Low: F1 の `fill()` 後の `tokio::task::yield_now()` は非決定的で、ポジション反映が間に合わない可能性があります。`pt.has_position(&market_a)` をポーリングするなど、反映待ちの同期方法を明記するとテストのフレーキーさを抑えられます。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:238-241`, `.claude/plans/2026-01-24-review-findings-fix.md:296-298`

## Doc Check

- order status 一覧と orderUpdates のデータ構造は公式ドキュメントの記載と整合していることを確認済みです。

## Change Summary

- 主要な API 互換性は整ってきました。残るのはテスト配置とテスト同期の明確化のみです。これらを直せば計画として実装可能な状態です。
