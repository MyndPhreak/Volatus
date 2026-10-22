//! Browse button state: at most one folder dialog is open at a time.
//!
//! The dialog runs off the UI thread, so the Browse button can be pressed again while it is open.
//! This state decides whether a press opens a dialog and whether a result is applied.

use std::path::PathBuf;

#[derive(Debug, Default)]
pub struct BrowseState {
    open: bool,
}

impl BrowseState {
    /// True while a dialog is open. The Browse button is disabled then.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// A press of Browse. True if the caller must open a dialog; false if one is already open.
    pub fn try_open(&mut self) -> bool {
        !std::mem::replace(&mut self.open, true)
    }

    /// The dialog closed with `picked` (`None` if cancelled). Returns the path to apply, if any.
    /// A result with no open dialog is dropped.
    pub fn finish(&mut self, picked: Option<PathBuf>) -> Option<PathBuf> {
        std::mem::replace(&mut self.open, false).then_some(picked).flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_open_new_state_is_idle() {
        assert!(!BrowseState::default().is_open());
    }

    #[test]
    fn try_open_when_idle_opens_and_returns_true() {
        let mut s = BrowseState::default();
        assert!(s.try_open());
        assert!(s.is_open());
    }

    #[test]
    fn try_open_while_open_is_ignored() {
        let mut s = BrowseState::default();
        s.try_open();
        assert!(!s.try_open());
        assert!(s.is_open());
    }

    #[test]
    fn finish_with_path_returns_it_and_goes_idle() {
        let mut s = BrowseState::default();
        s.try_open();
        assert_eq!(s.finish(Some(PathBuf::from("C:\\Data"))), Some(PathBuf::from("C:\\Data")));
        assert!(!s.is_open());
    }

    #[test]
    fn finish_cancelled_returns_nothing_and_goes_idle() {
        let mut s = BrowseState::default();
        s.try_open();
        assert_eq!(s.finish(None), None);
        assert!(!s.is_open());
    }

    #[test]
    fn finish_without_open_dialog_drops_the_result() {
        let mut s = BrowseState::default();
        assert_eq!(s.finish(Some(PathBuf::from("C:\\Data"))), None);
        assert!(!s.is_open());
    }

    #[test]
    fn try_open_after_finish_opens_again() {
        let mut s = BrowseState::default();
        s.try_open();
        s.finish(None);
        assert!(s.try_open());
    }
}
