//! TP2.2: one group's split into folder bands, the "files" band and "other (N)".

use flow::{MERGE_PERCENT, Part, Split, members, split};
use scan::rollup::{RawEntry, rollup};
use scan::{NodeId, Tree};

/// Builds a tree with `rollup`. Every entry is reachable, so a raw id `i` (1-based, in input
/// order) is `NodeId(i - 1)` (`rollup`: "reachable entries in input order").
struct Build {
    entries: Vec<RawEntry>,
}

impl Build {
    /// The root folder is raw id 1.
    fn new() -> Self {
        let mut b = Build { entries: Vec::new() };
        b.push(1, "root", 0, true);
        b
    }

    fn push(&mut self, parent: u64, name: &str, size: u64, is_dir: bool) -> u64 {
        let id = self.entries.len() as u64 + 1;
        self.entries.push(RawEntry {
            id,
            parent,
            name: name.into(),
            size,
            allocated: size,
            is_dir,
            modified: scan::time::UNKNOWN,
        });
        id
    }

    fn dir(&mut self, parent: u64, name: &str) -> u64 {
        self.push(parent, name, 0, true)
    }

    fn file(&mut self, parent: u64, name: &str, size: u64) -> u64 {
        self.push(parent, name, size, false)
    }

    /// A folder holding one file of `size` (none when `size == 0`), so its rolled-up size is `size`.
    fn folder(&mut self, parent: u64, name: &str, size: u64) -> u64 {
        let d = self.dir(parent, name);
        if size > 0 {
            self.file(d, "f", size);
        }
        d
    }

    fn build(self) -> Tree {
        let n = self.entries.len();
        let r = rollup(1, self.entries).unwrap();
        assert_eq!((r.orphans, r.skipped), (0, 0));
        assert_eq!(r.files + r.dirs, n as u64);
        r.tree
    }
}

fn nid(raw: u64) -> NodeId {
    NodeId((raw - 1) as u32)
}

const ROOT: u64 = 1;

/// The TP2.2 fixture: P = 1000: A 600, B 300, C 40, D 25, E 0, files 20 + 15.
/// Children are given out of size order on purpose.
struct Fixture {
    tree: Tree,
    p: NodeId,
    a: NodeId,
    b: NodeId,
    c: NodeId,
    d: NodeId,
    e: NodeId,
    f20: NodeId,
    f15: NodeId,
}

fn fixture() -> Fixture {
    let mut b = Build::new();
    let c = b.folder(ROOT, "C", 40);
    let f20 = b.file(ROOT, "f20", 20);
    let a = b.folder(ROOT, "A", 600);
    let e = b.folder(ROOT, "E", 0);
    let d = b.folder(ROOT, "D", 25);
    let f15 = b.file(ROOT, "f15", 15);
    let bb = b.folder(ROOT, "B", 300);
    let tree = b.build();
    assert_eq!(tree.node(nid(ROOT)).size, 1000);
    Fixture {
        tree,
        p: nid(ROOT),
        a: nid(a),
        b: nid(bb),
        c: nid(c),
        d: nid(d),
        e: nid(e),
        f20: nid(f20),
        f15: nid(f15),
    }
}

/// A root holding one folder per size, in the given order; returns the tree and the folder ids.
fn folders_of(sizes: &[u64]) -> (Tree, Vec<NodeId>) {
    let mut b = Build::new();
    let ids: Vec<u64> = sizes.iter().enumerate().map(|(i, &s)| b.folder(ROOT, &format!("d{i}"), s)).collect();
    (b.build(), ids.into_iter().map(nid).collect())
}

#[test]
fn split_order_is_folders_by_size_then_files_then_other() {
    let f = fixture();

    let s = split(&f.tree, f.p, 1.0, 6.0, false);

    assert_eq!(s.folders, [f.a, f.b, f.c]);
    assert_eq!(s.files, Some(35));
    assert_eq!(s.other, Some((1, false, 25)));
}

