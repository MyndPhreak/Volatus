mod common;

use common::{TempDir, make_placeholder, set_reparse_tag};
use scan::{Method, ScanError, scan};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;

fn run(dir: &Path, cancel: &AtomicBool) -> Result<(scan::Tree, scan::ScanStats), ScanError> {
    let (tx, _rx) = channel();
    scan(dir, tx, cancel)
}

#[test]
fn scan_known_tree_totals_match() {
    let t = TempDir::new();
    t.file("a.bin", 100);
    t.file("sub/b.bin", 2000);
    t.file("sub/deep/c.bin", 30);

    let (tree, stats) = run(t.path(), &AtomicBool::new(false)).unwrap();

    assert_eq!(tree.node(tree.root()).size, 2130);
    assert_eq!((stats.files, stats.dirs, stats.skipped), (3, 3, 0));
    assert_eq!(stats.method, Method::Walk);
}

#[test]
fn scan_reports_progress() {
    let t = TempDir::new();
    t.file("a.bin", 1);
    t.file("sub/b.bin", 1);
    let (tx, rx) = channel();

    scan(t.path(), tx, &AtomicBool::new(false)).unwrap();

    let last = rx.try_iter().last().expect("at least one progress message");
    assert_eq!((last.files, last.dirs), (2, 2));
}

#[test]
fn scan_cancel_flag_set_returns_cancelled() {
    let t = TempDir::new();
    t.file("a.bin", 1);
    t.file("sub/b.bin", 1);
    let cancel = AtomicBool::new(false);
    cancel.store(true, Ordering::Relaxed);

    let result = run(t.path(), &cancel);

    assert!(matches!(result, Err(ScanError::Cancelled)));
}

#[test]
fn scan_root_missing_returns_io_error() {
    let t = TempDir::new();

    let result = run(&t.path().join("nope"), &AtomicBool::new(false));

    assert!(matches!(result, Err(ScanError::Io(_))));
}

#[test]
fn scan_junction_is_not_followed() {
    let t = TempDir::new();
    t.file("real/big.bin", 5000);
    let out = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(t.path().join("link"))
        .arg(t.path().join("real"))
        .output()
        .unwrap();
    assert!(out.status.success(), "mklink failed: {out:?}");

    let (tree, stats) = run(t.path(), &AtomicBool::new(false)).unwrap();

    assert_eq!(tree.node(tree.root()).size, 5000);
    assert_eq!(stats.files, 2, "big.bin plus the junction entry itself");
}

#[test]
fn scan_reparse_placeholder_file_counts_logical_size() {
    let t = TempDir::new();
    let p = t.file("cloud.docx", 777);
    make_placeholder(&p);

    let (tree, _) = run(t.path(), &AtomicBool::new(false)).unwrap();

    assert_eq!(tree.node(tree.root()).size, 777);
}

/// Runs the walker alone, so these tests pin it even when the process is elevated.
fn walk_only(dir: &Path) -> (scan::Tree, scan::ScanStats) {
    let (tx, _rx) = channel();
    scan::walk::walk(dir, &tx, &AtomicBool::new(false)).unwrap()
}

#[test]
fn walk_hardlink_two_names_in_one_folder_counts_size_once() {
    let t = TempDir::new();
    let a = t.file("a.bin", 1000);
    std::fs::hard_link(&a, t.path().join("b.bin")).unwrap();

    let (tree, _) = walk_only(t.path());

    assert_eq!(tree.node(tree.root()).size, 1000);
}

#[test]
fn walk_hardlink_names_in_two_folders_count_size_once() {
    let t = TempDir::new();
    let x = t.file("a/x.bin", 5000);
    std::fs::create_dir(t.path().join("b")).unwrap();
    std::fs::hard_link(&x, t.path().join("b").join("y.bin")).unwrap();

    let (tree, _) = walk_only(t.path());

    assert_eq!(tree.node(tree.root()).size, 5000);
}

#[test]
fn walk_hardlink_two_names_lists_one_file_like_mft() {
    let t = TempDir::new();
    let a = t.file("a.bin", 1000);
    std::fs::hard_link(&a, t.path().join("b.bin")).unwrap();

    let (_, stats) = walk_only(t.path());

    assert_eq!(stats.files, 1);
}

#[test]
fn walk_non_surrogate_reparse_directory_is_descended() {
    let t = TempDir::new();
    let d = t.path().join("synced");
    std::fs::create_dir(&d).unwrap();
    set_reparse_tag(&d, 0x1000_0123); // directory bit set, name-surrogate bit clear (cloud-like)
    std::fs::write(d.join("inner.bin"), vec![0u8; 4321]).unwrap();

    let (tree, _) = walk_only(t.path());

    assert_eq!(tree.node(tree.root()).size, 4321);
}

/// Denies list-directory to the current user; restores on drop so cleanup works.
struct Denied(PathBuf, String);

impl Denied {
    fn new(path: PathBuf) -> Self {
        let user = std::env::var("USERNAME").unwrap();
        let out = Command::new("icacls")
            .arg(&path)
            .args(["/deny", &format!("{user}:(RD)")])
            .output()
            .unwrap();
        assert!(out.status.success(), "icacls deny failed: {out:?}");
        Denied(path, user)
    }
}

impl Drop for Denied {
    fn drop(&mut self) {
        let _ = Command::new("icacls").arg(&self.0).args(["/remove:d", &self.1]).output();
    }
}

#[test]
fn scan_unreadable_dir_counts_skipped_and_continues() {
    let t = TempDir::new();
    t.file("ok/a.bin", 40);
    t.file("locked/secret.bin", 999);
    let _guard = Denied::new(t.path().join("locked"));

    let (tree, stats) = run(t.path(), &AtomicBool::new(false)).unwrap();

    assert_eq!(stats.skipped, 1);
    assert_eq!(tree.node(tree.root()).size, 40);
}

#[test]
fn scan_unreadable_root_returns_io_error() {
    let t = TempDir::new();
    t.file("secret.bin", 999);
    let _guard = Denied::new(t.path().to_path_buf());

    let result = run(t.path(), &AtomicBool::new(false));

    assert!(matches!(result, Err(ScanError::Io(_))), "got {:?}", result.map(|(_, s)| s.skipped));
}
