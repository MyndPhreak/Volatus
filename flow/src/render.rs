//! The graph picture: translucent ribbons under opaque bands, straight RGBA.
//!
//! Drawn in packed premultiplied RGBA (R in the low byte), straight in the output buffer, then
//! turned to straight alpha in place. Each ribbon is filled one pixel column at a time: its top
//! and bottom edges are worked out once per column, so the per-pixel work is one integer blend.

use crate::{Band, Flow, Graph};
use treemap::Theme;

/// The colour of a band that `colours` does not cover: a neutral mid gray.
const MISSING: [u8; 3] = [128, 128, 128];

/// Ribbon opacity of one theme, 0..=255. Dark: the colours glow on the dark background, so a
/// lighter touch keeps the bands in front. Light: pale colours on a light background need more.
fn ribbon_alpha(theme: Theme) -> u32 {
    match theme {
        Theme::Dark => 102,  // 40 %
        Theme::Light => 128, // 50 %
    }
}

/// See [`crate::render`].
pub fn render(graph: &Graph, colours: &[[u8; 3]], w: u32, h: u32, theme: Theme) -> Vec<u8> {
    let (wu, hu) = (w as usize, h as usize);
    let mut px = vec![0u8; wu * hu * 4];
    if wu == 0 || hu == 0 {
        return px;
    }
    let colour = |i: usize| colours.get(i).copied().unwrap_or(MISSING);
    let alpha = ribbon_alpha(theme);
    let mut canvas = Canvas { px: &mut px, wu, hu };
    for f in &graph.flows {
        let (Some(from), Some(to)) = (graph.bands.get(f.from), graph.bands.get(f.to)) else { continue };
        let c = colour(f.to);
        let src = u32::from_le_bytes([c[0], c[1], c[2], 255]);
        canvas.ribbon(from, to, f, mul(src, alpha));
    }
    for (i, b) in graph.bands.iter().enumerate() {
        canvas.band(b, colour(i));
    }
    unpremultiply(&mut px);
    px
}

/// Pixel edge of coordinate `v` in `0..=max`: rounded, as the treemap's `edge`.
fn edge(v: f64, max: usize) -> usize {
    if v.is_nan() { 0 } else { (v.round().max(0.0) as usize).min(max) }
}

struct Canvas<'a> {
    px: &'a mut [u8],
    wu: usize,
    hu: usize,
}