#[test]
fn split_zero_size_child_is_in_no_band_and_not_counted() {
    let f = fixture();

    let s = split(&f.tree, f.p, 1.0, 6.0, false);
    let merged = members(&f.tree, &Part::Other { parent: f.p, folders: 1, files: false });

    assert_eq!(f.tree.node(f.e).size, 0);
    assert!(!s.folders.contains(&f.e));
    assert_eq!(s.other.map(|o| o.0), Some(1));
    assert_eq!(merged, [f.d]);
}

#[test]
fn split_share_rule_alone_shows_3_percent_and_merges_below() {
    // Parent 10,000; at k = 1.0 every child is far above 6 px, so only the share rule acts.
    let (t, ids) = folders_of(&[9401, 300, 299]);
    assert_eq!(t.node(nid(ROOT)).size, 10_000);

    let s = split(&t, nid(ROOT), 1.0, 6.0, false);

    assert_eq!(s.folders, [ids[0], ids[1]]);
    assert_eq!(s.files, None);
    assert_eq!(s.other, Some((1, false, 299)));
}

#[test]
fn split_pixel_rule_alone_shows_6_px_and_merges_5_px() {
    // Parent 100: 5 % and 6 % are above the share line, so only the pixel rule acts.
    let (t, ids) = folders_of(&[89, 5, 6]);
    assert_eq!(t.node(nid(ROOT)).size, 100);

    let s = split(&t, nid(ROOT), 1.0, 6.0, false);

    assert_eq!(s.folders, [ids[0], ids[2]]);
    assert_eq!(s.files, None);
    assert_eq!(s.other, Some((1, false, 5)));
}

#[test]
fn split_keep_small_drops_the_share_rule_but_keeps_the_pixel_rule() {
    let (t, ids) = folders_of(&[9696, 299, 5]);
    assert_eq!(t.node(nid(ROOT)).size, 10_000);

    let s = split(&t, nid(ROOT), 1.0, 6.0, true);

    assert_eq!(s.folders, [ids[0], ids[1]]);
    assert_eq!(s.files, None);
    assert_eq!(s.other, Some((1, false, 5)));
}

#[test]
fn split_files_band_under_the_rule_merges_into_other() {
    let mut b = Build::new();
    let big = b.folder(ROOT, "big", 990);
    let f1 = b.file(ROOT, "f1", 6);
    let f2 = b.file(ROOT, "f2", 4);
    let t = b.build();

    let s = split(&t, nid(ROOT), 1.0, 6.0, false);
    let merged = members(&t, &Part::Other { parent: nid(ROOT), folders: 0, files: true });

    assert_eq!(s.folders, [nid(big)]);
    assert_eq!(s.files, None);
    assert_eq!(s.other, Some((0, true, 10)));
    assert_eq!(merged, [nid(f1), nid(f2)]);
}

#[test]
fn split_all_children_merged_is_only_other() {
    let mut b = Build::new();
    b.folder(ROOT, "x", 1000);
    b.folder(ROOT, "y", 2000);
    b.file(ROOT, "f", 500);
    let t = b.build();

    // k = 0.001: 2000 bytes is 2 px.
    let s = split(&t, nid(ROOT), 0.001, 6.0, false);

    assert!(s.folders.is_empty());
    assert_eq!(s.files, None);
    assert_eq!(s.other, Some((2, true, 3500)));
}

#[test]
fn split_one_child_of_100_percent_is_one_folder_only() {
    let (t, ids) = folders_of(&[500]);

    let s = split(&t, nid(ROOT), 1.0, 6.0, false);

    assert_eq!(s.folders, [ids[0]]);
    assert_eq!(s.files, None);
    assert_eq!(s.other, None);
}

#[test]
fn split_only_files_is_one_files_band() {
    let mut b = Build::new();
    b.file(ROOT, "a", 10);
    b.file(ROOT, "b", 20);
    let t = b.build();

    let s = split(&t, nid(ROOT), 1.0, 6.0, false);

    assert!(s.folders.is_empty());
    assert_eq!(s.files, Some(30));
    assert_eq!(s.other, None);
}

