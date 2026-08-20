//! #1128 — fast-capture grabber STUCK detector (ShadowCast ~62.5 fps + persistent corrupted).
//!
//! ## What this catches that the existing self-heal does not
//!
//! The GENKI ShadowCast 2 grabber can enter a state where its internal USB output clock free-runs
//! at ~62.5 fps (16.0 ms/frame) AND it delivers persistent corrupted buffers
//! (`V4L2_BUF_FLAG_ERROR`, ~4 per 5 s window). Live-confirmed on CAM1 (#1110 comment 5338231650,
//! 2026-08-19): `systemctl restart camera-box` merely re-opens the V4L2 device and does NOT clear
//! the grabber's internal state — the cadence stays 62.x. Only a USB re-enumeration
//! (`echo 0/1 > .../authorized`) re-negotiates the device and clears it. That re-auth mechanism
//! ALREADY exists and is well-tested (`capture_rate_selfheal::perform_usb_reset`).
//!
//! The GAP this module fills is the DETECTOR. The existing self-heal trigger
//! (`capture_rate_selfheal::should_trigger_selfheal`) is deliberately narrow after #909/#914: the
//! ShadowCast jitter-band tolerance was widened to 9 % (62.5/60 = 4.17 % never trips it) precisely
//! to avoid reset-spamming the grabber's benign clock wobble — which the genlock decimation gate
//! already absorbs into exact NDI output. The only remaining path, the #971 chronic sustained
//! band, needs 180 consecutive 5 s windows (15 minutes). And the corrupted-frame counter
//! (`capture.corrupted_frames()`), while logged on the `Streaming:` line, feeds NO decision at all.
//!
//! ## The discriminator: over-rate AND persistent-corrupted, both sustained
//!
//! This detector keys on the COMBINED signature — a captured rate at/above [`OVER_RATE_FPS_FLOOR`]
//! AND a nonzero per-window corrupted delta — held for [`STUCK_CONFIRM_WINDOWS`] consecutive
//! windows. The corrupted band is the DISCRIMINATOR: a benign over-rate wobble (0 corrupted, which
//! the decimation gate absorbs — #909) can never trip this, so declaring STUCK on it and acting is
//! safe from the reset-spam trap that #909/#914 spent three tickets escaping. The two bands are
//! calibrated from #1128's non-overlapping live data (healthy 60.0 ± 0.2 fps / 0 corrupted; stuck
//! 62.2–62.8 fps / 4 corrupted per window).
//!
//! ## Tier-0 pure
//!
//! No probe deps, no I/O, no sysfs, no `tracing` — pure decision + formatting so it unit-tests on
//! default features. `src/main.rs`'s capture loop feeds each 5 s report window's already-computed
//! `cap_fps` + `capture.corrupted_frames()` into a single [`GrabberStuckTracker`], logs the
//! report-only [`stuck_warn_message`] on a [`GrabberStuckVerdict::Stuck`], and — only when the
//! opt-in `CAMERA_BOX_GRABBER_STUCK_SELFHEAL` env is set — funnels that into the existing
//! `capture_rate_selfheal` throttle + USB-reset path. The dev1 alert watchdog
//! (`scripts/grabber-stuck-alert-watchdog.sh`) greps the exact `#1128 grabber STUCK` marker this
//! module emits, keeping ONE source of truth for the verdict (Rust decides; the watchdog relays).

/// Over-rate floor (fps). A captured rate at or above this is "over-rate" for one window.
/// Calibrated from #1128's non-overlapping live bands: healthy 60.0 ± 0.2, stuck 62.2–62.8 — 61.5
/// sits cleanly between (well above the healthy max 60.2, well below the stuck min 62.2).
pub const OVER_RATE_FPS_FLOOR: f64 = 61.5;

/// Consecutive 5 s report windows BOTH bands (over-rate AND persistent-corrupted) must hold before
/// the grabber is declared STUCK. 6 windows = ~30 s: long enough that a single transient corrupted
/// frame or a one-window rate blip never fires, short enough to act ~30× faster than the existing
/// #971 chronic band (180 windows / 15 minutes).
pub const STUCK_CONFIRM_WINDOWS: u32 = 6;

