//! Keyboard navigation logic for the file tree and the treemap. Plain functions, no Slint
//! types, so it is unit tested. The window maps key presses to these calls.

use crate::tree_view::Row;
use scan::NodeId;
use treemap::Rect;

/// What a Left or Right key does in the file tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeMove {
    /// Collapse or expand the selected folder.
    Toggle,
    /// Select the row at this index.
    Select(usize),
    /// The key has no effect.
    Nothing,
}

/// Rect edges closer than this (in pixels) count as touching.
const EDGE_TOLERANCE: f64 = 1e-6;

/// A direction key in the treemap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// Row after moving `delta` rows from `current`, stopped at the first and last row.
/// With no current row, any move selects the first row. `None` only if there are no rows.
pub fn step_row(len: usize, current: Option<usize>, delta: isize) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(current.map_or(0, |c| c.saturating_add_signed(delta).min(last)))
}

/// Left key on row `i`: collapse an expanded folder, otherwise go to the parent row.
pub fn left_action(rows: &[Row], i: usize) -> TreeMove {
    let Some(row) = rows.get(i) else { return TreeMove::Nothing };
    if row.expanded {
        return TreeMove::Toggle;
    }
    // The parent is the closest row above with a smaller depth.
    rows[..i].iter().rposition(|r| r.depth < row.depth).map_or(TreeMove::Nothing, TreeMove::Select)
}

/// Right key on row `i`: expand a collapsed folder, go to the first child of an expanded one.
pub fn right_action(rows: &[Row], i: usize) -> TreeMove {
    match rows.get(i) {
        Some(r) if r.expanded => TreeMove::Select(i + 1),
        Some(r) if r.has_children => TreeMove::Toggle,
        _ => TreeMove::Nothing,
    }
}

