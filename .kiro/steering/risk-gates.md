# Risk Gates

Risk gates are **hard stop conditions** that must ALL pass before any trade can be executed. The bot prioritizes stopping over trading when in doubt.

## Gate Philosophy

> "When in doubt, block."

Gates are evaluated in a specific order to prevent side effects (like EWMA updates) when data is stale or invalid.

## Gate Evaluation Order

```
Phase 1: Prerequisite Gates (early return on block)
  ├─ Gate 1: BboUpdate (data freshness)
  ├─ Gate 2: CtxUpdate (data freshness, covers oracle)
  └─ Gate 3: TimeRegression (data integrity)

Phase 2: BBO Validity (before EWMA update)
  └─ Gate 4: MarkMidDivergence

Phase 3: Side Effect Gates (EWMA update)
  └─ Gate 5: SpreadShock

Phase 4: Position and Market Status
  ├─ Gate 6: OiCap
  ├─ Gate 7: ParamChange
  └─ Gate 8: Halt
```

## Gate Details

### Gate 1: BboUpdate (P0-12)
**Purpose**: Block if BBO data is stale
**Threshold**: `max_bbo_age_ms` (default: 2000ms)
**Trigger**: No BBO update received within threshold
**Recovery**: Automatic when fresh data arrives

### Gate 2: CtxUpdate (P0-12)
**Purpose**: Block if AssetCtx (including oracle) is stale
**Threshold**: `max_ctx_age_ms` (default: 8000ms)
**Trigger**: No AssetCtx update received within threshold
**Recovery**: Automatic when fresh data arrives
**Note**: Replaces deprecated OracleFresh gate (BUG-002 fix)

### Gate 3: TimeRegression (P0-16)
**Purpose**: Block if server time goes backwards (data integrity)
**Trigger**: `current_bbo_time < last_bbo_time`
**Recovery**: **Requires reconnect** (manual reset)
**Critical**: Indicates potential data corruption or replay

### Gate 4: MarkMidDivergence
**Purpose**: Block if mark price diverges too much from mid price
**Threshold**: `max_mark_mid_divergence_bps` (default: 50 bps)
**Formula**: `|mark - mid| / mid * 10000 > threshold`
**Trigger**: Abnormal divergence suggests oracle/feed issues

### Gate 5: SpreadShock
**Purpose**: Reduce size or block if spread is abnormally wide
**Method**: Compare current spread against EWMA
**Threshold**: `spread_shock_multiplier` (default: 3x EWMA)
**Actions**:
- `spread > 2x threshold`: **Block**
- `spread > threshold`: **ReduceSize** (factor: 0.2)

**EWMA Protection**: Only updated after prerequisite gates pass

### Gate 6: OiCap
**Purpose**: Block if open interest at capacity
**Threshold**: `max_oi_fraction` of market OI cap (default: 1%)
**Trigger**: Current OI >= market OI cap
**Warning**: Logged at 95% utilization

### Gate 7: ParamChange
**Purpose**: Block permanently if market parameters change
**Trigger**: Tick size, lot size, or fee changes detected
**Recovery**: **Requires manual restart**
**Rationale**: Parameter changes may invalidate strategy assumptions

### Gate 8: Halt
**Purpose**: Block if market is halted or inactive
**Trigger**: `spec.is_active == false` or halt signal received
**Recovery**: Automatic when market resumes

## Position Gates (Executor Layer)

These gates operate at the Executor level, not the RiskGate module:

### MaxPositionPerMarket
**Purpose**: Limit position size per market
**Formula**: `current_notional + pending_notional + order_notional <= max_notional_usd`
**Note**: Excludes reduce-only orders from pending calculation

### MaxPositionTotal
**Purpose**: Limit total portfolio exposure across all markets
**Formula**: `sum(all_position_notionals) + order_notional <= max_total_notional_usd`

## Configuration (TOML)

```toml
[risk]
max_oracle_age_ms = 8000
max_mark_mid_divergence_bps = 50
spread_shock_multiplier = 3
min_buffer_ratio = 0.15
max_oi_fraction = 0.01
max_bbo_age_ms = 2000
max_ctx_age_ms = 8000
```

## Logging Strategy (BUG-003 Fix)

- **Block events**: Use `trace!` level (not `warn!`) to reduce log spam
- **State changes**: Log only when gate transitions from pass→block or block→pass
- **Market context**: Include `MarketKey` in log messages for debugging

---
_Document gate logic and thresholds, not implementation details_
