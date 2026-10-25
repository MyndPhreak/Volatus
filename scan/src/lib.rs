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

/// Default bytes per `$MFT` read: 32 MiB (3 buffers = 96 MiB). Measured on a 5.1M-file volume,
/// warm: 4 MiB 7.15 s, 8: 6.26, 16: 5.58, 32: 5.26, 64: 5.16 (see docs/BENCHMARK.md).
pub const DEFAULT_CHUNK: usize = 32 << 20;

/// Tuning knobs. `Default` gives the values `scan` uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanOptions {
    /// Bytes per `$MFT` read. Rounded down to whole records and clusters, at least one of each.
    pub chunk: usize,
    /// Worker threads. `None` = the MFT on rayon's global pool (one per logical CPU), the walker
    /// on `walker_threads` at below-normal priority (`background_pool`).
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

/// Which scanner `scan_with` runs, and whether a failure of it falls back to the walker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub method: Method,
    pub fallback_to_walk: bool,
}

/// True when `path` (already canonical) is the root of a local drive: `C:\` or `\\?\C:\`.
pub fn is_volume_root(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    let mut parts = path.components();
    matches!(
        parts.next(),
        Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
    ) && parts.next() == Some(Component::RootDir)
        && parts.next().is_none()
}

/// Picks the scanner. A set `method` is obeyed, with no fallback. Otherwise only a whole-volume
/// root tries the MFT (and falls back to the walker on failure); any other root is walked,
/// because the MFT path counts a hard-linked file only under its first name.
pub fn route(whole_volume: bool, method: Option<Method>) -> Route {
    match method {
        Some(method) => Route { method, fallback_to_walk: false },
        None if whole_volume => Route { method: Method::Mft, fallback_to_walk: true },
        None => Route { method: Method::Walk, fallback_to_walk: false },
    }
}

/// Walker threads when `ScanOptions::threads` is `None`: one per logical CPU but one, at least
/// one, so the window that shows the scan keeps a CPU.
pub fn walker_threads(logical: usize) -> usize {
    logical.saturating_sub(1).max(1)
}

/// A pool of `n` threads at below-normal priority, so the window drawn while the scan runs gets
/// the CPU first.
pub fn background_pool(n: usize) -> Result<rayon::ThreadPool, rayon::ThreadPoolBuildError> {
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
    rayon::ThreadPoolBuilder::new()
        .num_threads(n)
        // SAFETY: GetCurrentThread returns a pseudo handle that is always valid on its own thread.
        // A failed priority change leaves the thread at normal priority: the scan still runs.
        .start_handler(|_| unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        })
        .build()
}

/// Scans `root`: reads the NTFS `$MFT` when elevated and `root` is a whole drive, else walks
/// directories. Any MFT failure other than cancel (no admin, not NTFS, bad volume header or
/// `$MFT` run list) falls back to the walker; `ScanStats::method` says which one ran.
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
    // A root that cannot be resolved is not a drive root: the walker reports the error.
    let whole_volume = std::fs::canonicalize(root).is_ok_and(|p| is_volume_root(&p));
    let Route { method, fallback_to_walk } = route(whole_volume, opts.method);
    let pool_error = |e: rayon::ThreadPoolBuildError| ScanError::Io(std::io::Error::other(e));
    // With no thread count the walker runs on all CPUs but one at below-normal priority, so the
    // window keeps drawing (P8 start-up stall). The MFT path stays on the global pool.
    let walk = || match opts.threads {
        Some(_) => walk::walk(root, &progress, cancel),
        None => {
            let logical = std::thread::available_parallelism().map_or(1, |n| n.get());
            background_pool(walker_threads(logical)).map_err(pool_error)?.install(|| walk::walk(root, &progress, cancel))
        }
    };
    let run = || match method {
        Method::Walk => walk(),
        Method::Mft => match mft::scan(root, &progress, cancel, opts.chunk) {
            Err(ScanError::Cancelled) => Err(ScanError::Cancelled),
            Err(_) if fallback_to_walk => walk(),
            done => done,
        },
    };
    match opts.threads {
        None => run(),
        Some(n) => rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .map_err(pool_error)?
            .install(run),
    }
}
