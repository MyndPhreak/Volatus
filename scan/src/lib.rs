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
#[derive(Debug, Clone)]
pub struct Tree {
    pub(crate) nodes: Vec<Node>,
    pub(crate) names: String,
    pub(crate) root: NodeId,
}

/// What `Tree::remove` took out of the tree: the folder the node was in and the node's totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Removed {
    pub parent: NodeId,
    /// `size` of the removed node: taken off every ancestor.
    pub size: u64,
    /// `allocated` of the removed node: taken off every ancestor.
    pub allocated: u64,
}

impl Tree {
    /// Unlinks `n` from its parent's child list and takes its `size` and `allocated` off every
    /// ancestor, the root included. The node and its subtree stay in the arena, detached
    /// (`is_attached` is false for each); the other children keep their order. `None`, and
    /// nothing changes, for the root or a node that is already detached.
    /// ponytail: O(siblings + depth); finding the previous sibling walks the parent's child list.
    pub fn remove(&mut self, n: NodeId) -> Option<Removed> {
        if !self.is_attached(n) {
            return None;
        }
        let Node { parent, size, allocated, next_sibling, .. } = *self.node(n);
        // The root has no parent.
        let parent = parent?;
        if self.node(parent).first_child == Some(n) {
            self.nodes[parent.0 as usize].first_child = next_sibling;
        } else {
            // An attached node is in its parent's list, so `prev` is found.
            let prev = self.children(parent).find(|&c| self.node(c).next_sibling == Some(n))?;
            self.nodes[prev.0 as usize].next_sibling = next_sibling;
        }
        let gone = &mut self.nodes[n.0 as usize];
        (gone.parent, gone.next_sibling) = (None, None);
        let mut up = Some(parent);
        while let Some(p) = up {
            let node = &mut self.nodes[p.0 as usize];
            // Every ancestor holds at least `n`'s totals; saturating keeps a bad tree from panicking.
            node.size = node.size.saturating_sub(size);
            node.allocated = node.allocated.saturating_sub(allocated);
            up = node.parent;
        }
        Some(Removed { parent, size, allocated })
    }

    /// True when `n` is the root or its chain of parents reaches the root.
    pub fn is_attached(&self, n: NodeId) -> bool {
        std::iter::successors(Some(n), |&c| self.node(c).parent).any(|c| c == self.root)
    }
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

/// `scan_with` for a given `route` and MFT scanner `mft`: the seam that lets a test inject an MFT
/// failure on any folder.
#[doc(hidden)]
pub fn scan_routed(
    root: &Path,
    progress: Sender<Progress>,
    cancel: &AtomicBool,
    opts: &ScanOptions,
    route: Route,
    mft: impl FnOnce() -> Result<(Tree, ScanStats), ScanError> + Send,
) -> Result<(Tree, ScanStats), ScanError> {
    let Route { method, fallback_to_walk } = route;
    let pool_error = |e: rayon::ThreadPoolBuildError| ScanError::Io(std::io::Error::other(e));
    // The walker runs at below-normal priority, so the window keeps drawing (P8 start-up stall):
    // on `threads` threads, or with no count on all CPUs but one. The MFT path stays on the global pool.
    let walk = || {
        let logical = std::thread::available_parallelism().map_or(1, |n| n.get());
        let n = opts.threads.unwrap_or_else(|| walker_threads(logical));
        background_pool(n).map_err(pool_error)?.install(|| walk::walk(root, &progress, cancel))
    };
    let run = || match method {
        Method::Walk => walk(),
        Method::Mft => match mft() {
            Err(ScanError::Cancelled) => Err(ScanError::Cancelled),
            Err(_) if fallback_to_walk => walk(),
            done => done,
        },
    };
    match opts.threads {
        // The walker makes its own pool (above): an outer one would only sit idle.
        Some(n) if method == Method::Mft => rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .map_err(pool_error)?
            .install(run),
        _ => run(),
    }
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
    let route = route(whole_volume, opts.method);
    let for_mft = progress.clone();
    scan_routed(root, progress, cancel, opts, route, || mft::scan(root, &for_mft, cancel, opts.chunk))
}

#[cfg(test)]
mod tests {
    use super::{NodeId, Removed, Tree};
    use crate::rollup::{RawEntry, rollup};

