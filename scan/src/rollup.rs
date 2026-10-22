//! Parent linking and size roll-up from flat records (shared by walker and MFT).

use crate::{ScanError, Tree};

/// One file or directory record with a reference to its parent.
#[derive(Debug, Clone)]
pub struct RawEntry {
    pub id: u64,
    pub parent: u64,
    pub name: String,
    pub size: u64,
    pub allocated: u64,
    pub is_dir: bool,
}

#[derive(Debug)]
pub struct Rolled {
    pub tree: Tree,
    pub files: u64,
    pub dirs: u64,
    /// Entries whose parent reference was missing (attached under `$Orphan`).
    pub orphans: u64,
    /// Entries unreachable from the root (parent cycles).
    pub skipped: u64,
}

pub fn rollup(_root: u64, _entries: Vec<RawEntry>) -> Result<Rolled, ScanError> {
    unimplemented!()
}
