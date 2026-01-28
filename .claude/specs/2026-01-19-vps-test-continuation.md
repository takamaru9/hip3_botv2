# Phase A 24時間テスト VPS継続実行 Spec

## Metadata

| Item | Value |
|------|-------|
| Date | 2026-01-19 |
| Status | `[IN_PROGRESS]` |
| Source | Phase A分析レポート (`.claude/specs/2026-01-19-phase-a-analysis.md`) |

---

## 背景

Phase Aの24時間テストを約15時間実施したが、米国市場が開いている時間帯（UTC 14:30-21:00）のデータが不足。VPSでテストを継続して24時間以上のデータを収集する。

### 既存データ

| 項目 | 値 |
|------|-----|
| 既存データ期間 | 2026-01-18T17:35 〜 2026-01-19T08:40 UTC |
| 実行時間 | 約15時間 |
| 総シグナル数 | 178,637 |
| カバー済み時間帯 | アジア・欧州時間帯 |
| 不足時間帯 | **米国市場時間帯 (UTC 14:30-21:00)** |

---

## 実施内容

### 1. GitHubリポジトリ作成

| 項目 | 値 |
|------|-----|
| リポジトリURL | https://github.com/takamaru9/hip3_botv2 |
| 可視性 | Public（VPSからcloneするため） |
| 初回コミット | `caee2fc` |

### 2. VPSデプロイ

| 項目 | 値 |
|------|-----|
| VPS IP | 5.104.81.76 |
| プロバイダ | Contabo |
| OS | Ubuntu 22.04.5 LTS |
| デプロイ先 | `/opt/hip3-bot` |
| デプロイ方式 | GitHub clone + Docker Compose |

### 3. コード更新手順

```bash
# 1. 既存データをバックアップ
cp -r /opt/hip3-bot/data /tmp/hip3-data-backup

# 2. GitHubから最新コードをclone
rm -rf /opt/hip3-bot
cd /opt
git clone https://github.com/takamaru9/hip3_botv2.git hip3-bot

# 3. データを復元
mkdir -p /opt/hip3-bot/data
cp -r /tmp/hip3-data-backup/* /opt/hip3-bot/data/

# 4. Docker Compose build & start
cd /opt/hip3-bot
docker compose build
docker compose up -d
```

### 4. テスト開始確認

| 項目 | 値 |
|------|-----|
| 開始時刻 | 2026-01-19T09:01:39 UTC |
| 監視市場数 | 32 |
| WebSocket接続 | ✅ 成功 |
| シグナル検出 | ✅ 動作中 |

初期シグナル確認：
```
Signal detected (#16), market: xyz:2, side: buy, edge_bps: 17.29
```

---

## テストスケジュール

| 時刻 (UTC) | 時刻 (JST) | イベント |
|-----------|-----------|----------|
| 09:01 | 18:01 | テスト開始 |
| 14:30 | 23:30 | 米国市場開場 |
| 21:00 | 06:00 (翌日) | 米国市場閉場 |
| 09:01 (翌日) | 18:01 (翌日) | 24h経過 |

---

## 確認コマンド

### ログ確認
```bash
sshpass -p 'RD3lDP8x8Xa2vQ3pVWwZ9dAr0' ssh root@5.104.81.76 \
  "docker compose -f /opt/hip3-bot/docker-compose.yml logs --tail 50"
```

### シグナル数確認
```bash
sshpass -p 'RD3lDP8x8Xa2vQ3pVWwZ9dAr0' ssh root@5.104.81.76 \
  "docker compose -f /opt/hip3-bot/docker-compose.yml logs 2>/dev/null | grep -c 'Signal detected'"
```

### コンテナ状態
```bash
sshpass -p 'RD3lDP8x8Xa2vQ3pVWwZ9dAr0' ssh root@5.104.81.76 \
  "docker compose -f /opt/hip3-bot/docker-compose.yml ps"
```

### エラー確認
```bash
sshpass -p 'RD3lDP8x8Xa2vQ3pVWwZ9dAr0' ssh root@5.104.81.76 \
  "docker compose -f /opt/hip3-bot/docker-compose.yml logs 2>/dev/null | grep -E 'ERROR|panic|FATAL'"
```

---

## データ取得後の分析計画

1. **24h経過後**: VPSからParquetファイルをダウンロード
2. **分析対象**:
   - 米国市場時間帯のシグナル分布
   - 時間帯別EV分析
   - 市場別パフォーマンス比較（全時間帯）
3. **Phase B判定**: 24h DoD達成確認

---

## ファイル構成

