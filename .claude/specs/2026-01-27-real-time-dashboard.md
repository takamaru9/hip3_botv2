# Real-Time Dashboard Implementation Spec

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-01-27 |
| Last Updated | 2026-01-27 |
| Status | `[COMPLETED]` |
| Source Plan | `.claude/plans/2026-01-27-real-time-dashboard.md` |

## Implementation Status

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P1 | New crate setup (`hip3-dashboard`) | [x] DONE | Added to workspace with axum/tower-http deps |
| P2.1 | Dashboard types (`types.rs`) | [x] DONE | DashboardSnapshot, DashboardMessage, etc. |
| P2.2 | Dashboard state (`state.rs`) | [x] DONE | Aggregates MarketState, PositionTracker, HardStopLatch |
| P3.1 | HTTP server (`server.rs`) | [x] DONE | axum Router with `/`, `/api/snapshot`, `/ws` |
| P3.2 | Connection limiter | [x] DONE | AtomicUsize-based with ConnectionGuard |
| P3.3 | Basic auth | [x] DONE | Optional, configurable via config |
| P4 | WebSocket broadcaster | [x] DONE | tokio::sync::broadcast with 100ms interval |
| P5 | Frontend HTML/JS | [x] DONE | Single-file embedded via include_str! |
| P6.1 | Dashboard config in hip3-bot | [x] DONE | Added `[dashboard]` section |
| P6.2 | Integration in app.rs | [x] DONE | Spawns server in Trading mode |
| P6.3 | recent_signals buffer | [x] DONE | Arc<RwLock<VecDeque<SignalRecord>>> |
| P7 | Tests pass | [x] DONE | All workspace tests pass |

## Deviations from Plan

1. **MarketSnapshot fields not Option<>**
   - Original: Assumed `bbo` and `ctx` were `Option<Bbo>` and `Option<AssetCtx>`
   - Actual: `MarketSnapshot` has direct `Bbo` and `AssetCtx` fields (not wrapped in Option)
   - Resolution: Updated `snapshot_to_market_data()` to use references directly

2. **Observation mode dashboard** (UPDATED)
   - Original: Plan didn't specify behavior
   - Initial: Dashboard skipped in Observation mode (no PositionTracker/HardStopLatch available)
   - Updated: Dashboard now supports Observation mode with limited features
   - Implementation: `DashboardState::new_observation_mode()` provides market data only
   - Features in Observation mode: Market data (BBO, Oracle, Edge), Signals, Gate blocks
   - Not available in Observation mode: Positions, P&L, HardStop status (shows "Observation mode")

## Key Implementation Details

### New Files Created

| File | Purpose |
|------|---------|
| `crates/hip3-dashboard/Cargo.toml` | Crate manifest with axum, tower-http deps |
| `crates/hip3-dashboard/src/lib.rs` | Module exports |
| `crates/hip3-dashboard/src/types.rs` | API response types (Serialize) |
| `crates/hip3-dashboard/src/state.rs` | DashboardState aggregator |
| `crates/hip3-dashboard/src/server.rs` | axum HTTP server + WebSocket |
| `crates/hip3-dashboard/src/broadcast.rs` | Broadcaster task |
| `crates/hip3-dashboard/src/config.rs` | DashboardConfig |
| `crates/hip3-dashboard/static/index.html` | Frontend UI (embedded) |

### Modified Files

| File | Changes |
|------|---------|
| `Cargo.toml` (root) | Added workspace member, axum/tower-http deps |
| `crates/hip3-bot/Cargo.toml` | Added hip3-dashboard dep |
| `crates/hip3-bot/src/config.rs` | Added DashboardConfig field |
| `crates/hip3-bot/src/app.rs` | Added recent_signals, dashboard spawn |
| `config/default.toml` | Added [dashboard] section |

### Architecture

**Trading Mode (full features):**
```
Browser <-- HTTP/WS (port 8080) --> axum Server
                                        |
                                   DashboardState
                                   /      |       \
                          MarketState  PositionTracker  HardStopLatch
                          (Arc<>)      (Handle)         (Arc<>)
```

**Observation Mode (market data only):**
```
Browser <-- HTTP/WS (port 8080) --> axum Server
                                        |
                                   DashboardState
                                   /           \
                          MarketState      RecentSignals
                          (Arc<>)          (Arc<RwLock<>>)
```

### Security Features

- Basic auth (optional, configurable)
- Connection limiter (max 10 concurrent WS by default)
- Read-only (no control endpoints)

### Configuration Example

```toml
[dashboard]
enabled = true
port = 8080
update_interval_ms = 100
max_connections = 10
username = "admin"
password = "secret"
```

## Testing Notes

- Unit tests: 3 tests in hip3-dashboard (types serialization, broadcast channel)
- Integration: Works in both Trading and Observation modes with dashboard.enabled = true
- Manual verification: Open browser to http://VPS_IP:8080
- Observation mode: Shows market data only (positions/risk sections will be empty/disabled)

## Production Deployment

1. Set `dashboard.enabled = true` in config
2. Configure basic auth credentials for security
3. Expose port 8080 in docker-compose
4. (Recommended) Use nginx reverse proxy with HTTPS/TLS
