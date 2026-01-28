# hip3_botv2: Path to 10/10 Score

## Metadata
| Item | Value |
|------|-------|
| Date | 2026-01-22 |
| Current Score | 8.5/10 |
| Target Score | 10/10 |
| Reviewer | code-reviewer agent |
| Files Analyzed | 15+ core modules |

---

## Executive Summary

hip3_botv2 is a well-architected Rust trading bot with solid fundamentals: proper error handling patterns, good separation of concerns, and non-negotiable safety-first design. However, to achieve a perfect 10/10 score, several areas need improvement:

1. **Test Coverage** (current: ~60%, target: 90%+)
2. **Code Structure** (large files need decomposition)
3. **Configuration** (hardcoded values need extraction)
4. **Documentation** (missing README and module docs)
5. **Error Handling** (remaining `unwrap()`/`expect()` in hot paths)

---

## Current Assessment: 8.5/10

### Score Breakdown

| Category | Current | Max | Notes |
|----------|---------|-----|-------|
| Architecture | 2.0 | 2.0 | Excellent crate separation, actor pattern |
| Safety | 1.8 | 2.0 | Good, but `unwrap()` in metrics.rs |
| Test Coverage | 1.0 | 2.0 | Missing integration tests, sparse unit tests |
| Code Quality | 1.7 | 2.0 | Large files, magic numbers |
| Documentation | 1.0 | 1.0 | Basic docs, missing README |
| Maintainability | 1.0 | 1.0 | Clean patterns, needs refactoring |
| **Total** | **8.5** | **10.0** | |

### Deduction Reasons

| Issue | Deduction | Category |
|-------|-----------|----------|
| No integration tests | -0.5 | Test Coverage |
| Large files (>1000 lines) | -0.2 | Code Quality |
| 27+ `unwrap()` in metrics.rs | -0.1 | Safety |
| Hardcoded constants | -0.1 | Code Quality |
| Missing README | -0.3 | Documentation |
| No crate-level READMEs | -0.1 | Documentation |
| config.rs `expect()` calls | -0.1 | Safety |
| Missing tests for app.rs | -0.1 | Test Coverage |

---

## Improvement Roadmap

### Priority P0 (Critical - Must Fix)

#### P0-1: Add Integration Tests

**Current State:** `/tests/` directory exists but is empty (only `__init__.py`).

**Impact:** Cannot verify end-to-end behavior, crate interactions, or regression.

**Solution:**

```rust
// tests/integration/ws_executor_flow.rs
#[tokio::test]
async fn test_signal_to_order_flow() {
    // 1. Start mock WS server
    let mock_server = MockWsServer::start().await;

    // 2. Create minimal Application
    let config = AppConfig::test_config(&mock_server.url());
    let app = Application::new(config).unwrap();

    // 3. Inject signal
    let signal = DislocationSignal::test_signal();

    // 4. Verify order is queued
    assert!(app.executor_loop.batch_scheduler().has_pending_orders());
}
```

**Files to Create:**
```
tests/
  integration/
    mod.rs
    ws_connection_test.rs      # WS connect/reconnect/shutdown
    signal_flow_test.rs        # Signal -> Gate -> Executor -> Order
    risk_gate_test.rs          # Gate blocking scenarios
    position_tracker_test.rs   # Position state management
  common/
    mod.rs
    mock_ws_server.rs          # Mock WebSocket server
    test_helpers.rs            # Shared test utilities
```

**Estimated Effort:** 3-4 days

---

#### P0-2: Fix `unwrap()` in Production Hot Paths

**Current State:** 27+ `unwrap()` calls in `metrics.rs`, critical path `expect()` in `config.rs`.

**Files Affected:**
| File | Line(s) | Issue |
|------|---------|-------|
| metrics.rs | 23-302 | 27 `unwrap()` in Lazy static initialization |
| config.rs | 166, 182 | `expect("Markets not set")` |
| writer.rs | 195, 350 | `expect("active_writer should exist")` |
| real_ws_sender.rs | 40, 53 | `expect("...serialization")` |
| connection.rs | 581 | `unwrap()` in jitter calculation |
| app.rs | 435, 517 | `unwrap()` in async tasks |

**Solution for metrics.rs:**

```rust
// Before: Panics on registration failure
pub static WS_CONNECTED: Lazy<Gauge> = Lazy::new(|| {
    register_gauge!("hip3_ws_connected", "...").unwrap()
});

// After: Fallback to default gauge on failure
pub static WS_CONNECTED: Lazy<Gauge> = Lazy::new(|| {
    register_gauge!("hip3_ws_connected", "...")
        .unwrap_or_else(|e| {
            tracing::error!(?e, "Failed to register WS_CONNECTED metric");
            Gauge::new("hip3_ws_connected_fallback", "Fallback gauge")
                .expect("Fallback gauge creation should not fail")
        })
});
```

