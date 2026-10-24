use scan::rollup::{RawEntry, rollup};
use scan::{NodeId, Tree};
use treemap::{Chrome, Rect, Theme, layout, layout_framed, render_framed};

const W: u32 = 800;
const H: u32 = 600;
const EPS: f64 = 1e-6;

/// The chrome at 100%: band 16 px, inset 2 px.
const ONE: Chrome = Chrome { header: 16.0, pad: 2.0 };

/// Files of `sizes` directly under a root folder.
fn tree_of(sizes: &[u64]) -> Tree {
    let mut entries = vec![RawEntry { id: 0, parent: 0, name: "root".into(), size: 0, allocated: 0, is_dir: true }];
    for (i, &s) in sizes.iter().enumerate() {
        let id = i as u64 + 1;
        entries.push(RawEntry { id, parent: 0, name: format!("f{id}"), size: s, allocated: s, is_dir: false });
    }
    rollup(0, entries).unwrap().tree
}

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

fn rect_of(rects: &[Rect], n: NodeId) -> Rect {
    rects.iter().find(|r| r.node == n).copied().unwrap_or_else(|| panic!("{n:?} has no rect"))
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

/// root{ n1{ n11=1_000_000, 1000 files of 1 byte }, n2=1_000_000 }: the 1-byte files are far under 1 px.
fn tiny_files() -> Tree {
    let mut rows = vec![(1, 0, 0, true), (11, 1, 1_000_000, false), (2, 0, 1_000_000, false)];
    rows.extend((0..1000).map(|i| (100 + i, 1, 1, false)));
    nested(&rows)
}

/// `n` folders x `n` folders x `n` files, sizes from a fixed xorshift sequence (1 B to about 1 GB).
fn random_nested(n: u64) -> Tree {
    let mut rows = Vec::new();
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next_id = 1u64;
    for _ in 0..n {
        let top = next_id;
        next_id += 1;
        rows.push((top, 0, 0, true));
        for _ in 0..n {
            let mid = next_id;
            next_id += 1;
            rows.push((mid, top, 0, true));
            for _ in 0..n {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let size = 1 + (x % 1024) * (1u64 << ((x >> 58) % 21));
                rows.push((next_id, mid, size, false));
                next_id += 1;
            }
        }
    }
    nested(&rows)
}

/// Inner rect of the view root: the inset on all sides, never a band.
fn root_inner(chrome: Chrome, w: u32, h: u32) -> (f64, f64, f64, f64) {
    let p = chrome.pad;
    (p, p, f64::from(w) - 2.0 * p, f64::from(h) - 2.0 * p)
}

/// Each rect lies inside the inner rect of its parent (the view root's for top-level rects).
fn assert_inside_inner(tree: &Tree, rects: &[Rect], chrome: Chrome, w: u32, h: u32) {
    for c in rects {
        let parent = tree.node(c.node).parent.unwrap();
        let (ix, iy, iw, ih) =
            if parent == tree.root() { root_inner(chrome, w, h) } else { chrome.inner(&rect_of(rects, parent)) };
        assert!(
            c.x >= ix - EPS && c.y >= iy - EPS && c.x + c.w <= ix + iw + EPS && c.y + c.h <= iy + ih + EPS,
            "{} {c:?} is outside its parent's inner rect ({ix}, {iy}, {iw}, {ih})",
            tree.name(c.node)
        );
    }
}

fn assert_same_as_layout(tree: &Tree, w: u32, h: u32) {
    let plain = layout(tree, tree.root(), w, h);

    let framed = layout_framed(tree, tree.root(), w, h, Chrome::NONE);

    assert!(!plain.is_empty());
    assert_eq!(framed.len(), plain.len());
    for (a, b) in framed.iter().zip(&plain) {
        assert_eq!(a.node, b.node);
        let d = (a.x - b.x).abs().max((a.y - b.y).abs()).max((a.w - b.w).abs()).max((a.h - b.h).abs());
        assert!(d <= 1e-9, "{a:?} differs from {b:?} by {d}");
    }
}

// ---- Chrome ----

#[test]
fn chrome_scaled_one_is_band_16_inset_2() {
    assert_eq!(Chrome::scaled(1.0), ONE);
}

#[test]
fn chrome_scaled_two_is_band_32_inset_4() {
    assert_eq!(Chrome::scaled(2.0), Chrome { header: 32.0, pad: 4.0 });
}

#[test]
fn has_header_height_boundary() {
    let r = |h: f64| Rect { node: NodeId(1), x: 0.0, y: 0.0, w: 100.0, h };
    assert!(!ONE.has_header(&r(31.99)));
    assert!(ONE.has_header(&r(32.0)));
}

#[test]
fn has_header_width_boundary() {
    let r = |w: f64| Rect { node: NodeId(1), x: 0.0, y: 0.0, w, h: 100.0 };
    assert!(!ONE.has_header(&r(47.99)));
    assert!(ONE.has_header(&r(48.0)));
}

#[test]
fn has_header_no_band_chrome_is_false() {
    let r = Rect { node: NodeId(1), x: 0.0, y: 0.0, w: 1000.0, h: 1000.0 };
    assert!(!Chrome::NONE.has_header(&r));
}

#[test]
fn inner_with_header_is_below_the_band_and_inset() {
    let r = Rect { node: NodeId(1), x: 10.0, y: 20.0, w: 100.0, h: 50.0 };
    assert_eq!(ONE.inner(&r), (12.0, 36.0, 96.0, 32.0));
}

#[test]
fn inner_without_header_is_inset_on_all_sides() {
    let r = Rect { node: NodeId(1), x: 10.0, y: 20.0, w: 40.0, h: 8.0 };
    assert_eq!(ONE.inner(&r), (12.0, 22.0, 36.0, 4.0));
}

#[test]
fn inner_below_four_insets_is_the_rect_itself() {
    let r = Rect { node: NodeId(1), x: 10.0, y: 20.0, w: 40.0, h: 7.99 };
    assert_eq!(ONE.inner(&r), (10.0, 20.0, 40.0, 7.99));
}

// ---- layout_framed ----

#[test]
fn layout_framed_zero_chrome_equals_layout_sample() {
    assert_same_as_layout(&sample(), W, H);
}

#[test]
fn layout_framed_zero_chrome_equals_layout_tiny_files() {
    assert_same_as_layout(&tiny_files(), 100, 100);
}

#[test]
fn layout_framed_zero_chrome_equals_layout_nested() {
    assert_same_as_layout(&random_nested(10), W, H);
}

#[test]
fn layout_framed_children_lie_inside_the_parent_inner_rect() {
    let tree = sample();

    let rects = layout_framed(&tree, tree.root(), W, H, ONE);

    assert_eq!(rects.len(), 12, "{rects:?}");
    assert_inside_inner(&tree, &rects, ONE, W, H);
}

#[test]
fn layout_framed_no_rect_lies_in_a_header_band() {
    let tree = random_nested(6);

    let rects = layout_framed(&tree, tree.root(), W, H, ONE);

    let framed: Vec<&Rect> = rects.iter().filter(|r| ONE.has_header(r)).collect();
    assert!(framed.len() >= 2, "only {} frames with a band", framed.len());
    for f in framed {
        let band = (f.x, f.y, f.w, ONE.header);
        for c in rects.iter().filter(|c| is_ancestor(&tree, f.node, c.node)) {
            let ox = (c.x + c.w).min(band.0 + band.2) - c.x.max(band.0);
            let oy = (c.y + c.h).min(band.1 + band.3) - c.y.max(band.1);
            assert!(ox <= EPS || oy <= EPS, "{c:?} is in the band of {f:?}");
        }
    }
}

#[test]
fn layout_framed_sibling_areas_are_proportional_to_size() {
    let tree = random_nested(6);

    let rects = layout_framed(&tree, tree.root(), W, H, ONE);

    let mut groups = 0;
    for p in std::iter::once(tree.root()).chain(rects.iter().map(|r| r.node)) {
        let kids: Vec<&Rect> = rects.iter().filter(|r| tree.node(r.node).parent == Some(p)).collect();
        if kids.len() < 2 {
            continue;
        }
        groups += 1;
        let per_byte = |r: &Rect| r.w * r.h / tree.node(r.node).size as f64;
        let first = per_byte(kids[0]);
        for k in &kids[1..] {
            let rel = (per_byte(k) - first).abs() / first;
            assert!(rel <= 1e-6, "{} {k:?}: area per byte off by {rel}", tree.name(k.node));
        }
    }
    assert!(groups >= 10, "only {groups} sibling groups");
}

#[test]
fn layout_framed_frame_below_four_insets_is_filled_with_no_inset() {
    // root{ n1{ n11 } } at 8x8: the root insets to 4x4, which is under 4 * 2 px.
    let tree = nested(&[(1, 0, 0, true), (11, 1, 10, false)]);

    let rects = layout_framed(&tree, tree.root(), 8, 8, ONE);

    let at = |r: &Rect| (r.x, r.y, r.w, r.h);
    assert_eq!(rects.len(), 2, "{rects:?}");
    assert_eq!(at(&rects[0]), (2.0, 2.0, 4.0, 4.0));
    assert_eq!(at(&rects[1]), (2.0, 2.0, 4.0, 4.0));
}

#[test]
fn layout_framed_one_huge_file_is_the_root_inner_rect() {
    let tree = tree_of(&[42]);

    let rects = layout_framed(&tree, tree.root(), W, H, ONE);

    assert_eq!(rects.len(), 1);
    let r = rects[0];
    assert_eq!((r.x, r.y, r.w, r.h), root_inner(ONE, W, H));
}

#[test]
fn layout_framed_all_zero_sizes_returns_empty() {
    let tree = tree_of(&[0, 0]);

    assert!(layout_framed(&tree, tree.root(), W, H, ONE).is_empty());
}

#[test]
fn layout_framed_zero_width_or_height_returns_empty() {
    let tree = sample();

    assert!(layout_framed(&tree, tree.root(), 0, H, ONE).is_empty());
    assert!(layout_framed(&tree, tree.root(), W, 0, ONE).is_empty());
}

#[test]
fn layout_framed_one_by_one_pixel_does_not_panic() {
    let tree = tree_of(&[42]);

    let rects = layout_framed(&tree, tree.root(), 1, 1, Chrome::scaled(2.0));

    assert!(rects.len() <= 1, "{rects:?}");
}

#[test]
fn layout_framed_lists_each_folder_before_its_descendants() {
    let tree = sample();

    let rects = layout_framed(&tree, tree.root(), W, H, ONE);

    let mut pairs = 0;
    for (j, d) in rects.iter().enumerate() {
        for (i, a) in rects.iter().enumerate() {
            if is_ancestor(&tree, a.node, d.node) {
                pairs += 1;
                assert!(i < j, "{} (index {i}) comes after its descendant {} (index {j})", tree.name(a.node), tree.name(d.node));
            }
        }
    }
    assert_eq!(pairs, 10);
}

#[test]
fn layout_framed_same_input_gives_same_output() {
    let tree = random_nested(6);

    let a = layout_framed(&tree, tree.root(), W, H, ONE);
    let b = layout_framed(&tree, tree.root(), W, H, ONE);

    assert!(!a.is_empty());
    assert_eq!(a, b);
}

#[test]
fn layout_framed_million_files_rect_count_is_bounded_by_pixel_count() {
    let tree = random_nested(100);
    let (w, h) = (1600, 900);

    let started = std::time::Instant::now();
    let rects = layout_framed(&tree, tree.root(), w, h, ONE);
    eprintln!("framed layout of 1,010,100 nodes at {w}x{h}: {:?}, {} rects", started.elapsed(), rects.len());

    assert!(!rects.is_empty());
    assert!(rects.len() <= (w * h) as usize, "{} rects for {} pixels", rects.len(), w * h);
}

#[test]
fn layout_framed_at_scale_two_rects_lie_inside_the_scaled_inner_rects() {
    let tree = sample();
    let chrome = Chrome::scaled(2.0);

    let rects = layout_framed(&tree, tree.root(), 2 * W, 2 * H, chrome);

    assert_eq!(rects.len(), 12, "{rects:?}");
    assert!(rects.iter().any(|r| chrome.has_header(r)));
    assert_inside_inner(&tree, &rects, chrome, 2 * W, 2 * H);
}

// ---- render_framed ----

// root
//   n1 (dir)  n11=600  n12 (dir){ n121=300  n122=100 }
//   n2=200
fn frames() -> Tree {
    nested(&[(1, 0, 0, true), (11, 1, 600, false), (12, 1, 0, true), (121, 12, 300, false), (122, 12, 100, false), (2, 0, 200, false)])
}

struct Shot {
    tree: Tree,
    rects: Vec<Rect>,
    px: Vec<u8>,
}

fn shot(theme: Theme) -> Shot {
    let tree = frames();
    let rects = layout_framed(&tree, tree.root(), W, H, ONE);
    let px = render_framed(&tree, &rects, W, H, ONE, theme);
    Shot { tree, rects, px }
}

impl Shot {
    fn at(&self, x: f64, y: f64) -> [u8; 4] {
        let i = ((y as usize) * W as usize + x as usize) * 4;
        self.px[i..i + 4].try_into().unwrap()
    }

    fn rect(&self, name: &str) -> Rect {
        rect_of(&self.rects, named(&self.tree, name))
    }

    /// A pixel in the middle of the label band of folder `name`.
    fn band(&self, name: &str) -> [u8; 4] {
        let r = self.rect(name);
        assert!(ONE.has_header(&r), "{name} has no band: {r:?}");
        self.at(r.x + r.w / 2.0, r.y + ONE.header / 2.0)
    }
}

/// Brightness of a straight RGBA pixel drawn over the dark Mica backdrop #202020.
fn lum_over_dark(p: [u8; 4]) -> f64 {
    let a = f64::from(p[3]) / 255.0;
    let c = |v: u8| f64::from(v) * a + 32.0 * (1.0 - a);
    0.2126 * c(p[0]) + 0.7152 * c(p[1]) + 0.0722 * c(p[2])
}

#[test]
fn render_framed_no_rects_is_fully_transparent() {
    let tree = frames();

    let px = render_framed(&tree, &[], 4, 4, ONE, Theme::Dark);

    assert_eq!(px, vec![0u8; 64]);
}

#[test]
fn render_framed_returns_w_times_h_rgba_pixels() {
    let s = shot(Theme::Dark);

    assert_eq!(s.px.len(), (W * H * 4) as usize);
}

#[test]
fn render_framed_file_tile_centre_is_opaque() {
    let s = shot(Theme::Dark);
    let r = s.rect("n11");

    assert_eq!(s.at(r.x + r.w / 2.0, r.y + r.h / 2.0)[3], 255);
}

#[test]
fn render_framed_header_band_is_translucent() {
    let s = shot(Theme::Dark);

    let a = s.band("n1")[3];

    assert!(a > 0 && a < 255, "band alpha {a}");
}

#[test]
fn render_framed_nested_band_is_more_opaque_than_the_outer_band() {
    let s = shot(Theme::Dark);

    let (outer, inner) = (s.band("n1")[3], s.band("n12")[3]);

    assert!(inner > outer, "nested band alpha {inner}, outer band alpha {outer}");
}

#[test]
fn render_framed_border_is_lighter_than_fill_in_dark() {
    let s = shot(Theme::Dark);
    let r = s.rect("n1");

    let border = s.at(r.x.round(), r.y + r.h / 2.0);
    let fill = s.band("n1");

    assert!(lum_over_dark(border) > lum_over_dark(fill), "border {border:?}, fill {fill:?}");
}

#[test]
fn render_framed_tile_of_12_px_or_more_has_a_cut_corner() {
    let s = shot(Theme::Dark);
    let r = s.rect("n2");
    assert!(r.w >= 12.0 && r.h >= 12.0, "{r:?}");

    let corner = s.at(r.x.round(), r.y.round());

    assert!(corner[3] < 255, "corner pixel {corner:?} is fully painted");
}

#[test]
fn render_framed_tile_top_row_is_lighter_than_bottom_row() {
    let s = shot(Theme::Dark);
    let r = s.rect("n11");
    let x = r.x + r.w / 2.0;
    let top = r.y.round();
    // The last painted row: the tile may leave a gap at its bottom edge.
    let bottom = (top as usize..(r.y + r.h).round() as usize).rev().find(|&y| s.at(x, y as f64)[3] == 255).unwrap();

    let (t, b) = (s.at(x, top), s.at(x, bottom as f64));

    assert!(lum_over_dark(t) > lum_over_dark(b), "top {t:?}, bottom {b:?}");
}

#[test]
fn render_framed_light_fill_differs_from_dark_fill() {
    let (dark, light) = (shot(Theme::Dark), shot(Theme::Light));

    assert_ne!(dark.band("n1"), light.band("n1"));
}
