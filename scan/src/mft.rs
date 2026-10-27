//! NTFS fast path: reads `$MFT` from the raw volume and parses FILE records.
//!
//! Clean-room from the public NTFS on-disk format. The volume is only ever opened read-only.
//! Every offset and length read from disk is bounds-checked: a bad record is counted and
//! skipped, never trusted, never a panic.
//!
//! Records are merged by the base-record reference in each extension record, so the content of
//! `$ATTRIBUTE_LIST` is never needed. Only the unnamed `$DATA` stream counts (the walker sees the
//! same). A file with several hard links is one entry, under its first non-DOS name.

use crate::rollup::{IdMap, RawEntry, Rolled, rollup};
use crate::{DEFAULT_CHUNK, Method, NodeId, Phases, Progress, ScanError, ScanStats, Tree};
use rayon::prelude::*;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, Prefix};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::time::{Duration, Instant};

/// Record number of the volume's root directory.
pub const ROOT_RECORD: u64 = 5;

/// A FILE record that failed validation (bad signature, fixup mismatch, out-of-bounds field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed;

/// One `$FILE_NAME` that is not a DOS 8.3 alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileName {
    /// Parent directory record number.
    pub parent: u64,
    pub name: String,
}

/// Sizes of the unnamed `$DATA` stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DataSize {
    pub size: u64,
    pub allocated: u64,
}

/// Facts parsed from one in-use FILE record (base or extension).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub number: u64,
    /// Base record number when this is an extension record.
    pub base: Option<u64>,
    pub is_dir: bool,
    /// POSIX, Win32 and Win32+DOS names, in attribute order. DOS-only names are dropped.
    pub names: Vec<FileName>,
    /// Unnamed `$DATA` sizes, from the resident value or the extent that starts at VCN 0.
    pub data: Option<DataSize>,
    /// Holds a name-surrogate reparse point (junction, symlink): never followed.
    pub surrogate: bool,
}

/// Low 48 bits of a file reference are the record number; the high 16 are a sequence number.
const REF_MASK: u64 = (1 << 48) - 1;
/// Update sequence stride: every 512 bytes ends with the update sequence number.
const STRIDE: usize = 512;
const IN_USE: u16 = 0x01;
const IS_DIR: u16 = 0x02;
const END: u32 = 0xFFFF_FFFF;
const FILE_NAME: u32 = 0x30;
const DATA: u32 = 0x80;
const REPARSE_POINT: u32 = 0xC0;
const DOS_NAMESPACE: u8 = 2;
const COMPRESSED_OR_SPARSE: u16 = 0x0001 | 0x8000;
pub(crate) const NAME_SURROGATE: u32 = 0x2000_0000;
/// Read buffers shared by the reader and the parser: one being read, one queued, one parsed.
const BUFFERS: usize = 3;

fn get<const N: usize>(b: &[u8], at: usize) -> Option<[u8; N]> {
    b.get(at..at.checked_add(N)?)?.try_into().ok()
}
fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    get(b, at).map(u16::from_le_bytes)
}
pub(crate) fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    get(b, at).map(u32::from_le_bytes)
}
pub(crate) fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    get(b, at).map(u64::from_le_bytes)
}

/// Checks and undoes the update sequence: each stride's last word must hold the USN and is
/// replaced by the saved word from the array. The array must cover the buffer exactly.
fn fixup(rec: &mut [u8]) -> Option<()> {
    let usa = usize::from(u16_at(rec, 4)?);
    let count = usize::from(u16_at(rec, 6)?);
    if count == 0 || (count - 1) * STRIDE != rec.len() {
        return None;
    }
    let usn = get::<2>(rec, usa)?;
    for i in 1..count {
        let saved = get::<2>(rec, usa.checked_add(2 * i)?)?;
        let end = i * STRIDE - 2;
        if get::<2>(rec, end)? != usn {
            return None;
        }
        rec[end..end + 2].copy_from_slice(&saved);
    }
    Some(())
}

