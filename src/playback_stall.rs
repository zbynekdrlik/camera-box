//! Issue 1539: the pure "the intercom playback device is not consuming" detector.
//!
//! The intercom loop (`intercom::run_intercom_inner`) writes the headset playback with a
//! BLOCKING `writei`, so a device whose hw_ptr stops advancing does not show up as failed
//! writes: the writes block and the frames the device CONSUMES per wall second collapse. Once
//! per check window the loop hands this detector what it can read cheaply -- the elapsed wall
//! time, the frames it wrote, `snd_pcm_delay`, the VBAN packets that arrived and the headset
//! card's identity (`/proc/asound/HID` -> `cardN/usbbus`) -- and the detector says whether the
//! playback handle must be re-opened:
//!
//! - consumed = frames written - (delay now - delay at the previous window); a window is LOW
//!   when consumed < [`STALL_CONSUMED_RATIO`] x the frames the elapsed wall time implies.
//!   [`STALL_CONFIRM_WINDOWS`] LOW windows in a row WHILE packets arrive = `NotConsuming`
//!   (no packets = nobody to hear, never a verdict; one hiccup window never re-opens).
//! - the card behind the handle was re-enumerated (another card index or USB bus/device
//!   number than at open) = `CardReenumerated`; no `HID` card at all = `CardGone`.
//!
//! No I/O here: every input is passed in, so the tests run on default features.

/// A window is LOW when it consumed less than this share of the wall-clock frames.
pub const STALL_CONSUMED_RATIO: f64 = 0.5;
/// LOW windows in a row (while packets arrive) before the verdict.
pub const STALL_CONFIRM_WINDOWS: u32 = 2;

/// The headset card as the kernel names it: its ALSA card index and its USB bus/device
/// number (`/proc/asound/cardN/usbbus`, e.g. `001/004`; a re-enumeration gives a new one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardIdentity {
    pub card: i32,
    pub usbbus: String,
}

/// What the loop could read of the headset card this window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityRead {
    Present(CardIdentity),
    /// No `HID` card exists any more.
    Gone,
    /// Could not be read this window (never a verdict).
    Unknown,
}

/// One check window's observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackObservation {
    pub elapsed_ns: u64,
    pub frames_written: u64,
    /// `snd_pcm_delay` at the end of the window; `None` when the read failed.
    pub delay_frames: Option<i64>,
    pub packets_received: u64,
    pub identity: IdentityRead,
}

/// Why the playback handle must be re-opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StallReason {
    NotConsuming { consumed: u64, expected: u64 },
    CardReenumerated { opened: CardIdentity, now: CardIdentity },
    CardGone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaybackVerdict {
    /// Nothing to do. `consumed` = the frames the device drained this window, when measured.
    Ok { consumed: Option<u64> },
    Reopen(StallReason),
}

#[derive(Debug)]
pub struct PlaybackStallDetector {
    sample_rate: u32,
    opened: Option<CardIdentity>,
    prev_delay: Option<i64>,
    low_windows: u32,
}

impl PlaybackStallDetector {
    /// `opened` = the card identity read right after the playback open (`None` = unreadable,
    /// then only `CardGone` is checked).
    pub fn new(sample_rate: u32, opened: Option<CardIdentity>) -> Self {
        Self {
            sample_rate,
            opened,
            prev_delay: None,
            low_windows: 0,
        }
    }

    pub fn observe(&mut self, _obs: &PlaybackObservation) -> PlaybackVerdict {
        PlaybackVerdict::Ok { consumed: None }
    }
}

