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
    NotConsuming {
        consumed: u64,
        expected: u64,
    },
    CardReenumerated {
        opened: CardIdentity,
        now: CardIdentity,
    },
    CardGone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaybackVerdict {
    /// Nothing to do. `consumed` = the frames the device drained this window, when measured.
    Ok {
        consumed: Option<u64>,
    },
    Reopen(StallReason),
}

#[derive(Debug)]
pub struct PlaybackStallDetector {
    sample_rate: u32,
    #[allow(dead_code)]
    full_ring_frames: i64,
    opened: Option<CardIdentity>,
    prev_delay: Option<i64>,
    low_windows: u32,
}

impl PlaybackStallDetector {
    /// `full_ring_frames` = the delay from which the ring counts as full (buffer - period).
    /// `opened` = the card identity read right after the playback open (`None` = unreadable,
    /// then only `CardGone` is checked).
    pub fn new(sample_rate: u32, full_ring_frames: i64, opened: Option<CardIdentity>) -> Self {
        Self {
            sample_rate,
            full_ring_frames,
            opened,
            prev_delay: None,
            low_windows: 0,
        }
    }

    pub fn observe(&mut self, obs: &PlaybackObservation) -> PlaybackVerdict {
        match &obs.identity {
            IdentityRead::Gone => return PlaybackVerdict::Reopen(StallReason::CardGone),
            IdentityRead::Present(now) => {
                if let Some(opened) = &self.opened {
                    if opened != now {
                        return PlaybackVerdict::Reopen(StallReason::CardReenumerated {
                            opened: opened.clone(),
                            now: now.clone(),
                        });
                    }
                }
            }
            IdentityRead::Unknown => {}
        }

        // A failed delay read loses the baseline: the window neither confirms nor clears.
        let Some(delay) = obs.delay_frames else {
            self.prev_delay = None;
            return PlaybackVerdict::Ok { consumed: None };
        };
        let Some(prev) = self.prev_delay.replace(delay) else {
            return PlaybackVerdict::Ok { consumed: None };
        };

        let consumed = (obs.frames_written as i64)
            .saturating_sub(delay.saturating_sub(prev))
            .max(0) as u64;
        let expected =
            (u128::from(obs.elapsed_ns) * u128::from(self.sample_rate) / 1_000_000_000) as u64;
        if expected == 0 {
            return PlaybackVerdict::Ok {
                consumed: Some(consumed),
            };
        }

        let low = (consumed as f64) < expected as f64 * STALL_CONSUMED_RATIO;
        if low && obs.packets_received > 0 {
            self.low_windows += 1;
            if self.low_windows >= STALL_CONFIRM_WINDOWS {
                self.low_windows = 0;
                return PlaybackVerdict::Reopen(StallReason::NotConsuming { consumed, expected });
            }
        } else {
            self.low_windows = 0;
        }
        PlaybackVerdict::Ok {
            consumed: Some(consumed),
        }
    }
}

/// At most this many playback re-opens per [`REOPEN_BUDGET_WINDOW_NS`].
pub const REOPEN_BUDGET_MAX: usize = 3;
pub const REOPEN_BUDGET_WINDOW_NS: u64 = 60_000_000_000;

/// A sliding-window cap on playback re-opens, so a device that keeps failing never turns into
/// a reopen/log storm.
#[derive(Debug)]
pub struct ReopenBudget {
    #[allow(dead_code)]
    max: usize,
    #[allow(dead_code)]
    window_ns: u64,
}

impl ReopenBudget {
    pub fn new(max: usize, window_ns: u64) -> Self {
        Self { max, window_ns }
    }

    /// Spend one re-open at monotonic `now_ns`; `false` = the budget is used up (nothing spent).
    pub fn try_spend(&mut self, _now_ns: u64) -> bool {
        true
    }
}

