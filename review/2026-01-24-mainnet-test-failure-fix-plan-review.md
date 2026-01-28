# Mainnet Test Failure Fix Plan レビュー

対象: `.claude/plans/2026-01-24-mainnet-test-failure-fix.md`

## Findings

- [HIGH] `ExecutorLoop::new` のシグネチャ前提が現行コードと一致していません。計画では `vault_address` / `interval` を引数に追加していますが、現行は `timeout_ms` を受け取り `PostRequestManager::new(timeout_ms)` を使っています。さらに `with_ws_sender` の更新が計画に含まれていません。このまま実装するとコンパイルエラー/設定ミスになります。`new`/`with_ws_sender` の両方で `spec_cache` と `timeout_ms` を扱う形に計画を修正し、全呼び出し元を列挙してください。 (L160-178, L233-242)
- [HIGH] `spec_cache.get(...).expect(...)` は SpecCache 未充足時にパニックします。SpecCache は非同期更新なので、起動直後や市場追加時に落ちる可能性が高いです。`Result` で上位に伝播するか、注文をスキップして警告を出すなど、フェイルセーフ設計を追加してください。 (L221-224)
- [MEDIUM] P3 の「from_bytes が未使用かも」という記述は誤りです。`SignedActionBuilder::with_signature()` で実際に使われています。`0x` 追加でテスト `test_signature_from_bytes` の期待値（長さ 64）も壊れるため、使用箇所の洗い出しとテスト更新を計画に入れてください。 (L341-358)
- [MEDIUM] P0 の根拠がドキュメント上で確定していません。公式ドキュメントは signature 構造を示すのみで `0x` 必須の明記がなく、サードパーティ例では `0x` 付きです。SDK 実装の一次ソースか、Testnet で `0x` 有無の AB テストを計画に明記しないと原因断定が弱いです。 (L29-31, L52-80)
- [LOW] P1 のテスト計画が抽象的です。`OrderWire::from_pending_order` が `format_price/size` を使うことを直接検証するユニットテスト（丸め方向、sig figs/tick/lot を含む）を追加しておくと回 regressions を防げます。 (L270-274)

## Residual Risks / Gaps

- 精度制限の正確な値は未確認のままです。`meta` 取得結果と `MarketSpec` の丸め仕様を突き合わせる検証を Testnet 手順に組み込む必要があります。 (L20-25)
- P4 の市場別閾値を採用するなら、設定読み込み/適用ロジックの追加とテストが必要ですが、実装ステップが明示されていません。 (L375-392)

## Change Summary

- 方向性は妥当ですが、`ExecutorLoop` のシグネチャ前提のズレと SpecCache 未充足時のパニックが大きなリスクです。P0 の根拠は一次ソース or AB テストで明確化し、P1/P3 はテスト計画を具体化してください。
