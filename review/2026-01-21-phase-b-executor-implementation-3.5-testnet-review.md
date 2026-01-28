# Phase B 実装計画レビュー（3.5 Testnet検証）

対象: `.claude/plans/2026-01-19-phase-b-executor-implementation.md`（3.5 Week 3: Testnet検証）  
確認日: 2026-01-21

## 結論
**未承諾**。3.5 はチェックリストとしては良いですが、実行手順（Runbook）と合否基準が不足しており、実施フェーズで「何をどう確認するか」「失敗時にどう止めるか」が曖昧で詰まります。次の3点を追記してください。

## 指摘（今回の指摘: 3点）

### 1) 「Testnet接続設定」が曖昧で、誤って Mainnet 設定/鍵で起動し得る
現状だと「どの config を使うか」「trading mode に切り替えるか」「署名に使う trading key の供給元（EnvVar/File）」「Testnet URL の確認」等が書かれていません。

→ 3.5 に **起動Runbook** を固定してください（例: 以下を明記）。
- `HIP3_CONFIG=config/testnet.toml` で起動（`ws_url`/`info_url` が Testnet であることをログで確認）
- `mode = "trading"` に切り替える手順（Phase B 実取引の検証なので observation のままだと 3.5 が実施不能）
- trading key の供給方法（`KeySource::EnvVar { var_name }` / `KeySource::File { path }` のどちらを使うか、実際の環境変数名 or ファイルパス）
- **安全装置**（Testnetでのみ有効にするガード、max_notional、対象 market を BTC/ETH/SOL に限定、停止条件）

### 2) 各検証項目が「具体的な手順」と「観測点（ログ/メトリクス）」に落ちていない
表は「何を確認したいか」レベルで、現場で再現できる手順になっていません（例: nonce 衝突/レート制限/切断時の挙動は、負荷のかけ方と確認方法が必要）。

→ 3.5 の #1〜#10 を、それぞれ **(a)手順 (b)期待ログ/メトリクス (c)合否** に分解して追記してください（最低限の例）:
- #2 署名検証: 1回の new_order を出して `post` 応答が `Ok`、`orderUpdates` が追従すること（失敗時は reason を記録）
- #8 nonce: 連続で N 回 tick を回し、`nonce` 単調増加・重複なし（ログで確認）
- #9 レート制限: inflight 上限付近まで負荷をかけ、`on_batch_sent/complete` がドリフトしない（メトリクス/ログ）
- #7 TimeStop: 短い閾値で発火させ、reduce_only が優先キューで必達（失敗なら CRITICAL）

### 3) #10「reject時のリトライ動作」が、3.4 の方針と矛盾しやすい
3.4 では `Rejected` は terminal 扱いで **再キューしない**（cleanup して終了）方針です。一方で 3.5 #10 は「reject時のリトライ」を成功基準にしており、どちらが正か曖昧です。

→ 3.5 #10 を **3.4 と整合する成功基準**に修正してください（例）:
- `Rejected` は再キューしない（ただし pending state は必ず cleanup される）
- リトライ対象は `reduce_only/cancel` の「送信失敗/タイムアウト/切断」のみ（再キューして成功すること）

---

## 再レビュー（2026-01-21, 修正後）
前回の3点（起動Runbook追加、#1〜#10 の手順/観測点/合否の詳細化、#10 の 3.4 整合）は反映されています。  
ただしまだ **未承諾** で、次の2点を直してください（今回の指摘: 2点）。

## 指摘（今回の指摘: 2点）

### 1) Runbook が「存在しないコマンド/フラグ」に依存していて、手順として実行不能になり得る
Runbook 内に `cargo run --bin hip3-cli -- check-key` と `--dry-run` が出てきますが、計画内で **その CLI/フラグの実装がタスク化されていません**（現状の repo にも存在しません）。

→ どちらかに断定して、3.5 の本文とタスクに反映してください。
- **A. 実装する**: `hip3-cli check-key`（or `hip3-bot check-key` サブコマンド）と `--dry-run` の仕様/期待出力/実装タスクを追加
- **B. 手順を置き換える**: 起動時ログで `Signer address` を確認する方式に統一し、`hip3-cli` と `--dry-run` 記述を削除

### 2) #9 レート制限・inflight の合否基準が不正確（常時 `sent==complete` は成立しない）
`batch_sent_total == batch_complete_total` は、常に inflight が 0 の瞬間しか成立しません。正しい不変条件は 3.4 のテスト項目と同じく **`batch_sent_total - batch_complete_total == inflight_current`** です。

→ #9 の合否を次のように修正してください。
- 運転中: `batch_sent_total - batch_complete_total == inflight_current` が常に成立（ドリフトなし）
- テスト終了時（キュー枯渇後）: `inflight_current == 0` かつ `batch_sent_total == batch_complete_total`

---

## 再レビュー（2026-01-21, 修正後）
前回の2点（未実装コマンド/フラグの削除、#9 合否基準の修正）は反映されています。  
**3.5 Testnet検証は承諾**します（この内容で実装/検証に進めます）。
