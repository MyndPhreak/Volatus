//! The decisions of the context menu, with no Windows call except the disk read `read_disk`: when
//! a menu may open, which item it is for, what the disk says about it, which Volatus items it
//! shows, what an id the shell menu returns means, which flags the shell calls get, and what a
//! change on disk after an action means for the map. The shell calls themselves are in `shell`
//! (P4); they use the traits and functions here.

use crate::model::ScanState;
use scan::{NodeId, Tree};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What the disk holds at a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disk {
    /// Nothing there (or the path cannot name anything on disk, as a name made lossy at scan time).
    Gone,
    /// A folder. A junction or directory symlink is not one: the scan stores it as a file.
    Dir,
    /// A file, or a junction or symlink, with its size in bytes.
    File(u64),
    /// The disk could not say (access denied or another error).
    Unreadable,
}

/// `Disk` of the result of `symlink_metadata`. A name-surrogate link (junction, symlink) is a
/// `File`, as the scan stores it (never followed, size 0); any error but "not found" is `Unreadable`.
pub fn disk_of(m: std::io::Result<std::fs::Metadata>) -> Disk {
    match m {
        // std: a symlink or a junction (the name-surrogate reparse points the scan never follows).
        Ok(m) if m.file_type().is_symlink() => Disk::File(m.len()),
        Ok(m) if m.is_dir() => Disk::Dir,
        Ok(m) => Disk::File(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Disk::Gone,
        Err(_) => Disk::Unreadable,
    }
}

/// `Disk` of `p`, read now. Plain `symlink_metadata`: the path is not followed through a link.
pub fn read_disk(p: &Path) -> Disk {
    disk_of(std::fs::symlink_metadata(p))
}

/// The item a menu is for, checked against the disk before the shell sees its path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prepared {
    /// The path (raw names from the tree, never `display_name`) and what the disk held at it.
    Ready { path: PathBuf, before: Disk },
    /// The disk does not match the scan: no shell menu for this item.
    Changed,
}

/// The path of node `n` and its disk check. `Ready` only when the disk holds a folder for a
/// folder node, or a file for a file node; anything else, an error included, is `Changed`.
pub fn prepare(tree: &Tree, n: NodeId, disk: impl Fn(&Path) -> Disk) -> Prepared {
    let path = tree.path(n);
    let before = disk(&path);
    match (&before, tree.node(n).is_dir) {
        (Disk::Dir, true) | (Disk::File(_), false) => Prepared::Ready { path, before },
        _ => Prepared::Changed,
    }
}

/// True when a menu may open: a finished scan whose data is loaded.
pub fn allowed(state: &ScanState, loaded: bool) -> bool {
    loaded && matches!(state, ScanState::Done(_))
}

/// A Volatus menu item. The value is the menu id.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    ExploreFolder = 1,
    CommandPrompt,
    CopyPath,
    /// Opens the folder in the map. It does not scan.
    ShowFolder,
    Rescan,
}

/// A line of the Volatus part of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Action(Cmd, &'static str),
    Disabled(&'static str),
    Separator,
}

/// The Volatus items for a prepared item. `is_dir`: the node is a folder. `shell_ok`: the shell
/// menu was built; if not, a disabled line says so.
pub fn items(p: &Prepared, is_dir: bool, shell_ok: bool) -> Vec<Item> {
    if matches!(p, Prepared::Changed) {
        return vec![Item::Disabled("This item changed on disk. Rescan."), Item::Action(Cmd::Rescan, "Rescan")];
    }
    let mut out = vec![
        Item::Action(Cmd::ExploreFolder, "Explore Folder"),
        Item::Action(Cmd::CommandPrompt, "Command Prompt Here"),
        Item::Action(Cmd::CopyPath, "Copy Path"),
    ];
    if is_dir {
        out.push(Item::Action(Cmd::ShowFolder, "Show this folder"));
    }
    out.push(Item::Separator);
    if !shell_ok {
        out.push(Item::Disabled("Windows has no menu for this item."));
    }
    out
}

/// First menu id of the shell's items.
pub const SHELL_FIRST: u32 = 0x100;
/// Last menu id of the shell's items.
pub const SHELL_LAST: u32 = 0x7FFF;

/// What a menu id means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picked {
    /// No pick, or an id that is none of ours (a shell separator has one too).
    Nothing,
    Volatus(Cmd),
    /// A shell item: its offset from `SHELL_FIRST`, as `IContextMenu` wants it.
    Shell(u32),
}

