//! The graph layout: columns of bands, the flows between them, hit-testing and label spots
//! (plan decisions 7 to 10).
//!
//! Column 0 is the view folder alone. Column `c + 1` is the `split` of every band of column `c`
//! that is open: the view folder, or a folder in `expanded` that has a sized subfolder. Groups
//! keep their parents' order, so the bands come out column by column, each column in parent
//! order, each group in `split` order (folders, "files", "other").
//!
//! One scale for the whole layout (`px_per_byte`): every band is exactly `size * px_per_byte`
//! tall. It is chosen so that the tallest need of any column (its bytes, at most the view's,
//! plus its gaps) fits in `h`, over all columns and not only the visible ones, so scrolling
//! sideways never resizes a band. The gaps shrink when they would take more than half of `h`.
//!
//! Vertical placement: a group is centred on its parent band; groups that overlap are pushed
//! down (forward pass), and groups that run past the bottom are pushed back up (backward pass).

use scan::{NodeId, Tree};

use crate::split::is_merged;
use crate::{BAND_W, Band, Expander, Flow, Graph, Input, MERGE_PX, Part, VISIBLE_COLUMNS, split};

/// Gap between two bands of one group, in logical px.
const GAP_IN: f64 = 2.0;
/// Gap between two groups of one column, in logical px.
const GAP_OUT: f64 = 8.0;
/// The gaps of a column take at most this share of the height; beyond it they shrink.
const MAX_GAP_SHARE: f64 = 0.5;

/// One band before placement.
struct Node {
    part: Part,
    column: u16,
    size: u64,
    newest: u32,
    expander: Expander,
}

/// The children of one open band: `nodes[start..end]`, all in the column after the parent's.
struct Group {
    parent: usize,
    start: usize,
    end: usize,
}

/// The bands and groups at one scale, column by column.
struct Shape {
    nodes: Vec<Node>,
    groups: Vec<Group>,
    columns: u16,
}

fn has_sized_subfolder(tree: &Tree, n: NodeId) -> bool {
    tree.children(n).any(|c| {
        let node = tree.node(c);
        node.is_dir && node.size > 0
    })
}

/// The newest time of a group's "files" band (every direct file, zero-size ones too, as the
/// roll-up counts them) and of its merged folders. One pass over the children, no allocation.
fn group_newest(tree: &Tree, parent: NodeId, k: f64, min_px: f64, keep_small: bool) -> (u32, u32) {
    let parent_size = tree.node(parent).size;
    let (mut files, mut merged) = (0u32, 0u32);
    for c in tree.children(parent) {
        let node = tree.node(c);
        if !node.is_dir {
            files = files.max(node.modified);
        } else if node.size > 0 && is_merged(node.size, parent_size, k, min_px, keep_small) {
            merged = merged.max(node.modified);
        }
    }
    (files, merged)
}

/// The bands at scale `k`. `full` also fills `newest` and the expanders (the last pass only).
fn build(input: &Input, k: f64, min_px: f64, full: bool) -> Shape {
    let tree = input.tree;
    let view = tree.node(input.view);
    let mut nodes = vec![Node {
        part: Part::Folder(input.view),
        column: 0,
        size: view.size,
        newest: view.modified,
        expander: Expander::None,
    }];
    let mut groups = Vec::new();
    let mut i = 0;
    while i < nodes.len() {
        let (part, column) = (nodes[i].part, nodes[i].column);
        let Part::Folder(id) = part else {
            i += 1;
            continue;
        };
        let can_open = i == 0 || has_sized_subfolder(input.tree, id);
        if i > 0 && full && can_open {
            nodes[i].expander = if input.expanded.contains(&id) { Expander::Expanded } else { Expander::Collapsed };
        }
        if can_open && (i == 0 || input.expanded.contains(&id)) {
            let keep_small = input.open_other.contains(&id);
            let s = split(tree, id, k, min_px, keep_small);
            let (files_newest, merged_newest) = if full { group_newest(tree, id, k, min_px, keep_small) } else { (0, 0) };
            let start = nodes.len();
            let child = |part: Part, size: u64, newest: u32| Node { part, column: column + 1, size, newest, expander: Expander::None };
            for &f in &s.folders {
                let n = tree.node(f);
                nodes.push(child(Part::Folder(f), n.size, n.modified));
            }
            if let Some(bytes) = s.files {
                nodes.push(child(Part::Files(id), bytes, files_newest));
            }
            if let Some((folders, files, bytes)) = s.other {
                let newest = if files { merged_newest.max(files_newest) } else { merged_newest };
                nodes.push(child(Part::Other { parent: id, folders, files }, bytes, newest));
            }
            if nodes.len() > start {
                groups.push(Group { parent: i, start, end: nodes.len() });
            }
        }
        i += 1;
    }
    let columns = nodes.last().map_or(0, |n| n.column + 1);
    Shape { nodes, groups, columns }
}

