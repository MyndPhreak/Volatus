//! Delete policy: which items may go, how a run is reported. Pure logic, no UI and no deletion.
//! The checks come first and fail closed: an unknown target is refused, never deleted.

#![cfg_attr(not(test), expect(dead_code, reason = "wired in the UI task"))]
#![cfg_attr(test, allow(dead_code, reason = "tests do not use every item; wired in the UI task"))]

use crate::model::{Unit, format_size};
use scan::NodeId;
use std::path::{Path, PathBuf};

/// How the item leaves the disk. Plain Delete is always `RecycleBin`; only Shift gives `Permanent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMode {
    RecycleBin,
    Permanent,
}

/// Why a delete is refused before it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    NotUnderRoot,
    ProtectedFolder,
    DriveRoot,
    Missing,
    KindChanged,
    ScanRunning,
    /// The root or the item could not be resolved to a real path, or the environment is unknown.
    Unresolved,
}

/// The item the user chose, with the scan's numbers for it.
#[derive(Debug, Clone)]
pub struct DeleteTarget {
    pub node: NodeId,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: u64,
    pub files: u64,
    pub dirs: u64,
}

/// What the file system says about a path right now. The seam for tests.
pub trait Probe {
    fn exists(&self, p: &Path) -> bool;
    fn is_dir(&self, p: &Path) -> bool;
    /// The real path of `p` (links, `..` and trailing dots resolved), or `None` if unknown.
    fn resolve(&self, p: &Path) -> Option<PathBuf>;
}

/// The real file system. A link is not followed: it counts as a file, as in the scan.
pub struct RealFs;

impl Probe for RealFs {
    fn exists(&self, p: &Path) -> bool {
        std::fs::symlink_metadata(p).is_ok()
    }
    fn is_dir(&self, p: &Path) -> bool {
        std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir())
    }
    fn resolve(&self, p: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(p).ok()
    }
}

/// The environment values the protected list is built from. Tests pass fixed strings.
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub windir: String,
    pub program_files: String,
    pub program_files_x86: String,
    pub program_data: String,
    pub user_profile: String,
}

impl Env {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Option<Env> {
        let _ = get;
        None
    }

    pub fn from_process() -> Option<Env> {
        None
    }
}

/// How a protected entry applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The folder and everything inside it.
    Subtree,
    /// The folder itself only.
    Exact,
}

pub fn protected_folders(env: &Env) -> Vec<(PathBuf, Scope)> {
    let mut v = vec![(PathBuf::from(&env.windir), Scope::Subtree)];
    for p in [&env.program_files, &env.program_files_x86, &env.program_data, &env.user_profile] {
        if !p.is_empty() {
            v.push((PathBuf::from(p), Scope::Exact));
        }
    }
    v
}

pub fn refusal_text(r: Refusal) -> &'static str {
    let _ = r;
    ""
}

/// Lowercase, `\\?\` stripped, `/` as `\`, trailing separators trimmed. The form paths compare in.
fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().to_lowercase().replace('/', r"\");
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    s.trim_end_matches('\\').to_string()
}

/// True when `child` is `parent` or inside it. Both are in `norm` form.
fn within(child: &str, parent: &str) -> bool {
    child == parent || child.strip_prefix(parent).is_some_and(|rest| rest.starts_with('\\'))
}

/// Checks the item against the scan root, the protected list and the disk, in this order:
/// `NotUnderRoot`, `DriveRoot`, `NotUnderRoot` (the root itself), `ProtectedFolder`, `Missing`,
/// `KindChanged`. Reads the real environment for the protected list.
pub fn check_target(root: &Path, target: &Path, is_dir: bool, fs: &dyn Probe) -> Result<(), Refusal> {
    match Env::from_process() {
        Some(env) => check_target_in(root, target, is_dir, fs, &env),
        None => Err(Refusal::Unresolved),
    }
}

