//! View-model logic: size formatting, banner text and the scan state machine.
//! Plain functions, no Slint types, so it is unit tested.

use scan::{Method, ScanError, ScanStats};
use std::time::Duration;

/// What the banner needs from a finished scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub method: Method,
    pub files: u64,
    pub dirs: u64,
    pub skipped: u64,
    pub elapsed: Duration,
    /// Size of the scan root in bytes.
    pub total: u64,
}

impl Summary {
    pub fn new(stats: &ScanStats, total: u64) -> Self {
        let _ = (stats, total);
        unimplemented!("Not implemented")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanState {
    Idle,
    Scanning { files: u64, dirs: u64 },
    Done(Summary),
    Error(String),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Start,
    Progress { files: u64, dirs: u64 },
    Finished(Summary),
    Failed(String),
    Cancelled,
}

/// Next state. An event that is not valid in the current state is ignored.
pub fn next(state: ScanState, event: Event) -> ScanState {
    let _ = (state, event);
    unimplemented!("Not implemented")
}

/// Binary units, one decimal above 1 KB: `1.5 MB`.
pub fn format_size(bytes: u64) -> String {
    let _ = bytes;
    unimplemented!("Not implemented")
}

/// `1234567` -> `1,234,567`.
pub fn group_digits(n: u64) -> String {
    let _ = n;
    unimplemented!("Not implemented")
}

pub fn format_elapsed(d: Duration) -> String {
    let _ = d;
    unimplemented!("Not implemented")
}

/// Short, plain message for a failed scan. Never empty.
pub fn error_message(e: &ScanError) -> String {
    let _ = e;
    unimplemented!("Not implemented")
}

/// Text for the status banner in each state. Never empty.
pub fn banner_text(state: &ScanState) -> String {
    let _ = state;
    unimplemented!("Not implemented")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn summary(method: Method, skipped: u64) -> Summary {
        Summary { method, files: 1234, dirs: 56, skipped, elapsed: Duration::from_millis(2500), total: 3 * 1024 * 1024 }
    }

    // ---- format_size ----
    #[test]
    fn format_size_known_values_formats_binary_units() {
        let cases: [(u64, &str); 9] = [
            (0, "0 B"),
            (1023, "1023 B"),
            (1024, "1.0 KB"),
            (1536, "1.5 KB"),
            (1024 * 1024, "1.0 MB"),
            (5 * 1024 * 1024 * 1024 / 2, "2.5 GB"),
            (1024u64.pow(4), "1.0 TB"),
            (1024u64.pow(5), "1.0 PB"),
            (u64::MAX, "16.0 EB"),
        ];
        for (n, want) in cases {
            assert_eq!(format_size(n), want, "input {n}");
        }
    }

    // ---- group_digits ----
    #[test]
    fn group_digits_known_values_inserts_commas() {
        let cases: [(u64, &str); 5] =
            [(0, "0"), (999, "999"), (1000, "1,000"), (1234567, "1,234,567"), (u64::MAX, "18,446,744,073,709,551,615")];
        for (n, want) in cases {
            assert_eq!(group_digits(n), want, "input {n}");
        }
    }

    // ---- format_elapsed ----
    #[test]
    fn format_elapsed_known_values_formats_seconds_and_minutes() {
        let cases: [(u64, &str); 5] =
            [(0, "0.0 s"), (250, "0.3 s"), (2500, "2.5 s"), (59_900, "59.9 s"), (125_000, "2 min 5 s")];
        for (ms, want) in cases {
            assert_eq!(format_elapsed(Duration::from_millis(ms)), want, "input {ms} ms");
        }
    }

    // ---- error_message ----
    #[test]
    fn error_message_permission_denied_says_access_denied() {
        let e = ScanError::Io(io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(error_message(&e), "Access denied. Choose a different folder.");
    }

    #[test]
    fn error_message_not_found_says_folder_missing() {
        let e = ScanError::Io(io::Error::from(io::ErrorKind::NotFound));
        assert_eq!(error_message(&e), "Folder not found.");
    }

    #[test]
    fn error_message_other_io_includes_detail() {
        let e = ScanError::Io(io::Error::other("disk exploded"));
        assert!(error_message(&e).contains("disk exploded"));
    }

    #[test]
    fn error_message_no_root_is_not_empty() {
        assert!(!error_message(&ScanError::NoRoot).is_empty());
    }

    // ---- banner_text ----
    #[test]
    fn banner_text_idle_tells_user_what_to_do() {
        assert_eq!(banner_text(&ScanState::Idle), "Choose a folder or drive. Then select Scan.");
    }

    #[test]
    fn banner_text_scanning_shows_counts() {
        let t = banner_text(&ScanState::Scanning { files: 1234, dirs: 5 });
        assert_eq!(t, "Scanning. 1,234 files, 5 folders so far.");
    }

    #[test]
    fn banner_text_done_mft_shows_method_counts_size_and_time() {
        let t = banner_text(&ScanState::Done(summary(Method::Mft, 0)));
        assert_eq!(t, "Fast scan (MFT). 3.0 MB in 1,234 files and 56 folders. 0 skipped. 2.5 s.");
    }

    #[test]
    fn banner_text_done_walk_says_standard_scan_and_why_slower() {
        let t = banner_text(&ScanState::Done(summary(Method::Walk, 0)));
        assert_eq!(
            t,
            "Standard scan. 3.0 MB in 1,234 files and 56 folders. 0 skipped. 2.5 s. Restart as administrator for a faster scan."
        );
    }

    #[test]
    fn banner_text_done_with_skipped_shows_skipped_count() {
        let t = banner_text(&ScanState::Done(summary(Method::Walk, 7)));
        assert!(t.contains("7 skipped."), "{t}");
    }

    #[test]
    fn banner_text_error_prefixes_message() {
        assert_eq!(banner_text(&ScanState::Error("Access denied.".into())), "Scan failed. Access denied.");
    }

    #[test]
    fn banner_text_error_with_empty_message_is_still_not_blank() {
        assert_eq!(banner_text(&ScanState::Error(String::new())), "Scan failed.");
    }

    #[test]
    fn banner_text_cancelled_says_no_result() {
        assert_eq!(banner_text(&ScanState::Cancelled), "Scan stopped. There is no result.");
    }

    // ---- Summary ----
    #[test]
    fn summary_new_copies_stats_and_total() {
        let stats = ScanStats { files: 4, dirs: 2, skipped: 1, elapsed: Duration::from_secs(3), method: Method::Walk };
        let s = Summary::new(&stats, 99);
        assert_eq!(
            s,
            Summary { method: Method::Walk, files: 4, dirs: 2, skipped: 1, elapsed: Duration::from_secs(3), total: 99 }
        );
    }

    // ---- state machine: every (state, event) pair ----
    fn states() -> Vec<ScanState> {
        vec![
            ScanState::Idle,
            ScanState::Scanning { files: 1, dirs: 1 },
            ScanState::Done(summary(Method::Mft, 0)),
            ScanState::Error("x".into()),
            ScanState::Cancelled,
        ]
    }

    fn not_scanning() -> Vec<ScanState> {
        states().into_iter().filter(|s| !matches!(s, ScanState::Scanning { .. })).collect()
    }

    #[test]
    fn next_start_from_non_scanning_states_begins_scan_with_zero_counts() {
        for s in not_scanning() {
            assert_eq!(next(s.clone(), Event::Start), ScanState::Scanning { files: 0, dirs: 0 }, "from {s:?}");
        }
    }

    #[test]
    fn next_start_while_scanning_is_ignored() {
        let s = ScanState::Scanning { files: 9, dirs: 9 };
        assert_eq!(next(s.clone(), Event::Start), s);
    }

    #[test]
    fn next_progress_while_scanning_updates_counts() {
        let s = ScanState::Scanning { files: 1, dirs: 1 };
        assert_eq!(next(s, Event::Progress { files: 10, dirs: 3 }), ScanState::Scanning { files: 10, dirs: 3 });
    }

    #[test]
    fn next_progress_outside_scanning_is_ignored() {
        for s in not_scanning() {
            assert_eq!(next(s.clone(), Event::Progress { files: 10, dirs: 3 }), s, "from {s:?}");
        }
    }

    #[test]
    fn next_finished_while_scanning_moves_to_done() {
        let sum = summary(Method::Mft, 2);
        assert_eq!(next(ScanState::Scanning { files: 1, dirs: 1 }, Event::Finished(sum.clone())), ScanState::Done(sum));
    }

    #[test]
    fn next_failed_while_scanning_moves_to_error() {
        let got = next(ScanState::Scanning { files: 1, dirs: 1 }, Event::Failed("bad".into()));
        assert_eq!(got, ScanState::Error("bad".into()));
    }

    #[test]
    fn next_cancelled_while_scanning_moves_to_cancelled() {
        assert_eq!(next(ScanState::Scanning { files: 1, dirs: 1 }, Event::Cancelled), ScanState::Cancelled);
    }

    #[test]
    fn next_finish_events_outside_scanning_are_ignored() {
        let events = [Event::Finished(summary(Method::Walk, 0)), Event::Failed("bad".into()), Event::Cancelled];
        for s in not_scanning() {
            for e in &events {
                assert_eq!(next(s.clone(), e.clone()), s, "{s:?} + {e:?}");
            }
        }
    }
}