/// One attribute: its type and its bytes (header included).
struct Attr<'a> {
    ty: u32,
    bytes: &'a [u8],
}

impl Attr<'_> {
    fn non_resident(&self) -> bool {
        self.bytes[8] != 0 // length >= 0x10 is checked when the attribute is cut out
    }
    fn unnamed(&self) -> bool {
        self.bytes[9] == 0
    }
    fn flags(&self) -> Option<u16> {
        u16_at(self.bytes, 0x0C)
    }
    fn value(&self) -> Option<&[u8]> {
        let len = u32_at(self.bytes, 0x10)? as usize;
        let off = usize::from(u16_at(self.bytes, 0x14)?);
        self.bytes.get(off..off.checked_add(len)?)
    }
}

/// Attributes of a fixed-up record, up to the end marker. Errors on any out-of-bounds field.
fn attributes(rec: &[u8]) -> Result<Vec<Attr<'_>>, Malformed> {
    let used = u32_at(rec, 0x18).ok_or(Malformed)? as usize;
    let body = rec.get(..used).ok_or(Malformed)?;
    let mut off = usize::from(u16_at(rec, 0x14).ok_or(Malformed)?);
    let mut out = Vec::new();
    loop {
        let ty = u32_at(body, off).ok_or(Malformed)?;
        if ty == END {
            return Ok(out);
        }
        let len = u32_at(body, off + 4).ok_or(Malformed)? as usize;
        if len < 0x18 {
            return Err(Malformed); // also guarantees progress
        }
        let bytes = body.get(off..off.checked_add(len).ok_or(Malformed)?).ok_or(Malformed)?;
        out.push(Attr { ty, bytes });
        off += len;
    }
}

fn file_name(a: &Attr) -> Option<Option<FileName>> {
    let v = a.value()?;
    let parent = u64_at(v, 0)? & REF_MASK;
    let len = usize::from(*v.get(0x40)?);
    if *v.get(0x41)? == DOS_NAMESPACE {
        return Some(None);
    }
    let raw = v.get(0x42..0x42 + 2 * len)?;
    let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
    Some(Some(FileName { parent, name: String::from_utf16_lossy(&units) }))
}

/// Sizes of an unnamed `$DATA` attribute; `None` inside for an extent past VCN 0.
fn data_size(a: &Attr) -> Option<Option<DataSize>> {
    if !a.non_resident() {
        // Resident data lives inside the record: no clusters allocated.
        return Some(Some(DataSize { size: a.value()?.len() as u64, allocated: 0 }));
    }
    if u64_at(a.bytes, 0x10)? != 0 {
        return Some(None);
    }
    let size = u64_at(a.bytes, 0x30)?;
    let allocated = if a.flags()? & COMPRESSED_OR_SPARSE != 0 {
        // The compressed-size field exists only in the longer header.
        if u16_at(a.bytes, 0x20)? < 0x48 {
            return None;
        }
        u64_at(a.bytes, 0x40)?
    } else {
        u64_at(a.bytes, 0x28)?
    };
    Some(Some(DataSize { size, allocated }))
}

/// Parses one FILE record in place (applies fixups). `Ok(None)` = not in use or never used.
pub fn parse_record(buf: &mut [u8], number: u64) -> Result<Option<Record>, Malformed> {
    match buf.get(..4) {
        Some(b"FILE") => {}
        Some([0, 0, 0, 0]) => return Ok(None),
        _ => return Err(Malformed),
    }
    fixup(buf).ok_or(Malformed)?;
    let flags = u16_at(buf, 0x16).ok_or(Malformed)?;
    if flags & IN_USE == 0 {
        return Ok(None);
    }
    let base_ref = u64_at(buf, 0x20).ok_or(Malformed)?;
    let mut rec = Record {
        number,
        base: (base_ref != 0).then_some(base_ref & REF_MASK),
        is_dir: flags & IS_DIR != 0,
        names: Vec::new(),
        data: None,
        surrogate: false,
    };
    for a in attributes(buf)? {
        match a.ty {
            FILE_NAME => rec.names.extend(file_name(&a).ok_or(Malformed)?),
            DATA if a.unnamed() => {
                if let Some(d) = data_size(&a).ok_or(Malformed)? {
                    rec.data = Some(d);
                }
            }
            // A non-resident reparse buffer is never a junction or symlink (those are small).
            REPARSE_POINT if !a.non_resident() => {
                let tag = u32_at(a.value().ok_or(Malformed)?, 0).ok_or(Malformed)?;
                rec.surrogate = tag & NAME_SURROGATE != 0;
            }
            _ => {}
        }
    }
    Ok(Some(rec))
}

