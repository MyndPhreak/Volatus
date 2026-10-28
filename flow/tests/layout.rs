//! TP2.3: columns, flows, hit-testing and label spots of the graph layout.

use std::collections::{HashMap, HashSet};

use flow::{BAND_W, Band, Expander, Graph, Input, MERGE_PX, Part, VISIBLE_COLUMNS, folder_rects, hit, label_spots, layout, members};
use scan::rollup::{RawEntry, rollup};
use scan::{NodeId, Tree};

const EPS: f64 = 1e-6;

/// Builds a tree with `rollup`. Every entry is reachable, so a raw id `i` (1-based, in input
/// order) is `NodeId(i - 1)`.
struct Build {
    entries: Vec<RawEntry>,
}

impl Build {
    fn new() -> Self {
        let mut b = Build { entries: Vec::new() };
        b.push(1, "root", 0, true, 0);
        b
    }

    fn push(&mut self, parent: u64, name: &str, size: u64, is_dir: bool, modified: u32) -> u64 {
        let id = self.entries.len() as u64 + 1;
        self.entries.push(RawEntry { id, parent, name: name.into(), size, allocated: size, is_dir, modified });
        id
    }

    fn dir(&mut self, parent: u64, name: &str) -> u64 {
        self.push(parent, name, 0, true, 0)
    }

    fn file(&mut self, parent: u64, name: &str, size: u64, modified: u32) -> u64 {
        self.push(parent, name, size, false, modified)
    }

    /// A folder holding one file of `size` written at `modified`.
    fn folder(&mut self, parent: u64, name: &str, size: u64, modified: u32) -> u64 {
        let d = self.dir(parent, name);
        if size > 0 {
            self.file(d, "f", size, modified);
        }
        d
    }

    fn build(self) -> Tree {
        let r = rollup(1, self.entries).unwrap();
        assert_eq!((r.orphans, r.skipped), (0, 0));
        r.tree
    }
}

fn nid(raw: u64) -> NodeId {
    NodeId((raw - 1) as u32)
}

const ROOT: u64 = 1;

/// The main fixture. root:
/// - A 5,100: A1 3,000 {x 2,000, A1s 1,000}, A2 1,500 (one file), a.txt 400, five tiny folders of
///   40 (0.8 %, merged), and A3 0 (zero size)
/// - B 3,000: B1 2,000 (one file), B2 900 {B2s 900}, b.txt 100
/// - C 1,500: only files (c1 1,000, c2 500)
/// - D 400: one file
/// - s1, s2, s3 of 50 (0.5 %, merged); s1 holds a subfolder; r.txt 50; z (0 bytes, the newest)
struct Fixture {
    tree: Tree,
    root: NodeId,
    a: NodeId,
    a1: NodeId,
    a1s: NodeId,
    a2: NodeId,
    b: NodeId,
    b2: NodeId,
    c: NodeId,
    d: NodeId,
    s1: NodeId,
    e: NodeId,
}

fn fixture() -> Fixture {
    let mut b = Build::new();
    let a = b.dir(ROOT, "A");
    let a1 = b.dir(a, "A1");
    b.file(a1, "x", 2000, 110);
    let a1s = b.folder(a1, "A1s", 1000, 120);
    let a2 = b.folder(a, "A2", 1500, 130);
    b.file(a, "a.txt", 400, 140);
    for i in 0..5 {
        b.folder(a, &format!("at{i}"), 40, 150 + i);
    }
    b.dir(a, "A3");
    let bb = b.dir(ROOT, "B");
    b.folder(bb, "B1", 2000, 210);
    let b2 = b.dir(bb, "B2");
    b.folder(b2, "B2s", 900, 220);
    b.file(bb, "b.txt", 100, 230);
    let c = b.dir(ROOT, "C");
    b.file(c, "c1", 1000, 310);
    b.file(c, "c2", 500, 320);
    let d = b.folder(ROOT, "D", 400, 410);
    let s1 = b.dir(ROOT, "s1");
    b.folder(s1, "s1s", 50, 510);
    b.folder(ROOT, "s2", 50, 520);
    b.folder(ROOT, "s3", 50, 530);
    b.file(ROOT, "r.txt", 50, 540);
    b.file(ROOT, "z", 0, 999);
    let e = b.dir(ROOT, "E");
    let tree = b.build();
    assert_eq!(tree.node(nid(a)).size, 5100);
    assert_eq!(tree.node(nid(ROOT)).size, 5100 + 3000 + 1500 + 400 + 150 + 50);
    Fixture {
        tree,
        root: nid(ROOT),
        a: nid(a),
        a1: nid(a1),
        a1s: nid(a1s),
        a2: nid(a2),
        b: nid(bb),
        b2: nid(b2),
        c: nid(c),
        d: nid(d),
        s1: nid(s1),
        e: nid(e),
    }
}