/// What `id` (the result of `TrackPopupMenuEx`, 0 for no pick) means. Total.
pub fn picked(id: u32) -> Picked {
    match id {
        1 => Picked::Volatus(Cmd::ExploreFolder),
        2 => Picked::Volatus(Cmd::CommandPrompt),
        3 => Picked::Volatus(Cmd::CopyPath),
        4 => Picked::Volatus(Cmd::ShowFolder),
        5 => Picked::Volatus(Cmd::Rescan),
        SHELL_FIRST..=SHELL_LAST => Picked::Shell(id - SHELL_FIRST),
        _ => Picked::Nothing,
    }
}

/// `uFlags` of `QueryContextMenu`: `CMF_NORMAL`, plus `CMF_EXTENDEDVERBS` with Shift. Never
/// `CMF_CANRENAME`: Volatus does not rename.
pub fn query_flags(shift: bool) -> u32 {
    if shift { CMF_EXTENDEDVERBS } else { CMF_NORMAL }
}

/// `fMask` of `CMINVOKECOMMANDINFOEX`: Unicode and a point, plus the keys held at invoke.
pub fn invoke_mask(shift: bool, ctrl: bool) -> u32 {
    let mut mask = CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE;
    if shift {
        mask |= CMIC_MASK_SHIFT_DOWN;
    }
    if ctrl {
        mask |= CMIC_MASK_CONTROL_DOWN;
    }
    mask
}

/// True only for the canonical verb `delete` of the shell (ASCII case). A verb the shell could not
/// give (`None`) is not delete: only the disk can then change the map, as "outdated".
pub fn is_delete(verb: Option<&str>) -> bool {
    verb.is_some_and(|v| v.eq_ignore_ascii_case("delete"))
}

/// True for the result of `InvokeCommand` that means the user cancelled (`COPYENGINE_E_USER_CANCELLED`).
/// The shell's delete returns after the item is gone, so a cancelled one needs no check.
pub fn user_cancelled(hr: i32) -> bool {
    hr == COPYENGINE_E_USER_CANCELLED
}

/// What the map does after an action, from the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Look again later.
    Wait,
    /// The item was deleted: remove it from the map.
    Remove,
    /// The item changed in another way: show "outdated" and Rescan.
    Outdated,
    /// No change.
    Nothing,
}

/// The outcome of one check. `delete`: the action was the shell's delete. `before`, `now`: the
/// disk at the menu and at this check. `last`: no check follows.
pub fn judge(delete: bool, before: &Disk, now: &Disk, last: bool) -> Outcome {
    match now {
        Disk::Unreadable => Outcome::Outdated,
        Disk::Gone if delete => Outcome::Remove,
        _ if now == before => if last { Outcome::Nothing } else { Outcome::Wait },
        _ => Outcome::Outdated,
    }
}

/// `CMF_NORMAL` of shobjidl_core.h.
const CMF_NORMAL: u32 = 0;
/// `CMF_EXTENDEDVERBS`: the verbs Explorer shows with Shift held.
const CMF_EXTENDEDVERBS: u32 = 0x100;
/// `CMIC_MASK_UNICODE` (= `SEE_MASK_UNICODE`): `lpVerbW` is valid. Not defined in `windows` 0.62.2.
const CMIC_MASK_UNICODE: u32 = 0x4000;
/// `CMIC_MASK_PTINVOKE`: `ptInvoke` is valid.
const CMIC_MASK_PTINVOKE: u32 = 0x2000_0000;
/// `CMIC_MASK_SHIFT_DOWN`.
const CMIC_MASK_SHIFT_DOWN: u32 = 0x1000_0000;
/// `CMIC_MASK_CONTROL_DOWN`.
const CMIC_MASK_CONTROL_DOWN: u32 = 0x4000_0000;
/// `COPYENGINE_E_USER_CANCELLED`, the result of `InvokeCommand` after "No" or a closed dialog.
const COPYENGINE_E_USER_CANCELLED: i32 = 0x8027_0000_u32 as i32;
/// Times between the checks of a `Follow`: the checks run at 0 s, 0.5 s and 2 s.
const FOLLOW_DELAYS: [Duration; 2] = [Duration::from_millis(500), Duration::from_millis(1500)];

/// The checks after an action: at once, after 0.5 s and after 2 s. UI thread only; the session
/// holds at most one, and a new menu, a scan or exit drops it.
#[derive(Debug, Clone)]
pub struct Follow {
    /// The item the menu was for.
    pub node: NodeId,
    /// `Loaded::edits` at the menu: a removal since then makes the node id stale.
    pub edits: u64,
    pub path: PathBuf,
    /// The action was the shell's delete.
    pub delete: bool,
    /// The disk at the menu.
    pub before: Disk,
    stage: u8,
}

impl Follow {
    pub fn new(node: NodeId, edits: u64, path: PathBuf, delete: bool, before: Disk) -> Self {
        Self { node, edits, path, delete, before, stage: 0 }
    }