**Better Solution: Wrap in Result-returning function**

```rust
// metrics.rs
use std::sync::OnceLock;

static METRICS: OnceLock<MetricsRegistry> = OnceLock::new();

pub fn init_metrics() -> Result<(), MetricsError> {
    let registry = MetricsRegistry::new()?;
    METRICS.set(registry).map_err(|_| MetricsError::AlreadyInitialized)
}

impl MetricsRegistry {
    fn new() -> Result<Self, MetricsError> {
        Ok(Self {
            ws_connected: register_gauge!("hip3_ws_connected", "...")?,
            // ... all other metrics
        })
    }
}

// Usage: Metrics now accessed via METRICS.get()
pub struct Metrics;
impl Metrics {
    pub fn ws_connected() {
        if let Some(m) = METRICS.get() {
            m.ws_connected.set(1.0);
        }
    }
}
```

**Solution for config.rs:**

```rust
// Before
pub fn subscription_targets(&self) -> Vec<SubscriptionTarget> {
    self.markets
        .as_ref()
        .expect("Markets not set - run preflight first")  // PANIC
        .iter()
        // ...
}

// After: Return Result
pub fn subscription_targets(&self) -> Result<Vec<SubscriptionTarget>, ConfigError> {
    let markets = self.markets.as_ref()
        .ok_or(ConfigError::MarketsNotConfigured)?;
    Ok(markets.iter().map(/* ... */).collect())
}
```

**Estimated Effort:** 1-2 days

---

#### P0-3: Add Tests for app.rs (1141 lines, 0 tests)

**Current State:** The largest file in the project has no unit tests.

**Test Strategy:**
```rust
// crates/hip3-bot/src/app.rs or tests/app_tests.rs

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: Create minimal AppConfig for testing
    fn test_config() -> AppConfig {
        AppConfig {
            mode: OperatingMode::Observation,
            ws_url: "ws://localhost:8080".to_string(),
            markets: Some(vec![MarketConfig {
                asset_idx: 0,
                coin: "TEST".to_string(),
            }]),
            ..Default::default()
        }
    }

    #[test]
    fn test_application_new_observation_mode() {
        let config = test_config();
        let app = Application::new(config);
        assert!(app.is_ok());
    }

    #[test]
    fn test_preflight_requires_markets_for_trading() {
        let mut config = test_config();
        config.mode = OperatingMode::Trading;
        config.markets = None;

        let mut app = Application::new(config).unwrap();
        let result = tokio_test::block_on(app.run_preflight());

        assert!(matches!(result, Err(AppError::Preflight(_))));
    }

    #[test]
    fn test_coin_to_market_mapping() {
        let config = test_config();
        let app = Application::new(config).unwrap();

        let market = app.coin_to_market("TEST");
        assert!(market.is_some());

        let unknown = app.coin_to_market("UNKNOWN");
        assert!(unknown.is_none());
    }

    #[tokio::test]
    async fn test_handle_order_update_missing_cloid() {
        let config = test_config();
        let app = Application::new(config).unwrap();

        let update = OrderUpdatePayload {
            order: OrderInfo {
                cloid: None,  // Missing cloid
                oid: 123,
                // ...
            },
            status: "open".to_string(),
        };

        // Should log warning, not panic
        app.handle_order_update(&update);
    }
}
```

**Estimated Effort:** 2 days

---

### Priority P1 (High - Should Fix)

#### P1-1: Decompose Large Files

**Current State:**

| File | Lines | Recommended Split |
|------|-------|-------------------|
| gates.rs | 1182 | Move tests to `gates/tests.rs` |
| app.rs | 1141 | Split into `preflight.rs`, `event_loop.rs`, `shutdown.rs` |
| tracker.rs | 1098 | Split into `actor.rs`, `handle.rs` |
| executor.rs | 1065 | Extract `market_state_cache.rs` |
| time_stop.rs | 926 | Move tests to separate file |
| batch.rs | 874 | Move tests to separate file |
| signer.rs | 804 | Extract `action_input.rs`, `key_manager.rs` |

**Example: Decomposing app.rs**

```
crates/hip3-bot/src/
  app/
    mod.rs           # Re-exports
    application.rs   # Application struct + new()
    preflight.rs     # run_preflight() + market discovery
    event_loop.rs    # run() + message handling
    shutdown.rs      # Graceful shutdown logic
    handlers.rs      # handle_order_update, handle_user_fill
    followup.rs      # schedule_followups, capture_followup
```

