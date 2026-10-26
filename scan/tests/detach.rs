use scan::rollup::{RawEntry, rollup};
use scan::{NodeId, Tree};

fn dir(id: u64, parent: u64, name: &str) -> RawEntry {
    RawEntry { id, parent, name: name.into(), size: 0, allocated: 0, is_dir: true }
}

fn file(id: u64, parent: u64, name: &str, size: u64) -> RawEntry {
    RawEntry { id, parent, name: name.into(), size, allocated: size + 10, is_dir: false }
}

/// root { a { f1 (10), f2 (20) }, b (5), c (7) }
fn sample() -> Tree {
    let entries = vec![
        dir(1, 1, "root"),
        dir(2, 1, "a"),
        file(3, 2, "f1", 10),
        file(4, 2, "f2", 20),
        file(5, 1, "b", 5),
        file(6, 1, "c", 7),
    ];
    rollup(1, entries).unwrap().tree
}

fn find(t: &Tree, parent: NodeId, name: &str) -> NodeId {
    t.children(parent).find(|&n| t.name(n) == name).unwrap()
}

fn names(t: &Tree, parent: NodeId) -> Vec<&str> {
    t.children(parent).map(|n| t.name(n)).collect()
}

#[test]
fn detach_file_lowers_every_ancestor_and_keeps_sibling_order() {
    let mut t = sample();
    let root = t.root();
    let a = find(&t, root, "a");
    let f1 = find(&t, a, "f1");

    let d = t.detach(f1).unwrap();

    assert_eq!((d.size, d.allocated, d.files, d.dirs), (10, 20, 1, 0));
    assert_eq!((t.node(a).size, t.node(a).allocated), (20, 30));
    assert_eq!((t.node(root).size, t.node(root).allocated), (32, 30 + 15 + 17));
    assert_eq!(names(&t, root), ["a", "b", "c"]);
    assert_eq!(names(&t, a), ["f2"]);
}

#[test]
fn detach_folder_lowers_ancestors_by_rolled_up_size_and_counts_subtree() {
    let mut t = sample();
    let root = t.root();
    let a = find(&t, root, "a");

    let d = t.detach(a).unwrap();

    assert_eq!((d.size, d.allocated, d.files, d.dirs), (30, 50, 2, 1));
    assert_eq!((t.node(root).size, t.node(root).allocated), (12, 12 + 20));
    assert_eq!(names(&t, root), ["b", "c"]);
}

#[test]
fn detach_first_child_keeps_chain_intact() {
    let mut t = sample();
    let root = t.root();
    let a = find(&t, root, "a");

    t.detach(a).unwrap();

    assert_eq!(names(&t, root), ["b", "c"]);
}

#[test]
fn detach_middle_child_keeps_chain_intact() {
    let mut t = sample();
    let root = t.root();
    let b = find(&t, root, "b");

    t.detach(b).unwrap();

    assert_eq!(names(&t, root), ["a", "c"]);
}

#[test]
fn detach_last_child_keeps_chain_intact() {
    let mut t = sample();
    let root = t.root();
    let c = find(&t, root, "c");

    t.detach(c).unwrap();

    assert_eq!(names(&t, root), ["a", "b"]);
    assert_eq!(t.node(find(&t, root, "b")).next_sibling, None);
}

#[test]
fn detach_root_returns_none_and_leaves_tree_unchanged() {
    let mut t = sample();
    let root = t.root();
    let before = (t.node(root).size, t.node(root).allocated);

    assert_eq!(t.detach(root), None);

    assert_eq!((t.node(root).size, t.node(root).allocated), before);
    assert_eq!(names(&t, root), ["a", "b", "c"]);
}

#[test]
fn detached_node_is_not_reachable_from_its_parent() {
    let mut t = sample();
    let root = t.root();
    let b = find(&t, root, "b");

    t.detach(b).unwrap();

    assert!(!t.children(root).any(|n| n == b));
}

#[test]
fn detach_twice_returns_none_the_second_time() {
    let mut t = sample();
    let root = t.root();
    let b = find(&t, root, "b");

    assert!(t.detach(b).is_some());
    let size_after_first = t.node(root).size;

    assert_eq!(t.detach(b), None);
    assert_eq!(t.node(root).size, size_after_first);
}
