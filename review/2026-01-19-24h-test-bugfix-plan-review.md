# レビュー: 24時間テストBug修正計画（ethereal-sauteeing-galaxy.md）

対象計画: `/Users/taka/.claude/plans/ethereal-sauteeing-galaxy.md`  
関連: `review/2026-01-19-phase-a-24h-test-bugs-root-cause.md`

## 総評
- BUG-001/BUG-002 ともに「症状に対する最短の手当て」としては妥当です。
- ただし BUG-001 は **「なぜ 0 bytes のままか」** の原因認識が一部不足しており、現状の修正案だと **SIGTERM/abort 等の“非グレースフル終了”で再発**しやすいです（24h運用ではここが本丸）。

## BUG-001（Parquet 0 bytes）の指摘

### 原因認識の補足（重要）
- 計画では「`write()`はメモリ、`close()`でfooter」まで書かれていますが、今回の **0 bytes** の直接原因は、
  - `ArrowWriter` の **row group が flush されていない**（= `ArrowWriter::flush()` 相当が呼ばれていない）
  - かつ Parquet の `max_row_group_size` のデフォルトが **1,048,576 行**で、100行バッチを何回 `write()` しても自動flushが走らない
 という点です（詳細は `review/2026-01-19-phase-a-24h-test-bugs-root-cause.md`）。

### 修正案（計画案）への評価
- `hip3-bot/app.rs` の終了処理を `self.writer.close()?;` にするのは **グレースフル終了（SIGINT/ctrl-c）** に対して有効。
- ただし計画の統合テスト手順が `kill $PID`（SIGTERM）になっており、現状のアプリは `ctrl_c()`（SIGINT）しか待っていないため、**closeが呼ばれず 0 bytes のまま終了**し得ます。

### 追加で入れたい対策（推奨）
- `crates/hip3-persistence/src/writer.rs` の `active.writer.write(&batch)?;` の直後に **`active.writer.flush()?;`** を呼ぶ（row group をディスクへ吐き出す）。
  - これで「プロセス稼働中でもファイルサイズが増える」状態になり、SIGTERM/abort 時の損失が“0 bytes全損”からは改善します（※footer未完なので parquet-tools で読めるのは close 後）。
- さらに堅くするなら、テスト/運用で SIGTERM を使うなら **SIGTERMハンドリング**（graceful shutdown）も検討（macなら `tokio::signal::unix::signal`）。
- `close_active_writer()` を `pub` にするより、外部向けは `close()` のみ公開し、内部finalizeは隠蔽した方が安全です（誤用防止）。

### テスト手順の修正（必須）
- `kill $PID` を使うなら `kill -INT $PID` に変更（= ctrl-c 相当）し、close 経路を通す。
- parquet-tools での読み取り確認は **close 後**（= プロセス終了後）を前提にする（close 前は footer が無いので読めないことがある）。

## BUG-002（oracle_age が価格変化ベース）の指摘

### 修正案への評価
- `oracle_fresh` の入力を `ctx_age_ms` に置き換える方針は、今回の症状（市場閉鎖/価格固定で stale）に対して効果があります。

### ただし注意点
- 根本原因は `MarketStateEntry::oracle_age_ms()` が「価格変化」指標になっている点と、`RiskGate::check_oracle_fresh()` がそれを「更新鮮度」として使っている点です。
- `app.rs` 側で `oracle_age_ms = ctx_age_ms` としてしまうと、ログ上の `oracle_fresh` が実態は `ctx_age` になり、将来のデバッグで混乱しやすいです。
  - 可能なら次段で「oracle_fresh gate を `ctx_update` に統合/削除」または「gate名/ログ文言を `ctx_fresh` に寄せる」ことを推奨します。

## 成功基準の補足
- 「Parquetファイルのサイズが 0 より大きい」は、`ArrowWriter::flush()` を入れない限り **close 後にしか満たせない**可能性が高いです。
- 「parquet-tools で読み取り可能」も **close 後が前提**です（close 前は footer が無い）。

