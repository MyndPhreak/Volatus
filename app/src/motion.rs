//! When the treemap picture moves and how: pure decisions. The window code holds the timer and
//! the pictures and does what these functions say.

use scan::NodeId;
use treemap::Frame;

/// A move of the treemap picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The old picture zooms into the rect of the opened folder until that rect fills the map.
    In,
    /// The new picture starts zoomed into the rect of the folder that going up left, so that rect
    /// fills the map as that folder did, and zooms out until it fits the map. (The pictures are
    /// glass, so two of them over each other would mix: only one moves.)
    Out,
    /// The first picture of a scan fades in.
    #[allow(dead_code, reason = "the fade comes in P8.T2")]
    Fade,
}

/// Motion is on: the Windows setting "Animation effects" (`SPI_GETCLIENTAREAANIMATION`) is on.
/// A setting that cannot be read counts as off, as no motion is never harmful.
pub fn allowed(setting: Option<bool>) -> bool {
    setting == Some(true)
}

/// A layout that lands while a zoom in runs waits until the zoom ends: until then a click is
/// found through the rects of the zooming picture, which the new layout would replace.
pub fn holds_layout(moving: Option<Kind>, running: bool) -> bool {
    moving == Some(Kind::In) && running
}

/// What plays when a layout lands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Landing {
    /// The new picture fades in.
    #[allow(dead_code, reason = "the fade comes in P8.T2")]
    Fade,
    /// The new picture zooms out from this rect of it (`Kind::Out`).
    Out(Frame),
    /// The new picture shows at once.
    Still,
}

/// What plays when a layout lands in place of the picture of `old_root`: `allowed` motion is on;
/// `first` it is the first picture of a scan; `out_from` the folder that going up left
/// (`tree_view::ZoomWay::Out`), with `out_rect` its rect (or the rect of its nearest drawn folder)
/// in the new picture. It zooms out only when the old picture shows `out_from`.
pub fn landing(allowed: bool, first: bool, out_from: Option<NodeId>, old_root: Option<NodeId>, out_rect: Option<Frame>) -> Landing {
    let _ = first;
    match out_rect {
        Some(r) if allowed && out_from.is_some() && out_from == old_root => Landing::Out(r),
        _ => Landing::Still,
    }
}

/// The Windows setting "Animation effects" (Settings > Accessibility > Visual effects). `None`
/// when it cannot be read.
#[cfg(windows)]
pub fn client_area_animation() -> Option<bool> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW};
    let mut on: windows_sys::core::BOOL = 0;
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL to the pointer, which points to `on`.
    let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut on).cast(), 0) } != 0;
    ok.then_some(on != 0)
}

/// Other systems have no such setting here: motion is on.
#[cfg(not(windows))]
pub fn client_area_animation() -> Option<bool> {
    Some(true)
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

    #[test]
    fn landing_first_picture_of_a_scan_fades_in() {
        assert_eq!(landing(true, true, None, None, None), Landing::Fade);
    }

    #[test]
    fn landing_first_picture_of_a_scan_fades_in_also_after_an_up() {
        assert_eq!(landing(true, true, LEFT, LEFT, RECT), Landing::Fade);
    }

    #[cfg(windows)]
    #[test]
    fn client_area_animation_reads_the_setting() {
        assert!(client_area_animation().is_some());
    }
}
