# ethereal-sauteeing-galaxy プラン再々レビュー（メインネット少額テスト設定）

確認日: 2026-01-22  
対象: `/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md`

---

## 結論

**未承諾**。前回指摘（`[detector]` 完備、`perpDexs` 由来の `asset_idx`、`signer_address` の位置づけ）は概ね反映されています。  
ただし、現状の Step 1 の `jq` が成立しておらず **`asset_idx` を確定できない**ため、手順としてまだ不十分です。以下の3点を修正してください。

---

## 指摘（今回の指摘: 3点）

### 1) Step 1 の `jq` が壊れていて `asset_idx` が取れない
`assetToStreamingOiCap` は「配列（[asset_key, cap] の配列）」なので、`to_entries` 後の `.key` は **index文字列**であり `"TLT"` は含まれません（`contains("TLT")` が成立しない）。

→ 例として、下のどちらかに置き換えてください（0-indexed）。

**推奨（完全一致で index を出す）**
```bash
curl -s https://api.hyperliquid.xyz/info \
  -H 'Content-Type: application/json' \
  -d '{"type":"perpDexs"}' \
  | jq -r '.[] | select(.name=="xyz") | .assetToStreamingOiCap
           | to_entries[]
           | select(.value[0]=="xyz:TLT")
           | (.key|tonumber)'
```

**確認用（index と name を並べて表示）**
```bash
curl -s https://api.hyperliquid.xyz/info \
  -H 'Content-Type: application/json' \
  -d '{"type":"perpDexs"}' \
  | jq -r '.[] | select(.name=="xyz") | .assetToStreamingOiCap
           | to_entries[]
           | "\(.key)\t\(.value[0])"'
```

### 2) 「簡易版（手動で index を数える）」の `jq keys` は目的に合っていない
`keys` は index（0..n-1）しか出さないので `"xyz:TLT"` を探せません。  
→ こちらも上の「確認用（index と name）」に差し替えてください。

### 3) 参照レビューが古い（API wallet前提の差分が反映されたレビューに差し替え推奨）
プラン冒頭が `review/2026-01-21-mainnet-micro-test-readiness-rereview.md` のままですが、現状のプランは **API wallet前提のアドレス分離**を含みます。  
→ 追記でよいので、次のレビューも参照に入れてください:
- `review/2026-01-22-api-wallet-address-separation-review.md`

