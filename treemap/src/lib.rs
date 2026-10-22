//! Squarified treemap layout and rasterizer.

use scan::{NodeId, Tree};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub node: NodeId,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Nested treemap of `root` in a `w` x `h` area: one rect for each non-empty node below
/// `root` (folders and files), each inside its parent's rect, with area proportional to size.
/// Squarified (Bruls et al.) per folder, largest first, ties broken by `NodeId`: deterministic.
///
/// A rect less than 1 px wide or high is pruned with everything below it; its area stays
/// part of the parent's rect. So the count is bounded by the pixels, not by the nodes.
/// Order is depth first: a folder comes before its descendants, so the last rect that
/// contains a point is the deepest one.
pub fn layout(tree: &Tree, root: NodeId, w: u32, h: u32) -> Vec<Rect> {
    let total = tree.node(root).size;
    let mut out = Vec::new();
    if total == 0 || w == 0 || h == 0 {
        return out;
    }
    let scale = f64::from(w) * f64::from(h) / total as f64;
    let mut stack = vec![Rect { node: root, x: 0.0, y: 0.0, w: f64::from(w), h: f64::from(h) }];
    let (mut kids, mut placed) = (Vec::new(), Vec::new());
    while let Some(parent) = stack.pop() {
        if parent.node != root {
            out.push(parent);
        }
        // Under 1 px² a rect cannot be 1 px on both sides: drop it before sorting.
        kids.clear();
        kids.extend(tree.children(parent.node).map(|c| (c, tree.node(c).size)).filter(|&(_, s)| s as f64 * scale >= 1.0));
        kids.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let areas: Vec<f64> = kids.iter().map(|&(_, s)| s as f64 * scale).collect();
        // Snap the last row to the edge only when the children fill the parent exactly.
        let fills = kids.iter().map(|&(_, s)| s).sum::<u64>() == tree.node(parent.node).size;
        squarify(&areas, &parent, fills, &mut placed);
        // Reversed, so the largest child pops first.
        for (&(node, _), &(x, y, w, h)) in kids.iter().zip(&placed).rev() {
            if w >= 1.0 && h >= 1.0 {
                stack.push(Rect { node, x, y, w, h });
            }
        }
    }
    out
}

/// Places `areas` (sorted largest first) inside `b` as rows along the short side. Rects go
/// to `out` as `(x, y, w, h)`, in the order of `areas`. With `fills`, the last row takes
/// exactly what is left of `b`, so float drift cannot leave a gap.
fn squarify(areas: &[f64], b: &Rect, fills: bool, out: &mut Vec<(f64, f64, f64, f64)>) {
    out.clear();
    let (mut x, mut y, mut rw, mut rh) = (b.x, b.y, b.w, b.h);
    let mut i = 0;
    while i < areas.len() {
        let side = rw.min(rh);
        let mut end = i + 1;
        let mut sum = areas[i];
        // Sorted, so the row's largest area is its first and its smallest its last.
        while end < areas.len() {
            let next = sum + areas[end];
            if worst(areas[i], areas[end], next, side) > worst(areas[i], areas[end - 1], sum, side) {
                break;
            }
            sum = next;
            end += 1;
        }
        let horizontal = rw >= rh; // column on the left, items stacked top to bottom
        let snap = fills && end == areas.len();
        let thickness = match (snap, horizontal) {
            (true, true) => rw,
            (true, false) => rh,
            (false, true) => sum / rh,
            (false, false) => sum / rw,
        };
        let (mut cx, mut cy) = (x, y);
        let row = &areas[i..end];
        for (k, &area) in row.iter().enumerate() {
            // A row always spans the whole side: its last item takes what is left of it.
            let last = k + 1 == row.len();
            let (iw, ih) = if horizontal {
                (thickness, if last { y + rh - cy } else { area / thickness })
            } else {
                (if last { x + rw - cx } else { area / thickness }, thickness)
            };
            out.push((cx, cy, iw, ih));
            if horizontal {
                cy += ih;
            } else {
                cx += iw;
            }
        }
        if horizontal {
            x += thickness;
            rw -= thickness;
        } else {
            y += thickness;
            rh -= thickness;
        }
        i = end;
    }
}

/// Worst aspect ratio of a row with largest area `max` and smallest `min` (summing to `sum`)
/// laid along `side`.
fn worst(max: f64, min: f64, sum: f64, side: f64) -> f64 {
    let s2 = side * side;
    (s2 * max / (sum * sum)).max(sum * sum / (s2 * min))
}

/// RGBA pixels, `w * h * 4` bytes. Background is transparent; each rect gets a stable colour.
/// Rects are drawn in order, so a child paints over its folder; the folder's colour shows
/// where pruned children were.
pub fn render(rects: &[Rect], w: u32, h: u32) -> Vec<u8> {
    let (wu, hu) = (w as usize, h as usize);
    let mut px = vec![0u8; wu * hu * 4];
    for r in rects {
        let edge = |v: f64, max: usize| (v.round().max(0.0) as usize).min(max);
        let (x0, x1) = (edge(r.x, wu), edge(r.x + r.w, wu));
        let (y0, y1) = (edge(r.y, hu), edge(r.y + r.h, hu));
        let c = colour(r.node);
        for y in y0..y1 {
            for x in x0..x1 {
                px[(y * wu + x) * 4..][..4].copy_from_slice(&c);
            }
        }
    }
    px
}

/// Golden-ratio hash of the node id: neighbours get visibly different, mid-bright colours.
fn colour(n: NodeId) -> [u8; 4] {
    let hsh = n.0.wrapping_mul(2_654_435_761);
    [64 + (hsh >> 24) as u8 / 2, 64 + (hsh >> 16) as u8 / 2, 64 + (hsh >> 8) as u8 / 2, 255]
}
