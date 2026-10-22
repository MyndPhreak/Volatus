//! Synthetic NTFS structures built from the public on-disk format (no real volume needed).
//!
//! Layout used here: 512-byte sectors, 4096-byte clusters, 1024-byte FILE records with a
//! 3-entry update sequence array (one USN plus one saved word per 512-byte stride).

pub const RECORD: usize = 1024;
pub const CLUSTER: usize = 4096;
pub const ROOT: u64 = 5;
pub const FIRST_USER: u64 = 16;

pub const IN_USE: u16 = 0x01;
pub const DIR: u16 = 0x02;

pub const POSIX: u8 = 0;
pub const WIN32: u8 = 1;
pub const DOS: u8 = 2;
pub const WIN32_DOS: u8 = 3;

pub const COMPRESSED: u16 = 0x0001;
pub const SPARSE: u16 = 0x8000;

pub const TAG_MOUNT_POINT: u32 = 0xA000_0003; // junction: name surrogate bit set
pub const TAG_CLOUD: u32 = 0x9000_001A; // cloud files placeholder: not a name surrogate

const USN: u16 = 0x0007;
const USA_OFFSET: usize = 0x30;
const USA_COUNT: usize = RECORD / 512 + 1;
const SEQ1: u64 = 1 << 48;

fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn align8(n: usize) -> usize {
    n.div_ceil(8) * 8
}
fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// A resident attribute (type, optional name, value).
pub fn resident_attr(ty: u32, name: &str, value: &[u8]) -> Vec<u8> {
    let name = utf16(name);
    let voff = align8(0x18 + name.len());
    let len = align8(voff + value.len());
    let mut a = vec![0u8; len];
    put32(&mut a, 0, ty);
    put32(&mut a, 4, len as u32);
    a[9] = (name.len() / 2) as u8;
    put16(&mut a, 0x0A, 0x18);
    a[0x18..0x18 + name.len()].copy_from_slice(&name);
    put32(&mut a, 0x10, value.len() as u32);
    put16(&mut a, 0x14, voff as u16);
    a[voff..voff + value.len()].copy_from_slice(value);
    a
}

/// A non-resident attribute. `compressed` adds the compressed-size field (header 0x48 bytes).
#[allow(clippy::too_many_arguments)]
pub fn nonresident_attr(
    ty: u32,
    name: &str,
    flags: u16,
    lowest_vcn: u64,
    allocated: u64,
    size: u64,
    compressed: Option<u64>,
    runs: &[u8],
) -> Vec<u8> {
    let name = utf16(name);
    let header = if compressed.is_some() { 0x48 } else { 0x40 };
    let mp = align8(header + name.len());
    let len = align8(mp + runs.len());
    let mut a = vec![0u8; len];
    put32(&mut a, 0, ty);
    put32(&mut a, 4, len as u32);
    a[8] = 1;
    a[9] = (name.len() / 2) as u8;
    put16(&mut a, 0x0A, header as u16);
    put16(&mut a, 0x0C, flags);
    a[header..header + name.len()].copy_from_slice(&name);
    let clusters = (allocated / CLUSTER as u64).max(1);
    put64(&mut a, 0x10, lowest_vcn);
    put64(&mut a, 0x18, lowest_vcn + clusters - 1);
    put16(&mut a, 0x20, mp as u16);
    put16(&mut a, 0x22, if flags & COMPRESSED != 0 { 4 } else { 0 });
    put64(&mut a, 0x28, allocated);
    put64(&mut a, 0x30, size);
    put64(&mut a, 0x38, size);
    if let Some(c) = compressed {
        put64(&mut a, 0x40, c);
    }
    a[mp..mp + runs.len()].copy_from_slice(runs);
    a
}

/// `$FILE_NAME` (0x30) in `parent` with the given namespace.
pub fn file_name(parent: u64, name: &str, namespace: u8) -> Vec<u8> {
    let n = utf16(name);
    let mut v = vec![0u8; 0x42 + n.len()];
    put64(&mut v, 0, parent | SEQ1);
    v[0x40] = (n.len() / 2) as u8;
    v[0x41] = namespace;
    v[0x42..].copy_from_slice(&n);
    resident_attr(0x30, "", &v)
}

/// Unnamed resident `$DATA` of `len` bytes.
pub fn resident_data(len: usize) -> Vec<u8> {
    resident_attr(0x80, "", &vec![0xAB; len])
}

