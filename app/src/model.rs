//! View-model logic: size formatting, banner text and the scan state machine.
//! Plain functions, no Slint types, so it is unit tested.

use scan::{Method, Phases, ScanError, ScanStats};
use std::borrow::Cow;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// What the banner needs from a finished scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub method: Method,
    pub files: u64,
    pub dirs: u64,
    pub skipped: u64,
    pub elapsed: Duration,
    /// Time in each part of the scan, for the `--timing` line.
    pub phases: Phases,
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
            phases: stats.phases,
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

    /// "New scan": leaves a finished scan and goes back to `Idle`. The id stays, so a late result
    /// of the finished scan must also be checked against the data the window shows.
    /// `false`, and no change, in any other state.
    pub fn reset(&mut self) -> bool {
        if !matches!(self.state, ScanState::Done(_)) {
            return false;
        }
        self.state = ScanState::Idle;
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

/// True when the window shows the start screen: nothing is scanning and no result is shown.
pub fn shows_start(state: &ScanState) -> bool {
    !matches!(state, ScanState::Scanning { .. } | ScanState::Done(_))
}

/// How the window shows a size: `Dynamic` picks the unit per value, the others fix it.
// Unused until the settings task routes the chosen unit through the window; Task 3 removes the allow.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Unit {
    #[default]
    Dynamic,
    Byte,
    Kb,
    Mb,
    Gb,
    Tb,
}

#[allow(dead_code)]
impl Unit {
    pub const ALL: [Unit; 6] = [Unit::Dynamic, Unit::Byte, Unit::Kb, Unit::Mb, Unit::Gb, Unit::Tb];

    /// The unit at position `i` in `ALL`; an unknown index is `Dynamic`.
    pub fn from_index(i: i32) -> Unit {
        usize::try_from(i).ok().and_then(|i| Unit::ALL.get(i).copied()).unwrap_or_default()
    }

    /// The position of this unit in `ALL`.
    pub fn index(self) -> i32 {
        Unit::ALL.iter().position(|&u| u == self).unwrap_or(0) as i32
    }
}

/// Binary units. `Dynamic`: one decimal above 1 KB, `1.5 MB`. A fixed unit: one decimal, digits
/// grouped, `2,048.0 GB`; a non-empty size below 0.05 of the unit is `< 0.1 GB`.
pub fn format_size(bytes: u64, unit: Unit) -> String {
    let (label, power) = match unit {
        Unit::Dynamic => return format_size_dynamic(bytes),
        Unit::Byte => return format!("{} B", group_digits(bytes)),
        Unit::Kb => ("KB", 1),
        Unit::Mb => ("MB", 2),
        Unit::Gb => ("GB", 3),
        Unit::Tb => ("TB", 4),
    };
    let v = bytes as f64 / 1024f64.powi(power);
    if bytes > 0 && v < 0.05 {
        return format!("< 0.1 {label}");
    }
    let one_decimal = format!("{v:.1}");
    let (whole, tenth) = one_decimal.split_once('.').unwrap_or((&one_decimal, "0"));
    // `v` is at most 2^64 / 1024, so `whole` always fits a u64.
    format!("{}.{tenth} {label}", group_digits(whole.parse().unwrap_or(0)))
}

fn format_size_dynamic(bytes: u64) -> String {
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

/// The name of a file or folder as the window shows it: each character that is invisible or
/// reorders text (bidi and other format controls, control characters, line and paragraph
/// separators, blank fillers) becomes U+FFFD, so a name cannot show other text than it holds. The
/// zero width non-joiner and joiner stay when both neighbours are visible characters that are not
/// ASCII (Persian, Indic and emoji sequences). A name with none is returned borrowed. The stored
/// name is not changed.
pub fn display_name(name: &str) -> Cow<'_, str> {
    // The C0 and C1 controls, the format controls (Cf) that show nothing or reorder text, the line
    // and paragraph separators, and the default-ignorable fillers and selectors that draw blank.
    // U+FE0F stays: it makes a symbol an emoji.
    fn hidden(c: char) -> bool {
        c.is_control()
            || matches!(c, '\u{AD}' | '\u{34F}' | '\u{61C}' | '\u{115F}'..='\u{1160}' | '\u{17B4}'..='\u{17B5}'
                | '\u{180B}'..='\u{180F}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0E}' | '\u{FEFF}' | '\u{FFA0}'
                | '\u{FFF0}'..='\u{FFFB}' | '\u{1BCA0}'..='\u{1BCA3}' | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0000}'..='\u{E0FFF}')
    }
    // A visible character a joiner may touch. A joiner and every other hidden character is not one.
    fn joinable(c: Option<&char>) -> bool {
        c.is_some_and(|&c| !c.is_ascii() && !c.is_whitespace() && !hidden(c))
    }
    if !name.chars().any(hidden) {
        return Cow::Borrowed(name);
    }
    let chars: Vec<char> = name.chars().collect();
    let shown = |i: usize| {
        let joiner = matches!(chars[i], '\u{200C}' | '\u{200D}');
        !hidden(chars[i]) || (joiner && joinable(i.checked_sub(1).and_then(|p| chars.get(p))) && joinable(chars.get(i + 1)))
    };
    if (0..chars.len()).all(shown) {
        return Cow::Borrowed(name);
    }
    Cow::Owned((0..chars.len()).map(|i| if shown(i) { chars[i] } else { '\u{FFFD}' }).collect())
}

