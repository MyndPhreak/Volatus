//! Send to the Recycle Bin: one shell operation (`SHFileOperationW`, `FO_DELETE` with
//! `FOF_ALLOWUNDO`) on the one checked path. A folder goes into the bin as one item. The shell
//! takes only a plain (`C:\...`) path, so the verbatim (`\\?\`) path from `check_target` is made
//! plain only where the plain form names the same item; any other path is one skip whose text
//! offers the permanent delete. Nothing here ever falls back to a permanent delete by itself.

#![cfg_attr(not(test), expect(dead_code, reason = "wired in the UI task"))]
#![cfg_attr(test, allow(dead_code, reason = "tests do not use every item; wired in the UI task"))]

use crate::delete::{DeleteTarget, Outcome, Remover, SkipReason, Skipped, reason_from_win32};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOCONFIRMMKDIR, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING,
    SHFILEOPSTRUCTW, SHFileOperationW,
};

/// The Recycle Bin delete.
pub struct Recycle;

/// `SkipReason::Other` code for a path the shell cannot take as the same item (see `shell_path`).
/// Not a shell or Win32 code: those are never this negative.
pub const PATH_REFUSED: i32 = i32::MIN;

/// `SkipReason::Other` code for a call that returned 0 but set `fAnyOperationsAborted`.
pub const ABORTED: i32 = i32::MIN + 1;

/// The legacy `DE_*` codes `SHFileOperationW` can return (shellapi.h does not define them; the
/// values are from the `SHFileOperation` documentation). The ones not named here map through
/// `reason_from_win32` to `Other(code)`, as they collide with none of its special codes.
const DE_OPCANCELLED: i32 = 0x75;
const DE_ACCESSDENIEDSRC: i32 = 0x78;
const DE_PATHTOODEEP: i32 = 0x79;
const DE_INVALIDFILES: i32 = 0x7C;
const DE_FILENAMETOOLONG: i32 = 0x81;
const DE_FILE_TOO_LARGE: i32 = 0x85;

/// Longest plain path (in UTF-16 units, no NUL) the shell is given: `MAX_PATH` less the NUL.
const MAX_PLAIN: usize = 259;

/// `FOF_ALLOWUNDO` is what makes the delete a recycle. `FOF_NOCONFIRMATION` answers "Yes to all"
/// to every shell prompt, which includes "this item is too big for the Recycle Bin (or the bin is
/// off on this drive): delete it permanently?". `FOF_WANTNUKEWARNING` takes that one prompt back
/// out of `FOF_NOCONFIRMATION`, so the shell never permanently deletes an item without asking.
/// `FOF_SILENT` and `FOF_NOERRORUI` turn the shell's own progress and error UI off.
const FLAGS: u32 =
    FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_NOCONFIRMMKDIR | FOF_WANTNUKEWARNING;
const _: () = assert!(FLAGS <= u16::MAX as u32, "fFlags is a u16");

impl Remover for Recycle {
    /// One shell call for the whole target. There is no progress during it: `progress(0, 0)` is
    /// called before it and `progress(1, t.size)` after a success. `stop` is checked only before
    /// the call (Recycle Bin mode has no Stop button); when it is set nothing is done.
    fn remove(&self, t: &DeleteTarget, progress: &dyn Fn(u64, u64), stop: &AtomicBool) -> Outcome {
        let mut out = Outcome::default();
        if stop.load(Ordering::Relaxed) {
            out.stopped = true;
            return out;
        }
        let skip = |reason| Skipped { path: t.path.clone(), reason };
        let Some(from) = shell_path(&t.path) else {
            out.skipped.push(skip(SkipReason::Other(PATH_REFUSED)));
            return out;
        };
        progress(0, 0);
        let (code, aborted) = shell_delete(&from);
        match skip_for_shell_result(code, aborted) {
            None => {
                out.removed_items = 1;
                out.removed_bytes = t.size;
                progress(1, t.size);
            }
            Some(reason) => out.skipped.push(skip(reason)),
        }
        out
    }
}

