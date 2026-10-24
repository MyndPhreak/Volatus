//! Squarified treemap layout and rasterizer.

pub mod filetype;

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

/// Frame chrome of the framed layout, in physical pixels: a label band on top of a folder
/// and an inset on its other sides. Derived per rect, never stored, so the layout, the
/// renderer and the label layer all get the same answer from the same rect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chrome {
    /// Height of the label band of a folder big enough for one.
    pub header: f64,
    /// Inset between a folder's edge and its children.
    pub pad: f64,
}

impl Chrome {
    /// No band and no inset: `layout_framed` then gives the same rects as `layout`.
    pub const NONE: Chrome = Chrome { header: 0.0, pad: 0.0 };

    /// The chrome at a display scale factor (1.0 = 96 DPI).
    pub fn scaled(scale: f64) -> Chrome {
        Chrome { header: 16.0 * scale, pad: 2.0 * scale }
    }

    /// True when `r` is big enough for a label band: 28 x 24 logical px at a 16 px band. Then
    /// the band is at least 24 px wide, so the label minimum width decides the narrowest label.
    pub fn has_header(&self, r: &Rect) -> bool {
        self.header > 0.0 && r.w >= 1.75 * self.header && r.h >= 1.5 * self.header
    }

    /// The area `(x, y, w, h)` inside folder `r` where its children go.
    pub fn inner(&self, r: &Rect) -> (f64, f64, f64, f64) {
        let p = self.pad;
        if self.has_header(r) { (r.x + p, r.y + self.header, r.w - 2.0 * p, r.h - self.header - p) } else { self.inset(r) }
    }

    /// `r` less the inset on all sides; `r` itself when it is under four insets wide or high.
    fn inset(&self, r: &Rect) -> (f64, f64, f64, f64) {
        let p = self.pad;
        if r.w >= 4.0 * p && r.h >= 4.0 * p { (r.x + p, r.y + p, r.w - 2.0 * p, r.h - 2.0 * p) } else { (r.x, r.y, r.w, r.h) }
    }
}

/// Light or dark window theme: picks the glass tokens of `render_framed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

/// Like `layout`, but every folder is a frame: its children lie inside `chrome.inner` of its
/// rect. The view root gets the inset on all sides and never a band.
///
/// Frames remove area, so each folder has its own scale (inner area / folder size): sizes are
/// proportional among siblings, not across the whole map. Pruning and order are as in `layout`.
pub fn layout_framed(tree: &Tree, root: NodeId, w: u32, h: u32, chrome: Chrome) -> Vec<Rect> {
    let mut out = Vec::new();
    if tree.node(root).size == 0 || w == 0 || h == 0 {
        return out;
    }
    let mut stack = vec![Rect { node: root, x: 0.0, y: 0.0, w: f64::from(w), h: f64::from(h) }];
    let (mut kids, mut placed) = (Vec::new(), Vec::new());
    while let Some(parent) = stack.pop() {
        let (x, y, iw, ih) = if parent.node == root {
            chrome.inset(&parent)
        } else {
            out.push(parent);
            chrome.inner(&parent)
        };
        let size = tree.node(parent.node).size;
        if size == 0 || iw <= 0.0 || ih <= 0.0 {
            continue;
        }
        let scale = iw * ih / size as f64;
        // Under 1 px² a rect cannot be 1 px on both sides: drop it before sorting.
        kids.clear();
        kids.extend(tree.children(parent.node).map(|c| (c, tree.node(c).size)).filter(|&(_, s)| s as f64 * scale >= 1.0));
        kids.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let areas: Vec<f64> = kids.iter().map(|&(_, s)| s as f64 * scale).collect();
        let fills = kids.iter().map(|&(_, s)| s).sum::<u64>() == size;
        squarify(&areas, &Rect { node: parent.node, x, y, w: iw, h: ih }, fills, &mut placed);
        // Reversed, so the largest child pops first.
        for (&(node, _), &(x, y, w, h)) in kids.iter().zip(&placed).rev() {
            if w >= 1.0 && h >= 1.0 {
                stack.push(Rect { node, x, y, w, h });
            }
        }
    }
    out
}

