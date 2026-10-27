//! TP3.1: the graph picture.

use flow::{Band, Expander, Flow, Graph, Part, render};
use scan::NodeId;
use treemap::Theme;

const W: u32 = 300;
const H: u32 = 200;
const PARENT: [u8; 3] = [200, 40, 30];
const UPPER: [u8; 3] = [30, 160, 220];
const LOWER: [u8; 3] = [60, 200, 90];

fn band(x: f64, y: f64, w: f64, h: f64) -> Band {
    Band { part: Part::Folder(NodeId(0)), column: 0, x, y, w, h, size: 1, newest: 0, expander: Expander::None }
}

/// A parent band (0) at the left flowing into two children (1 above 2) at the right.
fn fixture() -> Graph {
    let bands = vec![band(10.0, 20.0, 12.0, 100.0), band(200.0, 50.0, 12.0, 60.0), band(200.0, 150.0, 12.0, 30.0)];
    let flows = vec![
        Flow { from: 0, to: 1, from_y: (20.0, 80.0), to_y: (50.0, 110.0) },
        Flow { from: 0, to: 2, from_y: (80.0, 120.0), to_y: (150.0, 180.0) },
    ];
    Graph { bands, flows, columns: 2, first_column: 0, px_per_byte: 1.0 }
}

fn colours() -> Vec<[u8; 3]> {
    vec![PARENT, UPPER, LOWER]
}

fn pixel(px: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = (y as usize * w as usize + x as usize) * 4;
    [px[i], px[i + 1], px[i + 2], px[i + 3]]
}

fn opaque(c: [u8; 3]) -> [u8; 4] {
    [c[0], c[1], c[2], 255]
}

#[test]
fn render_empty_graph_is_all_zero_bytes() {
    for theme in [Theme::Dark, Theme::Light] {
        let px = render(&Graph::default(), &[], 40, 30, theme);
        assert_eq!(px.len(), 40 * 30 * 4);
        assert!(px.iter().all(|&b| b == 0));
    }
}

#[test]
fn render_band_centre_is_its_colour_opaque() {
    for theme in [Theme::Dark, Theme::Light] {
        let px = render(&fixture(), &colours(), W, H, theme);
        assert_eq!(px.len(), (W * H * 4) as usize);
        assert_eq!(pixel(&px, W, 16, 70), opaque(PARENT));
        assert_eq!(pixel(&px, W, 206, 80), opaque(UPPER));
        assert_eq!(pixel(&px, W, 206, 165), opaque(LOWER));
    }
}

#[test]
fn render_mid_flow_pixel_is_the_child_colour_translucent() {
    for theme in [Theme::Dark, Theme::Light] {
        let px = render(&fixture(), &colours(), W, H, theme);
        // Half way across the upper ribbon: from (20, 80) to (50, 110), so (35, 95) there.
        let [r, g, b, a] = pixel(&px, W, 111, 65);
        assert!((1..=254).contains(&a), "alpha {a}");
        for (got, want) in [r, g, b].into_iter().zip(UPPER) {
            assert!(got.abs_diff(want) <= 3, "{:?} is not near {UPPER:?}", [r, g, b]);
        }
        // The lower ribbon at its middle: (100, 165).
        let [r, g, b, a] = pixel(&px, W, 111, 132);
        assert!((1..=254).contains(&a), "alpha {a}");
        for (got, want) in [r, g, b].into_iter().zip(LOWER) {
            assert!(got.abs_diff(want) <= 3, "{:?} is not near {LOWER:?}", [r, g, b]);
        }
    }
}

#[test]
fn render_gap_without_flow_is_transparent() {
    let px = render(&fixture(), &colours(), W, H, Theme::Dark);
    // Between the two child bands, and above and below everything.
    assert_eq!(pixel(&px, W, 206, 130)[3], 0);
    assert_eq!(pixel(&px, W, 111, 5)[3], 0);
    assert_eq!(pixel(&px, W, 111, 195)[3], 0);
    // Right of the children: nothing flows out of them.
    assert_eq!(pixel(&px, W, 260, 80)[3], 0);
}

#[test]
fn render_band_edges_follow_the_rounded_rectangle() {
    // x 10.4..22.4 -> pixels 10..22, y 20.4..120.4 -> pixels 20..120 (the treemap's rounding).
    let g = Graph { bands: vec![band(10.4, 20.4, 12.0, 100.0)], ..Graph::default() };
    let px = render(&g, &[PARENT], W, H, Theme::Light);
    assert_eq!(pixel(&px, W, 10, 20), opaque(PARENT), "top left inside");
    assert_eq!(pixel(&px, W, 21, 119), opaque(PARENT), "bottom right inside");
    assert_ne!(pixel(&px, W, 22, 70), opaque(PARENT), "right of the right edge");
    assert_ne!(pixel(&px, W, 16, 120), opaque(PARENT), "below the bottom edge");
    assert_ne!(pixel(&px, W, 9, 70), opaque(PARENT), "left of the left edge");
    assert_ne!(pixel(&px, W, 16, 19), opaque(PARENT), "above the top edge");
}

