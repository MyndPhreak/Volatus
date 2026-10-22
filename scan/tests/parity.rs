//! Walker vs MFT parity: the same tree must give the same totals on both paths.

mod common;

use common::ntfs::{ROOT, dir, file, volume};
use common::{TempDir, make_placeholder};
use scan::mft::scan_device;
use scan::{Method, Tree, scan};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;

/// Every node as (path, size, is_dir), sorted: equal listings mean equal totals at every level.
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

fn sample_tree(t: &TempDir) {
    t.file("a.bin", 100);
    t.file("zero.bin", 0);
    t.file("sub/b.bin", 2000);
    t.file("sub/deep/c.bin", 30);
    t.file("sub/deep/big.bin", 70_000);
    t.file("unicode-\u{540d}\u{524d}.txt", 5);
    fs::create_dir(t.path().join("empty")).unwrap();
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
            out.push((id, dir(parent, &name)));
            encode(&e.path(), id, next, out);
        } else {
            out.push((id, file(parent, &name, md.len())));
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
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
    fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
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
#[test]
#[ignore = "needs Administrator rights and an NTFS temp folder"]
fn parity_live_volume_mft_matches_walker() {
    let t = TempDir::new();
    sample_tree(&t);
    make_placeholder(&t.file("placeholder.docx", 777));
    flush_all(t.path());
    let (tx, _rx) = channel();
    let no = AtomicBool::new(false);

    let (mft_tree, mft_stats) = scan(t.path(), tx.clone(), &no).unwrap();
    let (walk_tree, _) = scan::walk::walk(t.path(), &tx, &no).unwrap();

    assert_eq!(mft_stats.method, Method::Mft, "precondition: the MFT path ran (elevated, NTFS)");
    assert_eq!(listing(&mft_tree), listing(&walk_tree));
}