fn set(ids: &[NodeId]) -> HashSet<NodeId> {
    ids.iter().copied().collect()
}

#[allow(clippy::too_many_arguments)]
fn run(tree: &Tree, view: NodeId, expanded: &[NodeId], open_other: &[NodeId], w: f64, h: f64, scale: f64, first_column: u16) -> Graph {
    let (expanded, open_other) = (set(expanded), set(open_other));
    layout(&Input { tree, view, expanded: &expanded, open_other: &open_other, w, h, scale, first_column })
}

fn fixture_graph(f: &Fixture, w: f64, h: f64) -> Graph {
    run(&f.tree, f.root, &[f.a, f.b], &[], w, h, 1.0, 0)
}

fn index_of(g: &Graph, part: Part) -> usize {
    g.bands.iter().position(|b| b.part == part).unwrap_or_else(|| panic!("no band {part:?} in {:#?}", g.bands))
}

fn has_sized_subfolder(t: &Tree, n: NodeId) -> bool {
    t.children(n).any(|c| t.node(c).is_dir && t.node(c).size > 0)
}

/// The folder whose split made `part`.
fn group_parent(t: &Tree, part: Part) -> Option<NodeId> {
    match part {
        Part::Folder(n) => t.node(n).parent,
        Part::Files(p) | Part::Other { parent: p, .. } => Some(p),
    }
}

fn newest_of(t: &Tree, part: Part) -> u32 {
    match part {
        Part::Folder(n) => t.node(n).modified,
        _ => members(t, &part).iter().map(|&m| t.node(m).modified).max().unwrap_or(0),
    }
}

fn check_invariants(g: &Graph, t: &Tree, w: f64, h: f64) {
    check_invariants_at(g, t, w, h, 1.0);
}

