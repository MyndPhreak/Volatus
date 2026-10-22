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