/// Unnamed non-resident `$DATA` starting at VCN 0 (runs point at LCN 1000; never read).
pub fn data(size: u64, allocated: u64) -> Vec<u8> {
    let clusters = (allocated / CLUSTER as u64).max(1);
    nonresident_attr(0x80, "", 0, 0, allocated, size, None, &runs(&[(1000, clusters)]))
}

/// `$ATTRIBUTE_LIST` (0x20) with one entry pointing `$DATA` at extension record `ext`.
pub fn attribute_list(ext: u64) -> Vec<u8> {
    let mut e = vec![0u8; 0x20];
    put32(&mut e, 0, 0x80);
    put16(&mut e, 4, 0x20);
    e[7] = 0x1A;
    put64(&mut e, 0x10, ext | SEQ1);
    resident_attr(0x20, "", &e)
}

/// `$REPARSE_POINT` (0xC0) carrying `tag`.
pub fn reparse(tag: u32) -> Vec<u8> {
    let mut v = vec![0u8; 8];
    put32(&mut v, 0, tag);
    resident_attr(0xC0, "", &v)
}

/// A complete FILE record with fixups applied. `base` is the base record number (0 = base record).
pub fn record(flags: u16, base: u64, attrs: &[Vec<u8>]) -> Vec<u8> {
    let mut r = vec![0u8; RECORD];
    r[0..4].copy_from_slice(b"FILE");
    put16(&mut r, 4, USA_OFFSET as u16);
    put16(&mut r, 6, USA_COUNT as u16);
    put16(&mut r, 0x10, 1);
    put16(&mut r, 0x12, 1);
    let first = align8(USA_OFFSET + 2 * USA_COUNT);
    put16(&mut r, 0x14, first as u16);
    put16(&mut r, 0x16, flags);
    if base != 0 {
        put64(&mut r, 0x20, base | SEQ1);
    }
    let mut off = first;
    for a in attrs {
        r[off..off + a.len()].copy_from_slice(a);
        off += a.len();
    }
    put32(&mut r, off, 0xFFFF_FFFF);
    off += 8;
    assert!(off <= RECORD - 2, "fixture attributes do not fit in one record");
    put32(&mut r, 0x18, off as u32);
    put32(&mut r, 0x1C, RECORD as u32);
    protect(&mut r);
    r
}

/// Applies the update sequence: saves each stride's last word into the array, writes the USN.
fn protect(r: &mut [u8]) {
    put16(r, USA_OFFSET, USN);
    for i in 1..USA_COUNT {
        let end = i * 512 - 2;
        let saved = [r[end], r[end + 1]];
        r[USA_OFFSET + 2 * i..USA_OFFSET + 2 * i + 2].copy_from_slice(&saved);
        put16(r, end, USN);
    }
}

/// A plain in-use file record: one Win32 name, `size` bytes of data (resident when small).
pub fn file(parent: u64, name: &str, size: u64) -> Vec<u8> {
    let d = if size <= 256 {
        resident_data(size as usize)
    } else {
        data(size, size.div_ceil(CLUSTER as u64) * CLUSTER as u64)
    };
    record(IN_USE, 0, &[file_name(parent, name, WIN32), d])
}

/// A plain in-use directory record with one Win32 name.
pub fn dir(parent: u64, name: &str) -> Vec<u8> {
    record(IN_USE | DIR, 0, &[file_name(parent, name, WIN32)])
}

