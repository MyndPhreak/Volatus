mod common;

use common::reference::{Rng, assert_same_tree, rollup_reference};
use scan::ScanError;
use scan::rollup::{RawEntry, rollup};

fn dir(id: u64, parent: u64, name: &str) -> RawEntry {
    RawEntry { id, parent, name: name.into(), size: 0, allocated: 0, is_dir: true, modified: scan::time::UNKNOWN }
}

fn file(id: u64, parent: u64, name: &str, size: u64) -> RawEntry {
    RawEntry { id, parent, name: name.into(), size, allocated: size + 10, is_dir: false, modified: scan::time::UNKNOWN }
}

#[test]
fn rollup_nested_dirs_parent_size_is_sum_of_children() {
    let entries = vec![
        dir(5, 5, "root"),
        dir(6, 5, "a"),
        file(7, 6, "f1", 10),
        file(8, 6, "f2", 20),
        file(9, 5, "f3", 5),
    ];

    let r = rollup(5, entries).unwrap();

    let root = r.tree.root();
    let a = r.tree.children(root).find(|&n| r.tree.name(n) == "a").unwrap();
    assert_eq!(r.tree.node(a).size, 30);
    assert_eq!(r.tree.node(root).size, 35);
    assert_eq!(r.tree.node(root).allocated, 35 + 30);
    assert_eq!((r.files, r.dirs, r.orphans, r.skipped), (3, 2, 0, 0));
}

#[test]
fn rollup_children_keep_input_order() {
    let entries = vec![dir(1, 1, "root"), file(2, 1, "z", 1), file(3, 1, "a", 1), file(4, 1, "m", 1)];

    let r = rollup(1, entries).unwrap();

    let names: Vec<&str> = r.tree.children(r.tree.root()).map(|n| r.tree.name(n)).collect();
    assert_eq!(names, ["z", "a", "m"]);
}

#[test]
fn path_nested_node_joins_names_from_root() {
    let entries = vec![dir(1, 1, "root"), dir(2, 1, "a"), file(3, 2, "f1", 1)];

    let r = rollup(1, entries).unwrap();

    let a = r.tree.children(r.tree.root()).next().unwrap();
    let f = r.tree.children(a).next().unwrap();
    assert_eq!(r.tree.path(f), std::path::Path::new("root").join("a").join("f1"));
}

#[test]
fn rollup_orphan_parent_ref_attaches_under_orphan_dir_and_counts() {
    let entries = vec![dir(1, 1, "root"), file(2, 1, "ok", 4), file(3, 999, "lost", 7)];

    let r = rollup(1, entries).unwrap();

    let root = r.tree.root();
    let orphan = r.tree.children(root).find(|&n| r.tree.name(n) == "$Orphan").unwrap();
    let lost = r.tree.children(orphan).next().unwrap();
    assert_eq!(r.tree.name(lost), "lost");
    assert_eq!(r.tree.node(orphan).size, 7);
    assert_eq!(r.tree.node(root).size, 11);
    assert_eq!((r.orphans, r.skipped, r.files), (1, 0, 2));
}

#[test]
fn rollup_without_orphans_has_no_orphan_dir() {
    let r = rollup(1, vec![dir(1, 1, "root"), file(2, 1, "f", 1)]).unwrap();

    assert!(r.tree.children(r.tree.root()).all(|n| r.tree.name(n) != "$Orphan"));
}

#[test]
fn rollup_parent_cycle_terminates_and_counts_skipped() {
    let entries = vec![
        dir(1, 1, "root"),
        file(2, 1, "ok", 3),
        dir(3, 4, "x"),
        dir(4, 3, "y"),
        file(5, 3, "under", 100),
    ];

    let r = rollup(1, entries).unwrap();

    assert_eq!(r.skipped, 3);
    assert_eq!(r.tree.node(r.tree.root()).size, 3);
    assert_eq!((r.files, r.dirs), (1, 1));
}

#[test]
fn rollup_self_parent_non_root_counts_skipped() {
    let r = rollup(1, vec![dir(1, 1, "root"), dir(2, 2, "self")]).unwrap();

    assert_eq!(r.skipped, 1);
}

