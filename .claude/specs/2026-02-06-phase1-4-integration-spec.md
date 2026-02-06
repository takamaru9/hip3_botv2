# Phase 1-4 全方位改善 Integration Spec

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-02-04 |
| Last Updated | 2026-02-06 |
| Status | `[COMPLETED]` |
| Source Plan | `.claude/plans/rustling-doodling-puddle.md` |

## Implementation Status

### Phase 1: Observability基盤

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P1-1 | テレメトリ強化 (metrics.rs) | [x] DONE | trade_pnl_bps, position_holding_time_ms, entry_edge_bps, signal_to_order_latency_ms追加 |
| P1-2 | EdgeTracker拡張 (edge_tracker.rs) | [x] DONE | P50/P75/P90/P99パーセンタイル、実現エッジ記録 |
| P1-3 | Risk Gate詳細メトリクス (gates.rs) | [x] DONE | Gate別・市場別のブロック回数Prometheusカウンター |
| P1-4 | Exit理由アトリビューション (tracker.rs, oracle_exit.rs) | [x] DONE | exit_reason (OracleReversal/OracleCatchup/MarkRegression/TimeStop)出力 |

### Phase 2: Config駆動の改善

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P2-1 | Oracle速度分析 (oracle_tracker.rs, detector.rs) | [x] DONE | `oracle_velocity_sizing` flag, velocity_multiplier_cap |
| P2-2 | 適応的閾値 (detector.rs, config.rs) | [x] DONE | `adaptive_threshold` flag, spread_ewma tracking |
| P2-3 | ドローダウン制御 (gates.rs) | [x] DONE | MaxDrawdownGate, `max_hourly_drawdown_usd=0` (無効) |
| P2-4 | 相関クールダウン (gates.rs) | [x] DONE | CorrelationCooldownGate, `correlation_close_threshold=0` (無効) |
| P2-5 | 動的Exit閾値 (oracle_exit.rs) | [x] DONE | `dynamic_thresholds` flag, high/low edge thresholds |
| P2-6 | 時間減衰Exit (exit_watcher.rs) | [x] DONE | `time_decay_enabled` flag, decay_start_ms/min_factor |
| P2-7 | イベント駆動Executor (batch.rs, executor_loop.rs) | [x] DONE | Notify-based wakeup, 5ms cooldown |
| P2-8 | TCP_NODELAY + 事前シリアライズ (connection.rs) | [x] DONE | TCP_NODELAY設定 |

### Phase 3: 高度な改善

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P3-1 | 信頼度ベースサイジング (detector.rs, signal.rs) | [x] DONE | `confidence_sizing` flag, multi-factor 0.0-1.0 score |
| P3-2 | トレイリングストップ (oracle_exit.rs) | [x] DONE | `trailing_stop` flag, activation_bps/trail_bps |
| P3-3 | 相関ポジションリミット (gates.rs) | [x] DONE | CorrelationPositionGate, weighted counting |
| P3-4 | Dashboard PnLサマリー (state.rs, types.rs, broadcast.rs) | [x] DONE | CompletedTrade, MarketPnlStats, PnlSummary types |

### Phase 4: 統合 + デプロイ

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P4-1 | 全機能のapp.rs統合 | [x] DONE | 全機能ワイヤリング済み、feature flag文書化済み |
| P4-2 | 段階的VPSロールアウト | [ ] TODO | 下記ロールアウト計画に従う |

---

## Feature Flag 一覧 (P4-1文書)

### 全Feature Flags（有効化推奨順序付き）

