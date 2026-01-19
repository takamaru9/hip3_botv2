# Phase A 観測データ分析レポート

**分析日**: 2026-01-19
**データ期間**: 2026-01-18T17:35 〜 2026-01-19T08:40 UTC（約15時間）
**総シグナル数**: 178,637

---

## 1. テスト実行サマリー

| 項目 | 値 |
|------|-----|
| 開始時刻 | 2026-01-18T17:35:09 UTC |
| 終了時刻 | 2026-01-19T08:40:05 UTC |
| 実行時間 | 約15時間5分 |
| 総シグナル数 | 178,637 |
| WS再接続 | 1回（HeartbeatTimeout → 自律復旧） |
| 監視市場数 | 32 |

---

## 2. 市場別シグナル分析

### 2.1 EV正の市場ランキング（Phase B候補）

基準: mean edge_bps > 30 bps = 高EV（HIP-3手数料2x + スリッページ込みでEV正）

| Rank | Market ID | Symbol | シグナル数 | Mean Edge (bps) | Median (bps) | P95 (bps) | Max (bps) | 判定 |
|------|-----------|--------|-----------|-----------------|--------------|-----------|-----------|------|
| 1 | xyz:12 | **HOOD** | 5,344 | 144.67 | 137.27 | 184.37 | 189.74 | 🟢 高EV |
| 2 | xyz:18 | **MSTR** | 18,068 | 48.06 | 44.40 | 77.52 | 81.58 | 🟢 高EV |
| 3 | xyz:8 | **CRCL** | 79 | 42.14 | 42.14 | 42.14 | 42.14 | 🟢 高EV |
| 4 | xyz:22 | **NVDA** | 7,676 | 38.80 | 44.11 | 47.38 | 49.01 | 🟢 高EV |
| 5 | xyz:5 | **COIN** | 22,828 | 33.04 | 32.89 | 34.98 | 99.64 | 🟢 高EV |
| 6 | xyz:28 | **SNDK** | 441 | 30.15 | 23.14 | 68.29 | 68.29 | 🟢 高EV |

### 2.2 中EV市場（要監視）

| Rank | Market ID | Symbol | シグナル数 | Mean Edge (bps) |
|------|-----------|--------|-----------|-----------------|
| 7 | xyz:16 | META | 1,834 | 23.23 |
| 8 | xyz:25 | RIVN | 7,054 | 22.51 |
| 9 | xyz:6 | COPPER | 65,572 | 21.41 |
| 10 | xyz:4 | CL | 923 | 19.76 |
| 11 | xyz:19 | MU | 1,578 | 18.39 |
| 12 | xyz:26 | SILVER | 35,240 | 16.02 |

### 2.3 低EV市場（観察継続）

| Market ID | Symbol | シグナル数 | Mean Edge (bps) |
|-----------|--------|-----------|-----------------|
| xyz:29 | TSLA | 3,281 | 14.23 |
| xyz:13 | INTC | 3,183 | 13.22 |
| xyz:24 | PLTR | 3,844 | 12.33 |
| xyz:2 | AMZN | 1,692 | 11.50 |

---

## 3. Phase B 候補市場の詳細分析

### 3.1 HOOD (Robinhood) - xyz:12

- **Mean Edge**: 144.67 bps（最高）
- **シグナル数**: 5,344
- **特徴**: Oracle乖離が非常に大きい、高ボラティリティ銘柄
- **推奨**: Phase B優先候補

### 3.2 MSTR (MicroStrategy) - xyz:18

- **Mean Edge**: 48.06 bps
- **シグナル数**: 18,068（2番目に多い）
- **特徴**: BTC連動性が高く、Oracle更新頻度にギャップ
- **推奨**: Phase B候補

### 3.3 NVDA (NVIDIA) - xyz:22

- **Mean Edge**: 38.80 bps
- **シグナル数**: 7,676
- **特徴**: 高流動性だがOracle乖離発生頻度高い
- **推奨**: Phase B候補（流動性面で有利）

### 3.4 COIN (Coinbase) - xyz:5

- **Mean Edge**: 33.04 bps
- **シグナル数**: 22,828（最多）
- **特徴**: 最多シグナル、安定したEV
- **推奨**: Phase B候補（サンプル数で信頼性高い）

---

## 4. Phase B移行判定

### 4.1 達成状況

| 条件 | 状態 | 詳細 |
|------|------|------|
| Phase A DoD | 🟡 部分達成 | 15h稼働（24hには未達）、WS自律復旧は確認 |
| EV正の市場2-3個特定 | ✅ 達成 | 6市場で高EV確認（HOOD, MSTR, NVDA, COIN, CRCL, SNDK） |
| Risk Gate停止品質 | ✅ 安定 | エラー1件（HeartbeatTimeout）、自律復旧 |
| ctx/bbo受信間隔分布 | ✅ 把握済み | 正常範囲で推移 |

### 4.2 推奨

**Phase B準備開始可能**

理由:
1. 6市場で明確なEV正の兆候
2. WS自律復旧が機能
3. Risk Gateが正常動作

注意点:
- 24h連続稼働テストは別途完了させることを推奨
- HOOD (144 bps) は特異値の可能性、実弾での検証が必要

---

## 5. Phase B 推奨実施順序

| 順位 | Market | Symbol | 理由 |
|------|--------|--------|------|
| 1 | xyz:5 | COIN | 最多シグナル、33 bps、信頼性高い |
| 2 | xyz:22 | NVDA | 高流動性、38 bps |
| 3 | xyz:18 | MSTR | 48 bps、シグナル数多い |
| 4 | xyz:12 | HOOD | 144 bps、ただしサンプル少なめ |

---

## 6. Coin Mapping 参照

| Asset ID | Symbol | 銘柄名 |
|----------|--------|--------|
| 0 | AAPL | Apple |
| 1 | AMD | AMD |
| 2 | AMZN | Amazon |
| 3 | BABA | Alibaba |
| 4 | CL | Crude Oil |
| 5 | COIN | Coinbase |
| 6 | COPPER | Copper |
| 7 | COST | Costco |
| 8 | CRCL | Circle |
| 9 | EUR | Euro |
| 10 | GOLD | Gold |
| 11 | GOOGL | Google |
| 12 | HOOD | Robinhood |
| 13 | INTC | Intel |
| 14 | JPY | Japanese Yen |
| 15 | LLY | Eli Lilly |
| 16 | META | Meta |
| 17 | MSFT | Microsoft |
| 18 | MSTR | MicroStrategy |
| 19 | MU | Micron |
| 20 | NATGAS | Natural Gas |
| 21 | NFLX | Netflix |
| 22 | NVDA | NVIDIA |
| 23 | ORCL | Oracle |
| 24 | PLTR | Palantir |
| 25 | RIVN | Rivian |
| 26 | SILVER | Silver |
| 27 | SKHX | Skyhex |
| 28 | SNDK | SanDisk |
| 29 | TSLA | Tesla |
| 30 | TSM | TSMC |
| 31 | XYZ100 | XYZ100 Index |

---

## 7. 次のステップ

1. **Phase B準備**: hip3-executor実装開始（NonceManager、Batching、IOC発注）
2. **鍵管理**: API wallet分離（P0-11）
3. **24hテスト再実施**: DoDを厳密に満たすため
4. **Phase B初期市場**: COIN (xyz:5) から開始推奨