/// `check_target` with the environment given, so tests never read the real one.
pub fn check_target_in(
    root: &Path,
    target: &Path,
    is_dir: bool,
    fs: &dyn Probe,
    env: &Env,
) -> Result<(), Refusal> {
    let (r, t) = (norm(root), norm(target));
    if !within(&t, &r) {
        return Err(Refusal::NotUnderRoot);
    }
    // A drive root is named before the root check below, so a scan of `C:\` says "drive root".
    let bare_drive = t.len() == 2 && t.ends_with(':');
    if bare_drive || scan::is_volume_root(target) {
        return Err(Refusal::DriveRoot);
    }
    if t == r {
        return Err(Refusal::NotUnderRoot);
    }
    // `protected_folders` lists `Windows` first: it guards everything inside; the rest, only itself.
    let list: Vec<String> = protected_folders(env).iter().map(|p| norm(&p.0)).collect();
    let hit = list.iter().enumerate().any(|(i, p)| if i == 0 { within(&t, p) } else { t == *p });
    if hit {
        return Err(Refusal::ProtectedFolder);
    }
    if !fs.exists(target) {
        return Err(Refusal::Missing);
    }
    if fs.is_dir(target) != is_dir {
        return Err(Refusal::KindChanged);
    }
    Ok(())
}

/// Plain Delete is the Recycle Bin. Only Shift gives the permanent delete.
pub fn mode_for(shift: bool) -> DeleteMode {
    if shift { DeleteMode::Permanent } else { DeleteMode::RecycleBin }
}

/// Why one item was not removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Locked,
    AccessDenied,
    NeedsAdmin,
    TooLong,
    Other(i32),
}

/// An item the run left in place.
#[derive(Debug, Clone)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: SkipReason,
}

/// What a run did.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub removed_items: u64,
    pub removed_bytes: u64,
    pub skipped: Vec<Skipped>,
    pub stopped: bool,
}

/// Maps a Win32 error code to a skip reason.
pub fn reason_from_win32(code: i32) -> SkipReason {
    match code {
        5 => SkipReason::AccessDenied,
        32 | 33 => SkipReason::Locked,
        206 => SkipReason::TooLong,
        other => SkipReason::Other(other),
    }
}

/// The result text, one short sentence per line (ASD-STE100).
pub fn summary_lines(mode: DeleteMode, o: &Outcome) -> Vec<String> {
    let mut lines = Vec::new();
    if o.removed_items > 0 {
        match mode {
            DeleteMode::RecycleBin => {
                lines.push(format!("Moved {} to the Recycle Bin.", items(o.removed_items)));
                lines.push("Disk space is freed only when you empty the Recycle Bin.".to_string());
            }
            DeleteMode::Permanent => {
                lines.push(format!("Deleted {}.", items(o.removed_items)));
                lines.push(format!("Freed {}.", format_size(o.removed_bytes, Unit::Dynamic)));
            }
        }
    }
    if o.stopped {
        lines.push("You stopped the delete.".to_string());
        lines.push("Items that Volatus removed stay removed.".to_string());
    }
    if !o.skipped.is_empty() {
        lines.push(format!("Skipped {}.", items(o.skipped.len() as u64)));
        lines.push("The list shows the reason for each item.".to_string());
    }
    if o.skipped.iter().any(|s| s.reason == SkipReason::NeedsAdmin) {
        lines.push("Windows needs administrator rights to remove some items.".to_string());
        lines.push("Restart as administrator, then delete them again.".to_string());
    }
    lines
}

