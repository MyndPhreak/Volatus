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
pub fn layout(_tree: &Tree, _root: NodeId, _w: u32, _h: u32) -> Vec<Rect> {
    unimplemented!()
}

/// RGBA pixels, `w * h * 4` bytes.
pub fn render(_rects: &[Rect], _w: u32, _h: u32) -> Vec<u8> {
    unimplemented!()
}
