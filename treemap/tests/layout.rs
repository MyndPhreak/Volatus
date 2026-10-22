use scan::rollup::{RawEntry, rollup};
use scan::{NodeId, Tree};
use treemap::{Rect, layout, render};

fn tree_of(sizes: &[u64]) -> Tree {
    let mut entries = vec![RawEntry { id: 0, parent: 0, name: "root".into(), size: 0, allocated: 0, is_dir: true }];
    for (i, &s) in sizes.iter().enumerate() {
        let id = i as u64 + 1;
        entries.push(RawEntry { id, parent: 0, name: format!("f{id}"), size: s, allocated: s, is_dir: false });
    }
    rollup(0, entries).unwrap().tree
}

const W: u32 = 800;
const H: u32 = 600;

fn assert_inside(r: &Rect) {
    let eps = 1e-6;
    assert!(r.x >= -eps && r.y >= -eps, "{r:?}");
    assert!(r.x + r.w <= W as f64 + eps && r.y + r.h <= H as f64 + eps, "{r:?}");
}

#[test]
fn layout_areas_are_proportional_to_size() {
    let sizes = [500u64, 300, 120, 50, 20, 7, 3];
    let tree = tree_of(&sizes);
    let total: u64 = sizes.iter().sum();

    let rects = layout(&tree, tree.root(), W, H);

    assert_eq!(rects.len(), sizes.len());
    for r in &rects {
        let size = tree.node(r.node).size as f64;
        let expected = size / total as f64 * (W * H) as f64;
        assert!((r.w * r.h - expected).abs() <= 1.0, "{r:?} expected area {expected}");
    }
}

#[test]
fn layout_rects_do_not_overlap_and_stay_in_bounds() {
    let tree = tree_of(&[90, 80, 70, 60, 50, 40, 30, 20, 10, 5, 1]);

    let rects = layout(&tree, tree.root(), W, H);

    assert!(!rects.is_empty());
    let eps = 1e-6;
    for (i, a) in rects.iter().enumerate() {
        assert_inside(a);
        for b in &rects[i + 1..] {
            let ox = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
            let oy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
            assert!(ox <= eps || oy <= eps, "overlap {a:?} {b:?}");
        }
    }
}

#[test]
fn layout_total_area_fills_bounds() {
    let tree = tree_of(&[9, 5, 3, 1]);

    let rects = layout(&tree, tree.root(), W, H);

    let area: f64 = rects.iter().map(|r| r.w * r.h).sum();
    assert!((area - (W * H) as f64).abs() <= 1.0, "area {area}");
}

#[test]
fn layout_zero_size_nodes_get_no_rects() {
    let tree = tree_of(&[0, 10, 0]);

    let rects = layout(&tree, tree.root(), W, H);

    assert_eq!(rects.len(), 1);
    assert_eq!(tree.node(rects[0].node).size, 10);
}

#[test]
fn layout_all_zero_returns_empty() {
    let tree = tree_of(&[0, 0]);

    assert!(layout(&tree, tree.root(), W, H).is_empty());
}

#[test]
fn layout_empty_dir_returns_empty() {
    let tree = tree_of(&[]);

    assert!(layout(&tree, tree.root(), W, H).is_empty());
}

#[test]
fn layout_single_node_fills_bounds() {
    let tree = tree_of(&[42]);

    let rects = layout(&tree, tree.root(), W, H);

    assert_eq!(rects.len(), 1);
    let r = rects[0];
    assert_eq!((r.x, r.y, r.w, r.h), (0.0, 0.0, W as f64, H as f64));
}

#[test]
fn layout_zero_area_bounds_returns_empty() {
    let tree = tree_of(&[1, 2]);

    assert!(layout(&tree, tree.root(), 0, H).is_empty());
    assert!(layout(&tree, tree.root(), W, 0).is_empty());
}

#[test]
fn layout_same_input_gives_same_output() {
    let tree = tree_of(&[5, 5, 5, 3, 3, 1, 1, 1]);

    let a = layout(&tree, tree.root(), W, H);
    let b = layout(&tree, tree.root(), W, H);

    assert!(!a.is_empty());
    assert_eq!(a, b);
}

#[test]
fn layout_equal_sizes_order_by_node_id() {
    let tree = tree_of(&[5, 5, 5]);

    let ids: Vec<NodeId> = layout(&tree, tree.root(), W, H).iter().map(|r| r.node).collect();

    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids.len(), 3);
    assert_eq!(ids, sorted);
}

// ---- nested layout ----

/// Tree from `(id, parent, size, is_dir)` rows under a root folder with id 0. Node names are `n{id}`.
fn nested(rows: &[(u64, u64, u64, bool)]) -> Tree {
    let mut entries = vec![RawEntry { id: 0, parent: 0, name: "root".into(), size: 0, allocated: 0, is_dir: true }];
    entries.extend(rows.iter().map(|&(id, parent, size, is_dir)| RawEntry {
        id,
        parent,
        name: format!("n{id}"),
        size,
        allocated: size,
        is_dir,
    }));
    rollup(0, entries).unwrap().tree
}