/// Merges extension records into their base and emits one entry per named base record.
/// Returns the entries and the count of records dropped as unusable.
pub fn entries(mut records: Vec<Record>) -> (Vec<RawEntry>, u64) {
    // Extensions are rare (about 0.5 %): index only them, by base number, in input order.
    // Value: (the LAST base record with that number, the extension records).
    let mut ext: IdMap<(Option<usize>, Vec<usize>)> = IdMap::default();
    for (i, r) in records.iter().enumerate() {
        if let Some(b) = r.base {
            ext.entry(b).or_default().1.push(i);
        }
    }
    let ext_count: usize = ext.values().map(|(_, v)| v.len()).sum();
    if !ext.is_empty() {
        for (i, r) in records.iter().enumerate() {
            if let (None, Some(slot)) = (r.base, ext.get_mut(&r.number)) {
                slot.0 = Some(i);
            }
        }
    }
    // An extension whose base is gone is stale: its base was already counted or never in use.
    // One whose base number belongs to another extension finds no base and is dropped too.
    for (base, list) in ext.values() {
        let Some(base) = *base else { continue };
        for &e in list {
            let (names, data, surrogate) = {
                let e = &mut records[e];
                (std::mem::take(&mut e.names), e.data, e.surrogate)
            };
            let b = &mut records[base];
            b.names.extend(names);
            b.data = b.data.or(data);
            b.surrogate |= surrogate;
        }
    }
    let mut skipped = 0;
    let mut out = Vec::with_capacity(records.len() - ext_count);
    for r in records.into_iter().filter(|r| r.base.is_none()) {
        let data = r.data.unwrap_or_default();
        let Some(first) = r.names.into_iter().next() else {
            // Reserved system records are in use with no name and no data: nothing is lost.
            if data != DataSize::default() {
                skipped += 1;
            }
            continue;
        };
        // A junction or directory symlink is a zero-size file entry, as in the walker.
        let is_dir = r.is_dir && !r.surrogate;
        let (size, allocated) = if is_dir { (0, 0) } else { (data.size, data.allocated) };
        out.push(RawEntry { id: r.number, parent: first.parent, name: first.name, size, allocated, is_dir, modified: crate::time::UNKNOWN });
    }
    (out, skipped)
}

fn bad(msg: &'static str) -> ScanError {
    ScanError::Io(io::Error::new(io::ErrorKind::InvalidData, msg))
}

struct Geometry {
    cluster: u64,
    record: usize,
    mft_lcn: u64,
}

/// Reads the NTFS boot sector fields this scanner needs, validating each.
fn parse_boot(b: &[u8]) -> Option<Geometry> {
    if b.get(3..11)? != b"NTFS    " {
        return None;
    }
    let sector = u64::from(u16_at(b, 0x0B)?);
    let spc = *b.get(0x0D)?;
    // Values above 0x80 encode 2^(256 - v) sectors per cluster (large clusters).
    let spc = if spc <= 0x80 { u64::from(spc) } else { 1u64.checked_shl(256 - u32::from(spc))? };
    let cluster = sector.checked_mul(spc)?;
    if !cluster.is_power_of_two() || !(512..=2 << 20).contains(&cluster) {
        return None;
    }
    let per = *b.get(0x40)? as i8;
    let record = if per > 0 { cluster * per as u64 } else { 1u64.checked_shl(u32::from(per.unsigned_abs()))? };
    if record % STRIDE as u64 != 0 || !(512..=1 << 16).contains(&record) {
        return None;
    }
    Some(Geometry { cluster, record: record as usize, mft_lcn: u64_at(b, 0x30)? })
}