    /// The next check, with the disk as `now`: its outcome and the time to the check after it.
    /// `None` for the time when the outcome is a decision or no check is left; after that the
    /// follow gives `(Nothing, None)`.
    pub fn step(&mut self, now: &Disk) -> (Outcome, Option<Duration>) {
        if usize::from(self.stage) > FOLLOW_DELAYS.len() {
            return (Outcome::Nothing, None);
        }
        let last = usize::from(self.stage) == FOLLOW_DELAYS.len();
        let outcome = judge(self.delete, &self.before, now, last);
        let delay = FOLLOW_DELAYS.get(usize::from(self.stage)).copied().filter(|_| outcome == Outcome::Wait);
        // A decision, or the last check, ends the follow.
        self.stage = if delay.is_some() { self.stage + 1 } else { FOLLOW_DELAYS.len() as u8 + 1 };
        (outcome, delay)
    }
}

/// Runs the picked shell item, then gives the checks that follow it (`follow`, made at the pick).
/// The disk is read again first (`disk`): when the path is gone since the menu opened, `invoke`
/// does not run (the shell would ask to confirm, then show "Item Not Found" until it is closed:
/// spike S7) and the checks tell what happened. Else `invoke` runs the item and gives its
/// HRESULT: a cancel by the user (`user_cancelled`) changed nothing and needs no checks (`None`);
/// any other result, an error too, gets the checks: the disk decides.
pub fn after_pick(follow: Follow, disk: impl Fn(&Path) -> Disk, invoke: impl FnOnce() -> i32) -> Option<Follow> {
    if disk(&follow.path) == Disk::Gone {
        return Some(follow);
    }
    (!user_cancelled(invoke())).then_some(follow)
}

/// The folder a command prompt opens in for the item at `path`: a folder itself, a file's folder.
pub fn terminal_folder(path: &Path, is_dir: bool) -> PathBuf {
    if is_dir { path.to_path_buf() } else { path.parent().unwrap_or(path).to_path_buf() }
}

/// Where a menu opens on the screen, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// Its top-left corner is at the pointer.
    Point(i32, i32),
    /// Under a box the menu must not cover (a key opened it, for the item in the box): its left,
    /// top, right and bottom edges.
    Below { left: i32, top: i32, right: i32, bottom: i32 },
}

/// Where a menu was asked for in the window, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum At {
    /// A right-click at this point.
    Pointer(f32, f32),
    /// A key, for the item in the box at (x, y) of this width and height.
    Box(f32, f32, f32, f32),
}

/// The screen anchor of the menu asked for `at`, for a window of `scale` whose client origin is at
/// `origin` on the screen (`screen_point`).
pub fn anchor(at: At, scale: f32, origin: (i32, i32)) -> Anchor {
    match at {
        At::Pointer(x, y) => {
            let (x, y) = screen_point((x, y), scale, origin);
            Anchor::Point(x, y)
        }
        At::Box(x, y, w, h) => {
            let (left, top) = screen_point((x, y), scale, origin);
            let (right, bottom) = screen_point((x + w, y + h), scale, origin);
            Anchor::Below { left, top, right, bottom }
        }
    }
}