| # | Feature | Config Key | Type | Default | Rollout Day | Risk |
|---|---------|-----------|------|---------|-------------|------|
| 1 | MarkRegression Exit | `mark_regression.enabled` | bool | true | Day 0 (既存) | - |
| 2 | Oracle Exit | `oracle_exit.enabled` | bool | true | Day 0 (既存) | - |
| 3 | Dashboard | `dashboard.enabled` | bool | false | Day 0 (既存) | Low |
| 4 | Dynamic Position Sizing | `position.dynamic_sizing.enabled` | bool | false | Day 0 (既存) | - |
| 5 | イベント駆動Executor | N/A (常時有効) | - | - | Day 2 | None |
| 6 | TCP_NODELAY | N/A (常時有効) | - | - | Day 2 | None |
| 7 | Max Drawdown Gate | `max_drawdown.max_hourly_drawdown_usd` | f64 | 0.0 (off) | Day 3 | None (防御のみ) |
| 8 | Correlation Cooldown Gate | `correlation_cooldown.correlation_close_threshold` | u32 | 0 (off) | Day 3 | None (防御のみ) |
| 9 | Oracle Velocity Sizing | `detector.oracle_velocity_sizing` | bool | false | Day 4 | Low |
| 10 | Adaptive Threshold | `detector.adaptive_threshold` | bool | false | Day 4 | Low |
| 11 | Dynamic Exit Thresholds | `oracle_exit.dynamic_thresholds` | bool | false | Day 5 | Low |
| 12 | Time Decay Exit | `mark_regression.time_decay_enabled` | bool | false | Day 5 | Low |
| 13 | Confidence Sizing | `detector.confidence_sizing` | bool | false | Day 6+ | Medium |
| 14 | Trailing Stop | `oracle_exit.trailing_stop` | bool | false | Day 6+ | Medium |
| 15 | Correlation Position Limit | `correlation_position.enabled` | bool | false | Day 6+ | Medium |

### 段階的有効化 Config例

#### Day 2: レイテンシ改善 (リスクなし)
```toml
# P2-7, P2-8は常時有効 (コード変更で反映済み)
# 追加config変更不要
```

#### Day 3: リスク制御 (防御のみ、取引変更なし)
```toml
[max_drawdown]
max_hourly_drawdown_usd = 10.0  # $10/hour上限
reset_interval_secs = 3600

[correlation_cooldown]
correlation_close_threshold = 3  # 30秒内に3件クローズでCooldown
correlation_window_secs = 30
correlation_cooldown_secs = 60
```

#### Day 4: エッジ改善 (1-2市場で検証)
```toml
[detector]
oracle_velocity_sizing = true
velocity_multiplier_cap = 1.5

adaptive_threshold = true
spread_threshold_multiplier = 1.5
```

#### Day 5: Exit改善
```toml
[oracle_exit]
dynamic_thresholds = true
high_edge_bps = 40
low_edge_bps = 25

[mark_regression]
time_decay_enabled = true
decay_start_ms = 5000
min_factor = 0.2
```

#### Day 6+: Phase 3機能
```toml
[detector]
confidence_sizing = true
confidence_edge_weight = 0.3
confidence_velocity_weight = 0.2
confidence_consecutive_weight = 0.2
confidence_depth_weight = 0.15
confidence_profile_weight = 0.15

[oracle_exit]
trailing_stop = true
activation_bps = 5.0
trail_bps = 3.0

[correlation_position]
enabled = true
max_weighted_positions = 5.0

[[correlation_position.groups]]
name = "Precious Metals"
markets = ["GOLD", "SILVER", "PLATINUM"]
weight = 1.5

[[correlation_position.groups]]
name = "Currency"
markets = ["JPY", "EUR", "DXY", "GBP"]
weight = 1.3
```

---

## Config後方互換性

- **AppConfig**: 全新規フィールドに `#[serde(default)]` 付与済み
- **子Config**: P1-P3で追加された全フィールドに `#[serde(default)]` 付与済み
- **既存TOMLファイル**: 新フィールド追加なしでそのまま動作

## テスト結果

| Metric | Value |
|--------|-------|
| Total Tests | 474 |
| Passed | 471 |
| Failed | 0 |
| Ignored | 3 |
| Clippy Warnings | 0 |

## Deviations from Plan

| # | Plan | Actual | Reason |
|---|------|--------|--------|
| 1 | P2-8にJSONペイロード事前シリアライズ | TCP_NODELAYのみ実装 | 事前シリアライズのリファクタは影響範囲が大きいため別途検討 |
| 2 | P3-3をMaxPositionTotalGateに統合 | 独立CorrelationPositionGateを追加 | 既存Gate変更よりも新Gateの方がリスク低・テスト容易 |
| 3 | P3-4にエッジ減衰曲線を追加 | PnLサマリー・勝率のみ | エッジ減衰曲線はフロントエンドJS変更が必要、別途対応 |