/// Little-endian integer of 1..=8 bytes, sign-extended when `signed`.
fn le(bytes: &[u8], signed: bool) -> i64 {
    let mut v = [0u8; 8];
    v[..bytes.len()].copy_from_slice(bytes);
    let shift = 64 - 8 * bytes.len() as u32;
    let v = u64::from_le_bytes(v) << shift;
    if signed { (v as i64) >> shift } else { (v >> shift) as i64 }
}

/// Decodes mapping pairs into `(lcn, clusters)` runs. Sparse runs are rejected: `$MFT` has none.
fn decode_runs(mp: &[u8]) -> Option<Vec<(u64, u64)>> {
    let (mut i, mut lcn, mut out) = (0usize, 0i64, Vec::new());
    loop {
        let h = *mp.get(i)?;
        if h == 0 {
            return Some(out);
        }
        let (nl, no) = (usize::from(h & 0x0F), usize::from(h >> 4));
        if !(1..=8).contains(&nl) || !(1..=8).contains(&no) {
            return None;
        }
        let len = le(mp.get(i + 1..i + 1 + nl)?, false);
        lcn = lcn.checked_add(le(mp.get(i + 1 + nl..i + 1 + nl + no)?, true))?;
        if lcn < 0 || len <= 0 {
            return None;
        }
        out.push((lcn as u64, len as u64));
        i += 1 + nl + no;
    }
}

/// `$MFT`'s own runs and data size, from record 0.
fn mft_runs(rec0: &mut [u8]) -> Option<(Vec<(u64, u64)>, u64)> {
    fixup(rec0)?;
    let attrs = attributes(rec0).ok()?;
    let a = attrs.iter().find(|a| a.ty == DATA && a.unnamed() && a.non_resident())?;
    if u64_at(a.bytes, 0x10)? != 0 {
        return None;
    }
    let mp = usize::from(u16_at(a.bytes, 0x20)?);
    Some((decode_runs(a.bytes.get(mp..)?)?, u64_at(a.bytes, 0x30)?))
}

fn read_at<R: Read + Seek>(dev: &mut R, offset: u64, buf: &mut [u8]) -> Result<(), ScanError> {
    dev.seek(SeekFrom::Start(offset)).and_then(|_| dev.read_exact(buf)).map_err(ScanError::Io)
}

/// Parses whole records in parallel. Returns records in use and the count of malformed ones.
/// Checks `cancel` before each record.
fn parse_records(
    buf: &mut [u8],
    record: usize,
    first: u64,
    cancel: &AtomicBool,
) -> Result<(Vec<Record>, u64), ScanError> {
    let parsed: Vec<_> = buf
        .par_chunks_mut(record)
        .enumerate()
        .map(|(i, r)| {
            if cancel.load(Ordering::Relaxed) {
                return Err(ScanError::Cancelled);
            }
            Ok(parse_record(r, first + i as u64))
        })
        .collect::<Result<_, _>>()?;
    let mut skipped = 0;
    let records = parsed
        .into_iter()
        .filter_map(|r| r.unwrap_or_else(|Malformed| {
            skipped += 1;
            None
        }))
        .collect();
    Ok((records, skipped))
}

