//! The life cycle of the treemap tooltip as a pure state machine. The window code feeds it events
//! and does what the returned action says; it holds no timer and no window.

/// Where the tooltip is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipState {
    /// Nothing under the pointer, or no pointer on the map. No tooltip, no timer.
    Idle,
    /// The pointer is on a node and the delay runs.
    Pending,
    /// The tooltip shows.
    Shown,
    /// A key, a click, a scroll, a new layout or a view change hid the tooltip. It stays hidden
    /// until the pointer moves again.
    Suppressed,
}

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipEvent {
    /// A real pointer move over the map. `over`: a node is under the pointer. `changed`: it is not
    /// the node of the move before.
    Moved { over: bool, changed: bool },
    /// The delay ended.
    Timer,
    /// A key in the map or the tree.
    Key,
    /// A mouse button went down on the map.
    Click,
    /// The wheel turned over the map.
    Scroll,
    /// The pointer left the map.
    Leave,
    /// A new layout landed, or the shown folder changed.
    Layout,
}

/// What the window code does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipAction {
    None,
    /// Hide the tooltip, then start the delay again.
    Start,
    /// Show the tooltip at the pointer.
    Show,
    /// Hide the tooltip and stop the delay.
    Hide,
}

/// The next state and the action for `event` in `state`.
pub fn step(state: TipState, event: TipEvent) -> (TipState, TipAction) {
    let _ = (state, event);
    todo!("tip::step")
}

#[cfg(test)]
mod tests {
    use super::TipAction::{Hide, None as Nothing, Show, Start};
    use super::TipEvent::{Click, Key, Layout, Leave, Moved, Scroll, Timer};
    use super::TipState::{Idle, Pending, Shown, Suppressed};
    use super::*;

    const ON_NEW: TipEvent = Moved { over: true, changed: true };
    const ON_SAME: TipEvent = Moved { over: true, changed: false };
    const OFF: TipEvent = Moved { over: false, changed: true };

    #[test]
    fn step_move_onto_a_node_from_idle_starts_the_delay() {
        assert_eq!(step(Idle, ON_NEW), (Pending, Start));
    }

    #[test]
    fn step_move_onto_another_node_restarts_the_delay_in_every_state() {
        for state in [Idle, Pending, Shown, Suppressed] {
            assert_eq!(step(state, ON_NEW), (Pending, Start), "{state:?}");
        }
    }

    #[test]
    fn step_move_inside_the_same_node_keeps_pending_and_shown() {
        assert_eq!(step(Pending, ON_SAME), (Pending, Nothing));
        assert_eq!(step(Shown, ON_SAME), (Shown, Nothing));
    }

    #[test]
    fn step_move_inside_the_same_node_after_suppress_shows_again_after_the_delay() {
        assert_eq!(step(Suppressed, ON_SAME), (Pending, Start));
    }

    #[test]
    fn step_move_off_every_node_hides_in_every_state() {
        for state in [Idle, Pending, Shown, Suppressed] {
            assert_eq!(step(state, OFF), (Idle, Hide), "{state:?}");
        }
    }

    #[test]
    fn step_timer_while_pending_shows() {
        assert_eq!(step(Pending, Timer), (Shown, Show));
    }

    #[test]
    fn step_timer_when_not_pending_does_nothing() {
        for state in [Idle, Shown, Suppressed] {
            assert_eq!(step(state, Timer), (state, Nothing), "{state:?}");
        }
    }

    #[test]
    fn step_hiding_events_suppress_pending_and_shown() {
        for event in [Key, Click, Scroll, Layout] {
            for state in [Pending, Shown] {
                assert_eq!(step(state, event), (Suppressed, Hide), "{state:?} {event:?}");
            }
        }
    }

    #[test]
    fn step_hiding_events_keep_suppressed_suppressed() {
        for event in [Key, Click, Scroll, Layout] {
            assert_eq!(step(Suppressed, event), (Suppressed, Hide), "{event:?}");
        }
    }

    #[test]
    fn step_hiding_events_in_idle_stay_idle() {
        for event in [Key, Click, Scroll, Layout] {
            assert_eq!(step(Idle, event), (Idle, Nothing), "{event:?}");
        }
    }

    #[test]
    fn step_leave_goes_idle_and_hides_in_every_state() {
        for state in [Idle, Pending, Shown, Suppressed] {
            assert_eq!(step(state, Leave), (Idle, Hide), "{state:?}");
        }
    }

    #[test]
    fn step_layout_while_shown_hides_and_stays_hidden_through_a_late_timer() {
        let (state, action) = step(Shown, Layout);
        assert_eq!((state, action), (Suppressed, Hide));
        assert_eq!(step(state, Timer), (Suppressed, Nothing));
    }

    #[test]
    fn step_key_while_pending_cancels_the_show() {
        let (state, _) = step(Pending, Key);
        assert_eq!(step(state, Timer).1, Nothing);
    }

    #[test]
    fn step_click_then_the_delay_passes_stays_hidden() {
        let (state, _) = step(Shown, Click);
        assert_eq!(step(state, Timer), (Suppressed, Nothing));
    }

    #[test]
    fn step_move_after_suppress_shows_after_the_delay() {
        let (state, _) = step(Shown, Key);
        let (state, action) = step(state, ON_SAME);
        assert_eq!((state, action), (Pending, Start));
        assert_eq!(step(state, Timer), (Shown, Show));
    }
}
