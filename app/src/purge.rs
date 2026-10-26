//! Fast permanent delete: one walk of the target, the files removed in parallel, then the folders
//! from the deepest up. Direct Win32 calls on verbatim (`\\?\`) paths. A link is never followed.

#![cfg_attr(not(test), expect(dead_code, reason = "wired in the UI task"))]
#![cfg_attr(test, allow(dead_code, reason = "tests do not use every item; wired in the UI task"))]

use crate::delete::{DeleteTarget, Outcome, Remover};
use std::sync::atomic::AtomicBool;

/// The fast permanent delete.
pub struct Purge;

impl Remover for Purge {
    fn remove(&self, _t: &DeleteTarget, _progress: &dyn Fn(u64, u64), _stop: &AtomicBool) -> Outcome {
        Outcome::default()
    }
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
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;

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
}
