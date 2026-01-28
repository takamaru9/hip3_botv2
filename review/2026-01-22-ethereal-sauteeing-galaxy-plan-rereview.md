# ethereal-sauteeing-galaxy プラン再レビュー（メインネット少額テスト設定）

確認日: 2026-01-22  
対象: `/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md`

---

## 結論

**未承諾**。前回指摘（API wallet前提のアドレス分離、起動コマンド、`[[markets]]` 明示）は反映されています。  
ただし、現状のままだと **設定がパースできない/asset_idx を確定できない** などで手順が成立しません。次の3点を直してください。

---

## 指摘（今回の指摘: 3点）

### 1) `config/mainnet-test.toml` の `[detector]` が不完全で、設定パースが失敗する
`DetectorConfig` は `max_notional` だけではデシリアライズできません（他フィールドが必須）。  
→ `config/mainnet-test.toml` の `[detector]` は `config/mainnet.toml` の内容を **丸ごとコピー**した上で `max_notional = 20` だけ変更、の形にしてください。

### 2) `asset_idx` 取得手順が成立していない（`meta`/`default.toml` では決まらない）
プランの Step 1:
- `cargo run -- --config config/default.toml | grep -i TLT` は **testnet設定**かつ `[[markets]]` が既に入っているため、xyz市場の自動発見になりません
- `curl ... {"type":"meta"}` は HIP-3 DEX の `asset_idx`（= `perpDexs` の列挙順）を決める情報源ではありません

→ `asset_idx` は `perpDexs` から固定してください（例）:
- `curl -s https://api.hyperliquid.xyz/info -H 'Content-Type: application/json' -d '{\"type\":\"perpDexs\"}' | jq ...` で `name == \"xyz\"` の `assetToStreamingOiCap` を `to_entries` し、`\"xyz:TLT\"` の **index** を `asset_idx` として採用

### 3) `signer_address` の説明が実装と不整合（「省略可（自動検証）」になっている）
現実装は `signer_address` を **設定した場合のみ** `HIP3_TRADING_KEY` の導出アドレス一致を検証します。省略すると検証しません。  
→ プランの表記を以下に修正してください。
- `signer_address`: **推奨（可能なら必須）**。設定すると起動時に `HIP3_TRADING_KEY` の誤設定を検知できる

