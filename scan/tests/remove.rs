//! `Tree::remove` over random trees, checked against a tree rolled up again from scratch from
//! the entries that are left after each removal.
mod common;

use common::reference::Rng;
use scan::rollup::{RawEntry, rollup};
use scan::{NodeId, Removed, Tree};
use std::collections::HashSet;

/// Random entries in walk order: entry `i` has id `i` and a folder before it as its parent, so
/// every entry is reachable and node `i` of the rolled-up tree is entry `i`.
fn random_entries(rng: &mut Rng) -> Vec<RawEntry> {
    let n = 1 + rng.below(300);
    let mut dirs = vec![0u64];
    let mut entries = vec![RawEntry { id: 0, parent: 0, name: "root".into(), size: 0, allocated: 0, is_dir: true }];
    for id in 1..n as u64 {
        let parent = dirs[rng.below(dirs.len())];
        let is_dir = rng.chance(35);
        let size = if is_dir { 0 } else { [0, rng.next() % 10, rng.next() % (1 << 20), rng.next() % (1 << 40)][rng.below(4)] };
        let allocated = if is_dir { 0 } else { size.div_ceil(4096) * 4096 + rng.below(2) as u64 * 4096 };
        if is_dir {
            dirs.push(id);
        }
        entries.push(RawEntry { id, parent, name: format!("n{id}"), size, allocated, is_dir });
    }
    entries
}

/// True when entry `i` or one of its ancestors is in `gone`.
fn under_gone(entries: &[RawEntry], gone: &HashSet<u64>, mut i: u64) -> bool {
    loop {
        if gone.contains(&i) {
            return true;
        }
        if i == 0 {
            return false;
        }
        i = entries[i as usize].parent;
    }
}

/// The tree rolled up from scratch from the entries left, and the node of each original entry
/// in it (`None`: removed).
fn recomputed(entries: &[RawEntry], gone: &HashSet<u64>) -> (Tree, Vec<Option<NodeId>>) {
    let mut map = vec![None; entries.len()];
    let mut left = Vec::new();
    for e in entries {
        if !under_gone(entries, gone, e.id) {
            map[e.id as usize] = Some(NodeId(left.len() as u32));
            left.push(e.clone());
        }
    }
    (rollup(0, left).unwrap().tree, map)
}

/// Every original node of `tree` against the recomputed tree: attached or not, and for the
/// attached ones the parent, totals and children in order.
fn assert_matches(tree: &Tree, entries: &[RawEntry], gone: &HashSet<u64>, case: &str) {
    let (want, map) = recomputed(entries, gone);
    let to_new = |n: NodeId| map[n.0 as usize].unwrap_or_else(|| panic!("{case}: {n:?} is attached in the tree but removed"));
    for (i, new) in map.iter().enumerate() {
        let n = NodeId(i as u32);
        assert_eq!(tree.is_attached(n), new.is_some(), "{case}: is_attached({n:?})");
        let Some(new) = *new else { continue };
        let (got, exp) = (tree.node(n), want.node(new));
        assert_eq!((got.size, got.allocated, got.is_dir), (exp.size, exp.allocated, exp.is_dir), "{case}: totals of {n:?}");
        assert_eq!(got.parent.map(to_new), exp.parent, "{case}: parent of {n:?}");
        let kids: Vec<NodeId> = tree.children(n).map(to_new).collect();
        assert_eq!(kids, want.children(new).collect::<Vec<_>>(), "{case}: children of {n:?}");
    }
}

/// A node to remove: often attached, sometimes a child of the root, sometimes one already
/// removed or under a removed one, sometimes the root.
fn pick(rng: &mut Rng, tree: &Tree, entries: &[RawEntry], gone: &HashSet<u64>) -> u64 {
    let n = entries.len();
    match rng.below(10) {
        0 => 0,
        1 => tree.children(tree.root()).nth(rng.below(n)).map_or(0, |c| u64::from(c.0)),
        2 if !gone.is_empty() => *gone.iter().nth(rng.below(gone.len())).unwrap(),
        _ => rng.below(n) as u64,
    }
}

#[test]
fn remove_random_removals_match_a_tree_rolled_up_again_from_scratch() {
    let mut rng = Rng::new(0x005E_ED0F_2E30);
    for t in 0..400 {
        let entries = random_entries(&mut rng);
        let mut tree = rollup(0, entries.clone()).unwrap().tree;
        let mut gone = HashSet::new();
        for k in 0..1 + rng.below(12) {
            let i = pick(&mut rng, &tree, &entries, &gone);
            let case = format!("tree {t}, removal {k} of node {i}");
            let (before, map) = recomputed(&entries, &gone);
            let want = map[i as usize].filter(|_| i != 0).map(|new| Removed {
                parent: NodeId(entries[i as usize].parent as u32),
                size: before.node(new).size,
                allocated: before.node(new).allocated,
            });

            let got = tree.remove(NodeId(i as u32));

            assert_eq!(got, want, "{case}");
            if got.is_some() {
                gone.insert(i);
            }
            assert_matches(&tree, &entries, &gone, &case);
        }
    }
}
