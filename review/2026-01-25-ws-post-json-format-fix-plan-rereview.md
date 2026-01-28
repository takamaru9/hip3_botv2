# WebSocket POST JSON Format Fix Plan 再レビュー

対象: `.claude/plans/2026-01-25-ws-post-json-format-fix.md`

## Findings

- [MEDIUM] ドキュメントでは、WebSocket の post payload は Exchange endpoint の signed action 例に従うとされ、`vaultAddress` は「vault / subaccount で取引する場合に設定する」項目として記載されています。現プランは「WebSocket では vaultAddress が必須」という仮説に強く寄っているため、Phase A の検証条件に **「vaultAddress を省略した現行形式」** を明示的に含めるか、少なくとも「省略=既知の失敗」かどうかを記録する手順が必要です（仮説と一次情報の整合性がまだ取れていません）。
- [LOW] Step B-2 のテスト一覧に `test_action_signature_from_bytes` / `test_signed_action_builder` が挙がっていますが、現在のコードでは `test_signature_from_bytes` / `test_signature_from_bytes_normalizes_v(_zero)` など別名になっています。実在するテスト名に合わせて列挙し直さないと修正漏れが起きやすいです。

## Residual Risks / Gaps

- `vaultAddress: null` の扱いはサーバ実装依存の可能性があるため、Phase A で拒否された場合に「空文字」/「signer address」へ切り替える判断基準は良いですが、その前提として **省略ケースが本当に無効なのか** を必ず記録しておくのが安全です。

## Change Summary

- 段階的検証・キー存在テスト・代替フロー追加は改善されています。残りは一次情報との整合性（vaultAddress 必須仮説の位置づけ）と、テスト対象の列挙精度の調整です。
