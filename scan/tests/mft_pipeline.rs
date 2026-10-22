//! Overlapped `$MFT` read and parse: same result as one big read for every chunk size, cancel
//! stops the reader, and a reader error is never a partial tree.

mod common;

use common::ntfs::{CLUSTER, MFT_LCN_A, MFT_LCN_B, ROOT, STRADDLE_512, dir, file, volume, volume_512};
use scan::mft::{scan_device, scan_device_with};
use scan::{DEFAULT_CHUNK, ScanError, ScanStats, Tree};
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, sync_channel};
use std::time::Duration;

/// Chunk sizes to try: below, at and between record (1024) and cluster (4096) sizes.
const CHUNKS: [usize; 8] = [1, 1000, 1024, 1536, 4096, 5000, 12288, DEFAULT_CHUNK];

type Summary = (Vec<(PathBuf, u64, bool)>, u64, u64, u64);

/// Every node as (path, size, is_dir), sorted, plus files, dirs and skipped.
fn summary(tree: &Tree, stats: &ScanStats) -> Summary {
    let mut out = Vec::new();
    let mut stack = vec![tree.root()];
    while let Some(n) = stack.pop() {
        out.push((tree.path(n), tree.node(n).size, tree.node(n).is_dir));
        stack.extend(tree.children(n));
    }
    out.sort();
    (out, stats.files, stats.dirs, stats.skipped)
}

fn sample() -> Vec<(u64, Vec<u8>)> {
    vec![
        (16, dir(ROOT, "data")),
        (17, file(16, "a.bin", 5000)),
        (18, dir(16, "sub")),
        (19, file(18, "b.txt", 100)),
        (20, file(ROOT, "top.txt", 7)),
    ]
}

/// About 120 records: many more 4 KB chunks than the pipeline has buffers.
fn many() -> Vec<(u64, Vec<u8>)> {
    let mut out = Vec::new();
    for d in 0..10u64 {
        let id = 16 + d * 11;
        out.push((id, dir(ROOT, &format!("d{d}"))));
        for f in 1..=10u64 {
            out.push((id + f, file(id, &format!("f{f}.bin"), d * 1000 + f * 37)));
        }
    }
    out
}

fn fixtures() -> Vec<(&'static str, Vec<u8>, &'static str)> {
    let mut broken = sample();
    broken[4].1[1022] ^= 0xFF; // top.txt: fixup mismatch, counted as skipped
    vec![
        ("sample", volume(&sample()), ""),
        ("sample sub", volume(&sample()), "data"),
        ("many", volume(&many()), ""),
        ("many sub", volume(&many()), "d3"),
        ("fixup mismatch", volume(&broken), ""),
    ]
}

fn reference(image: Vec<u8>, sub: &str) -> Summary {
    let (tx, _rx) = channel();
    let (tree, stats) =
        scan_device(&mut Cursor::new(image), Path::new(sub), "X:\\", &tx, &AtomicBool::new(false)).unwrap();
    summary(&tree, &stats)
}

/// Read/seek seam over an image: can set the cancel flag on a seek, fail reads from an
/// offset, and counts seeks made after the flag was set.
struct Dev {
    inner: Cursor<Vec<u8>>,
    cancel: Arc<AtomicBool>,
    /// Seeking to this offset sets `cancel`.
    cancel_at: Option<u64>,
    /// Reads that start in this range fail.
    fail_in: Option<Range<u64>>,
    seeks_after_cancel: usize,
}

impl Dev {
    fn new(image: Vec<u8>) -> Self {
        Dev {
            inner: Cursor::new(image),
            cancel: Arc::new(AtomicBool::new(false)),
            cancel_at: None,
            fail_in: None,
            seeks_after_cancel: 0,
        }
    }
}

impl Read for Dev {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.fail_in.as_ref().is_some_and(|r| r.contains(&self.inner.position())) {
            return Err(io::Error::other("injected read failure"));
        }
        self.inner.read(buf)
    }
}

impl Seek for Dev {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        if self.cancel.load(Ordering::Relaxed) {
            self.seeks_after_cancel += 1;
        }
        let p = self.inner.seek(pos)?;
        if Some(p) == self.cancel_at {
            self.cancel.store(true, Ordering::Relaxed);
        }
        Ok(p)
    }
}

