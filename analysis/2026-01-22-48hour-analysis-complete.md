# HIP-3 Signal Validation Analysis Report (Complete)

**Analysis Date:** 2026-01-23
**Data Period:** 2026-01-20 ~ 2026-01-22 (48.4 hours)
**Total Signals:** 4,402,100
**Total Followups:** 12,973,800 (Complete data)

> **Note (2026-01-26):** Ticker names corrected based on `meta(dex=xyz)` API.
> Original analysis used incorrect ticker labels. Market indices and numerical data remain unchanged.

## Executive Summary

48時間の完全なメインネットテストデータを分析した結果、最適閾値を適用することで**22/28市場で利益が出る**ことが確認されました。

### Key Metrics

| Metric | Raw | With Optimal Threshold |
|--------|-----|------------------------|
| Profitable Markets | 4/28 (14%) | **22/28 (79%)** |
| Average Win Rate | ~30% | **~85%+** |

## Complete Analysis Results (T+5s, Fee=10bps)

| Market | Ticker | Matched | Raw EV | Shrink% | Opt Thr | Opt Count | **Opt EV** | Opt Win% |
|--------|--------|---------|--------|---------|---------|-----------|------------|----------|
| xyz:12 | GOOGL | 141,907 | -1.92 | 80.7% | 45 | 165 | **+74.22** | 100.0% |
| xyz:19 | NFLX | 286,526 | -4.27 | 68.4% | 45 | 143 | **+42.84** | 97.2% |
| xyz:21 | LLY | 125,497 | +1.29 | 83.4% | 40 | 390 | **+42.41** | 71.8% |
| xyz:3 | GOLD | 33,641 | -1.11 | 78.9% | 30 | 293 | **+37.63** | 100.0% |
| xyz:1 | TSLA | 135,583 | +0.09 | 85.4% | 40 | 220 | **+35.68** | 100.0% |
| xyz:24 | JPY | 96,776 | -2.18 | 83.5% | 35 | 156 | **+34.25** | 92.3% |
| xyz:29 | CL | 43,343 | -2.06 | 84.4% | 35 | 208 | **+33.89** | 100.0% |
| xyz:0 | XYZ100 | 20,508 | -0.67 | 90.4% | 35 | 149 | **+33.73** | 100.0% |
| xyz:4 | HOOD | 35,257 | -8.36 | 60.6% | 15 | 54 | **+32.96** | 100.0% |
| xyz:26 | SILVER | 187,053 | -4.83 | 78.1% | 30 | 102 | **+29.25** | 100.0% |
| xyz:18 | CRCL | 194,311 | -0.72 | 86.5% | 35 | 328 | **+28.74** | 69.5% |
| xyz:13 | AMZN | 342,247 | -3.05 | 72.7% | 35 | 777 | **+25.75** | 77.7% |
| xyz:23 | TSM | 39,632 | -0.37 | 88.3% | 30 | 748 | **+23.45** | 98.7% |
| xyz:8 | META | 202,239 | -2.82 | 81.7% | 35 | 480 | **+23.06** | 100.0% |
| xyz:17 | MSTR | 8,842 | -0.19 | 81.4% | 15 | 776 | **+22.12** | 97.8% |
| xyz:2 | NVDA | 20,716 | -0.18 | 87.5% | 25 | 237 | **+21.87** | 100.0% |
| xyz:11 | ORCL | 26,340 | -1.26 | 82.4% | 25 | 1,060 | **+17.89** | 92.8% |
| xyz:22 | SKHX | 30,877 | -0.67 | 86.3% | 35 | 340 | **+17.69** | 62.6% |
| xyz:5 | INTC | 93,016 | -1.54 | 84.2% | 25 | 1,655 | **+17.59** | 78.7% |
| xyz:28 | BABA | 192,554 | +0.89 | 79.3% | 35 | 1,207 | **+16.89** | 71.6% |
| xyz:16 | SNDK | 21,528 | -1.04 | 85.2% | 30 | 747 | **+9.56** | 85.3% |
| xyz:25 | EUR | 78,976 | +0.94 | 83.9% | 15 | 10,813 | **+5.80** | 55.4% |
| xyz:10 | MSFT | 6,484 | -4.68 | 73.9% | 5 | 456 | -1.52 | 45.0% |
| xyz:20 | COST | 234,734 | -7.12 | 56.6% | 45 | 15,014 | -5.88 | 15.8% |
| xyz:6 | PLTR | 1,553,114 | -9.74 | 51.3% | 50 | 2,213 | -5.97 | 17.8% |
| xyz:14 | AMD | 69,222 | -9.92 | 50.7% | 20 | 1,207 | -7.34 | 6.9% |
| xyz:31 | NATGAS | 70,192 | -9.68 | 27.1% | 20 | 3,233 | -9.34 | 1.8% |
| xyz:9 | AAPL | 33,121 | -9.85 | 14.7% | 0 | 33,121 | -9.85 | 0.1% |

