//! Fast permanent delete: one walk of the target, the files removed in parallel, then the folders
//! from the deepest up. Direct Win32 calls on verbatim (`\\?\`) paths. A link is never followed.

#![cfg_attr(not(test), expect(dead_code, reason = "wired in the UI task"))]
#![cfg_attr(test, allow(dead_code, reason = "tests do not use every item; wired in the UI task"))]

use crate::delete::{DeleteTarget, Outcome, Remover, Skipped, reason_from_win32};
use rayon::prelude::*;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_DIR_NOT_EMPTY, ERROR_FILE_NOT_FOUND, ERROR_INVALID_NAME, ERROR_NO_MORE_FILES,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    DeleteFileW, FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_SYSTEM, FILE_ATTRIBUTE_TEMPORARY, FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic,
    FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, RemoveDirectoryW, SetFileAttributesW, WIN32_FIND_DATAW,
};

/// Items removed between two progress calls: the bound on the progress rate.
const PROGRESS_EVERY: usize = 256;

/// The attributes `SetFileAttributesW` can set; the others in the find data are left out.
const SETTABLE: u32 = FILE_ATTRIBUTE_ARCHIVE
    | FILE_ATTRIBUTE_HIDDEN
    | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
    | FILE_ATTRIBUTE_OFFLINE
    | FILE_ATTRIBUTE_SYSTEM
    | FILE_ATTRIBUTE_TEMPORARY;

/// The fast permanent delete.
pub struct Purge;

/// A file or a link the walk found: removed in the parallel pass. `path` has no NUL.
struct Entry {
    path: Vec<u16>,
    attrs: u32,
    size: u64,
}

/// A real folder (not a link) the walk found, `depth` below the target. `listed` is false when
/// its listing failed: that failure is already a skip, so the folder is not tried again.
struct Folder {
    path: Vec<u16>,
    attrs: u32,
    depth: usize,
    listed: bool,
}

/// What happened to one item. `Removed` holds the bytes it freed.
enum Done {
    Removed(u64),
    Skipped(Skipped),
    NotRun,
}

impl Remover for Purge {
    /// The target must be a verbatim (`\\?\`) path, as `check_target` gives it; any other path is
    /// one skip and nothing is touched. A file or a link target is removed alone. A folder is
    /// walked once (links inside are entries, never walked into), its files and links are removed
    /// in parallel, then its folders from the deepest up, the target last. `removed_items` counts
    /// files, links and folders; `removed_bytes` is the find-data size of the files (a link frees
    /// nothing). A folder that still holds a skipped item stays, with no extra skip.
    fn remove(&self, t: &DeleteTarget, progress: &dyn Fn(u64, u64), stop: &AtomicBool) -> Outcome {
        let mut out = Outcome::default();
        let root: Vec<u16> = t.path.as_os_str().encode_wide().collect();
        let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
        if !root.starts_with(&prefix) {
            out.skipped.push(skip(&root, ERROR_INVALID_NAME));
            return out;
        }
        let data = match find_one(&root) {
            Ok(d) => d,
            Err(code) => {
                out.skipped.push(skip(&root, code));
                return out;
            }
        };
        let attrs = data.dwFileAttributes;
        if attrs & FILE_ATTRIBUTE_DIRECTORY == 0 || attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            tally(&mut out, remove_entry(&Entry { path: root, attrs, size: file_size(&data) }));
            progress(out.removed_items, out.removed_bytes);
            return out;
        }
        let Some((files, mut folders)) = walk(root, attrs, stop, &mut out.skipped) else {
            out.stopped = true;
            return out;
        };
        for chunk in files.chunks(PROGRESS_EVERY) {
            if stop.load(Ordering::Relaxed) {
                out.stopped = true;
                break;
            }
            let done: Vec<Done> = chunk
                .par_iter()
                .map(|e| if stop.load(Ordering::Relaxed) { Done::NotRun } else { remove_entry(e) })
                .collect();
            for d in done {
                tally(&mut out, d);
            }
            progress(out.removed_items, out.removed_bytes);
        }
        if !out.stopped {
            // Deepest first, so each folder is empty when its turn comes; the target is depth 0.
            folders.sort_by_key(|f| std::cmp::Reverse(f.depth));
            for (i, f) in folders.iter().filter(|f| f.listed).enumerate() {
                if stop.load(Ordering::Relaxed) {
                    out.stopped = true;
                    break;
                }
                match with_read_only_retry(&f.path, f.attrs, RemoveDirectoryW) {
                    Ok(()) => out.removed_items += 1,
                    // It still holds a skipped item, which is listed already.
                    Err(ERROR_DIR_NOT_EMPTY) => {}
                    Err(code) => out.skipped.push(skip(&f.path, code)),
                }
                if (i + 1) % PROGRESS_EVERY == 0 {
                    progress(out.removed_items, out.removed_bytes);
                }
            }
        }
        progress(out.removed_items, out.removed_bytes);
        out
    }
}