/// Byte ranges `(offset, length)` of `$MFT` on the volume, in order, covering `data_size`
/// rounded up to whole clusters. The caller has checked that the runs cover `data_size`.
fn extents(runs: &[(u64, u64)], cluster: u64, data_size: u64) -> Result<Vec<(u64, u64)>, ScanError> {
    let mut want = data_size.div_ceil(cluster) * cluster;
    let mut out = Vec::new();
    for &(lcn, len) in runs {
        if want == 0 {
            break;
        }
        // `len * cluster` cannot overflow: the caller summed it with checked arithmetic.
        let n = (len * cluster).min(want);
        let at = lcn
            .checked_mul(cluster)
            .filter(|a| a.checked_add(n).is_some())
            .ok_or_else(|| bad("bad $MFT run"))?;
        out.push((at, n));
        want -= n;
    }
    Ok(out)
}

/// One buffer of `$MFT` bytes: the number of its first record and the length of its whole
/// records inside `$MFT`'s data size.
struct Batch {
    first: u64,
    buf: Vec<u8>,
    len: usize,
}

/// Reads `extents` into the buffers from `empty` and sends each one to `full` when it is full.
/// Buffers hold whole records, so a record split across two runs is joined here. Checks
/// `cancel` before each read. Returns its busy time; returns early, without error, when the
/// parser has stopped.
fn read_batches<R: Read + Seek>(
    dev: &mut R,
    extents: &[(u64, u64)],
    (data_size, record): (u64, usize),
    full: SyncSender<Batch>,
    empty: Receiver<Vec<u8>>,
    cancel: &AtomicBool,
) -> Result<Duration, ScanError> {
    let mut busy = Duration::ZERO;
    let (mut first, mut filled) = (0u64, 0usize);
    let Ok(mut buf) = empty.recv() else { return Ok(busy) };
    let batch = |first: u64, buf: Vec<u8>, filled: usize| {
        let inside = data_size.saturating_sub(first * record as u64).min(filled as u64) as usize;
        Batch { first, buf, len: inside / record * record }
    };
    for &(mut at, mut left) in extents {
        while left > 0 {
            if cancel.load(Ordering::Relaxed) {
                return Err(ScanError::Cancelled);
            }
            let n = ((buf.len() - filled) as u64).min(left) as usize;
            let t = Instant::now();
            read_at(dev, at, &mut buf[filled..filled + n])?;
            busy += t.elapsed();
            (at, left, filled) = (at + n as u64, left - n as u64, filled + n);
            if filled == buf.len() {
                if full.send(batch(first, buf, filled)).is_err() {
                    return Ok(busy);
                }
                first += (filled / record) as u64;
                filled = 0;
                buf = match empty.recv() {
                    Ok(b) => b,
                    Err(_) => return Ok(busy),
                };
            }
        }
    }
    if filled > 0 {
        let _ = full.send(batch(first, buf, filled));
    }
    Ok(busy)
}

/// Parses each batch from `full` and gives its buffer back to `empty`. Ends when the reader
/// stops sending. Returns the records in use, the count of malformed ones and the busy time.
fn parse_batches(
    full: Receiver<Batch>,
    empty: Sender<Vec<u8>>,
    record: usize,
    progress: &Sender<Progress>,
    cancel: &AtomicBool,
) -> Result<(Vec<Record>, u64, Duration), ScanError> {
    let (mut records, mut skipped, mut busy) = (Vec::new(), 0u64, Duration::ZERO);
    let (mut files, mut dirs) = (0u64, 0u64);
    for mut b in full {
        let t = Instant::now();
        let (parsed, bad_count) = parse_records(&mut b.buf[..b.len], record, b.first, cancel)?;
        busy += t.elapsed();
        skipped += bad_count;
        for r in parsed.iter().filter(|r| r.base.is_none()) {
            if r.is_dir { dirs += 1 } else { files += 1 }
        }
        records.extend(parsed);
        // Err: the reader has sent its last batch and needs no more buffers.
        let _ = empty.send(b.buf);
        let _ = progress.send(Progress { files, dirs });
    }
    Ok((records, skipped, busy))
}

/// Bytes per read buffer: `chunk` rounded down to whole `unit`s (at least one), never more than
/// `total` bytes of `$MFT` rounded up to whole units, so a tiny `$MFT` allocates little.
pub fn buffer_size(chunk: usize, unit: u64, total: u64) -> usize {
    ((chunk as u64 / unit).max(1) * unit).min(total.div_ceil(unit).saturating_mul(unit)) as usize
}

