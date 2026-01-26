//! Nonce manager for order submission with monotonic guarantees.
//!
//! Provides unique, monotonically increasing nonces that track server time
//! while maintaining ordering guarantees even under clock drift.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use thiserror::Error;

/// Error types for nonce management.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum NonceError {
    /// Time drift between local and server clocks exceeds acceptable threshold.
    #[error("time drift too large: {0}ms")]
    TimeDriftTooLarge(i64),
}

/// Trait for obtaining current time, enabling testability.
pub trait Clock: Send + Sync {
    /// Returns current time in milliseconds since Unix epoch.
    fn now_ms(&self) -> u64;
}

/// System clock implementation using real time.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before Unix epoch")
            .as_millis() as u64
    }
}

/// Manages nonce generation with monotonic and server-time-tracking guarantees.
///
/// # Guarantees
/// - Nonces are always monotonically increasing
/// - Nonces track approximate server time when synced
/// - Thread-safe for concurrent access
///
/// # Offset Convention
/// `server_offset_ms = server_time - local_time`
/// - Positive: server clock is ahead of local
/// - Negative: server clock is behind local
pub struct NonceManager<C: Clock> {
    /// Last issued nonce (monotonically increasing counter).
    counter: AtomicU64,
    /// Offset: server_time - local_time (positive = server ahead).
    server_offset_ms: AtomicI64,
    /// Last sync timestamp in local time.
    last_sync_ms: AtomicU64,
    /// Clock source for current time.
    clock: C,
}

impl<C: Clock> NonceManager<C> {
    /// Threshold for warning about time drift (2 seconds).
    const DRIFT_WARN_THRESHOLD_MS: i64 = 2000;
    /// Threshold for error on time drift (5 seconds).
    const DRIFT_ERROR_THRESHOLD_MS: i64 = 5000;

    /// Creates a new `NonceManager` with the given clock.
    ///
    /// The counter is initialized to the current Unix timestamp in milliseconds,
    /// ensuring nonces start at a reasonable value (not zero).
    #[must_use]
    pub fn new(clock: C) -> Self {
        let now = clock.now_ms();
        Self {
            counter: AtomicU64::new(now),
            server_offset_ms: AtomicI64::new(0),
            last_sync_ms: AtomicU64::new(0),
            clock,
        }
    }

    /// Returns approximate server time based on local time and known offset.
    ///
    /// `approx_server_time = local_time + server_offset`
    #[must_use]
    pub fn approx_server_time_ms(&self) -> u64 {
        let local = self.clock.now_ms();
        let offset = self.server_offset_ms.load(Ordering::Acquire);
        if offset >= 0 {
            local.saturating_add(offset as u64)
        } else {
            local.saturating_sub(offset.unsigned_abs())
        }
    }

    /// Generates the next nonce value.
    ///
    /// Returns `max(last_nonce + 1, approx_server_time_ms())`, ensuring:
    /// - Monotonic increase (never returns a value <= previous)
    /// - Tracks server time when possible
    ///
    /// Thread-safe via CAS loop.
    pub fn next(&self) -> u64 {
        let target = self.approx_server_time_ms();

        loop {
            let current = self.counter.load(Ordering::Acquire);
            let next_val = current.saturating_add(1).max(target);

            match self.counter.compare_exchange_weak(
                current,
                next_val,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return next_val,
                Err(_) => continue,
            }
        }
    }

    /// Synchronizes with server time and updates the offset.
    ///
    /// # Arguments
    /// * `server_time_ms` - Server's current time in milliseconds
    ///
    /// # Errors
    /// Returns `NonceError::TimeDriftTooLarge` if drift exceeds 5 seconds.
    ///
    /// # Warnings
    /// Logs a warning if drift exceeds 2 seconds but is under 5 seconds.
    pub fn sync_with_server(&self, server_time_ms: u64) -> Result<(), NonceError> {
        let local_time = self.clock.now_ms();

        // Calculate offset: server - local (positive = server ahead)
        let offset = if server_time_ms >= local_time {
            (server_time_ms - local_time) as i64
        } else {
            -((local_time - server_time_ms) as i64)
        };

        // Check drift thresholds
        let abs_offset = offset.abs();
        if abs_offset > Self::DRIFT_ERROR_THRESHOLD_MS {
            return Err(NonceError::TimeDriftTooLarge(offset));
        }

        if abs_offset > Self::DRIFT_WARN_THRESHOLD_MS {
            tracing::warn!(
                offset_ms = offset,
                "significant time drift detected with server"
            );
        }

        // Update offset and sync timestamp
        self.server_offset_ms.store(offset, Ordering::Release);
        self.last_sync_ms.store(local_time, Ordering::Release);

        // Fast-forward counter if server time is ahead
        self.fast_forward_counter(server_time_ms);

        Ok(())
    }