/// Where the frame labels go: for each folder in `rects` with a label band (`chrome.has_header`)
/// whose band is at least `min_w` wide less the inset on both sides, the rect
/// `(x + pad, y, w - 2 pad, header)` with the folder's node. Files get none.
///
/// At most `cap` spots: the largest folders by area (ties: the first in `rects`), in the order
/// of `rects`, so a folder's label comes before the labels of its descendants.
pub fn label_spots(tree: &Tree, rects: &[Rect], chrome: Chrome, min_w: f64, cap: usize) -> Vec<Rect> {
    let p = chrome.pad;
    let mut spots: Vec<(usize, Rect)> = rects
        .iter()
        .enumerate()
        .filter(|(_, r)| tree.node(r.node).is_dir && chrome.has_header(r) && r.w - 2.0 * p >= min_w)
        .map(|(i, r)| (i, Rect { node: r.node, x: r.x + p, y: r.y, w: r.w - 2.0 * p, h: chrome.header }))
        .collect();
    if spots.len() > cap {
        let area = |i: usize| rects[i].w * rects[i].h;
        spots.select_nth_unstable_by(cap, |a, b| area(b.0).total_cmp(&area(a.0)).then(a.0.cmp(&b.0)));
        spots.truncate(cap);
        spots.sort_unstable_by_key(|s| s.0);
    }
    spots.into_iter().map(|(_, s)| s).collect()
}