#[test]
fn rollup_missing_root_returns_no_root() {
    let err = rollup(1, vec![file(2, 1, "f", 1)]).unwrap_err();

    assert!(matches!(err, ScanError::NoRoot));
}

// ---- pins added before the P7 rewrite (green on 81181b2) ----

use scan::rollup::Rolled;
use scan::{NodeId, Tree};

fn find(t: &Tree, name: &str) -> NodeId {
    let mut stack = vec![t.root()];
    while let Some(n) = stack.pop() {
        if t.name(n) == name {
            return n;
        }
        stack.extend(t.children(n));
    }
    panic!("no node {name}")
}

fn kids(t: &Tree, n: NodeId) -> Vec<&str> {
    t.children(n).map(|c| t.name(c)).collect()
}

type Shape = Vec<(Option<NodeId>, String, u64, u64, bool, Option<NodeId>, Option<NodeId>)>;

/// Every node by NodeId: parent, name, sizes, kind, links. Walks ids until the first out of range.
fn shape(r: &Rolled) -> Shape {
    let mut out = Vec::new();
    let mut dfs = vec![r.tree.root()];
    let mut ids = Vec::new();
    while let Some(n) = dfs.pop() {
        ids.push(n);
        dfs.extend(r.tree.children(n));
    }
    ids.sort();
    for n in ids {
        let x = r.tree.node(n);
        out.push((x.parent, r.tree.name(n).to_string(), x.size, x.allocated, x.is_dir, x.first_child, x.next_sibling));
    }
    out
}

#[test]
fn rollup_duplicate_id_first_wins_and_later_is_skipped() {
    let entries = vec![dir(1, 1, "root"), dir(2, 1, "first"), dir(2, 1, "second"), file(3, 2, "c", 5)];

    let r = rollup(1, entries).unwrap();

    let t = &r.tree;
    assert_eq!(kids(t, t.root()), ["first"]);
    assert_eq!(kids(t, NodeId(1)), ["c"]);
    assert_eq!((r.skipped, r.dirs, r.files, r.orphans), (1, 2, 1, 0));
}

#[test]
fn rollup_duplicate_id_file_size_is_not_counted() {
    let entries = vec![dir(1, 1, "root"), file(2, 1, "first", 5), file(2, 1, "dup", 100)];

    let r = rollup(1, entries).unwrap();

    assert_eq!((r.tree.node(r.tree.root()).size, r.skipped), (5, 1));
}

#[test]
fn rollup_parent_is_a_file_attaches_child_under_orphan() {
    let entries = vec![dir(1, 1, "root"), file(2, 1, "f", 4), file(3, 2, "child", 6)];

    let r = rollup(1, entries).unwrap();

    let t = &r.tree;
    assert_eq!(kids(t, t.root()), ["f", "$Orphan"]);
    assert_eq!(kids(t, find(t, "$Orphan")), ["child"]);
    assert_eq!((r.orphans, r.skipped, t.node(t.root()).size), (1, 0, 10));
}

#[test]
fn rollup_several_orphans_keep_input_order_and_orphan_is_last_node_and_last_child() {
    let entries = vec![
        dir(1, 1, "root"),
        file(2, 999, "o1", 1),
        file(3, 1, "ok", 1),
        file(4, 998, "o2", 1),
        dir(5, 1, "d"),
    ];

    let r = rollup(1, entries).unwrap();

    let t = &r.tree;
    assert_eq!(kids(t, t.root()), ["ok", "d", "$Orphan"]);
    assert_eq!(kids(t, NodeId(5)), ["o1", "o2"]);
    assert_eq!((t.name(NodeId(5)), r.orphans), ("$Orphan", 2));
}

#[test]
fn rollup_orphan_size_and_allocated_roll_into_orphan_dir_and_root() {
    let entries = vec![dir(1, 1, "root"), file(2, 1, "ok", 3), file(3, 999, "lost", 7)];

    let r = rollup(1, entries).unwrap();

    let t = &r.tree;
    let o = t.node(find(t, "$Orphan"));
    assert_eq!((o.size, o.allocated), (7, 17));
    let root = t.node(t.root());
    assert_eq!((root.size, root.allocated), (10, 30));
}