    /// Fast-forwards the counter to at least the given value.
    fn fast_forward_counter(&self, min_value: u64) {
        loop {
            let current = self.counter.load(Ordering::Acquire);
            if current >= min_value {
                break;
            }

            match self.counter.compare_exchange_weak(
                current,
                min_value,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(_) => continue,
            }
        }
    }

    /// Returns the current server offset in milliseconds.
    #[must_use]
    pub fn server_offset_ms(&self) -> i64 {
        self.server_offset_ms.load(Ordering::Acquire)
    }

    /// Returns the last sync timestamp in local time.
    #[must_use]
    pub fn last_sync_ms(&self) -> u64 {
        self.last_sync_ms.load(Ordering::Acquire)
    }
}

impl NonceManager<SystemClock> {
    /// Creates a new `NonceManager` with the system clock.
    #[must_use]
    pub fn with_system_clock() -> Self {
        Self::new(SystemClock)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use std::thread;

    use super::*;

    /// Mock clock for testing with controllable time.
    struct MockClock {
        time_ms: AtomicU64,
    }

    impl MockClock {
        fn new(initial_ms: u64) -> Self {
            Self {
                time_ms: AtomicU64::new(initial_ms),
            }
        }

        fn set(&self, time_ms: u64) {
            self.time_ms.store(time_ms, Ordering::Release);
        }

        fn advance(&self, delta_ms: u64) {
            self.time_ms.fetch_add(delta_ms, Ordering::AcqRel);
        }
    }

    impl Clock for MockClock {
        fn now_ms(&self) -> u64 {
            self.time_ms.load(Ordering::Acquire)
        }
    }

    impl Clock for Arc<MockClock> {
        fn now_ms(&self) -> u64 {
            self.time_ms.load(Ordering::Acquire)
        }
    }

    const BASE_TIME: u64 = 1_700_000_000_000; // ~2023-11-14

    #[test]
    fn test_monotonic_increase() {
        let clock = MockClock::new(BASE_TIME);
        let manager = NonceManager::new(clock);

        let mut prev = 0u64;
        for _ in 0..1000 {
            let nonce = manager.next();
            assert!(nonce > prev, "nonce must be strictly increasing");
            prev = nonce;
        }
    }

    #[test]
    fn test_concurrent_no_duplicates() {
        let clock = Arc::new(MockClock::new(BASE_TIME));
        let manager = Arc::new(NonceManager::new(Arc::clone(&clock)));

        let num_threads = 8;
        let iterations_per_thread = 1000;

        let handles: Vec<_> = (0..num_threads)
            .map(|_| {
                let manager = Arc::clone(&manager);
                thread::spawn(move || {
                    let mut nonces = Vec::with_capacity(iterations_per_thread);
                    for _ in 0..iterations_per_thread {
                        nonces.push(manager.next());
                    }
                    nonces
                })
            })
            .collect();

        let mut all_nonces: Vec<u64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();

        all_nonces.sort_unstable();
        let original_len = all_nonces.len();
        all_nonces.dedup();

        assert_eq!(
            all_nonces.len(),
            original_len,
            "all nonces must be unique across threads"
        );
    }

    #[test]
    fn test_clock_regression_no_decrease() {
        let clock = MockClock::new(BASE_TIME);
        let manager = NonceManager::new(clock);

        // Get some nonces
        let n1 = manager.next();
        let n2 = manager.next();

        // Regress the clock by 10 seconds
        manager.clock.set(BASE_TIME - 10_000);

        // Nonces must still increase
        let n3 = manager.next();
        let n4 = manager.next();

        assert!(n2 > n1);
        assert!(n3 > n2, "nonce must not decrease when clock regresses");
        assert!(n4 > n3);
    }

    #[test]
    fn test_sync_fast_forward() {
        let clock = MockClock::new(BASE_TIME);
        let manager = NonceManager::new(clock);

        // Counter starts at BASE_TIME
        let n1 = manager.next();
        assert!(n1 >= BASE_TIME);

        // Sync with server time 1 second ahead
        let server_time = BASE_TIME + 1000;
        manager.sync_with_server(server_time).unwrap();

        // Counter should fast-forward to at least server_time
        let n2 = manager.next();
        assert!(
            n2 >= server_time,
            "counter should fast-forward to server time"
        );
    }

    #[test]
    fn test_drift_warn_threshold() {
        // This test verifies that a warning is logged for drift > 2s but <= 5s
        // We can't easily verify tracing output in unit tests, but we verify
        // the function succeeds without error

        let clock = MockClock::new(BASE_TIME);
        let manager = NonceManager::new(clock);

        // Server ahead by 2001ms (just over warn threshold)
        let server_time = BASE_TIME + 2001;
        let result = manager.sync_with_server(server_time);

        assert!(result.is_ok(), "drift of 2001ms should not error");
        assert_eq!(manager.server_offset_ms(), 2001);
    }

    #[test]
    fn test_drift_error_threshold() {
        let clock = MockClock::new(BASE_TIME);
        let manager = NonceManager::new(clock);

        // Server ahead by 5001ms (over error threshold)
        let server_time = BASE_TIME + 5001;
        let result = manager.sync_with_server(server_time);

        assert!(matches!(result, Err(NonceError::TimeDriftTooLarge(5001))));
    }

    #[test]
    fn test_drift_error_negative() {
        let clock = MockClock::new(BASE_TIME);
        let manager = NonceManager::new(clock);

        // Server behind by 5001ms (negative drift over error threshold)
        let server_time = BASE_TIME - 5001;
        let result = manager.sync_with_server(server_time);

        assert!(matches!(result, Err(NonceError::TimeDriftTooLarge(-5001))));
    }

    #[test]
    fn test_nonce_tracks_server_time() {
        let clock = MockClock::new(BASE_TIME);
        let manager = NonceManager::new(clock);

        // Initial nonces should be around BASE_TIME
        let n1 = manager.next();
        assert!(n1 >= BASE_TIME && n1 < BASE_TIME + 100);

        // Advance clock by 5 seconds
        manager.clock.advance(5000);

        // Drain any accumulated difference
        for _ in 0..10 {
            manager.next();
        }

        // Now nonces should be near BASE_TIME + 5000
        let n2 = manager.next();
        let expected_min = BASE_TIME + 5000;

        assert!(
            n2 >= expected_min,
            "nonce should track server time: got {n2}, expected >= {expected_min}"
        );
    }

    #[test]
    fn test_approx_server_time_with_positive_offset() {
        let clock = MockClock::new(BASE_TIME);
        let manager = NonceManager::new(clock);

        // Sync with server that is 500ms ahead
        manager.sync_with_server(BASE_TIME + 500).unwrap();

        let approx = manager.approx_server_time_ms();
        // Should be local_time + offset = BASE_TIME + 500
        assert_eq!(approx, BASE_TIME + 500);
    }

    #[test]
    fn test_approx_server_time_with_negative_offset() {
        let clock = MockClock::new(BASE_TIME);
        let manager = NonceManager::new(clock);

        // Sync with server that is 500ms behind
        manager.sync_with_server(BASE_TIME - 500).unwrap();

        let approx = manager.approx_server_time_ms();
        // Should be local_time + offset = BASE_TIME - 500
        assert_eq!(approx, BASE_TIME - 500);
    }

    #[test]
    fn test_new_initializes_counter_to_current_time() {
        let clock = MockClock::new(BASE_TIME);
        let manager = NonceManager::new(clock);

        let n1 = manager.next();
        // First nonce should be BASE_TIME + 1 (max of counter+1 and approx_time)
        assert!(
            n1 >= BASE_TIME,
            "initial counter should be based on current time"
        );
    }
}