/// Runs `scan_device_with` on its own thread. A hang (a deadlocked pipeline) fails the test.
fn run(mut dev: Dev, chunk: usize, sub: &'static str) -> (Result<Summary, ScanError>, Dev) {
    let (done_tx, done_rx) = sync_channel(1);
    std::thread::spawn(move || {
        let (tx, _rx) = channel();
        let cancel = dev.cancel.clone();
        let result = scan_device_with(&mut dev, Path::new(sub), "X:\\", &tx, &cancel, chunk)
            .map(|(tree, stats)| summary(&tree, &stats));
        let _ = done_tx.send((result, dev));
    });
    done_rx.recv_timeout(Duration::from_secs(20)).expect("scan_device_with finished within 20 s")
}

#[test]
fn scan_device_with_every_chunk_size_matches_one_big_read() {
    for (name, image, sub) in fixtures() {
        let want = reference(image.clone(), sub);
        for chunk in CHUNKS {
            let (got, _) = run(Dev::new(image.clone()), chunk, sub);

            assert_eq!(got.unwrap(), want, "fixture {name}, chunk {chunk}");
        }
    }
}

#[test]
fn scan_device_with_record_split_across_runs_is_parsed() {
    let records =
        vec![(STRADDLE_512, file(ROOT, "split.bin", 3000)), (16, dir(ROOT, "d")), (17, file(16, "x.bin", 10))];
    for chunk in [1, 512, 1024, 1536, 2048, DEFAULT_CHUNK] {
        let (got, _) = run(Dev::new(volume_512(&records)), chunk, "");

        let (list, _, _, skipped) = got.unwrap();
        let split = list.iter().find(|(p, _, _)| p.ends_with("split.bin")).map(|(_, size, _)| *size);
        assert_eq!((split, skipped), (Some(3000), 0), "chunk {chunk}");
    }
}

#[test]
fn scan_device_with_read_error_mid_mft_returns_io_error() {
    // Run B (below run A on disk) is read last: by then some chunks of run A have been parsed.
    for chunk in [4096, 12288, DEFAULT_CHUNK] {
        let mut dev = Dev::new(volume(&many()));
        dev.fail_in = Some(MFT_LCN_B * CLUSTER as u64..MFT_LCN_A * CLUSTER as u64);

        let (got, _) = run(dev, chunk, "");

        assert!(matches!(got, Err(ScanError::Io(_))), "chunk {chunk}: got {:?}", got.map(|s| s.1));
    }
}

#[test]
fn scan_device_with_cancel_during_read_returns_cancelled() {
    for chunk in [4096, 12288, DEFAULT_CHUNK] {
        let mut dev = Dev::new(volume(&many()));
        dev.cancel_at = Some(MFT_LCN_B * CLUSTER as u64);

        let (got, _) = run(dev, chunk, "");

        assert!(matches!(got, Err(ScanError::Cancelled)), "chunk {chunk}: got {:?}", got.map(|s| s.1));
    }
}

#[test]
fn scan_device_with_cancel_during_read_stops_further_reads() {
    // Cancel arrives while the second 4 KB chunk of run A is read: no read may follow it.
    let mut dev = Dev::new(volume(&many()));
    dev.cancel_at = Some((MFT_LCN_A + 1) * CLUSTER as u64);

    let (_, dev) = run(dev, 4096, "");

    assert_eq!(dev.seeks_after_cancel, 0);
}

#[test]
fn scan_device_with_cancel_set_before_start_returns_cancelled() {
    for chunk in [4096, DEFAULT_CHUNK] {
        let dev = Dev::new(volume(&many()));
        dev.cancel.store(true, Ordering::Relaxed);

        let (got, _) = run(dev, chunk, "");

        assert!(matches!(got, Err(ScanError::Cancelled)), "chunk {chunk}");
    }
}

#[test]
fn scan_device_with_small_chunks_reports_progress_per_chunk() {
    let (tx, rx) = channel();

    scan_device_with(&mut Cursor::new(volume(&many())), Path::new(""), "X:\\", &tx, &AtomicBool::new(false), 4096)
        .unwrap();

    assert!(rx.try_iter().count() > 1, "progress arrives while $MFT is read, not once at the end");
}

#[test]
fn scan_device_with_reports_read_parse_and_rollup_time() {
    let (tx, _rx) = channel();

    let (_, stats) =
        scan_device_with(&mut Cursor::new(volume(&many())), Path::new(""), "X:\\", &tx, &AtomicBool::new(false), 4096)
            .unwrap();

    let p = stats.phases;
    assert!(p.read > Duration::ZERO && p.parse > Duration::ZERO && p.rollup > Duration::ZERO, "{p:?}");
}
