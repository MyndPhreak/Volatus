//! View-model logic: size formatting, banner text and the scan state machine.
//! Plain functions, no Slint types, so it is unit tested.

use scan::{Method, ScanError, ScanStats};
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
        Self {
            method: stats.method,
            files: stats.files,
            dirs: stats.dirs,
            skipped: stats.skipped,
            elapsed: stats.elapsed,
            total,
        }
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
    match (state, event) {
        (ScanState::Scanning { .. }, Event::Progress { files, dirs }) => ScanState::Scanning { files, dirs },
        (ScanState::Scanning { .. }, Event::Finished(s)) => ScanState::Done(s),
        (ScanState::Scanning { .. }, Event::Failed(m)) => ScanState::Error(m),
        (ScanState::Scanning { .. }, Event::Cancelled) => ScanState::Cancelled,
        (s @ ScanState::Scanning { .. }, _) => s,
        (_, Event::Start) => ScanState::Scanning { files: 0, dirs: 0 },
        (s, _) => s,
    }
}

/// The scan the window shows: its state, its id (generation) and its stop flag. UI thread only.
/// Each scan gets a new id and a new flag, so a worker of an older scan can neither change
/// the window nor see the flag of a newer scan.
#[derive(Debug)]
pub struct ScanControl {
    state: ScanState,
    generation: u64,
    cancel: Arc<AtomicBool>,
}

impl Default for ScanControl {
    fn default() -> Self {
        Self { state: ScanState::Idle, generation: 0, cancel: Arc::new(AtomicBool::new(false)) }
    }
}

impl ScanControl {
    pub fn state(&self) -> &ScanState {
        &self.state
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// True if events from scan `generation` still apply.
    pub fn is_current(&self, generation: u64) -> bool {
        generation == self.generation
    }

    fn is_scanning(&self) -> bool {
        matches!(self.state, ScanState::Scanning { .. })
    }

    /// Starts a scan: a new id, a new unset stop flag and state `Scanning`.
    /// `None` while a scan runs.
    pub fn start(&mut self) -> Option<(u64, Arc<AtomicBool>)> {
        if self.is_scanning() {
            return None;
        }
        self.generation += 1;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.state = next(std::mem::replace(&mut self.state, ScanState::Idle), Event::Start);
        Some((self.generation, self.cancel.clone()))
    }

    /// Stops the running scan at once: sets its flag, retires its id and moves to `Cancelled`.
    /// Its worker ends in the background and what it posts later is dropped.
    /// `false`, and no change, if no scan runs.
    /// ponytail: a cancelled scan stops in under a second when nothing else runs, but when a new
    /// scan starts at once both share rayon's global pool, and the old one measured 31-113 s to
    /// return on C: (it ends near the end of the new scan). Until then two scans share the disk,
    /// the CPUs and memory; the old result is dropped. Upgrade path: a thread pool per scan, or
    /// join the old worker before the new scan starts.
    pub fn cancel(&mut self) -> bool {
        if !self.is_scanning() {
            return false;
        }
        self.cancel.store(true, Ordering::Relaxed);
        self.generation += 1;
        self.state = next(std::mem::replace(&mut self.state, ScanState::Idle), Event::Cancelled);
        true
    }

    /// Applies `event` from scan `generation`. `false`, and no change, if that scan is not current.
    pub fn apply(&mut self, generation: u64, event: Event) -> bool {
        if !self.is_current(generation) {
            return false;
        }
        self.state = next(std::mem::replace(&mut self.state, ScanState::Idle), event);
        true
    }

    /// Applies `Finished(summary)` from scan `generation` and gives `result` back for the window
    /// to show. `None`, and no change, if that scan is not current: a late result is never shown.
    pub fn finish<T>(&mut self, generation: u64, summary: Summary, result: T) -> Option<T> {
        self.apply(generation, Event::Finished(summary)).then_some(result)
    }
}

/// Binary units, one decimal above 1 KB: `1.5 MB`.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", UNITS[i])
}

/// `1234567` -> `1,234,567`.
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn format_elapsed(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 59_950 {
        // Integer rounding: f64 formatting rounds ties to even (0.25 -> 0.2).
        let tenths = (ms + 50) / 100;
        format!("{}.{} s", tenths / 10, tenths % 10)
    } else {
        let secs = (ms + 500) / 1000;
        format!("{} min {} s", secs / 60, secs % 60)
    }
}

/// Short, plain message for a failed scan. Never empty.
pub fn error_message(e: &ScanError) -> String {
    match e {
        ScanError::Io(io) if io.kind() == ErrorKind::PermissionDenied => {
            "Access denied. Choose a different folder.".into()
        }
        ScanError::Io(io) if io.kind() == ErrorKind::NotFound => "Folder not found.".into(),
        ScanError::Io(io) => format!("Could not read the folder. {io}"),
        ScanError::NoRoot => "The folder has no root entry.".into(),
        ScanError::Cancelled => "The scan was stopped.".into(),
    }
}

