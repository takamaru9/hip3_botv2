# WebSocket POST JSON Format Fix Plan 再レビュー2

対象: `.claude/plans/2026-01-25-ws-post-json-format-fix.md`

## Findings

- [HIGH] 依頼内容（Testnet が難しいので Mainnet 少額テストで確認）と計画が矛盾しています。Phase A/B の検証手順と Non‑Negotiable Requirements が **Testnet 前提** のままで、Mainnet 実施時の手順・条件・安全策が明文化されていません。Mainnet 実施を認めるなら、Phase A/B の検証環境を置き換えた上で、Mainnet 用の安全ガード（極小サイズ・指値乖離・即時キャンセル・損失上限・緊急停止手順）を「必須条件」として追記してください。`/.claude/plans/2026-01-25-ws-post-json-format-fix.md:92` `/.claude/plans/2026-01-25-ws-post-json-format-fix.md:101` `/.claude/plans/2026-01-25-ws-post-json-format-fix.md:167` `/.claude/plans/2026-01-25-ws-post-json-format-fix.md:306` `/.claude/plans/2026-01-25-ws-post-json-format-fix.md:437` `/.claude/plans/2026-01-25-ws-post-json-format-fix.md:445`
- [MEDIUM] 未確認事項の「Testnet で AB 検証」および代替案の検証先が Testnet 固定です。Mainnet に切り替える場合、AB の順序と「省略/ null / 空文字 / signer address」の比較手順を **Mainnet 向けに再定義** しないと、検証結果の整合性が崩れます（例: Phase A/B の実施順と、Alternative A/B の再試行条件）。`/.claude/plans/2026-01-25-ws-post-json-format-fix.md:88` `/.claude/plans/2026-01-25-ws-post-json-format-fix.md:352`
- [LOW] Testnet コマンドがそのまま残っているため、Mainnet で実行する場合の起動方法・設定ファイル（例: `config/mainnet.toml`）を明示しておく必要があります。`/.claude/plans/2026-01-25-ws-post-json-format-fix.md:175` `/.claude/plans/2026-01-25-ws-post-json-format-fix.md:313`

## Residual Risks / Gaps

- Mainnet での検証は実損リスクがあるため、Phase A/B を Mainnet に切り替える場合は「最小ロット」「約定を避ける価格」「自動キャンセル」「損失上限」「即時停止」の実施条件を明文化しない限り、運用リスクが残ります。

## Change Summary

- 既知の失敗記録やテスト名の整合などは改善されています。残りは **Mainnet 実施前提への手順・要件の全面的な整合** です（Testnet 前提の手順がそのまま残っています）。