**Example mod.rs:**
```rust
// crates/hip3-bot/src/app/mod.rs
mod application;
mod event_loop;
mod followup;
mod handlers;
mod preflight;
mod shutdown;

pub use application::Application;
```

**Estimated Effort:** 3-4 days

---

#### P1-2: Extract Hardcoded Constants to Configuration

**Current State:** Multiple hardcoded values scattered across files.

**Constants to Extract:**

```rust
// Create: crates/hip3-core/src/constants.rs

/// Application constants (can be overridden via config)
pub mod defaults {
    use std::time::Duration;

    // app.rs
    pub const DAILY_STATS_INTERVAL: Duration = Duration::from_secs(3600);
    pub const FOLLOWUP_OFFSETS_MS: [u64; 3] = [1000, 3000, 5000];
    pub const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(30);
    pub const POSITION_TRACKER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
    pub const WS_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

    // time_stop.rs
    pub const TIME_STOP_MS: u64 = 30_000;
    pub const REDUCE_ONLY_TIMEOUT_MS: u64 = 60_000;

    // nonce.rs
    pub const DRIFT_WARN_THRESHOLD_MS: u64 = 2000;
    pub const DRIFT_ERROR_THRESHOLD_MS: u64 = 5000;

    // risk.rs
    pub const REJECTED_RESET_TIME_SECS: u64 = 3600;

    // subscription.rs
    pub const BBO_TIMEOUT_SECS: u64 = 10;
    pub const MAX_DATA_AGE_SECS: u64 = 8;

    // registry/client.rs
    pub const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

    // connection.rs
    pub const RECONNECT_WAIT_MS: u64 = 1000;
}
```

**Config Extension:**
```toml
# config/default.toml

[timeouts]
preflight_secs = 30
position_tracker_shutdown_secs = 5
ws_shutdown_secs = 5

[trading]
time_stop_ms = 30000
reduce_only_timeout_ms = 60000

[monitoring]
daily_stats_interval_secs = 3600
followup_offsets_ms = [1000, 3000, 5000]

[nonce]
drift_warn_threshold_ms = 2000
drift_error_threshold_ms = 5000
```

**Estimated Effort:** 1 day

---

#### P1-3: Fix Magic Numbers

**Current State:**

| File:Line | Code | Issue |
|-----------|------|-------|
| gates.rs:346 | `threshold * 2` | Magic multiplier |
| gates.rs:355 | `Decimal::new(2, 1)` | 0.2 factor unexplained |
| gates.rs:385 | `Decimal::new(95, 2)` | 95% threshold |
| signal.rs:28-30 | `Decimal::from(5), Decimal::from(15)` | Edge thresholds |

**Solution:**
```rust
// gates.rs
const SPREAD_SHOCK_BLOCK_MULTIPLIER: u32 = 2;
const SPREAD_SHOCK_REDUCE_FACTOR: Decimal = Decimal::new(2, 1);  // 0.2 = 1/5
const OI_WARNING_THRESHOLD_PCT: Decimal = Decimal::new(95, 2);   // 95%

// Usage:
if spread_bps > threshold * Decimal::from(SPREAD_SHOCK_BLOCK_MULTIPLIER) {
    return GateResult::Block(/* ... */);
}
```

**Estimated Effort:** 0.5 days

---

### Priority P2 (Medium - Nice to Have)

#### P2-1: Add Unit Tests for Low-Coverage Modules

**Target Files:**

| File | Current Tests | Target |
|------|---------------|--------|
| tracker.rs (1098 lines) | 2 tests | 15+ tests |
| connection.rs (596 lines) | 1 test | 10+ tests |
| telemetry/logging.rs | 0 tests | 5+ tests |
| telemetry/metrics.rs | 0 tests | 3+ tests |
| telemetry/daily_stats.rs | 0 tests | 5+ tests |

**Example Tests for tracker.rs:**

