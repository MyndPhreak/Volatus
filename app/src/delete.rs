//! Delete policy: which items may go, how a run is reported. Pure logic, no UI and no deletion.
//! The checks come first and fail closed: an unknown target is refused, never deleted.

#![cfg_attr(not(test), expect(dead_code, reason = "wired in the UI task"))]

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
    /// Reads the process environment. A missing system variable falls back to its usual path, so
    /// the list never loses an entry silently. `user_profile` has no usual path; an empty one is
    /// left out of the list.
    pub fn from_process() -> Env {
        let var = |name: &str, fallback: &str| {
            std::env::var(name).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| fallback.to_string())
        };
        Env {
            windir: var("SystemRoot", r"C:\Windows"),
            program_files: var("ProgramFiles", r"C:\Program Files"),
            program_files_x86: var("ProgramFiles(x86)", r"C:\Program Files (x86)"),
            program_data: var("ProgramData", r"C:\ProgramData"),
            user_profile: var("USERPROFILE", ""),
        }
    }
}

/// The folders that may not be deleted as the item itself: `Windows` first (it also protects
/// everything inside it), then the four whose children are allowed. Empty values are left out.
pub fn protected_folders(env: &Env) -> Vec<PathBuf> {
    [&env.windir, &env.program_files, &env.program_files_x86, &env.program_data, &env.user_profile]
        .into_iter()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .collect()
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
    check_target_in(root, target, is_dir, fs, &Env::from_process())
}

/// `check_target` with the environment given, so tests never read the real one.
pub fn check_target_in(
    root: &Path,
    target: &Path,
    is_dir: bool,
    fs: &dyn Probe,
    env: &Env,
) -> Result<(), Refusal> {
    let _ = (root, target, is_dir, fs, env);
    Err(Refusal::ProtectedFolder)
}

/// Plain Delete is the Recycle Bin. Only Shift gives the permanent delete.
pub fn mode_for(shift: bool) -> DeleteMode {
    let _ = shift;
    DeleteMode::Permanent
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
    SkipReason::Other(code)
}

/// The result text, one short sentence per line (ASD-STE100).
pub fn summary_lines(mode: DeleteMode, o: &Outcome) -> Vec<String> {
    let _ = (mode, o);
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A map-backed file system: key is the `norm` form, value is `is_dir`.
    struct Fake(HashMap<String, bool>);

    impl Fake {
        fn with(items: &[(&str, bool)]) -> Fake {
            Fake(items.iter().map(|(p, d)| (norm(Path::new(p)), *d)).collect())
        }
    }

    impl Probe for Fake {
        fn exists(&self, p: &Path) -> bool {
            self.0.contains_key(&norm(p))
        }
        fn is_dir(&self, p: &Path) -> bool {
            self.0.get(&norm(p)).copied().unwrap_or(false)
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
    fn protected_list_holds_on_every_volume_root_scan() {
        // The user profile is refused even when the scan started at the profile's parent.
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
    fn protected_folders_come_from_the_env_and_skip_empty_values() {
        let mut e = env();
        e.user_profile = String::new();
        let list = protected_folders(&e);
        assert_eq!(list.len(), 4);
        assert_eq!(list[0], PathBuf::from(r"C:\Windows"));
        assert_eq!(protected_folders(&env()).len(), 5);
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
        assert!(has(&lines, "Disk space is freed only when you empty the Recycle Bin."), "{lines:?}");
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
        assert!(has(&lines, "You stopped the delete."), "{lines:?}");
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
