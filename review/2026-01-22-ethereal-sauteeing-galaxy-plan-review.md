# ethereal-sauteeing-galaxy プランレビュー（メインネット少額テスト設定）

確認日: 2026-01-22  
対象: `/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md`

---

## 結論

**未承諾**。方向性は良いですが、Hyperliquid公式Docs（API wallets / Exchange endpoint）と照らすと、アドレス/起動手順/市場指定が不整合で、誤設定のまま走り得ます。以下の3点を直してください。

---

## 指摘（今回の指摘: 3点）

### 1) `user_address` と `HIP3_TRADING_KEY` の導出アドレスは一致しないケースがある（API wallet前提）
Docs上、**API wallet（agent wallet）は「署名だけ」**に使われ、口座データ取得/購読の `user` には **master/subaccount の実アドレス**を渡す必要があります。  
一方プランの「鍵・アドレス一致強制（user_address と HIP3_TRADING_KEY 不一致で起動エラー）」は前提が逆です。

→ プランを以下の形に修正してください。
- `user_address`: 口座（購読/情報取得）のアドレス（master もしくは subaccount）
- `signer_address`（推奨）: `HIP3_TRADING_KEY` から導出されるアドレス（API wallet/執行鍵）の一致検証に使う
- subaccount/vaultで取引する場合のみ `vault_address` を設定（Docsの `vaultAddress` に対応）

### 2) 起動コマンドが実装と不整合（`HIP3_CONFIG=... cargo run` では読まれない）
現状 `hip3-bot` は `--config` 引数で設定ファイルを読む実装なので、プランの
`HIP3_CONFIG=config/mainnet-test.toml cargo run`
は成立しません。

→ 例として以下に直してください。
- `cargo run -- --config config/mainnet-test.toml`

（あわせて `max_notional` はトップレベルではなく `[detector] max_notional = 20` のように **tomlのセクション込み**で記載してください）

### 3) `markets` の指定が曖昧で、Trading mode の要件（`[[markets]]` 必須）に落ちない
Trading mode は自動発見を禁止しているため、`[[markets]]` で `asset_idx` と `coin` を **確定値で**入れる必要があります。  
プランの `markets | xyz:TLT (asset_idx: API から取得)` だけだと、設定ファイルに落とせず誤設定になり得ます。

→ プランに次を追記してください。
- `config/mainnet-test.toml` に `[[markets]] asset_idx = ... / coin = "xyz:TLT"` を明記
- `asset_idx` を取得して固定する手順（例: 事前に observation で起動してログから拾う、または info endpoint の `meta/universe` で確認）

