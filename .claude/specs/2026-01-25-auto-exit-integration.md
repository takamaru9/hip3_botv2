# Auto Exit Integration Implementation Spec

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-01-25 |
| Last Updated | 2026-01-25 |
| Status | `[COMPLETED]` |
| Source Plan | `.claude/plans/2026-01-25-auto-exit-integration.md` |

---

## Implementation Status

### Phase 1: MarkPriceProvider

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P1-1 | PriceProvider trait確認 | [x] DONE | `hip3-position/src/time_stop.rs:273-278` |
| P1-2 | MarkPriceProvider実装 | [x] DONE | `hip3-executor/src/price_provider.rs` 新規作成 |
| P1-3 | hip3-executor/lib.rs exports | [x] DONE | `pub use price_provider::MarkPriceProvider` |
| P1-4 | ユニットテスト | [x] DONE | 既存テストパス |

### Phase 2: TimeStopMonitor Integration

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P2-1 | TimeStopConfig構造体 | [x] DONE | `hip3-bot/src/config.rs` に追加 |
| P2-2 | config/default.toml追加 | [x] DONE | `[time_stop]` セクション追加 |
| P2-3 | flatten channel作成 | [x] DONE | `app.rs` で `mpsc::channel` 作成 |
| P2-4 | MarkPriceProvider初期化 | [x] DONE | `MarketStateCache` からラップ |
| P2-5 | TimeStopMonitor起動 | [x] DONE | `tokio::spawn` でバックグラウンド実行 |
| P2-6 | ログ出力確認 | [x] DONE | `TimeStopMonitor started: threshold=30000ms` |

### Phase 3: RiskMonitor Integration

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P3-1 | RiskMonitorConfig構造体 | [x] DONE | `hip3-bot/src/config.rs` に追加 |
| P3-2 | ExecutorRiskMonitorConfig変換 | [x] DONE | `app.rs` で型変換実装 |
| P3-3 | RiskMonitor初期化 | [x] DONE | `hip3-executor::RiskMonitor` 使用 |
| P3-4 | ExecutionEvent送信 | [x] DONE | `handle_order_update()` でRejected送信 |
| P3-5 | ログ出力確認 | [x] DONE | `RiskMonitor started: max_loss_usd=20.0` |

### Phase 4: HardStop Flatten Integration

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P4-1 | flatten_all_positions import | [x] DONE | `hip3-position` から import |
| P4-2 | HardStop watcher task | [x] DONE | 100ms間隔でlatch監視 |
| P4-3 | flatten実行ロジック | [x] DONE | 全ポジション → reduce-only注文 |
| P4-4 | リトライロジック | [x] DONE | 最大3回、1秒間隔 |
| P4-5 | CRITICALアラート | [x] DONE | 残ポジション時にエラーログ |
| P4-6 | ログ出力確認 | [x] DONE | `HardStop flatten watcher started` |

### Phase 5: Flattener State Management (Optional)

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P5-1 | Flattener状態追跡 | [-] SKIPPED | Phase 4のリトライで十分 |

---

## Deviations from Plan

### D1: Config Loading Fix (Bug Fix)

**Original**: 計画になし

**Actual**: `main.rs` で `HIP3_CONFIG` 環境変数が無視されるバグを発見・修正

```rust
// Before: CLI引数のみ使用
let args = Args::parse();
let config = AppConfig::from_file(&args.config)?;

// After: CLI > 環境変数 > デフォルト
let config_path = args
    .config
    .or_else(|| std::env::var("HIP3_CONFIG").ok())
    .unwrap_or_else(|| "config/default.toml".to_string());
```

**Reason**: メインネットテスト時に設定が読み込まれないバグが発覚

### D2: RiskMonitor Location

**Original**: `hip3-risk::RiskMonitor` を使用予定

**Actual**: `hip3-executor::RiskMonitor` を使用（既にそこに実装されていた）

**Reason**: 既存コードの調査で正しいパスを特定

### D3: ExecutionEvent Channel vs Direct Call

**Original**: RiskMonitor に `on_event()` 直接呼び出し

**Actual**: `mpsc::channel<ExecutionEvent>` 経由で非同期送信

**Reason**: app.rs から executor crate への依存を減らし、テスト容易性を向上

---

## Key Implementation Details

### File Changes Summary

| File | Change Type | Description |
|------|-------------|-------------|
| `crates/hip3-bot/src/main.rs` | Modified | Config loading priority fix |
| `crates/hip3-bot/src/app.rs` | Major | All component integrations |
| `crates/hip3-bot/src/config.rs` | Modified | TimeStopConfig, RiskMonitorConfig |
| `crates/hip3-bot/Cargo.toml` | Modified | rust_decimal dependency |
| `crates/hip3-executor/src/price_provider.rs` | New | MarkPriceProvider |
| `crates/hip3-executor/src/lib.rs` | Modified | price_provider export |
| `config/default.toml` | Modified | time_stop, risk_monitor sections |
| `config/mainnet-test.toml` | Modified | time_stop, risk_monitor sections |

### Mainnet Test Results (2026-01-25)

| Metric | Value |
|--------|-------|
| Config Loaded | ✅ Trading mode, mainnet URLs |
| Signer Initialized | ✅ is_mainnet: true |
| TimeStopMonitor | ✅ threshold_ms: 30000 |
| RiskMonitor | ✅ max_loss_usd: 20.0 |
| HardStop Watcher | ✅ Started |
| WebSocket | ✅ Connected |
| Market Data | ✅ xyz:SILVER, xyz:CL receiving |

---

## Test Results

### Unit Tests

```
cargo test --workspace
# 379+ tests passed
```

### Mainnet Integration Test

```
HIP3_CONFIG=config/mainnet-test.toml HIP3_TRADING_KEY=... ./target/release/hip3-bot
```

- Configuration: Trading mode, mainnet URLs ✅
- All components initialized ✅
- WebSocket connected ✅
- Market data flowing ✅
- No errors during 25s runtime ✅

---

## Completion Checklist

- [x] Phase 1: MarkPriceProvider
- [x] Phase 2: TimeStopMonitor Integration
- [x] Phase 3: RiskMonitor Integration
- [x] Phase 4: HardStop Flatten Integration
- [-] Phase 5: Flattener State Management (deferred)
- [x] Config loading bug fix
- [x] Mainnet test passed
- [x] All workspace tests pass (379+)