/// The scan root as the title above the crumbs shows it: through `display_name`.
pub fn scan_title(root: &Path) -> String {
    display_name(&root.to_string_lossy()).into_owned()
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

/// "Timing: scan 7.2 s (open 0.5, read 3.3, parse 2.3, rollup 1.1), after scan 0.2 s, total 7.9 s."
/// `scan` is the scan itself, `after` the work from the end of the scan until the treemap is shown,
/// `total` the time from the Scan click. The steps in brackets come from `phases`: all four for the
/// MFT scan, `read` and `rollup` for the walker.
pub fn format_timing(summary: &Summary, after: Duration, total: Duration) -> String {
    let p = summary.phases;
    let steps = match summary.method {
        Method::Mft => format!(
            " (open {}, read {}, parse {}, rollup {})",
            step(p.open),
            step(p.read),
            step(p.parse),
            step(p.rollup)
        ),
        Method::Walk => format!(" (read {}, rollup {})", step(p.read), step(p.rollup)),
    };
    let (scan, after, total) = (format_elapsed(summary.elapsed), format_elapsed(after), format_elapsed(total));
    format!("Timing: scan {scan}{steps}, after scan {after}, total {total}.")
}

/// A step time without the unit: `3.3`. From a minute up the unit stays: `1 min 5 s`.
fn step(d: Duration) -> String {
    let t = format_elapsed(d);
    t.strip_suffix(" s").filter(|_| !t.contains("min")).map_or_else(|| t.clone(), str::to_owned)
}

/// The banner with the timing line after it. Without a line the banner is unchanged.
pub fn append_timing(banner: String, timing: Option<&str>) -> String {
    match timing {
        Some(line) => format!("{banner} {line}"),
        None => banner,
    }
}

/// Timings of the latest scan, kept only when the app starts with `--timing`.
pub struct TimingRun {
    /// When the Scan click reached `start_scan`.
    clicked: Instant,
    /// When the worker's scan() returned, captured on the worker.
    finished: Option<Instant>,
    /// The scan result, for the scan time and the steps.
    summary: Option<Summary>,
    /// The finished line. Set once, when the first treemap of the scan is shown.
    line: Option<String>,
}

impl TimingRun {
    pub fn new(clicked: Instant) -> Self {
        Self { clicked, finished: None, summary: None, line: None }
    }

    /// The worker finished the scan at `finished`. "After scan" counts from here, so the wait until
    /// the UI thread takes the result is part of it.
    pub fn scan_finished(&mut self, finished: Instant, summary: &Summary) {
        (self.finished, self.summary) = (Some(finished), Some(summary.clone()));
    }

    /// The first treemap of the scan is shown at `now`. Returns the finished line once; `None`
    /// before the scan result arrived and for later layouts of the same scan.
    pub fn complete(&mut self, now: Instant) -> Option<String> {
        if self.line.is_some() {
            return None;
        }
        let finished = self.finished?;
        let line = format_timing(self.summary.as_ref()?, now - finished, now - self.clicked);
        self.line = Some(line.clone());
        Some(line)
    }

    /// The finished line, if the treemap was shown.
    pub fn line(&self) -> Option<&str> {
        self.line.as_deref()
    }
}

/// The timing line the banner may show: only for a finished scan.
pub fn timing_for_state(state: &ScanState, line: Option<&str>) -> Option<String> {
    line.filter(|_| matches!(state, ScanState::Done(_))).map(str::to_owned)
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

/// [`error_message`] for a scan that ran with `method`. With `--method mft` there is no fallback,
/// so a read error most likely means the MFT cannot be used: it says so.
pub fn scan_error_message(e: &ScanError, method: Option<Method>) -> String {
    match e {
        ScanError::Io(io) if method == Some(Method::Mft) && io.kind() != ErrorKind::NotFound => {
            "The MFT method needs administrator rights and a folder on a local NTFS drive.".into()
        }
        _ => error_message(e),
    }
}

/// The banner with the options of this run: the thread count of a finished scan when `--threads`
/// was given, then the notices about ignored options (in every state).
pub fn with_options(banner: String, state: &ScanState, threads: Option<usize>, notices: &[String]) -> String {
    let threads = threads.filter(|_| matches!(state, ScanState::Done(_)));
    let parts = threads.map(|n| format!("Threads: {n}.")).into_iter().chain(notices.iter().cloned());
    parts.fold(banner, |text, part| format!("{text} {part}"))
}

/// True when the "Restart as administrator" button helps: the scan fell back to the walker, the
/// user is not an administrator, and `root` (canonical) is a whole drive. Only a whole drive
/// takes the MFT path, so for a sub-folder the button would change nothing.
pub fn offer_elevation(method: Method, root: &Path, is_admin: bool) -> bool {
    method == Method::Walk && !is_admin && scan::is_volume_root(root)
}

/// [`offer_elevation`] for the window in `state`: only a finished scan can offer it. `root` is the
/// scan root as the worker resolved it once per scan; `None` while it is not known gives no offer.
pub fn offer_for(state: &ScanState, root: Option<&Path>, is_admin: bool) -> bool {
    matches!((state, root), (ScanState::Done(s), Some(root)) if offer_elevation(s.method, root, is_admin))
}

/// Text for the status banner in each state. Never empty. `offer_elevation` is the result of
/// [`offer_elevation`] for the finished scan: the restart hint shows only when the button does.
pub fn banner_text(state: &ScanState, offer_elevation: bool) -> String {
    match state {
        ScanState::Idle => "Select a drive or a folder to scan.".into(),
        ScanState::Scanning { files, dirs } => {
            format!("Scanning. {} files, {} folders so far.", group_digits(*files), group_digits(*dirs))
        }
        ScanState::Done(s) => {
            let name = match s.method {
                Method::Mft => "Fast scan (MFT)",
                Method::Walk => "Standard scan",
            };
            let hint = if offer_elevation && s.method == Method::Walk { " Restart as administrator to scan a whole drive faster." } else { "" };
            format!(
                "{name}. {} in {} files and {} folders. {} skipped. {}.{hint}",
                format_size(s.total, Unit::Dynamic),
                group_digits(s.files),
                group_digits(s.dirs),
                group_digits(s.skipped),
                format_elapsed(s.elapsed),
            )
        }
        ScanState::Error(m) => format!("Scan failed. {}", display_name(m)).trim_end().to_string(),
        ScanState::Cancelled => "Scan stopped. There is no result.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn summary(method: Method, skipped: u64) -> Summary {
        Summary { method, files: 1234, dirs: 56, skipped, elapsed: Duration::from_millis(2500), phases: Phases::default(), total: 3 * 1024 * 1024 }
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
            assert_eq!(format_size(n, Unit::Dynamic), want, "input {n}");
        }
    }

    #[test]
    fn format_size_dynamic_matches_the_old_values() {
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
            assert_eq!(format_size(n, Unit::Dynamic), want, "input {n}");
        }
    }

    #[test]
    fn format_size_fixed_units_known_values() {
        let cases: [(u64, Unit, &str); 4] = [
            (1536, Unit::Kb, "1.5 KB"),
            (1024 * 1024, Unit::Mb, "1.0 MB"),
            (2 * 1024u64.pow(4), Unit::Gb, "2,048.0 GB"),
            (3 * 1024u64.pow(3) / 2, Unit::Mb, "1,536.0 MB"),
        ];
        for (n, unit, want) in cases {
            assert_eq!(format_size(n, unit), want, "input {n} in {unit:?}");
        }
    }

    #[test]
    fn format_size_byte_shows_a_whole_number_with_separators() {
        assert_eq!(format_size(1_234_567, Unit::Byte), "1,234,567 B");
        assert_eq!(format_size(u64::MAX, Unit::Byte), "18,446,744,073,709,551,615 B");
    }

    #[test]
    fn format_size_tiny_non_empty_file_in_a_big_unit() {
        let cases: [(u64, Unit, &str); 4] = [
            (1, Unit::Mb, "< 0.1 MB"),
            (51, Unit::Kb, "< 0.1 KB"),
            (52, Unit::Kb, "0.1 KB"),
            (0, Unit::Mb, "0.0 MB"),
        ];
        for (n, unit, want) in cases {
            assert_eq!(format_size(n, unit), want, "input {n} in {unit:?}");
        }
    }

    #[test]
    fn unit_index_roundtrip_and_unknown_index_is_dynamic() {
        for (i, unit) in Unit::ALL.into_iter().enumerate() {
            assert_eq!(unit.index(), i as i32, "{unit:?}");
            assert_eq!(Unit::from_index(i as i32), unit, "index {i}");
        }
        assert_eq!(Unit::from_index(-1), Unit::Dynamic);
        assert_eq!(Unit::from_index(6), Unit::Dynamic);
        assert_eq!(Unit::from_index(i32::MAX), Unit::Dynamic);
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

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn timed(method: Method, elapsed: Duration, phases: Phases) -> Summary {
        Summary { elapsed, phases, ..summary(method, 0) }
    }

    fn phases(open: u64, read: u64, parse: u64, rollup: u64) -> Phases {
        Phases { open: ms(open), read: ms(read), parse: ms(parse), rollup: ms(rollup) }
    }

    #[test]
    fn format_timing_mft_lists_open_read_parse_and_rollup() {
        let s = timed(Method::Mft, ms(7200), phases(500, 3300, 2300, 1100));
        assert_eq!(
            format_timing(&s, ms(200), ms(7900)),
            "Timing: scan 7.2 s (open 0.5, read 3.3, parse 2.3, rollup 1.1), after scan 0.2 s, total 7.9 s."
        );
    }

    #[test]
    fn format_timing_walk_lists_read_and_rollup_only() {
        let s = timed(Method::Walk, ms(5300), phases(0, 4100, 0, 1200));
        assert_eq!(
            format_timing(&s, ms(200), ms(5500)),
            "Timing: scan 5.3 s (read 4.1, rollup 1.2), after scan 0.2 s, total 5.5 s."
        );
    }

    #[test]
    fn format_timing_walk_ignores_open_and_parse() {
        let s = timed(Method::Walk, ms(1000), phases(900, 100, 800, 200));
        assert_eq!(
            format_timing(&s, ms(0), ms(1000)),
            "Timing: scan 1.0 s (read 0.1, rollup 0.2), after scan 0.0 s, total 1.0 s."
        );
    }

    #[test]
    fn format_timing_mft_with_zero_phases_shows_zeros() {
        let s = timed(Method::Mft, ms(0), Phases::default());
        assert_eq!(
            format_timing(&s, ms(0), ms(0)),
            "Timing: scan 0.0 s (open 0.0, read 0.0, parse 0.0, rollup 0.0), after scan 0.0 s, total 0.0 s."
        );
    }

    #[test]
    fn format_timing_phase_rounds_half_up_at_the_0_05_boundary() {
        let cases: [(u64, &str); 4] = [(49, "0.0"), (50, "0.1"), (149, "0.1"), (150, "0.2")];
        for (millis, want) in cases {
            let s = timed(Method::Walk, ms(1000), phases(0, millis, 0, 0));
            let line = format_timing(&s, ms(0), ms(1000));
            assert!(line.contains(&format!("(read {want}, rollup 0.0)")), "{millis} ms: {line}");
        }
    }

    #[test]
    fn format_timing_large_values_use_minutes_in_every_part() {
        let s = timed(Method::Mft, Duration::from_secs(3725), Phases {
            open: Duration::from_secs(65),
            read: Duration::from_secs(3600),
            parse: Duration::from_millis(59_949),
            rollup: Duration::from_millis(59_950),
        });
        assert_eq!(
            format_timing(&s, Duration::from_secs(120), Duration::from_secs(3845)),
            "Timing: scan 62 min 5 s (open 1 min 5 s, read 60 min 0 s, parse 59.9, rollup 1 min 0 s), after scan 2 min 0 s, total 64 min 5 s."
        );
    }

    #[test]
    fn summary_new_copies_the_phases() {
        let stats = ScanStats {
            files: 0,
            dirs: 1,
            skipped: 0,
            elapsed: ms(10),
            method: Method::Mft,
            phases: phases(1, 2, 3, 4),
            threads: 1,
        };
        assert_eq!(Summary::new(&stats, 0).phases, phases(1, 2, 3, 4));
    }

    // ---- TimingRun ----
    #[test]
    fn timing_run_after_scan_counts_from_the_worker_finish_so_the_wait_for_the_ui_thread_is_in_it() {
        let t0 = Instant::now();
        let mut run = TimingRun::new(t0);
        // The worker finished at 5.3 s and the treemap showed at 6.1 s, after the UI thread took the result.
        run.scan_finished(t0 + ms(5300), &timed(Method::Walk, ms(5300), phases(0, 4100, 0, 1200)));
        let line = run.complete(t0 + ms(6100));
        assert_eq!(
            line.as_deref(),
            Some("Timing: scan 5.3 s (read 4.1, rollup 1.2), after scan 0.8 s, total 6.1 s.")
        );
    }

    #[test]
    fn timing_run_complete_before_the_result_arrived_gives_none() {
        let t0 = Instant::now();
        let mut run = TimingRun::new(t0);
        assert_eq!(run.complete(t0 + Duration::from_secs(1)), None);
        assert_eq!(run.line(), None);
    }

    #[test]
    fn timing_run_complete_twice_gives_the_line_once() {
        let t0 = Instant::now();
        let mut run = TimingRun::new(t0);
        run.scan_finished(t0 + Duration::from_secs(2), &timed(Method::Walk, Duration::from_secs(2), Phases::default()));
        let first = run.complete(t0 + Duration::from_secs(3));
        assert!(first.is_some());
        assert_eq!(run.line(), first.as_deref());
        assert_eq!(run.complete(t0 + Duration::from_secs(9)), None);
        assert_eq!(run.line(), first.as_deref());
    }

    // ---- timing_for_state ----
    #[test]
    fn timing_for_state_shows_the_line_only_when_done() {
        let line = Some("Timing: x.");
        let cases = [
            (ScanState::Idle, None),
            (ScanState::Scanning { files: 1, dirs: 1 }, None),
            (ScanState::Done(summary(Method::Walk, 0)), Some("Timing: x.".to_string())),
            (ScanState::Error("e".into()), None),
            (ScanState::Cancelled, None),
        ];
        for (state, want) in cases {
            assert_eq!(timing_for_state(&state, line), want, "state {state:?}");
        }
    }

    #[test]
    fn timing_for_state_done_without_a_line_gives_none() {
        assert_eq!(timing_for_state(&ScanState::Done(summary(Method::Walk, 0)), None), None);
    }

    #[test]
    fn append_timing_with_line_adds_it_after_the_banner() {
        assert_eq!(append_timing("Done.".into(), Some("Timing: x.")), "Done. Timing: x.");
    }

    #[test]
    fn append_timing_without_line_leaves_the_banner_unchanged() {
        assert_eq!(append_timing("Done.".into(), None), "Done.");
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

    // ---- scan_error_message ----
    const MFT_MESSAGE: &str = "The MFT method needs administrator rights and a folder on a local NTFS drive.";

    #[test]
    fn scan_error_message_explicit_mft_io_errors_explain_the_mft_needs() {
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::InvalidData, io::ErrorKind::Other] {
            let e = ScanError::Io(io::Error::new(kind, "x"));
            assert_eq!(scan_error_message(&e, Some(Method::Mft)), MFT_MESSAGE, "kind {kind:?}");
        }
    }

    #[test]
    fn scan_error_message_explicit_mft_missing_folder_still_says_not_found() {
        let e = ScanError::Io(io::Error::new(io::ErrorKind::NotFound, "x"));
        assert_eq!(scan_error_message(&e, Some(Method::Mft)), "Folder not found.");
    }

    #[test]
    fn scan_error_message_other_methods_match_error_message() {
        let errors = [
            ScanError::Io(io::Error::new(io::ErrorKind::PermissionDenied, "x")),
            ScanError::Io(io::Error::other("disk exploded")),
            ScanError::NoRoot,
            ScanError::Cancelled,
        ];
        for method in [None, Some(Method::Walk)] {
            for e in &errors {
                assert_eq!(scan_error_message(e, method), error_message(e), "method {method:?}, error {e:?}");
            }
        }
        for e in &errors[2..] {
            assert_eq!(scan_error_message(e, Some(Method::Mft)), error_message(e), "error {e:?}");
        }
    }

    // ---- with_options ----
    fn notices(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn with_options_done_scan_shows_the_thread_count_when_given() {
        let state = ScanState::Done(summary(Method::Walk, 0));
        assert_eq!(with_options("Done.".into(), &state, Some(8), &[]), "Done. Threads: 8.");
    }

    #[test]
    fn with_options_done_scan_without_threads_is_unchanged() {
        let state = ScanState::Done(summary(Method::Walk, 0));
        assert_eq!(with_options("Done.".into(), &state, None, &[]), "Done.");
    }

    #[test]
    fn with_options_other_states_do_not_show_the_thread_count() {
        let states = [ScanState::Idle, ScanState::Scanning { files: 1, dirs: 1 }, ScanState::Error("x".into()), ScanState::Cancelled];
        for state in states {
            assert_eq!(with_options("B.".into(), &state, Some(8), &[]), "B.", "state {state:?}");
        }
    }

    #[test]
    fn with_options_notices_follow_the_banner_in_every_state() {
        let list = notices(&["Volatus ignored the unknown option --foo.", "The value for --method is not correct. Use auto, mft or walk."]);
        let states = [
            ScanState::Idle,
            ScanState::Scanning { files: 1, dirs: 1 },
            ScanState::Done(summary(Method::Walk, 0)),
            ScanState::Error("x".into()),
            ScanState::Cancelled,
        ];
        for state in states {
            let want = "B. Volatus ignored the unknown option --foo. The value for --method is not correct. Use auto, mft or walk.";
            assert_eq!(with_options("B.".into(), &state, None, &list), want, "state {state:?}");
        }
    }

    #[test]
    fn with_options_threads_come_before_the_notices() {
        let state = ScanState::Done(summary(Method::Walk, 0));
        let got = with_options("Done.".into(), &state, Some(2), &notices(&["N."]));
        assert_eq!(got, "Done. Threads: 2. N.");
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
        assert_eq!(banner_text(&ScanState::Idle, false), "Select a drive or a folder to scan.");
    }

    // ---- shows_start ----
    #[test]
    fn shows_start_each_state_matches_its_screen() {
        let cases = [
            (ScanState::Idle, true),
            (ScanState::Scanning { files: 0, dirs: 0 }, false),
            (ScanState::Done(summary(Method::Walk, 0)), false),
            (ScanState::Error("e".into()), true),
            (ScanState::Cancelled, true),
        ];
        for (state, want) in cases {
            assert_eq!(shows_start(&state), want, "state {state:?}");
        }
    }

    // ---- reset ----
    fn control_in(state: ScanState) -> ScanControl {
        ScanControl { state, generation: 7, cancel: Arc::new(AtomicBool::new(false)) }
    }

    #[test]
    fn reset_done_goes_to_idle_and_keeps_the_generation() {
        let mut c = control_in(ScanState::Done(summary(Method::Walk, 0)));
        assert!(c.reset());
        assert_eq!(c.state(), &ScanState::Idle);
        assert_eq!(c.generation(), 7);
    }

    #[test]
    fn reset_other_states_change_nothing() {
        let others = [
            ScanState::Idle,
            ScanState::Scanning { files: 1, dirs: 2 },
            ScanState::Error("e".into()),
            ScanState::Cancelled,
        ];
        for state in others {
            let mut c = control_in(state.clone());
            assert!(!c.reset(), "state {state:?}");
            assert_eq!(c.state(), &state);
            assert_eq!(c.generation(), 7);
        }
    }

    #[test]
    fn banner_text_scanning_shows_counts() {
        let t = banner_text(&ScanState::Scanning { files: 1234, dirs: 5 }, false);
        assert_eq!(t, "Scanning. 1,234 files, 5 folders so far.");
    }

    #[test]
    fn banner_text_done_mft_shows_method_counts_size_and_time() {
        let t = banner_text(&ScanState::Done(summary(Method::Mft, 0)), false);
        assert_eq!(t, "Fast scan (MFT). 3.0 MB in 1,234 files and 56 folders. 0 skipped. 2.5 s.");
    }

    #[test]
    fn banner_text_done_walk_says_standard_scan_and_why_slower() {
        let t = banner_text(&ScanState::Done(summary(Method::Walk, 0)), true);
        assert_eq!(
            t,
            "Standard scan. 3.0 MB in 1,234 files and 56 folders. 0 skipped. 2.5 s. Restart as administrator to scan a whole drive faster."
        );
    }

    #[test]
    fn banner_text_done_walk_without_offer_has_no_restart_hint() {
        let t = banner_text(&ScanState::Done(summary(Method::Walk, 0)), false);
        assert_eq!(t, "Standard scan. 3.0 MB in 1,234 files and 56 folders. 0 skipped. 2.5 s.");
    }

    #[test]
    fn banner_text_done_mft_with_offer_has_no_restart_hint() {
        let t = banner_text(&ScanState::Done(summary(Method::Mft, 0)), true);
        assert_eq!(t, "Fast scan (MFT). 3.0 MB in 1,234 files and 56 folders. 0 skipped. 2.5 s.");
    }

    /// The hint follows `offer_elevation` for every method, root and admin combination.
    #[test]
    fn banner_text_hint_shows_exactly_when_offer_elevation_does() {
        let hint = "Restart as administrator";
        let cases: [(Method, &str, bool, bool); 6] = [
            (Method::Walk, r"C:\", false, true),
            (Method::Walk, r"C:\Users", false, false),
            (Method::Walk, r"C:\", true, false),
            (Method::Walk, r"C:\Users", true, false),
            (Method::Mft, r"C:\", false, false),
            (Method::Mft, r"C:\Users", false, false),
        ];
        for (method, root, admin, want) in cases {
            let offer = offer_elevation(method, Path::new(root), admin);
            let t = banner_text(&ScanState::Done(summary(method, 0)), offer);
            assert_eq!(t.contains(hint), want, "{method:?} {root} admin={admin}: {t}");
        }
    }

    #[test]
    fn banner_text_done_with_skipped_shows_skipped_count() {
        let t = banner_text(&ScanState::Done(summary(Method::Walk, 7)), false);
        assert!(t.contains("7 skipped."), "{t}");
    }

    #[test]
    fn banner_text_error_prefixes_message() {
        assert_eq!(banner_text(&ScanState::Error("Access denied.".into()), false), "Scan failed. Access denied.");
    }

    #[test]
    fn banner_text_error_with_empty_message_is_still_not_blank() {
        assert_eq!(banner_text(&ScanState::Error(String::new()), false), "Scan failed.");
    }

    #[test]
    fn banner_text_cancelled_says_no_result() {
        assert_eq!(banner_text(&ScanState::Cancelled, false), "Scan stopped. There is no result.");
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
            Summary { method: Method::Walk, files: 4, dirs: 2, skipped: 1, elapsed: Duration::from_secs(3), phases: Phases::default(), total: 99 }
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

    // ---- offer_for ----
    /// Every state with every resolved root (none yet, a drive, a folder), walker, not admin: only a
    /// finished scan of a whole drive offers the restart.
    #[test]
    fn offer_for_covers_every_state_and_resolved_root() {
        let states = [
            ScanState::Idle,
            ScanState::Scanning { files: 1, dirs: 1 },
            ScanState::Done(summary(Method::Walk, 0)),
            ScanState::Error("bad".into()),
            ScanState::Cancelled,
        ];
        let roots = [None, Some(Path::new(r"C:\")), Some(Path::new(r"C:\Users"))];
        for state in &states {
            for root in roots {
                let want = matches!(state, ScanState::Done(_)) && root == Some(Path::new(r"C:\"));
                assert_eq!(offer_for(state, root, false), want, "{state:?} {root:?}");
            }
        }
    }

    #[test]
    fn offer_for_finished_drive_scan_as_admin_gives_no_offer() {
        assert!(!offer_for(&ScanState::Done(summary(Method::Walk, 0)), Some(Path::new(r"C:\")), true));
    }

    // ---- display_name ----
    const FFFD: char = '\u{FFFD}';

    /// Every character `display_name` hides, as inclusive ranges.
    const HIDDEN: [(u32, u32); 19] = [
        (0x00, 0x1F),
        (0x7F, 0x9F),
        (0xAD, 0xAD),
        (0x34F, 0x34F),
        (0x61C, 0x61C),
        (0x115F, 0x1160),
        (0x17B4, 0x17B5),
        (0x180B, 0x180F),
        (0x200B, 0x200F),
        (0x2028, 0x202E),
        (0x2060, 0x206F),
        (0x3164, 0x3164),
        (0xFE00, 0xFE0E),
        (0xFEFF, 0xFEFF),
        (0xFFA0, 0xFFA0),
        (0xFFF0, 0xFFFB),
        (0x1BCA0, 0x1BCA3),
        (0x1D173, 0x1D17A),
        (0xE0000, 0xE0FFF),
    ];

    #[test]
    fn display_name_every_hidden_character_becomes_one_placeholder() {
        let mut kept = Vec::new();
        for c in HIDDEN.iter().flat_map(|&(a, b)| a..=b).filter_map(char::from_u32) {
            let name = format!("a{c}b");
            if display_name(&name) != format!("a{FFFD}b") {
                kept.push(format!("U+{:04X}", c as u32));
            }
        }
        assert!(kept.is_empty(), "not replaced: {kept:?}");
    }

    #[test]
    fn display_name_characters_next_to_the_hidden_ranges_are_kept() {
        // Hair space, hyphen, narrow no-break space, medium math space, superscript zero,
        // variation selector 16, object replacement character, first printable ASCII, U+00A0, U+00A1.
        let next_to = "\u{200A}\u{2010}\u{2027}\u{202F}\u{205F}\u{2070}\u{FE0F}\u{FFFC}\u{20}\u{A0}\u{A1}\u{E1000}\u{3163}\u{3165}\u{FFA1}\u{FFEF}\u{115E}\u{1161}\u{17B3}\u{17B6}\u{180A}\u{1810}\u{FE10}\u{34E}\u{350}\u{1BC9F}\u{1BCA4}\u{1D172}\u{1D17B}";
        assert_eq!(display_name(next_to), next_to);
    }

    #[test]
    fn display_name_right_to_left_override_cannot_reverse_the_extension() {
        let shown = display_name("invoice\u{202E}fdp.exe");
        assert_eq!(shown, format!("invoice{FFFD}fdp.exe"));
    }

    #[test]
    fn display_name_isolates_and_marks_are_replaced() {
        assert_eq!(display_name("\u{2066}x\u{2069}\u{200E}y\u{200F}\u{061C}"), format!("{FFFD}x{FFFD}{FFFD}y{FFFD}{FFFD}"));
    }

    #[test]
    fn display_name_zero_width_and_bom_are_replaced() {
        assert_eq!(display_name("a\u{200B}b\u{200C}c\u{200D}d\u{FEFF}"), format!("a{FFFD}b{FFFD}c{FFFD}d{FFFD}"));
    }

    #[test]
    fn display_name_name_of_only_controls_is_only_placeholders() {
        assert_eq!(display_name("\u{202E}\u{200B}\u{FEFF}"), format!("{FFFD}{FFFD}{FFFD}"));
    }

    #[test]
    fn display_name_empty_name_stays_empty() {
        assert_eq!(display_name(""), "");
    }

    #[test]
    fn display_name_plain_ascii_is_borrowed_unchanged() {
        let shown = display_name("pagefile.sys");
        assert!(matches!(shown, Cow::Borrowed("pagefile.sys")), "{shown:?}");
    }

    #[test]
    fn display_name_emoji_hebrew_and_arabic_are_borrowed_unchanged() {
        for name in ["clip \u{1F3AC}.mp4", "\u{5E9}\u{5DC}\u{5D5}\u{5DD}.txt", "\u{645}\u{631}\u{62D}\u{628}\u{627}", "\u{2764}\u{FE0F}.png"] {
            assert!(matches!(display_name(name), Cow::Borrowed(_)), "{name}");
            assert_eq!(display_name(name), name);
        }
    }

    #[test]
    fn display_name_a_replaced_name_is_owned() {
        assert!(matches!(display_name("a\u{202E}b"), Cow::Owned(_)));
    }

    #[test]
    fn display_name_a_name_of_hangul_fillers_is_only_placeholders() {
        assert_eq!(display_name("\u{3164}\u{3164}\u{FFA0}\u{115F}\u{1160}"), format!("{FFFD}{FFFD}{FFFD}{FFFD}{FFFD}"));
    }

    #[test]
    fn display_name_variation_selector_16_after_a_symbol_is_kept() {
        assert!(matches!(display_name("\u{2764}\u{FE0F}"), Cow::Borrowed(_)));
    }

    // The zero width non-joiner (U+200C) and joiner (U+200D) stay only between two visible
    // characters that are not ASCII.
    const ZWNJ: char = '\u{200C}';
    const ZWJ: char = '\u{200D}';

    #[test]
    fn display_name_persian_zero_width_non_joiner_is_kept() {
        let name = "\u{645}\u{6CC}\u{200C}\u{62E}\u{648}\u{627}\u{647}\u{645}";
        assert!(matches!(display_name(name), Cow::Borrowed(_)));
        assert_eq!(display_name(name), name);
    }

    #[test]
    fn display_name_indic_conjunct_joiner_is_kept() {
        let name = "\u{915}\u{94D}\u{200D}\u{937}";
        assert_eq!(display_name(name), name);
    }

    #[test]
    fn display_name_emoji_family_sequence_is_kept() {
        let name = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}.png";
        assert_eq!(display_name(name), name);
    }

    #[test]
    fn display_name_joiner_between_ascii_letters_is_replaced() {
        for joiner in [ZWNJ, ZWJ] {
            assert_eq!(display_name(&format!("a{joiner}b")), format!("a{FFFD}b"));
        }
    }

    #[test]
    fn display_name_joiner_with_ascii_on_one_side_is_replaced() {
        for joiner in [ZWNJ, ZWJ] {
            assert_eq!(display_name(&format!("\u{645}{joiner}a")), format!("\u{645}{FFFD}a"));
            assert_eq!(display_name(&format!("a{joiner}\u{645}")), format!("a{FFFD}\u{645}"));
        }
    }

    #[test]
    fn display_name_joiner_at_the_start_or_the_end_is_replaced() {
        for joiner in [ZWNJ, ZWJ] {
            assert_eq!(display_name(&format!("{joiner}\u{645}")), format!("{FFFD}\u{645}"));
            assert_eq!(display_name(&format!("\u{645}{joiner}")), format!("\u{645}{FFFD}"));
            assert_eq!(display_name(&joiner.to_string()), FFFD.to_string());
        }
    }

    #[test]
    fn display_name_two_joiners_in_a_row_are_both_replaced() {
        assert_eq!(display_name("\u{645}\u{200D}\u{200D}\u{646}"), format!("\u{645}{FFFD}{FFFD}\u{646}"));
        assert_eq!(display_name("\u{645}\u{200C}\u{200D}\u{646}"), format!("\u{645}{FFFD}{FFFD}\u{646}"));
    }

    #[test]
    fn display_name_joiner_next_to_another_hidden_character_is_replaced() {
        assert_eq!(display_name("\u{645}\u{200B}\u{200D}\u{646}"), format!("\u{645}{FFFD}{FFFD}\u{646}"));
        assert_eq!(display_name("\u{645}\u{200D}\u{202E}\u{646}"), format!("\u{645}{FFFD}{FFFD}\u{646}"));
    }

    #[test]
    fn display_name_joiner_next_to_a_space_is_replaced() {
        assert_eq!(display_name("\u{645}\u{200D}\u{A0}\u{646}"), format!("\u{645}{FFFD}\u{A0}\u{646}"));
    }

    // ---- scan_title ----
    #[test]
    fn scan_title_plain_path_is_unchanged() {
        assert_eq!(scan_title(Path::new(r"D:\data")), r"D:\data");
    }

    #[test]
    fn scan_title_root_with_a_right_to_left_override_shows_a_placeholder() {
        assert_eq!(scan_title(Path::new("D:\\x\u{202E}y")), format!("D:\\x{FFFD}y"));
    }

    // ---- other places a name reaches the window ----
    #[test]
    fn banner_text_error_message_with_a_hidden_character_shows_a_placeholder() {
        let m = "Could not read D:\\x\u{202E}y.".to_string();
        assert_eq!(banner_text(&ScanState::Error(m), false), format!("Scan failed. Could not read D:\\x{FFFD}y."));
    }
}
