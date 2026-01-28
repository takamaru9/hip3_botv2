# API wallet 前提のアドレス分離 再レビュー

確認日: 2026-01-22  
対象: `hip3-bot` / `hip3-executor`（main口座アドレスと執行鍵アドレスの分離）

---

## 結論

**計画としてはOK**（main口座アドレス ≠ 執行鍵アドレス、という前提を置ける設計に寄せました）。  
ただしこの環境からは Hyperliquid 公式ドキュメントのホスト名が解決できず、一次情報の再確認ができません。`vaultAddress/active_pool` を **main口座で必須とするか**は、ドキュメント該当箇所の貼り付けで最終確認してください。

---

## 変更点（実装）

### 1) `user_address` と `HIP3_TRADING_KEY` の一致強制を撤回（分離対応）
- Trading mode の `user_address` は **購読/口座スコープ**として必須（形式チェックのみ）。
- `HIP3_TRADING_KEY` の導出アドレスは `signer_address` を設定した場合のみ一致検証（API wallet の誤設定検知用）。
- 実装: `crates/hip3-bot/src/app.rs` `crates/hip3-bot/src/config.rs`

### 2) `vaultAddress` を署名と post の両方へ反映できるように拡張
- `vault_address`（= active_pool 相当）を設定した場合:
  - 署名の `SigningInput.vault_address` に反映（action_hashが変わる）
  - WS post payload の `vaultAddress` にも反映（署名と整合）
- 実装: `crates/hip3-executor/src/executor_loop.rs` `crates/hip3-executor/src/real_ws_sender.rs`

---

## 設定スキーマ（Trading の必須/推奨）

- 必須: `mode="trading"`, `is_mainnet=true|false`, `user_address="0x..."`, `private_key`（env利用の有効化）, `[[markets]]...`
- 推奨: `signer_address="0x..."`（HIP3_TRADING_KEY の導出アドレス一致を検証）
- 条件付き: `vault_address="0x..."`（API wallet/運用形態により必要な場合）

---

## 検証

```bash
cargo test -p hip3-ws -p hip3-executor -p hip3-bot
cargo clippy -p hip3-ws -p hip3-executor -p hip3-bot -- -D warnings
```