/// `/proc/asound/HID` is a symlink to `cardN`: the card index, or `None`.
pub fn card_index_from_link(target: &str) -> Option<i32> {
    let _ = target;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const WINDOW_NS: u64 = 1_000_000_000;

    fn id(card: i32, usbbus: &str) -> CardIdentity {
        CardIdentity {
            card,
            usbbus: usbbus.to_string(),
        }
    }

    fn obs(written: u64, delay: Option<i64>, packets: u64) -> PlaybackObservation {
        PlaybackObservation {
            elapsed_ns: WINDOW_NS,
            frames_written: written,
            delay_frames: delay,
            packets_received: packets,
            identity: IdentityRead::Present(id(0, "001/004")),
        }
    }

    fn det() -> PlaybackStallDetector {
        PlaybackStallDetector::new(RATE, Some(id(0, "001/004")))
    }

    #[test]
    fn healthy_drain_never_reopens_and_reports_consumption_1539() {
        let mut d = det();
        assert_eq!(
            d.observe(&obs(48_000, Some(512), 187)),
            PlaybackVerdict::Ok { consumed: None },
            "the first window only sets the delay baseline"
        );
        for _ in 0..20 {
            assert_eq!(
                d.observe(&obs(48_000, Some(512), 187)),
                PlaybackVerdict::Ok {
                    consumed: Some(48_000)
                }
            );
        }
    }

    #[test]
    fn frozen_drain_while_packets_arrive_reopens_after_confirm_windows_1539() {
        let mut d = det();
        d.observe(&obs(48_000, Some(512), 187));
        // The device stops draining: writes block, only the buffer's worth lands, delay pegs full.
        assert_eq!(
            d.observe(&obs(512, Some(1024), 187)),
            PlaybackVerdict::Ok { consumed: Some(0) },
            "one LOW window is not yet a verdict"
        );
        assert_eq!(
            d.observe(&obs(0, Some(1024), 187)),
            PlaybackVerdict::Reopen(StallReason::NotConsuming {
                consumed: 0,
                expected: 48_000
            })
        );
    }

    #[test]
    fn one_hiccup_window_resets_and_never_reopens_1539() {
        let mut d = det();
        d.observe(&obs(48_000, Some(512), 187));
        for _ in 0..10 {
            assert!(matches!(
                d.observe(&obs(512, Some(1024), 187)),
                PlaybackVerdict::Ok { .. }
            ));
            // the device drains again: the buffer empties to 512 + a full second written
            assert!(matches!(
                d.observe(&obs(48_000, Some(512), 187)),
                PlaybackVerdict::Ok { .. }
            ));
        }
    }

    #[test]
    fn no_packets_arriving_is_never_a_consumption_verdict_1539() {
        let mut d = det();
        d.observe(&obs(48_000, Some(512), 0));
        for _ in 0..10 {
            assert!(matches!(
                d.observe(&obs(0, Some(1024), 0)),
                PlaybackVerdict::Ok { .. }
            ));
        }
    }

    #[test]
    fn half_rate_boundary_counts_as_low_1539() {
        let mut d = det();
        d.observe(&obs(48_000, Some(512), 187));
        // 23_999 frames consumed of 48_000 expected: below half
        d.observe(&obs(23_999, Some(512), 187));
        assert_eq!(
            d.observe(&obs(23_999, Some(512), 187)),
            PlaybackVerdict::Reopen(StallReason::NotConsuming {
                consumed: 23_999,
                expected: 48_000
            })
        );
        let mut d = det();
        d.observe(&obs(48_000, Some(512), 187));
        for _ in 0..5 {
            assert_eq!(
                d.observe(&obs(24_000, Some(512), 187)),
                PlaybackVerdict::Ok {
                    consumed: Some(24_000)
                },
                "exactly half is not LOW"
            );
        }
    }

    #[test]
    fn failed_delay_read_neither_confirms_nor_clears_1539() {
        let mut d = det();
        d.observe(&obs(48_000, Some(512), 187));
        d.observe(&obs(0, Some(1024), 187)); // LOW 1
        assert_eq!(
            d.observe(&obs(0, None, 187)),
            PlaybackVerdict::Ok { consumed: None }
        );
        // The next measured window re-baselines from the failed read, then LOW 2 confirms.
        assert_eq!(
            d.observe(&obs(0, Some(1024), 187)),
            PlaybackVerdict::Ok { consumed: None }
        );
        assert!(matches!(
            d.observe(&obs(0, Some(1024), 187)),
            PlaybackVerdict::Reopen(StallReason::NotConsuming { .. })
        ));
    }

    #[test]
    fn underrun_recovery_delay_drop_is_consumption_not_a_stall_1539() {
        let mut d = det();
        d.observe(&obs(48_000, Some(1024), 187));
        // an xrun + recover empties the ring: delay falls, consumed = written + the drained ring
        assert_eq!(
            d.observe(&obs(46_000, Some(0), 187)),
            PlaybackVerdict::Ok {
                consumed: Some(47_024)
            }
        );
    }

    #[test]
    fn card_reenumerated_reopens_at_once_1539() {
        let mut d = det();
        let mut o = obs(48_000, Some(512), 187);
        o.identity = IdentityRead::Present(id(0, "001/007"));
        assert_eq!(
            d.observe(&o),
            PlaybackVerdict::Reopen(StallReason::CardReenumerated {
                opened: id(0, "001/004"),
                now: id(0, "001/007")
            })
        );
        let mut d = det();
        o.identity = IdentityRead::Present(id(1, "001/004"));
        assert!(matches!(
            d.observe(&o),
            PlaybackVerdict::Reopen(StallReason::CardReenumerated { .. })
        ));
    }

    #[test]
    fn card_gone_reopens_and_unknown_identity_never_does_1539() {
        let mut d = det();
        let mut o = obs(48_000, Some(512), 187);
        o.identity = IdentityRead::Gone;
        assert_eq!(
            d.observe(&o),
            PlaybackVerdict::Reopen(StallReason::CardGone)
        );
        let mut d = det();
        o.identity = IdentityRead::Unknown;
        for _ in 0..5 {
            assert!(matches!(d.observe(&o), PlaybackVerdict::Ok { .. }));
        }
    }

    #[test]
    fn unreadable_open_identity_still_catches_card_gone_1539() {
        let mut d = PlaybackStallDetector::new(RATE, None);
        let mut o = obs(48_000, Some(512), 187);
        assert!(matches!(d.observe(&o), PlaybackVerdict::Ok { .. }));
        o.identity = IdentityRead::Gone;
        assert_eq!(
            d.observe(&o),
            PlaybackVerdict::Reopen(StallReason::CardGone)
        );
    }

    #[test]
    fn card_index_parses_the_proc_link_1539() {
        assert_eq!(card_index_from_link("card0"), Some(0));
        assert_eq!(card_index_from_link("card12"), Some(12));
        assert_eq!(card_index_from_link("/proc/asound/card3"), Some(3));
        assert_eq!(card_index_from_link("HID"), None);
        assert_eq!(card_index_from_link("cardx"), None);
    }
}