fn named(tree: &Tree, name: &str) -> NodeId {
    let mut stack = vec![tree.root()];
    while let Some(n) = stack.pop() {
        if tree.name(n) == name {
            return n;
        }
        stack.extend(tree.children(n));
    }
    panic!("no node {name}");
}

fn is_ancestor(tree: &Tree, anc: NodeId, mut n: NodeId) -> bool {
    while let Some(p) = tree.node(n).parent {
        if p == anc {
            return true;
        }
        n = p;
    }
    false
}

fn rect_of(rects: &[Rect], n: NodeId) -> Option<Rect> {
    rects.iter().find(|r| r.node == n).copied()
}

// root
//   n1 (dir)  n11=300  n12=200  n13 (dir){ n131=100  n132=50 }
//   n2 (dir)  n21=400
//   n3=120
//   n4 (dir)  n41=30  n42=20
fn sample() -> Tree {
    nested(&[
        (1, 0, 0, true),
        (11, 1, 300, false),
        (12, 1, 200, false),
        (13, 1, 0, true),
        (131, 13, 100, false),
        (132, 13, 50, false),
        (2, 0, 0, true),
        (21, 2, 400, false),
        (3, 0, 120, false),
        (4, 0, 0, true),
        (41, 4, 30, false),
        (42, 4, 20, false),
    ])
}

#[test]
fn layout_nested_every_node_gets_a_rect_with_area_proportional_to_size() {
    let tree = sample();
    let total = tree.node(tree.root()).size as f64;

    let rects = layout(&tree, tree.root(), W, H);

    // 12 nodes below the root, none small enough to prune.
    assert_eq!(rects.len(), 12, "{rects:?}");
    for r in &rects {
        let expected = tree.node(r.node).size as f64 / total * (W * H) as f64;
        assert!((r.w * r.h - expected).abs() <= 1.0, "{} {r:?} expected area {expected}", tree.name(r.node));
    }
}

#[test]
fn layout_nested_child_rects_lie_inside_their_parent_rect() {
    let tree = sample();
    let eps = 1e-6;

    let rects = layout(&tree, tree.root(), W, H);

    for c in &rects {
        assert_inside(c);
        let parent = tree.node(c.node).parent.unwrap();
        if parent == tree.root() {
            continue;
        }
        let p = rect_of(&rects, parent).unwrap_or_else(|| panic!("folder {} has no rect", tree.name(parent)));
        assert!(
            c.x >= p.x - eps && c.y >= p.y - eps && c.x + c.w <= p.x + p.w + eps && c.y + c.h <= p.y + p.h + eps,
            "{} {c:?} is outside its parent {p:?}",
            tree.name(c.node)
        );
    }
}

#[test]
fn layout_nested_rects_overlap_only_their_ancestors() {
    let tree = sample();
    let eps = 1e-6;

    let rects = layout(&tree, tree.root(), W, H);

    assert!(rect_of(&rects, named(&tree, "n1")).is_some(), "folder n1 has no rect: {rects:?}");
    for (i, a) in rects.iter().enumerate() {
        for b in &rects[i + 1..] {
            let ox = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
            let oy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
            let related = is_ancestor(&tree, a.node, b.node) || is_ancestor(&tree, b.node, a.node);
            assert!(related || ox <= eps || oy <= eps, "overlap {a:?} {b:?}");
        }
    }
}

#[test]
fn layout_nested_lists_each_folder_before_its_descendants() {
    let tree = sample();

    let rects = layout(&tree, tree.root(), W, H);

    let mut pairs = 0;
    for (j, d) in rects.iter().enumerate() {
        for (i, a) in rects.iter().enumerate() {
            if is_ancestor(&tree, a.node, d.node) {
                pairs += 1;
                assert!(i < j, "{} (index {i}) comes after its descendant {} (index {j})", tree.name(a.node), tree.name(d.node));
            }
        }
    }
    // n1>n11, n1>n12, n1>n13, n1>n131, n1>n132, n13>n131, n13>n132, n2>n21, n4>n41, n4>n42
    assert_eq!(pairs, 10);
}

/// root{ n1{ n11=1_000_000, 1000 files of 1 byte }, n2=1_000_000 }: the 1-byte files are far under 1 px.
fn tiny_files() -> Tree {
    let mut rows = vec![(1, 0, 0, true), (11, 1, 1_000_000, false), (2, 0, 1_000_000, false)];
    rows.extend((0..1000).map(|i| (100 + i, 1, 1, false)));
    nested(&rows)
}