/// This window's verdict from [`GrabberStuckTracker::observe`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GrabberStuckVerdict {
    /// Neither band is currently building — nothing to act on.
    Healthy,
    /// At least one band is building toward confirmation, but not both are confirmed yet.
    Watching {
        over_rate_windows: u32,
        corrupted_windows: u32,
    },
    /// BOTH bands have held for [`STUCK_CONFIRM_WINDOWS`] consecutive windows — the grabber is
    /// stuck. `windows` is the (equal-or-shorter) confirmed run length; `corrupted_delta` /
    /// `captured_fps` are this window's observed values, for the report line.
    Stuck {
        captured_fps: f64,
        corrupted_delta: u64,
        windows: u32,
    },
}

/// Per-process tracker: one instance lives for the lifetime of a capture loop. State is
/// deliberately NOT persisted — a self-heal USB reset exits the process, so a fresh process
/// (fresh tracker, fresh baseline) is exactly the right reset of these counters.
#[derive(Debug, Clone)]
pub struct GrabberStuckTracker {
    confirm_windows: u32,
    fps_floor: f64,
    over_rate_run: u32,
    corrupted_run: u32,
    prev_corrupted_total: Option<u64>,
}

impl Default for GrabberStuckTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl GrabberStuckTracker {
    /// A tracker with the production thresholds ([`STUCK_CONFIRM_WINDOWS`], [`OVER_RATE_FPS_FLOOR`]).
    pub fn new() -> Self {
        Self::with_thresholds(STUCK_CONFIRM_WINDOWS, OVER_RATE_FPS_FLOOR)
    }

    /// A tracker with explicit thresholds (tests use short windows; `confirm_windows` is floored to
    /// 1 so a degenerate 0 can never make an empty run "confirmed").
    pub fn with_thresholds(confirm_windows: u32, fps_floor: f64) -> Self {
        Self {
            confirm_windows: confirm_windows.max(1),
            fps_floor,
            over_rate_run: 0,
            corrupted_run: 0,
            prev_corrupted_total: None,
        }
    }

    /// Feed one 5 s report window: the window's captured fps and the appliance's CUMULATIVE
    /// corrupted-frame counter (`capture.corrupted_frames()`). The per-window delta is derived
    /// internally. Returns this window's [`GrabberStuckVerdict`].
    ///
    /// Band bookkeeping:
    /// - over-rate run: `+1` while `captured_fps >= fps_floor`, else reset to 0 (a single window
    ///   below the floor breaks the streak — the bands must be CONTINUOUS).
    /// - corrupted run: `+1` while the per-window delta is `> 0`, else reset to 0. The first
    ///   observe of a process (no prior sample) records only the baseline — the delta is unknown
    ///   and treated as 0, so a large cumulative counter carried into a fresh process never
    ///   masquerades as one window's worth of corruption.
    pub fn observe(&mut self, captured_fps: f64, corrupted_total: u64) -> GrabberStuckVerdict {
        if captured_fps >= self.fps_floor {
            self.over_rate_run = self.over_rate_run.saturating_add(1);
        } else {
            self.over_rate_run = 0;
        }

        let corrupted_delta = match self.prev_corrupted_total {
            Some(prev) => corrupted_total.saturating_sub(prev),
            None => 0,
        };
        self.prev_corrupted_total = Some(corrupted_total);
        if corrupted_delta > 0 {
            self.corrupted_run = self.corrupted_run.saturating_add(1);
        } else {
            self.corrupted_run = 0;
        }

        // STUCK only when BOTH bands are confirmed. The corrupted band is the discriminator that
        // keeps a benign over-rate wobble (0 corrupted; absorbed by the decimation gate, #909)
        // from ever reaching a reset.
        if self.over_rate_run >= self.confirm_windows && self.corrupted_run >= self.confirm_windows
        {
            return GrabberStuckVerdict::Stuck {
                captured_fps,
                corrupted_delta,
                windows: self.over_rate_run.min(self.corrupted_run),
            };
        }
        if self.over_rate_run > 0 || self.corrupted_run > 0 {
            return GrabberStuckVerdict::Watching {
                over_rate_windows: self.over_rate_run,
                corrupted_windows: self.corrupted_run,
            };
        }
        GrabberStuckVerdict::Healthy
    }
}

