//! Disk-usage scanner: builds a size-rolled-up `Tree` of a folder or volume.

pub mod mft;
pub mod rollup;
pub mod walk;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::time::Duration;

/// Index of a node in the `Tree` arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

/// Slice of the tree's interned name buffer.
#[derive(Debug, Clone, Copy)]
pub struct NameRef {
    pub start: u32,
    pub len: u32,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub name: NameRef,
    pub size: u64,
    pub allocated: u64,
    pub is_dir: bool,
    pub first_child: Option<NodeId>,
    pub next_sibling: Option<NodeId>,
}

/// Flat arena of nodes plus one shared name buffer.
#[derive(Debug)]
pub struct Tree {
    pub(crate) nodes: Vec<Node>,
    pub(crate) names: String,
    pub(crate) root: NodeId,
}

impl Tree {
    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn node(&self, n: NodeId) -> &Node {
        &self.nodes[n.0 as usize]
    }

    pub fn name(&self, n: NodeId) -> &str {
        let r = self.node(n).name;
        &self.names[r.start as usize..(r.start + r.len) as usize]
    }

    pub fn children(&self, n: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(self.node(n).first_child, move |&c| self.node(c).next_sibling)
    }

    /// Names from the root down to `n`, joined. The root's name is its full path.
    pub fn path(&self, n: NodeId) -> PathBuf {
        let mut parts = Vec::new();
        let mut cur = Some(n);
        while let Some(c) = cur {
            parts.push(self.name(c));
            cur = self.node(c).parent;
        }
        parts.iter().rev().collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Mft,
    Walk,
}

#[derive(Debug, Clone)]
pub struct ScanStats {
    /// File nodes.
    pub files: u64,
    /// Directory nodes, root included.
    pub dirs: u64,
    /// Entries that were unreadable, malformed or unreachable.
    pub skipped: u64,
    pub elapsed: Duration,
    pub method: Method,
    /// Time spent in each part of the scan.
    pub phases: Phases,
    /// Worker threads the scan ran on (the rayon pool size).
    pub threads: usize,
}

/// Time spent in each part of a scan. On the MFT path `read` and `parse` run at the same time,
/// so their sum can exceed the total.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Phases {
    /// Opening the volume and reading the boot sector and `$MFT` record 0. Zero for the walker.
    pub open: Duration,
    /// Busy time reading `$MFT` (MFT path) or listing directories (walker).
    pub read: Duration,
    /// Busy time parsing FILE records. Zero for the walker.
    pub parse: Duration,
    /// Linking parents and rolling up sizes.
    pub rollup: Duration,
}

/// Default bytes per `$MFT` read.
pub const DEFAULT_CHUNK: usize = 4 << 20;

/// Tuning knobs. `Default` gives the values `scan` uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanOptions {
    /// Bytes per `$MFT` read. Rounded down to whole records and clusters, at least one of each.
    pub chunk: usize,
    /// Worker threads. `None` = rayon's global pool (one per logical CPU).
    pub threads: Option<usize>,
    /// Scanner to run. `None` = the MFT, falling back to the walker.
    pub method: Option<Method>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self { chunk: DEFAULT_CHUNK, threads: None, method: None }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub files: u64,
    pub dirs: u64,
}

#[derive(Debug)]
pub enum ScanError {
    Cancelled,
    /// The root entry is missing from the input.
    NoRoot,
    Io(std::io::Error),
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::Cancelled => write!(f, "scan cancelled"),
            ScanError::NoRoot => write!(f, "root entry not found"),
            ScanError::Io(e) => write!(f, "i/o error: {e}"),
        }
    }
}

impl std::error::Error for ScanError {}

/// Scans `root`: reads the NTFS `$MFT` when elevated, else walks directories.
/// Any MFT failure other than cancel (no admin, not NTFS, bad volume header or `$MFT` run
/// list) falls back to the walker; `ScanStats::method` says which one ran.
pub fn scan(
    root: &Path,
    progress: Sender<Progress>,
    cancel: &AtomicBool,
) -> Result<(Tree, ScanStats), ScanError> {
    scan_with(root, progress, cancel, &ScanOptions::default())
}

/// `scan` with tuning knobs. `opts.method` set to one scanner runs only that one, with no
/// fallback; `opts.threads` runs the scan on its own thread pool of that size.
pub fn scan_with(
    root: &Path,
    progress: Sender<Progress>,
    cancel: &AtomicBool,
    opts: &ScanOptions,
) -> Result<(Tree, ScanStats), ScanError> {
    let run = || match opts.method {
        Some(Method::Mft) => mft::scan(root, &progress, cancel, opts.chunk),
        Some(Method::Walk) => walk::walk(root, &progress, cancel),
        None => match mft::scan(root, &progress, cancel, opts.chunk) {
            Err(ScanError::Cancelled) => Err(ScanError::Cancelled),
            Err(_) => walk::walk(root, &progress, cancel),
            done => done,
        },
    };
    match opts.threads {
        None => run(),
        Some(n) => rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .map_err(|e| ScanError::Io(std::io::Error::other(e)))?
            .install(run),
    }
}
