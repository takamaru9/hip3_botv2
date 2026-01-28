# HIP-3 Bot 実装状況レビュー

## メタデータ

| 項目 | 値 |
|------|-----|
| レビュー日 | 2026-01-25 |
| 対象 | hip3_botv2 全体 |
| ステータス | **本番運用非推奨（自動決済未実装）** |

---

## エグゼクティブサマリー

HIP-3 Botは**エントリー（注文送信）機能は実装済み**だが、**自動イグジット（ポジション決済）機能が未統合**。

現状は「ポジションを取ることはできるが、自動で決済できない」状態であり、**手動監視なしでの本番運用は危険**。

---

## 実装状況

### ✅ 実装済み（動作可能）

| 機能 | ファイル | 説明 |
|------|----------|------|
| WebSocket接続 | `hip3-ws/` | 再接続、heartbeat、rate limit |
| MarketState管理 | `hip3-bot/src/app.rs` | BBO/AssetCtx更新 |
| RiskGate (8ゲート) | `hip3-risk/src/gates.rs` | 事前チェック（MarkMidDivergence等）|
| Detector | `hip3-detector/` | シグナル検出（oracle vs bid/ask）|
| Executor | `hip3-executor/` | 注文送信、BatchScheduler |
| PositionTracker | `hip3-position/src/tracker.rs` | ポジション追跡（Actor model）|
| Fill処理 | `hip3-bot/src/app.rs:933` | 約定 → ポジション更新 |
| HardStopLatch | `hip3-risk/src/hard_stop.rs` | ラッチ機構（発火→リセットまで持続）|
| ActionBudget | `hip3-executor/` | 注文数制限 |
| Signer | `hip3-executor/src/signer.rs` | EIP-712署名 |
| NonceManager | `hip3-executor/src/nonce.rs` | Nonce管理 |

### ❌ 未実装/未統合

| 機能 | ファイル | 状態 | 影響 |
|------|----------|------|------|
| **TimeStopMonitor** | `hip3-position/src/time_stop.rs` | コード存在、**未統合** | 30秒経過後の自動決済なし |
| **Flattener統合** | `hip3-position/src/flatten.rs` | コード存在、**未統合** | 決済状態管理なし |
| **HardStop flatten** | - | **未実装** | 緊急停止時の自動決済なし |
| **RiskMonitor統合** | `hip3-risk/src/hard_stop.rs` | コード存在、**未統合** | 累積損失/連続失敗監視なし |
| Spec定期更新 | `hip3-bot/src/app.rs:610` | TODO記載 | 起動時のみ取得 |

---

## アーキテクチャ

### 現在の動作フロー

```
WebSocket
    ↓ メッセージ受信
MarketState更新
    ↓
RiskGate チェック (8ゲート)
    ↓ Pass
Detector (シグナル検出)
    ↓ シグナル発火
Executor → 注文送信
    ↓
Fill受信 → PositionTracker更新
    ↓
【ここで停止 - 自動決済なし】
```

### 本来あるべきフロー（未実装部分）

```
Fill受信 → PositionTracker更新
    ↓
TimeStopMonitor (毎秒チェック)
    ├─→ 30秒超過? → Flattener → reduce-only注文
    └─→ 正常 → 継続監視

RiskMonitor (イベント監視)
    ├─→ 累積損失 > $1000? → HardStop発火
    ├─→ 連続失敗 >= 5? → HardStop発火
    └─→ 正常 → 継続

HardStop発火時
    → flatten_all_positions() → 全ポジション即時決済
```

---

## 設定値（デフォルト）

### 実装済みだが未使用の設定

| 設定 | デフォルト値 | 定義場所 |
|------|-------------|----------|
| TIME_STOP_MS | 30,000 ms (30秒) | `time_stop.rs:22` |
| REDUCE_ONLY_TIMEOUT_MS | 60,000 ms (60秒) | `time_stop.rs:27` |
| max_consecutive_failures | 5 | `hard_stop.rs:191` |
| max_loss_usd | $1,000 | `hard_stop.rs:193` |

### config/mainnet-test.toml の警告

```toml
# WARNING: HardStop flatten not implemented - manual flatten required
```

---

## リスク評価

### 高リスク

| リスク | 説明 | 影響度 |
|--------|------|--------|
| ポジション放置 | 自動決済なしのため、価格逆行時に損失拡大 | **致命的** |
| 手動介入依存 | 問題発生時に人間が決済操作する必要あり | **高** |
| 24/7運用不可 | 監視なしでの長時間運用は危険 | **高** |

### 中リスク

| リスク | 説明 | 影響度 |
|--------|------|--------|
| 連続失敗検知なし | API障害時に注文失敗が累積しても停止しない | **中** |
| 累積損失監視なし | 損失が閾値を超えても自動停止しない | **中** |

---

## TODOリスト（コード内）

```
crates/hip3-bot/src/app.rs:610
  // P1-3: Phase B TODO - Add periodic spec refresh task

crates/hip3-executor/src/signer.rs:107
  observation_address: Address::ZERO, // TODO: Set separately if needed
```

---

## 本番運用に必要な追加実装

### 優先度: 高（必須）

1. **TimeStopMonitor の統合**
   - `app.rs` の初期化で `TimeStopMonitor` を起動
   - flatten注文をExecutorへ送信するチャネル接続
   - 30秒経過したポジションを自動決済

2. **HardStop flatten処理**
   - HardStopLatch発火時に `flatten_all_positions()` を呼び出し
   - 全ポジションに対してreduce-only注文を送信

3. **RiskMonitor の統合**
   - ExecutionEvent を RiskMonitor に送信
   - 累積損失 / 連続失敗でHardStop発火

### 優先度: 中

4. **Flattener の統合**
   - 決済注文の状態管理（InProgress/Completed/Failed）
   - 60秒タイムアウト検出とリトライ

5. **Spec定期更新**
   - 起動後も定期的にmeta APIを取得
   - szDecimals等の変更を反映

---

## テスト時の注意事項

### 現状でテスト可能な範囲

- ✅ シグナル検出ロジック
- ✅ 注文送信・約定確認
- ✅ ポジション追跡

### 現状でテスト不可能な範囲

- ❌ 自動決済（手動で決済が必要）
- ❌ 緊急停止後の自動決済
- ❌ 損失リミットによる自動停止

---

## 結論

| 項目 | 状態 |
|------|------|
| エントリー（注文） | ✅ 可能 |
| ポジション追跡 | ✅ 可能 |
| **自動決済** | ❌ **未実装** |
| **緊急停止時決済** | ❌ **未実装** |
| **本番運用** | ⚠️ **手動監視必須** |

**現状は「エントリーのみ可能、イグジットは手動」という状態。**

本番運用には TimeStopMonitor / HardStop flatten / RiskMonitor の統合が必須。

---

## 参照ファイル

| ファイル | 内容 |
|----------|------|
| `crates/hip3-position/src/time_stop.rs` | TimeStop/TimeStopMonitor実装 |
| `crates/hip3-position/src/flatten.rs` | Flattener/FlattenRequest実装 |
| `crates/hip3-position/src/tracker.rs` | PositionTracker実装 |
| `crates/hip3-risk/src/hard_stop.rs` | HardStopLatch/RiskMonitor実装 |
| `crates/hip3-risk/src/gates.rs` | RiskGate (8ゲート) 実装 |
| `crates/hip3-bot/src/app.rs` | メインアプリ（統合部分）|
| `config/mainnet-test.toml` | 設定ファイル（WARNING記載）|
