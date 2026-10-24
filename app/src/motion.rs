//! When the treemap picture moves and how: pure decisions. The window code holds the timer and
//! the pictures and does what these functions say.

use scan::NodeId;
use treemap::Frame;

/// A move of the treemap picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The old picture zooms into the rect of the opened folder until that rect fills the map.
    In,
    /// The old picture shrinks into the rect of its folder in the new picture and fades out.
    Out,
    /// The first picture of a scan fades in.
    Fade,
}

/// Motion is on: the Windows setting "Animation effects" (`SPI_GETCLIENTAREAANIMATION`) is on.
/// A setting that cannot be read counts as off, as no motion is never harmful.
pub fn allowed(setting: Option<bool>) -> bool {
    let _ = setting;
    todo!()
}

/// A layout that lands while a zoom in runs waits until the zoom ends: until then a click is
/// found through the rects of the zooming picture, which the new layout would replace.
pub fn holds_layout(moving: Option<Kind>, running: bool) -> bool {
    let _ = (moving, running);
    todo!()
}

/// What plays when a layout lands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Landing {
    /// The new picture fades in.
    Fade,
    /// The old picture shrinks into this frame of the new picture and fades out.
    Out(Frame),
    /// The new picture shows at once.
    Still,
}

/// What plays when a layout lands in place of the picture of `old_root`: `allowed` motion is on;
/// `first` it is the first picture of a scan; `out_from` the folder that going up left
/// (`tree_view::ZoomWay::Out`), with `out_rect` its rect (or the rect of its nearest drawn folder)
/// in the new picture. The old picture shrinks only when it shows `out_from`.
pub fn landing(allowed: bool, first: bool, out_from: Option<NodeId>, old_root: Option<NodeId>, out_rect: Option<Frame>) -> Landing {
    let _ = (allowed, first, out_from, old_root, out_rect);
    todo!()
}

/// The Windows setting "Animation effects" (Settings > Accessibility > Visual effects). `None`
/// when it cannot be read.
pub fn client_area_animation() -> Option<bool> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_setting_on_allows_motion() {
        assert!(allowed(Some(true)));
    }

    #[test]
    fn allowed_setting_off_stops_motion() {
        assert!(!allowed(Some(false)));
    }

    #[test]
    fn allowed_setting_that_cannot_be_read_stops_motion() {
        assert!(!allowed(None));
    }

    #[test]
    fn holds_layout_only_while_a_zoom_in_runs() {
        let cases = [None, Some(Kind::In), Some(Kind::Out), Some(Kind::Fade)];
        let got: Vec<bool> = cases.iter().flat_map(|&k| [false, true].map(|r| holds_layout(k, r))).collect();
        // (none, still) (none, runs) (in, waits) (in, runs) (out, ..) (out, ..) (fade, ..) (fade, ..)
        assert_eq!(got, [false, false, false, true, false, false, false, false]);
    }

    const LEFT: Option<NodeId> = Some(NodeId(6));
    const RECT: Option<Frame> = Some((4.0, 18.0, 40.0, 30.0));

    #[test]
    fn landing_after_up_old_picture_of_the_folder_left_shrinks_into_its_rect() {
        assert_eq!(landing(true, false, LEFT, LEFT, RECT), Landing::Out(RECT.unwrap()));
    }

    #[test]
    fn landing_after_up_old_picture_of_another_folder_does_not_move() {
        assert_eq!(landing(true, false, LEFT, Some(NodeId(1)), RECT), Landing::Still);
    }

    #[test]
    fn landing_after_up_folder_left_not_drawn_does_not_move() {
        assert_eq!(landing(true, false, LEFT, LEFT, None), Landing::Still);
    }

    #[test]
    fn landing_with_no_up_does_not_move() {
        assert_eq!(landing(true, false, None, LEFT, RECT), Landing::Still);
    }

    #[test]
    fn landing_after_up_with_motion_off_does_not_move() {
        assert_eq!(landing(false, false, LEFT, LEFT, RECT), Landing::Still);
    }

    #[cfg(windows)]
    #[test]
    fn client_area_animation_reads_the_setting() {
        assert!(client_area_animation().is_some());
    }
}
