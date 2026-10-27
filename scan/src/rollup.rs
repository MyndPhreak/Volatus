//! Parent linking and size roll-up from flat records (shared by walker and MFT).

use crate::time::UNKNOWN;
use crate::{NameRef, Node, NodeId, ScanError, Tree};
use rayon::prelude::*;
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
    /// Last write time (`time` encoding). A directory's own value is ignored.
    pub modified: u32,
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

const NO_NODE: u32 = u32::MAX;
const ORPHAN_NAME: &str = "$Orphan";

/// "No entry" in the `u32` entry-index arrays below.
const NO_ENTRY: u32 = u32::MAX;

/// Length of a dense id table (`Vec` indexed by id) for `n` entries whose largest id is
/// `max_id`, or `None` when the ids are too sparse and the `IdMap` must be used.
/// ponytail: the table may hold up to 8 slots (32 B) per entry; sparser ids take the slower
/// `IdMap`. MFT record numbers and walker ids are dense; sub-folder `subtree` ids may not be.
fn dense_len(n: usize, max_id: u64) -> Option<usize> {
    let limit = (n as u64).saturating_mul(8).saturating_add(1024);
    if max_id < limit {
        usize::try_from(max_id).ok().map(|m| m + 1)
    } else {
        None
    }
}

/// Entry index of the first entry with each id.
enum IdIndex {
    Dense(Vec<u32>),
    Map(IdMap<u32>),
}

impl IdIndex {
    /// Bounds-checked: an id beyond the table is a miss, never a panic.
    fn get(&self, id: u64) -> Option<u32> {
        match self {
            IdIndex::Dense(v) => usize::try_from(id)
                .ok()
                .and_then(|i| v.get(i))
                .copied()
                .filter(|&i| i != NO_ENTRY),
            IdIndex::Map(m) => m.get(&id).copied(),
        }
    }
}

/// Indexes entries by id. The first entry with an id wins; later ones are flagged in the
/// returned `Vec` (dropped and counted skipped).
/// ponytail: entry indexes are `u32`, the same ceiling as `NodeId`; `u32::MAX` entries of 64 B
/// each (275 GB) cannot be held in memory, so the ceiling is not checked.
fn index_ids(entries: &[RawEntry]) -> (IdIndex, Vec<bool>) {
    let mut dup = vec![false; entries.len()];
    let max_id = entries.par_iter().map(|e| e.id).max().unwrap_or(0);
    let index = match dense_len(entries.len(), max_id) {
        Some(len) => {
            let mut table = vec![NO_ENTRY; len];
            for (i, e) in entries.iter().enumerate() {
                // In range: every id is at most `max_id`.
                let slot = &mut table[e.id as usize];
                if *slot == NO_ENTRY {
                    *slot = i as u32;
                } else {
                    dup[i] = true;
                }
            }
            IdIndex::Dense(table)
        }
        None => {
            let mut map: IdMap<u32> =
                IdMap::with_capacity_and_hasher(entries.len(), Default::default());
            for (i, e) in entries.iter().enumerate() {
                if let std::collections::hash_map::Entry::Vacant(v) = map.entry(e.id) {
                    v.insert(i as u32);
                } else {
                    dup[i] = true;
                }
            }
            IdIndex::Map(map)
        }
    };
    (index, dup)
}

/// Parent entry index of every entry; `NO_ENTRY` when the parent is missing or not a
/// directory (an orphan). Resolved in parallel, in input order.
fn parent_entries(entries: &[RawEntry], index: &IdIndex) -> Vec<u32> {
    let is_dir: Vec<bool> = entries.par_iter().map(|e| e.is_dir).collect();
    entries
        .par_iter()
        .map(|e| {
            index
                .get(e.parent)
                .filter(|&p| is_dir[p as usize])
                .unwrap_or(NO_ENTRY)
        })
        .collect()
}

