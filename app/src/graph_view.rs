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
    use scan::{Method, Phases};
    use std::time::Duration;

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