/// Every invariant line of TP2.3 that holds for any input.
fn check_invariants_at(g: &Graph, t: &Tree, w: f64, h: f64, scale: f64) {
    let ctx = format!("{w}x{h} at scale {scale}");
    let k = g.px_per_byte;
    assert!(k.is_finite() && k > 0.0, "{ctx}: px_per_byte {k}");
    assert!(g.columns >= 1 && !g.bands.is_empty(), "{ctx}: empty graph");
    let first = g.first_column;
    let shown = g.columns.min(VISIBLE_COLUMNS);
    let pitch = w / f64::from(shown);
    let thick = BAND_W * scale;

    for (i, b) in g.bands.iter().enumerate() {
        let bc = format!("{ctx}: band {i} {:?}", b.part);
        // Heights are exactly proportional, with one scale for the whole layout.
        assert!((b.h - b.size as f64 * k).abs() < EPS, "{bc}: h {} for size {}", b.h, b.size);
        assert!(b.size > 0, "{bc}: zero size");
        // Inside the picture.
        assert!(b.y >= -EPS && b.y + b.h <= h + EPS, "{bc}: y {} h {}", b.y, b.h);
        // Only the visible columns, at whole pitches, BAND_W thick.
        assert!(b.column >= first && b.column < first + VISIBLE_COLUMNS && b.column < g.columns, "{bc}: column {}", b.column);
        assert!((b.x - f64::from(b.column - first) * pitch).abs() < EPS, "{bc}: x {} pitch {pitch}", b.x);
        assert!((b.w - thick).abs() < EPS, "{bc}: w {}", b.w);
        if g.columns <= VISIBLE_COLUMNS {
            assert!(b.x + b.w <= w + EPS, "{bc}: right edge {}", b.x + b.w);
        }
        // Only "other" bands (and the view folder itself) are under the merge line.
        if b.column > 0 && !matches!(b.part, Part::Other { .. }) {
            assert!(b.h >= MERGE_PX * scale - EPS, "{bc}: {} px is under the merge line", b.h);
        }
        // The parts are what they say.
        match b.part {
            Part::Folder(n) => {
                assert!(t.node(n).is_dir, "{bc}");
                assert_eq!(b.size, t.node(n).size, "{bc}");
            }
            Part::Files(p) => {
                let files: u64 = t.children(p).filter(|&c| !t.node(c).is_dir).map(|c| t.node(c).size).sum();
                assert_eq!(b.size, files, "{bc}");
                assert_eq!(b.expander, Expander::None, "{bc}");
            }
            Part::Other { .. } => {
                let sum: u64 = members(t, &b.part).iter().map(|&m| t.node(m).size).sum();
                assert_eq!(b.size, sum, "{bc}");
                assert_eq!(b.expander, Expander::None, "{bc}");
            }
        }
        assert_eq!(b.newest, newest_of(t, b.part), "{bc}: newest");
        // Column 0 is the view folder alone, always open, with no expander.
        if b.column == 0 {
            assert!(matches!(b.part, Part::Folder(_)), "{bc}");
            assert_eq!(b.expander, Expander::None, "{bc}");
            assert_eq!(g.bands.iter().filter(|o| o.column == 0).count(), 1, "{bc}: column 0 holds one band");
        }
        // Expanders follow the tree.
        if let Part::Folder(n) = b.part
            && b.column > 0
        {
            let can = has_sized_subfolder(t, n);
            assert_eq!(b.expander != Expander::None, can, "{bc}: expander {:?}", b.expander);
        }
    }

    // No overlap within a column.
    for c in first..first + VISIBLE_COLUMNS {
        let mut col: Vec<&Band> = g.bands.iter().filter(|b| b.column == c).collect();
        col.sort_by(|a, b| a.y.total_cmp(&b.y));
        for pair in col.windows(2) {
            assert!(pair[1].y >= pair[0].y + pair[0].h - EPS, "{ctx}: column {c} overlap {:?} / {:?}", pair[0], pair[1]);
        }
    }

    // Flows: one per (parent, child), between neighbouring columns.
    let mut pairs = HashSet::new();
    let mut incoming = vec![0usize; g.bands.len()];
    let mut out: HashMap<usize, Vec<usize>> = HashMap::new();
    for (fi, fl) in g.flows.iter().enumerate() {
        let fc = format!("{ctx}: flow {fi} {fl:?}");
        assert!(pairs.insert((fl.from, fl.to)), "{fc}: two flows for one pair");
        let (p, c) = (&g.bands[fl.from], &g.bands[fl.to]);
        assert_eq!(c.column, p.column + 1, "{fc}");
        assert!(fl.from < fl.to, "{fc}");
        assert_eq!(group_parent(t, c.part), match p.part {
            Part::Folder(n) => Some(n),
            _ => panic!("{fc}: a flow from a {:?} band", p.part),
        }, "{fc}: child of another folder");
        assert!((fl.to_y.0 - c.y).abs() < EPS && (fl.to_y.1 - (c.y + c.h)).abs() < EPS, "{fc}: to_y");
        incoming[fl.to] += 1;
        out.entry(fl.from).or_default().push(fi);
    }
    // Every band right of the first visible column has exactly one parent flow.
    for (i, b) in g.bands.iter().enumerate() {
        let want = usize::from(b.column > first);
        assert_eq!(incoming[i], want, "{ctx}: band {i} {:?} has {} incoming flows", b.part, incoming[i]);
    }
    // Per group: sizes and heights are conserved, from_y tiles the parent in child order, and
    // the order is folders by size (ties by id), then files, then other.
    for (&p, fis) in &out {
        let pb = &g.bands[p];
        let gc = format!("{ctx}: group of {:?}", pb.part);
        let mut fis = fis.clone();
        fis.sort_by_key(|&fi| g.flows[fi].to);
        let sizes: u64 = fis.iter().map(|&fi| g.bands[g.flows[fi].to].size).sum();
        let hs: f64 = fis.iter().map(|&fi| g.bands[g.flows[fi].to].h).sum();
        assert_eq!(sizes, pb.size, "{gc}: sizes");
        assert!((hs - pb.h).abs() < EPS, "{gc}: heights {hs} vs {}", pb.h);
        let mut cursor = pb.y;
        for &fi in &fis {
            let fl = &g.flows[fi];
            let c = &g.bands[fl.to];
            assert!((fl.from_y.0 - cursor).abs() < EPS, "{gc}: from_y {:?} at {cursor}", fl.from_y);
            assert!((fl.from_y.1 - fl.from_y.0 - c.h).abs() < EPS, "{gc}: from_y span {:?}", fl.from_y);
            cursor = fl.from_y.1;
        }
        assert!((cursor - (pb.y + pb.h)).abs() < EPS, "{gc}: from_y ends at {cursor}");
        let rank = |part: Part| match part {
            Part::Folder(_) => 0,
            Part::Files(_) => 1,
            Part::Other { .. } => 2,
        };
        let kids: Vec<&Band> = fis.iter().map(|&fi| &g.bands[g.flows[fi].to]).collect();
        for pair in kids.windows(2) {
            let (x, y) = (pair[0], pair[1]);
            assert!(rank(x.part) <= rank(y.part), "{gc}: order {:?} then {:?}", x.part, y.part);
            if let (Part::Folder(xn), Part::Folder(yn)) = (x.part, y.part) {
                assert!(x.size > y.size || (x.size == y.size && xn < yn), "{gc}: folder order");
            }
            assert!(rank(y.part) == 0 || rank(x.part) != rank(y.part), "{gc}: two files or other bands");
        }
    }
    // An open band whose children are on screen has them; a closed one has none.
    for (i, b) in g.bands.iter().enumerate() {
        let open = b.column == 0 || b.expander == Expander::Expanded;
        let next_shown = b.column + 1 < first + VISIBLE_COLUMNS;
        if open && next_shown {
            assert!(out.contains_key(&i), "{ctx}: open band {:?} has no children", b.part);
        }
        if !open {
            assert!(!out.contains_key(&i), "{ctx}: closed band {:?} has children", b.part);
        }
    }
}