#[test]
fn rollup_three_level_nesting_allocated_sums_at_every_level() {
    let entries = vec![
        dir(1, 1, "root"),
        dir(2, 1, "a"),
        dir(3, 2, "b"),
        file(4, 3, "f", 10),
        file(5, 2, "g", 3),
    ];

    let r = rollup(1, entries).unwrap();

    let t = &r.tree;
    let alloc = |n: &str| (t.node(find(t, n)).size, t.node(find(t, n)).allocated);
    assert_eq!((alloc("b"), alloc("a"), alloc("root")), ((10, 20), (13, 33), (13, 33)));
}

#[test]
fn rollup_root_last_in_input_with_parent_ref_to_descendant_ignores_that_ref() {
    let entries = vec![file(2, 1, "f", 3), dir(3, 1, "d"), file(4, 3, "g", 4), dir(1, 3, "root")];

    let r = rollup(1, entries).unwrap();

    let t = &r.tree;
    assert_eq!(t.root(), NodeId(3));
    assert_eq!(kids(t, t.root()), ["f", "d"]);
    assert_eq!((t.node(t.root()).size, r.skipped, r.orphans, r.files, r.dirs), (7, 0, 0, 2, 2));
}

#[test]
fn rollup_two_cycle_with_subtree_skips_cycle_and_all_descendants() {
    let entries = vec![
        dir(1, 1, "root"),
        file(2, 1, "ok", 3),
        dir(3, 4, "x"),
        dir(4, 3, "y"),
        dir(5, 3, "sub"),
        file(6, 5, "deep", 100),
        file(7, 4, "u", 50),
    ];

    let r = rollup(1, entries).unwrap();

    let t = &r.tree;
    assert_eq!(shape(&r).len(), 2);
    assert_eq!(kids(t, t.root()), ["ok"]);
    assert_eq!((r.skipped, r.files, r.dirs, t.node(t.root()).size), (5, 1, 1, 3));
}

#[test]
fn rollup_directory_record_size_is_ignored_and_dir_size_is_sum_of_children() {
    let mut d = dir(2, 1, "d");
    d.size = 999;
    d.allocated = 999;
    let mut empty = dir(3, 1, "empty");
    empty.size = 50;
    empty.allocated = 60;
    let entries = vec![dir(1, 1, "root"), d, file(4, 2, "f", 5), empty];

    let r = rollup(1, entries).unwrap();

    let t = &r.tree;
    let sz = |n: &str| (t.node(find(t, n)).size, t.node(find(t, n)).allocated);
    assert_eq!((sz("d"), sz("empty"), sz("root")), ((5, 15), (0, 0), (5, 15)));
}

fn sparse_input(map: impl Fn(u64) -> u64) -> Vec<RawEntry> {
    let mut v = vec![
        dir(1, 1, "root"),
        dir(2, 1, "d"),
        file(3, 2, "f", 10),
        file(4, 1000, "high-parent", 4),
        file(5, 2, "g", 1),
    ];
    for e in &mut v {
        e.id = map(e.id);
        e.parent = map(e.parent);
    }
    v
}

#[test]
fn rollup_ids_above_u32_with_gaps_and_parent_above_every_id_equals_compact_ids() {
    let compact = rollup(1, sparse_input(|x| x)).unwrap();

    let sparse = rollup(7 + (1 << 33), sparse_input(|x| x * (1 << 33) + 7)).unwrap();

    assert_eq!(shape(&sparse), shape(&compact));
    assert_eq!((sparse.orphans, sparse.skipped), (1, 0));
    assert_eq!(kids(&sparse.tree, find(&sparse.tree, "$Orphan")), ["high-parent"]);
}

#[test]
fn rollup_root_id_above_max_id_returns_no_root() {
    let entries = || vec![dir(1, 1, "root"), file(2, 1, "f", 1), file(3, 1, "g", 1)];

    let above = rollup(100, entries()).unwrap_err();
    let max = rollup(u64::MAX, entries()).unwrap_err();

    assert!(matches!(above, ScanError::NoRoot) && matches!(max, ScanError::NoRoot));
}