#[test]
fn render_band_edges_with_a_flow_out_of_them() {
    let px = render(&fixture(), &colours(), W, H, Theme::Dark);
    assert_eq!(pixel(&px, W, 10, 20), opaque(PARENT));
    assert_eq!(pixel(&px, W, 21, 119), opaque(PARENT));
    // Just right of the parent is the ribbon, not the band.
    let right = pixel(&px, W, 22, 50);
    assert!((1..=254).contains(&right[3]), "{right:?}");
    // The child's left column is the child, not the ribbon under it.
    assert_eq!(pixel(&px, W, 200, 80), opaque(UPPER));
    assert_ne!(pixel(&px, W, 16, 120), opaque(PARENT));
}

#[test]
fn render_overlapping_ribbons_never_reach_opaque() {
    // Fifty ribbons over the same span, and fifty more crossing them.
    let mut g = fixture();
    let one = g.flows[0];
    let cross = Flow { from: 0, to: 2, from_y: (20.0, 80.0), to_y: (150.0, 180.0) };
    g.flows = std::iter::repeat_n(one, 50).chain(std::iter::repeat_n(cross, 50)).collect();
    for theme in [Theme::Dark, Theme::Light] {
        let px = render(&g, &colours(), W, H, theme);
        assert_eq!(pixel(&px, W, 16, 70), opaque(PARENT), "the band stays on top");
        let mid = pixel(&px, W, 111, 65)[3];
        assert!((1..=254).contains(&mid), "alpha {mid}");
        // Outside the bands, no pixel is opaque.
        let inside = |x: u32, y: u32| g.bands.iter().any(|b| f64::from(x) >= b.x && f64::from(x) < b.x + b.w && f64::from(y) >= b.y && f64::from(y) < b.y + b.h);
        for y in 0..H {
            for x in 0..W {
                if !inside(x, y) {
                    assert!(pixel(&px, W, x, y)[3] <= 254, "({x}, {y}) is opaque");
                }
            }
        }
    }
}

#[test]
fn render_with_too_few_colours_draws_gray() {
    for colours in [vec![], vec![PARENT]] {
        let px = render(&fixture(), &colours, W, H, Theme::Dark);
        let [r, g, b, a] = pixel(&px, W, 206, 80);
        assert_eq!(a, 255);
        assert!(r == g && g == b, "{:?} is not gray", [r, g, b]);
    }
}

#[test]
fn render_tiny_pictures_do_not_panic() {
    assert_eq!(render(&fixture(), &colours(), 1, 1, Theme::Dark).len(), 4);
    assert!(render(&fixture(), &colours(), 0, 0, Theme::Dark).is_empty());
    assert!(render(&fixture(), &colours(), 0, 10, Theme::Light).is_empty());
    assert!(render(&fixture(), &colours(), 10, 0, Theme::Light).is_empty());
    // The fixture's bands are all to the right of a 1x1 picture: it stays transparent.
    assert_eq!(render(&fixture(), &colours(), 1, 1, Theme::Dark), vec![0; 4]);
}

#[test]
fn render_is_deterministic() {
    for theme in [Theme::Dark, Theme::Light] {
        let a = render(&fixture(), &colours(), W, H, theme);
        let b = render(&fixture(), &colours(), W, H, theme);
        assert_eq!(a, b);
    }
}

#[test]
fn render_clips_a_band_partly_outside_the_picture() {
    let g = Graph {
        bands: vec![band(-5.0, -10.0, 12.0, 1000.0), band(40.0, 30.0, 30.0, 40.0)],
        flows: vec![Flow { from: 0, to: 1, from_y: (-10.0, 990.0), to_y: (30.0, 70.0) }],
        ..Graph::default()
    };
    let px = render(&g, &[PARENT, UPPER], 50, 50, Theme::Dark);
    assert_eq!(px.len(), 50 * 50 * 4);
    assert_eq!(pixel(&px, 50, 0, 0), opaque(PARENT));
    assert_eq!(pixel(&px, 50, 6, 49), opaque(PARENT));
    assert_eq!(pixel(&px, 50, 49, 40), opaque(UPPER));
    let mid = pixel(&px, 50, 20, 40)[3];
    assert!((1..=254).contains(&mid), "alpha {mid}");
}
