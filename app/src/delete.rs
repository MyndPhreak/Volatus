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
    /// The path is not a local drive path (a network share, for example).
    NotLocal,
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
    /// The real path of the item to delete. A link item stays the link.
    fn resolve_target(&self, p: &Path) -> Option<PathBuf>;
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
    /// A link item (any reparse point) is not followed: its parent is resolved and the name joined
    /// back, so the link is what gets deleted. Any other item is resolved in full, so an 8.3 short
    /// name becomes the long name. An item that is gone has its parent resolved; `exists` then says
    /// `Missing`.
    fn resolve_target(&self, p: &Path) -> Option<PathBuf> {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let by_parent = || {
            let name = p.file_name()?;
            std::fs::canonicalize(p.parent()?).ok().map(|dir| dir.join(name))
        };
        match std::fs::symlink_metadata(p) {
            Ok(m) if m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => by_parent(),
            Ok(_) => std::fs::canonicalize(p).ok(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => by_parent(),
            Err(_) => None,
        }
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
    /// Builds the environment from a lookup. `None` if the Windows folder or the user profile is
    /// unset or empty: the policy cannot name what to protect, so it refuses everything.
    /// `ProgramW6432` (the 64-bit Program Files) wins over `ProgramFiles` when present. The other
    /// folders may be empty: the every-volume rule in `check_target_in` still covers them.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Option<Env> {
        let var = |name: &str| get(name).filter(|v| !v.is_empty());
        Some(Env {
            windir: var("SystemRoot").or_else(|| var("windir"))?,
            program_files: var("ProgramW6432").or_else(|| var("ProgramFiles")).unwrap_or_default(),
            program_files_x86: var("ProgramFiles(x86)").unwrap_or_default(),
            program_data: var("ProgramData").unwrap_or_default(),
            user_profile: var("USERPROFILE")?,
        })
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

/// The folders taken from the environment that may not be deleted: `Windows` with everything
/// inside it, and the four whose children are allowed. Empty values are left out.
pub fn protected_folders(env: &Env) -> Vec<(PathBuf, Scope)> {
    [
        (&env.windir, Scope::Subtree),
        (&env.program_files, Scope::Exact),
        (&env.program_files_x86, Scope::Exact),
        (&env.program_data, Scope::Exact),
        (&env.user_profile, Scope::Exact),
    ]
    .into_iter()
    .filter(|(v, _)| !v.is_empty())
    .map(|(v, scope)| (PathBuf::from(v), scope))
    .collect()
}

/// The same names on every volume, whatever the environment says: `<volume>\windows` with its
/// contents, and the three Program folders themselves. `t` is in `norm` form.
fn on_any_volume(t: &str) -> Option<Scope> {
    let rest = t.get(1..3).filter(|c| *c == r":\").and_then(|_| t.get(3..))?;
    if within(rest, "windows") {
        Some(Scope::Subtree)
    } else if ["program files", "program files (x86)", "programdata"].contains(&rest) {
        Some(Scope::Exact)
    } else {
        None
    }
}

/// The text for a refusal, one short sentence (ASD-STE100).
pub fn refusal_text(r: Refusal) -> &'static str {
    match r {
        Refusal::NotUnderRoot => "The item is not inside the scanned folder.",
        Refusal::ProtectedFolder => "Windows needs this folder. You cannot delete it.",
        Refusal::DriveRoot => "You cannot delete a whole drive.",
        Refusal::Missing => "The item is already gone.",
        Refusal::KindChanged => "The item changed after the scan. Scan again.",
        Refusal::ScanRunning => "A scan is running. Wait until it ends.",
        Refusal::Unresolved => "Volatus cannot find the real path of the item. It does not delete the item.",
        Refusal::NotLocal => "Volatus deletes items on local drives only.",
    }
}

/// Lowercase, `\\?\` stripped, `/` as `\`, trailing dots and spaces of each name dropped (Windows
/// ignores them), trailing separators trimmed. The form paths compare in.
fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().to_lowercase().replace('/', r"\");
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    let parts: Vec<&str> =
        s.split('\\').map(|c| if c == "." || c == ".." { c } else { c.trim_end_matches(['.', ' ']) }).collect();
    parts.join(r"\").trim_end_matches('\\').to_string()
}

/// True when `child` is `parent` or inside it. Both are in `norm` form.
fn within(child: &str, parent: &str) -> bool {
    child == parent || child.strip_prefix(parent).is_some_and(|rest| rest.starts_with('\\'))
}

/// Checks the item against the scan root, the protected list and the disk. The root and the item
/// are resolved first (`..`, links, trailing dots); if either cannot be resolved the result is
/// `Unresolved`. Then, in this order: `NotUnderRoot`, `DriveRoot`, `NotUnderRoot` (the root
/// itself), `ProtectedFolder`, `Missing`, `KindChanged`. Reads the real environment for the
/// protected list; if that is unknown, everything is `Unresolved`.
pub fn check_target(root: &Path, target: &Path, is_dir: bool, fs: &dyn Probe) -> Result<PathBuf, Refusal> {
    check_target_with(|k| std::env::var(k).ok(), root, target, is_dir, fs)
}

/// `check_target` with the environment lookup given, so tests never read the real one.
pub fn check_target_with(
    get: impl Fn(&str) -> Option<String>,
    root: &Path,
    target: &Path,
    is_dir: bool,
    fs: &dyn Probe,
) -> Result<PathBuf, Refusal> {
    match Env::from_lookup(get) {
        Some(env) => check_target_in(root, target, is_dir, fs, &env),
        None => Err(Refusal::Unresolved),
    }
}

/// True for `<letter>:` or `<letter>:\...` in `norm` form.
fn is_local(n: &str) -> bool {
    let b = n.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\')
}

/// A network path, in `norm` form: `\\server\...`, `\\.\...` or `\\?\UNC\...` (which `norm` shows
/// as `unc\...`).
fn is_unc(n: &str) -> bool {
    n.starts_with(r"\\") || n.starts_with(r"unc\")
}

/// `check_target` with the environment given, so tests never read the real one. The Ok value is
/// the RESOLVED path: the caller must delete exactly that path, so the approved path and the
/// deleted path are the same.
pub fn check_target_in(
    root: &Path,
    target: &Path,
    is_dir: bool,
    fs: &dyn Probe,
    env: &Env,
) -> Result<PathBuf, Refusal> {
    // Only local drive paths are accepted. A network path is refused before anything else.
    if is_unc(&norm(root)) || is_unc(&norm(target)) {
        return Err(Refusal::NotLocal);
    }
    // A relative or drive-relative path (`C:`, `C:x`, `\`) means nothing without a current
    // directory; an item with no name (`x\..`) is not one item. Only a drive root has no name.
    let drive_root = scan::is_volume_root(target);
    if !root.is_absolute() || !target.is_absolute() || (target.file_name().is_none() && !drive_root) {
        return Err(Refusal::Unresolved);
    }
    // A miss here never falls back to the raw path: the text of a path proves nothing.
    let root = fs.resolve(root).ok_or(Refusal::Unresolved)?;
    let target = fs.resolve_target(target).ok_or(Refusal::Unresolved)?;
    let (r, t) = (norm(&root), norm(&target));
    if is_unc(&r) || is_unc(&t) || !is_local(&r) || !is_local(&t) {
        return Err(Refusal::NotLocal);
    }
    if !within(&t, &r) {
        return Err(Refusal::NotUnderRoot);
    }
    // A drive root is named before the root check below, so a scan of `C:\` says "drive root".
    if t.len() == 2 || scan::is_volume_root(&target) {
        return Err(Refusal::DriveRoot);
    }
    if t == r {
        return Err(Refusal::NotUnderRoot);
    }
    // Each environment entry is compared as given and as resolved (8.3 names, links). The Windows
    // folder and the user profile must resolve; a Program folder may be absent.
    let mut list: Vec<(String, Scope)> = Vec::new();
    for (p, scope) in protected_folders(env) {
        list.push((norm(&p), scope));
        match fs.resolve(&p) {
            Some(resolved) => list.push((norm(&resolved), scope)),
            None if p == Path::new(&env.windir) || p == Path::new(&env.user_profile) => {
                return Err(Refusal::Unresolved);
            }
            None => {}
        }
    }
    let from_env = list.iter().any(|(p, scope)| match scope {
        Scope::Subtree => within(&t, p),
        Scope::Exact => t == *p,
    });
    if from_env || on_any_volume(&t).is_some() {
        return Err(Refusal::ProtectedFolder);
    }
    if !fs.exists(&target) {
        return Err(Refusal::Missing);
    }
    if fs.is_dir(&target) != is_dir {
        return Err(Refusal::KindChanged);
    }
    Ok(target)
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
                lines.push("Windows frees the disk space only when you empty the Recycle Bin.".to_string());
            }
            DeleteMode::Permanent => {
                lines.push(format!("Deleted {}.", items(o.removed_items)));
                lines.push(format!("Freed {}.", format_size(o.removed_bytes, Unit::Dynamic)));
            }
        }
    }
    if o.stopped {
        lines.push("You stopped the operation.".to_string());
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
    /// under it) that resolves to another path. `shorts`: 8.3 names that resolve to long names.
    /// `broken`: paths `resolve` cannot answer for. `resolve` also does `..` and trailing dots, as
    /// `canonicalize` does; `resolve_target` does not follow a link item, as `RealFs` does.
    struct Fake {
        items: HashMap<String, bool>,
        links: Vec<(String, String)>,
        shorts: Vec<(String, String)>,
        broken: Vec<String>,
    }

    impl Fake {
        fn with(items: &[(&str, bool)]) -> Fake {
            Fake {
                items: items.iter().map(|(p, d)| (norm(Path::new(p)), *d)).collect(),
                links: Vec::new(),
                shorts: Vec::new(),
                broken: Vec::new(),
            }
        }
        fn link(mut self, from: &str, to: &str) -> Fake {
            self.links.push((norm(Path::new(from)), norm(Path::new(to))));
            self
        }
        fn short(mut self, from: &str, to: &str) -> Fake {
            self.shorts.push((norm(Path::new(from)), norm(Path::new(to))));
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
            // A bare `C:` is drive-relative: it must not turn into `c:\`.
            if n.contains('\\') && !out.contains('\\') {
                out.push('\\');
            }
            let key = out.trim_end_matches('\\').to_string();
            if self.broken.iter().any(|b| within(&key, b)) {
                return None;
            }
            for (from, to) in self.shorts.iter().chain(self.links.iter()) {
                if let Some(rest) = key.strip_prefix(from.as_str()).filter(|r| r.is_empty() || r.starts_with('\\')) {
                    return Some(PathBuf::from(format!("{to}{rest}")));
                }
            }
            Some(PathBuf::from(out))
        }
        fn resolve_target(&self, p: &Path) -> Option<PathBuf> {
            let key = norm(p);
            if self.links.iter().any(|(from, _)| *from == key) {
                let name = p.file_name()?;
                return self.resolve(p.parent()?).map(|d| d.join(name));
            }
            self.resolve(p)
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
        check_target_in(Path::new(root), Path::new(target), true, fs, &env()).map(|_| ())
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
        let got = check_target_in(Path::new(r"C:\data"), Path::new(r"C:\data\x"), false, &fs, &env()).map(|_| ());
        assert_eq!(got, Err(Refusal::KindChanged));
    }

    #[test]
    fn a_file_of_the_expected_kind_is_ok() {
        let fs = Fake::with(&[(r"C:\data\x.txt", false)]);
        let got = check_target_in(Path::new(r"C:\data"), Path::new(r"C:\data\x.txt"), false, &fs, &env()).map(|_| ());
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
        let got = check_target_in(Path::new(r"C:\data"), Path::new(r"C:\data\lnk"), false, &fs, &env()).map(|_| ());
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
            Refusal::NotLocal,
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
    fn unc_paths_are_not_local() {
        let fs = all_dirs(&[]);
        for (root, t) in [
            (r"\\localhost\C$\", r"\\localhost\C$\Windows"),
            (r"\\?\UNC\localhost\C$\", r"\\?\UNC\localhost\C$\Windows"),
            (r"\\server\share", r"\\server\share\x"),
            (r"C:\", r"\\server\share\x"),
            (r"\\server\share", r"C:\x"),
        ] {
            assert_eq!(check(root, t, &fs), Err(Refusal::NotLocal), "{root} {t}");
        }
    }

    #[test]
    fn short_names_resolve_to_the_protected_long_names() {
        let fs = all_dirs(&[r"C:\Program Files\App"])
            .short(r"C:\PROGRA~1", r"C:\Program Files")
            .short(r"C:\PROGRA~3", r"C:\ProgramData")
            .short(r"C:\Users\DEV~1", r"C:\Users\dev");
        for p in [r"C:\PROGRA~1", r"C:\PROGRA~3", r"C:\Users\DEV~1", r"c:\progra~1\"] {
            assert_eq!(check(r"C:\", p, &fs), Err(Refusal::ProtectedFolder), "{p}");
        }
        // A child through the short name is a child of the long name: allowed.
        assert_eq!(check(r"C:\", r"C:\PROGRA~1\App", &fs), Ok(()));
    }

    #[test]
    fn a_link_item_named_like_a_short_name_is_allowed() {
        let fs = Fake::with(&[(r"C:\PROGRA~1", false)]).link(r"C:\PROGRA~1", r"C:\Elsewhere");
        let got = check_target_in(Path::new(r"C:\"), Path::new(r"C:\PROGRA~1"), false, &fs, &env());
        assert_eq!(got.map(|_| ()), Ok(()));
    }

    #[test]
    fn a_target_that_is_not_absolute_or_has_no_name_is_unresolved() {
        let fs = all_dirs(&[r"C:\data"]);
        for t in [r"C:", r"C:x", r"\", r"data\x", r"C:\data\.."] {
            assert_eq!(check(r"C:\", t, &fs), Err(Refusal::Unresolved), "{t}");
        }
        assert_eq!(check(r"C:", r"C:\data", &fs), Err(Refusal::Unresolved));
    }

    #[test]
    fn ok_returns_the_resolved_path() {
        let fs = all_dirs(&[r"C:\data\x"]);
        let got = check_target_in(Path::new(r"C:\data"), Path::new(r"C:\data\SUB\..\x"), true, &fs, &env());
        assert_eq!(got, Ok(PathBuf::from(r"c:\data\x")));
    }

    #[test]
    fn a_user_profile_in_short_form_still_protects_the_profile() {
        let fs = all_dirs(&[]).short(r"C:\Users\DEV~1", r"C:\Users\dev");
        let mut e = env();
        e.user_profile = r"C:\Users\DEV~1".into();
        let got = check_target_in(Path::new(r"C:\"), Path::new(r"C:\Users\dev"), true, &fs, &e);
        assert_eq!(got, Err(Refusal::ProtectedFolder));
    }

    #[test]
    fn a_user_profile_that_does_not_resolve_is_unresolved() {
        let fs = all_dirs(&[r"C:\data\x"]).broken(r"C:\Users\ghost");
        let mut e = env();
        e.user_profile = r"C:\Users\ghost".into();
        let got = check_target_in(Path::new(r"C:\data"), Path::new(r"C:\data\x"), true, &fs, &e);
        assert_eq!(got, Err(Refusal::Unresolved));
    }

    #[test]
    fn a_windows_dir_that_does_not_resolve_is_unresolved() {
        let fs = all_dirs(&[r"C:\data\x"]).broken(r"C:\Windows");
        let got = check_target_in(Path::new(r"C:\data"), Path::new(r"C:\data\x"), true, &fs, &env());
        assert_eq!(got, Err(Refusal::Unresolved));
    }

    #[test]
    fn a_program_folder_that_does_not_resolve_falls_back_to_its_raw_path() {
        // An absent `Program Files (x86)` is legal on a 32-bit machine.
        let fs = all_dirs(&[r"C:\data\x"]).broken(r"C:\Program Files (x86)");
        let got = check_target_in(Path::new(r"C:\data"), Path::new(r"C:\data\x"), true, &fs, &env());
        assert_eq!(got.map(|_| ()), Ok(()));
    }

    #[test]
    fn check_target_with_no_env_is_unresolved_and_with_an_env_checks() {
        let fs = all_dirs(&[r"C:\data\x"]);
        let (root, t) = (Path::new(r"C:\data"), Path::new(r"C:\data\x"));
        assert_eq!(check_target_with(lookup(&[]), root, t, true, &fs), Err(Refusal::Unresolved));
        assert_eq!(check_target_with(lookup(&FULL), root, t, true, &fs), Ok(PathBuf::from(r"c:\data\x")));
        let win = all_dirs(&[]);
        let got = check_target_with(lookup(&FULL), Path::new(r"C:\"), Path::new(r"C:\Windows"), true, &win);
        assert_eq!(got, Err(Refusal::ProtectedFolder));
    }

    #[test]
    fn from_lookup_with_a_relative_windir_or_profile_is_none() {
        for (key, bad) in [("SystemRoot", r"Windows"), ("USERPROFILE", r"Users\dev"), ("USERPROFILE", "C:dev")] {
            let mut pairs = FULL.to_vec();
            pairs.retain(|(k, _)| *k != key);
            pairs.push((key, bad));
            assert!(Env::from_lookup(lookup(&pairs)).is_none(), "{key}={bad}");
        }
    }

    /// Real file system tests. Every path is under one unique directory in the temp folder that
    /// the test creates and removes. A case is skipped (and says so) only when the OS refuses to
    /// create its fixture.
    mod real {
        use super::*;
        use std::fs;
        use std::os::windows::ffi::OsStrExt;

        /// A unique directory, canonical (verbatim) form. Removed on drop; a junction inside is
        /// removed first, so nothing outside is ever followed.
        struct Dir {
            base: PathBuf,
            junctions: std::cell::RefCell<Vec<PathBuf>>,
        }

        impl Dir {
            fn new(tag: &str) -> Dir {
                let plain = std::env::temp_dir().join(format!("volatus-delete-{}-{tag}", std::process::id()));
                let _ = fs::remove_dir_all(&plain);
                fs::create_dir_all(&plain).expect("temp dir");
                Dir { base: fs::canonicalize(&plain).expect("canonical"), junctions: Default::default() }
            }
            /// `mklink /J`. `None` (skip) if the OS refuses.
            fn junction(&self, name: &str, to: &Path) -> Option<PathBuf> {
                let link = self.base.join(name);
                let plain = |p: &Path| p.to_string_lossy().trim_start_matches(r"\\?\").to_string();
                let ok = std::process::Command::new("cmd")
                    .args(["/c", "mklink", "/J", &plain(&link), &plain(to)])
                    .output()
                    .is_ok_and(|o| o.status.success());
                if ok {
                    self.junctions.borrow_mut().push(link.clone());
                    Some(link)
                } else {
                    eprintln!("skipped: mklink /J was refused");
                    None
                }
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                for j in self.junctions.borrow().iter() {
                    let _ = fs::remove_dir(j);
                }
                let _ = fs::remove_dir_all(&self.base);
            }
        }

        fn plain(p: &Path) -> PathBuf {
            PathBuf::from(p.to_string_lossy().trim_start_matches(r"\\?\"))
        }

        #[test]
        fn a_name_with_a_trailing_dot_stays_itself() {
            let d = Dir::new("dot");
            fs::create_dir(d.base.join("dotdir")).unwrap();
            if fs::create_dir(d.base.join("dotdir.")).is_err() {
                eprintln!("skipped: the OS refused a name with a trailing dot");
                return;
            }
            fs::write(d.base.join("dotdir.").join("mark"), b"x").unwrap();
            let got = RealFs.resolve_target(&plain(&d.base.join("dotdir."))).expect("resolves");
            assert_eq!(got.file_name().unwrap(), "dotdir.", "{got:?}");
            assert!(got.join("mark").exists(), "{got:?} is not the dotdir. folder");
        }

        #[test]
        fn a_junction_item_is_not_followed() {
            let d = Dir::new("junction");
            fs::create_dir(d.base.join("other")).unwrap();
            fs::write(d.base.join("other").join("keep"), b"x").unwrap();
            let Some(link) = d.junction("link", &d.base.join("other")) else { return };
            let got = RealFs.resolve_target(&plain(&link)).expect("resolves");
            assert_eq!(got, link);
            assert!(d.base.join("other").join("keep").exists());
        }

        #[test]
        fn a_short_name_of_a_junction_item_becomes_its_long_name() {
            let d = Dir::new("shortlink");
            fs::create_dir(d.base.join("other")).unwrap();
            let Some(link) = d.junction("a long junction name", &d.base.join("other")) else { return };
            let wide: Vec<u16> = link.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut buf = [0u16; 1024];
            // SAFETY: `wide` is NUL-terminated and `buf` is 1024 wide chars, as the length says.
            let n = unsafe {
                windows_sys::Win32::Storage::FileSystem::GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), 1024)
            } as usize;
            let short = PathBuf::from(String::from_utf16_lossy(&buf[..n]));
            if n == 0 || short == link {
                eprintln!("skipped: no 8.3 name for the junction (8.3 names are off)");
                return;
            }
            let got = RealFs.resolve_target(&plain(&short)).expect("resolves");
            assert_eq!(got.file_name().unwrap(), "a long junction name", "{got:?}");
        }

        #[test]
        fn a_normal_item_resolves_to_its_canonical_long_form() {
            let d = Dir::new("normal");
            fs::create_dir(d.base.join("NormalItem")).unwrap();
            let got = RealFs.resolve_target(&plain(&d.base.join("normalitem"))).expect("resolves");
            assert_eq!(got, d.base.join("NormalItem"));
        }

        #[test]
        fn a_missing_item_is_missing_and_a_missing_parent_is_unresolved() {
            let d = Dir::new("missing");
            let root = plain(&d.base);
            let got = check_target_in(&root, &root.join("nope"), true, &RealFs, &env());
            assert_eq!(got, Err(Refusal::Missing));
            let got = check_target_in(&root, &root.join("no").join("x"), true, &RealFs, &env());
            assert_eq!(got, Err(Refusal::Unresolved));
        }
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
