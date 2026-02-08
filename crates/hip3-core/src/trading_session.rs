//! Trading session utilities.
//!
//! Provides session classification based on UTC time.
//! Used for:
//! - Weekend MM strategy activation
//! - Session-aware parameter adjustment
//! - Weekend-to-weekday transition timing

use chrono::{Datelike, Timelike, Utc, Weekday};
use serde::{Deserialize, Serialize};

/// Trading session classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TradingSession {
    /// Weekend: Saturday 00:00 UTC – Sunday 21:00 UTC.
    /// Primary MM window. No traditional equity markets open.
    Weekend,
    /// Asian session: Sunday 21:00 – Monday 01:00, or weekday 21:00 – 01:00 UTC.
    Asia,
    /// European session: Weekday 07:00 – 14:30 UTC.
    Europe,
    /// US session: Weekday 14:30 – 21:00 UTC.
    US,
    /// Off-hours: Weekday gaps between major sessions.
    OffHours,
}

impl std::fmt::Display for TradingSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Weekend => write!(f, "Weekend"),
            Self::Asia => write!(f, "Asia"),
            Self::Europe => write!(f, "Europe"),
            Self::US => write!(f, "US"),
            Self::OffHours => write!(f, "OffHours"),
        }
    }
}

/// Check if the current UTC time is within the weekend window.
///
/// Weekend is defined as:
/// - Friday 21:00 UTC (US market close) through Sunday 21:00 UTC
///
/// This is when MM strategy should be active (no equity price discovery).
#[must_use]
pub fn is_weekend_utc() -> bool {
    is_weekend_at(Utc::now())
}

/// Check if a given UTC datetime is within the weekend window.
#[must_use]
pub fn is_weekend_at(dt: chrono::DateTime<Utc>) -> bool {
    let weekday = dt.weekday();
    let hour = dt.hour();

    match weekday {
        Weekday::Sat => true,
        Weekday::Sun => hour < 21,
        Weekday::Fri => hour >= 21,
        _ => false,
    }
}

/// Get the current trading session based on UTC time.
#[must_use]
pub fn current_session() -> TradingSession {
    session_at(Utc::now())
}

/// Get the trading session at a given UTC datetime.
#[must_use]
pub fn session_at(dt: chrono::DateTime<Utc>) -> TradingSession {
    if is_weekend_at(dt) {
        return TradingSession::Weekend;
    }

    let hour = dt.hour();

    // US session: 14:30 – 21:00 UTC (NYSE/NASDAQ open)
    // Simplified: 14 – 21 (we use hour boundaries for simplicity)
    if (14..21).contains(&hour) {
        return TradingSession::US;
    }

    // European session: 07:00 – 14:00 UTC (London open)
    if (7..14).contains(&hour) {
        return TradingSession::Europe;
    }

    // Asian session: 21:00 – 01:00 UTC (overlap with end of US / start of Asia)
    // and 01:00 – 07:00 UTC
    if !(7..21).contains(&hour) {
        // Check if this is actually weekend (already handled above)
        return TradingSession::Asia;
    }

    TradingSession::OffHours
}

/// Check if current time is within the MM shutdown window.
///
/// Returns true during Sunday 21:00 – Monday 00:00 UTC.
/// During this window, MM should cancel all quotes and flatten positions.
#[must_use]
pub fn is_mm_shutdown_window() -> bool {
    is_mm_shutdown_at(Utc::now())
}

