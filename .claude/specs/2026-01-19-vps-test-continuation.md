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

## 次のステップ

1. [ ] 24時間経過を待つ
2. [ ] 米国市場時間帯のシグナルデータを分析
3. [ ] Phase A DoD最終判定
4. [ ] Phase B準備開始（条件達成の場合）