/// The largest gap total of any column, in logical px times `scale` (before any shrinking).
fn max_gaps(shape: &Shape, scale: f64) -> f64 {
    let mut worst: f64 = 0.0;
    let mut current = (u16::MAX, 0.0);
    for g in &shape.groups {
        let column = shape.nodes[g.start].column;
        let inner = (g.end - g.start - 1) as f64 * GAP_IN * scale;
        current = if column == current.0 { (column, current.1 + GAP_OUT * scale + inner) } else { (column, inner) };
        worst = worst.max(current.1);
    }
    worst
}

/// Lays the graph out. An empty graph when the view has no size or the picture no area.
///
/// `Part::Other { parent, folders, files }` names the merged members by count, so it is only
/// valid for the tree it was made from: after the tree changes (a delete), lay out again and
/// do not reuse an old `Other` (for example as a cache key) without a fresh split.
pub fn layout(input: &Input) -> Graph {
    let (w, h, scale) = (input.w, input.h, input.scale);
    let total = input.tree.node(input.view).size;
    let usable = [w, h, scale].iter().all(|v| v.is_finite() && *v > 0.0);
    if !usable || total == 0 {
        return Graph::default();
    }
    let min_px = MERGE_PX * scale;

    // Pass 1 at the scale without gaps gives the most bands, so the most gaps any column can
    // need. A smaller scale only merges more, so pass 2 needs no more gaps than pass 1 found.
    let first = build(input, h / total as f64, min_px, false);
    let gaps = max_gaps(&first, scale);
    let shrink = if gaps > MAX_GAP_SHARE * h { MAX_GAP_SHARE * h / gaps } else { 1.0 };
    let k = (h - shrink * gaps) / total as f64;
    debug_assert!(k >= 0.0, "px_per_byte {k}");
    let shape = build(input, k, min_px, true);

    let ys = place(&shape, k, h, scale * shrink);
    emit(&shape, &ys, k, input)
}

/// The top of every node: column 0 centred, each group centred on its parent, then overlaps
/// pushed down and the bottom pushed back up.
fn place(shape: &Shape, k: f64, h: f64, gap_scale: f64) -> Vec<f64> {
    let nodes = &shape.nodes;
    let mut ys = vec![0.0; nodes.len()];
    ys[0] = (h - nodes[0].size as f64 * k) / 2.0;
    let (gap_in, gap_out) = (GAP_IN * gap_scale, GAP_OUT * gap_scale);
    let span = |g: &Group| {
        let bytes: f64 = nodes[g.start..g.end].iter().map(|n| n.size as f64 * k).sum();
        bytes + (g.end - g.start - 1) as f64 * gap_in
    };
    let mut tops = Vec::new();
    let mut at = 0;
    while at < shape.groups.len() {
        let column = nodes[shape.groups[at].start].column;
        let end = at + shape.groups[at..].iter().take_while(|g| nodes[g.start].column == column).count();
        let col = &shape.groups[at..end];
        // Forward: centred on the parent, but below the previous group.
        tops.clear();
        let mut floor: f64 = 0.0;
        for g in col {
            let p = &nodes[g.parent];
            let s = span(g);
            let top = (ys[g.parent] + p.size as f64 * k / 2.0 - s / 2.0).max(floor);
            tops.push(top);
            floor = top + s + gap_out;
        }
        // Backward: above the next group and inside the picture.
        let mut ceiling = h;
        for (g, top) in col.iter().zip(tops.iter_mut()).rev() {
            let s = span(g);
            *top = top.min(ceiling - s);
            ceiling = *top - gap_out;
        }
        for (g, &top) in col.iter().zip(&tops) {
            let mut y = top;
            for (n, slot) in nodes[g.start..g.end].iter().zip(&mut ys[g.start..g.end]) {
                *slot = y;
                y += n.size as f64 * k + gap_in;
            }
        }
        at = end;
    }
    ys
}

