# Followup Snapshot Feature Implementation Spec

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-01-20 |
| Last Updated | 2026-01-20 |
| Status | `[COMPLETED]` |
| Source Plan | `/Users/taka/.claude/plans/gleaming-bouncing-moonbeam.md` |

## Overview

シグナル発生後 T+1s, T+3s, T+5s にマーケット状態をキャプチャし、シグナルが正しかったかを検証するためのデータを収集する機能。

### Purpose

- Oracle は約3秒ごとにデプロイヤーが更新
- Market Price が先行する場合と、Oracle が先行する場合がある
- シグナル発生時点だけでなく、その後の収束状況を記録することで、シグナルの有効性を検証可能にする

## Implementation Status

| ID | Item | Status | Notes |
|----|------|--------|-------|
| F-1 | `FollowupRecord` struct | [x] DONE | `crates/hip3-persistence/src/writer.rs:36-74` |
| F-2 | `FollowupWriter` class | [x] DONE | `crates/hip3-persistence/src/writer.rs:244-388` |
| F-3 | Export new types in lib.rs | [x] DONE | `crates/hip3-persistence/src/lib.rs:15` |
| F-4 | `FollowupContext` struct | [x] DONE | `crates/hip3-bot/src/app.rs` |
| F-5 | `FOLLOWUP_OFFSETS_MS` constant | [x] DONE | `[1000, 3000, 5000]` |
| F-6 | `followup_writer` field in Application | [x] DONE | `Arc<Mutex<FollowupWriter>>` |
| F-7 | `schedule_followups` method | [x] DONE | Spawns 3 background tasks |
| F-8 | `capture_followup` async function | [x] DONE | Delayed snapshot capture |
| F-9 | Unit tests | [x] DONE | 4 tests passed |
| F-10 | VPS deployment & verification | [x] DONE | All 3 offsets working |

## Data Structures

### FollowupRecord

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowupRecord {
    /// Reference to original signal.
    pub signal_id: String,
    /// Market key (e.g., "xyz:0").
    pub market_key: String,
    /// Trade side (buy/sell).
    pub side: String,

    /// T+0 signal detection time (milliseconds since epoch).
    pub signal_timestamp_ms: i64,
    /// Offset from signal time (1000, 3000, 5000 ms).
    pub offset_ms: u64,
    /// Actual capture time (milliseconds since epoch).
    pub captured_at_ms: i64,

    /// T+0 oracle price (for comparison).
    pub t0_oracle_px: f64,
    /// T+0 best price (for comparison).
    pub t0_best_px: f64,
    /// T+0 raw edge in basis points (for comparison).
    pub t0_raw_edge_bps: f64,

    /// Current oracle price at T+N.
    pub oracle_px: f64,
    /// Current best price at T+N.
    pub best_px: f64,
    /// Current best size at T+N.
    pub best_size: f64,
    /// Recalculated edge at T+N.
    pub raw_edge_bps: f64,

    /// Edge change from T+0 (raw_edge_bps - t0_raw_edge_bps).
    pub edge_change_bps: f64,
    /// Oracle movement in bps: (oracle_px - t0_oracle_px) / t0_oracle_px * 10000.
    pub oracle_moved_bps: f64,
    /// Market movement in bps: (best_px - t0_best_px) / t0_best_px * 10000.
    pub market_moved_bps: f64,
}
```

### FollowupContext (Internal)

```rust
#[derive(Debug, Clone)]
struct FollowupContext {
    signal_id: String,
    market_key: MarketKey,
    side: OrderSide,
    signal_timestamp_ms: i64,
    t0_oracle_px: f64,
    t0_best_px: f64,
    t0_raw_edge_bps: f64,
}
```

## File Output

### Location

```
data/mainnet/signals/
├── signals_YYYY-MM-DD.jsonl       # シグナル
└── followups_YYYY-MM-DD.jsonl     # フォローアップ
```

### Sample Output

```json
{"signal_id":"xyz-BTC_USDC:0-1737365041234","market_key":"xyz-BTC_USDC:0","side":"sell","signal_timestamp_ms":1737365041234,"offset_ms":1000,"captured_at_ms":1737365042235,"t0_oracle_px":104123.5,"t0_best_px":104145.2,"t0_raw_edge_bps":20.85,"oracle_px":104125.0,"best_px":104140.0,"best_size":0.5,"raw_edge_bps":14.42,"edge_change_bps":-6.43,"oracle_moved_bps":0.14,"market_moved_bps":-4.99}
```

## Deviations from Plan

### 1. Mutex Implementation

| Original Quote | Actual Implementation | Reason |
|----------------|----------------------|--------|
| `parking_lot::Mutex` | `std::sync::Mutex` | `parking_lot` crate not available in project dependencies |

### 2. Error Handling

| Original Quote | Actual Implementation | Reason |
|----------------|----------------------|--------|
| `if let Ok(mut writer) = followup_writer.lock()` | `match followup_writer.lock() { Ok(mut writer) => ... }` | More explicit error handling with warning logs for lock failures |

## Verification Results

### VPS Deployment

- **Host**: `root@5.104.81.76`
- **Deployment Date**: 2026-01-20
- **Method**: Docker compose rebuild and restart

### Data Verification

| Offset | Records Count | Status |
|--------|--------------|--------|
| T+1s (1000ms) | 11,088 | ✓ Working |
| T+3s (3000ms) | 11,040 | ✓ Working |
| T+5s (5000ms) | 10,972 | ✓ Working |

### Symbol Coverage

- **Total xyz symbols**: 32
- **Symbols with signals/followups**: 25
- **Coverage**: 78% (expected - not all symbols generate dislocations)

## Analysis Metrics

### Available Metrics

| Metric | Calculation | Interpretation |
|--------|-------------|----------------|
| Edge Retention | `edge_t5 / edge_t0` | How much edge remains after 5s |
| Oracle Convergence | `oracle_moved_bps` sign | + = Oracle moved toward market |
| Market Convergence | `market_moved_bps` sign | + = Market moved toward oracle |
| Signal Validity | `edge_change_bps < 0` | Edge shrinking = correct signal |

### Analysis Query Example

```python
import polars as pl

