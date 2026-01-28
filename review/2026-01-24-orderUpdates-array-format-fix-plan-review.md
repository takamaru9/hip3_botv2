# orderUpdates 配列形式対応 修正計画 レビュー

対象: `.claude/plans/2026-01-24-orderUpdates-array-format-fix.md`

## Findings

- [HIGH] 公式仕様の `limitPx` / `timestamp` が現行構造体に無いままなので、配列対応だけでは実運用で取り逃がしが残る可能性が高いです。P1 に先送りされていますが、公式 docs は `WsOrder[]` + `limitPx` + `timestamp` を示しており、`px` 固定のままだと要素単位でパース失敗→空配列扱いになります。少なくとも `#[serde(alias = "limitPx")]` と `timestamp` の optional 追加は P0 に前倒しし、テストも `limitPx` / `timestamp` 版を含めてください。 (L24-41, L44-49, L105-130, L165-188, L196-229)
- [MEDIUM] エラー可視性が低下します。従来は `warn!` でパース失敗が検知できましたが、新設 `as_order_updates()` は空 Vec を返し、呼び出し側は debug ログのみで終了します。`data` が非空で `updates.is_empty()` の場合は warn を残すか、`Result<(Vec<_>, ParseStats)>` のように失敗数を返して通知できる形にしてください。 (L105-130, L149-156)
- [LOW] 追加テストが `px` 前提で、公式スキーマとの差異を検知できません。`limitPx` と `timestamp` を含む配列/単体オブジェクトのテストを追加し、互換性の確認に使える形にしてください。 (L196-229, L270-286, L298-325)

## Residual Risks / Gaps

- `orderUpdates` の空配列が実際に届くかは未確認です。空配列を正常扱いにする方針なら、その根拠（実測ログ）を残すか、`data` 非空時は警告を出す仕様にしておくと安全です。 (L149-156)

## Change Summary

- 配列対応の方向性は妥当ですが、公式スキーマとの差異と観測性低下が残っています。P1 の前倒し＋テスト強化が必要です。