/// The report-only WARN line the appliance emits on a [`GrabberStuckVerdict::Stuck`]. The exact
/// substring `#1128 grabber STUCK` is the marker the dev1 alert watchdog greps — keep it stable.
/// Pure formatting, so it is directly unit-testable.
pub fn stuck_warn_message(
    video_device_path: &str,
    captured_fps: f64,
    corrupted_delta: u64,
    windows: u32,
) -> String {
    format!(
        "#1128 grabber STUCK: {video_device_path} captured {captured_fps:.2} fps (>= {OVER_RATE_FPS_FLOOR:.1} fps over-rate floor) WITH persistent corrupted frames ({corrupted_delta}/window) \
         sustained for {windows} consecutive report windows (~{}s) — the ShadowCast free-run+corruption state that a `systemctl restart` does NOT clear; only a USB re-enumeration does (see #1128, #1110). \
         Report-only unless CAMERA_BOX_GRABBER_STUCK_SELFHEAL is set.",
        windows as u64 * 5
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive `n` identical windows through the tracker, returning the last verdict.
    fn drive(
        t: &mut GrabberStuckTracker,
        fps: f64,
        corrupted_per_window: u64,
        n: u32,
    ) -> GrabberStuckVerdict {
        let mut total = t.prev_corrupted_total.unwrap_or(0);
        let mut last = GrabberStuckVerdict::Healthy;
        for _ in 0..n {
            total += corrupted_per_window;
            last = t.observe(fps, total);
        }
        last
    }

    fn is_stuck(v: GrabberStuckVerdict) -> bool {
        matches!(v, GrabberStuckVerdict::Stuck { .. })
    }

    #[test]
    fn healthy_stream_60fps_zero_corrupted_never_stuck() {
        // #1128 healthy band: 60.0 ± 0.2 fps, 0 corrupted. Must stay Healthy indefinitely.
        let mut t = GrabberStuckTracker::new();
        for fps in [
            60.0_f64, 59.9, 60.1, 60.0, 59.8, 60.2, 60.0, 60.0, 60.0, 60.0, 60.0, 60.0,
        ] {
            assert_eq!(t.observe(fps, 0), GrabberStuckVerdict::Healthy);
        }
    }

    #[test]
    fn benign_over_rate_wobble_with_zero_corrupted_never_stuck() {
        // THE discriminator test (#909 anti-reset-spam): a ShadowCast free-running at 62.5 fps but
        // delivering ZERO corrupted frames is the benign wobble the decimation gate absorbs — it
        // must NEVER be declared STUCK, no matter how long it holds.
        let mut t = GrabberStuckTracker::new();
        for _ in 0..100 {
            let v = t.observe(62.5, 0);
            assert!(
                !is_stuck(v),
                "benign over-rate wobble with 0 corrupted must never be STUCK: {v:?}"
            );
        }
        // Over-rate run IS building (Watching), corrupted run is pinned at 0.
        match t.observe(62.5, 0) {
            GrabberStuckVerdict::Watching {
                over_rate_windows,
                corrupted_windows,
            } => {
                assert!(over_rate_windows >= STUCK_CONFIRM_WINDOWS);
                assert_eq!(corrupted_windows, 0);
            }
            other => panic!("expected Watching with corrupted_windows=0, got {other:?}"),
        }
    }

    #[test]
    fn genuinely_stuck_62fps_plus_persistent_corrupted_fires_after_confirm_windows() {
        // #1128 stuck band: 62.2–62.8 fps + 4 corrupted/window. Realistic path — the grabber runs
        // healthy first, so the tracker already has a corrupted baseline established when the stuck
        // spell begins. From that point STUCK fires after exactly STUCK_CONFIRM_WINDOWS windows.
        let mut t = GrabberStuckTracker::new();
        // healthy baseline window (establishes prev_corrupted_total; keeps both runs at 0):
        assert_eq!(t.observe(60.0, 0), GrabberStuckVerdict::Healthy);
        let mut total = 0u64;
        for w in 1..=STUCK_CONFIRM_WINDOWS {
            total += 4;
            let v = t.observe(62.5, total);
            if w < STUCK_CONFIRM_WINDOWS {
                assert!(
                    !is_stuck(v),
                    "must not be STUCK before window {STUCK_CONFIRM_WINDOWS}: window {w} -> {v:?}"
                );
            } else {
                match v {
                    GrabberStuckVerdict::Stuck {
                        captured_fps,
                        corrupted_delta,
                        windows,
                    } => {
                        assert_eq!(corrupted_delta, 4);
                        assert!((captured_fps - 62.5).abs() < 1e-9);
                        assert_eq!(windows, STUCK_CONFIRM_WINDOWS);
                    }
                    other => {
                        panic!("expected STUCK at window {STUCK_CONFIRM_WINDOWS}, got {other:?}")
                    }
                }
            }
        }
    }

    #[test]
    fn cold_start_into_an_already_stuck_grabber_takes_one_extra_window_for_the_baseline() {
        // A fresh process (post-restart) whose grabber is ALREADY stuck: window 1 is the corrupted
        // baseline (delta unknown -> 0), so the corrupted band confirms one window after the
        // over-rate band. STUCK at STUCK_CONFIRM_WINDOWS + 1 windows from a cold start — a safe
        // ~5 s later than the warm path, never earlier (the baseline can never be undercounted).
        let mut t = GrabberStuckTracker::new();
        let mut total = 100u64; // inherited cumulative counter
        for w in 1..=(STUCK_CONFIRM_WINDOWS + 1) {
            total += 4;
            let v = t.observe(62.5, total);
            assert_eq!(
                is_stuck(v),
                w == STUCK_CONFIRM_WINDOWS + 1,
                "cold start STUCK only at window {} (w={w}): {v:?}",
                STUCK_CONFIRM_WINDOWS + 1
            );
        }
    }

    #[test]
    fn corrupted_without_over_rate_is_a_different_fault_never_this_stuck() {
        // Corrupted frames at a NORMAL 60 fps rate is NOT the over-rate+corruption stuck class —
        // this detector must stay silent (it is not the fault it exists to catch).
        let mut t = GrabberStuckTracker::new();
        let v = drive(&mut t, 60.0, 4, 50);
        assert!(
            !is_stuck(v),
            "corrupted-only at normal rate must not be this STUCK: {v:?}"
        );
    }

    #[test]
    fn transient_single_corrupted_frame_in_healthy_stream_never_stuck() {
        // One isolated corrupted window in an otherwise healthy 60 fps stream must not fire.
        let mut t = GrabberStuckTracker::new();
        assert_eq!(t.observe(60.0, 0), GrabberStuckVerdict::Healthy);
        // one transient corrupted frame:
        let v = t.observe(60.0, 1);
        assert!(!is_stuck(v));
        // back to healthy — corrupted run resets:
        assert_eq!(t.observe(60.0, 1), GrabberStuckVerdict::Healthy);
    }

    #[test]
    fn a_rate_dip_below_floor_breaks_the_over_rate_streak() {
        // Both bands must be CONTINUOUS: a single window under the floor resets the over-rate run,
        // so a subsequent stuck spell must re-accumulate from scratch.
        let mut t = GrabberStuckTracker::with_thresholds(3, OVER_RATE_FPS_FLOOR);
        let mut total = 0u64;
        // two stuck windows...
        for _ in 0..2 {
            total += 4;
            assert!(!is_stuck(t.observe(62.5, total)));
        }
        // ...then one window at healthy rate (still corrupted) breaks the over-rate streak:
        total += 4;
        assert!(!is_stuck(t.observe(60.0, total)));
        // now it needs 3 fresh over-rate windows again:
        for w in 1..=3 {
            total += 4;
            let v = t.observe(62.5, total);
            if w < 3 {
                assert!(
                    !is_stuck(v),
                    "streak must restart after the dip: window {w}"
                );
            } else {
                assert!(is_stuck(v), "STUCK after 3 fresh continuous windows");
            }
        }
    }

    #[test]
    fn a_clean_window_breaks_the_corrupted_streak() {
        // Symmetric to the rate dip: one window with zero corrupted delta resets the corrupted run.
        let mut t = GrabberStuckTracker::with_thresholds(3, OVER_RATE_FPS_FLOOR);
        let mut total = 0u64;
        for _ in 0..2 {
            total += 4;
            assert!(!is_stuck(t.observe(62.5, total)));
        }
        // over-rate but a CLEAN window (no new corrupted): corrupted run resets, over-rate holds.
        let v = t.observe(62.5, total); // total unchanged -> delta 0
        assert!(!is_stuck(v));
        // three fresh corrupted+over-rate windows needed again:
        for w in 1..=3 {
            total += 4;
            let v = t.observe(62.5, total);
            assert_eq!(
                is_stuck(v),
                w == 3,
                "STUCK only on the 3rd fresh window (w={w})"
            );
        }
    }

    #[test]
    fn first_window_baseline_does_not_count_the_cumulative_counter_as_one_windows_corruption() {
        // A fresh process inheriting a large cumulative counter (100) must NOT read that as one
        // window's delta. First observe records the baseline; the delta is 0.
        let mut t = GrabberStuckTracker::new();
        let v = t.observe(62.5, 100);
        match v {
            GrabberStuckVerdict::Watching {
                over_rate_windows,
                corrupted_windows,
            } => {
                assert_eq!(over_rate_windows, 1);
                assert_eq!(
                    corrupted_windows, 0,
                    "cumulative baseline must not count as corruption"
                );
            }
            other => panic!("expected Watching(1,0) on baseline window, got {other:?}"),
        }
    }

    #[test]
    fn recovery_after_stuck_is_not_latched() {
        // After a STUCK spell, a return to healthy rate + no new corruption clears the verdict —
        // no latch (the caller / systemd restart owns the action, not a sticky flag here).
        let mut t = GrabberStuckTracker::with_thresholds(3, OVER_RATE_FPS_FLOOR);
        let mut total = 0u64;
        for _ in 0..3 {
            total += 4;
            t.observe(62.5, total);
        }
        assert!(is_stuck(t.observe(62.5, total + 4)));
        // recover: healthy rate, no new corruption.
        assert_eq!(t.observe(60.0, total + 4), GrabberStuckVerdict::Healthy);
    }

    #[test]
    fn stuck_warn_message_carries_the_grep_marker_and_the_key_values() {
        let m = stuck_warn_message("/dev/video0", 62.5, 4, 6);
        assert!(
            m.contains("#1128 grabber STUCK"),
            "watchdog greps this exact marker: {m}"
        );
        assert!(m.contains("/dev/video0"));
        assert!(m.contains("62.5"));
        assert!(m.contains("6 consecutive report windows"));
        assert!(m.contains("~30s"));
        assert!(m.contains("USB re-enumeration"));
    }

    #[test]
    fn with_thresholds_floors_a_degenerate_zero_confirm_window_to_one() {
        // confirm_windows(0) must not make an empty run "confirmed" — floored to 1.
        let mut t = GrabberStuckTracker::with_thresholds(0, OVER_RATE_FPS_FLOOR);
        // healthy window: no run, must be Healthy (not spuriously Stuck via a 0 threshold).
        assert_eq!(t.observe(60.0, 0), GrabberStuckVerdict::Healthy);
        // one stuck window now meets the floored threshold of 1:
        assert!(is_stuck(t.observe(62.5, 1)));
    }
}
