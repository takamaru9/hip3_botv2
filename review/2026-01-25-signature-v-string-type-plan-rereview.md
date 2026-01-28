# Signature `v` Field String Type Fix Plan 再レビュー

対象: `.claude/plans/2026-01-25-signature-v-string-type.md`

## Findings

- [LOW] `hip3-ws/src/message.rs` の PostRequest シリアライズテストは `signature.v` の値だけでなく、`json[...]["v"]` のアサートも数値→文字列に更新が必要です。計画では `v: "27"` への変更だけが記載されているため、アサート更新を明記してください。 (`crates/hip3-ws/src/message.rs` テスト)
- [LOW] `SignaturePayload` は `Deserialize` を保持したまま `v: String` になるため、将来もし数値 `v` を受け取るケースがあるとデシリアライズが失敗します。実際にデシリアライズ経路が無いことを明記するか、許容型デシリアライザ/`Deserialize` の削除を検討してください。

## Residual Risks / Gaps

- なし

## Change Summary

- 主要な修正方針は妥当で、AB 検証計画も追加済みです。残りはテストのアサート更新と `Deserialize` の扱いの明確化です。