/// Child lists in CSR form: the children of entry `p` are `kids[off[p]..off[p + 1]]`, in input
/// order. Orphans (parent missing or not a directory) are listed apart, in input order. The root
/// and the duplicate entries are nobody's child. Returns `(off, kids, orphans)`.
fn child_lists(parent: &[u32], dup: &[bool], root: usize) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
    let n = parent.len();
    let child = |i: usize| !dup[i] && i != root;
    // Counting sort: count into off[p + 1], prefix sum, then fill with off[p] as the cursor.
    let mut off = vec![0u32; n + 1];
    let mut orphans = Vec::new();
    for (i, &p) in parent.iter().enumerate() {
        if !child(i) {
            continue;
        }
        if p == NO_ENTRY {
            orphans.push(i as u32);
        } else {
            off[p as usize + 1] += 1;
        }
    }
    for p in 0..n {
        off[p + 1] += off[p];
    }
    let mut kids = vec![0u32; off[n] as usize];
    for (i, &p) in parent.iter().enumerate() {
        if child(i) && p != NO_ENTRY {
            let cursor = &mut off[p as usize];
            kids[*cursor as usize] = i as u32;
            *cursor += 1;
        }
    }
    // Each cursor now holds the end of its list, which is the start of the next one.
    off.copy_within(0..n, 1);
    off[0] = 0;
    (off, kids, orphans)
}

/// Smallest slice of a level that rayon may split off to another thread.
const MIN_SPLIT: usize = 4096;

/// Reachable entries, level by level: level 0 is the root then the orphans; each next level is
/// the children of the previous one, parent by parent. Cycles are never reached. Returns the
/// order and the level bounds (`order[bounds[l]..bounds[l + 1]]` is level `l`).
fn levels(root: u32, orphans: &[u32], off: &[u32], kids: &[u32]) -> (Vec<u32>, Vec<usize>) {
    let mut order: Vec<u32> = Vec::with_capacity(off.len() - 1);
    order.push(root);
    order.extend_from_slice(orphans);
    let mut bounds = vec![0, order.len()];
    loop {
        let level = &order[bounds[bounds.len() - 2]..];
        // Ordered collect: the result does not depend on the thread count.
        // ponytail: min_len 4096 (MIN_SPLIT) stops rayon splitting narrow levels, which made
        // very deep trees up to 17x slower than a serial walk; tune if a level is cheaper/dearer.
        let next: Vec<u32> = level
            .par_iter()
            .with_min_len(MIN_SPLIT)
            .flat_map_iter(|&p| {
                kids[off[p as usize] as usize..off[p as usize + 1] as usize]
                    .iter()
                    .copied()
            })
            .collect();
        if next.is_empty() {
            return (order, bounds);
        }
        order.extend_from_slice(&next);
        bounds.push(order.len());
    }
}

/// Position in `order` of the first child of `order[j]`, for every `j`, plus the end. A parent's
/// children are contiguous in `order` and follow those of the parent before it.
fn first_child_positions(order: &[u32], level0: usize, off: &[u32]) -> Vec<u32> {
    let mut at: Vec<u32> = order
        .par_iter()
        .map(|&p| off[p as usize + 1] - off[p as usize])
        .collect();
    let mut next = level0 as u32;
    for a in &mut at {
        (*a, next) = (next, next + *a);
    }
    at.push(next);
    at
}

/// (size, allocated, newest modified) of every entry in `order`, children summed into parents
/// level by level from the deepest. Each parent is written by exactly one task and only integers
/// are added or compared, so the result does not depend on the thread count. Directory records'
/// own sizes and times are ignored.
fn roll_up_sizes(
    entries: &[RawEntry],
    order: &[u32],
    bounds: &[usize],
    kid_at: &[u32],
) -> Vec<(u64, u64, u32)> {
    let mut sums: Vec<(u64, u64, u32)> = order
        .par_iter()
        .map(|&i| {
            let e = &entries[i as usize];
            if e.is_dir {
                (0, 0, UNKNOWN)
            } else {
                (e.size, e.allocated, e.modified)
            }
        })
        .collect();
    for w in bounds.windows(2).rev() {
        let (start, end) = (w[0], w[1]);
        // The level's children all sit in the next level, at or after `end`.
        let (head, below) = sums.split_at_mut(end);
        head[start..]
            .par_iter_mut()
            .with_min_len(MIN_SPLIT)
            .enumerate()
            .for_each(|(k, sum)| {
                let j = start + k;
                for &(size, allocated, modified) in
                    &below[kid_at[j] as usize - end..kid_at[j + 1] as usize - end]
                {
                    sum.0 += size;
                    sum.1 += allocated;
                    sum.2 = sum.2.max(modified);
                }
            });
    }
    sums
}

