# WebSocket POST JSON Format Fix Plan 再レビュー3

対象: `.claude/plans/2026-01-25-ws-post-json-format-fix.md`

## Findings

- [LOW] Step B-2 のテスト一覧で `test_post_request_with_vault_address` の `v` が "27" と記載されていますが、現行テストは `"28"` です。数値化の趣旨は正しいものの、**値が不一致**なので実装時に混乱を招きます。実在する値に合わせて `28` へ修正するか、テストの値を変更するなら理由を追記してください。`/.claude/plans/2026-01-25-ws-post-json-format-fix.md:339`

## Residual Risks / Gaps

- なし

## Change Summary

- Mainnet 少額テストへの置き換えと安全ガードは十分に整理されています。残りはテスト一覧の値の整合だけです。