#[test]
fn rollup_empty_input_returns_no_root() {
    let err = rollup(1, Vec::new()).unwrap_err();

    assert!(matches!(err, ScanError::NoRoot));
}

#[test]
fn rollup_orphan_directory_children_hang_under_it_and_sizes_roll_through_orphan() {
    let entries = vec![dir(1, 1, "root"), dir(2, 999, "od"), file(3, 2, "f", 8), file(4, 2, "g", 2)];

    let r = rollup(1, entries).unwrap();

    let t = &r.tree;
    let orphan = find(t, "$Orphan");
    assert_eq!(kids(t, orphan), ["od"]);
    assert_eq!(kids(t, find(t, "od")), ["f", "g"]);
    assert_eq!((t.node(orphan).size, t.node(t.root()).size, r.orphans, r.dirs), (10, 10, 1, 2));
}

#[test]
fn rollup_node_ids_follow_input_order_of_reachable_entries_without_gaps() {
    let entries = vec![
        dir(1, 1, "root"),
        file(4, 3, "x", 1),
        dir(3, 1, "d"),
        dir(8, 8, "selfloop"),
        file(5, 1, "y", 1),
    ];

    let r = rollup(1, entries).unwrap();

    let t = &r.tree;
    let names: Vec<&str> = (0..4).map(|i| t.name(NodeId(i))).collect();
    assert_eq!(names, ["root", "x", "d", "y"]);
    assert_eq!((kids(t, NodeId(2)), kids(t, t.root()), r.skipped), (vec!["x"], vec!["d", "y"], 1));
    assert_eq!(shape(&r).len(), 4);
}

// ---- parity with the 81181b2 oracle on seeded forests ----

const NAME_POOL: [&str; 6] = ["a", "", "\u{e9}t\u{e9}", "\u{65e5}\u{672c}\u{8a9e}", "x y", "$Orphan"];

/// A forest rooted at id 1 (returned). Directories nest up to 40 deep; entries are permuted so
/// parents may follow their children; the root lands at a random position. With `weird`, adds
/// parents that are files, self-parents, missing parents, 2- and 3-cycles (with their
/// subtrees), duplicate ids (including of directories with children) and a random root parent.
fn forest(seed: u64, n: usize, weird: bool) -> (u64, Vec<RawEntry>) {
    let mut rng = Rng::new(seed);
    let mut all = vec![dir(1, 1, "root")];
    let mut open_dirs: Vec<(u64, u32)> = vec![(1, 0)];
    for k in 1..n {
        let (parent, depth) = open_dirs[rng.below(open_dirs.len())];
        let id = k as u64 + 1;
        let base = NAME_POOL[rng.below(NAME_POOL.len())];
        let name = if rng.chance(20) { base.to_string() } else { format!("{base}{k}") };
        let is_dir = rng.chance(35);
        let size = rng.below(10_000) as u64;
        let (size, allocated) = if is_dir && rng.chance(50) { (0, 0) } else { (size, size + rng.below(4096) as u64) };
        let modified = if rng.chance(30) { scan::time::UNKNOWN } else { rng.below(1_000_000) as u32 + 1 };
        all.push(RawEntry { id, parent, name, size, allocated, is_dir, modified });
        if is_dir && depth < 40 {
            open_dirs.push((id, depth + 1));
        }
    }
    let mut rest = all.split_off(1);
    for i in (1..rest.len()).rev() {
        rest.swap(i, rng.below(i + 1));
    }
    if weird && n >= 4 {
        for _ in 0..1 + n / 20 {
            let (a, b, c) = (rng.below(rest.len()), rng.below(rest.len()), rng.below(rest.len()));
            match rng.below(6) {
                0 => rest[a].parent = rest[b].id,
                1 => rest[a].parent = rest[a].id,
                2 => rest[a].parent = (1 << 20) + a as u64,
                3 => {
                    (rest[a].parent, rest[b].parent) = (rest[b].id, rest[a].id);
                    (rest[a].is_dir, rest[b].is_dir) = (true, true);
                }
                4 => {
                    (rest[a].parent, rest[b].parent, rest[c].parent) = (rest[b].id, rest[c].id, rest[a].id);
                    (rest[a].is_dir, rest[b].is_dir, rest[c].is_dir) = (true, true, true);
                }
                _ => rest[a].id = rest[b].id,
            }
        }
    }
    let mut root = all.pop().expect("root");
    if weird && !rest.is_empty() && rng.chance(50) {
        root.parent = rest[rng.below(rest.len())].id;
    }
    let at = if rng.chance(50) { 0 } else { rng.below(rest.len() + 1) };
    rest.insert(at, root);
    (1, rest)
}

