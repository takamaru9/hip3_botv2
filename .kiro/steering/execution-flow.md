# Execution Flow

The bot operates in two phases: **Observation (Phase A)** for signal validation, and **Trading (Phase B)** for live execution.

## Operating Modes

| Mode | Purpose | Actions |
|------|---------|---------|
| `observation` | Signal validation, strategy tuning | Detect, log, no execution |
| `trading` | Live trading | Detect, execute, manage positions |

## Main Event Loop

```
WebSocket Events
      │
      ▼
┌─────────────────────┐
│  Message Parsing    │  (hip3-feed)
│  - BBO updates      │
│  - AssetCtx updates │
│  - Order updates    │
└─────────────────────┘
      │
      ▼
┌─────────────────────┐
│  Market State       │  (hip3-feed)
│  - Aggregate feeds  │
│  - Track freshness  │
└─────────────────────┘
      │
      ▼
┌─────────────────────┐
│  Risk Gate Check    │  (hip3-risk)
│  - 8 gates          │
│  - Early return     │
└─────────────────────┘
      │
      ├── BLOCKED ──► Log, skip
      │
      ▼ PASS
┌─────────────────────┐
│  Dislocation        │  (hip3-detector)
│  Detection          │
│  - Oracle vs BBO    │
│  - Edge calculation │
└─────────────────────┘
      │
      ├── NO SIGNAL ──► Continue
      │
      ▼ SIGNAL
┌─────────────────────┐
│  Phase A: Record    │  (hip3-persistence)
│  - Write Parquet    │
│  - Cross tracking   │
│  - Followup capture │
└─────────────────────┘
      │
      ▼ (trading mode only)
┌─────────────────────┐
│  Phase B: Execute   │  (hip3-executor)
│  - Gate checks      │
│  - Build order      │
│  - Submit IOC       │
└─────────────────────┘
```

## Phase A: Observation Mode

**Purpose**: Validate strategy before live trading

### Components Active
- WebSocket connection
- Market data feeds (BBO, AssetCtx)
- Risk gates (for monitoring only)
- Dislocation detector
- Signal persistence (Parquet)
- Cross duration tracking (P0-31)
- Followup snapshot capture (T+1s, T+3s, T+5s)

### Output Artifacts
```
data/signals/
├── signals_YYYYMMDD.parquet    # Signal records
└── followup_YYYYMMDD.parquet   # T+N validation snapshots
```

### No Execution
- No orders submitted
- No position tracking
- No executor loop

## Dashboard Integration

The `hip3-dashboard` crate provides real-time monitoring:

### Capabilities
- REST API (`GET /api/snapshot`) for current state
- WebSocket (`/ws`) for real-time updates (100ms interval)
- Signal push: Detected signals are broadcast to connected clients

### Signal Broadcasting
When a `DislocationSignal` is detected, it is pushed to the dashboard via `SignalSender`:
```
Detector generates signal
      │
      ▼
SignalSender.send(signal) ──► Dashboard WebSocket broadcast
      │
      ▼
Persistence (Parquet)
      │
      ▼
[Trading mode] Executor
```

## Phase B: Trading Mode

**Purpose**: Execute on detected dislocations

### Additional Components
- Position tracker (fills, pending orders)
- Executor loop (100ms tick)
- Batch scheduler (priority queue)
- Signer (request authentication)
- Rate limiter (ActionBudget)
- Risk monitor (HardStop triggers)
- Exit monitors:
  - **TimeStop**: Auto-flatten after threshold_ms (default 30s)
  - **MarkRegressionMonitor**: Profit-taking when BBO returns to Oracle

### Executor Gate Checks (in order)

```
Signal arrives at Executor
      │
      ▼
1. HardStop latch     → Rejected::HardStop
      │
      ▼
2. READY-TRADING      → Rejected::NotReady
      │
      ▼
3. MaxPositionPerMarket → Rejected::MaxPositionPerMarket
      │
      ▼
4. MaxPositionTotal   → Rejected::MaxPositionTotal
      │
      ▼
5. MaxConcurrentPositions → Rejected::MaxConcurrentPositions
      │
      ▼
6. has_position       → Skipped::AlreadyHasPosition
      │
      ▼
7. PendingOrder       → Skipped::PendingOrderExists
      │
      ▼
8. ActionBudget       → Skipped::BudgetExhausted
      │
      ▼
9. ALL PASSED         → try_mark_pending_market + enqueue
```

### Position Limits (Config)

