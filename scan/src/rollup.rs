//! Parent linking and size roll-up from flat records (shared by walker and MFT).

use crate::{NameRef, Node, NodeId, ScanError, Tree};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Multiply-rotate hash for `u64` ids (the FxHash step). Ids are record numbers, walk order
/// numbers or NTFS file IDs, never chosen by a remote party, so hash flooding is not a concern.
/// SipHash cost 0.6 s of a 2.8 s roll-up on 4.2 million entries.
#[derive(Default)]
pub(crate) struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x51_7C_C1_B7_27_22_0A_95);
    }
}

pub(crate) type IdMap<V> = HashMap<u64, V, BuildHasherDefault<IdHasher>>;

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
    /// File entries in the tree.
    pub files: u64,
    /// Directory entries in the tree, root included, the synthetic `$Orphan` excluded.
    pub dirs: u64,
    /// Entries whose parent reference was missing (attached under `$Orphan`).
    pub orphans: u64,
    /// Entries unreachable from the root (parent cycles) or with a duplicate id.
    pub skipped: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Unknown,
    Visiting,
    Reachable,
    Dropped,
}

const NO_NODE: u32 = u32::MAX;
const ORPHAN_NAME: &str = "$Orphan";

/// Builds the tree. `root` is the id of the root entry; its own parent ref is ignored.
/// Iterative throughout: depth and cycles cannot overflow the stack or loop forever.
pub fn rollup(root: u64, entries: Vec<RawEntry>) -> Result<Rolled, ScanError> {
    let mut skipped = 0u64;
    let mut index: IdMap<usize> = IdMap::with_capacity_and_hasher(entries.len(), Default::default());
    let mut state = vec![State::Unknown; entries.len()];
    for (i, e) in entries.iter().enumerate() {
        if let std::collections::hash_map::Entry::Vacant(v) = index.entry(e.id) {
            v.insert(i);
        } else {
            state[i] = State::Dropped; // first entry with an id wins; counted below
        }
    }
    let root_idx = *index.get(&root).ok_or(ScanError::NoRoot)?;
    state[root_idx] = State::Reachable;

    // Parent entry index; None = missing or not a directory (orphan).
    let parent_idx: Vec<Option<usize>> = entries
        .iter()
        .map(|e| index.get(&e.parent).copied().filter(|&p| entries[p].is_dir))
        .collect();

    // Resolve reachability by walking up each chain once.
    let mut chain = Vec::new();
    for start in 0..entries.len() {
        if state[start] != State::Unknown {
            continue;
        }
        chain.clear();
        let mut cur = start;
        let reachable = loop {
            match state[cur] {
                State::Reachable => break true,
                State::Dropped | State::Visiting => break false,
                State::Unknown => {
                    state[cur] = State::Visiting;
                    chain.push(cur);
                    match parent_idx[cur] {
                        Some(p) => cur = p,
                        None => break true, // orphan, attached under $Orphan
                    }
                }
            }
        };
        let s = if reachable { State::Reachable } else { State::Dropped };
        for &c in &chain {
            state[c] = s;
        }
    }

    // Arena: reachable entries in input order, `$Orphan` last.
    let mut new_id = vec![NO_NODE; entries.len()];
    let mut nodes: Vec<Node> = Vec::with_capacity(entries.len() + 1);
    let mut names = String::with_capacity(entries.iter().map(|e| e.name.len()).sum::<usize>() + ORPHAN_NAME.len());
    let (mut files, mut dirs, mut orphans) = (0u64, 0u64, 0u64);
    let mut push_node = |name: &str, size: u64, allocated: u64, is_dir: bool| -> u32 {
        let start = names.len() as u32;
        names.push_str(name);
        nodes.push(Node {
            parent: None,
            name: NameRef { start, len: name.len() as u32 },
            size,
            allocated,
            is_dir,
            first_child: None,
            next_sibling: None,
        });
        (nodes.len() - 1) as u32
    };
    for (i, e) in entries.iter().enumerate() {
        if state[i] != State::Reachable {
            skipped += 1;
            continue;
        }
        if e.is_dir {
            dirs += 1;
        } else {
            files += 1;
        }
        if i != root_idx && parent_idx[i].is_none() {
            orphans += 1;
        }
        // Directory sizes come from the roll-up, never from the record.
        let (size, allocated) = if e.is_dir { (0, 0) } else { (e.size, e.allocated) };
        new_id[i] = push_node(&e.name, size, allocated, e.is_dir);
    }
    let orphan_node = (orphans > 0).then(|| push_node(ORPHAN_NAME, 0, 0, true));
    let root_node = new_id[root_idx];

    // Link children, preserving input order: prepend while walking backwards.
    let link = |nodes: &mut Vec<Node>, child: u32, parent: u32| {
        nodes[child as usize].parent = Some(NodeId(parent));
        nodes[child as usize].next_sibling = nodes[parent as usize].first_child;
        nodes[parent as usize].first_child = Some(NodeId(child));
    };
    if let Some(o) = orphan_node {
        link(&mut nodes, o, root_node);
    }
    for i in (0..entries.len()).rev() {
        if new_id[i] == NO_NODE || i == root_idx {
            continue;
        }
        let parent = match parent_idx[i] {
            Some(p) => new_id[p],
            None => orphan_node.expect("orphans > 0"),
        };
        link(&mut nodes, new_id[i], parent);
    }

    // Roll sizes up: breadth-first order, then children before parents in reverse.
    let mut order = vec![root_node];
    let mut head = 0;
    while head < order.len() {
        let mut c = nodes[order[head] as usize].first_child;
        head += 1;
        while let Some(NodeId(n)) = c {
            order.push(n);
            c = nodes[n as usize].next_sibling;
        }
    }
    for &n in order.iter().skip(1).rev() {
        let (size, allocated) = (nodes[n as usize].size, nodes[n as usize].allocated);
        let p = nodes[n as usize].parent.expect("linked").0 as usize;
        nodes[p].size += size;
        nodes[p].allocated += allocated;
    }

    // Freeing millions of names takes about 0.5 s: done off the caller's thread.
    std::thread::spawn(move || drop(entries));
    let tree = Tree { nodes, names, root: NodeId(root_node) };
    Ok(Rolled { tree, files, dirs, orphans, skipped })
}
