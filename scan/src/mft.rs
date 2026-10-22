//! NTFS fast path: reads `$MFT` from the raw volume and parses FILE records.
//!
//! Clean-room from the public NTFS on-disk format. The volume is only ever opened read-only.

use crate::rollup::RawEntry;
use crate::{Progress, ScanError, ScanStats, Tree};
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;

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

/// Parses one FILE record in place (applies fixups). `Ok(None)` = not in use or never used.
pub fn parse_record(buf: &mut [u8], number: u64) -> Result<Option<Record>, Malformed> {
    let _ = (buf, number);
    unimplemented!("mft::parse_record")
}

/// Merges extension records into their base and emits one entry per named base record.
/// Returns the entries and the count of records dropped as unusable.
pub fn entries(records: Vec<Record>) -> (Vec<RawEntry>, u64) {
    let _ = records;
    unimplemented!("mft::entries")
}

/// Reads `$MFT` from an NTFS volume (or image) and builds the tree of `sub`, a path relative
/// to the volume root (empty = whole volume). The tree root is named `root_name`.
pub fn scan_device<R: Read + Seek>(
    dev: &mut R,
    sub: &Path,
    root_name: &str,
    progress: &Sender<Progress>,
    cancel: &AtomicBool,
) -> Result<(Tree, ScanStats), ScanError> {
    let _ = (dev, sub, root_name, progress, cancel);
    unimplemented!("mft::scan_device")
}
