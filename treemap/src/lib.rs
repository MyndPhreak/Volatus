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

/// Rects for every non-empty leaf under `root`, tiling a `w` x `h` area.
/// Squarified (Bruls et al.), largest first, ties broken by `NodeId`: deterministic.
pub fn layout(tree: &Tree, root: NodeId, w: u32, h: u32) -> Vec<Rect> {
    let mut leaves = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let mut kids = tree.children(n).peekable();
        if kids.peek().is_some() {
            stack.extend(kids);
        } else if tree.node(n).size > 0 {
            leaves.push((n, tree.node(n).size));
        }
    }
    let total: u64 = leaves.iter().map(|&(_, s)| s).sum();
    if total == 0 || w == 0 || h == 0 {
        return Vec::new();
    }
    leaves.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let scale = f64::from(w) * f64::from(h) / total as f64;
    let areas: Vec<f64> = leaves.iter().map(|&(_, s)| s as f64 * scale).collect();

    let (mut x, mut y, mut rw, mut rh) = (0.0, 0.0, f64::from(w), f64::from(h));
    let mut out = Vec::with_capacity(leaves.len());
    let mut i = 0;
    while i < leaves.len() {
        let side = rw.min(rh);
        let mut end = i + 1;
        let mut sum = areas[i];
        while end < leaves.len() {
            let next = sum + areas[end];
            if worst(&areas[i..=end], next, side) > worst(&areas[i..end], sum, side) {
                break;
            }
            sum = next;
            end += 1;
        }
        // The row fills the short side; its thickness is what the row's area needs.
        let horizontal = rw >= rh; // column on the left, items stacked top to bottom
        // The last row takes exactly what is left, so float drift cannot leave a gap.
        let thickness = match (end == leaves.len(), horizontal) {
            (true, true) => rw,
            (true, false) => rh,
            (false, true) => sum / rh,
            (false, false) => sum / rw,
        };
        let (mut cx, mut cy) = (x, y);
        for k in i..end {
            let last = k + 1 == end;
            let (iw, ih) = if horizontal {
                (thickness, if last { y + rh - cy } else { areas[k] / thickness })
            } else {
                (if last { x + rw - cx } else { areas[k] / thickness }, thickness)
            };
            out.push(Rect { node: leaves[k].0, x: cx, y: cy, w: iw, h: ih });
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
    out
}

/// Worst aspect ratio of a row of `areas` (summing to `sum`) laid along `side`.
fn worst(areas: &[f64], sum: f64, side: f64) -> f64 {
    let max = areas.iter().cloned().fold(f64::MIN, f64::max);
    let min = areas.iter().cloned().fold(f64::MAX, f64::min);
    let s2 = side * side;
    (s2 * max / (sum * sum)).max(sum * sum / (s2 * min))
}

/// RGBA pixels, `w * h * 4` bytes. Background is transparent; each rect gets a stable colour.
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