/// The visible columns as bands and flows.
fn emit(shape: &Shape, ys: &[f64], k: f64, input: &Input) -> Graph {
    let columns = shape.columns;
    let first = input.first_column.min(columns.saturating_sub(VISIBLE_COLUMNS));
    let shown = columns.min(VISIBLE_COLUMNS);
    let pitch = input.w / f64::from(shown);
    let band_w = (BAND_W * input.scale).min(pitch);
    let visible = |c: u16| c >= first && c - first < VISIBLE_COLUMNS;

    let mut index = vec![usize::MAX; shape.nodes.len()];
    let mut bands = Vec::new();
    for (i, n) in shape.nodes.iter().enumerate() {
        if visible(n.column) {
            index[i] = bands.len();
            bands.push(Band {
                part: n.part,
                column: n.column,
                x: f64::from(n.column - first) * pitch,
                y: ys[i],
                w: band_w,
                h: n.size as f64 * k,
                size: n.size,
                newest: n.newest,
                expander: n.expander,
            });
        }
    }
    let mut flows = Vec::new();
    for g in &shape.groups {
        let (from, child_column) = (index[g.parent], shape.nodes[g.start].column);
        if from == usize::MAX || !visible(child_column) {
            continue;
        }
        let mut cursor = ys[g.parent];
        for c in g.start..g.end {
            let hc = shape.nodes[c].size as f64 * k;
            flows.push(Flow { from, to: index[c], from_y: (cursor, cursor + hc), to_y: (ys[c], ys[c] + hc) });
            cursor += hc;
        }
    }
    Graph { bands, flows, columns, first_column: first, px_per_byte: k }
}

/// The band under `(x, y)`: left and top edges are inside, right and bottom edges are not.
/// Flows and gaps give `None`.
pub fn hit(graph: &Graph, x: f64, y: f64) -> Option<usize> {
    graph.bands.iter().position(|b| x >= b.x && x < b.x + b.w && y >= b.y && y < b.y + b.h)
}

/// One `Rect` per folder band, with the band's geometry, in band order.
pub fn folder_rects(graph: &Graph) -> Vec<treemap::Rect> {
    graph
        .bands
        .iter()
        .filter_map(|b| match b.part {
            Part::Folder(node) => Some(treemap::Rect { node, x: b.x, y: b.y, w: b.w, h: b.h }),
            _ => None,
        })
        .collect()
}

/// The bands tall enough for a label (`h >= min_h`): at most `cap`, the tallest kept (ties to
/// the earlier band), returned in band order. O(n).
pub fn label_spots(graph: &Graph, min_h: f64, cap: usize) -> Vec<usize> {
    let bands = &graph.bands;
    let mut spots: Vec<usize> = (0..bands.len()).filter(|&i| bands[i].h >= min_h).collect();
    if spots.len() > cap {
        spots.select_nth_unstable_by(cap, |&a, &b| bands[b].h.total_cmp(&bands[a].h).then(a.cmp(&b)));
        spots.truncate(cap);
        spots.sort_unstable();
    }
    spots
}
