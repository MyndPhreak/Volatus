//! Folder flow graph: pure layout and drawing, no window code.

pub mod age;
mod layout;
mod render;
mod split;
mod types;

use std::collections::HashSet;

use scan::{NodeId, Tree};
pub use layout::{folder_rects, hit, label_spots, layout};
pub use split::{members, split};
pub use types::dominant;
pub use treemap::Theme;

/// A group merges into "other" below this share of its parent, in percent.
pub const MERGE_PERCENT: u64 = 3;
/// A band merges into "other" below this height, in logical px.
pub const MERGE_PX: f64 = 6.0;
/// Columns shown at once.
pub const VISIBLE_COLUMNS: u16 = 4;
/// Band width, in logical px.
pub const BAND_W: f64 = 12.0;

/// What a band stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Part {
    Folder(NodeId),
    Files(NodeId),
    /// The merged folders (the smallest `folders` of the parent's sized subfolders) and, when
    /// `files`, the parent's direct files. Valid only for the tree it was split from: re-split
    /// after the tree changes.
    Other { parent: NodeId, folders: u32, files: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expander {
    None,
    Collapsed,
    Expanded,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub part: Part,
    pub column: u16,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub size: u64,
    pub newest: u32,
    pub expander: Expander,
}

/// A ribbon from one band to a band in the next column.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flow {
    pub from: usize,
    pub to: usize,
    pub from_y: (f64, f64),
    pub to_y: (f64, f64),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Graph {
    pub bands: Vec<Band>,
    pub flows: Vec<Flow>,
    pub columns: u16,
    pub first_column: u16,
    pub px_per_byte: f64,
}

pub struct Input<'a> {
    pub tree: &'a Tree,
    pub view: NodeId,
    pub expanded: &'a HashSet<NodeId>,
    pub open_other: &'a HashSet<NodeId>,
    pub w: f64,
    pub h: f64,
    pub scale: f64,
    pub first_column: u16,
}

/// One group's split: shown folders, the "files" size, and the "other" group.
pub struct Split {
    pub folders: Vec<NodeId>,
    pub files: Option<u64>,
    /// (folders merged, files merged, bytes)
    pub other: Option<(u32, bool, u64)>,
}

/// Straight RGBA picture of the graph, `w * h * 4` bytes, transparent background. Graph
/// coordinates are pixels.
///
/// `colours[i]` is the colour of `graph.bands[i]`; a band past the end of `colours` is a neutral
/// gray. Each flow is a smooth ribbon from its parent band's right edge to its child band's left
/// edge, in the child's colour at partial alpha (the theme sets how much); overlapping ribbons
/// add up but never reach full opacity. Bands are opaque, drawn over the ribbons, and cover
/// their rectangle rounded to whole pixels as the treemap's tiles do. Anything outside the
/// picture is clipped.
pub fn render(graph: &Graph, colours: &[[u8; 3]], w: u32, h: u32, theme: Theme) -> Vec<u8> {
    render::render(graph, colours, w, h, theme)
}