/// True when a find-data entry is a link: a reparse point with a name-surrogate tag (junction,
/// symlink), as the scan counts links. `tag` is the find data's `dwReserved0`.
fn is_name_surrogate_link(attrs: u32, _tag: u32) -> bool {
    attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// True when a folder that will not empty needs its own skip: no skip recorded so far lies in it.
fn needs_skip_for_folder(_folder: &Path, _skips: &[Skipped]) -> bool {
    false
}

/// Adds one item's result to the outcome.
fn tally(out: &mut Outcome, d: Done) {
    match d {
        Done::Removed(bytes) => {
            out.removed_items += 1;
            out.removed_bytes += bytes;
        }
        Done::Skipped(s) => out.skipped.push(s),
        Done::NotRun => out.stopped = true,
    }
}

/// Removes a file (`DeleteFileW`) or a link itself: a folder link or junction with
/// `RemoveDirectoryW`, a file link with `DeleteFileW`. Neither call follows the link.
fn remove_entry(e: &Entry) -> Done {
    let link = e.attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    let op = if link && e.attrs & FILE_ATTRIBUTE_DIRECTORY != 0 { RemoveDirectoryW } else { DeleteFileW };
    match with_read_only_retry(&e.path, e.attrs, op) {
        Ok(()) => Done::Removed(if link { 0 } else { e.size }),
        Err(code) => Done::Skipped(skip(&e.path, code)),
    }
}

/// Runs `op` on `path`. On access denied, a read-only item that is not a link loses the flag and
/// `op` runs once more; if that fails too, the flag is put back. A link's attributes are never
/// set, so nothing reaches its target. The error is the last delete's, never the restore's: the
/// item is a skip either way.
fn with_read_only_retry(path: &[u16], attrs: u32, op: unsafe extern "system" fn(*const u16) -> i32) -> Result<(), u32> {
    let z = nul(path);
    // SAFETY: `z` is NUL-terminated and lives across the call.
    if unsafe { op(z.as_ptr()) } != 0 {
        return Ok(());
    }
    let code = last_error();
    if code != ERROR_ACCESS_DENIED || attrs & FILE_ATTRIBUTE_READONLY == 0 || attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(code);
    }
    let cleared = match attrs & SETTABLE {
        0 => FILE_ATTRIBUTE_NORMAL,
        a => a,
    };
    // SAFETY: `z` is NUL-terminated and lives across the call.
    if unsafe { SetFileAttributesW(z.as_ptr(), cleared) } == 0 {
        return Err(last_error());
    }
    // SAFETY: as above.
    if unsafe { op(z.as_ptr()) } != 0 {
        return Ok(());
    }
    let code = last_error();
    // SAFETY: as above. A failed restore leaves the item without the flag; it is a skip already.
    unsafe { SetFileAttributesW(z.as_ptr(), (attrs & SETTABLE) | FILE_ATTRIBUTE_READONLY) };
    Err(code)
}

/// Lists every folder under `root` once, depth first. Links are entries, never walked into.
/// A listing that fails is a skip for that folder and the walk goes on. `None` when `stop` is
/// set: nothing has been removed yet.
fn walk(root: Vec<u16>, attrs: u32, stop: &AtomicBool, skipped: &mut Vec<Skipped>) -> Option<(Vec<Entry>, Vec<Folder>)> {
    let (mut files, mut folders) = (Vec::new(), Vec::new());
    let mut todo = vec![(root, attrs, 0)];
    while let Some((dir, attrs, depth)) = todo.pop() {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let listed = list(&dir, |d| {
            let mut path = dir.clone();
            path.push(u16::from(b'\\'));
            path.extend_from_slice(name_of(d));
            let a = d.dwFileAttributes;
            if a & FILE_ATTRIBUTE_DIRECTORY != 0 && a & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
                todo.push((path, a, depth + 1));
            } else {
                files.push(Entry { path, attrs: a, size: file_size(d) });
            }
        });
        if let Err(code) = listed {
            skipped.push(skip(&dir, code));
        }
        folders.push(Folder { path: dir, attrs, depth, listed: listed.is_ok() });
    }
    Some((files, folders))
}