/// Maps every id and parent through `f`.
fn relabel(entries: &[RawEntry], f: impl Fn(u64) -> u64) -> Vec<RawEntry> {
    entries.iter().map(|e| RawEntry { id: f(e.id), parent: f(e.parent), ..e.clone() }).collect()
}

fn assert_matches_reference(root: u64, entries: Vec<RawEntry>) {
    let want = rollup_reference(root, entries.clone());
    match (rollup(root, entries), want) {
        (Ok(got), Some(want)) => assert_same_tree(&got, &want),
        (Err(ScanError::NoRoot), None) => {}
        (got, want) => panic!("outcome differs: got ok={}, reference some={}", got.is_ok(), want.is_some()),
    }
}

fn size_for(seed: u64) -> usize {
    1 + (seed as usize * 2477) % 5000
}

#[test]
fn rollup_seeded_sane_forests_equal_the_reference() {
    for seed in 1..=200 {
        let (root, entries) = forest(seed, size_for(seed), false);

        assert_matches_reference(root, entries);
    }
}

#[test]
fn rollup_seeded_weird_forests_equal_the_reference() {
    for seed in 1..=200 {
        let (root, entries) = forest(1000 + seed, size_for(seed), true);

        assert_matches_reference(root, entries);
    }
}

#[test]
fn rollup_seeded_forests_with_sparse_ids_equal_the_reference() {
    for seed in 1..=60 {
        let (root, entries) = forest(2000 + seed, size_for(seed).max(8), seed % 2 == 0);

        let sparse = relabel(&entries, |x| x * (1 << 33) + 7);

        assert_matches_reference(root * (1 << 33) + 7, sparse);
    }
}

#[test]
fn rollup_seeded_forests_at_the_dense_index_boundary_equal_the_reference() {
    for seed in 1..=40 {
        let (root, entries) = forest(3000 + seed, size_for(seed).max(8), seed % 2 == 0);
        let n = entries.len() as u64;
        let max = entries.iter().map(|e| e.id).max().expect("non-empty");

        for target in [8 * n + 1023, 8 * n + 1024] {
            assert_matches_reference(root, relabel(&entries, |x| if x == max { target } else { x }));
        }
    }
}

#[test]
fn rollup_150k_entry_forest_equals_the_reference() {
    let (root, entries) = forest(77, 150_000, false);

    assert_matches_reference(root, entries);
}

#[test]
fn rollup_10k_deep_chain_equals_the_reference() {
    let n = 10_000u64;
    let entries: Vec<RawEntry> = (1..=n)
        .map(|id| if id < n { dir(id, id.max(2) - 1, &format!("c{id}")) } else { file(id, id - 1, "leaf", 9) })
        .collect();

    assert_matches_reference(1, entries);
}

#[test]
fn rollup_single_entry_input_equals_the_reference() {
    assert_matches_reference(1, vec![dir(1, 1, "root")]);
}

#[test]
fn rollup_root_with_only_orphans_equals_the_reference() {
    assert_matches_reference(1, vec![file(2, 50, "o1", 1), dir(1, 7, "root"), dir(3, 51, "o2")]);
}

#[test]
fn rollup_weird_forest_is_identical_in_pools_of_1_4_and_8_threads() {
    let (root, entries) = forest(4242, 3000, true);
    let want = rollup_reference(root, entries.clone()).expect("root present");

    for threads in [1, 4, 8] {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();

        let got = pool.install(|| rollup(root, entries.clone())).unwrap();

        assert_same_tree(&got, &want);
    }
}

// ---- entry times: "newest inside" ----