## Phase B Mainnet Test Recommendations

### Tier 1: High Confidence (OptEV >= 10 bps, Signals >= 200)

| Ticker | Market | Threshold | Expected EV | Win Rate | Signals/48h |
|--------|--------|-----------|-------------|----------|-------------|
| **GOOGL** | xyz:12 | 45 | +74.22 bps | 100.0% | 165 |
| **LLY** | xyz:21 | 40 | +42.41 bps | 71.8% | 390 |
| **GOLD** | xyz:3 | 30 | +37.63 bps | 100.0% | 293 |
| **TSLA** | xyz:1 | 40 | +35.68 bps | 100.0% | 220 |
| **CRCL** | xyz:18 | 35 | +28.74 bps | 69.5% | 328 |
| **AMZN** | xyz:13 | 35 | +25.75 bps | 77.7% | 777 |
| **TSM** | xyz:23 | 30 | +23.45 bps | 98.7% | 748 |
| **META** | xyz:8 | 35 | +23.06 bps | 100.0% | 480 |
| **MSTR** | xyz:17 | 15 | +22.12 bps | 97.8% | 776 |
| **NVDA** | xyz:2 | 25 | +21.87 bps | 100.0% | 237 |
| **ORCL** | xyz:11 | 25 | +17.89 bps | 92.8% | 1,060 |
| **INTC** | xyz:5 | 25 | +17.59 bps | 78.7% | 1,655 |
| **BABA** | xyz:28 | 35 | +16.89 bps | 71.6% | 1,207 |
| **SNDK** | xyz:16 | 30 | +9.56 bps | 85.3% | 747 |

### Tier 2: Medium Confidence (5 <= OptEV < 10 bps or Low Signal Count)

| Ticker | Market | Threshold | Expected EV | Win Rate | Signals/48h | Note |
|--------|--------|-----------|-------------|----------|-------------|------|
| NFLX | xyz:19 | 45 | +42.84 bps | 97.2% | 143 | 高EV、低シグナル |
| JPY | xyz:24 | 35 | +34.25 bps | 92.3% | 156 | 高EV、低シグナル |
| CL | xyz:29 | 35 | +33.89 bps | 100.0% | 208 | 高EV |
| XYZ100 | xyz:0 | 35 | +33.73 bps | 100.0% | 149 | 高EV、低シグナル |
| HOOD | xyz:4 | 15 | +32.96 bps | 100.0% | 54 | サンプル少 |
| SILVER | xyz:26 | 30 | +29.25 bps | 100.0% | 102 | サンプル少 |
| SKHX | xyz:22 | 35 | +17.69 bps | 62.6% | 340 | Win率低め |
| EUR | xyz:25 | 15 | +5.80 bps | 55.4% | 10,813 | 高シグナル、低EV |

### Exclude List

| Market | Ticker | Opt EV | Reason |
|--------|--------|--------|--------|
| xyz:6 | PLTR | -5.97 | 収束せず (51.3%)、100% Sell偏り |
| xyz:20 | COST | -5.88 | 収束せず (56.6%) |
| xyz:14 | AMD | -7.34 | 収束せず (50.7%) |
| xyz:31 | NATGAS | -9.34 | 収束せず (27.1%) |
| xyz:9 | AAPL | -9.85 | 収束せず (14.7%) |
| xyz:10 | MSFT | -1.52 | 収益性なし |

## Recommended Configuration

