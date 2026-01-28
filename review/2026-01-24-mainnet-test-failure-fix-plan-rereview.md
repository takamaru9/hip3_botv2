# Mainnet Test Failure Fix Plan 再レビュー

対象: `.claude/plans/2026-01-24-mainnet-test-failure-fix.md`

## Findings

- [HIGH] Step 4 のフェイルセーフ案が `batch_to_action()` の型と整合していません。現状は `fn batch_to_action(&self, ...) -> Action` ですが、計画では `return None` を入れておりコンパイル不整合になります。`Option/Result` への型変更・呼び出し側のハンドリング（`tick()` 側の早期 return と `handle_send_failure` 連携）を計画に明記してください。 (L278-307, L312-325)
- [HIGH] `SpecCache` 未充足時に「スキップ」すると、バッチ送信は成功扱いのまま注文が消える可能性があります。特に `reduce_only` は再送必須のため、欠損が1件でもあればバッチ全体を失敗扱いにするか、スキップした注文を `handle_send_failure` 相当でクリーンアップ/再キューする方針を明記してください。 (L285-307, L328)
- [MEDIUM] 代替案の `ExecutorError::MarketSpecNotFound` は現行の `ExecutorError` に存在しません。追加するか、既存のエラー種別で表現するかを決めて計画に反映してください。 (L312-320)
- [MEDIUM] テスト例の `MarketSpec` と `PendingOrder` が必須フィールド不足でコンパイルしません。`MarketSpec` には `min_size/max_leverage/fees/name/max_sig_figs` などが必須、`PendingOrder` には `cloid/market/reduce_only/created_at` が必須です。既存テストヘルパーがあれば流用する前提を明記してください。 (L397-415, L429-433)

## Residual Risks / Gaps

- `SpecCache` 取得失敗時の挙動が運用上の安全性に直結するため、最終方針（バッチ失敗 or 部分スキップ）を選定し、再送/クリーンアップの仕様を確定させる必要があります。 (L285-307, L328)

## Change Summary

- 主要な前回指摘（コンストラクタ更新、from_bytes の使用/テスト、ABテスト根拠）は解消されていますが、`batch_to_action` の型整合とスキップ時の注文消失リスクが未解決です。