/// Where the file names go: the tile of each file in `rects` that is at least `min_w` x `min_h`.
/// Folders get none.
///
/// At most `cap` spots: the largest tiles by area (ties: the first in `rects`), in the order of
/// `rects`. The caller gives the room the frame labels leave, so frames come first.
pub fn file_label_spots(tree: &Tree, rects: &[Rect], min_w: f64, min_h: f64, cap: usize) -> Vec<Rect> {
    let _ = (tree, rects, min_w, min_h, cap);
    todo!()
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

/// Straight RGBA pixels, `w * h * 4` bytes, of the framed layout `rects`: folders are glass
/// frames, files are rounded, lit tiles. Background is transparent.
///
/// Frames are translucent (fill, a light border and a highlight row under it), so nested
/// frames add up. Tiles are opaque, with a highlight that fades out over their top half; a
/// tile of `6 * pad` or more also gets rounded corners and a light ring. The tile colour is
/// the file-type colour of its name (`filetype::category_of`). Units scale with `chrome.pad`.
pub fn render_framed(tree: &Tree, rects: &[Rect], w: u32, h: u32, chrome: Chrome, theme: Theme) -> Vec<u8> {
    let (wu, hu) = (w as usize, h as usize);
    // Packed premultiplied RGBA (R in the low byte) while drawing: translucent layers blend
    // with one multiply-add per channel pair.
    let mut px = vec![0u32; wu * hu];
    let glass = Glass::of(theme);
    // 1 px at 100%, 2 px at 200%: border, highlight row and tile gap.
    let unit = (chrome.pad / 2.0).round().max(1.0) as usize;
    let edge = |v: f64, max: usize| (v.round().max(0.0) as usize).min(max);
    for r in rects {
        let b = Bounds { x0: edge(r.x, wu), x1: edge(r.x + r.w, wu), y0: edge(r.y, hu), y1: edge(r.y + r.h, hu) };
        if b.x0 >= b.x1 || b.y0 >= b.y1 {
            continue;
        }
        let side = (b.x1 - b.x0).min(b.y1 - b.y0) as f64;
        let radius = if chrome.pad > 0.0 && side >= 6.0 * chrome.pad { 2.0 * chrome.pad } else { 0.0 };
        if tree.node(r.node).is_dir {
            draw_frame(&mut px, wu, &b, radius, unit, &glass);
        } else {
            let c = filetype::colour(filetype::category_of(tree.name(r.node)), theme);
            draw_tile(&mut px, wu, &b, radius, unit, c);
        }
    }
    // Back to straight alpha: c * 255 / a, through a 16.16 reciprocal per alpha.
    let recip: Vec<u32> = (0..256u32).map(|a| (255u32 << 16).checked_div(a).unwrap_or(0)).collect();
    let mut out = Vec::with_capacity(wu * hu * 4);
    for &p in &px {
        let mut c = p.to_le_bytes();
        if c[3] > 0 && c[3] < 255 {
            let r = recip[usize::from(c[3])];
            for v in &mut c[..3] {
                *v = ((u32::from(*v) * r + 0x8000) >> 16).min(255) as u8;
            }
        }
        out.extend_from_slice(&c);
    }
    out
}

/// Glass tokens of one theme, packed premultiplied RGBA.
struct Glass {
    fill: u32,
    border: u32,
    /// The fill with the highlight over it: the first row inside the border.
    lit: u32,
}

impl Glass {
    fn of(theme: Theme) -> Glass {
        // Straight (grey level, alpha). Dark: white glass as in the mockup (style.html option B).
        // Light: a white fill does not show over light Mica, so the fill is a faint shade and
        // the border a dark line (the P1 spike found white 0.45 / black 0.14 too faint).
        let (fill, border, highlight) = match theme {
            Theme::Dark => ((255, 0.045), (255, 0.18), (255, 0.12)),
            Theme::Light => ((0, 0.04), (0, 0.30), (255, 0.70)),
        };
        let pm = |(c, a): (u8, f64)| {
            let v = (f64::from(c) * a).round() as u8;
            u32::from_le_bytes([v, v, v, (255.0 * a).round() as u8])
        };
        let (fill, border) = (pm(fill), pm(border));
        Glass { fill, border, lit: over(fill, pm(highlight)) }
    }
}

/// Pixel bounds `[x0, x1) x [y0, y1)`.
#[derive(Clone, Copy)]
struct Bounds {
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
}

impl Bounds {
    /// Shrunk by `d` on every side; empty if it is too small.
    fn inset(&self, d: usize) -> Bounds {
        let (x0, y0) = (self.x0 + d, self.y0 + d);
        Bounds { x0, x1: self.x1.saturating_sub(d).max(x0), y0, y1: self.y1.saturating_sub(d).max(y0) }
    }

    fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    /// Share of pixel `(x, y)` inside these bounds with corners rounded to `radius`, 0..=1.
    fn coverage(&self, x: usize, y: usize, radius: f64) -> f64 {
        if !self.contains(x, y) {
            return 0.0;
        }
        let (cx, cy) = (x as f64 + 0.5, y as f64 + 0.5);
        let (l, t) = (self.x0 as f64 + radius, self.y0 as f64 + radius);
        let (r, b) = (self.x1 as f64 - radius, self.y1 as f64 - radius);
        let dx = if cx < l { l - cx } else if cx > r { cx - r } else { return 1.0 };
        let dy = if cy < t { t - cy } else if cy > b { cy - b } else { return 1.0 };
        (radius - dx.hypot(dy) + 0.5).clamp(0.0, 1.0)
    }
}

/// Packed premultiplied `src` over `dst`: `src + dst * (255 - src alpha) / 255` per channel,
/// two channels at a time. No channel overflows: premultiplied channels never exceed alpha.
fn over(dst: u32, src: u32) -> u32 {
    let k = 255 - (src >> 24);
    let div = |v: u32| {
        let v = v + 0x0080_0080;
        ((v + ((v >> 8) & 0x00FF_00FF)) >> 8) & 0x00FF_00FF
    };
    src + (div((dst & 0x00FF_00FF) * k) | (div(((dst >> 8) & 0x00FF_00FF) * k) << 8))
}

/// Packed premultiplied `c` with its coverage scaled by `share`.
fn scale(c: u32, share: f64) -> u32 {
    u32::from_le_bytes(c.to_le_bytes().map(|v| (f64::from(v) * share).round() as u8))
}

/// A glass frame over what is below it: the border ring, then the fill with its highlight row.
fn draw_frame(px: &mut [u32], wu: usize, b: &Bounds, radius: f64, unit: usize, g: &Glass) {
    let inner = b.inset(unit);
    let inner_radius = (radius - unit as f64).max(0.0);
    // Pixels at least this far from every side are plain fill: no border, no corner.
    let e = (radius.ceil() as usize).max(unit);
    let (core0, core1) = (b.x0 + e, b.x1.saturating_sub(e));
    for y in b.y0..b.y1 {
        let fill = if y == inner.y0 { g.lit } else { g.fill };
        let row = &mut px[y * wu..(y + 1) * wu];
        let core_row = y >= b.y0 + e && y + e < b.y1 && core0 < core1;
        let mut x = b.x0;
        while x < b.x1 {
            if core_row && x == core0 {
                for p in &mut row[core0..core1] {
                    *p = over(*p, fill);
                }
                x = core1;
                continue;
            }
            let (co, ci) = (b.coverage(x, y, radius), inner.coverage(x, y, inner_radius));
            let src = over(scale(g.border, co - ci), scale(fill, ci));
            row[x] = over(row[x], src);
            x += 1;
        }
    }
}

/// An opaque tile in colour `c`, lit from the top, with a `unit` gap on its right and bottom
/// when it has room. Rounded tiles also get a light ring.
fn draw_tile(px: &mut [u32], wu: usize, b: &Bounds, radius: f64, unit: usize, c: [u8; 3]) {
    let gap = |lo: usize, hi: usize| if hi - lo >= 3 * unit { hi - unit } else { hi };
    let t = Bounds { x1: gap(b.x0, b.x1), y1: gap(b.y0, b.y1), ..*b };
    let ring = radius > 0.0;
    let lighten = |c: [u8; 3], a: f64| c.map(|v| (f64::from(v) + (255.0 - f64::from(v)) * a).round() as u8);
    let height = (t.y1 - t.y0) as f64;
    for y in t.y0..t.y1 {
        // White 0.28 at the top edge, gone at 55% of the height.
        let s = ((y - t.y0) as f64 + 0.5) / height;
        let body = lighten(c, 0.28 * (1.0 - s / 0.55).max(0.0));
        let rim = lighten(body, 0.14);
        let (body, rim) = (u32::from_le_bytes([body[0], body[1], body[2], 255]), u32::from_le_bytes([rim[0], rim[1], rim[2], 255]));
        let row = &mut px[y * wu..(y + 1) * wu];
        let edge_row = ring && (y == t.y0 || y + 1 == t.y1);
        let corner_row = (y as f64) < t.y0 as f64 + radius || (y + 1) as f64 > t.y1 as f64 - radius;
        if !corner_row {
            let span = &mut row[t.x0..t.x1];
            span.fill(if edge_row { rim } else { body });
            if ring {
                let n = span.len();
                (span[0], span[n - 1]) = (rim, rim);
            }
            continue;
        }
        for (x, p) in row.iter_mut().enumerate().take(t.x1).skip(t.x0) {
            let col = if edge_row || (ring && (x == t.x0 || x + 1 == t.x1)) { rim } else { body };
            let cov = t.coverage(x, y, radius);
            if cov >= 1.0 {
                *p = col;
            } else if cov > 0.0 {
                *p = over(*p, scale(col, cov));
            }
        }
    }
}

/// Golden-ratio hash of the node id: neighbours get visibly different, mid-bright colours.
fn colour(n: NodeId) -> [u8; 4] {
    let hsh = n.0.wrapping_mul(2_654_435_761);
    [64 + (hsh >> 24) as u8 / 2, 64 + (hsh >> 16) as u8 / 2, 64 + (hsh >> 8) as u8 / 2, 255]
}