impl Canvas<'_> {
    fn get(&self, i: usize) -> u32 {
        u32::from_le_bytes([self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]])
    }

    fn set(&mut self, i: usize, p: u32) {
        self.px[i..i + 4].copy_from_slice(&p.to_le_bytes());
    }

    /// Blends `src` over pixel byte offset `i`, never to full opacity: only bands are opaque.
    fn blend(&mut self, i: usize, src: u32) {
        let p = over(self.get(i), src);
        self.set(i, if p >> 24 == 255 { u32::from_le_bytes(p.to_le_bytes().map(|v| v.min(254))) } else { p });
    }

    /// An opaque band over its rounded pixel rectangle.
    fn band(&mut self, b: &Band, c: [u8; 3]) {
        let (x0, x1) = (edge(b.x, self.wu), edge(b.x + b.w, self.wu));
        let (y0, y1) = (edge(b.y, self.hu), edge(b.y + b.h, self.hu));
        if x0 >= x1 {
            return;
        }
        let p = [c[0], c[1], c[2], 255];
        for y in y0..y1 {
            for chunk in self.px[(y * self.wu + x0) * 4..(y * self.wu + x1) * 4].as_chunks_mut::<4>().0 {
                *chunk = p;
            }
        }
    }

    /// The ribbon of `f` from the right edge of `from` to the left edge of `to`, in packed
    /// premultiplied `src`. Its top edge runs from `from_y.0` to `to_y.0` and its bottom edge
    /// from `from_y.1` to `to_y.1`, each along the same smoothstep (`3t² - 2t³`): flat where it
    /// leaves and where it arrives, so the ribbon meets both bands square on. Edges are
    /// anti-aliased with a ramp as wide as the edge drops across the pixel column.
    fn ribbon(&mut self, from: &Band, to: &Band, f: &Flow, src: u32) {
        let (xa, xb) = (from.x + from.w, to.x);
        let ys = [f.from_y.0, f.from_y.1, f.to_y.0, f.to_y.1];
        if !(xa.is_finite() && xb.is_finite() && xb > xa) || !ys.iter().all(|v| v.is_finite()) {
            return;
        }
        let (x0, x1) = (edge(xa, self.wu), edge(xb, self.wu));
        let at = |x: usize| {
            let t = ((x as f64 - xa) / (xb - xa)).clamp(0.0, 1.0);
            let s = t * t * (3.0 - 2.0 * t);
            (f.from_y.0 + (f.to_y.0 - f.from_y.0) * s, f.from_y.1 + (f.to_y.1 - f.from_y.1) * s)
        };
        let hf = self.hu as f64;
        let mut left = at(x0);
        for x in x0..x1 {
            let right = at(x + 1);
            // Each edge across this column: its middle and the width of its ramp (at least 1 px).
            let (top_mid, top_ramp) = ((left.0 + right.0) / 2.0, (left.0 - right.0).abs().max(1.0));
            let (bot_mid, bot_ramp) = ((left.1 + right.1) / 2.0, (left.1 - right.1).abs().max(1.0));
            left = right;
            // Coverage of row r: (top part) * (bottom part), both ramps of the row's centre.
            let cover = |r: usize| {
                let c = r as f64 + 0.5;
                let top = ((c - top_mid) / top_ramp + 0.5).clamp(0.0, 1.0);
                let bot = ((bot_mid - c) / bot_ramp + 0.5).clamp(0.0, 1.0);
                (top + bot - 1.0).max(0.0)
            };
            let row = |v: f64| v.clamp(0.0, hf) as usize;
            let start = row((top_mid - top_ramp / 2.0).floor());
            // A ribbon turned upside down (top below bottom) covers nothing.
            let end = row((bot_mid + bot_ramp / 2.0).ceil()).max(start);
            // Rows fully covered: past the top ramp and before the bottom ramp.
            let full0 = row((top_mid + top_ramp / 2.0 - 0.5).ceil()).clamp(start, end);
            let full1 = row((bot_mid - bot_ramp / 2.0 - 0.5).floor() + 1.0).clamp(full0, end);
            let partial = |canvas: &mut Self, r: usize| {
                let k = (cover(r) * 255.0).round() as u32;
                if k > 0 {
                    canvas.blend((r * canvas.wu + x) * 4, mul(src, k));
                }
            };
            for r in start..full0 {
                partial(self, r);
            }
            for r in full0..full1 {
                self.blend((r * self.wu + x) * 4, src);
            }
            for r in full1..end {
                partial(self, r);
            }
        }
    }
}

/// Every channel of packed `c` times `k / 255`, rounded, two channels at a time.
fn mul(c: u32, k: u32) -> u32 {
    let div = |v: u32| {
        let v = v + 0x0080_0080;
        ((v + ((v >> 8) & 0x00FF_00FF)) >> 8) & 0x00FF_00FF
    };
    div((c & 0x00FF_00FF) * k) | (div(((c >> 8) & 0x00FF_00FF) * k) << 8)
}

/// Packed premultiplied `src` over `dst` (as the treemap's `over`). No channel overflows:
/// premultiplied channels never exceed alpha.
fn over(dst: u32, src: u32) -> u32 {
    src + mul(dst, 255 - (src >> 24))
}

/// Premultiplied to straight alpha in place: c * 255 / a, through a 16.16 reciprocal per alpha.
fn unpremultiply(px: &mut [u8]) {
    let recip: Vec<u32> = (0..256u32).map(|a| (255u32 << 16).checked_div(a).unwrap_or(0)).collect();
    for c in px.as_chunks_mut::<4>().0 {
        if c[3] > 0 && c[3] < 255 {
            let r = recip[usize::from(c[3])];
            for v in &mut c[..3] {
                *v = ((u32::from(*v) * r + 0x8000) >> 16).min(255) as u8;
            }
        }
    }
}
