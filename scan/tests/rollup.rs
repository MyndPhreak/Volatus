use scan::ScanError;
use scan::rollup::{RawEntry, rollup};

fn dir(id: u64, parent: u64, name: &str) -> RawEntry {
    RawEntry { id, parent, name: name.into(), size: 0, allocated: 0, is_dir: true }
}

fn file(id: u64, parent: u64, name: &str, size: u64) -> RawEntry {
    RawEntry { id, parent, name: name.into(), size, allocated: size + 10, is_dir: false }
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