```rust
#[cfg(test)]
mod additional_tests {
    use super::*;

    #[tokio::test]
    async fn test_position_flip_from_long_to_short() {
        let (handle, _join) = spawn_position_tracker(100);
        let market = sample_market();

        // Open long position: 0.5 BTC
        handle.fill(market, OrderSide::Buy, Price::new(dec!(50000)),
                    Size::new(dec!(0.5)), 1000).await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Sell 0.7 BTC (flip to short)
        handle.fill(market, OrderSide::Sell, Price::new(dec!(51000)),
                    Size::new(dec!(0.7)), 2000).await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let positions = handle.positions_snapshot();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].side, OrderSide::Sell);
        assert_eq!(positions[0].size, Size::new(dec!(0.2)));

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_snapshot_buffering() {
        let (handle, _join) = spawn_position_tracker(100);
        let market = sample_market();

        // Start snapshot
        handle.snapshot_start().await;

        // These should be buffered
        handle.fill(market, OrderSide::Buy, Price::new(dec!(50000)),
                    Size::new(dec!(0.1)), 1000).await;

        // Position should not exist yet (buffered)
        assert!(!handle.has_position(&market));

        // End snapshot - messages should be applied
        handle.snapshot_end().await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        assert!(handle.has_position(&market));

        handle.shutdown().await;
    }
}
```

**Estimated Effort:** 3 days

---

#### P2-2: Add README and Documentation

**Files to Create:**

```
README.md                           # Project overview, quick start
docs/
  architecture.md                   # System architecture diagram
  configuration.md                  # All config options explained
  deployment.md                     # Production deployment guide
  troubleshooting.md               # Common issues and solutions

crates/hip3-bot/README.md          # Main bot crate
crates/hip3-executor/README.md     # Execution engine
crates/hip3-position/README.md     # Position tracking
crates/hip3-risk/README.md         # Risk gates
crates/hip3-ws/README.md           # WebSocket client
```

**README.md Template:**

```markdown
# hip3_botv2

Oracle Dislocation Taker for Hyperliquid xyz markets.

## Features

- Real-time oracle vs market price monitoring
- Sub-second signal detection
- Risk gate protection (freshness, spread shock, position limits)
- IOC order execution via WebSocket

## Quick Start

\`\`\`bash
# Clone and build
git clone ...
cargo build --release

# Configure
cp config/example.toml config/default.toml
# Edit config/default.toml with your settings

# Run (observation mode)
./target/release/hip3-bot

# Run (trading mode)
export HIP3_TRADING_KEY="0x..."
export HIP3_CONFIG="config/mainnet.toml"
./target/release/hip3-bot
\`\`\`

## Architecture

\`\`\`
hip3_botv2/
  crates/
    hip3-bot/       # Main application
    hip3-core/      # Shared types
    hip3-detector/  # Signal detection
    hip3-executor/  # Order execution
    hip3-feed/      # Market data parsing
    hip3-position/  # Position tracking
    hip3-risk/      # Risk gates
    hip3-ws/        # WebSocket client
\`\`\`

## License

Proprietary
```

**Estimated Effort:** 2 days

---

#### P2-3: Add Doc Comments to Public Functions

**Target Functions (Missing Docs):**

```rust
// gates.rs:91-98
impl GateResult {
    /// Returns true if the gate check passed.
    #[must_use]
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Pass)
    }

    /// Returns true if the gate check blocked the order.
    #[must_use]
    pub fn is_block(&self) -> bool {
        matches!(self, Self::Block(_))
    }
}

// subscription.rs - add docs to all pub fn
/// Check if all required subscriptions have received initial data.
///
/// Returns `true` when:
/// - All BBO subscriptions have received at least one update
/// - All AssetCtx subscriptions have received at least one update
/// - Order updates subscription is confirmed (if user_address configured)
pub fn is_ready(&self) -> bool {
    // ...
}
```

**Estimated Effort:** 1 day

---

#### P2-4: Remove Duplicate Constants

**Current State:**
- `REDUCE_ONLY_TIMEOUT_MS` defined in both `time_stop.rs:27` and `flatten.rs:13`

**Solution:**
```rust
// Move to hip3-core/src/constants.rs
pub const REDUCE_ONLY_TIMEOUT_MS: u64 = 60_000;

// Import in both files
use hip3_core::constants::REDUCE_ONLY_TIMEOUT_MS;
```

**Estimated Effort:** 0.5 hours

---

#### P2-5: Address TODO/FIXME Comments

**Current Items:**

| File:Line | Comment | Resolution |
|-----------|---------|------------|
| app.rs:462 | `P1-3: Phase B TODO - Add periodic spec refresh task` | Implement or create tracking issue |
| signer.rs:107 | `TODO: Set separately if needed` | Document why observation_address is always ZERO |

**Solution for signer.rs:107:**
```rust
// Before
observation_address: Address::ZERO, // TODO: Set separately if needed

// After
/// Observation address (not used in current implementation).
/// The bot uses the trading address for all operations.
/// Reserved for future multi-address support.
observation_address: Address::ZERO,
```

**Estimated Effort:** 0.5 days

---

### Priority P3 (Low - Future Improvements)

#### P3-1: Add Module-Level Documentation