/// Reads every `$MFT` record along the run list, `chunk` bytes per read. A reader thread fills
/// the buffers while this thread parses the previous ones (in parallel, on the current rayon
/// pool), so the disk and the CPUs work at the same time.
fn read_mft<R: Read + Seek + Send>(
    dev: &mut R,
    progress: &Sender<Progress>,
    cancel: &AtomicBool,
    chunk: usize,
) -> Result<(Vec<Record>, u64, Phases), ScanError> {
    let started = Instant::now();
    let mut boot = vec![0u8; 4096];
    read_at(dev, 0, &mut boot)?;
    let g = parse_boot(&boot).ok_or_else(|| bad("not an NTFS volume"))?;
    let mut rec0 = vec![0u8; g.cluster.max(g.record as u64) as usize];
    let mft_at = g.mft_lcn.checked_mul(g.cluster).ok_or_else(|| bad("bad $MFT location"))?;
    read_at(dev, mft_at, &mut rec0)?;
    let (runs, data_size) = mft_runs(&mut rec0[..g.record]).ok_or_else(|| bad("bad $MFT record 0"))?;
    let covered = runs.iter().try_fold(0u64, |acc, &(_, len)| acc.checked_add(len.checked_mul(g.cluster)?));
    // ponytail: runs come from record 0 only; a $MFT so fragmented that its runs continue in
    // an extension record is reported here and the scan falls back to the walker.
    if covered.is_none_or(|c| c < data_size) {
        return Err(bad("$MFT run list is incomplete"));
    }
    let open = started.elapsed();

    let extents = extents(&runs, g.cluster, data_size)?;
    let total: u64 = extents.iter().map(|&(_, n)| n).sum();
    // Buffers of whole units: no record is cut at a buffer edge, and every read starts and
    // ends on a cluster (so a sector) boundary. The unit is a multiple of both sizes: the
    // record size is a power of two or a whole number of clusters (`parse_boot`).
    let size = buffer_size(chunk, g.cluster.max(g.record as u64), total);
    let (full_tx, full_rx) = sync_channel(BUFFERS);
    let (empty_tx, empty_rx) = channel();
    for _ in 0..BUFFERS {
        let _ = empty_tx.send(vec![0u8; size]);
    }
    let record = g.record;
    let (read, parsed) = std::thread::scope(|s| -> Result<_, ScanError> {
        let reader = std::thread::Builder::new()
            .spawn_scoped(s, move || read_batches(dev, &extents, (data_size, record), full_tx, empty_rx, cancel))
            .map_err(ScanError::Io)?;
        // Returning drops this side of both channels, so a waiting reader wakes and stops.
        let parsed = parse_batches(full_rx, empty_tx, record, progress, cancel);
        Ok((reader.join().unwrap_or_else(|p| std::panic::resume_unwind(p)), parsed))
    })?;
    // A reader error means records are missing: fail, never a partial tree.
    let read = read?;
    let (records, skipped, parse) = parsed?;
    Ok((records, skipped, Phases { open, read, parse, rollup: Duration::ZERO }))
}

/// Re-roots `tree` at `node`: its subtree as flat entries, the new root named `name`.
fn subtree(tree: &Tree, node: NodeId, name: &str) -> Vec<RawEntry> {
    let mut order = vec![node];
    let mut i = 0;
    while let Some(&n) = order.get(i) {
        order.extend(tree.children(n));
        i += 1;
    }
    order
        .into_iter()
        .map(|n| {
            let nd = tree.node(n);
            RawEntry {
                id: u64::from(n.0),
                parent: u64::from(nd.parent.unwrap_or(n).0),
                name: if n == node { name.to_owned() } else { tree.name(n).to_owned() },
                size: nd.size,
                allocated: nd.allocated,
                is_dir: nd.is_dir,
                modified: crate::time::UNKNOWN,
            }
        })
        .collect()
}

