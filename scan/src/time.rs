//! Entry times: whole seconds since 2000-01-01 UTC in a `u32`, `0` = unknown.
//! Unknown is the minimum, so "newest inside" is a plain `max`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// No time known (an unreadable time, an empty folder).
pub const UNKNOWN: u32 = 0;

/// 2000-01-01T00:00:00Z in 100 ns ticks since 1601-01-01 (a Windows FILETIME).
pub const FILETIME_2000: i64 = 125_911_584_000_000_000;

/// Seconds from 1970-01-01 to 2000-01-01.
const UNIX_TO_2000_SECS: u64 = 946_684_800;

/// 100 ns ticks in a second.
const TICKS_PER_SEC: i64 = 10_000_000;

/// A FILETIME as seconds since 2000. `<= 0` is `UNKNOWN`; before 2000-01-01 00:00:01 is `1`;
/// after 2136 is `u32::MAX`.
pub fn from_filetime(ticks: i64) -> u32 {
    if ticks <= 0 {
        return UNKNOWN;
    }
    // Truncating division: a time before 2000 gives zero or less, which the clamp lifts to 1.
    let secs = (ticks - FILETIME_2000) / TICKS_PER_SEC;
    secs.clamp(1, i64::from(u32::MAX)) as u32
}

/// A `SystemTime` as seconds since 2000, with the same clamps as `from_filetime`.
pub fn from_system_time(t: SystemTime) -> u32 {
    match t.duration_since(UNIX_EPOCH + Duration::from_secs(UNIX_TO_2000_SECS)) {
        Ok(d) => d.as_secs().clamp(1, u64::from(u32::MAX)) as u32,
        Err(_) => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: i64 = 10_000_000;

    #[test]
    fn from_filetime_zero_negative_and_min_are_unknown() {
        for ticks in [0, -1, i64::MIN] {
            assert_eq!(from_filetime(ticks), UNKNOWN, "ticks {ticks}");
        }
    }

    #[test]
    fn from_filetime_2000_is_one_never_unknown() {
        assert_eq!(from_filetime(FILETIME_2000), 1);
    }

    #[test]
    fn from_filetime_counts_whole_seconds_and_truncates() {
        let cases = [(SECOND, 1), (2 * SECOND, 2), (3 * SECOND - 1, 2)];

        for (extra, want) in cases {
            assert_eq!(from_filetime(FILETIME_2000 + extra), want, "extra {extra}");
        }
    }

    #[test]
    fn from_filetime_before_2000_is_one() {
        for ticks in [FILETIME_2000 - 1, 1] {
            assert_eq!(from_filetime(ticks), 1, "ticks {ticks}");
        }
    }

    #[test]
    fn from_filetime_after_2136_is_max() {
        for ticks in [FILETIME_2000 + i64::from(u32::MAX) * SECOND, i64::MAX] {
            assert_eq!(from_filetime(ticks), u32::MAX, "ticks {ticks}");
        }
    }

    #[test]
    fn from_system_time_and_from_filetime_agree_on_2026_10_26() {
        let t = UNIX_EPOCH + Duration::from_secs(1_792_972_800);
        let ticks = FILETIME_2000 + 846_288_000 * SECOND;

        assert_eq!(from_system_time(t), 846_288_000);
        assert_eq!(from_filetime(ticks), 846_288_000);
    }

    #[test]
    fn from_system_time_1970_is_one() {
        assert_eq!(from_system_time(UNIX_EPOCH), 1);
    }
}
