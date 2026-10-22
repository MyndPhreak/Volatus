//! Disk-usage scanner: builds a size-rolled-up `Tree` of a folder or volume.

pub mod rollup;
mod walk;

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
        unimplemented!()
    }
    pub fn node(&self, _n: NodeId) -> &Node {
        unimplemented!()
    }
    pub fn name(&self, _n: NodeId) -> &str {
        unimplemented!()
    }
    #[allow(unreachable_code)]
    pub fn children(&self, _n: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        unimplemented!();
        std::iter::empty()
    }
    pub fn path(&self, _n: NodeId) -> PathBuf {
        unimplemented!()
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

pub fn scan(
    _root: &Path,
    _progress: Sender<Progress>,
    _cancel: &AtomicBool,
) -> Result<(Tree, ScanStats), ScanError> {
    unimplemented!()
}
