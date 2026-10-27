//! The folder flow graph's window-free logic: its state, clicks and keys, colours, texts and
//! the choice of the view to show. Pure; the window code lives elsewhere.
// Most of this is wired in later tasks (P3.T3, P4.T1, P4.T2).
#![cfg_attr(not(test), expect(dead_code, reason = "wired in a later task"))]
#![cfg_attr(test, allow(dead_code))]

use crate::model::{ScanState, Unit};
use crate::settings::View;
use flow::age::Age;
use flow::{Band, Graph, Part, Theme};
use scan::{NodeId, Tree};
use std::collections::{HashMap, HashSet};
use treemap::filetype::Category;

/// What colours the bands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColourMode {
    #[default]
    Age,
    FileType,
}

/// The graph's state in one session.
#[derive(Default)]
pub struct GraphState {
    pub expanded: HashSet<NodeId>,
    pub open_other: HashSet<NodeId>,
    pub first_column: u16,
    pub colour: ColourMode,
    pub shown: Option<Graph>,
    pub types: HashMap<Part, Option<Category>>,
}

impl GraphState {
    pub fn toggle(&mut self, _n: NodeId) {}
    pub fn show_other(&mut self, _parent: NodeId) {}
    pub fn reroot(&mut self) {}
    pub fn after_detach(&mut self, _inside: impl Fn(NodeId) -> bool) {}
    pub fn scroll_to(&mut self, _column: u16, _columns: u16) {}
    pub fn scroll_by(&mut self, _delta: i32, _columns: u16) {}
}

/// What a click on the graph does.
pub enum Act {
    Select(NodeId),
    ShowOther(NodeId),
    Nothing,
}

/// What a key in the graph does.
pub enum KeyAct {
    Select(NodeId),
    Toggle(NodeId),
    Open(NodeId),
    Up,
    Delete { node: NodeId, shift: bool },
    Nothing,
}

pub fn click(_g: &Graph, _hit: Option<usize>) -> Act {
    Act::Nothing
}

pub fn menu_target(_g: &Graph, _hit: Option<usize>) -> Option<NodeId> {
    None
}

pub fn open_target(_g: &Graph, _hit: Option<usize>) -> Option<NodeId> {
    None
}

pub fn key_target(_g: &Graph, _selected: Option<NodeId>) -> Option<NodeId> {
    None
}

pub fn key(_g: &Graph, _tree: &Tree, _selected: Option<NodeId>, _key: &str) -> KeyAct {
    KeyAct::Nothing
}

pub fn colours(
    _g: &Graph,
    _mode: ColourMode,
    _now: u32,
    _theme: Theme,
    _types: &HashMap<Part, Option<Category>>,
) -> Vec<[u8; 3]> {
    Vec::new()
}

pub fn missing_types(_g: &Graph, _types: &HashMap<Part, Option<Category>>) -> Vec<Part> {
    Vec::new()
}

pub fn band_label(_tree: &Tree, _b: &Band, _unit: Unit) -> (String, String) {
    (String::new(), String::new())
}

pub fn tooltip(_tree: &Tree, _counts: &[u64], _b: &Band, _unit: Unit) -> (String, String) {
    (String::new(), String::new())
}

pub fn summary(_tree: &Tree, _g: &Graph, _unit: Unit) -> String {
    String::new()
}

/// The text the graph shows over `g`, the layout of folder `view`, or "" for none: the treemap's
/// `NO_SIZES` when the folder has no file with a size. A graph with no area shows no text.
pub fn graph_note(_tree: &Tree, _view: NodeId, _g: &Graph, _has_area: bool) -> &'static str {
    ""
}

/// A day as `2026-10-26`.
pub fn format_day(_t: u32) -> String {
    String::new()
}

/// The legend of the age colours, newest first.
pub const AGE_LEGEND: [(Age, &str); 4] =
    [(Age::Recent, ""), (Age::Middle, ""), (Age::Old, ""), (Age::Unknown, "")];