/// Reads `$MFT` from an NTFS volume (or image) and builds the tree of `sub`, a path relative
/// to the volume root (empty = whole volume). The tree root is named `root_name`.
pub fn scan_device<R: Read + Seek + Send>(
    dev: &mut R,
    sub: &Path,
    root_name: &str,
    progress: &Sender<Progress>,
    cancel: &AtomicBool,
) -> Result<(Tree, ScanStats), ScanError> {
    scan_device_with(dev, sub, root_name, progress, cancel, DEFAULT_CHUNK)
}

/// `scan_device` reading `chunk` bytes per call (rounded down to whole records and clusters,
/// at least one of each), with reading and parsing overlapped. The caller picks the thread pool.
pub fn scan_device_with<R: Read + Seek + Send>(
    dev: &mut R,
    sub: &Path,
    root_name: &str,
    progress: &Sender<Progress>,
    cancel: &AtomicBool,
    chunk: usize,
) -> Result<(Tree, ScanStats), ScanError> {
    let started = Instant::now();
    let (records, bad_records, mut phases) = read_mft(dev, progress, cancel, chunk)?;
    let rollup_started = Instant::now();
    let (mut list, unusable) = entries(records);
    if let Some(root) = list.iter_mut().find(|e| e.id == ROOT_RECORD) {
        root.name = root_name.to_owned();
    }
    let volume = rollup(ROOT_RECORD, list)?;
    // Skips are volume-wide: a sub-folder scan cannot tell where a dropped record belonged.
    let skipped = bad_records + unusable + volume.skipped;
    let rolled: Rolled = if sub.as_os_str().is_empty() {
        volume
    } else {
        let t = &volume.tree;
        let node = sub
            .components()
            .try_fold(t.root(), |n, c| {
                let want = c.as_os_str().to_str()?;
                t.children(n).find(|&k| t.name(k) == want)
            })
            .ok_or(ScanError::NoRoot)?;
        if !t.node(node).is_dir {
            return Err(ScanError::Io(io::Error::new(io::ErrorKind::InvalidInput, "root is not a directory")));
        }
        rollup(u64::from(node.0), subtree(t, node, root_name))?
    };
    phases.rollup = rollup_started.elapsed();
    let stats = ScanStats {
        files: rolled.files,
        dirs: rolled.dirs,
        skipped: skipped + rolled.skipped,
        elapsed: started.elapsed(),
        method: Method::Mft,
        phases,
        threads: rayon::current_num_threads(),
    };
    Ok((rolled.tree, stats))
}

/// MFT scan of `root` on its local volume. Fails (and `scan` falls back to the walker) when
/// the process is not elevated, the volume is not NTFS, or the root is not a local drive path.
pub(crate) fn scan(
    root: &Path,
    progress: &Sender<Progress>,
    cancel: &AtomicBool,
    chunk: usize,
) -> Result<(Tree, ScanStats), ScanError> {
    let started = Instant::now();
    let full = std::fs::canonicalize(root).map_err(ScanError::Io)?;
    let mut parts = full.components();
    let letter = match parts.next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::VerbatimDisk(d) | Prefix::Disk(d) => char::from(d),
            _ => return Err(bad("not a local drive path")),
        },
        _ => return Err(bad("not a local drive path")),
    };
    let sub: std::path::PathBuf = parts.filter(|c| matches!(c, Component::Normal(_))).collect();
    // Read-only: `File::open` asks for read access only. Denied without Administrator rights.
    let mut dev = std::fs::File::open(format!(r"\\.\{letter}:")).map_err(ScanError::Io)?;
    let opened = started.elapsed();
    let (tree, mut stats) = scan_device_with(&mut dev, &sub, &root.display().to_string(), progress, cancel, chunk)?;
    stats.phases.open += opened;
    stats.elapsed += opened;
    Ok((tree, stats))
}
