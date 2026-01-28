# Phase B 実装コードレビュー（プラン準拠性）

確認日: 2026-01-21
対象: Phase B 実装（主に `crates/hip3-executor`, `crates/hip3-position`, `crates/hip3-core`）

---

## 最終結論

**承諾（条件付き / 範囲限定）**。`v`値変換の修正（27/28）まで含めて、Phase B の内部ロジック（署名/キュー/Position/Risk gate）の整合とテストはOK。次の「hip3-ws統合（実送信・応答結線）」の実装に進めます。

ただし以下は **未完了** のため、**Testnet/実運用での実弾（＝実送信＋緊急停止の完結）としては未承諾** です:
- 実WsSender実装（hip3-ws統合）
- HardStop完全シーケンス（oid追跡・全cancel）
- WS応答処理の結線

---

## レビュー履歴

### 初回レビュー（3点）→ ✅ 全修正完了

| 指摘 | 問題 | 状態 |
|------|------|------|
| 1 | ExecutorLoop WS送信未実装 | ✅ 修正: WsSender trait + 署名統合 + sent状態管理 + 再キュー |
| 2 | MaxPositionTotal pending未加算 | ✅ 修正: mark_px統一 + Decimal比較 + pending合算 |
| 3 | pending_markets_cache 二重減算 | ✅ 修正: remove_order一本化 |

詳細: `review/2026-01-21-phase-b-executor-implementation-3.7-ws-integration-review.md`

### 再レビュー（3点）→ ✅ 1点修正 / 🟡 2点未修正

| 指摘 | 問題 | 状態 | 対応内容 |
|------|------|------|----------|
| 1 | v が 0/1 のまま | ✅ 修正 | `27 + signature.v() as u8` で recovery id に変換 |
| 2 | HardStop 停止シーケンス | 🟡 部分対応 | `TrackedOrder` に `oid` が無く、accepted済み注文を cancel できない（設計上の制約）。対応案は下記参照 |
| 3 | 実WsSender実装 | 🟡 未実装 | hip3-ws への統合が必要（別タスク） |

---

## 指摘1 v値変換（✅ 修正済み）

**問題**: `signature.v() as u8` は y_parity (0/1) を返すが、SDK wire は recovery id (27/28) が必要。

**修正**: `crates/hip3-executor/src/executor_loop.rs:380`
```rust
v: 27 + signature.v() as u8, // Convert y_parity (0/1) to recovery id (27/28)
```

---

## 指摘2 HardStop 停止シーケンス（🟡 部分対応）

**現在実装済み**:
- `drop_new_orders()`: pending queue の new_order を purge
- `remove_order()`: cleanup で pending_markets_cache 整合
- reduce_only 再キュー: timeout/失敗時に `enqueue_reduce_only()`

**未修正（ブロッカー）**:
- 全 pending 注文のキャンセル: oid 追跡が必要（現在 TrackedOrder に oid なし）
- 全 position の flatten: `positions_snapshot()` は存在するが、PriceProvider 注入が必要
- 完了条件チェック: position=0, pending=0 の監視ループ

**設計上の制約**:
- oid は exchange accepted 後に付与されるため、現在の TrackedOrder では追跡不可
- 対応案: TrackedOrder に `oid: Option<u64>` 追加 + orderUpdate 処理で設定

---

## 指摘3 WS統合（🟡 設計完了 / 実装未）

**現在実装済み**:
- `WsSender` trait: dyn-compatible async trait（BoxFuture使用）
- `SendResult` enum: Sent/Disconnected/RateLimited/Error
- `SignedAction` struct: action + nonce + signature + post_id
- `MockWsSender`: テスト用実装（82テスト通過）

**未修正（ブロッカー）**:
- `impl WsSender for RealWsSender`: hip3-ws の ConnectionManager と接続
- WS応答処理: `on_response_ok/on_response_rejected` を実WebSocketに結線
- Trading モードでの `ws_sender == None` を fatal 化

**現在のフォールバック動作**:
- `ws_sender == None` の場合はテストモードとして送信をシミュレート
- 本番前に `is_trading_mode` フラグで制御必須

---

## テスト結果

```
cargo test -p hip3-executor
test result: ok. 82 passed; 0 failed; 0 ignored

cargo clippy -p hip3-executor -- -D warnings: ✅ 成功
```

---

## Testnet移行チェックリスト

| 項目 | 状態 | 備考 |
|------|------|------|
| NonceManager | ✅ | Clock trait + server_offset同期 |
| Signer (EIP-712) | ✅ | PhantomAgent 署名 |
| BatchScheduler | ✅ | 3-tier queue + inflight 100 |
| Executor 7-Gate | ✅ | 全Gate実装 |
| PositionTracker | ✅ | Actor + DashMap caches |
| TimeStop/Flattener | ✅ | reduce_only生成 |
| MaxPosition Gates | ✅ | Decimal比較 + pending含む |
| HardStopLatch | ✅ | 永続停止 |
| WsSender trait | ✅ | 抽象定義完了 |
| v値変換 (27/28) | ✅ | 修正済み |
| 実WsSender | 🟡 | hip3-ws統合待ち |
| HardStop全cancel | 🟡 | oid追跡待ち |

---

## 次のステップ

1. **Testnet 接続**: hip3-ws に `impl WsSender` を追加
2. **応答処理結線**: orderUpdate/userFills → PositionTracker
3. **E2E テスト**: Testnet で 10 トレード成功
4. **HardStop 完全実装**: oid 追跡 + 全 cancel/flatten