/// Mapping pairs for `(lcn, clusters)` runs, terminated by a zero byte.
pub fn runs(list: &[(u64, u64)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut prev = 0i64;
    for &(lcn, len) in list {
        let delta = lcn as i64 - prev;
        prev = lcn as i64;
        let len_bytes = len.to_le_bytes();
        let nl = (8 - len.leading_zeros() as usize / 8).max(1);
        let no = (1..=8).find(|&k| {
            let shift = 64 - 8 * k;
            ((delta << shift) >> shift) == delta
        });
        let no = no.unwrap();
        out.push(((no as u8) << 4) | nl as u8);
        out.extend_from_slice(&len_bytes[..nl]);
        out.extend_from_slice(&delta.to_le_bytes()[..no]);
    }
    out.push(0);
    out
}

/// LCNs of the two `$MFT` fragments. The second sits before the first (negative run delta).
pub const MFT_LCN_A: u64 = 40;
pub const MFT_LCN_B: u64 = 10;

/// NTFS boot sector: 512 B/sector, 8 sectors/cluster, `$MFT` at `MFT_LCN_A`, 1024-byte records.
pub fn boot_sector(total_clusters: u64) -> Vec<u8> {
    boot_sector_with(total_clusters, 8)
}

/// NTFS boot sector with `spc` 512-byte sectors per cluster.
fn boot_sector_with(total_clusters: u64, spc: u8) -> Vec<u8> {
    let mut b = vec![0u8; 512];
    b[0..3].copy_from_slice(&[0xEB, 0x52, 0x90]);
    b[3..11].copy_from_slice(b"NTFS    ");
    put16(&mut b, 0x0B, 512);
    b[0x0D] = spc;
    put64(&mut b, 0x28, total_clusters * u64::from(spc));
    put64(&mut b, 0x30, MFT_LCN_A);
    put64(&mut b, 0x38, 2);
    b[0x40] = (-10i8) as u8; // 2^10 = 1024-byte records
    b[0x44] = 1;
    b[0x1FE] = 0x55;
    b[0x1FF] = 0xAA;
    b
}

/// Options for `volume_with`.
pub struct Volume {
    /// Clusters cut from the end of `$MFT`'s run list (simulates an incomplete run list).
    pub short_runs: u64,
}

/// A volume image holding `records` (by record number). Adds `$MFT` (record 0) and a root
/// directory (record 5) unless given.
pub fn volume(records: &[(u64, Vec<u8>)]) -> Vec<u8> {
    volume_with(records, Volume { short_runs: 0 })
}

pub fn volume_with(records: &[(u64, Vec<u8>)], opts: Volume) -> Vec<u8> {
    build(records, opts.short_runs, CLUSTER)
}

/// Record that `volume_512` splits across its two `$MFT` runs: its first 512 bytes end run A,
/// its last 512 bytes start run B.
pub const STRADDLE_512: u64 = 12;

/// Like `volume`, with 512-byte clusters and an odd first run, so record `STRADDLE_512` is
/// split across the two `$MFT` runs. Record numbers up to `FIRST_USER + 8` only.
pub fn volume_512(records: &[(u64, Vec<u8>)]) -> Vec<u8> {
    assert!(records.iter().all(|(n, _)| *n <= FIRST_USER + 8), "volume_512 holds records 0..=24 only");
    build(records, 0, 512)
}

fn build(records: &[(u64, Vec<u8>)], short_runs: u64, cluster: usize) -> Vec<u8> {
    let max = records.iter().map(|(n, _)| *n).max().unwrap_or(0).max(FIRST_USER + 8);
    let size = ((max + 1) * RECORD as u64).next_multiple_of(cluster as u64);
    let clusters = size / cluster as u64;
    // Clusters smaller than a record: an odd split puts one record across the two runs.
    let a = if cluster < RECORD { (clusters / 2) | 1 } else { clusters / 2 };
    let b = clusters - a;
    assert!(MFT_LCN_B + b <= MFT_LCN_A, "fixture $MFT fragments overlap");
    if cluster < RECORD {
        assert_eq!(a * cluster as u64 / RECORD as u64, STRADDLE_512, "fixture straddle record moved");
    }
    let run_list = runs(&[(MFT_LCN_A, a), (MFT_LCN_B, b - short_runs)]);
    let mft = record(
        IN_USE,
        0,
        &[file_name(ROOT, "$MFT", WIN32_DOS), nonresident_attr(0x80, "", 0, 0, size, size, None, &run_list)],
    );
    let mut all: Vec<(u64, Vec<u8>)> = vec![(0, mft)];
    if !records.iter().any(|(n, _)| *n == ROOT) {
        all.push((ROOT, record(IN_USE | DIR, 0, &[file_name(ROOT, ".", WIN32_DOS)])));
    }
    all.extend(records.iter().cloned());

    let total_clusters = MFT_LCN_A + a + 4;
    let mut img = vec![0u8; total_clusters as usize * cluster];
    img[..512].copy_from_slice(&boot_sector_with(total_clusters, (cluster / 512) as u8));
    // Each record is copied in pieces of at most one cluster, each to its own LCN.
    let piece = cluster.min(RECORD);
    for (n, bytes) in all {
        for k in (0..RECORD).step_by(piece) {
            let v = n as usize * RECORD + k;
            let vcn = (v / cluster) as u64;
            let lcn = if vcn < a { MFT_LCN_A + vcn } else { MFT_LCN_B + (vcn - a) };
            let at = lcn as usize * cluster + v % cluster;
            img[at..at + piece].copy_from_slice(&bytes[k..k + piece]);
        }
    }
    img
}