signals = pl.read_ndjson("signals_2026-01-20.jsonl")
followups = pl.read_ndjson("followups_2026-01-20.jsonl")

# Edge progression by signal
joined = followups.join(signals, on="signal_id")
edge_decay = joined.group_by("signal_id").agg([
    pl.col("t0_raw_edge_bps").first().alias("edge_t0"),
    pl.col("raw_edge_bps").filter(pl.col("offset_ms") == 1000).first().alias("edge_t1"),
    pl.col("raw_edge_bps").filter(pl.col("offset_ms") == 3000).first().alias("edge_t3"),
    pl.col("raw_edge_bps").filter(pl.col("offset_ms") == 5000).first().alias("edge_t5"),
])

# Edge retention rates
edge_decay.with_columns([
    (pl.col("edge_t1") / pl.col("edge_t0")).alias("retention_1s"),
    (pl.col("edge_t3") / pl.col("edge_t0")).alias("retention_3s"),
    (pl.col("edge_t5") / pl.col("edge_t0")).alias("retention_5s"),
])
```

## Key Implementation Details

### Data Flow

```
Signal Detection (T+0ms)
    ↓
[DislocationSignal 検出]
    ├─→ SignalRecord → signals_YYYY-MM-DD.jsonl (即時)
    ├─→ Spawn task 1: sleep(1s) → capture → followups_YYYY-MM-DD.jsonl
    ├─→ Spawn task 2: sleep(3s) → capture → followups_YYYY-MM-DD.jsonl
    └─→ Spawn task 3: sleep(5s) → capture → followups_YYYY-MM-DD.jsonl
```

### Thread Safety

- `FollowupWriter` wrapped in `Arc<Mutex<>>` for concurrent access
- Each spawned task holds its own `Arc` clone
- Lock acquisition uses proper error handling with `match` statement

### Resource Considerations

- **Memory**: 1 signal = 3 lightweight spawned tasks
- **Rate Limiting**: Followup capture is read-only, no WebSocket impact
- **Fault Tolerance**: Task failures only log warnings, main loop unaffected

## Future Improvements

1. **Refactoring**: `JsonLinesWriter` and `FollowupWriter` share significant code duplication - could be generalized with generic type parameter
2. **Configurable Offsets**: Currently hardcoded `[1000, 3000, 5000]`, could be made configurable
3. **Batch Writes**: Current implementation writes immediately on each capture, could batch for efficiency

## Related Files

| File | Purpose |
|------|---------|
| `crates/hip3-persistence/src/writer.rs` | FollowupRecord, FollowupWriter |
| `crates/hip3-persistence/src/lib.rs` | Type exports |
| `crates/hip3-bot/src/app.rs` | schedule_followups, capture_followup |
