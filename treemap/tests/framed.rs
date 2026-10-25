use scan::rollup::{RawEntry, rollup};
use scan::{NodeId, Tree};
use treemap::{Chrome, Frame, Rect, Theme, ease_out, eased, file_label_spots, label_spots, layout, layout_framed, render_framed, unzoom, zoom_at, zoom_frame};

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
    assert!(!ONE.has_header(&r(23.99)));
    assert!(ONE.has_header(&r(24.0)));
}

#[test]
fn has_header_width_boundary() {
    let r = |w: f64| Rect { node: NodeId(1), x: 0.0, y: 0.0, w, h: 100.0 };
    assert!(!ONE.has_header(&r(27.99)));
    assert!(ONE.has_header(&r(28.0)));
}

#[test]
fn has_header_at_scale_two_boundaries_are_56_by_48() {
    let c = Chrome::scaled(2.0);
    let r = |w: f64, h: f64| Rect { node: NodeId(1), x: 0.0, y: 0.0, w, h };
    assert_eq!([c.has_header(&r(55.99, 48.0)), c.has_header(&r(56.0, 47.99)), c.has_header(&r(56.0, 48.0))], [false, false, true]);
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

// ---- cushion shading of small tiles ----

/// One file "f1" (file type Other) drawn as a `side` x `side` tile filling a `side` x `side`
/// picture at chrome `chrome`. Returns a pixel getter.
fn one_tile(side: u32, chrome: Chrome) -> impl Fn(u32, u32) -> [u8; 4] {
    let tree = tree_of(&[10]);
    let file = named(&tree, "f1");
    let rects = [Rect { node: file, x: 0.0, y: 0.0, w: f64::from(side), h: f64::from(side) }];
    let px = render_framed(&tree, &rects, side, side, chrome, Theme::Dark);
    move |x, y| {
        let i = ((y * side + x) * 4) as usize;
        px[i..i + 4].try_into().unwrap()
    }
}

#[test]
fn render_framed_small_tile_left_is_brighter_than_right_on_one_row() {
    let at = one_tile(16, ONE);

    // x 4 and 10 lie the same distance from the middle of the 15 px tile (1 px gap on the right).
    let (left, right) = (at(4, 5), at(10, 5));

    assert!(lum_over_dark(left) > lum_over_dark(right), "left {left:?}, right {right:?}");
}

#[test]
fn render_framed_small_tile_bottom_right_is_darker_than_its_colour() {
    let at = one_tile(16, ONE);
    let [r, g, b] = treemap::filetype::colour(treemap::filetype::Category::Other, Theme::Dark);

    let corner = at(11, 11);

    assert!(lum_over_dark(corner) < lum_over_dark([r, g, b, 255]), "bottom right {corner:?}, colour {:?}", [r, g, b]);
}

#[test]
fn render_framed_tile_of_23_px_is_shaded_across_a_row() {
    let at = one_tile(23, ONE);

    let (left, right) = (at(5, 6), at(17, 6));

    assert!(lum_over_dark(left) > lum_over_dark(right), "left {left:?}, right {right:?}");
}

#[test]
fn render_framed_tile_of_24_px_keeps_flat_rows() {
    let at = one_tile(24, ONE);

    let (left, right) = (at(5, 6), at(17, 6));

    assert_eq!(left, right);
}

#[test]
fn render_framed_at_scale_two_tile_of_46_px_is_shaded_across_a_row() {
    let at = one_tile(46, Chrome::scaled(2.0));

    let (left, right) = (at(10, 12), at(33, 12));

    assert!(lum_over_dark(left) > lum_over_dark(right), "left {left:?}, right {right:?}");
}

#[test]
fn render_framed_at_scale_two_tile_of_48_px_keeps_flat_rows() {
    let at = one_tile(48, Chrome::scaled(2.0));

    let (left, right) = (at(10, 12), at(35, 12));

    assert_eq!(left, right);
}

#[test]
fn render_framed_large_tile_is_unchanged_flat_rows_and_plain_bottom() {
    let at = one_tile(100, ONE);
    let [r, g, b] = treemap::filetype::colour(treemap::filetype::Category::Other, Theme::Dark);

    let row: Vec<[u8; 4]> = (2..96).map(|x| at(x, 30)).collect();
    let bottom = at(50, 90);

    assert!(row.iter().all(|&p| p == row[0]), "row 30 is not flat: {row:?}");
    assert_eq!(bottom, [r, g, b, 255]);
}

#[test]
fn render_framed_small_tiles_same_input_gives_the_same_pixels() {
    let tree = tree_of(&[50, 30, 20, 10, 5, 5, 3, 2, 1, 1]);
    let rects = layout_framed(&tree, tree.root(), 60, 40, ONE);

    let (a, b) = (render_framed(&tree, &rects, 60, 40, ONE, Theme::Dark), render_framed(&tree, &rects, 60, 40, ONE, Theme::Dark));

    assert_eq!(a, b);
}

#[test]
fn render_framed_every_small_tile_size_paints_opaque_pixels_without_overflow() {
    let tree = tree_of(&[10]);
    let file = named(&tree, "f1");
    for chrome in [ONE, Chrome::scaled(2.0)] {
        for w in 1..=48u32 {
            for h in 1..=48u32 {
                let rects = [Rect { node: file, x: 0.0, y: 0.0, w: f64::from(w), h: f64::from(h) }];
                for theme in [Theme::Dark, Theme::Light] {
                    let px = render_framed(&tree, &rects, w, h, chrome, theme);

                    // The first pixel of the middle row is painted: no corner and no gap there.
                    let i = (((h / 2) * w) * 4) as usize;
                    assert_eq!(px.len(), (w * h * 4) as usize);
                    assert!(px[i + 3] > 0, "{w}x{h} {chrome:?} {theme:?}: middle row starts clear");
                }
            }
        }
    }
}

// ---- label_spots ----

/// A rect of node `name` in `sample()`.
fn at(tree: &Tree, name: &str, x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { node: named(tree, name), x, y, w, h }
}

#[test]
fn label_spots_folder_with_a_band_gets_one_spot_on_its_band() {
    let tree = sample();
    let rects = [at(&tree, "n1", 10.0, 20.0, 100.0, 50.0)];

    let spots = label_spots(&tree, &rects, ONE, 96.0, 10);

    assert_eq!(spots, vec![at(&tree, "n1", 12.0, 20.0, 96.0, 16.0)]);
}

#[test]
fn label_spots_band_narrower_than_min_w_gets_none() {
    let tree = sample();
    let rects = [at(&tree, "n1", 10.0, 20.0, 100.0, 50.0)];

    assert!(label_spots(&tree, &rects, ONE, 96.01, 10).is_empty());
}

#[test]
fn label_spots_folder_without_a_band_gets_none() {
    let tree = sample();
    let rects = [at(&tree, "n1", 0.0, 0.0, 100.0, 23.99)];

    assert!(label_spots(&tree, &rects, ONE, 24.0, 10).is_empty());
}

/// The narrowest frame with a band: its band is exactly 24 px, so `min_w` 24 keeps it and
/// anything wider removes it. `min_w`, not the band rule, decides at this edge.
#[test]
fn label_spots_narrowest_banded_frame_is_kept_at_min_w_24() {
    let tree = sample();
    let rects = [at(&tree, "n1", 0.0, 0.0, 28.0, 24.0)];

    let spots = label_spots(&tree, &rects, ONE, 24.0, 10);

    assert_eq!(spots, vec![at(&tree, "n1", 2.0, 0.0, 24.0, 16.0)]);
}

#[test]
fn label_spots_file_gets_none() {
    let tree = sample();
    let rects = [at(&tree, "n3", 0.0, 0.0, 400.0, 300.0)];

    assert!(label_spots(&tree, &rects, ONE, 24.0, 10).is_empty());
}

/// Folders in depth-first order; by area n2 > n1 > n13 > n4.
fn four_frames(tree: &Tree) -> [Rect; 4] {
    [
        at(tree, "n1", 0.0, 0.0, 200.0, 100.0),
        at(tree, "n13", 2.0, 16.0, 100.0, 40.0),
        at(tree, "n2", 200.0, 0.0, 300.0, 200.0),
        at(tree, "n4", 0.0, 100.0, 60.0, 40.0),
    ]
}

#[test]
fn label_spots_more_than_cap_keeps_the_largest_in_depth_first_order() {
    let tree = sample();
    let rects = four_frames(&tree);

    let spots = label_spots(&tree, &rects, ONE, 24.0, 2);

    let names: Vec<&str> = spots.iter().map(|s| tree.name(s.node)).collect();
    assert_eq!(names, ["n1", "n2"]);
}

#[test]
fn label_spots_cap_above_the_count_keeps_every_spot() {
    let tree = sample();
    let rects = four_frames(&tree);

    let spots = label_spots(&tree, &rects, ONE, 24.0, 5);

    let names: Vec<&str> = spots.iter().map(|s| tree.name(s.node)).collect();
    assert_eq!(names, ["n1", "n13", "n2", "n4"]);
}

#[test]
fn label_spots_equal_areas_keep_the_first_in_rects() {
    let tree = sample();
    let rects = [at(&tree, "n2", 0.0, 0.0, 100.0, 50.0), at(&tree, "n1", 100.0, 0.0, 100.0, 50.0)];

    let spots = label_spots(&tree, &rects, ONE, 24.0, 1);

    assert_eq!(spots.len(), 1);
    assert_eq!(tree.name(spots[0].node), "n2");
}

#[test]
fn label_spots_cap_zero_is_empty() {
    let tree = sample();
    let rects = four_frames(&tree);

    assert!(label_spots(&tree, &rects, ONE, 24.0, 0).is_empty());
}

#[test]
fn label_spots_at_scale_two_band_is_32_high_inside_a_4_px_inset() {
    let tree = sample();
    let rects = [at(&tree, "n1", 10.0, 20.0, 200.0, 100.0)];

    let spots = label_spots(&tree, &rects, Chrome::scaled(2.0), 48.0, 10);

    assert_eq!(spots, vec![at(&tree, "n1", 14.0, 20.0, 192.0, 32.0)]);
}

#[test]
fn label_spots_of_a_real_layout_are_the_bands_of_the_frames() {
    let tree = sample();
    let rects = layout_framed(&tree, tree.root(), W, H, ONE);

    let spots = label_spots(&tree, &rects, ONE, 24.0, usize::MAX);

    let frames: Vec<NodeId> = rects.iter().filter(|r| tree.node(r.node).is_dir && ONE.has_header(r)).map(|r| r.node).collect();
    assert!(frames.len() >= 3, "{frames:?}");
    assert_eq!(spots.iter().map(|s| s.node).collect::<Vec<_>>(), frames);
}

// ---- file_label_spots ----

/// The least tile for a file name at 100%: 40 x 14 logical px.
const FILE_MIN: (f64, f64) = (40.0, 14.0);

#[test]
fn file_label_spots_tile_at_the_least_size_gets_its_tile() {
    let tree = sample();
    let rects = [at(&tree, "n3", 10.0, 20.0, 40.0, 14.0)];

    let spots = file_label_spots(&tree, &rects, FILE_MIN.0, FILE_MIN.1, 10);

    assert_eq!(spots, vec![at(&tree, "n3", 10.0, 20.0, 40.0, 14.0)]);
}

#[test]
fn file_label_spots_tile_narrower_than_min_w_gets_none() {
    let tree = sample();
    let rects = [at(&tree, "n3", 10.0, 20.0, 39.99, 300.0)];

    assert!(file_label_spots(&tree, &rects, FILE_MIN.0, FILE_MIN.1, 10).is_empty());
}

#[test]
fn file_label_spots_tile_lower_than_min_h_gets_none() {
    let tree = sample();
    let rects = [at(&tree, "n3", 10.0, 20.0, 400.0, 13.99)];

    assert!(file_label_spots(&tree, &rects, FILE_MIN.0, FILE_MIN.1, 10).is_empty());
}

#[test]
fn file_label_spots_folder_gets_none() {
    let tree = sample();
    let rects = [at(&tree, "n1", 0.0, 0.0, 400.0, 300.0)];

    assert!(file_label_spots(&tree, &rects, FILE_MIN.0, FILE_MIN.1, 10).is_empty());
}

#[test]
fn file_label_spots_zero_size_and_nan_tiles_get_none() {
    let tree = sample();
    let rects = [
        at(&tree, "n3", 0.0, 0.0, 0.0, 0.0),
        at(&tree, "n11", 0.0, 0.0, f64::NAN, 50.0),
        at(&tree, "n12", 0.0, 0.0, 50.0, f64::NAN),
        at(&tree, "n21", 0.0, 0.0, -80.0, 50.0),
    ];

    assert!(file_label_spots(&tree, &rects, FILE_MIN.0, FILE_MIN.1, 10).is_empty());
}

/// Files in depth-first order; by area n21 > n11 > n12 > n3.
fn four_tiles(tree: &Tree) -> [Rect; 4] {
    [
        at(tree, "n11", 0.0, 0.0, 100.0, 50.0),
        at(tree, "n12", 0.0, 50.0, 80.0, 50.0),
        at(tree, "n21", 200.0, 0.0, 300.0, 200.0),
        at(tree, "n3", 0.0, 100.0, 60.0, 40.0),
    ]
}

#[test]
fn file_label_spots_more_than_cap_keeps_the_largest_in_rects_order() {
    let tree = sample();
    let rects = four_tiles(&tree);

    let spots = file_label_spots(&tree, &rects, FILE_MIN.0, FILE_MIN.1, 2);

    let names: Vec<&str> = spots.iter().map(|s| tree.name(s.node)).collect();
    assert_eq!(names, ["n11", "n21"]);
}

#[test]
fn file_label_spots_cap_above_the_count_keeps_every_tile() {
    let tree = sample();
    let rects = four_tiles(&tree);

    let spots = file_label_spots(&tree, &rects, FILE_MIN.0, FILE_MIN.1, 5);

    let names: Vec<&str> = spots.iter().map(|s| tree.name(s.node)).collect();
    assert_eq!(names, ["n11", "n12", "n21", "n3"]);
}

#[test]
fn file_label_spots_equal_areas_keep_the_first_in_rects() {
    let tree = sample();
    let rects = [at(&tree, "n21", 0.0, 0.0, 100.0, 50.0), at(&tree, "n11", 100.0, 0.0, 50.0, 100.0)];

    let spots = file_label_spots(&tree, &rects, FILE_MIN.0, FILE_MIN.1, 1);

    let names: Vec<&str> = spots.iter().map(|s| tree.name(s.node)).collect();
    assert_eq!(names, ["n21"]);
}

#[test]
fn file_label_spots_cap_zero_is_empty() {
    let tree = sample();
    let rects = four_tiles(&tree);

    assert!(file_label_spots(&tree, &rects, FILE_MIN.0, FILE_MIN.1, 0).is_empty());
}

/// At 100%, 150% and 200% the least tile is 40 x 14 logical px times the scale: kept at the
/// edge, dropped 0.01 px under it on either side.
#[test]
fn file_label_spots_least_tile_scales_with_the_display_scale() {
    let tree = sample();
    let mut wrong = Vec::new();
    for scale in [1.0, 1.5, 2.0] {
        let (mw, mh) = (FILE_MIN.0 * scale, FILE_MIN.1 * scale);
        for (w, h, kept) in [(mw, mh, true), (mw - 0.01, mh, false), (mw, mh - 0.01, false)] {
            let rects = [at(&tree, "n3", 0.0, 0.0, w, h)];
            if file_label_spots(&tree, &rects, mw, mh, 10).len() != usize::from(kept) {
                wrong.push(format!("scale {scale}: {w} x {h} kept should be {kept}"));
            }
        }
    }

    assert!(wrong.is_empty(), "{wrong:?}");
}

#[test]
fn file_label_spots_of_a_real_layout_are_the_big_enough_file_tiles() {
    let tree = sample();
    let rects = layout_framed(&tree, tree.root(), W, H, ONE);

    let spots = file_label_spots(&tree, &rects, FILE_MIN.0, FILE_MIN.1, usize::MAX);

    let tiles: Vec<Rect> =
        rects.iter().filter(|r| !tree.node(r.node).is_dir && r.w >= FILE_MIN.0 && r.h >= FILE_MIN.1).copied().collect();
    assert!(tiles.len() >= 5, "{tiles:?}");
    assert_eq!(spots, tiles);
}

#[test]
fn file_label_spots_of_a_real_layout_overlap_no_frame_band() {
    let tree = sample();
    let rects = layout_framed(&tree, tree.root(), W, H, ONE);
    let bands = label_spots(&tree, &rects, ONE, 24.0, usize::MAX);

    let spots = file_label_spots(&tree, &rects, FILE_MIN.0, FILE_MIN.1, usize::MAX);

    let hits: Vec<(NodeId, NodeId)> = spots
        .iter()
        .flat_map(|s| bands.iter().filter(move |b| s.x < b.x + b.w && b.x < s.x + s.w && s.y < b.y + b.h && b.y < s.y + s.h).map(move |b| (s.node, b.node)))
        .collect();
    assert!(!spots.is_empty() && !bands.is_empty());
    assert!(hits.is_empty(), "{hits:?}");
}

// ---- zoom (P8) ----
/// A rect of node 0 (the zoom functions do not look at the node).
fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { node: NodeId(0), x, y, w, h }
}