```
/opt/hip3-bot/
├── docker-compose.yml    # restart: unless-stopped
├── config/
│   └── mainnet.toml      # 32市場設定
├── data/
│   └── mainnet/
│       └── signals/      # Parquetシグナルファイル
└── crates/               # Rustソースコード
```

---

## 注意事項

- Docker Composeは `restart: unless-stopped` 設定で自動再起動
- データは `/opt/hip3-bot/data/mainnet/signals/` に永続化
- VPS再起動時もコンテナは自動復帰

---

## 問題発生と修正 (2026-01-20)

### 発生した問題

| 項目 | 詳細 |
|------|-----|
| 発生日時 | 2026-01-20 |
| 症状 | VPS上のParquetファイルが破損・読み込み不可 |
| エラー | `Parquet magic bytes not found in footer` |
| 収集データ | 76,456シグナル（すべて破損） |

### 根本原因

1. **Permission denied エラー**
   - VPS上の `/opt/hip3-bot/data` がroot所有でコンテナ（UID 1000）から書き込み不可
   - `flush()` が失敗し、`ArrowWriter::close()` が呼ばれない

2. **Parquet形式の特性**
   - Parquetはファイル末尾にfooter（メタデータ）を書き込む
   - `close()` が呼ばれないとfooterが書き込まれない
   - footerがないとファイル全体が読み込み不可

### 修正内容

| 項目 | 変更前 | 変更後 |
|------|--------|--------|
| ファイル形式 | Parquet (.parquet) | JSON Lines (.jsonl) |
| 書き込み方式 | Arrow バッチ → footer | 行ごとにJSON追記 |
| 障害耐性 | footerがないと全データ喪失 | 各行が独立、部分破損のみ |
| 拡張子 | `.parquet` | `.jsonl` |

#### 変更ファイル

1. `crates/hip3-persistence/src/writer.rs` - 完全書き換え
   - `ArrowWriter` → `BufWriter<File>`
   - Append mode で既存データを保護
   - 各行を独立したJSONオブジェクトとして書き込み

2. `crates/hip3-persistence/src/error.rs`
   - Parquet/Arrowエラー → serde_json エラー

3. `crates/hip3-persistence/Cargo.toml`
   - parquet, arrow 依存を削除
   - serde_json を追加

#### 後方互換性

```rust
// ParquetWriter 型エイリアスで既存コードへの影響を最小化
pub type ParquetWriter = JsonLinesWriter;
```

### 修正後の確認

| 項目 | 結果 |
|------|------|
| GitHub push | ✅ commit `c4652f0` |
| VPS更新 | ✅ `git pull` 成功 |
| コンテナ再起動 | ✅ healthy |
| JSON Lines生成 | ✅ `signals_2026-01-20.jsonl` |
| データ読み込み | ✅ Python/Polars で正常読み込み |

### 検証結果

```bash
# ファイル確認
$ ls -la /opt/hip3-bot/data/mainnet/signals/
signals_2026-01-20.jsonl   # 141KB, 600+ records

# サンプルデータ
{"timestamp_ms":1768893220122,"market_key":"xyz:23","side":"buy",
 "raw_edge_bps":17.15,"net_edge_bps":6.15,"oracle_px":186.55,
 "best_px":186.23,"suggested_size":0.3,"signal_id":"sig_xyz:23_buy_..."}
```

---

## 次のステップ

1. [x] ~~24時間経過を待つ~~ → データ破損のためリセット
2. [x] データ破損原因調査・修正完了
3. [ ] JSON Lines形式で新規24時間テスト実行中
4. [ ] 米国市場時間帯のシグナルデータを分析
5. [ ] Phase A DoD最終判定
6. [ ] Phase B準備開始（条件達成の場合）

---

## テスト再開情報

| 項目 | 値 |
|------|-----|
| 再開日時 | 2026-01-20 07:13 UTC |
| データ形式 | JSON Lines (.jsonl) |
| 予定完了 | 2026-01-21 07:13 UTC |
| 次回米国市場 | 2026-01-20 14:30 UTC (JST 23:30) |

---

## 機能追加: best_size フィールド (2026-01-20 08:09 UTC)

### 背景

シグナル分析時に、提案されたサイズ（`suggested_size`）と実際に利用可能な流動性（オーダーブックのトップレベルサイズ）を比較できるようにする。

### 変更内容

| 項目 | 変更 |
|------|------|
| フィールド追加 | `best_size: f64` を `SignalRecord` に追加 |
| データソース | `DislocationSignal.book_size` から取得 |
| 意味 | ベストプライスで利用可能なサイズ（トップオブブック深度） |

### 変更ファイル