#[test]
fn layout_prunes_rects_smaller_than_one_pixel() {
    let tree = tiny_files();

    let rects = layout(&tree, tree.root(), 100, 100);

    let small: Vec<&Rect> = rects.iter().filter(|r| r.w < 1.0 || r.h < 1.0).collect();
    assert!(small.is_empty(), "{} rects under 1 px, first {:?}", small.len(), small.first());
}

#[test]
fn layout_prunes_a_sliver_of_one_square_pixel_or_more() {
    // 10 px² passes the 1 px² pre-filter, but the squarified row makes it 0.1 px wide.
    let tree = tree_of(&[9990, 10]);

    let rects = layout(&tree, tree.root(), 100, 100);

    assert_eq!(rects.len(), 1, "{rects:?}");
    assert_eq!(tree.node(rects[0].node).size, 9990);
}

#[test]
fn layout_pruned_children_leave_their_area_in_the_parent() {
    let tree = tiny_files();
    let n1 = named(&tree, "n1");
    let total = tree.node(tree.root()).size as f64;

    let rects = layout(&tree, tree.root(), 100, 100);

    let r = rect_of(&rects, n1).expect("folder n1 has a rect");
    let expected = tree.node(n1).size as f64 / total * 10_000.0;
    assert!((r.w * r.h - expected).abs() <= 1.0, "{r:?} expected area {expected}");
}

/// 100 folders x 100 folders x 100 files: 1,000,000 files, sizes from a fixed xorshift sequence.
fn million_files() -> Tree {
    let mut rows = Vec::with_capacity(1_010_100);
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next_id = 1u64;
    for _ in 0..100 {
        let top = next_id;
        next_id += 1;
        rows.push((top, 0, 0, true));
        for _ in 0..100 {
            let mid = next_id;
            next_id += 1;
            rows.push((mid, top, 0, true));
            for _ in 0..100 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                // 1 B to about 1 GB, spread over many orders of magnitude like real files.
                let size = 1 + (x % 1024) * (1u64 << ((x >> 58) % 21));
                rows.push((next_id, mid, size, false));
                next_id += 1;
            }
        }
    }
    nested(&rows)
}

#[test]
fn layout_million_files_rect_count_is_bounded_by_pixel_count() {
    let tree = million_files();

    let started = std::time::Instant::now();
    let rects = layout(&tree, tree.root(), W, H);
    eprintln!("layout of 1,010,100 nodes at {W}x{H}: {:?}, {} rects", started.elapsed(), rects.len());

    assert!(rects.len() <= (W * H) as usize, "{} rects for {} pixels", rects.len(), W * H);
}

#[test]
fn layout_nested_equal_size_folders_are_ordered_by_node_id_every_run() {
    let tree = nested(&[(1, 0, 0, true), (11, 1, 5, false), (2, 0, 0, true), (21, 2, 5, false), (3, 0, 0, true), (31, 3, 5, false)]);
    let want = vec![named(&tree, "n1"), named(&tree, "n2"), named(&tree, "n3")];

    let a = layout(&tree, tree.root(), W, H);
    let b = layout(&tree, tree.root(), W, H);

    let folders: Vec<NodeId> = a.iter().map(|r| r.node).filter(|&n| tree.node(n).is_dir).collect();
    assert_eq!(folders, want);
    assert_eq!(a, b);
}

#[test]
fn layout_mixed_sizes_order_equal_sizes_by_node_id() {
    // 200 files of 3 sizes in an interleaved order: more than the small-sort cut-off, and not
    // presorted, so an unstable sort on size alone moves equal sizes out of id order.
    let sizes: Vec<u64> = (0..200u64).map(|i| 1000 * (1 + (i * 7) % 3)).collect();
    let tree = tree_of(&sizes);

    let rects = layout(&tree, tree.root(), W, H);

    assert_eq!(rects.len(), sizes.len());
    let order: Vec<(u64, NodeId)> = rects.iter().map(|r| (tree.node(r.node).size, r.node)).collect();
    for pair in order.windows(2) {
        let ((sa, a), (sb, b)) = (pair[0], pair[1]);
        assert!(sa > sb || (sa == sb && a < b), "{a:?} (size {sa}) is placed before {b:?} (size {sb})");
    }
}

#[test]
fn render_returns_rgba_buffer_with_rect_pixels_opaque() {
    let tree = tree_of(&[3, 1]);
    let rects = layout(&tree, tree.root(), 40, 30);

    let px = render(&rects, 40, 30);

    assert_eq!(px.len(), 40 * 30 * 4);
    let r = rects[0];
    let (cx, cy) = ((r.x + r.w / 2.0) as usize, (r.y + r.h / 2.0) as usize);
    assert_eq!(px[(cy * 40 + cx) * 4 + 3], 255);
}

#[test]
fn render_no_rects_is_fully_transparent() {
    let px = render(&[], 4, 4);

    assert_eq!(px, vec![0u8; 64]);
}