// ---- Empty inputs ----

#[test]
fn layout_zero_size_view_is_empty() {
    let f = fixture();
    assert_eq!(f.tree.node(f.e).size, 0);

    let g = run(&f.tree, f.e, &[], &[], 800.0, 600.0, 1.0, 0);

    assert!(g.bands.is_empty() && g.flows.is_empty(), "{g:?}");
    assert_eq!(g.columns, 0);
}

#[test]
fn layout_zero_width_or_height_is_empty() {
    let f = fixture();
    for (w, h) in [(0.0, 600.0), (800.0, 0.0), (0.0, 0.0)] {
        let g = run(&f.tree, f.root, &[f.a], &[], w, h, 1.0, 0);
        assert!(g.bands.is_empty() && g.flows.is_empty(), "{w}x{h}: {g:?}");
        assert_eq!(g.columns, 0, "{w}x{h}");
    }
}

#[test]
fn layout_zero_size_tree_is_empty() {
    let mut b = Build::new();
    b.dir(ROOT, "empty");
    b.file(ROOT, "z", 0, 5);
    let t = b.build();

    let g = run(&t, t.root(), &[], &[], 800.0, 600.0, 1.0, 0);

    assert!(g.bands.is_empty() && g.flows.is_empty(), "{g:?}");
}