const VW: f64 = W as f64;
const VH: f64 = H as f64;

fn near(a: Frame, b: Frame) -> bool {
    [(a.0, b.0), (a.1, b.1), (a.2, b.2), (a.3, b.3)].iter().all(|(p, q)| (p - q).abs() < EPS)
}

#[test]
fn zoom_frame_target_that_is_the_whole_view_is_the_view() {
    assert_eq!(zoom_frame(&r(0.0, 0.0, VW, VH), VW, VH), Some((0.0, 0.0, VW, VH)));
}

#[test]
fn zoom_frame_lower_right_quarter_doubles_the_picture() {
    assert_eq!(zoom_frame(&r(VW / 2.0, VH / 2.0, VW / 2.0, VH / 2.0), VW, VH), Some((-VW, -VH, 2.0 * VW, 2.0 * VH)));
}

#[test]
fn zoom_frame_puts_the_target_corners_on_the_view_corners() {
    let mut wrong = Vec::new();
    for t in [r(10.0, 20.0, 100.0, 50.0), r(0.0, 300.0, 800.0, 300.0), r(799.0, 599.0, 1.0, 1.0), r(123.4, 5.6, 7.8, 90.1)] {
        let (fx, fy, fw, fh) = zoom_frame(&t, VW, VH).unwrap();
        let (sx, sy) = (fw / VW, fh / VH);
        let corners = (fx + t.x * sx, fy + t.y * sy, fx + (t.x + t.w) * sx, fy + (t.y + t.h) * sy);
        if !near(corners, (0.0, 0.0, VW, VH)) {
            wrong.push(format!("{t:?} -> {corners:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:?}");
}

#[test]
fn zoom_frame_end_frame_covers_the_whole_view() {
    let mut wrong = Vec::new();
    for t in [r(10.0, 20.0, 100.0, 50.0), r(0.0, 0.0, 1.0, VH), r(400.0, 0.0, 400.0, 1.0), r(0.0, 0.0, VW, VH)] {
        let (fx, fy, fw, fh) = zoom_frame(&t, VW, VH).unwrap();
        if !(fx <= 0.0 && fy <= 0.0 && fx + fw >= VW - EPS && fy + fh >= VH - EPS) {
            wrong.push(format!("{t:?} -> {:?}", (fx, fy, fw, fh)));
        }
    }
    assert!(wrong.is_empty(), "{wrong:?}");
}

#[test]
fn zoom_frame_target_with_no_width_or_no_height_is_none() {
    let got = [r(10.0, 10.0, 0.0, 50.0), r(10.0, 10.0, 50.0, 0.0), r(10.0, 10.0, 0.0, 0.0), r(10.0, 10.0, -5.0, 50.0)].map(|t| zoom_frame(&t, VW, VH));
    assert_eq!(got, [None; 4]);
}

#[test]
fn zoom_frame_target_outside_the_view_is_none() {
    let got = [r(VW, 0.0, 10.0, 10.0), r(0.0, VH, 10.0, 10.0), r(-10.0, 0.0, 10.0, 10.0), r(0.0, -10.0, 10.0, 10.0)].map(|t| zoom_frame(&t, VW, VH));
    assert_eq!(got, [None; 4]);
}

#[test]
fn zoom_frame_target_partly_outside_is_cut_to_the_view() {
    assert_eq!(zoom_frame(&r(-VW / 2.0, 0.0, VW, VH), VW, VH), Some((0.0, 0.0, 2.0 * VW, VH)));
}

#[test]
fn zoom_frame_view_with_no_area_is_none() {
    let t = r(0.0, 0.0, 10.0, 10.0);
    let got = [(0.0, VH), (VW, 0.0), (-1.0, VH), (0.0, 0.0)].map(|(w, h)| zoom_frame(&t, w, h));
    assert_eq!(got, [None; 4]);
}

#[test]
fn zoom_frame_value_that_is_not_finite_is_none() {
    let got = [
        zoom_frame(&r(f64::NAN, 0.0, 10.0, 10.0), VW, VH),
        zoom_frame(&r(0.0, 0.0, f64::INFINITY, 10.0), VW, VH),
        zoom_frame(&r(0.0, 0.0, 10.0, 10.0), f64::NAN, VH),
        zoom_frame(&r(0.0, 0.0, 10.0, 10.0), VW, f64::INFINITY),
    ];
    assert_eq!(got, [None; 4]);
}

#[test]
fn zoom_frame_at_scale_two_is_twice_the_frame_at_scale_one() {
    let (x, y, w, h) = (100.0, 50.0, 200.0, 120.0);
    let one = zoom_frame(&r(x, y, w, h), VW, VH).unwrap();
    let two = zoom_frame(&r(2.0 * x, 2.0 * y, 2.0 * w, 2.0 * h), 2.0 * VW, 2.0 * VH).unwrap();
    assert!(near(two, (2.0 * one.0, 2.0 * one.1, 2.0 * one.2, 2.0 * one.3)), "{one:?} {two:?}");
}

#[test]
fn ease_out_starts_at_zero_and_ends_at_one() {
    assert_eq!((ease_out(0.0), ease_out(1.0)), (0.0, 1.0));
}

#[test]
fn ease_out_rises_at_every_step() {
    let v: Vec<f64> = (0..=100).map(|i| ease_out(f64::from(i) / 100.0)).collect();
    assert!(v.windows(2).all(|p| p[1] > p[0]), "{v:?}");
}

#[test]
fn ease_out_is_fast_at_the_start_and_slow_at_the_end() {
    let steps: Vec<f64> = (0..10).map(|i| ease_out(f64::from(i + 1) / 10.0) - ease_out(f64::from(i) / 10.0)).collect();
    assert!((ease_out(0.5) - 0.875).abs() < EPS, "{}", ease_out(0.5));
    assert!(steps.windows(2).all(|p| p[1] < p[0]), "{steps:?}");
}

#[test]
fn ease_out_time_outside_zero_to_one_is_cut() {
    assert_eq!([-1.0, 2.0, f64::NEG_INFINITY, f64::INFINITY].map(ease_out), [0.0, 1.0, 0.0, 1.0]);
}

#[test]
fn ease_out_nan_is_the_end() {
    assert_eq!(ease_out(f64::NAN), 1.0);
}

#[test]
fn eased_fade_in_goes_from_clear_to_solid() {
    assert_eq!((eased(0.0, 1.0, 0.0), eased(0.0, 1.0, 1.0)), (0.0, 1.0));
}

#[test]
fn eased_fade_out_goes_from_solid_to_clear() {
    assert_eq!((eased(1.0, 0.0, 0.0), eased(1.0, 0.0, 1.0)), (1.0, 0.0));
}

#[test]
fn eased_fade_in_rises_at_every_step_and_stays_in_zero_to_one() {
    let v: Vec<f64> = (-10..=110).map(|i| eased(0.0, 1.0, f64::from(i) / 100.0)).collect();
    assert!(v.iter().all(|a| (0.0..=1.0).contains(a)), "{v:?}");
    assert!(v[10..=110].windows(2).all(|p| p[1] > p[0]), "{v:?}");
}

#[test]
fn eased_nan_time_is_the_end_value() {
    assert_eq!(eased(0.0, 1.0, f64::NAN), 1.0);
}

#[test]
fn zoom_at_starts_at_from_and_ends_at_to() {
    let (from, to) = ((0.0, 0.0, VW, VH), (-VW, -VH, 2.0 * VW, 2.0 * VH));
    assert_eq!((zoom_at(from, to, 0.0), zoom_at(from, to, 1.0)), (from, to));
}

#[test]
fn zoom_at_every_value_moves_one_way_from_from_to_to() {
    let (from, to) = ((0.0, 0.0, VW, VH), (-50.0, -400.0, 3.0 * VW, 1.5 * VH));
    let v: Vec<Frame> = (0..=50).map(|i| zoom_at(from, to, f64::from(i) / 50.0)).collect();
    let ok = v.windows(2).all(|p| p[1].0 < p[0].0 && p[1].1 < p[0].1 && p[1].2 > p[0].2 && p[1].3 > p[0].3);
    assert!(ok, "{v:?}");
}

#[test]
fn zoom_at_nan_time_is_the_end_frame() {
    let (from, to) = ((0.0, 0.0, VW, VH), (10.0, 20.0, 30.0, 40.0));
    assert_eq!(zoom_at(from, to, f64::NAN), to);
}

#[test]
fn unzoom_picture_at_the_view_gives_the_same_point() {
    assert_eq!(unzoom((0.0, 0.0, VW, VH), VW, VH, 123.0, 45.0), Some((123.0, 45.0)));
}

#[test]
fn unzoom_doubled_picture_centre_of_the_view_is_three_quarters_into_the_picture() {
    assert_eq!(unzoom((-VW, -VH, 2.0 * VW, 2.0 * VH), VW, VH, VW / 2.0, VH / 2.0), Some((0.75 * VW, 0.75 * VH)));
}

#[test]
fn unzoom_of_the_zoom_frame_puts_the_view_corners_on_the_target() {
    let t = r(100.0, 50.0, 200.0, 120.0);
    let frame = zoom_frame(&t, VW, VH).unwrap();
    let (a, b) = (unzoom(frame, VW, VH, 0.0, 0.0).unwrap(), unzoom(frame, VW, VH, VW, VH).unwrap());
    assert!(near((a.0, a.1, b.0, b.1), (t.x, t.y, t.x + t.w, t.y + t.h)), "{a:?} {b:?}");
}

#[test]
fn unzoom_frame_with_no_area_or_not_finite_is_none() {
    let got = [
        unzoom((0.0, 0.0, 0.0, VH), VW, VH, 1.0, 1.0),
        unzoom((0.0, 0.0, VW, 0.0), VW, VH, 1.0, 1.0),
        unzoom((f64::NAN, 0.0, VW, VH), VW, VH, 1.0, 1.0),
        unzoom((0.0, 0.0, VW, VH), VW, VH, f64::INFINITY, 1.0),
    ];
    assert_eq!(got, [None; 4]);
}