fn file_at(id: u64, parent: u64, name: &str, modified: u32) -> RawEntry {
    RawEntry { modified, ..file(id, parent, name, 1) }
}

fn dir_at(id: u64, parent: u64, name: &str, modified: u32) -> RawEntry {
    RawEntry { modified, ..dir(id, parent, name) }
}

fn modified_of(t: &scan::Tree, parent: scan::NodeId, name: &str) -> u32 {
    let n = t.children(parent).find(|&n| t.name(n) == name).unwrap();
    t.node(n).modified
}

#[test]
fn rollup_folder_modified_is_the_max_of_its_files_at_three_levels() {
    let entries = vec![
        dir(1, 1, "root"),
        dir(2, 1, "a"),
        dir(3, 2, "b"),
        file_at(4, 3, "deep", 70),
        file_at(5, 3, "deep2", 20),
        file_at(6, 2, "mid", 40),
        file_at(7, 1, "top", 10),
    ];

    let t = rollup(1, entries).unwrap().tree;

    let (root, a) = (t.root(), t.children(t.root()).find(|&n| t.name(n) == "a").unwrap());
    assert_eq!(modified_of(&t, a, "b"), 70);
    assert_eq!(t.node(a).modified, 70);
    assert_eq!(t.node(root).modified, 70);
    assert_eq!(modified_of(&t, root, "top"), 10);
}

#[test]
fn rollup_folder_modified_ignores_the_directory_records_own_value() {
    let entries = vec![
        dir_at(1, 1, "root", 500),
        dir_at(2, 1, "folder", 99),
        file_at(3, 2, "a", 10),
        file_at(4, 2, "b", 30),
        file_at(5, 2, "c", 20),
    ];

    let t = rollup(1, entries).unwrap().tree;

    let folder = t.children(t.root()).next().unwrap();
    assert_eq!(t.node(folder).modified, 30);
    assert_eq!(t.node(t.root()).modified, 30);
}

#[test]
fn rollup_empty_folder_and_all_unknown_files_are_unknown() {
    let entries = vec![
        dir_at(1, 1, "root", 5),
        dir_at(2, 1, "empty", 99),
        dir(3, 1, "unknown"),
        file_at(4, 3, "x", scan::time::UNKNOWN),
        file_at(5, 3, "y", scan::time::UNKNOWN),
    ];

    let t = rollup(1, entries).unwrap().tree;

    assert_eq!(modified_of(&t, t.root(), "empty"), scan::time::UNKNOWN);
    assert_eq!(modified_of(&t, t.root(), "unknown"), scan::time::UNKNOWN);
    assert_eq!(t.node(t.root()).modified, scan::time::UNKNOWN);
}

#[test]
fn rollup_known_and_unknown_files_mixed_give_the_known_max() {
    let entries = vec![
        dir(1, 1, "root"),
        file_at(2, 1, "a", scan::time::UNKNOWN),
        file_at(3, 1, "b", 8),
        file_at(4, 1, "c", scan::time::UNKNOWN),
        file_at(5, 1, "d", 3),
    ];

    let t = rollup(1, entries).unwrap().tree;

    assert_eq!(t.node(t.root()).modified, 8);
}

#[test]
fn rollup_orphan_node_gets_the_orphans_max_and_the_root_includes_it() {
    let entries = vec![
        dir(1, 1, "root"),
        file_at(2, 1, "kept", 5),
        file_at(3, 77, "lost1", 40),
        file_at(4, 78, "lost2", 90),
        file_at(5, 79, "lost3", 60),
    ];

    let t = rollup(1, entries).unwrap().tree;

    assert_eq!(modified_of(&t, t.root(), "$Orphan"), 90);
    assert_eq!(t.node(t.root()).modified, 90);
}

#[test]
fn node_size_stays_within_the_8_byte_budget() {
    // The design limit is +8 bytes per node (to 64). The value is 56 because the new `modified`
    // u32 fits the padding that was already there: pin it, so a new field shows here at once.
    assert_eq!(std::mem::size_of::<scan::Node>(), 56, "node size changed: the budget is +8 bytes per node, and `modified` fits the old padding");
}