// ---- Columns ----

#[test]
fn layout_column_0_is_the_view_band_alone() {
    let f = fixture();

    let g = fixture_graph(&f, 800.0, 600.0);

    let col0: Vec<&Band> = g.bands.iter().filter(|b| b.column == 0).collect();
    assert_eq!(col0.len(), 1);
    let v = col0[0];
    assert_eq!(v.part, Part::Folder(f.root));
    assert_eq!(v.expander, Expander::None);
    assert_eq!(v.size, f.tree.node(f.root).size);
    assert!((v.h - v.size as f64 * g.px_per_byte).abs() < EPS);
    assert_eq!(g.bands[0].part, Part::Folder(f.root));
}

#[test]
fn layout_fixture_has_three_columns_with_both_expanded_groups() {
    let f = fixture();

    let g = fixture_graph(&f, 800.0, 600.0);

    assert_eq!(g.columns, 3);
    assert_eq!(g.first_column, 0);
    let col1: Vec<Part> = g.bands.iter().filter(|b| b.column == 1).map(|b| b.part).collect();
    assert_eq!(col1, [
        Part::Folder(f.a),
        Part::Folder(f.b),
        Part::Folder(f.c),
        Part::Folder(f.d),
        Part::Other { parent: f.root, folders: 3, files: true },
    ]);
    // Column 2: A's group (A1, A2, files, other of 5), then B's group.
    let col2: Vec<Part> = g.bands.iter().filter(|b| b.column == 2).map(|b| b.part).collect();
    assert_eq!(col2[..4], [
        Part::Folder(f.a1),
        Part::Folder(f.a2),
        Part::Files(f.a),
        Part::Other { parent: f.a, folders: 5, files: false },
    ]);
    assert!(col2[4..].iter().all(|&p| group_parent(&f.tree, p) == Some(f.b)), "{col2:?}");
    assert!(col2.contains(&Part::Folder(f.b2)));
}

#[test]
fn layout_invariants_hold_at_every_size() {
    let f = fixture();
    for (w, h) in [(1920.0, 1080.0), (800.0, 600.0), (400.0, 300.0), (300.0, 5.0)] {
        let g = fixture_graph(&f, w, h);
        check_invariants(&g, &f.tree, w, h);
    }
}

#[test]
fn layout_invariants_hold_when_child_gaps_need_more_room() {
    // 20 equal folders of 5 % each, every one expanded with 5 equal subfolders: column 1 is
    // tall, column 2 has 100 bands in 20 groups, so its gaps decide the scale.
    let mut b = Build::new();
    let mut tops = Vec::new();
    for i in 0..20 {
        let d = b.dir(ROOT, &format!("d{i}"));
        for j in 0..5 {
            b.folder(d, &format!("d{i}_{j}"), 1000, i * 10 + j);
        }
        tops.push(nid(d));
    }
    let t = b.build();
    for (w, h) in [(1920.0, 1080.0), (800.0, 600.0), (400.0, 300.0), (300.0, 100.0), (300.0, 5.0)] {
        let g = run(&t, t.root(), &tops, &[], w, h, 1.0, 0);
        check_invariants(&g, &t, w, h);
    }
    // At 1920x1080 the 312 px of column 2's gaps fit and every subfolder is at least 6 px.
    let g = run(&t, t.root(), &tops, &[], 1920.0, 1080.0, 1.0, 0);
    assert_eq!(g.bands.iter().filter(|b| b.column == 2).count(), 100);
}