#[test]
fn split_equal_sizes_order_by_node_id() {
    let (t, ids) = folders_of(&[100, 500, 100, 100]);

    let s = split(&t, nid(ROOT), 1.0, 6.0, false);

    assert_eq!(s.folders, [ids[1], ids[0], ids[2], ids[3]]);
}

#[test]
fn members_of_other_is_exactly_the_merged_folders_then_the_direct_files() {
    let f = fixture();

    // k = 0.1: A 60 px, B 30 px; C 4 px, D 2.5 px and files 3.5 px merge.
    let s = split(&f.tree, f.p, 0.1, 6.0, false);
    let merged = members(&f.tree, &Part::Other { parent: f.p, folders: 2, files: true });

    assert_eq!(s.folders, [f.a, f.b]);
    assert_eq!(s.files, None);
    assert_eq!(s.other, Some((2, true, 100)));
    assert_eq!(merged, [f.c, f.d, f.f20, f.f15]);
}

#[test]
fn members_of_folder_and_files_parts() {
    let f = fixture();

    assert_eq!(members(&f.tree, &Part::Folder(f.a)), [f.a]);
    assert_eq!(members(&f.tree, &Part::Files(f.p)), [f.f20, f.f15]);
}

#[test]
fn split_boundary_at_exactly_3_percent_uses_integer_comparison() {
    // 300 of 10,000 is exactly 3 %: shown. 299 is below: merged.
    let (t, ids) = folders_of(&[9401, 300, 299]);
    let s = split(&t, nid(ROOT), 1.0, 0.0, false);
    assert_eq!(s.folders, [ids[0], ids[1]]);
    assert_eq!(s.other, Some((1, false, 299)));

    // Odd parent 9,999: 300 is 3.0003 % (shown), 299 is 2.990 % (merged).
    let (t, ids) = folders_of(&[9400, 300, 299]);
    assert_eq!(t.node(nid(ROOT)).size, 9_999);
    let s = split(&t, nid(ROOT), 1.0, 0.0, false);
    assert_eq!(s.folders, [ids[0], ids[1]]);
    assert_eq!(s.other, Some((1, false, 299)));
}

#[test]
fn split_200k_tiny_siblings_is_one_other_band() {
    const N: usize = 200_000;
    let mut b = Build::new();
    let big = b.folder(ROOT, "big", 10_000_000);
    for i in 0..N {
        b.folder(ROOT, &format!("t{i}"), 1);
    }
    let t = b.build();
    assert_eq!(t.node(nid(ROOT)).size, 10_000_000 + N as u64);

    let start = std::time::Instant::now();
    let s = split(&t, nid(ROOT), 1.0, 6.0, false);
    let took = start.elapsed();
    let merged = members(&t, &Part::Other { parent: nid(ROOT), folders: N as u32, files: false });
    eprintln!("split over {N} siblings: {took:?}");

    assert_eq!(s.folders, [nid(big)]);
    assert_eq!(s.files, None);
    assert_eq!(s.other, Some((N as u32, false, N as u64)));
    assert_eq!(merged.len(), N);
    assert!(!merged.contains(&nid(big)));
}

// ---- Property test: `split` and `members` agree over seeded random trees. ----

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn random_tree(rng: &mut Rng) -> Tree {
    let mut b = Build::new();
    let mut dirs = vec![ROOT];
    let n = 1 + rng.below(300);
    for i in 0..n {
        let parent = dirs[rng.below(dirs.len() as u64) as usize];
        if rng.below(3) == 0 {
            dirs.push(b.dir(parent, &format!("d{i}")));
        } else {
            // Small sizes give many ties; some zeros; now and then a big file.
            let size = match rng.below(10) {
                0 => 0,
                1 => rng.below(1_000_000),
                _ => rng.below(20),
            };
            b.file(parent, &format!("f{i}"), size);
        }
    }
    b.build()
}

/// The merge rule, written out again as the oracle.
fn is_merged(size: u64, parent: u64, k: f64, min_px: f64, keep_small: bool) -> bool {
    let share = !keep_small && (size as u128) * 100 < (parent as u128) * MERGE_PERCENT as u128;
    share || (size as f64) * k < min_px
}