/// Builds the tree. `root` is the id of the root entry; its own parent ref is ignored.
/// Iterative throughout: depth and cycles cannot overflow the stack or loop forever.
///
/// Layout (NodeIds) is fixed by the input: reachable entries in input order, then `$Orphan`
/// (only when there are orphans) as the last node and the last child of the root. Children
/// keep input order. An entry is reachable when its parent chain ends at the root or at an
/// orphan; entries in or under a parent cycle, and later entries with a duplicate id, are
/// skipped.
pub fn rollup(root: u64, entries: Vec<RawEntry>) -> Result<Rolled, ScanError> {
    let n = entries.len();
    let (index, dup) = index_ids(&entries);
    let root_idx = index.get(root).ok_or(ScanError::NoRoot)? as usize;
    let parent = parent_entries(&entries, &index);
    drop(index);
    let (off, kids, orphan_list) = child_lists(&parent, &dup, root_idx);
    drop(dup);
    let (order, bounds) = levels(root_idx as u32, &orphan_list, &off, &kids);
    let level0 = bounds[1];
    let kid_at = first_child_positions(&order, level0, &off);
    drop((off, kids));
    let mut sums = roll_up_sizes(&entries, &order, &bounds, &kid_at);
    // `$Orphan` holds the orphans' totals and newest time and rolls into the root.
    let orphan_sum = sums[1..level0]
        .iter()
        .fold((0u64, 0u64, UNKNOWN), |a, s| (a.0 + s.0, a.1 + s.1, a.2.max(s.2)));
    sums[0].0 += orphan_sum.0;
    sums[0].1 += orphan_sum.1;
    sums[0].2 = sums[0].2.max(orphan_sum.2);

    // Arena: reachable entries in input order. `pos` = position in `order`, NO_ENTRY = skipped.
    let mut pos = vec![NO_ENTRY; n];
    for (j, &i) in order.iter().enumerate() {
        pos[i as usize] = j as u32;
    }
    let mut new_id = vec![NO_NODE; n];
    let mut kept: Vec<u32> = Vec::with_capacity(order.len());
    // Name offsets wrap at u32 exactly as `names.len() as u32` did.
    let mut starts: Vec<u32> = Vec::with_capacity(order.len());
    let (mut at, mut files, mut dirs) = (0u32, 0u64, 0u64);
    for (i, e) in entries.iter().enumerate() {
        if pos[i] == NO_ENTRY {
            continue;
        }
        new_id[i] = kept.len() as u32;
        kept.push(i as u32);
        starts.push(at);
        at = at.wrapping_add(e.name.len() as u32);
        if e.is_dir {
            dirs += 1;
        } else {
            files += 1;
        }
    }
    let orphan_node = (!orphan_list.is_empty()).then_some(NodeId(kept.len() as u32));
    let names = copy_names(&entries, &kept, orphan_node.is_some());

    let node_at = |j: u32| NodeId(new_id[order[j as usize] as usize]);
    let build = |k: usize| -> Node {
        let i = kept[k] as usize;
        let e = &entries[i];
        let j = pos[i] as usize;
        let mut first_child = (kid_at[j] < kid_at[j + 1]).then(|| node_at(kid_at[j]));
        let (parent_node, next_sibling) = if j == 0 {
            // The root: its children, then `$Orphan`.
            first_child = first_child.or(orphan_node);
            (None, None)
        } else if j < level0 {
            // An orphan: a child of `$Orphan`, in input order.
            (orphan_node, (j + 1 < level0).then(|| node_at(j as u32 + 1)))
        } else {
            let p = parent[i] as usize;
            let last = kid_at[pos[p] as usize + 1] as usize;
            let next = if j + 1 < last {
                Some(node_at(j as u32 + 1))
            } else if p == root_idx {
                orphan_node
            } else {
                None
            };
            (Some(NodeId(new_id[p])), next)
        };
        let (size, allocated, modified) = sums[j];
        let name = NameRef {
            start: starts[k],
            len: e.name.len() as u32,
        };
        Node {
            parent: parent_node,
            name,
            size,
            allocated,
            is_dir: e.is_dir,
            first_child,
            next_sibling,
            modified,
        }
    };
    let root_node = node_at(0);
    let mut nodes: Vec<Node> = Vec::with_capacity(kept.len() + 1);
    (0..kept.len())
        .into_par_iter()
        .map(build)
        .collect_into_vec(&mut nodes);
    if orphan_node.is_some() {
        nodes.push(Node {
            parent: Some(root_node),
            name: NameRef {
                start: at,
                len: ORPHAN_NAME.len() as u32,
            },
            size: orphan_sum.0,
            allocated: orphan_sum.1,
            is_dir: true,
            first_child: Some(node_at(1)),
            next_sibling: None,
            modified: orphan_sum.2,
        });
    }
    let skipped = (n - order.len()) as u64;
    let orphans = orphan_list.len() as u64;

    // Freeing millions of names takes about 0.5 s: done off the caller's thread.
    // If the thread cannot start, the closure is dropped and `entries` is freed here.
    let _ = std::thread::Builder::new().spawn(move || drop(entries));
    let tree = Tree { nodes, names, root: root_node };
    Ok(Rolled { tree, files, dirs, orphans, skipped })
}