#[test]
fn layout_only_other_bands_are_under_the_line_and_the_line_scales() {
    // root 10,000: big 5,000, mid 4,600, small 400 (4 %, so only the pixel rule can merge it).
    let mut b = Build::new();
    b.folder(ROOT, "big", 5000, 1);
    b.folder(ROOT, "mid", 4600, 2);
    let small = nid(b.folder(ROOT, "small", 400, 3));
    let t = b.build();

    let g1 = run(&t, t.root(), &[], &[], 800.0, 300.0, 1.0, 0);
    let g2 = run(&t, t.root(), &[], &[], 800.0, 300.0, 2.0, 0);

    check_invariants_at(&g1, &t, 800.0, 300.0, 1.0);
    check_invariants_at(&g2, &t, 800.0, 300.0, 2.0);
    let s1 = &g1.bands[index_of(&g1, Part::Folder(small))];
    assert!(s1.h >= 6.0 && s1.h < 12.0, "small is {} px", s1.h);
    assert!(!g2.bands.iter().any(|b| b.part == Part::Folder(small)), "at scale 2 the line is 12 px");
    index_of(&g2, Part::Other { parent: t.root(), folders: 1, files: false });
    assert!(g2.bands.iter().all(|b| (b.w - 2.0 * BAND_W).abs() < EPS));
}

#[test]
fn layout_first_column_1_with_5_columns_drops_column_0() {
    // A chain root -> a -> b -> c -> d -> e, each with a file beside the next folder.
    let mut bld = Build::new();
    let mut parent = ROOT;
    let mut chain = Vec::new();
    for name in ["a", "b", "c", "d"] {
        let d = bld.dir(parent, name);
        bld.file(parent, &format!("{name}.bin"), 100, 1);
        chain.push(nid(d));
        parent = d;
    }
    bld.folder(parent, "e", 1000, 2);
    let t = bld.build();
    let expanded = [chain[0], chain[1], chain[2]];

    let all = run(&t, t.root(), &expanded, &[], 800.0, 600.0, 1.0, 0);
    let g = run(&t, t.root(), &expanded, &[], 800.0, 600.0, 1.0, 1);

    assert_eq!(all.columns, 5);
    assert_eq!(g.columns, 5);
    assert_eq!(g.first_column, 1);
    assert!(g.bands.iter().all(|b| b.column != 0));
    assert!(g.bands.iter().any(|b| b.column == 4));
    check_invariants(&all, &t, 800.0, 600.0);
    check_invariants(&g, &t, 800.0, 600.0);
    let a = &g.bands[index_of(&g, Part::Folder(chain[0]))];
    assert!(a.x.abs() < EPS && (a.w - BAND_W).abs() < EPS);
    // The scale does not depend on the scroll position.
    assert_eq!(g.px_per_byte, all.px_per_byte);

    // A first column past the end is clamped so the last four columns show.
    let far = run(&t, t.root(), &expanded, &[], 800.0, 600.0, 1.0, 9);
    assert_eq!(far, g);
}

// ---- Expanders ----

#[test]
fn layout_expanders_follow_subfolders_and_the_expanded_set() {
    let f = fixture();

    let g = fixture_graph(&f, 1920.0, 1080.0);
    let exp = |p: Part| g.bands[index_of(&g, p)].expander;

    assert_eq!(exp(Part::Folder(f.root)), Expander::None);
    assert_eq!(exp(Part::Folder(f.a)), Expander::Expanded);
    assert_eq!(exp(Part::Folder(f.b)), Expander::Expanded);
    assert_eq!(exp(Part::Folder(f.c)), Expander::None, "only files");
    assert_eq!(exp(Part::Folder(f.d)), Expander::None, "one file");
    assert_eq!(exp(Part::Folder(f.a1)), Expander::Collapsed);
    assert_eq!(exp(Part::Folder(f.b2)), Expander::Collapsed);
    assert_eq!(exp(Part::Folder(f.a2)), Expander::None);
    assert_eq!(exp(Part::Files(f.a)), Expander::None);
    assert_eq!(exp(Part::Other { parent: f.root, folders: 3, files: true }), Expander::None);
}

