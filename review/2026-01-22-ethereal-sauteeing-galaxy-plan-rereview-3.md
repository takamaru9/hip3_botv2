# ethereal-sauteeing-galaxy プラン再々々レビュー（メインネット少額テスト設定）

確認日: 2026-01-22  
対象: `/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md`

---

## 結論

**承諾**。前回までの指摘（API wallet 前提のアドレス分離、起動手順、`[[markets]]` 明示、`[detector]` の必須項目、`perpDexs` 由来の `asset_idx` 取得手順、参照レビューの更新）が反映され、手順として成立しています。

---

## 確認できたポイント

- `user_address`（口座/購読）と `signer_address`（署名鍵/検証）を分離し、API wallet 運用の落とし穴（agent walletで口座情報が空になる）を回避できる構成になっている
- `config/mainnet-test.toml` が現実装のconfigスキーマに沿っており、`cargo run -- --config ...` で起動できる
- `asset_idx` を `perpDexs` の `assetToStreamingOiCap` から 0-indexed で確定できる（`jq` が成立）

---

## 注意（承諾範囲外の運用メモ）

- `signer_address` を省略すると起動時の鍵検証が無くなるため、可能なら設定推奨
- HardStop の全cancel+全flatten が未完のため、UIでの手動flatten手順は必ず準備