/// The rect next to `selected` in direction `dir`. Rects that overlap the selected one (its
/// folders and its contents) are not neighbours. With no selection, or a selection that is not
/// drawn, the first rect is chosen. `None` if nothing lies in that direction.
pub fn neighbour_rect(rects: &[Rect], selected: Option<NodeId>, dir: Dir) -> Option<NodeId> {
    let Some(sel) = selected.and_then(|n| rects.iter().find(|r| r.node == n)) else {
        return rects.first().map(|r| r.node);
    };
    let (sx, sy) = (sel.x + sel.w / 2.0, sel.y + sel.h / 2.0);
    let overlaps = |r: &Rect| r.x < sel.x + sel.w && r.x + r.w > sel.x && r.y < sel.y + sel.h && r.y + r.h > sel.y;
    // Gap in the key direction (None if the rect lies the other way) plus the miss across it.
    let score = |r: &Rect| {
        let gap = match dir {
            Dir::Right => r.x - (sel.x + sel.w),
            Dir::Left => sel.x - (r.x + r.w),
            Dir::Down => r.y - (sel.y + sel.h),
            Dir::Up => sel.y - (r.y + r.h),
        };
        let across = match dir {
            Dir::Left | Dir::Right => (r.y + r.h / 2.0 - sy).abs(),
            Dir::Up | Dir::Down => (r.x + r.w / 2.0 - sx).abs(),
        };
        (gap >= -EDGE_TOLERANCE).then(|| gap.max(0.0) + 2.0 * across)
    };
    rects
        .iter()
        .filter(|r| r.node != sel.node && !overlaps(r))
        .filter_map(|r| score(r).map(|s| (r.node, s)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(n, _)| n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u32, depth: u32, expanded: bool, has_children: bool) -> Row {
        Row { node: NodeId(id), depth, expanded, has_children }
    }

    // root(0) expanded; a(1) expanded { a1(2) file, sub(3) collapsed folder }; c(4) file
    fn rows() -> Vec<Row> {
        vec![
            row(0, 0, true, true),
            row(1, 1, true, true),
            row(2, 2, false, false),
            row(3, 2, false, true),
            row(4, 1, false, false),
        ]
    }

    fn rect(id: u32, x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { node: NodeId(id), x, y, w, h }
    }

    // A(1) B(2)
    // C(3) D(4)
    fn grid() -> Vec<Rect> {
        vec![
            rect(1, 0.0, 0.0, 50.0, 50.0),
            rect(2, 50.0, 0.0, 50.0, 50.0),
            rect(3, 0.0, 50.0, 50.0, 50.0),
            rect(4, 50.0, 50.0, 50.0, 50.0),
        ]
    }

    // ---- step_row ----
    #[test]
    fn step_row_no_rows_gives_none() {
        assert_eq!(step_row(0, None, 1), None);
        assert_eq!(step_row(0, Some(0), -1), None);
    }

    #[test]
    fn step_row_no_current_selects_first_row_for_both_directions() {
        assert_eq!(step_row(5, None, 1), Some(0));
        assert_eq!(step_row(5, None, -1), Some(0));
    }

    #[test]
    fn step_row_middle_moves_one_row() {
        assert_eq!(step_row(5, Some(2), 1), Some(3));
        assert_eq!(step_row(5, Some(2), -1), Some(1));
    }

    #[test]
    fn step_row_stops_at_both_ends() {
        assert_eq!(step_row(5, Some(4), 1), Some(4));
        assert_eq!(step_row(5, Some(0), -1), Some(0));
    }

    #[test]
    fn step_row_current_past_end_after_rows_shrink_is_clamped() {
        assert_eq!(step_row(3, Some(9), 1), Some(2));
    }

    // ---- left_action ----
    #[test]
    fn left_action_expanded_folder_collapses() {
        assert_eq!(left_action(&rows(), 1), TreeMove::Toggle);
        assert_eq!(left_action(&rows(), 0), TreeMove::Toggle);
    }

    #[test]
    fn left_action_file_goes_to_parent_row() {
        assert_eq!(left_action(&rows(), 2), TreeMove::Select(1));
        assert_eq!(left_action(&rows(), 4), TreeMove::Select(0));
    }

    #[test]
    fn left_action_collapsed_folder_goes_to_parent_row() {
        assert_eq!(left_action(&rows(), 3), TreeMove::Select(1));
    }

    #[test]
    fn left_action_collapsed_root_does_nothing() {
        let rows = vec![row(0, 0, false, true)];
        assert_eq!(left_action(&rows, 0), TreeMove::Nothing);
    }

    #[test]
    fn left_action_row_out_of_range_does_nothing() {
        assert_eq!(left_action(&rows(), 99), TreeMove::Nothing);
    }

    // ---- right_action ----
    #[test]
    fn right_action_collapsed_folder_expands() {
        assert_eq!(right_action(&rows(), 3), TreeMove::Toggle);
    }

    #[test]
    fn right_action_expanded_folder_goes_to_first_child() {
        assert_eq!(right_action(&rows(), 1), TreeMove::Select(2));
        assert_eq!(right_action(&rows(), 0), TreeMove::Select(1));
    }

    #[test]
    fn right_action_file_does_nothing() {
        assert_eq!(right_action(&rows(), 2), TreeMove::Nothing);
    }

    #[test]
    fn right_action_row_out_of_range_does_nothing() {
        assert_eq!(right_action(&rows(), 99), TreeMove::Nothing);
    }

    // ---- neighbour_rect ----
    #[test]
    fn neighbour_rect_grid_all_moves_from_top_left() {
        let r = grid();
        assert_eq!(neighbour_rect(&r, Some(NodeId(1)), Dir::Right), Some(NodeId(2)));
        assert_eq!(neighbour_rect(&r, Some(NodeId(1)), Dir::Down), Some(NodeId(3)));
        assert_eq!(neighbour_rect(&r, Some(NodeId(1)), Dir::Left), None);
        assert_eq!(neighbour_rect(&r, Some(NodeId(1)), Dir::Up), None);
    }

    #[test]
    fn neighbour_rect_grid_all_moves_from_bottom_right() {
        let r = grid();
        assert_eq!(neighbour_rect(&r, Some(NodeId(4)), Dir::Left), Some(NodeId(3)));
        assert_eq!(neighbour_rect(&r, Some(NodeId(4)), Dir::Up), Some(NodeId(2)));
        assert_eq!(neighbour_rect(&r, Some(NodeId(4)), Dir::Right), None);
        assert_eq!(neighbour_rect(&r, Some(NodeId(4)), Dir::Down), None);
    }

    #[test]
    fn neighbour_rect_prefers_the_rect_in_line_with_the_selection() {
        // From A (y 0..50): B is above the centre line, C is on it.
        let r = vec![rect(1, 0.0, 0.0, 50.0, 50.0), rect(2, 50.0, 0.0, 50.0, 10.0), rect(3, 50.0, 20.0, 50.0, 10.0)];
        assert_eq!(neighbour_rect(&r, Some(NodeId(1)), Dir::Right), Some(NodeId(3)));
    }

    #[test]
    fn neighbour_rect_prefers_the_nearer_rect_over_a_farther_one_in_line() {
        let r = vec![rect(1, 0.0, 0.0, 50.0, 50.0), rect(2, 50.0, 0.0, 50.0, 50.0), rect(3, 100.0, 0.0, 50.0, 50.0)];
        assert_eq!(neighbour_rect(&r, Some(NodeId(1)), Dir::Right), Some(NodeId(2)));
    }

    #[test]
    fn neighbour_rect_ignores_the_folder_that_holds_the_selection() {
        // Folder 5 is listed before the files inside it.
        let mut r = vec![rect(5, 0.0, 0.0, 100.0, 100.0)];
        r.extend(grid());
        assert_eq!(neighbour_rect(&r, Some(NodeId(1)), Dir::Right), Some(NodeId(2)));
        assert_eq!(neighbour_rect(&r, Some(NodeId(1)), Dir::Left), None);
    }

    #[test]
    fn neighbour_rect_ignores_the_contents_of_the_selected_folder() {
        let mut r = vec![rect(5, 0.0, 0.0, 100.0, 100.0)];
        r.extend(grid());
        assert_eq!(neighbour_rect(&r, Some(NodeId(5)), Dir::Right), None);
    }

    #[test]
    fn neighbour_rect_no_selection_gives_first_rect() {
        assert_eq!(neighbour_rect(&grid(), None, Dir::Down), Some(NodeId(1)));
    }

    #[test]
    fn neighbour_rect_selection_not_drawn_gives_first_rect() {
        assert_eq!(neighbour_rect(&grid(), Some(NodeId(99)), Dir::Left), Some(NodeId(1)));
    }

    #[test]
    fn neighbour_rect_no_rects_gives_none() {
        assert_eq!(neighbour_rect(&[], Some(NodeId(1)), Dir::Right), None);
        assert_eq!(neighbour_rect(&[], None, Dir::Right), None);
    }
}