#[test]
fn layout_expanded_folder_merged_or_under_a_collapsed_folder_shows_no_children() {
    let f = fixture();

    let base = fixture_graph(&f, 800.0, 600.0);
    // s1 is merged into root's "other"; A1s sits under the collapsed A1.
    let extra = run(&f.tree, f.root, &[f.a, f.b, f.s1, f.a1s], &[], 800.0, 600.0, 1.0, 0);

    assert_eq!(extra, base);
    assert!(!base.bands.iter().any(|b| b.part == Part::Folder(f.a1s) || b.part == Part::Folder(f.s1)));
}

#[test]
fn layout_expanded_ids_outside_the_view_are_ignored() {
    let f = fixture();

    let plain = run(&f.tree, f.b, &[f.b2], &[], 800.0, 600.0, 1.0, 0);
    let with_outside = run(&f.tree, f.b, &[f.b2, f.a, f.a1, f.root], &[], 800.0, 600.0, 1.0, 0);

    assert_eq!(with_outside, plain);
    assert_eq!(plain.columns, 3);
    check_invariants(&plain, &f.tree, 800.0, 600.0);
}

#[test]
fn layout_folder_with_only_files_is_one_files_band_without_expander() {
    let f = fixture();

    let g = run(&f.tree, f.c, &[f.c], &[], 800.0, 600.0, 1.0, 0);

    let parts: Vec<Part> = g.bands.iter().map(|b| b.part).collect();
    assert_eq!(parts, [Part::Folder(f.c), Part::Files(f.c)]);
    assert!(g.bands.iter().all(|b| b.expander == Expander::None));
    assert_eq!(g.flows.len(), 1);
    check_invariants(&g, &f.tree, 800.0, 600.0);
}

#[test]
fn layout_open_other_shows_the_small_folders() {
    let f = fixture();

    // At 1920x1080 A's tiny folders (0.8 % each) are about 4 px: still merged by the pixel
    // rule. At a taller picture they pass it and show once "other" is opened.
    let g = run(&f.tree, f.root, &[f.a], &[f.a], 800.0, 6000.0, 1.0, 0);

    check_invariants(&g, &f.tree, 800.0, 6000.0);
    let a_kids = g.bands.iter().filter(|b| b.column == 2 && matches!(b.part, Part::Folder(_))).count();
    assert_eq!(a_kids, 2 + 5, "{:#?}", g.bands);
    assert!(!g.bands.iter().any(|b| matches!(b.part, Part::Other { parent, .. } if parent == f.a)));
}

// ---- Newest change ----

#[test]
fn layout_newest_is_folder_time_files_max_and_other_members_max() {
    // root 1,000: big 600 (at 10), u 80 (at 30), t 20 (2 %, merged, at 40), f1 300 (at 20),
    // z 0 bytes (at 50).
    let mut b = Build::new();
    let big = nid(b.folder(ROOT, "big", 600, 10));
    let u = nid(b.folder(ROOT, "u", 80, 30));
    b.folder(ROOT, "t", 20, 40);
    b.file(ROOT, "f1", 300, 20);
    b.file(ROOT, "z", 0, 50);
    let t = b.build();
    let root = t.root();

    let g = run(&t, root, &[], &[], 800.0, 600.0, 1.0, 0);
    let newest = |p: Part| g.bands[index_of(&g, p)].newest;

    assert_eq!(newest(Part::Folder(big)), 10);
    assert_eq!(newest(Part::Folder(u)), 30);
    assert_eq!(newest(Part::Files(root)), 50, "zero-size files count, as in the roll-up");
    assert_eq!(newest(Part::Other { parent: root, folders: 1, files: false }), 40);
    assert_eq!(newest(Part::Folder(root)), 50);
}

// ---- Hit-testing, folder rects, label spots ----

