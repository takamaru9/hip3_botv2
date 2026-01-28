# 2026-01-22 Mainnet Micro Test: Safety Fixes Review

対象: mainnet 少額テスト（`config/mainnet-test.toml`）に向けた安全性レビュー／修正の記録。

## 結論
- **ブロッカー級バグを1件修正**（PositionTracker の pending 二重計上 → 市場が永続 pending 化し得る）。
- **少額上限のすり抜けを防止**（Executor の notional gate を `detector.max_notional` に追従）。

## 重要修正（ブロッカー）

### 1) PositionTracker の pending 二重計上（TrySendError::Full 経路）

**現象**
- `Executor::try_register_order()` が `PositionTrackerHandle::try_register_order()` を呼ぶ。
- `try_register_order()` は **キャッシュ（特に `pending_markets_cache`）を先に更新**した後、`try_send()` で actor に投げる。
- channel が詰まって `TrySendError::Full` になると、Executor 側が slow-path で `register_order()` を呼ぶ。
- `register_order()` も **同じキャッシュ更新を再度実行**するため、`pending_markets_cache` が +2 される。
- その後 `remove_order()` / terminal `order_update()` でキャッシュ減算は 1 回しか発生しないため、**`pending_markets_cache` が残留**して市場が永続的に “pending 扱い” になり得る。

**影響**
- Gate 6 相当（PendingOrderExists）で **以後その market への発注が止まる**（再起動まで復帰しない可能性）。
- `pending_markets_cache` 残留により「未決済扱い」が永続化する（notional 計算自体は主に `pending_orders_data` を参照）。

**修正**
- slow-path 用に「キャッシュ更新なしで actor にだけ登録する」API を追加:
  - `PositionTrackerHandle::register_order_actor_only()`
- Executor 側の slow-path を上記 API に切替:
  - `Executor::try_register_order()` → `register_order_actor_only()`
- 追加ガード:
  - slow-path 実行時点で既にキャッシュから消えている cloid は **actor を復活更新しない**（stale state の復元防止）。

**該当ファイル**
- `crates/hip3-position/src/tracker.rs`
- `crates/hip3-executor/src/executor.rs`

**テスト**
- `crates/hip3-position/src/tracker.rs` に再現テストを追加:
  - `test_register_order_actor_only_does_not_double_count_caches`

## mainnet 少額テスト向けの安全性強化

### 2) notional 上限（$20）の executor 側ゲート反映

**背景**
- `config/mainnet-test.toml` の `$20` 制限は `detector.max_notional` に設定されているが、
  Executor の `ExecutorConfig` はデフォルト値（per-market 50 / total 100）だと上位で緩くなる。

**修正**
- Trading mode 初期化時に `ExecutorConfig` を `detector.max_notional` へ追従させ、少額上限を executor gate にも適用。
  - `max_notional_per_market = detector.max_notional`
  - `max_notional_total = detector.max_notional`

**該当ファイル**
- `crates/hip3-bot/src/app.rs`

### 3) config コメントの明確化
- `config/mainnet-test.toml` に「秘密鍵は `HIP3_TRADING_KEY` env から読む（raw key を置かない）」旨を追記。

## 残課題（運用でのカバーが必要）
- HardStop は latch はあるが **自動 flatten / RiskMonitor との結線が未完**（現状は手動 flatten 前提）。
- Nonce は server 時刻同期（`NonceManager::sync_with_server`）が未使用のため、VPS 時刻同期（chrony/ntpd）と reject 監視が必須。

## 実行したテスト
- `cargo test -p hip3-position -p hip3-executor`
- `cargo test -p hip3-bot`
