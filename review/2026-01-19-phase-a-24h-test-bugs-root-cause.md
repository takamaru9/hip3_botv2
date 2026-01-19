# Phase A 24h Test Bug 原因調査

対象: `bug/` 配下の 24h テスト報告（`logs/24h_test_20260119_012218.log` を含む）  
目的: 発見されたバグの「原因」をコード・ログから特定する

---

## BUG-001: ParquetWriter - データがディスクに書き込まれない（0 bytes）

### 観測事実（ログ/ファイル）
- ログ上は flush が成功している:
  - `hip3_persistence::writer` が `Creating new Parquet writer` → `Flushing signals to Parquet` → `Parquet write complete`
  - 例: `logs/24h_test_20260119_012218.log:2728` 付近
- 実ファイルが 0 bytes のまま:
  - `data/mainnet/signals/signals_2026-01-18.parquet` が 0 bytes（mtime も作成時刻から動かない）

### 原因（コード）
1) `ArrowWriter::write()` は **row group をメモリにバッファ**し、row group が満杯になるか `flush()` されるまで **ファイルへ書かれない**
   - Parquet crate の実装（`parquet-53.4.1`）で `DEFAULT_MAX_ROW_GROUP_SIZE = 1024*1024` 行
   - `ArrowWriter::write()` は `buffered_rows >= max_row_group_size` になった時だけ内部で `flush()` を呼ぶ
2) 現在の `ParquetWriter::flush()` は `active.writer.write(&batch)?;` までで **`active.writer.flush()` を呼んでいない**
   - `crates/hip3-persistence/src/writer.rs:134`〜（`ActiveWriter` 常駐方式）

つまり、Phase A の 100件バッファ flush を何回繰り返しても、**row group が 1,048,576 行に達するまではディスクに1バイトも出ない**ため、ファイルが 0 bytes のままになります。

### 追加の悪化要因
- `Cargo.toml` の `[profile.release] panic = "abort"` のため、panic 時は unwind せず Drop が走らない → writer が finalize されず、0 bytes のまま終了し得る
- 24h テストログ末尾に shutdown ログが見えず（`tail` では gate spam の途中で終わる）、プロセスが非正常終了/強制終了している可能性が高い

---

## BUG-002: oracle_age が「価格変化」基準のため、更新が来ていても stale 判定される

### 観測事実（ログ）
- `oracle_fresh` でブロックされる（例: `logs/24h_test_20260119_012218.log:6205` 付近）
  - `"Oracle stale: 8041ms > 8000ms max"` など
- 市場閉鎖/価格安定時に `oracle_age_ms` が数分〜十数分（ログでは 100秒〜700秒台）まで増加

### 原因（コード）
- `MarketStateEntry::update_ctx()` が `oracle_changed_at` を **oraclePx が変化した時だけ**更新している
  - `crates/hip3-feed/src/market_state.rs:76`〜
- `MarketStateEntry::oracle_age_ms()` は `oracle_changed_at` からの経過時間を返す（= 最終価格変化からの時間）
  - `crates/hip3-feed/src/market_state.rs:110`
- `RiskGate::check_oracle_fresh()` はその `oracle_age_ms` を「oracle stale」として扱い、閾値超えでブロックする
  - `crates/hip3-risk/src/gates.rs:276`

結果として、
- **oracle 更新は受信している（ctx は届く）**
- しかし **値が変わっていない**（市場閉鎖や安定相場）
のケースで `oracle_fresh` が誤ってブロックになります。

### 構造的な問題（重複/意図ズレ）
- `ctx_update` gate（`crates/hip3-risk/src/gates.rs:183`〜）が既に「更新受信ベースの鮮度」を見ている
- `oracle_fresh` gate が「価格変化ベースの鮮度」になっており、Phase A の “観測” を止める理由として強すぎる

---

## 参考: この2件によりログ量が爆増している
- `oracle_fresh` ブロックが大量発生し、`warn!` がマーケット×ループ頻度で出るためログが急増（`logs/24h_test_20260119_012218.log` が短時間で 400MB超）
- 原因そのものではないが、24h 継続テストではディスク圧迫要因になる

---

## 確認用の対処方針（原因検証を最短で通す）
- BUG-001:
  - `crates/hip3-persistence/src/writer.rs` の `active.writer.write(&batch)?;` の直後に `active.writer.flush()?;` を呼ぶ、または `WriterProperties::set_max_row_group_size(...)` を小さくして自動 flush が走るようにする
  - 期待結果: プロセス稼働中でも Parquet ファイルが 0 bytes のままにならない
- BUG-002:
  - `oracle_fresh` の入力を「価格変化」ではなく「更新受信」（`ctx_age_ms`）に合わせる（`oracle_fresh` gate を削除 or `ctx_update` に統合）
  - 期待結果: 市場閉鎖中でも `oracle_fresh` ブロックが発生しない（`ctx_update` ブロックのみ残る）