#[test]
fn hit_centre_is_the_band_and_right_and_bottom_edges_are_not() {
    let f = fixture();
    let g = fixture_graph(&f, 800.0, 600.0);

    for (i, b) in g.bands.iter().enumerate() {
        assert_eq!(hit(&g, b.x + b.w / 2.0, b.y + b.h / 2.0), Some(i), "{:?}", b.part);
        assert_eq!(hit(&g, b.x, b.y), Some(i), "top-left corner is inside: {:?}", b.part);
        assert_ne!(hit(&g, b.x + b.w, b.y + b.h / 2.0), Some(i), "right edge: {:?}", b.part);
        assert_ne!(hit(&g, b.x + b.w / 2.0, b.y + b.h), Some(i), "bottom edge: {:?}", b.part);
    }
}

#[test]
fn hit_gap_and_flow_area_are_none() {
    let f = fixture();
    let g = fixture_graph(&f, 800.0, 600.0);

    // A gap between two bands of column 1.
    let a = &g.bands[index_of(&g, Part::Folder(f.a))];
    let b = &g.bands[index_of(&g, Part::Folder(f.b))];
    assert!(b.y > a.y + a.h, "a gap between A and B");
    assert_eq!(hit(&g, a.x + a.w / 2.0, (a.y + a.h + b.y) / 2.0), None);
    // Between column 0 and column 1, where the flows run.
    let v = &g.bands[0];
    assert_eq!(hit(&g, (v.x + v.w + a.x) / 2.0, a.y + a.h / 2.0), None);
    // Outside the picture.
    assert_eq!(hit(&g, -1.0, 300.0), None);
    assert_eq!(hit(&g, 400.0, 601.0), None);
}

#[test]
fn folder_rects_one_per_folder_band_with_the_same_geometry() {
    let f = fixture();
    let g = fixture_graph(&f, 800.0, 600.0);

    let rects = folder_rects(&g);

    let folders: Vec<&Band> = g.bands.iter().filter(|b| matches!(b.part, Part::Folder(_))).collect();
    assert!(folders.len() >= 8);
    assert_eq!(rects.len(), folders.len());
    for (r, b) in rects.iter().zip(&folders) {
        assert_eq!(Part::Folder(r.node), b.part);
        assert_eq!((r.x, r.y, r.w, r.h), (b.x, b.y, b.w, b.h));
    }
}

fn band_of_h(h: f64) -> Band {
    Band { part: Part::Folder(NodeId(0)), column: 1, x: 0.0, y: 0.0, w: BAND_W, h, size: 1, newest: 0, expander: Expander::None }
}

fn graph_of_heights(hs: &[f64]) -> Graph {
    Graph { bands: hs.iter().map(|&h| band_of_h(h)).collect(), flows: Vec::new(), columns: 2, first_column: 0, px_per_byte: 1.0 }
}

#[test]
fn label_spots_only_tall_enough_bands_in_band_order() {
    let g = graph_of_heights(&[30.0, 10.0, 16.0, 15.9, 50.0]);

    assert_eq!(label_spots(&g, 16.0, 10), [0, 2, 4]);
}

#[test]
fn label_spots_cap_keeps_the_tallest_in_band_order() {
    let g = graph_of_heights(&[30.0, 20.0, 80.0, 20.0, 50.0, 40.0]);

    assert_eq!(label_spots(&g, 16.0, 3), [2, 4, 5]);
    // A tie at the cut goes to the earlier band.
    assert_eq!(label_spots(&g, 16.0, 5), [0, 1, 2, 4, 5]);
    assert_eq!(label_spots(&g, 16.0, 0), Vec::<usize>::new());
}

// ---- Determinism ----

#[test]
fn layout_twice_on_the_same_input_is_equal() {
    let f = fixture();

    let one = fixture_graph(&f, 1920.0, 1080.0);
    let two = fixture_graph(&f, 1920.0, 1080.0);

    assert_eq!(one, two);
    assert!(!one.bands.is_empty());
}