**Files Missing `//!` Module Docs:**
- `hip3-persistence/src/lib.rs`
- `hip3-feed/src/lib.rs`

**Example:**
```rust
//! Market data feed parsing for Hyperliquid WebSocket messages.
//!
//! This crate provides:
//! - JSON parsing for BBO and AssetCtx updates
//! - Coin name to AssetId mapping
//! - Market state aggregation
//!
//! # Example
//!
//! ```rust,ignore
//! use hip3_feed::{MessageParser, MarketEvent};
//!
//! let mut parser = MessageParser::new();
//! parser.add_coin_mapping("BTC".to_string(), 0);
//!
//! if let Some(event) = parser.parse_channel_message("bbo:BTC", &json_data)? {
//!     match event {
//!         MarketEvent::BboUpdate { key, bbo } => { /* ... */ }
//!         MarketEvent::CtxUpdate { key, ctx } => { /* ... */ }
//!     }
//! }
//! ```
```

**Estimated Effort:** 1 day

---

## Implementation Timeline

### Week 1: Critical Fixes
| Day | Task | Priority |
|-----|------|----------|
| 1 | Set up integration test framework | P0-1 |
| 2 | Write WS connection integration tests | P0-1 |
| 3 | Write signal flow integration tests | P0-1 |
| 4 | Fix `unwrap()`/`expect()` in hot paths | P0-2 |
| 5 | Add app.rs unit tests | P0-3 |

### Week 2: Refactoring
| Day | Task | Priority |
|-----|------|----------|
| 1-2 | Decompose app.rs | P1-1 |
| 3 | Decompose gates.rs (extract tests) | P1-1 |
| 4 | Extract constants to config | P1-2 |
| 5 | Fix magic numbers | P1-3 |

### Week 3: Documentation & Polish
| Day | Task | Priority |
|-----|------|----------|
| 1 | Add unit tests for tracker.rs | P2-1 |
| 2 | Add unit tests for connection.rs | P2-1 |
| 3 | Write README.md and architecture docs | P2-2 |
| 4 | Add doc comments to public functions | P2-3 |
| 5 | Address TODO/FIXME, remove duplicates | P2-4, P2-5 |

---

## Expected State After Improvements

### Score Projection: 10/10

| Category | Before | After | Notes |
|----------|--------|-------|-------|
| Architecture | 2.0 | 2.0 | Already excellent |
| Safety | 1.8 | 2.0 | No production `unwrap()` |
| Test Coverage | 1.0 | 2.0 | 90%+ coverage |
| Code Quality | 1.7 | 2.0 | No file >500 lines |
| Documentation | 1.0 | 2.0 | Complete README/docs |
| Maintainability | 1.0 | 2.0 | Clean constants, no duplication |

### Test Coverage Target

| Crate | Before | After |
|-------|--------|-------|
| hip3-bot | ~20% | 80%+ |
| hip3-executor | ~60% | 90%+ |
| hip3-position | ~50% | 85%+ |
| hip3-risk | ~70% | 90%+ |
| hip3-ws | ~30% | 80%+ |
| hip3-telemetry | 0% | 70%+ |

### File Size Target

| File | Before | After |
|------|--------|-------|
| gates.rs | 1182 | ~600 (tests extracted) |
| app.rs | 1141 | ~400 (split into modules) |
| tracker.rs | 1098 | ~500 (actor/handle split) |
| executor.rs | 1065 | ~700 (cache extracted) |

---

## Appendix: Quick Wins (Can Do Today)

1. **Remove duplicate `REDUCE_ONLY_TIMEOUT_MS`** (5 minutes)
2. **Add `#[must_use]` to all `is_*()` methods** (10 minutes)
3. **Fix `connection.rs:581` unwrap** (5 minutes):
   ```rust
   // Before
   (nanos % 1000) as u64

   // After (already correct, but add safety comment)
   // SAFETY: subsec_nanos() always returns value < 1_000_000_000
   (nanos % 1000) as u64
   ```
4. **Add module doc to `hip3-feed/src/lib.rs`** (15 minutes)
5. **Document `observation_address` TODO in signer.rs** (5 minutes)

---

## Conclusion

hip3_botv2 has a solid foundation with excellent architecture and safety-first design. The path to 10/10 primarily involves:

1. **Comprehensive testing** - The biggest gap
2. **Code decomposition** - Large files need splitting
3. **Configuration extraction** - Magic numbers and hardcoded values
4. **Documentation** - README and public API docs

Total estimated effort: **3 weeks** of focused work.

The codebase is production-ready for its current scope, but these improvements will ensure long-term maintainability and confidence in changes.