/// Check if a given UTC datetime is within the MM shutdown window.
#[must_use]
pub fn is_mm_shutdown_at(dt: chrono::DateTime<Utc>) -> bool {
    let weekday = dt.weekday();
    let hour = dt.hour();

    // Sunday 21:00 – Monday 00:00 UTC
    weekday == Weekday::Sun && hour >= 21
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(year: i32, month: u32, day: u32, hour: u32, min: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, min, 0)
            .unwrap()
    }

    #[test]
    fn test_saturday_is_weekend() {
        // 2026-02-07 is Saturday
        assert!(is_weekend_at(utc(2026, 2, 7, 0, 0)));
        assert!(is_weekend_at(utc(2026, 2, 7, 12, 0)));
        assert!(is_weekend_at(utc(2026, 2, 7, 23, 59)));
    }

    #[test]
    fn test_sunday_before_21_is_weekend() {
        // 2026-02-08 is Sunday
        assert!(is_weekend_at(utc(2026, 2, 8, 0, 0)));
        assert!(is_weekend_at(utc(2026, 2, 8, 15, 0)));
        assert!(is_weekend_at(utc(2026, 2, 8, 20, 59)));
    }

    #[test]
    fn test_sunday_after_21_not_weekend() {
        // Sunday 21:00+ is MM shutdown window, not weekend trading
        assert!(!is_weekend_at(utc(2026, 2, 8, 21, 0)));
        assert!(!is_weekend_at(utc(2026, 2, 8, 23, 0)));
    }

    #[test]
    fn test_friday_after_21_is_weekend() {
        // 2026-02-06 is Friday
        assert!(is_weekend_at(utc(2026, 2, 6, 21, 0)));
        assert!(is_weekend_at(utc(2026, 2, 6, 23, 59)));
    }

    #[test]
    fn test_friday_before_21_not_weekend() {
        assert!(!is_weekend_at(utc(2026, 2, 6, 20, 0)));
        assert!(!is_weekend_at(utc(2026, 2, 6, 14, 30)));
    }

    #[test]
    fn test_weekday_not_weekend() {
        // 2026-02-09 is Monday
        assert!(!is_weekend_at(utc(2026, 2, 9, 12, 0)));
        // 2026-02-10 is Tuesday
        assert!(!is_weekend_at(utc(2026, 2, 10, 8, 0)));
        // 2026-02-11 is Wednesday
        assert!(!is_weekend_at(utc(2026, 2, 11, 15, 0)));
        // 2026-02-12 is Thursday
        assert!(!is_weekend_at(utc(2026, 2, 12, 3, 0)));
    }

    #[test]
    fn test_session_weekend() {
        assert_eq!(session_at(utc(2026, 2, 7, 12, 0)), TradingSession::Weekend);
        assert_eq!(session_at(utc(2026, 2, 8, 15, 0)), TradingSession::Weekend);
    }

    #[test]
    fn test_session_us() {
        // Monday 15:00 UTC = US session
        assert_eq!(session_at(utc(2026, 2, 9, 15, 0)), TradingSession::US);
        assert_eq!(session_at(utc(2026, 2, 9, 20, 0)), TradingSession::US);
    }

    #[test]
    fn test_session_europe() {
        // Monday 10:00 UTC = Europe session
        assert_eq!(session_at(utc(2026, 2, 9, 10, 0)), TradingSession::Europe);
        assert_eq!(session_at(utc(2026, 2, 9, 7, 0)), TradingSession::Europe);
    }

    #[test]
    fn test_session_asia() {
        // Monday 03:00 UTC = Asia session
        assert_eq!(session_at(utc(2026, 2, 9, 3, 0)), TradingSession::Asia);
        // Monday 23:00 UTC = Asia session (overlap)
        assert_eq!(session_at(utc(2026, 2, 9, 23, 0)), TradingSession::Asia);
    }

    #[test]
    fn test_mm_shutdown_window() {
        // Sunday 21:00 – Monday 00:00
        assert!(is_mm_shutdown_at(utc(2026, 2, 8, 21, 0)));
        assert!(is_mm_shutdown_at(utc(2026, 2, 8, 23, 59)));

        // Sunday 20:59 is still weekend, not shutdown
        assert!(!is_mm_shutdown_at(utc(2026, 2, 8, 20, 59)));

        // Monday 00:00 is not shutdown
        assert!(!is_mm_shutdown_at(utc(2026, 2, 9, 0, 0)));

        // Saturday is not shutdown
        assert!(!is_mm_shutdown_at(utc(2026, 2, 7, 21, 0)));
    }

    #[test]
    fn test_trading_session_display() {
        assert_eq!(TradingSession::Weekend.to_string(), "Weekend");
        assert_eq!(TradingSession::US.to_string(), "US");
        assert_eq!(TradingSession::Europe.to_string(), "Europe");
        assert_eq!(TradingSession::Asia.to_string(), "Asia");
        assert_eq!(TradingSession::OffHours.to_string(), "OffHours");
    }
}