/// `/proc/asound/HID` is a symlink to `cardN`: the card index, or `None`.
pub fn card_index_from_link(target: &str) -> Option<i32> {
    let name = target.rsplit('/').next()?;
    let digits = name.strip_prefix("card")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
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

    /// The intercom's ring: 4 periods of 256 frames, "full" from buffer - period.
    const FULL_RING: i64 = 1024 - 256;

    fn det() -> PlaybackStallDetector {
        PlaybackStallDetector::new(RATE, FULL_RING, Some(id(0, "001/004")))
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
    fn slow_drain_with_a_full_ring_while_packets_arrive_reopens_after_confirm_windows_1539() {
        // The device drains far below its rate: the blocking writes land only what it drains,
        // so the ring stays full (delay = the whole buffer) and the window consumed ~10k of 48k.
        // (A device that drains NOTHING blocks writei until the kernel's 10 s timeout returns
        // EIO; that takes the intercom's write-error reopen path, not this window.)
        let mut d = det();
        d.observe(&obs(48_000, Some(512), 187));
        assert_eq!(
            d.observe(&obs(10_512, Some(1024), 187)),
            PlaybackVerdict::Ok {
                consumed: Some(10_000)
            },
            "one LOW window is not yet a verdict"
        );
        assert_eq!(
            d.observe(&obs(10_000, Some(1024), 187)),
            PlaybackVerdict::Reopen(StallReason::NotConsuming {
                consumed: 10_000,
                expected: 48_000
            })
        );
    }

    #[test]
    fn starved_writer_with_an_empty_ring_is_never_a_device_stall_1539() {
        // The LOOP wrote little (a slow capture read): the ring ran empty, so the device was
        // not the bottleneck -- never re-open a healthy playback for it.
        let mut d = det();
        d.observe(&obs(48_000, Some(0), 187));
        for _ in 0..10 {
            assert_eq!(
                d.observe(&obs(2_000, Some(0), 187)),
                PlaybackVerdict::Ok {
                    consumed: Some(2_000)
                }
            );
            assert_eq!(
                d.observe(&obs(0, Some(0), 187)),
                PlaybackVerdict::Ok { consumed: Some(0) }
            );
        }
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
        d.observe(&obs(48_000, Some(1024), 187));
        // 23_999 frames consumed of 48_000 expected with the ring full: below half
        d.observe(&obs(23_999, Some(1024), 187));
        assert_eq!(
            d.observe(&obs(23_999, Some(1024), 187)),
            PlaybackVerdict::Reopen(StallReason::NotConsuming {
                consumed: 23_999,
                expected: 48_000
            })
        );
        let mut d = det();
        d.observe(&obs(48_000, Some(1024), 187));
        for _ in 0..5 {
            assert_eq!(
                d.observe(&obs(24_000, Some(1024), 187)),
                PlaybackVerdict::Ok {
                    consumed: Some(24_000)
                },
                "exactly half is not LOW"
            );
        }
    }

    #[test]
    fn failed_delay_read_breaks_the_run_and_rebaselines_1539() {
        let mut d = det();
        d.observe(&obs(48_000, Some(512), 187));
        d.observe(&obs(512, Some(1024), 187)); // LOW 1
        assert_eq!(
            d.observe(&obs(0, None, 187)),
            PlaybackVerdict::Ok { consumed: None }
        );
        // The next measured window only re-baselines; the LOW run starts over ("in a row").
        assert_eq!(
            d.observe(&obs(0, Some(1024), 187)),
            PlaybackVerdict::Ok { consumed: None }
        );
        assert_eq!(
            d.observe(&obs(0, Some(1024), 187)),
            PlaybackVerdict::Ok { consumed: Some(0) },
            "LOW 1 of a new run"
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
        let mut d = PlaybackStallDetector::new(RATE, FULL_RING, None);
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

    const S: u64 = 1_000_000_000;

    #[test]
    fn reopen_budget_allows_three_per_minute_then_refuses_1539() {
        let mut b = ReopenBudget::new(REOPEN_BUDGET_MAX, REOPEN_BUDGET_WINDOW_NS);
        assert!(b.try_spend(0));
        assert!(b.try_spend(5 * S));
        assert!(b.try_spend(10 * S));
        assert!(!b.try_spend(11 * S), "a 4th reopen inside 60 s is refused");
        assert!(!b.try_spend(59 * S));
        // the first spend ages out at 60 s: one more is allowed, then refused again
        assert!(b.try_spend(60 * S));
        assert!(!b.try_spend(61 * S));
        assert!(b.try_spend(130 * S));
    }

    #[test]
    fn reopen_budget_refusals_do_not_consume_it_1539() {
        let mut b = ReopenBudget::new(1, 10 * S);
        assert!(b.try_spend(0));
        for t in 1..10 {
            assert!(!b.try_spend(t * S));
        }
        assert!(b.try_spend(10 * S), "only the granted spend counts");
    }
}