fn check(t: &Tree, p: NodeId, k: f64, min_px: f64, keep_small: bool) {
    let ctx = format!("parent {p:?}, k {k}, min {min_px}, keep_small {keep_small}");
    let parent_size = t.node(p).size;
    let mut sized: Vec<NodeId> = t.children(p).filter(|&c| t.node(c).is_dir && t.node(c).size > 0).collect();
    sized.sort_by(|&x, &y| t.node(y).size.cmp(&t.node(x).size).then(x.cmp(&y)));
    let direct_files: Vec<NodeId> = t.children(p).filter(|&c| !t.node(c).is_dir).collect();
    let files_total: u64 = direct_files.iter().map(|&c| t.node(c).size).sum();
    let sized_total: u64 = t.children(p).map(|c| t.node(c).size).sum();

    let s: Split = split(t, p, k, min_px, keep_small);
    let (n, files_merged, other_bytes) = s.other.unwrap_or((0, false, 0));

    // Shown folders are the prefix of the size order, merged ones the suffix.
    let shown = sized.len() - n as usize;
    assert!(n as usize <= sized.len(), "{ctx}");
    assert_eq!(s.folders, sized[..shown], "{ctx}");
    for &c in &sized[..shown] {
        assert!(!is_merged(t.node(c).size, parent_size, k, min_px, keep_small), "{ctx}: {c:?} shown");
    }
    for &c in &sized[shown..] {
        assert!(is_merged(t.node(c).size, parent_size, k, min_px, keep_small), "{ctx}: {c:?} merged");
    }

    // Files: one band, or merged, or nothing when there are no file bytes.
    if files_total == 0 {
        assert_eq!((s.files, files_merged), (None, false), "{ctx}");
    } else {
        let merged = is_merged(files_total, parent_size, k, min_px, keep_small);
        assert_eq!(files_merged, merged, "{ctx}");
        assert_eq!(s.files, (!merged).then_some(files_total), "{ctx}");
    }
    assert_eq!(s.other.is_some(), n > 0 || files_merged, "{ctx}");

    // Sizes are conserved.
    let folder_bytes: u64 = s.folders.iter().map(|&c| t.node(c).size).sum();
    assert_eq!(folder_bytes + s.files.unwrap_or(0) + other_bytes, sized_total, "{ctx}");
    assert_eq!(sized_total, parent_size, "{ctx}");

    // `members(Other)` re-derives exactly the merged set.
    let mut expected: Vec<NodeId> = sized[shown..].to_vec();
    if files_merged {
        expected.extend(&direct_files);
    }
    let got = members(t, &Part::Other { parent: p, folders: n, files: files_merged });
    assert_eq!(got, expected, "{ctx}");
    let member_bytes: u64 = got.iter().map(|&c| t.node(c).size).sum();
    assert_eq!(member_bytes, other_bytes, "{ctx}");

    for &c in &s.folders {
        assert_eq!(members(t, &Part::Folder(c)), [c], "{ctx}");
    }
    assert_eq!(members(t, &Part::Files(p)), direct_files, "{ctx}");
}

#[test]
fn split_and_members_agree_over_random_trees() {
    const KS: [f64; 6] = [0.0, 1e-6, 1e-3, 0.05, 1.0, 10.0];
    const MINS: [f64; 3] = [0.0, 6.0, 12.0];
    for seed in 1..=60u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let t = random_tree(&mut rng);
        for p in dirs_of(&t) {
            for k in KS {
                for min_px in MINS {
                    for keep_small in [false, true] {
                        check(&t, p, k, min_px, keep_small);
                    }
                }
            }
        }
    }
}

fn dirs_of(t: &Tree) -> Vec<NodeId> {
    let mut dirs = Vec::new();
    let mut stack = vec![t.root()];
    while let Some(x) = stack.pop() {
        if t.node(x).is_dir {
            dirs.push(x);
            stack.extend(t.children(x));
        }
    }
    dirs
}