/// The screen point, in physical pixels, of the window point `window` in logical pixels, for a
/// window whose client origin is at `origin` on the screen. Rounds half away from zero.
pub fn screen_point(window: (f32, f32), scale: f32, origin: (i32, i32)) -> (i32, i32) {
    let at = |v: f32, o: i32| (v * scale).round() as i32 + o;
    (at(window.0, origin.0), at(window.1, origin.1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Summary;
    use scan::Method;
    use scan::rollup::{RawEntry, rollup};
    use std::ffi::OsString;
    use std::fs;
    use std::io::ErrorKind;
    use std::os::windows::ffi::OsStringExt;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    /// A new empty folder under the system temp folder, removed when dropped. `tag` is unique per test.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("volatus-menu-test-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn summary() -> Summary {
        Summary {
            method: Method::Walk,
            files: 1,
            dirs: 1,
            skipped: 0,
            elapsed: Duration::ZERO,
            phases: Default::default(),
            total: 10,
        }
    }

    /// Scans `root` with the walker.
    fn scanned(root: &Path) -> Tree {
        let (tx, _rx) = mpsc::channel();
        scan::scan_with(root, tx, &AtomicBool::new(false), &scan::ScanOptions { method: Some(Method::Walk), ..Default::default() }).unwrap().0
    }

    /// The node reached from the root by `names`.
    fn find(tree: &Tree, names: &[&str]) -> NodeId {
        names.iter().fold(tree.root(), |n, name| tree.children(n).find(|&c| tree.name(c) == *name).unwrap_or_else(|| panic!("no {name}")))
    }

    // ---- disk_of, read_disk ----
    #[test]
    fn disk_of_a_file_is_file_with_its_size() {
        let dir = TempDir::new("disk-of-file");
        let f = dir.0.join("f.txt");
        fs::write(&f, b"hello").unwrap();

        assert_eq!(disk_of(fs::symlink_metadata(&f)), Disk::File(5));
    }

    #[test]
    fn disk_of_a_folder_is_dir() {
        let dir = TempDir::new("disk-of-dir");

        assert_eq!(disk_of(fs::symlink_metadata(&dir.0)), Disk::Dir);
    }

    #[test]
    fn disk_of_a_junction_is_file_as_the_scan_stores_it() {
        let dir = TempDir::new("disk-of-junction");
        let (target, link) = (dir.0.join("target"), dir.0.join("link"));
        fs::create_dir(&target).unwrap();
        let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&link).arg(&target).output().unwrap();
        assert!(made.status.success(), "mklink failed: {made:?}");

        assert!(matches!(disk_of(fs::symlink_metadata(&link)), Disk::File(_)));
    }

    #[test]
    fn disk_of_not_found_is_gone() {
        assert_eq!(disk_of(Err(ErrorKind::NotFound.into())), Disk::Gone);
    }

    #[test]
    fn disk_of_another_error_is_unreadable() {
        assert_eq!(disk_of(Err(ErrorKind::PermissionDenied.into())), Disk::Unreadable);
    }

    #[test]
    fn read_disk_of_a_missing_path_is_gone() {
        let dir = TempDir::new("read-disk-missing");

        assert_eq!(read_disk(&dir.0.join("nothing")), Disk::Gone);
    }

    // ---- allowed ----
    #[test]
    fn allowed_is_true_only_for_a_finished_scan_with_data() {
        let cases = [
            ("idle", ScanState::Idle, true),
            ("scanning", ScanState::Scanning { files: 1, dirs: 1 }, true),
            ("error", ScanState::Error("x".into()), true),
            ("cancelled", ScanState::Cancelled, true),
            ("done", ScanState::Done(summary()), true),
            ("done without data", ScanState::Done(summary()), false),
        ];

        let got: Vec<(&str, bool)> = cases.iter().map(|(name, state, loaded)| (*name, allowed(state, *loaded))).collect();

        let want = [("idle", false), ("scanning", false), ("error", false), ("cancelled", false), ("done", true), ("done without data", false)];
        assert_eq!(got, want);
    }

    // ---- prepare ----
    /// `C:\scan`{ "d<RLO>x"(1){ g(3) }, "invoice<RLO>fdp.exe"(2) }: names that display_name would change.
    fn spoofed() -> (Tree, NodeId, NodeId, NodeId) {
        let e = |id: u64, parent: u64, name: &str, is_dir: bool| RawEntry { id, parent, name: name.into(), size: 10, allocated: 10, is_dir };
        let entries = vec![e(0, 0, "C:\\scan", true), e(1, 0, "d\u{202E}x", true), e(2, 0, "invoice\u{202E}fdp.exe", false), e(3, 1, "g", false)];
        let tree = rollup(0, entries).unwrap().tree;
        let find = |n: &str| tree.children(tree.root()).find(|&c| tree.name(c) == n).unwrap();
        let (folder, file) = (find("d\u{202E}x"), find("invoice\u{202E}fdp.exe"));
        let g = tree.children(folder).next().unwrap();
        (tree, folder, file, g)
    }

    #[test]
    fn prepare_path_is_the_scan_root_plus_the_raw_names() {
        let (tree, _, _, g) = spoofed();

        let got = prepare(&tree, g, |_| Disk::File(10));

        let want = PathBuf::from("C:\\scan").join("d\u{202E}x").join("g");
        assert_eq!(got, Prepared::Ready { path: want, before: Disk::File(10) });
    }

    #[test]
    fn prepare_a_folder_node_with_a_folder_on_disk_is_ready() {
        let (tree, folder, _, _) = spoofed();

        let got = prepare(&tree, folder, |_| Disk::Dir);

        assert_eq!(got, Prepared::Ready { path: PathBuf::from("C:\\scan").join("d\u{202E}x"), before: Disk::Dir });
    }

    #[test]
    fn prepare_a_disk_that_does_not_match_the_node_is_changed() {
        let (tree, folder, file, _) = spoofed();
        let cases = [
            ("file gone", file, Disk::Gone),
            ("file unreadable", file, Disk::Unreadable),
            ("file is a folder now", file, Disk::Dir),
            ("folder is a file now", folder, Disk::File(10)),
            ("folder gone", folder, Disk::Gone),
            ("folder unreadable", folder, Disk::Unreadable),
        ];

        let got: Vec<(&str, Prepared)> = cases.iter().map(|(name, n, disk)| (*name, prepare(&tree, *n, |_| disk.clone()))).collect();

        assert!(got.iter().all(|(_, p)| *p == Prepared::Changed), "{got:?}");
    }

    #[test]
    fn prepare_name_with_spaces_unicode_and_shell_characters_is_ready() {
        let dir = TempDir::new("prepare-odd");
        let name = "a b ü日本 ; & % , x.txt";
        fs::create_dir(dir.0.join("sub dir ;&%,")).unwrap();
        fs::write(dir.0.join("sub dir ;&%,").join(name), b"hello").unwrap();
        let tree = scanned(&dir.0);
        let n = find(&tree, &["sub dir ;&%,", name]);

        let got = prepare(&tree, n, read_disk);

        assert_eq!(got, Prepared::Ready { path: dir.0.join("sub dir ;&%,").join(name), before: Disk::File(5) });
    }

    #[test]
    fn prepare_path_of_300_characters_is_ready() {
        let dir = TempDir::new("prepare-long");
        let (a, b, file) = ("a".repeat(100), "b".repeat(100), format!("{}.txt", "c".repeat(100)));
        let folder = dir.0.join(&a).join(&b);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(&file), b"hello").unwrap();
        let tree = scanned(&dir.0);
        let n = find(&tree, &[&a, &b, &file]);

        let got = prepare(&tree, n, read_disk);

        assert!(tree.path(n).as_os_str().len() > 300);
        assert_eq!(got, Prepared::Ready { path: folder.join(&file), before: Disk::File(5) });
    }

    #[test]
    fn prepare_name_with_an_unpaired_surrogate_is_changed() {
        let dir = TempDir::new("prepare-surrogate");
        let name = OsString::from_wide(&[0x61, 0xD800, 0x62]);
        fs::write(dir.0.join(&name), b"hello").unwrap();
        let tree = scanned(&dir.0);
        // The scan made the name lossy: the path it gives names no file.
        let n = find(&tree, &["a\u{FFFD}b"]);

        let got = prepare(&tree, n, read_disk);

        assert_eq!(got, Prepared::Changed);
    }

    #[test]
    fn prepare_file_replaced_by_a_folder_after_the_scan_is_changed() {
        let dir = TempDir::new("prepare-replaced");
        fs::write(dir.0.join("x"), b"hello").unwrap();
        let tree = scanned(&dir.0);
        fs::remove_file(dir.0.join("x")).unwrap();
        fs::create_dir(dir.0.join("x")).unwrap();

        let got = prepare(&tree, find(&tree, &["x"]), read_disk);

        assert_eq!(got, Prepared::Changed);
    }

    #[test]
    fn prepare_name_with_a_trailing_dot_next_to_its_plain_twin_is_changed() {
        let dir = TempDir::new("proof-trailing-dot");
        // Made through \\?\: a plain path would lose the dot.
        let verbatim = |name: &str| PathBuf::from(format!(r"\\?\{}\{name}", dir.0.display()));
        fs::write(verbatim("a."), b"hello").unwrap();
        fs::write(verbatim("a"), b"abc").unwrap();
        let tree = scanned(&dir.0);
        let n = find(&tree, &["a."]);
        assert_eq!(tree.node(n).size, 5, "the scan keeps the raw name and its own size");

        let got = prepare(&tree, n, read_disk);

        let _ = fs::remove_file(verbatim("a."));
        assert_eq!(got, Prepared::Changed, "the disk check read another item");
    }

    #[test]
    fn prepare_on_disk_names_that_windows_changes_are_changed_and_the_others_ready() {
        let dir = TempDir::new("prepare-win32-names");
        let verbatim = |name: &str| PathBuf::from(format!(r"\\?\{}\{name}", dir.0.display()));
        let names = ["a.", "a", "b ", "b", "c.", "plain.txt", "u\u{FFFD}v"];
        for name in names {
            fs::write(verbatim(name), b"abc").unwrap();
        }
        let tree = scanned(&dir.0);

        let got: Vec<(&str, bool)> = names.iter().map(|&name| (name, prepare(&tree, find(&tree, &[name]), read_disk) != Prepared::Changed)).collect();

        for name in names {
            let _ = fs::remove_file(verbatim(name));
        }
        let want = [("a.", false), ("a", true), ("b ", false), ("b", true), ("c.", false), ("plain.txt", true), ("u\u{FFFD}v", false)];
        assert_eq!(got, want);
    }

    /// `prepare` of the last node of the chain `root`\`names` (folders, then a file), with a disk
    /// that matches the scan.
    fn prepare_chain(root: &str, names: &[&str]) -> Prepared {
        let mut entries = vec![RawEntry { id: 0, parent: 0, name: root.into(), size: 0, allocated: 0, is_dir: true }];
        for (i, name) in names.iter().enumerate() {
            let id = i as u64 + 1;
            entries.push(RawEntry { id, parent: id - 1, name: (*name).into(), size: 10, allocated: 10, is_dir: i + 1 < names.len() });
        }
        let tree = rollup(0, entries).unwrap().tree;
        let n = find(&tree, names);
        let kind = if tree.node(n).is_dir { Disk::Dir } else { Disk::File(10) };
        prepare(&tree, n, |_| kind.clone())
    }

    #[test]
    fn prepare_any_name_below_the_root_that_windows_would_change_is_changed() {
        let cases: [(&str, &[&str], bool); 13] = [
            ("drive root", &[], true),
            ("plain", &["a"], true),
            ("dot inside", &["a.b"], true),
            ("space inside", &["a b"], true),
            ("trailing dot", &["a."], false),
            ("trailing space", &["a "], false),
            ("only dots", &["..."], false),
            ("folder with a trailing dot", &["d.", "g"], false),
            ("folder with a trailing space", &["d ", "g"], false),
            ("replacement character", &["x\u{FFFD}y"], false),
            ("relative root", &["a"], true),
            ("root with a parent step", &["a"], true),
            ("root with slashes", &["a"], true),
        ];
        let roots = ["C:\\", "C:\\scan", "C:\\scan", "C:\\scan", "C:\\scan", "C:\\scan", "C:\\scan", "C:\\scan", "C:\\scan", "C:\\scan", "root", "..\\scan", "C:/scan/"];

        let got: Vec<(&str, bool)> = cases.iter().zip(roots).map(|((case, names, _), root)| (*case, prepare_chain(root, names) != Prepared::Changed)).collect();

        let want: Vec<(&str, bool)> = cases.iter().map(|(case, _, ready)| (*case, *ready)).collect();
        assert_eq!(got, want);
    }

    // ---- items ----
    const EXPLORE: Item = Item::Action(Cmd::ExploreFolder, "Explore Folder");
    const PROMPT: Item = Item::Action(Cmd::CommandPrompt, "Command Prompt Here");
    const COPY: Item = Item::Action(Cmd::CopyPath, "Copy Path");
    const SHOW: Item = Item::Action(Cmd::ShowFolder, "Show this folder");

    fn ready(before: Disk) -> Prepared {
        Prepared::Ready { path: PathBuf::from("C:\\x"), before }
    }

    #[test]
    fn items_file_has_explore_prompt_copy_and_a_separator() {
        assert_eq!(items(&ready(Disk::File(1)), false, true), [EXPLORE, PROMPT, COPY, Item::Separator]);
    }

    #[test]
    fn items_folder_adds_show_this_folder() {
        assert_eq!(items(&ready(Disk::Dir), true, true), [EXPLORE, PROMPT, COPY, SHOW, Item::Separator]);
    }

    #[test]
    fn items_changed_is_only_the_changed_line_and_rescan() {
        let want = [Item::Disabled("This item changed on disk. Rescan."), Item::Action(Cmd::Rescan, "Rescan")];

        assert_eq!([items(&Prepared::Changed, false, false), items(&Prepared::Changed, true, true)], [want, want]);
    }

    #[test]
    fn items_shell_failed_adds_the_no_menu_line() {
        let got = items(&ready(Disk::File(1)), false, false);

        assert_eq!(got, [EXPLORE, PROMPT, COPY, Item::Separator, Item::Disabled("Windows has no menu for this item.")]);
    }

    // ---- picked ----
    #[test]
    fn picked_maps_every_id_range() {
        let ids = [0, 1, 2, 3, 4, 5, 6, 0xFF, 0x100, 0x7FFC, 0x7FFF, 0x8000, u32::MAX];

        let got: Vec<Picked> = ids.iter().map(|&id| picked(id)).collect();

        let want = [
            Picked::Nothing,
            Picked::Volatus(Cmd::ExploreFolder),
            Picked::Volatus(Cmd::CommandPrompt),
            Picked::Volatus(Cmd::CopyPath),
            Picked::Volatus(Cmd::ShowFolder),
            Picked::Volatus(Cmd::Rescan),
            Picked::Nothing,
            Picked::Nothing,
            Picked::Shell(0),
            Picked::Shell(0x7EFC),
            Picked::Shell(0x7EFF),
            Picked::Nothing,
            Picked::Nothing,
        ];
        assert_eq!(got, want);
    }

    // ---- flags ----
    #[test]
    fn query_flags_without_shift_is_normal() {
        assert_eq!(query_flags(false), 0);
    }

    #[test]
    fn query_flags_with_shift_adds_only_the_extended_verbs() {
        assert_eq!(query_flags(true), 0x100);
    }

    #[test]
    fn query_flags_never_set_can_rename() {
        assert_eq!([query_flags(false), query_flags(true)].map(|f| f & 0x10), [0, 0]);
    }

    #[test]
    fn invoke_mask_covers_the_four_shift_and_ctrl_combinations() {
        const BASE: u32 = 0x4000 | 0x2000_0000;

        let got = [(false, false), (true, false), (false, true), (true, true)].map(|(shift, ctrl)| invoke_mask(shift, ctrl));

        assert_eq!(got, [BASE, BASE | 0x1000_0000, BASE | 0x4000_0000, BASE | 0x1000_0000 | 0x4000_0000]);
    }

    #[test]
    fn local_constants_match_those_of_the_windows_crate() {
        use windows::Win32::UI::Shell as sh;

        let local = [CMF_NORMAL, CMF_EXTENDEDVERBS, CMIC_MASK_UNICODE, CMIC_MASK_PTINVOKE, CMIC_MASK_SHIFT_DOWN, CMIC_MASK_CONTROL_DOWN];

        // CMIC_MASK_UNICODE is missing in windows 0.62.2: it is SEE_MASK_UNICODE (spike S7).
        let crate_values = [sh::CMF_NORMAL, sh::CMF_EXTENDEDVERBS, sh::SEE_MASK_UNICODE, sh::CMIC_MASK_PTINVOKE, sh::CMIC_MASK_SHIFT_DOWN, sh::CMIC_MASK_CONTROL_DOWN];
        assert_eq!((local, COPYENGINE_E_USER_CANCELLED), (crate_values, sh::COPYENGINE_E_USER_CANCELLED.0));
    }

    // ---- verbs ----
    #[test]
    fn is_delete_is_true_only_for_the_verb_delete_in_any_ascii_case() {
        let verbs = [Some("delete"), Some("Delete"), Some("DELETE"), Some("delete "), Some("deletex"), Some("Löschen"), Some(""), None];

        let got = verbs.map(is_delete);

        assert_eq!(got, [true, true, true, false, false, false, false, false]);
    }

    #[test]
    fn is_delete_is_false_for_the_verbs_the_shell_gave_in_the_spike() {
        // S6: copyaspath, properties and the like; third-party verbs are GUIDs or product names.
        let verbs = [
            "open", "Edit", "Print", "openas", "pintohomefile", "pintohome", "PinToStartScreen", "copyaspath", "Windows.ModernShare",
            "PreviousVersions", "cut", "copy", "link", "properties", "Powershell", "WSL", "opennewprocess",
            "{1861E28B-A1F0-4EF4-A1FE-4C8CA88E2174}", "PeaZip.ext2main", "SevenZipCompress", "TeraCopy", "ShareX", "Volatus", "WizTree",
        ];

        assert!(verbs.iter().all(|v| !is_delete(Some(v))));
    }

    #[test]
    fn user_cancelled_is_true_only_for_the_cancel_result_of_the_shell() {
        let results = [0x8027_0000_u32 as i32, 0, 0x8000_4005_u32 as i32, 0x8007_0057_u32 as i32];

        assert_eq!(results.map(user_cancelled), [true, false, false, false]);
    }

    // ---- judge ----
    #[test]
    fn judge_covers_delete_and_other_actions_for_every_disk_now_and_last_check() {
        let before = Disk::File(5);
        let nows = [("gone", Disk::Gone), ("same", Disk::File(5)), ("different", Disk::File(6)), ("unreadable", Disk::Unreadable)];
        use Outcome::{Nothing, Outdated, Remove, Wait};
        // (delete, now, last) in the order of the loops below.
        let want = [
            // delete: gone, same, different, unreadable; not last then last
            Remove, Remove, Wait, Nothing, Outdated, Outdated, Outdated, Outdated,
            // other action
            Outdated, Outdated, Wait, Nothing, Outdated, Outdated, Outdated, Outdated,
        ];

        let mut got = Vec::new();
        for delete in [true, false] {
            for (_, now) in &nows {
                for last in [false, true] {
                    got.push(judge(delete, &before, now, last));
                }
            }
        }

        assert_eq!(got, want);
    }

    // ---- Follow ----
    fn follow(delete: bool) -> Follow {
        Follow::new(NodeId(1), 0, PathBuf::from("C:\\x"), delete, Disk::File(5))
    }

    #[test]
    fn follow_step_waits_half_a_second_then_a_second_and_a_half_then_ends() {
        let mut f = follow(false);
        let same = Disk::File(5);

        let got = [f.step(&same), f.step(&same), f.step(&same)];

        let want = [
            (Outcome::Wait, Some(Duration::from_millis(500))),
            (Outcome::Wait, Some(Duration::from_millis(1500))),
            (Outcome::Nothing, None),
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn follow_step_a_decision_has_no_delay_and_stops_the_checks() {
        let mut f = follow(true);

        let got = [f.step(&Disk::Gone), f.step(&Disk::Gone)];

        assert_eq!(got, [(Outcome::Remove, None), (Outcome::Nothing, None)]);
    }

    #[test]
    fn follow_step_an_item_that_changes_between_checks_is_outdated() {
        let mut f = follow(false);

        let got = [f.step(&Disk::File(5)), f.step(&Disk::File(9))];

        assert_eq!(got, [(Outcome::Wait, Some(Duration::from_millis(500))), (Outcome::Outdated, None)]);
    }

    #[test]
    fn follow_new_keeps_what_it_was_made_with() {
        let f = follow(true);

        assert_eq!((f.node, f.edits, f.path, f.delete, f.before), (NodeId(1), 0, PathBuf::from("C:\\x"), true, Disk::File(5)));
    }

    // ---- after_pick ----
    /// Runs `after_pick` for `follow(delete)` with the disk `now` and an item that gives `hr`.
    /// Gives the checks and whether the item ran.
    fn pick(delete: bool, now: Disk, hr: i32) -> (Option<Follow>, bool) {
        let ran = std::cell::Cell::new(false);
        let got = after_pick(follow(delete), |_| now.clone(), || {
            ran.set(true);
            hr
        });
        (got, ran.get())
    }

    #[test]
    fn after_pick_runs_the_item_and_follows_it() {
        let (got, ran) = pick(true, Disk::File(5), 0);

        assert_eq!((got.map(|f| (f.node, f.delete)), ran), (Some((NodeId(1), true)), true));
    }

    #[test]
    fn after_pick_of_a_path_gone_since_the_menu_does_not_run_the_item_and_still_follows_it() {
        let (got, ran) = pick(true, Disk::Gone, 0);

        assert_eq!((got.is_some(), ran), (true, false));
    }

    #[test]
    fn after_pick_cancelled_by_the_user_has_no_checks() {
        let (got, ran) = pick(true, Disk::File(5), 0x8027_0000_u32 as i32);

        assert_eq!((got.is_none(), ran), (true, true));
    }

    #[test]
    fn after_pick_of_an_item_that_failed_still_has_the_checks() {
        let (got, ran) = pick(false, Disk::File(5), 0x8000_4005_u32 as i32);

        assert_eq!((got.is_some(), ran), (true, true));
    }

    // ---- terminal_folder ----
    #[test]
    fn terminal_folder_of_a_folder_is_the_folder() {
        assert_eq!(terminal_folder(Path::new("C:\\a\\b"), true), PathBuf::from("C:\\a\\b"));
    }

    #[test]
    fn terminal_folder_of_a_file_is_its_folder() {
        assert_eq!(terminal_folder(Path::new("C:\\a\\b.txt"), false), PathBuf::from("C:\\a"));
    }

    #[test]
    fn terminal_folder_of_a_file_in_a_drive_root_is_the_drive_root() {
        assert_eq!(terminal_folder(Path::new("C:\\b.txt"), false), PathBuf::from("C:\\"));
    }

    #[test]
    fn terminal_folder_of_a_drive_root_is_the_drive_root() {
        assert_eq!(terminal_folder(Path::new("C:\\"), true), PathBuf::from("C:\\"));
    }

    // ---- anchor ----
    #[test]
    fn anchor_of_a_pointer_is_its_screen_point() {
        assert_eq!(anchor(At::Pointer(10.0, 20.0), 1.5, (100, -50)), Anchor::Point(115, -20));
    }

    #[test]
    fn anchor_of_a_box_is_its_screen_rect() {
        let got = anchor(At::Box(10.0, 20.0, 30.0, 40.0), 2.0, (100, 50));

        assert_eq!(got, Anchor::Below { left: 120, top: 90, right: 180, bottom: 170 });
    }

    // ---- screen_point ----
    #[test]
    fn screen_point_scales_the_window_point_and_adds_the_origin() {
        let got = [1.0, 1.5, 2.0].map(|scale| screen_point((10.0, 20.0), scale, (100, -50)));

        assert_eq!(got, [(110, -30), (115, -20), (120, -10)]);
    }

    #[test]
    fn screen_point_rounds_half_away_from_zero() {
        let got = [screen_point((1.0, 3.0), 1.5, (0, 0)), screen_point((-1.0, -3.0), 1.5, (0, 0))];

        // 1.5 -> 2, 4.5 -> 5; -1.5 -> -2, -4.5 -> -5.
        assert_eq!(got, [(2, 5), (-2, -5)]);
    }
}