| Parameter | Default | Purpose |
|-----------|---------|---------|
| max_concurrent_positions | 5 | Max simultaneous open positions |
| max_total_notional | $100 | Max total exposure across all positions |
| max_notional_per_market | $50 | Max exposure per single market (hard cap) |

### Dynamic Position Sizing

When enabled, position size is dynamically calculated based on account balance:

```
effective_max = min(max_notional_per_market, balance × risk_per_market_pct)
```

| Parameter | Default | Purpose |
|-----------|---------|---------|
| dynamic_sizing.enabled | false | Enable balance-based sizing |
| dynamic_sizing.risk_per_market_pct | 0.10 | % of balance per market (10%) |

**Example**: Balance $186 × 10% = $18.60 effective max per market

**Balance Sync**: Account balance is fetched from `clearinghouseState` API at startup and periodically refreshed. Stored in `balance_cache` (AtomicU64) for lock-free reads.

### Order Lifecycle

```
SIGNAL ──► PENDING ──► SUBMITTED ──► FILLED/REJECTED/EXPIRED
              │             │              │
              │             ▼              ▼
              │        (WS response)   Update position
              │                        Trigger TimeStop if needed
              ▼
         (timeout)
         Retry or cancel
```

## READY Conditions

Trading is blocked until ALL conditions are met. Managed by `TradingReadyChecker` with 4 atomic flags:

| Flag | Source | Criteria |
|------|--------|----------|
| `md_ready` | Market State | BBO + AssetCtx received for all markets |
| `order_snapshot` | orderUpdates channel | Subscription ACKed |
| `fills_snapshot` | userFills channel | Subscription ACKed (snapshot skipped for stability) |
| `position_synced` | PositionTracker | Startup sync from Hyperliquid API complete |

**Additional Prerequisites** (checked separately):
| Condition | Source | Criteria |
|-----------|--------|----------|
| WS Connected | ConnectionManager | `state == OPEN` |
| HardStop Clear | HardStopLatch | No emergency stop active |

### Startup Sync Flow

```
1. WS connects
2. Subscribe to orderUpdates, userFills
3. TradingReadyChecker.reset() clears all flags
4. POST /info clearinghouseState (with dex param for HIP-3) to:
   a. Fetch open positions → PositionTracker
   b. Fetch account balance → balance_cache (for dynamic sizing)
5. set_position_synced(true) on success
6. MarketData starts flowing → set_md_ready(true)
7. Channel ACKs received → set_order_snapshot(true), set_fills_snapshot(true)
8. All 4 flags true → READY-TRADING
```

**Note**: `userFills` snapshot (`isSnapshot: true`) was previously required but is now skipped to prevent stale position reconstruction. Positions are synced via REST API instead.

## Reconnection Recovery

```
1. HardStop ACTIVATED (block all orders)
2. WS reconnect with exponential backoff
3. Re-subscribe to all channels
4. Wait for READY conditions
5. Optionally: fetch snapshots via POST
6. HardStop RELEASED (resume trading)
```

## Exit Strategies

### TimeStop (Failsafe)
Auto-flatten positions held beyond threshold.

| Parameter | Default | Purpose |
|-----------|---------|---------|
| threshold_ms | 30000 | Max holding time before forced exit |
| reduce_only_timeout_ms | 60000 | Timeout for reduce-only order retry |
| slippage_bps | 50 | Price tolerance for flatten |

### MarkRegressionMonitor (Profit-taking)
Exit when Oracle-BBO divergence resolves (BBO returns to Oracle).

| Parameter | Default | Purpose |
|-----------|---------|---------|
| exit_threshold_bps | 5 | Exit when BBO within N bps of Oracle |
| check_interval_ms | 200 | Monitor frequency |
| min_holding_time_ms | 1000 | Minimum hold before exit can trigger |
| slippage_bps | 50 | Price tolerance for flatten |

**Exit Logic:**
```
Long:  Exit when best_bid >= oracle * (1 - threshold_bps/10000)
Short: Exit when best_ask <= oracle * (1 + threshold_bps/10000)
```

## Key Timing Parameters

| Parameter | Default | Purpose |
|-----------|---------|---------|
| Executor tick | 100ms | Batch processing interval |
| Heartbeat interval | 45s | WS keep-alive ping |
| Max BBO age | 2000ms | Freshness threshold |
| Max Ctx age | 8000ms | Oracle freshness |
| MarkRegression check | 200ms | Profit-taking exit check |
| TimeStop threshold | 30000ms | Position hold limit |
| Reconnect base delay | 1000ms | Exponential backoff base |

---
_Document flow and conditions, not implementation details_
