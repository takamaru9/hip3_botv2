# orderUpdates 配列形式対応 修正計画 リレビュー

対象: `.claude/plans/2026-01-24-orderUpdates-array-format-fix.md`

## Findings

- [MEDIUM] `data` が Array/Object 以外の場合に `failed_count = 0` で握りつぶされます。エラー可視性を維持する方針なのに、この分岐は warn に繋がらず「失敗なし」に見えるため、少なくとも `failed_count = 1` とするか warn を出すべきです。 (L155-197)
- [LOW] コードフェンスの閉じ忘れがあり、以降のセクションがコードブロックに巻き込まれています。`app.rs` の例（P0-4）と `P1-6` のテスト例で ` ``` ` が閉じられていません。 (L216-242, L427-443)

## Residual Risks / Gaps

- 空配列が実際に届くかは未確認のため、実測ログが揃うまで “正常扱い” の判断は暫定です（現状は Residual Risk に記載済み）。

## Change Summary

- P0 で `limitPx` / `timestamp` 対応と `failed_count` を導入した点は良い方向です。上記の可視性とドキュメント整形だけ修正すれば実装に進めます。