/// The plain, double-NUL-terminated `pFrom` for a verbatim drive path, or `None` when the plain
/// form could name a different item or more than one. Win32 drops a trailing dot or space from a
/// name (and reaches a sibling), reads a `:` as a stream, reads a device name (`con`, `aux.txt`)
/// as a device, and the shell expands `*` and `?` (and `<`, `>`, `"` are wildcards to
/// `FindFirstFile`). So `None` for: a path that is not `\\?\<letter>:\<names>` (a plain path
/// too: this engine takes only `check_target`'s output, and the text of a plain path may already
/// have lost its trailing dots); a drive root; a plain form longer than `MAX_PLAIN`; and a name
/// that is empty, `.` or `..`, ends in a dot or space, holds a control character or one of
/// `: * ? < > " | /`, is not valid UTF-16, or is a reserved device name. The plain form is the
/// verbatim text with the `\\?\` taken off and nothing else changed.
pub(crate) fn shell_path(verbatim: &Path) -> Option<Vec<u16>> {
    let (colon, back) = (u16::from(b':'), u16::from(b'\\'));
    let w: Vec<u16> = verbatim.as_os_str().encode_wide().collect();
    let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
    let plain = w.strip_prefix(prefix.as_slice())?;
    let [letter, c, b, names @ ..] = plain else { return None };
    if !(u8::try_from(*letter).is_ok_and(|l| l.is_ascii_alphabetic()) && *c == colon && *b == back) {
        return None;
    }
    if names.is_empty() || plain.len() > MAX_PLAIN || !names.split(|c| *c == back).all(plain_name) {
        return None;
    }
    Some(plain.iter().copied().chain([0, 0]).collect())
}

/// True when Win32 reads `name` (one path component) as exactly this name. See `shell_path`.
fn plain_name(name: &[u16]) -> bool {
    let Ok(s) = String::from_utf16(name) else { return false };
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.ends_with(['.', ' '])
        && !s.chars().any(|c| c < ' ' || r#":*?<>"|/"#.contains(c))
        && !is_device(&s)
}

/// A reserved device name: the part before the first dot, trailing spaces dropped, is `con`,
/// `prn`, `aux`, `nul`, `conin$`, `conout$`, or `com`/`lpt` with one digit (`0`-`9`, `¹²³`).
/// Any case. Newer Windows reads fewer of these as devices; all are refused here.
fn is_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().trim_end_matches(' ').to_lowercase();
    let numbered = stem
        .strip_prefix("com")
        .or_else(|| stem.strip_prefix("lpt"))
        .is_some_and(|d| d.chars().count() == 1 && "0123456789¹²³".contains(d));
    numbered || ["con", "prn", "aux", "nul", "conin$", "conout$"].contains(&stem.as_str())
}

/// What one shell call means for the target: `None` for a success (0, not aborted), else the
/// one skip reason. A non-zero code or the aborted flag is never a success.
///
/// | Result                                  | Reason                       |
/// |-----------------------------------------|------------------------------|
/// | 0, not aborted                          | success (`None`)             |
/// | 0, aborted                              | `Other(ABORTED)`             |
/// | `DE_ACCESSDENIEDSRC` (0x78)             | `AccessDenied`               |
/// | `DE_PATHTOODEEP` (0x79), `DE_FILENAMETOOLONG` (0x81) | `TooLong`       |
/// | any other code (`DE_*` or Win32)        | `reason_from_win32(code)`    |
///
/// `reason_from_win32` gives `Locked`, `AccessDenied` and `TooLong` for the plain Win32 codes and
/// `Other(code)` for the rest, every `DE_*` code included (`DE_FILE_TOO_LARGE` 0x85,
/// `DE_OPCANCELLED` 0x75, `DE_INVALIDFILES` 0x7C, ...). `skip_text` has the words for the codes
/// that offer the permanent delete.
pub(crate) fn skip_for_shell_result(code: i32, aborted: bool) -> Option<SkipReason> {
    match code {
        0 if !aborted => None,
        0 => Some(SkipReason::Other(ABORTED)),
        DE_ACCESSDENIEDSRC => Some(SkipReason::AccessDenied),
        DE_PATHTOODEEP | DE_FILENAMETOOLONG => Some(SkipReason::TooLong),
        code => Some(reason_from_win32(code)),
    }
}

/// The words for a Recycle Bin skip that offers the permanent delete (ASD-STE100), or `None` for a
/// reason that has the general text. No automatic fallback: the user chooses.
pub fn skip_text(r: SkipReason) -> Option<&'static str> {
    match r {
        SkipReason::Other(PATH_REFUSED | DE_INVALIDFILES) => {
            Some("The Recycle Bin cannot take this path. You can delete the item permanently.")
        }
        SkipReason::Other(DE_FILE_TOO_LARGE) => {
            Some("The item is too large for the Recycle Bin. You can delete the item permanently.")
        }
        SkipReason::Other(ABORTED | DE_OPCANCELLED) => {
            Some("Windows did not move the item to the Recycle Bin. You can delete the item permanently.")
        }
        _ => None,
    }
}

