//! What a click on the tray icon does to the panel.
//!
//! Clicking the icon while the panel is open takes the panel's focus before
//! the click arrives, so the panel hides on blur first; the click must not
//! open it again. A click within [`BLUR_GRACE`] of that blur does nothing.

use std::time::{Duration, Instant};

/// How long after the panel hid on blur a tray click is ignored.
pub(crate) const BLUR_GRACE: Duration = Duration::from_millis(300);

/// What to do with the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Place it by the icon and show it.
    Show,
    /// Hide it.
    Hide,
    /// Leave it as it is.
    Nothing,
}

/// The panel's visibility as the tray sees it.
#[derive(Debug, Default)]
pub(crate) struct Toggle {
    visible: bool,
    blurred_at: Option<Instant>,
}

impl Toggle {
    /// A left click on the tray icon at `now`.
    pub(crate) fn click(&mut self, now: Instant) -> Action {
        if self.visible {
            self.visible = false;
            return Action::Hide;
        }
        let in_grace = self
            .blurred_at
            .is_some_and(|at| now.saturating_duration_since(at) < BLUR_GRACE);
        if in_grace {
            return Action::Nothing;
        }
        self.visible = true;
        Action::Show
    }

    /// The panel lost focus at `now`.
    pub(crate) fn blur(&mut self, now: Instant) -> Action {
        if !self.visible {
            return Action::Nothing;
        }
        self.visible = false;
        self.blurred_at = Some(now);
        Action::Hide
    }

    /// The panel was shown from elsewhere (a menu item).
    pub(crate) fn shown(&mut self) {
        self.visible = true;
    }

    /// The panel was hidden from elsewhere (the settings window opened).
    pub(crate) fn hidden(&mut self) {
        self.visible = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_opens_and_the_next_closes() {
        let t0 = Instant::now();
        let mut toggle = Toggle::default();
        assert_eq!(toggle.click(t0), Action::Show);
        assert_eq!(toggle.click(t0 + Duration::from_secs(1)), Action::Hide);
        assert_eq!(toggle.click(t0 + Duration::from_secs(2)), Action::Show);
    }

    #[test]
    fn the_click_that_blurred_the_panel_does_not_reopen_it() {
        let t0 = Instant::now();
        let mut toggle = Toggle::default();
        toggle.click(t0);
        let blur = t0 + Duration::from_secs(3);
        assert_eq!(toggle.blur(blur), Action::Hide);
        assert_eq!(
            toggle.click(blur + Duration::from_millis(120)),
            Action::Nothing
        );
    }

    #[test]
    fn a_click_after_the_grace_opens_it_again() {
        let t0 = Instant::now();
        let mut toggle = Toggle::default();
        toggle.click(t0);
        toggle.blur(t0);
        assert_eq!(toggle.click(t0 + BLUR_GRACE), Action::Show);
    }

    #[test]
    fn a_blur_while_hidden_starts_no_grace() {
        let t0 = Instant::now();
        let mut toggle = Toggle::default();
        assert_eq!(toggle.blur(t0), Action::Nothing);
        assert_eq!(toggle.click(t0 + Duration::from_millis(10)), Action::Show);
    }

    #[test]
    fn hidden_from_elsewhere_the_next_click_opens() {
        let t0 = Instant::now();
        let mut toggle = Toggle::default();
        toggle.shown();
        toggle.hidden();
        assert_eq!(toggle.click(t0), Action::Show);
    }
}