/// The view to show: the graph only for a finished scan with data and when it is chosen.
pub fn shown_view(state: &ScanState, loaded: bool, chosen: View) -> View {
    if loaded && matches!(state, ScanState::Done(_)) { chosen } else { View::Treemap }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Summary;
    use flow::age::{age_colour, age_of};
    use scan::rollup::{RawEntry, rollup};
    use scan::{Method, Phases};
    use std::time::Duration;
    use treemap::filetype;

    const NOW: u32 = 846_288_000;
    const D: u32 = 86_400;

    /// root { a { a1.mp4 600 at NOW }, b { b1.zip 300 at NOW - 400 D }, c.txt 100 unknown, e {} }
    fn tree() -> Tree {
        let e = |id: u64, parent: u64, name: &str, size: u64, is_dir: bool, modified: u32| RawEntry {
            id,
            parent,
            name: name.into(),
            size,
            allocated: size,
            is_dir,
            modified,
        };
        let entries = vec![
            e(0, 0, "root", 0, true, 0),
            e(1, 0, "a", 0, true, 0),
            e(2, 0, "b", 0, true, 0),
            e(3, 0, "c.txt", 100, false, 0),
            e(4, 1, "a1.mp4", 600, false, NOW),
            e(5, 2, "b1.zip", 300, false, NOW - 400 * D),
            e(6, 0, "e", 0, true, 0),
        ];
        rollup(0, entries).unwrap().tree
    }

    fn layout_of(t: &Tree, view: NodeId) -> Graph {
        let (expanded, open_other) = (HashSet::new(), HashSet::new());
        flow::layout(&flow::Input {
            tree: t,
            view,
            expanded: &expanded,
            open_other: &open_other,
            w: 800.0,
            h: 600.0,
            scale: 1.0,
            first_column: 0,
        })
    }

    #[test]
    fn colours_age_mode_is_the_age_colour_of_each_band() {
        let t = tree();
        let g = layout_of(&t, t.root());
        assert!(g.bands.len() >= 3, "{g:?}");
        for theme in [Theme::Light, Theme::Dark] {
            let want: Vec<[u8; 3]> = g.bands.iter().map(|b| age_colour(age_of(b.newest, NOW), theme)).collect();
            assert_eq!(colours(&g, ColourMode::Age, NOW, theme, &HashMap::new()), want, "{theme:?}");
        }
    }

    #[test]
    fn colours_file_type_with_a_cached_type_is_its_tile_colour_and_a_miss_is_other() {
        let t = tree();
        let g = layout_of(&t, t.root());
        let first = g.bands[0].part;
        let types = HashMap::from([(first, Some(Category::Video))]);
        let theme = Theme::Dark;

        let got = colours(&g, ColourMode::FileType, NOW, theme, &types);

        let mut want = vec![filetype::colour(Category::Other, theme); g.bands.len()];
        want[0] = filetype::colour(Category::Video, theme);
        assert_eq!(got, want);
        let missing: Vec<Part> = g.bands[1..].iter().map(|b| b.part).collect();
        assert_eq!(missing_types(&g, &types), missing);
    }

    #[test]
    fn colours_file_type_known_to_have_no_bytes_is_other_and_not_missing() {
        let t = tree();
        let g = layout_of(&t, t.root());
        let types: HashMap<Part, Option<Category>> = g.bands.iter().map(|b| (b.part, None)).collect();
        let got = colours(&g, ColourMode::FileType, NOW, Theme::Light, &types);
        assert_eq!(got, vec![filetype::colour(Category::Other, Theme::Light); g.bands.len()]);
        assert!(missing_types(&g, &types).is_empty());
    }

    #[test]
    fn graph_note_zero_size_view_is_no_sizes() {
        let t = tree();
        let empty = t.children(t.root()).find(|&n| t.name(n) == "e").unwrap();
        let g = layout_of(&t, empty);
        assert!(g.bands.is_empty());
        assert_eq!(graph_note(&t, empty, &g, true), crate::tree_view::NO_SIZES);
        // A graph with bands, or with no area, shows no text.
        let full = layout_of(&t, t.root());
        assert_eq!(graph_note(&t, t.root(), &full, true), "");
        assert_eq!(graph_note(&t, empty, &g, false), "");
    }

    fn done() -> ScanState {
        ScanState::Done(Summary {
            method: Method::Walk,
            files: 1,
            dirs: 1,
            skipped: 0,
            elapsed: Duration::ZERO,
            phases: Phases::default(),
            total: 1,
        })
    }

    #[test]
    fn shown_view_table() {
        let states = [
            ScanState::Idle,
            ScanState::Scanning { files: 1, dirs: 1 },
            done(),
            ScanState::Error("x".into()),
            ScanState::Cancelled,
        ];
        for state in &states {
            for chosen in [View::Treemap, View::Graph] {
                let want = if matches!(state, ScanState::Done(_)) && chosen == View::Graph {
                    View::Graph
                } else {
                    View::Treemap
                };
                assert_eq!(shown_view(state, true, chosen), want, "{state:?} loaded {chosen:?}");
            }
        }
        for chosen in [View::Treemap, View::Graph] {
            assert_eq!(shown_view(&done(), false, chosen), View::Treemap, "done without data {chosen:?}");
        }
    }
}