/// "1 item", "3 items".
fn items(n: u64) -> String {
    if n == 1 { "1 item".to_string() } else { format!("{n} items") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A map-backed file system. `items`: `norm` path -> `is_dir`. `links`: a path (and what is
    /// under it) that resolves to another path. `broken`: paths `resolve` cannot answer for.
    /// `resolve` also does `..` and trailing dots, as `canonicalize` does.
    struct Fake {
        items: HashMap<String, bool>,
        links: Vec<(String, String)>,
        broken: Vec<String>,
    }

    impl Fake {
        fn with(items: &[(&str, bool)]) -> Fake {
            Fake {
                items: items.iter().map(|(p, d)| (norm(Path::new(p)), *d)).collect(),
                links: Vec::new(),
                broken: Vec::new(),
            }
        }
        fn link(mut self, from: &str, to: &str) -> Fake {
            self.links.push((norm(Path::new(from)), norm(Path::new(to))));
            self
        }
        fn broken(mut self, p: &str) -> Fake {
            self.broken.push(norm(Path::new(p)));
            self
        }
    }

    impl Probe for Fake {
        fn exists(&self, p: &Path) -> bool {
            self.items.contains_key(&norm(p))
        }
        fn is_dir(&self, p: &Path) -> bool {
            self.items.get(&norm(p)).copied().unwrap_or(false)
        }
        fn resolve(&self, p: &Path) -> Option<PathBuf> {
            let mut parts: Vec<&str> = Vec::new();
            let n = p.to_string_lossy().to_lowercase().replace('/', "\\");
            let n = n.strip_prefix(r"\\?\").unwrap_or(&n).to_string();
            for c in n.split('\\') {
                match c {
                    "" | "." => {}
                    ".." => {
                        if parts.len() > 1 {
                            parts.pop();
                        }
                    }
                    c => parts.push(c.trim_end_matches(['.', ' '])),
                }
            }
            let mut out = parts.join("\\");
            if !out.contains('\\') {
                out.push('\\');
            }
            let key = out.trim_end_matches('\\').to_string();
            if self.broken.iter().any(|b| within(&key, b)) {
                return None;
            }
            for (from, to) in &self.links {
                if let Some(rest) = key.strip_prefix(from.as_str()).filter(|r| r.is_empty() || r.starts_with('\\')) {
                    return Some(PathBuf::from(format!("{to}{rest}")));
                }
            }
            Some(PathBuf::from(out))
        }
    }

    fn env() -> Env {
        Env {
            windir: r"C:\Windows".into(),
            program_files: r"C:\Program Files".into(),
            program_files_x86: r"C:\Program Files (x86)".into(),
            program_data: r"C:\ProgramData".into(),
            user_profile: r"C:\Users\dev".into(),
        }
    }

    /// A probe that says every path is a folder, for tests that stop before the disk checks.
    fn all_dirs(paths: &[&str]) -> Fake {
        Fake::with(&paths.iter().map(|p| (*p, true)).collect::<Vec<_>>())
    }

    fn check(root: &str, target: &str, fs: &Fake) -> Result<(), Refusal> {
        check_target_in(Path::new(root), Path::new(target), true, fs, &env())
    }

    #[test]
    fn child_of_the_root_is_ok() {
        let fs = all_dirs(&[r"C:\Users\dev\data\cache"]);
        assert_eq!(check(r"C:\Users\dev\data", r"C:\Users\dev\data\cache", &fs), Ok(()));
    }

    #[test]
    fn the_root_itself_is_not_under_the_root() {
        let fs = all_dirs(&[r"C:\Users\dev\data"]);
        assert_eq!(check(r"C:\Users\dev\data", r"C:\Users\dev\data", &fs), Err(Refusal::NotUnderRoot));
        assert_eq!(check(r"C:\Users\dev\data", r"C:\Users\dev\data\", &fs), Err(Refusal::NotUnderRoot));
    }

    #[test]
    fn a_sibling_of_the_root_is_not_under_the_root() {
        let fs = all_dirs(&[r"C:\Users\dev\other", r"C:\Users\dev\data2"]);
        assert_eq!(check(r"C:\Users\dev\data", r"C:\Users\dev\other", &fs), Err(Refusal::NotUnderRoot));
        // A name that only starts like the root is not inside it.
        assert_eq!(check(r"C:\Users\dev\data", r"C:\Users\dev\data2", &fs), Err(Refusal::NotUnderRoot));
    }

    #[test]
    fn each_protected_folder_as_the_item_is_refused() {
        let fs = all_dirs(&[]);
        for p in [
            r"C:\Windows",
            r"C:\Program Files",
            r"C:\Program Files (x86)",
            r"C:\ProgramData",
            r"C:\Users\dev",
        ] {
            assert_eq!(check(r"C:\", p, &fs), Err(Refusal::ProtectedFolder), "{p}");
        }
    }

    #[test]
    fn children_of_the_four_open_protected_folders_are_ok() {
        for p in [
            r"C:\Program Files\App",
            r"C:\Program Files (x86)\App",
            r"C:\ProgramData\App",
            r"C:\Users\dev\Downloads",
        ] {
            let fs = all_dirs(&[p]);
            assert_eq!(check(r"C:\", p, &fs), Ok(()), "{p}");
        }
    }

    #[test]
    fn everything_inside_windows_is_refused() {
        let fs = all_dirs(&[]);
        for p in [r"C:\Windows\Temp", r"C:\Windows\System32\drivers"] {
            assert_eq!(check(r"C:\", p, &fs), Err(Refusal::ProtectedFolder), "{p}");
        }
        // A name that only starts like `Windows` is not inside it.
        let fs = all_dirs(&[r"C:\Windows.old"]);
        assert_eq!(check(r"C:\", r"C:\Windows.old", &fs), Ok(()));
    }

    #[test]
    fn protected_match_ignores_case_prefix_and_trailing_separator() {
        let fs = all_dirs(&[]);
        for p in [r"c:\WINDOWS", r"C:\Windows\", r"\\?\C:\Windows", r"\\?\c:\program files\", r"C:/ProgramData"] {
            assert_eq!(check(r"C:\", p, &fs), Err(Refusal::ProtectedFolder), "{p}");
        }
    }

    #[test]
    fn user_profile_is_refused_when_the_scan_starts_above_it() {
        let fs = all_dirs(&[]);
        assert_eq!(check(r"C:\Users", r"C:\Users\dev", &fs), Err(Refusal::ProtectedFolder));
    }

    #[test]
    fn a_drive_root_is_refused_as_a_drive_root() {
        let fs = all_dirs(&[]);
        assert_eq!(check(r"C:\", r"C:\", &fs), Err(Refusal::DriveRoot));
        assert_eq!(check(r"C:\", r"c:\", &fs), Err(Refusal::DriveRoot));
        assert_eq!(check(r"\\?\D:\", r"\\?\D:\", &fs), Err(Refusal::DriveRoot));
        assert_eq!(check(r"D:\", r"\\?\D:\", &fs), Err(Refusal::DriveRoot));
    }

    #[test]
    fn a_drive_root_outside_the_scan_root_is_not_under_the_root() {
        let fs = all_dirs(&[]);
        assert_eq!(check(r"C:\", r"D:\", &fs), Err(Refusal::NotUnderRoot));
        assert_eq!(check(r"C:\Users\dev\data", r"C:\", &fs), Err(Refusal::NotUnderRoot));
    }

    #[test]
    fn refusals_come_in_the_stated_order() {
        // The probe knows nothing, so a Missing result would show the order is wrong.
        let fs = all_dirs(&[]);
        assert_eq!(check(r"D:\", r"C:\Windows", &fs), Err(Refusal::NotUnderRoot));
        assert_eq!(check(r"C:\", r"C:\Windows", &fs), Err(Refusal::ProtectedFolder));
        assert_eq!(check(r"C:\", r"C:\Users\dev\gone", &fs), Err(Refusal::Missing));
    }

    #[test]
    fn a_path_that_no_longer_exists_is_missing() {
        let fs = all_dirs(&[]);
        assert_eq!(check(r"C:\data", r"C:\data\gone", &fs), Err(Refusal::Missing));
    }

    #[test]
    fn a_file_where_a_folder_was_is_kind_changed() {
        let fs = Fake::with(&[(r"C:\data\x", false)]);
        assert_eq!(check(r"C:\data", r"C:\data\x", &fs), Err(Refusal::KindChanged));
    }

    #[test]
    fn a_folder_where_a_file_was_is_kind_changed() {
        let fs = Fake::with(&[(r"C:\data\x", true)]);
        let got = check_target_in(Path::new(r"C:\data"), Path::new(r"C:\data\x"), false, &fs, &env());
        assert_eq!(got, Err(Refusal::KindChanged));
    }

    #[test]
    fn a_file_of_the_expected_kind_is_ok() {
        let fs = Fake::with(&[(r"C:\data\x.txt", false)]);
        let got = check_target_in(Path::new(r"C:\data"), Path::new(r"C:\data\x.txt"), false, &fs, &env());
        assert_eq!(got, Ok(()));
    }

    #[test]
    fn protected_folders_come_from_the_env_with_a_scope_and_skip_empty_values() {
        let mut e = env();
        e.user_profile = String::new();
        let list = protected_folders(&e);
        assert_eq!(list.len(), 4);
        assert!(list.contains(&(PathBuf::from(r"C:\Windows"), Scope::Subtree)));
        assert!(list.contains(&(PathBuf::from(r"C:\ProgramData"), Scope::Exact)));
        let all = protected_folders(&env());
        assert_eq!(all.len(), 5);
        assert!(all.contains(&(PathBuf::from(r"C:\Users\dev"), Scope::Exact)));
        assert_eq!(all.iter().filter(|(_, sc)| *sc == Scope::Subtree).count(), 1);
    }

    #[test]
    fn other_volumes_have_the_same_protected_folders() {
        let fs = all_dirs(&[r"D:\Program Files\X", r"D:\Windows.old"]);
        for p in [
            r"D:\Windows",
            r"D:\Windows\x",
            r"d:\WINDOWS\System32",
            r"D:\Program Files",
            r"D:\Program Files (x86)",
            r"D:\ProgramData",
        ] {
            assert_eq!(check(r"D:\", p, &fs), Err(Refusal::ProtectedFolder), "{p}");
        }
        assert_eq!(check(r"D:\", r"D:\Program Files\X", &fs), Ok(()));
        assert_eq!(check(r"D:\", r"D:\Windows.old", &fs), Ok(()));
    }

    #[test]
    fn a_root_with_dot_dot_is_resolved_before_the_checks() {
        let fs = all_dirs(&[]);
        assert_eq!(check(r"C:\x\..", r"C:\x\..\Windows\System32", &fs), Err(Refusal::ProtectedFolder));
    }

    #[test]
    fn a_root_with_a_trailing_dot_is_resolved_before_the_checks() {
        let fs = all_dirs(&[]);
        assert_eq!(check(r"C:\Windows.", r"C:\Windows.\System32", &fs), Err(Refusal::ProtectedFolder));
    }

    #[test]
    fn a_root_that_is_a_junction_into_windows_is_refused() {
        let fs = all_dirs(&[r"C:\Link\x"]).link(r"C:\Link", r"C:\Windows");
        assert_eq!(check(r"C:\Link", r"C:\Link\x", &fs), Err(Refusal::ProtectedFolder));
    }

    #[test]
    fn a_link_item_is_not_followed() {
        // The link points into Windows, but the item is the link itself: only its parent resolves.
        let fs = Fake::with(&[(r"C:\data\lnk", false)]).link(r"C:\data\lnk", r"C:\Windows");
        let got = check_target_in(Path::new(r"C:\data"), Path::new(r"C:\data\lnk"), false, &fs, &env());
        assert_eq!(got, Ok(()));
    }

    #[test]
    fn an_unresolved_root_is_unresolved() {
        let fs = all_dirs(&[r"C:\data\x"]).broken(r"C:\gone");
        assert_eq!(check(r"C:\gone", r"C:\data\x", &fs), Err(Refusal::Unresolved));
    }

    #[test]
    fn an_unresolved_item_parent_is_unresolved() {
        let fs = all_dirs(&[r"C:\data\sub\x"]).broken(r"C:\data\sub");
        assert_eq!(check(r"C:\data", r"C:\data\sub\x", &fs), Err(Refusal::Unresolved));
    }

    #[test]
    fn unresolved_root_and_item_together_is_unresolved() {
        let fs = all_dirs(&[]).broken(r"C:\");
        assert_eq!(check(r"C:\data", r"C:\data\x", &fs), Err(Refusal::Unresolved));
    }

    fn lookup(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        let pairs = pairs.to_vec();
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    const FULL: [(&str, &str); 5] = [
        ("SystemRoot", r"C:\Windows"),
        ("ProgramFiles", r"C:\Program Files"),
        ("ProgramFiles(x86)", r"C:\Program Files (x86)"),
        ("ProgramData", r"C:\ProgramData"),
        ("USERPROFILE", r"C:\Users\dev"),
    ];

    #[test]
    fn from_lookup_reads_every_value() {
        let e = Env::from_lookup(lookup(&FULL)).expect("env");
        assert_eq!(e.windir, r"C:\Windows");
        assert_eq!(e.program_files, r"C:\Program Files");
        assert_eq!(e.program_files_x86, r"C:\Program Files (x86)");
        assert_eq!(e.program_data, r"C:\ProgramData");
        assert_eq!(e.user_profile, r"C:\Users\dev");
    }

    #[test]
    fn from_lookup_without_the_user_profile_is_none() {
        let mut pairs = FULL.to_vec();
        pairs.retain(|(k, _)| *k != "USERPROFILE");
        assert!(Env::from_lookup(lookup(&pairs)).is_none());
        pairs.push(("USERPROFILE", ""));
        assert!(Env::from_lookup(lookup(&pairs)).is_none());
    }

    #[test]
    fn from_lookup_without_windir_is_none() {
        let mut pairs = FULL.to_vec();
        pairs.retain(|(k, _)| *k != "SystemRoot");
        assert!(Env::from_lookup(lookup(&pairs)).is_none());
        pairs.push(("SystemRoot", ""));
        assert!(Env::from_lookup(lookup(&pairs)).is_none());
    }

    #[test]
    fn from_lookup_prefers_program_w6432_for_program_files() {
        let mut pairs = FULL.to_vec();
        pairs.retain(|(k, _)| *k != "ProgramFiles");
        pairs.push(("ProgramFiles", r"C:\Program Files (x86)"));
        pairs.push(("ProgramW6432", r"C:\Program Files"));
        let e = Env::from_lookup(lookup(&pairs)).expect("env");
        assert_eq!(e.program_files, r"C:\Program Files");
    }

    #[test]
    fn refusal_text_covers_every_refusal() {
        let all = [
            Refusal::NotUnderRoot,
            Refusal::ProtectedFolder,
            Refusal::DriveRoot,
            Refusal::Missing,
            Refusal::KindChanged,
            Refusal::ScanRunning,
            Refusal::Unresolved,
        ];
        let mut texts: Vec<&str> = all.iter().map(|r| refusal_text(*r)).collect();
        for t in &texts {
            assert!(t.ends_with('.'), "{t:?}");
        }
        texts.sort();
        texts.dedup();
        assert_eq!(texts.len(), all.len(), "texts must differ");
        assert!(refusal_text(Refusal::Unresolved).contains("real path"));
    }

    #[test]
    fn mode_for_table() {
        for (shift, want) in [(false, DeleteMode::RecycleBin), (true, DeleteMode::Permanent)] {
            assert_eq!(mode_for(shift), want, "shift={shift}");
        }
    }

    #[test]
    fn win32_codes_map_to_reasons() {
        for (code, want) in [
            (5, SkipReason::AccessDenied),
            (32, SkipReason::Locked),
            (33, SkipReason::Locked),
            (206, SkipReason::TooLong),
            (2, SkipReason::Other(2)),
            (0, SkipReason::Other(0)),
        ] {
            assert_eq!(reason_from_win32(code), want, "code={code}");
        }
    }

    fn skip(path: &str, reason: SkipReason) -> Skipped {
        Skipped { path: PathBuf::from(path), reason }
    }

    fn has(lines: &[String], needle: &str) -> bool {
        lines.iter().any(|l| l.contains(needle))
    }

    #[test]
    fn summary_all_removed_permanent() {
        let o = Outcome { removed_items: 3, removed_bytes: 2048, ..Outcome::default() };
        let lines = summary_lines(DeleteMode::Permanent, &o);
        assert_eq!(lines[0], "Deleted 3 items.");
        assert!(has(&lines, "Freed 2.0 KB."), "{lines:?}");
        assert!(!has(&lines, "skipped"), "{lines:?}");
        assert!(!has(&lines, "stopped"), "{lines:?}");
    }

    #[test]
    fn summary_one_item_uses_the_singular() {
        let o = Outcome { removed_items: 1, ..Outcome::default() };
        assert_eq!(summary_lines(DeleteMode::Permanent, &o)[0], "Deleted 1 item.");
    }

    #[test]
    fn summary_recycle_says_space_is_freed_when_the_bin_is_emptied() {
        let o = Outcome { removed_items: 1, removed_bytes: 5000, ..Outcome::default() };
        let lines = summary_lines(DeleteMode::RecycleBin, &o);
        assert_eq!(lines[0], "Moved 1 item to the Recycle Bin.");
        assert!(has(&lines, "Windows frees the disk space only when you empty the Recycle Bin."), "{lines:?}");
    }

    #[test]
    fn summary_permanent_does_not_mention_the_bin() {
        let o = Outcome { removed_items: 2, removed_bytes: 10, ..Outcome::default() };
        assert!(!has(&summary_lines(DeleteMode::Permanent, &o), "Recycle Bin"));
    }

    #[test]
    fn summary_some_skipped() {
        let o = Outcome {
            removed_items: 5,
            removed_bytes: 100,
            skipped: vec![skip(r"C:\a", SkipReason::Locked), skip(r"C:\b", SkipReason::TooLong)],
            stopped: false,
        };
        let lines = summary_lines(DeleteMode::Permanent, &o);
        assert!(has(&lines, "Skipped 2 items."), "{lines:?}");
        assert!(!has(&lines, "Restart as administrator"), "{lines:?}");
    }

    #[test]
    fn summary_stopped() {
        let o = Outcome { removed_items: 4, stopped: true, ..Outcome::default() };
        let lines = summary_lines(DeleteMode::Permanent, &o);
        assert!(has(&lines, "You stopped the operation."), "{lines:?}");
        assert!(has(&lines, "Items that Volatus removed stay removed."), "{lines:?}");
    }

    #[test]
    fn summary_needs_admin_offers_restart() {
        let o = Outcome {
            removed_items: 0,
            skipped: vec![skip(r"C:\a", SkipReason::NeedsAdmin)],
            ..Outcome::default()
        };
        let lines = summary_lines(DeleteMode::Permanent, &o);
        assert!(has(&lines, "Restart as administrator"), "{lines:?}");
    }

    #[test]
    fn summary_nothing_removed_has_no_recycle_line() {
        let o = Outcome { skipped: vec![skip(r"C:\a", SkipReason::Locked)], ..Outcome::default() };
        let lines = summary_lines(DeleteMode::RecycleBin, &o);
        assert!(!has(&lines, "Recycle Bin"), "{lines:?}");
        assert!(has(&lines, "Skipped 1 item."), "{lines:?}");
    }
}