/// Entries per parallel name-copy task.
const NAME_CHUNK: usize = 1 << 16;

/// The names of `kept` entries, concatenated in order, then `$Orphan` when `orphan`. Copied in
/// parallel into disjoint slices of one buffer.
fn copy_names(entries: &[RawEntry], kept: &[u32], orphan: bool) -> String {
    let name = |i: u32| entries[i as usize].name.as_bytes();
    let chunk_len: Vec<usize> = kept
        .par_chunks(NAME_CHUNK)
        .map(|c| c.iter().map(|&i| name(i).len()).sum())
        .collect();
    let total: usize = chunk_len.iter().sum();
    let tail = if orphan { ORPHAN_NAME } else { "" };
    let mut buf = vec![0u8; total + tail.len()];
    let (mut rest, end) = buf.split_at_mut(total);
    end.copy_from_slice(tail.as_bytes());
    let mut parts: Vec<&mut [u8]> = Vec::with_capacity(chunk_len.len());
    for &len in &chunk_len {
        let (part, after) = std::mem::take(&mut rest).split_at_mut(len);
        parts.push(part);
        rest = after;
    }
    kept.par_chunks(NAME_CHUNK)
        .zip(parts)
        .for_each(|(chunk, part)| {
            let mut at = 0;
            for &i in chunk {
                let s = name(i);
                part[at..at + s.len()].copy_from_slice(s);
                at += s.len();
            }
        });
    String::from_utf8(buf).expect("a concatenation of str values is UTF-8")
}

#[cfg(test)]
mod tests {
    use super::dense_len;

    #[test]
    fn dense_len_max_id_below_8n_plus_1024_is_dense_with_one_slot_per_id() {
        let cases = [(0, 0, 1), (0, 1023, 1024), (5, 1063, 1064), (1000, 9023, 9024), (6_000_000, 5_999_999, 6_000_000)];

        for (n, max_id, want) in cases {
            assert_eq!(dense_len(n, max_id), Some(want), "n {n} max_id {max_id}");
        }
    }

    #[test]
    fn dense_len_max_id_at_or_above_8n_plus_1024_uses_the_map() {
        let cases = [(0, 1024), (5, 1064), (1000, 9024), (1, 1 << 33), (5, u64::MAX), (usize::MAX, u64::MAX)];

        for (n, max_id) in cases {
            assert_eq!(dense_len(n, max_id), None, "n {n} max_id {max_id}");
        }
    }
}
