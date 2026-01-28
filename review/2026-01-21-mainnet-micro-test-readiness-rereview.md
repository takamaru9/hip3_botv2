# Mainnet少額テスト 再レビュー（Go/No-Go）

確認日: 2026-01-21  
対象: `hip3-bot` / `hip3-executor` / `hip3-ws`（Phase B: mainnet超小口テストの実行可否）

---

## 結論

**条件付きで Go（少額 mainnet テストに進んでよい）**。  
E2E結線に加えて、mainnet事故に直結しやすい3点（鍵・アドレス不一致、markPx未配線、Tradingでの市場自動全発見）をコード側で塞いだため、**「想定外に全市場へ発注」「約定/更新が取れずに無制限発注」**のリスクが大きく下がっています。

ただし、HardStop の「全cancel + 全flatten」など運用上の安全装置は未完なので、**“少額” + 手動エスケープ手順（UI/CLIでのflatten）を前提**にしてください。

---

## 今回の修正確認（mainnet micro-test 安全性）

### 1) Trading で `user_address` と署名鍵の一致を強制（誤設定での暴走防止）
- `user_address` を `Address` としてパースし、`KeyManager::load(..., expected_address)` で **導出アドレス一致を検証**。
- 不一致なら起動時にエラーで停止（orderUpdates購読アドレスと署名鍵がズレた状態で走らない）。
- 実装: `crates/hip3-bot/src/app.rs`

### 2) `MarketStateCache(markPx)` を bot 側から更新（notional gate 空洞化の解消）
- `MarketEvent::CtxUpdate` で `ctx.oracle.mark_px` を `executor.market_state_cache().update(...)` に反映。
- これにより `Executor::on_signal()` の `MaxPositionPerMarket/Total` が **常時有効**になりやすい。
- 実装: `crates/hip3-bot/src/app.rs`

### 3) Trading mode での市場自動発見（=全市場対象）を禁止
- Trading で `markets` 未指定の場合、`run_preflight()` は **エラーで停止**（auto-discovery 無効化）。
- mainnet超小口テストでの “意図せぬ多市場購読/多市場発注” を防止。
- 実装: `crates/hip3-bot/src/app.rs`

---

## 検証

```bash
cargo test -p hip3-ws -p hip3-executor -p hip3-bot
cargo clippy -p hip3-ws -p hip3-executor -p hip3-bot -- -D warnings
```

---

## 既知の制約（Goの前提 / 今回の承諾範囲外）

- HardStop の完全停止シーケンス（accepted済み注文の cancel/全flatten）は未完（手動 flatten を前提）
- reconnect/再起動時の userFills/orderUpdates のスナップショット整合は今後の強化ポイント

