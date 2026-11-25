//! Issue 1501: the cambox power button's single press versus double press.
//!
//! One press toggles the cameraman's microphone (as before), a double press switches the HDMI
//! preview between the strih NDI feed and the local camera. A press arms a window; a second press
//! inside it is a [`PressAction::Double`] and never touches the mic; a window that runs out with
//! one press pending is a [`PressAction::Single`]. So the mic toggle fires one window late (400 ms,
//! intercom confirmed on 11.11.2026 that neither the hub nor the phones depend on its timing).
//! Pure: the caller feeds millisecond timestamps from one monotonic clock.

/// The double-press window.
pub const DOUBLE_PRESS_WINDOW_MS: u64 = 400;

/// What a press (or the end of a window) means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressAction {
    /// One press and no second inside the window: the microphone toggle.
    Single,
    /// Two presses inside the window: the preview switch.
    Double,
}

/// Turns press instants into [`PressAction`]s.
#[derive(Debug, Clone)]
pub struct PressClassifier {
    window_ms: u64,
    pending_since_ms: Option<u64>,
}

impl Default for PressClassifier {
    fn default() -> Self {
        Self::new(DOUBLE_PRESS_WINDOW_MS)
    }
}

impl PressClassifier {
    pub fn new(window_ms: u64) -> Self {
        Self {
            window_ms,
            pending_since_ms: None,
        }
    }

    /// A button press at `now_ms`. `Some(Double)` when it closes a pending press inside the window.
    /// A press after an expired window that nobody ticked first resolves that old press as a
    /// `Single` (returned) and arms a new window for this one.
    pub fn press(&mut self, now_ms: u64) -> Option<PressAction> {
        match self.pending_since_ms {
            Some(t) if now_ms.saturating_sub(t) < self.window_ms => {
                self.pending_since_ms = None;
                Some(PressAction::Double)
            }
            Some(_) => {
                self.pending_since_ms = Some(now_ms);
                Some(PressAction::Single)
            }
            None => {
                self.pending_since_ms = Some(now_ms);
                None
            }
        }
    }

    /// The clock at `now_ms` with no press: `Some(Single)` once the pending press's window ran out.
    pub fn tick(&mut self, now_ms: u64) -> Option<PressAction> {
        match self.pending_since_ms {
            Some(t) if now_ms.saturating_sub(t) >= self.window_ms => {
                self.pending_since_ms = None;
                Some(PressAction::Single)
            }
            _ => None,
        }
    }

    /// Whether a press is waiting for its window to end.
    pub fn pending(&self) -> bool {
        self.pending_since_ms.is_some()
    }
}

/// Issue 1539: a press from ANOTHER device this close after the last accepted press is the same
/// physical press, reported twice (a cambox lists several KEY_POWER devices: two ACPI "Power
/// Button" devices and "Intel HID events"). A real double press is one button, ~100 ms or more apart.
pub const CROSS_DEVICE_ECHO_MS: u64 = 60;

/// Drops the second report of one physical press that two input devices both delivered.
#[derive(Debug, Clone, Default)]
pub struct PressEcho {
    last: Option<(u64, usize)>,
}

impl PressEcho {
    /// A press from `device` (the caller's device index) at `now_ms`: `true` = a press of its own,
    /// `false` = the echo of the last accepted press on another device (ignore it). The same device
    /// twice is always two presses.
    pub fn accept(&mut self, now_ms: u64, device: usize) -> bool {
        if let Some((t, d)) = self.last {
            if d != device && now_ms.saturating_sub(t) < CROSS_DEVICE_ECHO_MS {
                return false;
            }
        }
        self.last = Some((now_ms, device));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_press_echoed_by_another_device_inside_the_echo_window_is_dropped_1539() {
        let mut e = PressEcho::default();
        assert!(e.accept(1000, 0));
        assert!(!e.accept(1000, 2), "same instant, other device = the echo");
        assert!(!e.accept(1059, 1), "still inside 60 ms");
        assert!(e.accept(1060, 1), "the window edge is a press of its own");
    }

    #[test]
    fn the_same_device_twice_is_always_two_presses_1539() {
        let mut e = PressEcho::default();
        assert!(e.accept(1000, 0));
        assert!(e.accept(1001, 0));
    }

    #[test]
    fn an_echo_never_moves_the_window_1539() {
        let mut e = PressEcho::default();
        assert!(e.accept(1000, 0));
        assert!(!e.accept(1040, 1));
        assert!(
            e.accept(1070, 1),
            "measured from the ACCEPTED press, not the dropped echo"
        );
    }

    #[test]
    fn a_backwards_clock_reads_as_inside_the_window_1539() {
        let mut e = PressEcho::default();
        assert!(e.accept(1000, 0));
        assert!(!e.accept(900, 1));
    }

    #[test]
    fn a_single_press_fires_only_when_its_window_ends() {
        let mut c = PressClassifier::new(400);
        assert_eq!(c.press(1000), None);
        assert_eq!(c.tick(1200), None);
        assert_eq!(c.tick(1399), None);
        assert_eq!(c.tick(1400), Some(PressAction::Single));
        assert!(!c.pending());
        assert_eq!(c.tick(2000), None, "fires once");
    }

    #[test]
    fn two_presses_inside_the_window_are_a_double_and_never_a_single() {
        let mut c = PressClassifier::new(400);
        assert_eq!(c.press(1000), None);
        assert_eq!(c.press(1250), Some(PressAction::Double));
        assert_eq!(c.tick(1500), None);
        assert_eq!(c.tick(5000), None);
    }

    #[test]
    fn the_window_edge_is_exclusive_for_a_double() {
        let mut c = PressClassifier::new(400);
        c.press(1000);
        assert_eq!(c.press(1399), Some(PressAction::Double));
        let mut c = PressClassifier::new(400);
        c.press(1000);
        assert_eq!(
            c.press(1400),
            Some(PressAction::Single),
            "the old press resolves"
        );
        assert!(c.pending(), "the new press waits for its own window");
        assert_eq!(c.tick(1800), Some(PressAction::Single));
    }

    #[test]
    fn a_triple_press_is_a_double_then_a_single() {
        let mut c = PressClassifier::new(400);
        assert_eq!(c.press(0), None);
        assert_eq!(c.press(150), Some(PressAction::Double));
        assert_eq!(c.press(300), None);
        assert_eq!(c.tick(699), None);
        assert_eq!(c.tick(700), Some(PressAction::Single));
    }

    #[test]
    fn four_quick_presses_are_two_doubles() {
        let mut c = PressClassifier::new(400);
        let got: Vec<_> = [0, 100, 200, 300]
            .iter()
            .filter_map(|t| c.press(*t))
            .collect();
        assert_eq!(got, vec![PressAction::Double, PressAction::Double]);
        assert_eq!(c.tick(10_000), None);
    }

    #[test]
    fn a_clock_that_goes_backwards_never_panics_and_reads_as_inside_the_window() {
        let mut c = PressClassifier::new(400);
        c.press(1000);
        assert_eq!(c.tick(900), None);
        assert_eq!(c.press(900), Some(PressAction::Double));
    }

    #[test]
    fn the_default_window_is_400_ms() {
        let mut c = PressClassifier::default();
        c.press(0);
        assert_eq!(c.tick(399), None);
        assert_eq!(c.tick(400), Some(PressAction::Single));
    }
}