    // root(0)
    //   a(1)      a1(4)=100/104  a2(5)=50/56  sub(6){ s1(7)=30/32 }
    //   b(2)      b1(8)=400/400
    //   c(3)=10/16 (file)
    // Node ids follow the entry order (every entry is reachable).
    fn sample() -> Tree {
        let e = |id: u64, parent: u64, name: &str, size: u64, allocated: u64, is_dir: bool| RawEntry {
            id,
            parent,
            name: name.into(),
            size,
            allocated,
            is_dir,
        };
        let entries = vec![
            e(0, 0, "root", 0, 0, true),
            e(1, 0, "a", 0, 0, true),
            e(2, 0, "b", 0, 0, true),
            e(3, 0, "c", 10, 16, false),
            e(4, 1, "a1", 100, 104, false),
            e(5, 1, "a2", 50, 56, false),
            e(6, 1, "sub", 0, 0, true),
            e(7, 6, "s1", 30, 32, false),
            e(8, 2, "b1", 400, 400, false),
        ];
        rollup(0, entries).unwrap().tree
    }

    const ROOT: NodeId = NodeId(0);
    const A: NodeId = NodeId(1);
    const B: NodeId = NodeId(2);
    const C: NodeId = NodeId(3);
    const A1: NodeId = NodeId(4);
    const A2: NodeId = NodeId(5);
    const SUB: NodeId = NodeId(6);
    const S1: NodeId = NodeId(7);
    const B1: NodeId = NodeId(8);

    fn kids(tree: &Tree, n: NodeId) -> Vec<NodeId> {
        tree.children(n).collect()
    }

    fn totals(tree: &Tree, n: NodeId) -> (u64, u64) {
        (tree.node(n).size, tree.node(n).allocated)
    }

    #[test]
    fn remove_first_middle_or_last_child_unlinks_it_and_keeps_the_others_in_order() {
        let cases = [(A1, vec![A2, SUB]), (A2, vec![A1, SUB]), (SUB, vec![A1, A2])];
        for (gone, want) in cases {
            let mut tree = sample();

            tree.remove(gone);

            assert_eq!(kids(&tree, A), want, "removed {gone:?}");
        }
    }

    #[test]
    fn remove_subtracts_size_and_allocated_from_every_ancestor_root_included() {
        let mut tree = sample();

        let removed = tree.remove(S1);

        assert_eq!(removed, Some(Removed { parent: SUB, size: 30, allocated: 32 }));
        assert_eq!([totals(&tree, SUB), totals(&tree, A), totals(&tree, ROOT)], [(0, 0), (150, 160), (560, 576)]);
    }

    #[test]
    fn remove_of_a_folder_detaches_every_node_under_it_and_siblings_stay_attached() {
        let mut tree = sample();

        tree.remove(A);

        let attached: Vec<bool> = [A, A1, A2, SUB, S1, B, B1, C, ROOT].iter().map(|&n| tree.is_attached(n)).collect();
        assert_eq!(attached, [false, false, false, false, false, true, true, true, true]);
    }

    #[test]
    fn remove_of_the_root_or_a_detached_node_is_none_and_changes_nothing() {
        for gone in [ROOT, SUB, S1] {
            let mut tree = sample();
            tree.remove(A);
            let before = format!("{tree:?}");

            let removed = tree.remove(gone);

            assert_eq!((removed, format!("{tree:?}")), (None, before), "removed {gone:?}");
        }
    }

    #[test]
    fn remove_child_then_its_parent_takes_only_the_parent_remaining_size_off_the_ancestors() {
        let mut tree = sample();
        tree.remove(A1);

        let removed = tree.remove(A);

        assert_eq!(removed, Some(Removed { parent: ROOT, size: 80, allocated: 88 }));
        assert_eq!(totals(&tree, ROOT), (410, 416));
    }

    #[test]
    fn path_of_a_surviving_node_is_unchanged_after_a_removal() {
        let mut tree = sample();
        let before = [tree.path(B1), tree.path(C), tree.path(S1)];

        tree.remove(A2);

        assert_eq!([tree.path(B1), tree.path(C), tree.path(S1)], before);
    }
}
