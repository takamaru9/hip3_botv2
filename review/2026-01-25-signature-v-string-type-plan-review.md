# Signature `v` Field String Type Fix Plan レビュー

対象: `.claude/plans/2026-01-25-signature-v-string-type.md`

## Findings

- [MEDIUM] `SignedActionBuilder::with_signature_parts(self, r, s, v: u8)` が残ったままだと `ActionSignature.v: String` に変更後コンパイルできません。`with_signature_parts` の引数型を `String` に揃えるか、内部で `v.to_string()` する修正を計画に追加してください。 (`crates/hip3-executor/src/ws_sender.rs`)
- [MEDIUM] 公式ドキュメントは例として `v` を文字列で示していますが、型の明示はありません。誤って API が数値を要求していた場合、今回の変更で逆に失敗する可能性があります。Testnet で `v` 数値/文字列の AB 検証を計画に加えてください。citeturn2open0

## Residual Risks / Gaps

- ドキュメントが型を明言していないため、最終的な互換性は実測で確認が必要です。citeturn2open0

## Change Summary

- 主要な変更点は妥当ですが、`with_signature_parts` の型更新と、`v` 型の実測検証が不足しています。