1. `crates/hip3-persistence/src/writer.rs`
   - `SignalRecord` に `best_size` フィールド追加
   - テストヘルパー関数更新

2. `crates/hip3-bot/src/app.rs`
   - `persist_signal()` で `best_size` を設定

### デプロイ

| 項目 | 結果 |
|------|------|
| Commit | `51908f8` |
| GitHub push | ✅ 成功 |
| VPS更新 | ✅ `git pull` + rebuild + restart |
| コンテナ状態 | ✅ healthy |

### 新しいJSONフォーマット

```json
{
  "timestamp_ms": 1768893220122,
  "market_key": "xyz:23",
  "side": "buy",
  "raw_edge_bps": 17.15,
  "net_edge_bps": 6.15,
  "oracle_px": 186.55,
  "best_px": 186.23,
  "best_size": 1.5,      // ← NEW
  "suggested_size": 0.3,
  "signal_id": "sig_xyz:23_buy_..."
}
```

### 分析での活用

- `suggested_size / best_size` 比率で流動性消費率を確認
- 比率 > 1 の場合、スリッページリスクが高い
- 市場別の流動性特性を把握可能

---

## L2データ購読の検討 (2026-01-20)

### 調査結果

| 項目 | 状況 |
|------|------|
| Hyperliquid `l2Book` API | ✅ 利用可能 |
| 現在の購読 | BBO のみ |
| レート制限 | 2000 msg/min (L2追加で消費増) |

### 判断

**Phase Aデータを待つ**
- まず `best_size` 付きの24時間データを収集
- `suggested_size / best_size` 比率を分析
- 流動性が不足している市場が多ければL2購読を検討

### 次回判断タイミング

Phase A 24時間テスト完了後（2026-01-21 07:13 UTC 以降）

---

## 機能追加: Followup Snapshot (2026-01-20)

### 背景

シグナル発生後の収束状況を記録し、シグナルの有効性を検証するためのデータを収集する。

- Oracle は約3秒ごとにデプロイヤーが更新
- Market Price が先行する場合と、Oracle が先行する場合がある
- T+1s, T+3s, T+5s にマーケット状態をキャプチャ

### 変更内容

| 項目 | 詳細 |
|------|------|
| 新規Struct | `FollowupRecord` - フォローアップデータ |
| 新規Class | `FollowupWriter` - JSON Lines書き込み |
| 新規Method | `schedule_followups()` - 3タスクをspawn |
| 新規Function | `capture_followup()` - 遅延キャプチャ |
| オフセット | `[1000, 3000, 5000]` ms |

### 変更ファイル

1. `crates/hip3-persistence/src/writer.rs`
   - `FollowupRecord` struct追加
   - `FollowupWriter` class追加

2. `crates/hip3-persistence/src/lib.rs`
   - 新しい型をエクスポート

3. `crates/hip3-bot/src/app.rs`
   - `FollowupContext` struct
   - `schedule_followups()` method
   - `capture_followup()` async function

### デプロイ

| 項目 | 結果 |
|------|------|
| Tests | ✅ 4 tests passed |
| Clippy | ✅ No warnings |
| VPS Deploy | ✅ Rebuild + restart |
| Followup生成 | ✅ `followups_2026-01-20.jsonl` |

### 検証結果

| Offset | Records | Status |
|--------|---------|--------|
| T+1s | 11,088 | ✓ Working |
| T+3s | 11,040 | ✓ Working |
| T+5s | 10,972 | ✓ Working |

### 銘柄カバレッジ

- 全xyz銘柄: 32
- シグナル/フォローアップあり: 25
- カバー率: 78%

### 出力ファイル

```
data/mainnet/signals/
├── signals_2026-01-20.jsonl       # シグナル
└── followups_2026-01-20.jsonl     # フォローアップ
```

### データクリーンアップ

| 項目 | 詳細 |
|------|------|
| 削除対象 | フォローアップ機能追加前のデータ |
| 削除ファイル数 | 3ファイル (古いsignals) |
| 理由 | フォローアップなしのデータは分析対象外 |

### 詳細Spec

`.claude/specs/2026-01-20-followup-snapshot-feature.md` を参照

---

## 次のステップ（更新）

1. [x] ~~24時間経過を待つ~~ → データ破損のためリセット
2. [x] データ破損原因調査・修正完了
3. [x] JSON Lines形式で新規24時間テスト実行中
4. [x] best_size フィールド追加
5. [x] Followup Snapshot機能追加
6. [ ] 米国市場時間帯のシグナルデータを分析
7. [ ] フォローアップデータでシグナル有効性を検証
8. [ ] Phase A DoD最終判定
9. [ ] Phase B準備開始（条件達成の場合）
