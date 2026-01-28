# WebSocket POST JSON Format Fix Plan レビュー

対象: `.claude/plans/2026-01-25-ws-post-json-format-fix.md`

## Findings

- [HIGH] 段階的検証の方針と実装手順が矛盾しています。Phase A/B の順で切り分ける方針と「一度に複数の変更を入れない」が明記されている一方で、Implementation Steps は Step 1 と Step 2 を先に両方適用し、その後に Testnet 検証になっています。これだと原因切り分けができません。Step 1 適用→Testnet→Step 2 適用→Testnet の順に手順を組み替える必要があります。`/.claude/plans/2026-01-25-ws-post-json-format-fix.md:81` `/.claude/plans/2026-01-25-ws-post-json-format-fix.md:95` `/.claude/plans/2026-01-25-ws-post-json-format-fix.md:315`
- [MEDIUM] 「vaultAddress の有/無 AB 検証」を掲げているのに、Step 1 で常に `vaultAddress: null` を出力する設計に変えるため “省略” ケースが再現できません。AB を成立させるには、送信時に `vaultAddress` を出す/出さないを切り替えられるフラグや一時的な分岐が必要です。`/.claude/plans/2026-01-25-ws-post-json-format-fix.md:74` `/.claude/plans/2026-01-25-ws-post-json-format-fix.md:97` `/.claude/plans/2026-01-25-ws-post-json-format-fix.md:301`
- [MEDIUM] `vaultAddress` を「明示的に null で出す」ことを検証するテスト項目が不足しています。現状の `serde_json` 取得ではキー欠落でも `null` と見なされるため、`vaultAddress` が実際に出力されているか判定できません。Step 3 に「キーの存在確認」アサート追加を明記してください。`/.claude/plans/2026-01-25-ws-post-json-format-fix.md:221`
- [LOW] `signature.v` を `u8` に戻す変更は `hip3-ws`/`hip3-executor` の複数テストに波及します。Step 3 が抽象的で漏れやすいので、更新対象テスト（例: `crates/hip3-ws/src/message.rs` の PostRequest 直列化テスト、`crates/hip3-executor/src/ws_sender.rs`/`real_ws_sender.rs` のテスト）を列挙しておくと実装漏れを防げます。`/.claude/plans/2026-01-25-ws-post-json-format-fix.md:221`

## Residual Risks / Gaps

- `vaultAddress: null` がサーバ側で “無効なアドレス” と扱われる可能性があるため、テスト結果に応じて Alternative A/B へ即座に切り替える判断基準を明文化しておくと安全です。

## Change Summary

- 主要方針は妥当ですが、段階的検証の流れが手順に反映されていない点と、AB 検証のための “省略ケース” の作り方が不足しています。テスト項目の具体化で実装漏れを防げます。
