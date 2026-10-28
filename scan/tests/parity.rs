//! Walker vs MFT parity: the same tree must give the same totals on both paths.

mod common;

use common::ntfs::{ROOT, dir, dir_at, file_at, volume};
use common::{TempDir, make_placeholder};
use scan::mft::scan_device;
use scan::{Method, ScanOptions, Tree, scan_with};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;
use std::time::{SystemTime, UNIX_EPOCH};

/// Every node as (path, size, is_dir, modified), sorted: equal listings mean equal totals and
/// times at every level.
fn listing(tree: &Tree) -> Vec<(PathBuf, u64, bool, u32)> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root()];
    while let Some(n) = stack.pop() {
        out.push((tree.path(n), tree.node(n).size, tree.node(n).is_dir, tree.node(n).modified));
        stack.extend(tree.children(n));
    }
    out.sort();
    out
}

fn sample_tree(t: &TempDir) {
    // Distinct, fixed last-write times (2021 plus a different number of days per file), so a
    // listing that drops or mixes up times cannot match.
    let stamp = |path: PathBuf, days: u64| {
        let when = UNIX_EPOCH + std::time::Duration::from_secs(1_609_459_200 + days * 86_400);
        fs::File::options().write(true).open(path).unwrap().set_modified(when).unwrap();
    };
    stamp(t.file("a.bin", 100), 1);
    stamp(t.file("zero.bin", 0), 2);
    stamp(t.file("sub/b.bin", 2000), 3);
    stamp(t.file("sub/deep/c.bin", 30), 4);
    stamp(t.file("sub/deep/big.bin", 70_000), 5);
    stamp(t.file("unicode-\u{540d}\u{524d}.txt", 5), 6);
    fs::create_dir(t.path().join("empty")).unwrap();
}

/// A `SystemTime` as a FILETIME (100 ns ticks since 1601-01-01).
fn filetime(t: SystemTime) -> i64 {
    const SECS_1601_TO_1970: u64 = 11_644_473_600;
    let d = t.duration_since(UNIX_EPOCH).unwrap();
    ((d.as_secs() + SECS_1601_TO_1970) * 10_000_000 + u64::from(d.subsec_nanos() / 100)) as i64
}

/// Encodes the real folder `path` as FILE records under `parent`, numbering from `next`.
fn encode(path: &Path, parent: u64, next: &mut u64, out: &mut Vec<(u64, Vec<u8>)>) {
    let mut items: Vec<fs::DirEntry> = fs::read_dir(path).unwrap().map(Result::unwrap).collect();
    items.sort_by_key(fs::DirEntry::file_name);
    for e in items {
        let id = *next;
        *next += 1;
        let name = e.file_name().into_string().unwrap();
        let md = e.metadata().unwrap();
        if md.is_dir() {
            out.push((id, dir_at(parent, &name, filetime(md.modified().unwrap()))));
            encode(&e.path(), id, next, out);
        } else {
            out.push((id, file_at(parent, &name, md.len(), filetime(md.modified().unwrap()))));
        }
    }
}

#[test]
fn parity_temp_tree_walker_and_mft_image_listings_equal() {
    let t = TempDir::new();
    sample_tree(&t);
    let root_name = t.path().display().to_string();
    let mut records = vec![(16, dir(ROOT, "scratch"))];
    encode(t.path(), 16, &mut 17, &mut records);
    let (tx, _rx) = channel();
    let no = AtomicBool::new(false);

    let (walked, _) = scan::walk::walk(t.path(), &tx, &no).unwrap();
    let (parsed, _) =
        scan_device(&mut Cursor::new(volume(&records)), Path::new("scratch"), &root_name, &tx, &no).unwrap();

    assert_eq!(listing(&parsed), listing(&walked));
}

#[test]
fn parity_temp_tree_walker_and_mft_image_counts_equal() {
    let t = TempDir::new();
    sample_tree(&t);
    let mut records = vec![(16, dir(ROOT, "scratch"))];
    encode(t.path(), 16, &mut 17, &mut records);
    let (tx, _rx) = channel();
    let no = AtomicBool::new(false);

    let (_, w) = scan::walk::walk(t.path(), &tx, &no).unwrap();
    let (_, m) = scan_device(&mut Cursor::new(volume(&records)), Path::new("scratch"), "r", &tx, &no).unwrap();

    assert_eq!((m.files, m.dirs, m.skipped), (w.files, w.dirs, w.skipped));
}

/// Flushes a file's or folder's data and MFT record so a raw volume read sees it.
fn flush(path: &Path) {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT};
    fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .and_then(|f| f.sync_all())
        .unwrap_or_else(|e| panic!("flush {}: {e}", path.display()));
}

fn flush_all(path: &Path) {
    for e in fs::read_dir(path).unwrap().map(Result::unwrap) {
        if e.file_type().unwrap().is_dir() {
            flush_all(&e.path());
        }
        flush(&e.path());
    }
    flush(path);
}

/// M2 support: run elevated with `cargo test -p scan --test parity -- --ignored`.
/// Forces the MFT with the method override: the default routing walks a sub-folder.
/// The scratch folder holds no hard links, which is the one case where the two paths differ.
#[test]
#[ignore = "needs Administrator rights and an NTFS temp folder"]
fn parity_live_volume_mft_matches_walker() {
    let t = TempDir::new();
    sample_tree(&t);
    make_placeholder(&t.file("placeholder.docx", 777));
    flush_all(t.path());
    let (tx, _rx) = channel();
    let no = AtomicBool::new(false);
    let forced = ScanOptions { method: Some(Method::Mft), ..ScanOptions::default() };

    let (mft_tree, mft_stats) = scan_with(t.path(), tx.clone(), &no, &forced)
        .expect("MFT scan failed: run this test from an elevated (Administrator) prompt on an NTFS drive");
    let (walk_tree, _) = scan::walk::walk(t.path(), &tx, &no).unwrap();

    assert_eq!(mft_stats.method, Method::Mft, "precondition: the MFT path ran (run elevated, NTFS)");
    assert_eq!(listing(&mft_tree), listing(&walk_tree));
}

/// Whole-volume MFT scan of the system drive.
/// Needs Administrator rights: run `cargo test -p scan --test parity -- --ignored` from an
/// elevated prompt.
/// It checks sanity, not parity: a walker run on a whole drive is too slow and too noisy.
#[test]
#[ignore = "needs Administrator rights and a large NTFS system drive"]
fn live_whole_volume_mft_scan_is_consistent() {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let root = PathBuf::from(format!("{drive}\\"));
    let (tx, _rx) = channel();
    let no = AtomicBool::new(false);

    // Forced MFT: unelevated, this fails fast instead of walking the whole drive.
    let forced = ScanOptions { method: Some(Method::Mft), ..ScanOptions::default() };
    let (tree, stats) = scan_with(&root, tx, &no, &forced)
        .expect("whole-volume scan failed: run this test from an elevated (Administrator) prompt");

    assert_eq!(stats.method, Method::Mft, "precondition: the MFT path ran (run elevated, NTFS)");
    assert_eq!(stats.skipped, 0, "no record may be skipped");
    assert!(stats.files > 100_000, "files: {}", stats.files);
    let top = tree.node(tree.root()).size;
    assert!(top > 0, "total bytes");
    let children: u64 = tree.children(tree.root()).map(|c| tree.node(c).size).sum();
    assert_eq!(top, children, "root size equals the sum of its children");
}