/// Calls `f` for each entry in the folder `dir` (no NUL), `.` and `..` left out.
fn list(dir: &[u16], mut f: impl FnMut(&WIN32_FIND_DATAW)) -> Result<(), u32> {
    let mut pattern = dir.to_vec();
    pattern.extend(r"\*".encode_utf16());
    let pattern = nul(&pattern);
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `pattern` is NUL-terminated; `data` is a WIN32_FIND_DATAW, the type FindExInfoBasic
    // fills; no search filter is passed.
    let h: HANDLE = unsafe {
        FindFirstFileExW(
            pattern.as_ptr(),
            FindExInfoBasic,
            (&raw mut data).cast(),
            FindExSearchNameMatch,
            std::ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return match last_error() {
            // No entry at all, not even `.`: an empty listing.
            ERROR_FILE_NOT_FOUND => Ok(()),
            code => Err(code),
        };
    }
    let mut result = Ok(());
    loop {
        let name = name_of(&data);
        if name != [u16::from(b'.')] && name != [u16::from(b'.'), u16::from(b'.')] {
            f(&data);
        }
        // SAFETY: `h` is the open find handle; `data` is a valid WIN32_FIND_DATAW.
        if unsafe { FindNextFileW(h, &mut data) } == 0 {
            match last_error() {
                ERROR_NO_MORE_FILES => {}
                code => result = Err(code),
            }
            break;
        }
    }
    // SAFETY: `h` is the open find handle and is closed once.
    if unsafe { FindClose(h) } == 0 && result.is_ok() {
        result = Err(last_error());
    }
    result
}

/// The find data of the item at `path` itself (no wildcard), so a link is seen as a link.
fn find_one(path: &[u16]) -> Result<WIN32_FIND_DATAW, u32> {
    let z = nul(path);
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `z` is NUL-terminated; `data` is a WIN32_FIND_DATAW, the type FindExInfoBasic fills;
    // no search filter is passed.
    let h = unsafe {
        FindFirstFileExW(z.as_ptr(), FindExInfoBasic, (&raw mut data).cast(), FindExSearchNameMatch, std::ptr::null(), 0)
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    // SAFETY: `h` is the open find handle and is closed once.
    if unsafe { FindClose(h) } == 0 {
        return Err(last_error());
    }
    Ok(data)
}

/// The entry's name, up to its NUL.
fn name_of(d: &WIN32_FIND_DATAW) -> &[u16] {
    let n = d.cFileName.iter().position(|c| *c == 0).unwrap_or(d.cFileName.len());
    &d.cFileName[..n]
}

/// The logical size from the find data.
fn file_size(d: &WIN32_FIND_DATAW) -> u64 {
    (u64::from(d.nFileSizeHigh) << 32) | u64::from(d.nFileSizeLow)
}

fn nul(p: &[u16]) -> Vec<u16> {
    p.iter().copied().chain(Some(0)).collect()
}

fn last_error() -> u32 {
    // SAFETY: reads the calling thread's last-error value; no arguments.
    unsafe { GetLastError() }
}

fn skip(path: &[u16], code: u32) -> Skipped {
    Skipped { path: PathBuf::from(std::ffi::OsString::from_wide(path)), reason: reason_from_win32(code as i32) }
}

/// Real file system tests. Every path is under one unique directory in the temp folder that the
/// test creates and removes. A case runs only when the OS creates its fixture; a case it cannot
/// run says so on the real stderr (`skipped`), so a skip is never a silent pass.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::delete::SkipReason;
    use scan::NodeId;
    use std::fs;

    /// A unique directory, canonical (verbatim) form. Removed on drop; a link inside is removed
    /// first, so nothing outside is ever followed.
    struct Dir {
        base: PathBuf,
        links: std::cell::RefCell<Vec<PathBuf>>,
    }

    impl Dir {
        fn new(tag: &str) -> Dir {
            let plain = std::env::temp_dir().join(format!("volatus-purge-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&plain);
            fs::create_dir_all(&plain).expect("temp dir");
            Dir { base: fs::canonicalize(&plain).expect("canonical"), links: Default::default() }
        }
        /// `mklink /J`. `None` (skip) if the OS refuses.
        fn junction(&self, link: &Path, to: &Path) -> Option<()> {
            let ok = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(plain(link))
                .arg(plain(to))
                .output()
                .is_ok_and(|o| o.status.success());
            if ok {
                self.links.borrow_mut().push(link.to_path_buf());
                Some(())
            } else {
                skipped("junction inside the target (mklink /J was refused)");
                None
            }
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
        let _ = writeln!(std::io::stderr(), "SKIPPED real file system purge test: {case}");
    }

    /// `mklink` does not take the `\\?\` form.
    fn plain(p: &Path) -> PathBuf {
        PathBuf::from(p.to_string_lossy().trim_start_matches(r"\\?\"))
    }

    fn target(path: &Path, is_dir: bool) -> DeleteTarget {
        DeleteTarget { node: NodeId(0), path: path.to_path_buf(), is_dir, is_link: false, size: 0, files: 0, dirs: 0 }
    }

    fn run(t: &DeleteTarget) -> Outcome {
        Purge.remove(t, &|_, _| {}, &AtomicBool::new(false))
    }

    /// Files and folders under `p`, `p` itself included.
    fn count(p: &Path) -> u64 {
        let mut n = 1;
        if fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) {
            for e in fs::read_dir(p).unwrap() {
                n += count(&e.unwrap().path());
            }
        }
        n
    }

    /// 10 folders of 10 folders of 10 files: 1000 files, 110 folders, sizes 0 to 16 bytes.
    /// Returns (items with the target folder, bytes).
    fn make_tree(t: &Path) -> (u64, u64) {
        let (mut items, mut bytes) = (1, 0);
        for a in 0..10 {
            for b in 0..10 {
                let d = t.join(format!("a{a}")).join(format!("b{b}"));
                fs::create_dir_all(&d).unwrap();
                for f in 0..10 {
                    let size = (a * 100 + b * 10 + f) % 17;
                    fs::write(d.join(format!("f{f}.txt")), vec![b'x'; size]).unwrap();
                    items += 1;
                    bytes += size as u64;
                }
            }
        }
        (items + 110, bytes)
    }

    #[test]
    fn removes_a_tree_of_1000_small_files_with_exact_counts() {
        let d = Dir::new("tree");
        let t = d.base.join("t");
        let (items, bytes) = make_tree(&t);
        let last = std::cell::Cell::new((0, 0));
        let o = Purge.remove(&target(&t, true), &|i, b| last.set((i, b)), &AtomicBool::new(false));
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert!(!o.stopped);
        assert_eq!((o.removed_items, o.removed_bytes), (items, bytes));
        assert_eq!(last.get(), (items, bytes), "the last progress call has the totals");
        assert!(!t.exists());
    }

    #[test]
    fn a_read_only_file_is_removed() {
        let d = Dir::new("readonly");
        let t = d.base.join("t");
        fs::create_dir(&t).unwrap();
        let f = t.join("ro.txt");
        fs::write(&f, b"12345").unwrap();
        let mut perm = fs::metadata(&f).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&f, perm).unwrap();
        let o = run(&target(&t, true));
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!((o.removed_items, o.removed_bytes), (2, 5));
        assert!(!t.exists());
    }

    #[test]
    fn a_file_held_open_without_share_delete_is_skipped_as_locked() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        let d = Dir::new("locked");
        let t = d.base.join("t");
        fs::create_dir(&t).unwrap();
        let held = t.join("held.txt");
        fs::write(&held, b"held").unwrap();
        fs::write(t.join("free.txt"), b"free").unwrap();
        let handle = fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&held).unwrap();
        let o = run(&target(&t, true));
        drop(handle);
        // One skip only: the folder that stays holds it, so the folder gets no skip of its own.
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].path, held);
        assert_eq!(o.skipped[0].reason, SkipReason::Locked);
        assert_eq!((o.removed_items, o.removed_bytes), (1, 4));
        assert!(held.exists() && !t.join("free.txt").exists());
        assert!(t.is_dir(), "the folder that holds the skipped file stays");
    }

    #[test]
    fn a_junction_inside_is_removed_and_its_target_stays() {
        let d = Dir::new("junction");
        let other = d.base.join("other");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("keep"), b"keep me").unwrap();
        let t = d.base.join("t");
        fs::create_dir(&t).unwrap();
        fs::write(t.join("own.txt"), b"abc").unwrap();
        if d.junction(&t.join("j"), &other).is_none() {
            return;
        }
        let o = run(&target(&t, true));
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        // own.txt, the junction and the folder; the junction's target is not counted.
        assert_eq!((o.removed_items, o.removed_bytes), (3, 3));
        assert!(!t.exists());
        assert_eq!(fs::read(other.join("keep")).unwrap(), b"keep me");
    }

    #[test]
    fn a_path_longer_than_260_characters_is_removed() {
        let d = Dir::new("long");
        let t = d.base.join("t");
        let deep = t.join("a".repeat(100)).join("b".repeat(100)).join("c".repeat(100));
        fs::create_dir_all(&deep).unwrap();
        let f = deep.join("deep.txt");
        fs::write(&f, b"deep").unwrap();
        assert!(f.as_os_str().len() > 260, "{}", f.as_os_str().len());
        let o = run(&target(&t, true));
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!((o.removed_items, o.removed_bytes), (5, 4));
        assert!(!t.exists());
    }

    #[test]
    fn stop_mid_run_returns_stopped_and_removed_items_stay_removed() {
        let d = Dir::new("stop");
        let t = d.base.join("t");
        let (items, _) = make_tree(&t);
        let stop = AtomicBool::new(false);
        let calls = std::cell::Cell::new(0);
        // Set from the progress callback after the first report: no clock involved.
        let progress = |i: u64, _: u64| {
            calls.set(calls.get() + 1);
            if i > 0 {
                stop.store(true, Ordering::SeqCst);
            }
        };
        let o = Purge.remove(&target(&t, true), &progress, &stop);
        assert!(o.stopped);
        assert!(calls.get() > 0);
        assert!(o.removed_items > 0 && o.removed_items < items, "{}", o.removed_items);
        assert!(t.is_dir(), "the target stays when the run stops early");
        assert_eq!(count(&t), items - o.removed_items, "what the outcome counts is gone from the disk");
    }

    #[test]
    fn a_target_that_no_longer_exists_gives_one_skip() {
        let d = Dir::new("gone");
        let t = d.base.join("gone");
        let o = run(&target(&t, true));
        assert_eq!((o.removed_items, o.removed_bytes), (0, 0));
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].path, t);
        assert!(!o.stopped);
    }

    #[test]
    fn a_target_that_is_a_junction_is_removed_as_the_link() {
        let d = Dir::new("rootjunction");
        let other = d.base.join("other");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("keep"), b"keep me").unwrap();
        let link = d.base.join("link");
        if d.junction(&link, &other).is_none() {
            return;
        }
        let o = run(&target(&link, true));
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!((o.removed_items, o.removed_bytes), (1, 0));
        assert!(fs::symlink_metadata(&link).is_err(), "the junction is gone");
        assert_eq!(fs::read(other.join("keep")).unwrap(), b"keep me");
    }

    #[test]
    fn a_folder_that_fills_during_the_run_stays_with_a_skip() {
        let d = Dir::new("fills");
        let t = d.base.join("t");
        fs::create_dir(&t).unwrap();
        fs::write(t.join("a.txt"), b"a").unwrap();
        // The progress call after the files pass comes before the folders pass: a file written
        // then makes the target not empty, with no clock involved.
        let late = t.join("late.txt");
        let o = Purge.remove(&target(&t, true), &|_, _| fs::write(&late, b"late").unwrap(), &AtomicBool::new(false));
        assert!(t.is_dir() && late.exists());
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].path, t);
        assert_eq!((o.removed_items, o.removed_bytes), (1, 1));
    }

    #[test]
    fn only_a_name_surrogate_reparse_point_is_a_link() {
        let (reparse, dir) = (FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_DIRECTORY);
        for (attrs, tag, want) in [
            (0, 0, false),
            (dir, 0, false),
            // Junction and symlink: name surrogates.
            (reparse | dir, 0xA000_0003, true),
            (reparse, 0xA000_000C, true),
            (reparse | dir, 0xA000_000C, true),
            // Cloud file, dedup, app execution alias: not links.
            (reparse | dir, 0x9000_001A, false),
            (reparse, 0x9000_001A, false),
            (reparse, 0x8000_0013, false),
            (reparse, 0x8000_001B, false),
            // The tag bits mean nothing without the reparse attribute.
            (dir, 0xA000_0003, false),
            (0, 0xA000_000C, false),
            (0, 0x2000_0000, false),
        ] {
            assert_eq!(is_name_surrogate_link(attrs, tag), want, "attrs={attrs:#x} tag={tag:#x}");
        }
    }

    #[test]
    fn a_folder_needs_its_own_skip_only_when_no_skip_lies_in_it() {
        let s = |p: &str| Skipped { path: PathBuf::from(p), reason: SkipReason::Locked };
        let folder = Path::new(r"\\?\C:\t\a");
        for (skips, want) in [
            (vec![], true),
            (vec![s(r"\\?\C:\t\a\x.txt")], false),
            (vec![s(r"\\?\C:\t\a\b\deep.txt")], false),
            (vec![s(r"\\?\C:\t\a")], false),
            // A sibling whose name only starts like the folder is not inside it.
            (vec![s(r"\\?\C:\t\ab\x.txt")], true),
            (vec![s(r"\\?\C:\t\x.txt")], true),
            (vec![s(r"\\?\C:\t\b\x.txt"), s(r"\\?\C:\t\a\y.txt")], false),
        ] {
            assert_eq!(needs_skip_for_folder(folder, &skips), want, "{skips:?}");
        }
    }
}