```rust
// Phase B Mainnet Configuration
pub const OPTIMAL_THRESHOLDS: &[(&str, f64)] = &[
    // Tier 1: High Volume + High EV
    ("xyz:5",  25.0),  // INTC - 1,655 signals, +17.59 bps
    ("xyz:11", 25.0),  // ORCL - 1,060 signals, +17.89 bps
    ("xyz:28", 35.0),  // BABA - 1,207 signals, +16.89 bps
    ("xyz:13", 35.0),  // AMZN -   777 signals, +25.75 bps
    ("xyz:17", 15.0),  // MSTR -   776 signals, +22.12 bps
    ("xyz:23", 30.0),  // TSM  -   748 signals, +23.45 bps
    ("xyz:16", 30.0),  // SNDK -   747 signals, +9.56 bps
    ("xyz:8",  35.0),  // META -   480 signals, +23.06 bps
    ("xyz:21", 40.0),  // LLY  -   390 signals, +42.41 bps
    ("xyz:18", 35.0),  // CRCL -   328 signals, +28.74 bps
    ("xyz:3",  30.0),  // GOLD -   293 signals, +37.63 bps
    ("xyz:2",  25.0),  // NVDA -   237 signals, +21.87 bps
    ("xyz:1",  40.0),  // TSLA -   220 signals, +35.68 bps

    // Tier 2: Lower Volume but High EV
    ("xyz:29", 35.0),  // CL     -   208 signals, +33.89 bps
    ("xyz:12", 45.0),  // GOOGL  -   165 signals, +74.22 bps
    ("xyz:24", 35.0),  // JPY    -   156 signals, +34.25 bps
    ("xyz:0",  35.0),  // XYZ100 -   149 signals, +33.73 bps
    ("xyz:19", 45.0),  // NFLX   -   143 signals, +42.84 bps
    ("xyz:26", 30.0),  // SILVER -   102 signals, +29.25 bps

    // Tier 3: High Volume but Low EV
    ("xyz:25", 15.0),  // EUR  - 10,813 signals, +5.80 bps
];

// Exclude list
pub const EXCLUDED_MARKETS: &[&str] = &[
    "xyz:6",   // PLTR - No convergence
    "xyz:20",  // COST - No convergence
    "xyz:14",  // AMD - No convergence
    "xyz:31",  // NATGAS - No convergence
    "xyz:9",   // AAPL - No convergence
    "xyz:10",  // MSFT - Not profitable
];
```

## Key Insights

### 1. Signal Quality Matters
- 閾値なしでは4/28市場のみ利益
- 最適閾値適用で22/28市場が利益に転換
- **結論**: 高品質シグナル（net_edge >= 閾値）のみをトレードすべき

### 2. Volume vs Quality Tradeoff
- EUR: 10,813シグナルだが+5.80 bps
- GOOGL: 165シグナルだが+74.22 bps
- **推奨**: Phase Bでは中程度のシグナル数（200-1000）で高EVを優先

### 3. Convergence is Key
- Shrink Rate > 80%の市場は概ね利益
- Shrink Rate < 60%の市場（PLTR, COST, NATGAS）は除外すべき

### 4. Side Bias Warning
- PLTR: 100% Sell → 異常な偏り
- HOOD: 98.6% Sell → 要注意
- 極端な偏りはデータ品質問題の可能性

## Risk Considerations

1. **過学習リスク**: 高閾値市場はシグナル数が少なく、48時間データでの最適化は過学習の可能性
2. **市場環境変化**: テスト期間の市場環境が継続する保証なし
3. **実行遅延**: T+5s基準だが、実際の遅延は300-600ms
4. **スリッページ**: best_size考慮の執行可能性検証が必要

## Ticker Mapping Reference (meta(dex=xyz) API)

| Index | Ticker | Index | Ticker | Index | Ticker |
|-------|--------|-------|--------|-------|--------|
| xyz:0 | XYZ100 | xyz:12 | GOOGL | xyz:24 | JPY |
| xyz:1 | TSLA | xyz:13 | AMZN | xyz:25 | EUR |
| xyz:2 | NVDA | xyz:14 | AMD | xyz:26 | SILVER |
| xyz:3 | GOLD | xyz:15 | MU | xyz:27 | RIVN |
| xyz:4 | HOOD | xyz:16 | SNDK | xyz:28 | BABA |
| xyz:5 | INTC | xyz:17 | MSTR | xyz:29 | CL |
| xyz:6 | PLTR | xyz:18 | CRCL | xyz:30 | COPPER |
| xyz:7 | COIN | xyz:19 | NFLX | xyz:31 | NATGAS |
| xyz:8 | META | xyz:20 | COST | xyz:32 | URANIUM |
| xyz:9 | AAPL | xyz:21 | LLY | xyz:33 | ALUMINIUM |
| xyz:10 | MSFT | xyz:22 | SKHX | xyz:34 | SMSN |
| xyz:11 | ORCL | xyz:23 | TSM | xyz:35 | PLATINUM |

## Files Generated

| File | Description |
|------|-------------|
| `2026-01-22-48hour-analysis-complete.md` | This report |
| `2026-01-22-48hour-analysis-complete.json` | Complete analysis data |
| `optimal_thresholds_complete.json` | Production config (22 markets) |
| `2026-01-22-48hour-analysis.csv` | Excel-compatible summary |

---

*Generated: 2026-01-23 00:35 JST*
*Ticker Correction: 2026-01-26 (based on meta(dex=xyz) API)*
*Data Source: VPS mainnet signals (root@5.104.81.76)*
*Analysis Period: 48.4 hours (2026-01-20 ~ 2026-01-22)*