/// `SHFileOperationW` `FO_DELETE` with `FLAGS` on `from` (double-NUL terminated, one path).
/// Returns the call's code and `fAnyOperationsAborted`. No owner window: the only UI the flags
/// leave on is the shell's warning before a permanent delete.
fn shell_delete(from: &[u16]) -> (i32, bool) {
    assert!(from.ends_with(&[0, 0]) && !from[..from.len() - 2].contains(&0), "pFrom must be one path, double-NUL terminated");
    let mut op = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: FLAGS as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    // SAFETY: `op` is the windows-sys definition, fully set. `pFrom` points into `from`, which
    // holds one path and ends in two NULs and lives across the call; `pTo` and the progress title
    // are null, which `FO_DELETE` without `FOF_SIMPLEPROGRESS` allows. Without
    // `FOF_WANTMAPPINGHANDLE` the shell leaves `hNameMappings` unset, so nothing is to be freed.
    let code = unsafe { SHFileOperationW(&mut op) };
    (code, op.fAnyOperationsAborted != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scan::NodeId;
    use std::fs;
    use std::path::PathBuf;

    /// A unique directory, canonical (verbatim) form. Removed on drop; a link inside is removed
    /// first, so nothing outside is ever followed.
    struct Dir {
        base: PathBuf,
        links: std::cell::RefCell<Vec<PathBuf>>,
    }

    impl Dir {
        fn new(tag: &str) -> Dir {
            let plain = std::env::temp_dir().join(format!("volatus-recycle-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&plain);
            fs::create_dir_all(&plain).expect("temp dir");
            Dir { base: fs::canonicalize(&plain).expect("canonical"), links: Default::default() }
        }
        /// The name an item that goes to the real bin gets, so the owner can find it there.
        fn bin_name(tag: &str) -> String {
            format!("volatus-recycle-test-{}-{tag}", std::process::id())
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            for j in self.links.borrow().iter() {
                let _ = fs::remove_dir(j);
            }
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    /// Writes to the real stderr, which the test harness does not capture, so the skip shows in
    /// every `cargo test` run.
    fn skipped(case: &str) {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "SKIPPED real file system recycle test: {case}");
    }

    fn plain(p: &Path) -> PathBuf {
        PathBuf::from(p.to_string_lossy().trim_start_matches(r"\\?\"))
    }

    fn target(path: &Path, is_dir: bool, size: u64) -> DeleteTarget {
        DeleteTarget { node: NodeId(0), path: path.to_path_buf(), is_dir, is_link: false, size, files: 0, dirs: 0 }
    }

    /// Runs `Recycle` and records every progress call.
    fn run(t: &DeleteTarget, stop: bool) -> (Outcome, Vec<(u64, u64)>) {
        let calls = std::cell::RefCell::new(Vec::new());
        let o = Recycle.remove(t, &|i, b| calls.borrow_mut().push((i, b)), &AtomicBool::new(stop));
        (o, calls.into_inner())
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// `want` as the plain, double-NUL-terminated form.
    fn pfrom(want: &str) -> Vec<u16> {
        wide(want).into_iter().chain([0, 0]).collect()
    }

    #[test]
    fn shell_path_table() {
        let long = format!(r"C:\{}", "a".repeat(MAX_PLAIN - 3));
        let too_long = format!(r"C:\{}", "a".repeat(MAX_PLAIN - 2));
        let deep = format!(r"C:\{}\{}\{}", "L".repeat(120), "M".repeat(120), "N".repeat(40));
        let ok: Vec<String> = vec![
            r"C:\Users\dev\data".into(),
            r"c:\a\b.txt".into(),
            r"D:\x".into(),
            r"C:\a.b\c d\.hidden".into(),
            r"C:\café-日😀\x".into(),
            r"C:\console\nul2\com10\comx\lpt\conin".into(),
            long.clone(),
        ];
        for p in &ok {
            let verbatim = format!(r"\\?\{p}");
            // The plain form is the same text with the prefix off: never another item.
            assert_eq!(shell_path(Path::new(&verbatim)), Some(pfrom(p)), "{verbatim}");
        }
        assert_eq!(long.encode_utf16().count(), MAX_PLAIN);
        let refused: Vec<String> = vec![
            // Not verbatim, or not a drive path.
            r"C:\Users\dev\data".into(),
            r"\\?\UNC\server\share\x".into(),
            r"\\server\share\x".into(),
            r"\\.\C:\x".into(),
            r"\\?\Volume{0}\x".into(),
            r"\\?\C:".into(),
            r"\\?\C:\".into(),
            r"\\?\C:x".into(),
            r"\\?\1:\x".into(),
            // Too long for the shell.
            format!(r"\\?\{too_long}"),
            format!(r"\\?\{deep}"),
            // Names Win32 would change or read as something else.
            r"\\?\C:\data\x.".into(),
            r"\\?\C:\pd.\child".into(),
            r"\\?\C:\data\x ".into(),
            r"\\?\C:\x \y".into(),
            r"\\?\C:\data\ff:s".into(),
            r"\\?\C:\data\ff::$DATA".into(),
            r"\\?\C:\data\c:Docs".into(),
            r"\\?\C:\a\.\b".into(),
            r"\\?\C:\a\..\b".into(),
            r"\\?\C:\a\\b".into(),
            r"\\?\C:\a\".into(),
            r"\\?\C:\a/b".into(),
            // Devices.
            r"\\?\C:\data\con".into(),
            r"\\?\C:\data\CON".into(),
            r"\\?\C:\data\nul".into(),
            r"\\?\C:\data\com1".into(),
            r"\\?\C:\data\COM9.log".into(),
            r"\\?\C:\data\com¹".into(),
            r"\\?\C:\data\lpt0".into(),
            r"\\?\C:\data\aux.txt".into(),
            r"\\?\C:\data\prn.tar.gz".into(),
            r"\\?\C:\data\con .txt".into(),
            r"\\?\C:\data\conin$".into(),
            r"\\?\C:\data\CONOUT$".into(),
            r"\\?\C:\nul\x".into(),
            // Wildcards and other characters Win32 does not take in a name.
            r"\\?\C:\data\*".into(),
            r"\\?\C:\data\a?b".into(),
            r"\\?\C:\data\a<b".into(),
            r"\\?\C:\data\a>b".into(),
            r#"\\?\C:\data\a"b"#.into(),
            r"\\?\C:\data\a|b".into(),
            "\\\\?\\C:\\data\\a\tb".into(),
        ];
        for p in &refused {
            assert_eq!(shell_path(Path::new(p)), None, "{p}");
        }
    }

    #[test]
    fn shell_path_refuses_an_unpaired_surrogate() {
        use std::os::windows::ffi::OsStringExt;
        let mut w = wide(r"\\?\C:\data\x");
        w.push(0xD800);
        let p = PathBuf::from(std::ffi::OsString::from_wide(&w));
        assert_eq!(shell_path(&p), None);
    }

    #[test]
    fn shell_results_map_to_one_skip_or_success() {
        for (code, aborted, want) in [
            (0, false, None),
            (0, true, Some(SkipReason::Other(ABORTED))),
            (DE_ACCESSDENIEDSRC, false, Some(SkipReason::AccessDenied)),
            (DE_PATHTOODEEP, false, Some(SkipReason::TooLong)),
            (DE_FILENAMETOOLONG, false, Some(SkipReason::TooLong)),
            (DE_FILE_TOO_LARGE, false, Some(SkipReason::Other(DE_FILE_TOO_LARGE))),
            (DE_OPCANCELLED, true, Some(SkipReason::Other(DE_OPCANCELLED))),
            (DE_INVALIDFILES, false, Some(SkipReason::Other(DE_INVALIDFILES))),
            (0x71, false, Some(SkipReason::Other(0x71))),
            (0x74, false, Some(SkipReason::Other(0x74))),
            (0xB7, false, Some(SkipReason::Other(0xB7))),
            (0x10000, false, Some(SkipReason::Other(0x10000))),
            (0x402, false, Some(SkipReason::Other(0x402))),
            // Plain Win32 codes.
            (2, false, Some(SkipReason::Other(2))),
            (5, false, Some(SkipReason::AccessDenied)),
            (32, false, Some(SkipReason::Locked)),
            (32, true, Some(SkipReason::Locked)),
            (206, false, Some(SkipReason::TooLong)),
        ] {
            assert_eq!(skip_for_shell_result(code, aborted), want, "code={code:#x} aborted={aborted}");
        }
        // Every DE_* code from the documentation is a skip, never a success.
        for code in (0x71..=0x88).chain([0xB7, 0x402, 0x10000, 0x10074]) {
            assert!(skip_for_shell_result(code, false).is_some(), "code={code:#x}");
        }
    }

    #[test]
    fn skip_text_offers_the_permanent_delete_where_the_bin_cannot_take_the_item() {
        let offers = [
            SkipReason::Other(PATH_REFUSED),
            SkipReason::Other(DE_INVALIDFILES),
            SkipReason::Other(DE_FILE_TOO_LARGE),
            SkipReason::Other(ABORTED),
            SkipReason::Other(DE_OPCANCELLED),
        ];
        for r in offers {
            let t = skip_text(r).unwrap_or_else(|| panic!("{r:?} has no text"));
            assert!(t.contains("Recycle Bin"), "{t}");
            assert!(t.ends_with("You can delete the item permanently."), "{t}");
        }
        assert!(skip_text(SkipReason::Other(PATH_REFUSED)).unwrap().contains("path"));
        assert!(skip_text(SkipReason::Other(DE_FILE_TOO_LARGE)).unwrap().contains("too large"));
        for r in [SkipReason::Locked, SkipReason::AccessDenied, SkipReason::TooLong, SkipReason::Other(2)] {
            assert_eq!(skip_text(r), None, "{r:?}");
        }
    }

    #[test]
    fn the_flags_recycle_silently_and_warn_before_a_permanent_delete() {
        let f = FLAGS;
        for (flag, name) in [
            (FOF_ALLOWUNDO, "FOF_ALLOWUNDO"),
            (FOF_NOCONFIRMATION, "FOF_NOCONFIRMATION"),
            (FOF_SILENT, "FOF_SILENT"),
            (FOF_NOERRORUI, "FOF_NOERRORUI"),
            (FOF_NOCONFIRMMKDIR, "FOF_NOCONFIRMMKDIR"),
            (FOF_WANTNUKEWARNING, "FOF_WANTNUKEWARNING"),
        ] {
            assert_ne!(f & flag, 0, "{name}");
        }
    }

    #[test]
    fn a_missing_target_is_one_skip() {
        let d = Dir::new("missing");
        let t = target(&d.base.join("gone"), true, 10);
        let (o, _) = run(&t, false);
        assert_eq!((o.removed_items, o.removed_bytes, o.stopped), (0, 0, false));
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].path, t.path);
        assert_eq!(o.skipped[0].reason, reason_from_win32(2));
    }

    #[test]
    fn stop_set_before_the_call_leaves_the_item_in_place() {
        let d = Dir::new("stop");
        let item = d.base.join("t");
        fs::create_dir(&item).unwrap();
        fs::write(item.join("keep"), b"x").unwrap();
        let (o, calls) = run(&target(&item, true, 1), true);
        assert!(o.stopped);
        assert_eq!((o.removed_items, o.removed_bytes), (0, 0));
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert!(calls.is_empty(), "{calls:?}");
        assert!(item.join("keep").exists());
    }

    #[test]
    fn a_plain_path_is_one_skip_and_stays() {
        let d = Dir::new("plain");
        let item = d.base.join("t");
        fs::create_dir(&item).unwrap();
        let t = target(&plain(&item), true, 0);
        let (o, _) = run(&t, false);
        assert_eq!(o.removed_items, 0);
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].path, t.path);
        assert_eq!(o.skipped[0].reason, SkipReason::Other(PATH_REFUSED));
        assert!(item.exists());
    }

    #[test]
    fn a_trailing_dot_is_one_skip_and_its_sibling_stays() {
        let d = Dir::new("dot");
        fs::create_dir(d.base.join("pd")).unwrap();
        fs::write(d.base.join("pd").join("plain-mark"), b"x").unwrap();
        let dot = d.base.join("pd.");
        if fs::create_dir(&dot).is_err() {
            skipped("trailing dot (the OS refused the name pd.)");
            return;
        }
        fs::write(dot.join("dot-mark"), b"x").unwrap();
        let (o, calls) = run(&target(&dot, true, 1), false);
        assert_eq!(o.removed_items, 0);
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].reason, SkipReason::Other(PATH_REFUSED));
        assert!(calls.is_empty(), "the shell is not called: {calls:?}");
        // The plain form `...\pd.` is `...\pd` to Win32: both must stay.
        assert!(d.base.join("pd").join("plain-mark").exists());
        assert!(dot.join("dot-mark").exists());
    }

    #[test]
    fn a_path_too_long_for_the_shell_is_one_skip_and_stays() {
        let d = Dir::new("long");
        let item = d.base.join("L".repeat(120)).join("M".repeat(120)).join("N".repeat(40));
        fs::create_dir_all(&item).unwrap();
        assert!(plain(&item).as_os_str().encode_wide().count() > MAX_PLAIN);
        let (o, _) = run(&target(&item, true, 0), false);
        assert_eq!(o.removed_items, 0);
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].reason, SkipReason::Other(PATH_REFUSED));
        assert!(item.exists());
    }

    /// The bin's `$I` records (one per item) whose original path is `p`, in every bin folder of
    /// `p`'s drive this user can read. Only read, never changed. `None` when no bin folder can be
    /// read. Format: version (u64), size (u64), time (u64), then for version 2 the name length
    /// (u32) and the UTF-16 path; for version 1 a fixed 260-unit path.
    fn bin_records(p: &Path) -> Option<usize> {
        let want = plain(p).to_string_lossy().to_lowercase();
        let drive = want.get(..2)?.to_string();
        let mut readable = false;
        let mut n = 0;
        for user in fs::read_dir(format!(r"{drive}\$Recycle.Bin")).ok()?.flatten() {
            let Ok(entries) = fs::read_dir(user.path()) else { continue };
            readable = true;
            for e in entries.flatten() {
                if !e.file_name().to_string_lossy().starts_with("$I") {
                    continue;
                }
                let Ok(b) = fs::read(e.path()) else { continue };
                let text = |from: usize| -> String {
                    let w: Vec<u16> =
                        b.get(from..).unwrap_or_default().as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
                    String::from_utf16_lossy(&w).trim_end_matches('\0').to_lowercase()
                };
                let name = match b.first() {
                    Some(2) => text(28),
                    Some(1) => text(24),
                    _ => continue,
                };
                if name.trim_start_matches(r"\\?\") == want {
                    n += 1;
                }
            }
        }
        readable.then_some(n)
    }

    /// Checks that `p` came into the bin: one more `$I` record names it than `before`.
    fn assert_in_bin(p: &Path, before: Option<usize>) {
        match (before, bin_records(p)) {
            (Some(b), Some(a)) => assert_eq!(a, b + 1, "the bin has no new record for {p:?}"),
            _ => skipped("Recycle Bin record check (no bin folder could be read)"),
        }
    }

    #[test]
    #[ignore = "sends a temp folder to the real Recycle Bin"]
    fn a_folder_goes_to_the_recycle_bin_as_one_item() {
        let d = Dir::new("folder");
        let item = d.base.join(Dir::bin_name("folder"));
        for sub in ["a", "b"] {
            fs::create_dir_all(item.join(sub)).unwrap();
            fs::write(item.join(sub).join("f.txt"), b"12345").unwrap();
        }
        let before = bin_records(&item);
        let (o, calls) = run(&target(&item, true, 10), false);
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert!(!o.stopped);
        assert_eq!((o.removed_items, o.removed_bytes), (1, 10));
        assert_eq!(calls, vec![(0, 0), (1, 10)]);
        assert!(!item.exists(), "the folder left its place");
        assert_in_bin(&item, before);
    }

    #[test]
    #[ignore = "sends a temp folder to the real Recycle Bin"]
    fn the_shell_moves_a_file_with_no_prompt_and_no_abort() {
        let d = Dir::new("file");
        let item = d.base.join(format!("{}.txt", Dir::bin_name("file")));
        fs::write(&item, b"hello").unwrap();
        let before = bin_records(&item);
        let from = shell_path(&item).expect("a plain form");
        assert_eq!(shell_delete(&from), (0, false));
        assert!(!item.exists(), "the file left its place");
        assert_in_bin(&item, before);
    }

    #[test]
    #[ignore = "sends a temp folder to the real Recycle Bin"]
    fn a_junction_goes_to_the_bin_as_itself() {
        let d = Dir::new("junction");
        let other = d.base.join("other");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("keep"), b"x").unwrap();
        let link = d.base.join(Dir::bin_name("junction"));
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(plain(&link))
            .arg(plain(&other))
            .output()
            .is_ok_and(|o| o.status.success());
        if !made {
            skipped("junction item (mklink /J was refused)");
            return;
        }
        d.links.borrow_mut().push(link.clone());
        let mut t = target(&link, false, 0);
        t.is_link = true;
        let (o, _) = run(&t, false);
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!(o.removed_items, 1);
        assert!(fs::symlink_metadata(&link).is_err(), "the junction left its place");
        assert!(other.join("keep").exists(), "the junction was not followed");
    }
}