/// True when the "Restart as administrator" button helps: the scan fell back to the walker, the
/// user is not an administrator, and `root` (canonical) is a whole drive. Only a whole drive
/// takes the MFT path, so for a sub-folder the button would change nothing.
pub fn offer_elevation(method: Method, root: &Path, is_admin: bool) -> bool {
    method == Method::Walk && !is_admin && scan::is_volume_root(root)
}

/// Text for the status banner in each state. Never empty.
pub fn banner_text(state: &ScanState) -> String {
    match state {
        ScanState::Idle => "Choose a folder or drive. Then select Scan.".into(),
        ScanState::Scanning { files, dirs } => {
            format!("Scanning. {} files, {} folders so far.", group_digits(*files), group_digits(*dirs))
        }
        ScanState::Done(s) => {
            let (name, hint) = match s.method {
                Method::Mft => ("Fast scan (MFT)", ""),
                Method::Walk => ("Standard scan", " Restart as administrator to scan a whole drive faster."),
            };
            format!(
                "{name}. {} in {} files and {} folders. {} skipped. {}.{hint}",
                format_size(s.total),
                group_digits(s.files),
                group_digits(s.dirs),
                group_digits(s.skipped),
                format_elapsed(s.elapsed),
            )
        }
        ScanState::Error(m) => format!("Scan failed. {m}").trim_end().to_string(),
        ScanState::Cancelled => "Scan stopped. There is no result.".into(),
    }
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
            "Standard scan. 3.0 MB in 1,234 files and 56 folders. 0 skipped. 2.5 s. Restart as administrator to scan a whole drive faster."
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
        let stats = ScanStats {
            files: 4,
            dirs: 2,
            skipped: 1,
            elapsed: Duration::from_secs(3),
            method: Method::Walk,
            phases: Default::default(),
            threads: 1,
        };
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

    // ---- ScanControl: immediate cancel and stale scans ----
    fn progress() -> Event {
        Event::Progress { files: 10, dirs: 3 }
    }

    /// Every event a scan worker can post.
    fn worker_events() -> Vec<Event> {
        vec![progress(), Event::Finished(summary(Method::Walk, 0)), Event::Failed("bad".into()), Event::Cancelled]
    }

    /// A control in each state other than `Scanning`, reached through the public API.
    fn idle_controls() -> Vec<ScanControl> {
        let idle = ScanControl::default();
        let mut done = ScanControl::default();
        let (g, _) = done.start().unwrap();
        done.apply(g, Event::Finished(summary(Method::Mft, 0)));
        let mut failed = ScanControl::default();
        let (g, _) = failed.start().unwrap();
        failed.apply(g, Event::Failed("bad".into()));
        let mut cancelled = ScanControl::default();
        cancelled.start().unwrap();
        cancelled.cancel();
        vec![idle, done, failed, cancelled]
    }

    #[test]
    fn scan_control_start_from_idle_moves_to_scanning_with_zero_counts() {
        let mut c = ScanControl::default();

        c.start().unwrap();

        assert_eq!(c.state(), &ScanState::Scanning { files: 0, dirs: 0 });
    }

    #[test]
    fn scan_control_start_returns_the_current_generation() {
        let mut c = ScanControl::default();

        let (g, _) = c.start().unwrap();

        assert_eq!(c.generation(), g);
        assert!(c.is_current(g));
    }

    #[test]
    fn scan_control_start_gives_an_unset_flag() {
        let mut c = ScanControl::default();

        let (_, flag) = c.start().unwrap();

        assert!(!flag.load(Ordering::Relaxed));
    }

    #[test]
    fn scan_control_start_while_scanning_returns_none_and_keeps_the_scan() {
        let mut c = ScanControl::default();
        let (g, _) = c.start().unwrap();

        let second = c.start();

        assert!(second.is_none());
        assert!(c.is_current(g));
    }

    #[test]
    fn scan_control_cancel_while_scanning_moves_to_cancelled_at_once() {
        let mut c = ScanControl::default();
        c.start().unwrap();

        let changed = c.cancel();

        assert!(changed);
        assert_eq!(c.state(), &ScanState::Cancelled);
    }

    #[test]
    fn scan_control_cancel_while_scanning_sets_the_scan_flag() {
        let mut c = ScanControl::default();
        let (_, flag) = c.start().unwrap();

        c.cancel();

        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn scan_control_cancel_while_scanning_retires_the_generation() {
        let mut c = ScanControl::default();
        let (g, _) = c.start().unwrap();

        c.cancel();

        assert!(!c.is_current(g));
    }

    #[test]
    fn scan_control_cancel_when_not_scanning_changes_nothing() {
        for mut c in idle_controls() {
            let (state, generation) = (c.state().clone(), c.generation());

            let changed = c.cancel();

            assert!(!changed, "from {state:?}");
            assert_eq!((c.state(), c.generation()), (&state, generation), "from {state:?}");
        }
    }

    #[test]
    fn scan_control_late_events_from_a_cancelled_scan_are_dropped() {
        for e in worker_events() {
            let mut c = ScanControl::default();
            let (g, _) = c.start().unwrap();
            c.cancel();

            let applied = c.apply(g, e.clone());

            assert!(!applied, "{e:?}");
            assert_eq!(c.state(), &ScanState::Cancelled, "{e:?}");
        }
    }

    #[test]
    fn scan_control_start_after_cancel_is_accepted_at_once() {
        let mut c = ScanControl::default();
        let (old, _) = c.start().unwrap();
        c.cancel();

        let (new, _) = c.start().expect("a new scan starts right after a cancel");

        assert_ne!(new, old);
        assert_eq!(c.state(), &ScanState::Scanning { files: 0, dirs: 0 });
    }

    #[test]
    fn scan_control_late_events_from_an_old_scan_do_not_change_the_new_scan() {
        for e in worker_events() {
            let mut c = ScanControl::default();
            let (old, _) = c.start().unwrap();
            c.cancel();
            c.start().unwrap();

            let applied = c.apply(old, e.clone());

            assert!(!applied, "{e:?}");
            assert_eq!(c.state(), &ScanState::Scanning { files: 0, dirs: 0 }, "{e:?}");
        }
    }

    #[test]
    fn scan_control_new_scan_after_cancel_gets_a_fresh_flag_and_the_old_flag_stays_set() {
        let mut c = ScanControl::default();
        let (_, old_flag) = c.start().unwrap();
        c.cancel();

        let (_, new_flag) = c.start().unwrap();

        assert!(old_flag.load(Ordering::Relaxed), "the old worker must still see its stop flag");
        assert!(!new_flag.load(Ordering::Relaxed));
    }

    #[test]
    fn scan_control_events_from_the_new_scan_after_cancel_apply() {
        let mut c = ScanControl::default();
        c.start().unwrap();
        c.cancel();
        let (g, _) = c.start().unwrap();
        let sum = summary(Method::Walk, 0);

        let applied = c.apply(g, Event::Finished(sum.clone()));

        assert!(applied);
        assert_eq!(c.state(), &ScanState::Done(sum));
    }

    #[test]
    fn scan_control_apply_current_progress_updates_counts() {
        let mut c = ScanControl::default();
        let (g, _) = c.start().unwrap();

        let applied = c.apply(g, progress());

        assert!(applied);
        assert_eq!(c.state(), &ScanState::Scanning { files: 10, dirs: 3 });
    }

    // ---- ScanControl::finish: the post_done guard ----
    #[test]
    fn scan_control_finish_after_cancel_returns_no_result() {
        let mut c = ScanControl::default();
        let (g, _) = c.start().unwrap();
        c.cancel();

        let shown = c.finish(g, summary(Method::Walk, 0), "tree");

        assert_eq!((shown, c.state()), (None, &ScanState::Cancelled));
    }

    #[test]
    fn scan_control_finish_from_an_old_scan_returns_no_result_and_keeps_the_new_scan() {
        let mut c = ScanControl::default();
        let (old, _) = c.start().unwrap();
        c.cancel();
        c.start().unwrap();

        let shown = c.finish(old, summary(Method::Walk, 0), "old tree");

        assert_eq!((shown, c.state()), (None, &ScanState::Scanning { files: 0, dirs: 0 }));
    }

    #[test]
    fn scan_control_finish_current_scan_returns_the_result_and_moves_to_done() {
        let mut c = ScanControl::default();
        let (g, _) = c.start().unwrap();
        let sum = summary(Method::Mft, 0);

        let shown = c.finish(g, sum.clone(), "tree");

        assert_eq!((shown, c.state()), (Some("tree"), &ScanState::Done(sum)));
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

    #[test]
    fn offer_elevation_covers_every_method_root_and_admin_combination() {
        let cases = [
            (Method::Walk, r"C:\", false, true),
            (Method::Walk, "\\\\?\\C:\\", false, true),
            (Method::Walk, r"C:\Users", false, false),
            (Method::Walk, "\\\\?\\C:\\Users\\mynd", false, false),
            (Method::Walk, r"C:\", true, false),
            (Method::Mft, r"C:\", false, false),
            (Method::Mft, r"C:\", true, false),
            (Method::Mft, r"C:\Users", false, false),
            (Method::Walk, r"C:\Users", true, false),
        ];
        for (method, root, admin, want) in cases {
            assert_eq!(offer_elevation(method, Path::new(root), admin), want, "{method:?} {root} admin={admin}");
        }
    }
}
