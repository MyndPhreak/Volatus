//! Every device read is sector aligned, and every `$MFT` data read is cluster aligned, for every
//! chunk size. Guards the unit choice in `read_mft` (`max(cluster, record)`): a record-sized unit
//! on a 4096-byte-cluster volume keeps reads sector aligned but breaks cluster alignment.

mod common;

use common::ntfs::{ROOT, dir, file, volume, volume_512};
use scan::DEFAULT_CHUNK;
use scan::mft::scan_device_with;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;

const CHUNKS: [usize; 9] = [1, 511, 1000, 1024, 1536, 4096, 5000, 12289, DEFAULT_CHUNK];
/// Reads before the `$MFT` data: the boot sector and record 0.
const SETUP_READS: usize = 2;

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

fn small512() -> Vec<(u64, Vec<u8>)> {
    vec![(12, file(ROOT, "split.bin", 3000)), (16, dir(ROOT, "d")), (17, file(16, "x.bin", 10))]
}

/// Records every read as (offset, len).
struct Recorder {
    inner: Cursor<Vec<u8>>,
    reads: Vec<(u64, usize)>,
}

impl Read for Recorder {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reads.push((self.inner.position(), buf.len()));
        self.inner.read(buf)
    }
}

impl Seek for Recorder {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn scan_device_with_every_read_is_sector_aligned_and_mft_reads_are_cluster_aligned() {
    let images = [("4096-cluster", volume(&many()), 4096u64), ("512-cluster", volume_512(&small512()), 512)];
    for (name, img, cluster) in images {
        for chunk in CHUNKS {
            let mut dev = Recorder { inner: Cursor::new(img.clone()), reads: Vec::new() };
            let (tx, _rx) = channel();

            scan_device_with(&mut dev, Path::new(""), "X:\\", &tx, &AtomicBool::new(false), chunk).unwrap();

            for (i, &(at, len)) in dev.reads.iter().enumerate() {
                assert!(at % 512 == 0 && len % 512 == 0, "{name} chunk {chunk} read {i}: ({at},{len})");
                if i >= SETUP_READS {
                    assert!(
                        at % cluster == 0 && (len as u64).is_multiple_of(cluster),
                        "{name} chunk {chunk} $MFT read {i}: ({at},{len})"
                    );
                }
            }
        }
    }
}
