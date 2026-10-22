//! Tuning knobs: defaults unchanged, thread count and method applied.

mod common;

use common::TempDir;
use scan::{DEFAULT_CHUNK, Method, ScanOptions, ScanStats, Tree, scan_with};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;
use std::time::Duration;

fn listing(tree: &Tree) -> Vec<(PathBuf, u64, bool)> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root()];
    while let Some(n) = stack.pop() {
        out.push((tree.path(n), tree.node(n).size, tree.node(n).is_dir));
        stack.extend(tree.children(n));
    }
    out.sort();
    out
}

fn sample(t: &TempDir) {
    t.file("a.bin", 100);
    t.file("sub/b.bin", 2000);
    t.file("sub/deep/c.bin", 30);
}

fn walk_with(t: &TempDir, threads: Option<usize>) -> (Tree, ScanStats) {
    let (tx, _rx) = channel();
    let opts = ScanOptions { threads, method: Some(Method::Walk), ..ScanOptions::default() };
    scan_with(t.path(), tx, &AtomicBool::new(false), &opts).unwrap()
}

#[test]
fn scan_options_default_uses_a_32_mib_chunk() {
    let o = ScanOptions::default();

    assert_eq!((o.chunk, o.threads, o.method), (32 << 20, None, None));
    assert_eq!(DEFAULT_CHUNK, 32 << 20);
}

#[test]
fn scan_with_walk_on_two_threads_matches_the_walker() {
    let t = TempDir::new();
    sample(&t);
    let (tx, _rx) = channel();
    let (want, _) = scan::walk::walk(t.path(), &tx, &AtomicBool::new(false)).unwrap();

    let (got, stats) = walk_with(&t, Some(2));

    assert_eq!((listing(&got), stats.method), (listing(&want), Method::Walk));
}

#[test]
fn scan_with_threads_runs_on_a_pool_of_that_size() {
    let t = TempDir::new();
    sample(&t);
    for n in [1, 3] {
        let (_, stats) = walk_with(&t, Some(n));

        assert_eq!(stats.threads, n);
    }
}

#[test]
fn scan_with_no_thread_count_uses_the_global_pool() {
    let t = TempDir::new();
    sample(&t);

    let (_, stats) = walk_with(&t, None);

    assert_eq!(stats.threads, rayon::current_num_threads());
}

#[test]
fn scan_with_method_mft_never_falls_back_to_the_walker() {
    // Unelevated the MFT path fails; elevated it runs. Either way the walker must not run.
    let t = TempDir::new();
    sample(&t);
    let (tx, _rx) = channel();
    let opts = ScanOptions { method: Some(Method::Mft), ..ScanOptions::default() };

    let got = scan_with(t.path(), tx, &AtomicBool::new(false), &opts);

    assert!(
        got.as_ref().is_err() || got.as_ref().is_ok_and(|(_, s)| s.method == Method::Mft),
        "{:?}",
        got.map(|(_, s)| s.method)
    );
}

#[test]
fn scan_with_default_options_matches_scan() {
    let t = TempDir::new();
    sample(&t);
    let (tx, _rx) = channel();
    let (want, want_stats) = scan::scan(t.path(), tx.clone(), &AtomicBool::new(false)).unwrap();

    let (got, stats) = scan_with(t.path(), tx, &AtomicBool::new(false), &ScanOptions::default()).unwrap();

    assert_eq!((listing(&got), stats.method), (listing(&want), want_stats.method));
}

#[test]
fn walk_reports_listing_time() {
    let t = TempDir::new();
    sample(&t);

    let (_, stats) = walk_with(&t, None);

    assert!(stats.phases.read > Duration::ZERO, "{:?}", stats.phases);
}
