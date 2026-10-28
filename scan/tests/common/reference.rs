//! Test-only oracle: `entries` and `rollup` exactly as they were at 81181b2, and the seeded
//! generator both fuzz suites share. The library's own functions are compared against them.

use scan::mft::Record;
use scan::rollup::{RawEntry, Rolled};
use scan::{NameRef, Node, NodeId};
use scan::time::UNKNOWN;
use std::collections::HashMap;

/// Seeded xorshift64 (the generator of mft_records.rs). No new dependency, fully deterministic.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform-enough value in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// True with probability `percent` / 100.
    pub fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

/// `scan::mft::entries` at 81181b2, verbatim, plus the time rule: an extension's time fills a
/// base whose time is unknown (the first such extension in input order). Extensions merge into
/// the LAST base with a number (the index collect overwrites); an extension whose base is itself
/// an extension is dropped.
pub fn entries_reference(records: Vec<Record>) -> (Vec<RawEntry>, u64) {
    let (mut bases, extensions): (Vec<Record>, Vec<Record>) = records.into_iter().partition(|r| r.base.is_none());
    let index: HashMap<u64, usize> = bases.iter().enumerate().map(|(i, r)| (r.number, i)).collect();
    for e in extensions {
        if let Some(&i) = e.base.and_then(|b| index.get(&b)) {
            let b = &mut bases[i];
            b.names.extend(e.names);
            b.data = b.data.or(e.data);
            b.surrogate |= e.surrogate;
            if b.modified == UNKNOWN {
                b.modified = e.modified;
            }
        }
    }
    let mut skipped = 0;
    let mut out = Vec::with_capacity(bases.len());
    for r in bases {
        let data = r.data.unwrap_or_default();
        let Some(first) = r.names.into_iter().next() else {
            if data != scan::mft::DataSize::default() {
                skipped += 1;
            }
            continue;
        };
        let is_dir = r.is_dir && !r.surrogate;
        let (size, allocated) = if is_dir { (0, 0) } else { (data.size, data.allocated) };
        out.push(RawEntry { id: r.number, parent: first.parent, name: first.name, size, allocated, is_dir, modified: r.modified });
    }
    (out, skipped)
}

type EntryFields = (u64, u64, String, u64, u64, bool, u32);

fn fields(list: &[RawEntry]) -> Vec<EntryFields> {
    list.iter().map(|e| (e.id, e.parent, e.name.clone(), e.size, e.allocated, e.is_dir, e.modified)).collect()
}

/// Panics at the first differing entry (field tuples, in order) or count.
pub fn assert_same_entries(got: &(Vec<RawEntry>, u64), want: &(Vec<RawEntry>, u64)) {
    assert_eq!(got.1, want.1, "skipped count");
    assert_eq!(got.0.len(), want.0.len(), "entry count");
    let (g, w) = (fields(&got.0), fields(&want.0));
    for (i, (a, b)) in g.iter().zip(&w).enumerate() {
        assert_eq!(a, b, "entry {i} (id, parent, name, size, allocated, is_dir, modified)");
    }
}

/// What the 81181b2 `rollup` produced, as plain data (`Tree` cannot be built outside the crate).
pub struct RefRolled {
    pub nodes: Vec<Node>,
    pub names: String,
    pub root: NodeId,
    pub files: u64,
    pub dirs: u64,
    pub orphans: u64,
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

/// `scan::rollup::rollup` at 81181b2, verbatim except the hasher (std default) and the result
/// type. `None` = `ScanError::NoRoot`.
pub fn rollup_reference(root: u64, entries: Vec<RawEntry>) -> Option<RefRolled> {
    let mut skipped = 0u64;
    let mut index: HashMap<u64, usize> = HashMap::with_capacity(entries.len());
    let mut state = vec![State::Unknown; entries.len()];
    for (i, e) in entries.iter().enumerate() {
        if let std::collections::hash_map::Entry::Vacant(v) = index.entry(e.id) {
            v.insert(i);
        } else {
            state[i] = State::Dropped;
        }
    }
    let root_idx = *index.get(&root)?;
    state[root_idx] = State::Reachable;

    let parent_idx: Vec<Option<usize>> =
        entries.iter().map(|e| index.get(&e.parent).copied().filter(|&p| entries[p].is_dir)).collect();

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
                        None => break true,
                    }
                }
            }
        };
        let s = if reachable { State::Reachable } else { State::Dropped };
        for &c in &chain {
            state[c] = s;
        }
    }

    let mut new_id = vec![NO_NODE; entries.len()];
    let mut nodes: Vec<Node> = Vec::with_capacity(entries.len() + 1);
    let mut names = String::with_capacity(entries.iter().map(|e| e.name.len()).sum::<usize>() + ORPHAN_NAME.len());
    let (mut files, mut dirs, mut orphans) = (0u64, 0u64, 0u64);
    let mut push_node = |name: &str, size: u64, allocated: u64, is_dir: bool, modified: u32| -> u32 {
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
            modified,
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
        let (size, allocated) = if e.is_dir { (0, 0) } else { (e.size, e.allocated) };
        let modified = if e.is_dir { UNKNOWN } else { e.modified };
        new_id[i] = push_node(&e.name, size, allocated, e.is_dir, modified);
    }
    let orphan_node = (orphans > 0).then(|| push_node(ORPHAN_NAME, 0, 0, true, UNKNOWN));
    let root_node = new_id[root_idx];

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
        let modified = nodes[n as usize].modified;
        let p = nodes[n as usize].parent.expect("linked").0 as usize;
        nodes[p].size += size;
        nodes[p].allocated += allocated;
        nodes[p].modified = nodes[p].modified.max(modified);
    }
    Some(RefRolled { nodes, names, root: NodeId(root_node), files, dirs, orphans, skipped })
}

/// Panics with the first differing NodeId and field. Also walks the tree from the root and
/// requires it to visit exactly `want.nodes.len()` nodes (no unreachable or missing node).
pub fn assert_same_tree(got: &Rolled, want: &RefRolled) {
    assert_eq!(
        (got.files, got.dirs, got.orphans, got.skipped),
        (want.files, want.dirs, want.orphans, want.skipped),
        "counts (files, dirs, orphans, skipped)"
    );
    assert_eq!(got.tree.root(), want.root, "root");
    let mut visited = 0usize;
    let mut stack = vec![got.tree.root()];
    while let Some(n) = stack.pop() {
        visited += 1;
        assert!(visited <= want.nodes.len(), "walk from the root visits more than {} nodes", want.nodes.len());
        stack.extend(got.tree.children(n));
    }
    assert_eq!(visited, want.nodes.len(), "nodes reachable from the root");
    for (i, w) in want.nodes.iter().enumerate() {
        let id = NodeId(i as u32);
        let g = got.tree.node(id);
        assert_eq!(g.parent, w.parent, "node {i} parent");
        assert_eq!((g.name.start, g.name.len), (w.name.start, w.name.len), "node {i} name start/len");
        assert_eq!(got.tree.name(id), &want.names[w.name.start as usize..(w.name.start + w.name.len) as usize], "node {i} name");
        assert_eq!(g.size, w.size, "node {i} size");
        assert_eq!(g.allocated, w.allocated, "node {i} allocated");
        assert_eq!(g.is_dir, w.is_dir, "node {i} is_dir");
        assert_eq!(g.modified, w.modified, "node {i} modified");
        assert_eq!(g.first_child, w.first_child, "node {i} first_child");
        assert_eq!(g.next_sibling, w.next_sibling, "node {i} next_sibling");
    }
}
