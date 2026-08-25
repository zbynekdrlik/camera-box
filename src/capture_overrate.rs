//! #1193 — cam2 ShadowCast SUSTAINED-OVER-RATE detector (the 3rd self-heal trigger).
//!
//! ## What this catches that the other two triggers do not
//!
//! The GENKI ShadowCast 2 grabber on cam2 drifts into a SUSTAINED over-rate state: it captures
//! ~61.1 fps (the per-second `cap-1s` buckets read `[61,61,61,61,61]`) and the genlock decimation
//! gate preferentially sheds the surplus content-dupe as the "victim" (`ShedAction::Defer` →
//! `record_shed(true)` → the `dupe_shed` counter, ~6 per 5 s window). Downstream this drops cam2's
//! strih `presentation_cadence` to 0.82–0.94, below the last blocking E2E floor of 0.95 (issue
//! 1170). The ONLY cure is a USB re-enumeration (`echo 0/1 > .../authorized`) — a `systemctl
//! restart` does NOT clear the grabber's internal clock — and that cure DECAYS in ~2 h (live
//! 2026-08-24: re-auth 10:10 → back to 61.1 fps + 6 shed/5 s by 12:20).
//!
//! None of the existing triggers fire on it:
//! - the #656 jitter band's ShadowCast tolerance is 9 % (61.1/60 = 1.83 % never trips it);
//! - the #971 chronic sustained band was DECOUPLED from the reset (issue 909) because the
//!   decimation gate absorbs plain over-rate into exact NDI output;
//! - the #1128 grabber-STUCK detector requires a NONZERO corrupted-frame delta, which this state
//!   does not have (the harm here is the shed CHURN, not corruption).
//!
//! ## The discriminator: over-rate AND dupe-victim shed churn, both sustained
//!
//! This detector keys on the COMBINED signature — a MAJORITY of the last 1-second capture buckets
//! at/above [`OVER_RATE_BUCKET_FPS_FLOOR`] AND a per-window dupe-victim shed count at/above
//! [`OVER_RATE_SHED_CHURN_MIN`] — held for [`OVER_RATE_CONFIRM_WINDOWS`] consecutive report windows.
//! The shed-churn band is the DISCRIMINATOR, exactly as the corrupted band is for the #1128 STUCK
//! detector: a benign over-rate wobble that the decimation gate cleanly absorbs sheds ~0 dupe
//! victims, so its churn run stays 0 and it can NEVER reach [`CaptureOverRateVerdict::OverRate`] —
//! which keeps the false-positive rate at zero, the whole reason the #909/#914 reset-spam class is
//! not re-introduced. Bands calibrated from the ticket's disjoint live data (healthy 60.0 fps /
//! 0 shed; sick 61.1 fps / ~6 shed per 5 s window); the churn threshold sits at the midpoint.
//!
//! ## Tier-0 pure
//!
//! No probe deps, no I/O, no sysfs, no `tracing` — pure decision + formatting + the cooldown
//! predicate, so it unit-tests on default features. `src/main.rs`'s 5 s report block feeds each
//! window's `emit_ring.capture_buckets()` + the drained `dupe_shed` count into one
//! [`CaptureOverRateTracker`], logs the report-only [`over_rate_warn_message`] marker on an
//! [`CaptureOverRateVerdict::OverRate`], and — only when the opt-in
//! `CAMERA_BOX_GRABBER_OVERRATE_SELFHEAL` env is set AND the shared
//! `capture_rate_selfheal::cooldown_elapsed` floor permits (#1201 moved the predicate there) —
//! funnels that into the shared `capture_rate_selfheal::attempt_self_heal` throttle + USB-reset path via the
//! `OVER_RATE_SELF_HEAL_MESSAGES` const. A future dev1 alert watchdog can grep the exact
//! `#1193 grabber OVER-RATE` marker, keeping ONE source of truth (Rust decides; the watchdog would
//! only relay), the same shape as the #1128 `#1128 grabber STUCK` marker.

/// Per-second capture-bucket floor (frames captured in one 1-second window). A bucket at or above
/// this is "over-rate" for that second. Calibrated from the ticket's disjoint live bands: healthy
/// 60.0 fps → 60/second buckets; sick 61.1 fps → 61/second buckets. `61` sits cleanly above the
/// healthy 60 and at the sick floor, so a healthy card's occasional single 61-jitter bucket never
/// forms a MAJORITY (see [`CaptureOverRateTracker::observe`]).
pub const OVER_RATE_BUCKET_FPS_FLOOR: u32 = 61;

/// Minimum number of 1-second buckets that must be PRESENT before a window is judged over-rate —
/// a cold-start guard so a ring holding only 1–2 completed buckets can never let a single 61-bucket
/// masquerade as "the majority". The production `emit_rate_ring` reports up to 5 buckets.
pub const OVER_RATE_MIN_BUCKETS_PRESENT: usize = 3;

/// The dupe-victim shed-churn threshold (frames shed as the preferred content-dupe victim per 5 s
/// report window). At or above this the churn band holds. The midpoint of the disjoint live bands
/// (healthy 0 shed, sick ~6 shed): `3` is well above the healthy 0 and well below the sick ~6, so
/// neither band's normal spread crosses it. This is the DISCRIMINATOR from a benign over-rate
/// wobble (which sheds 0), mirroring the #1128 corrupted band's role.
pub const OVER_RATE_SHED_CHURN_MIN: u64 = 3;

/// Consecutive 5 s report windows BOTH bands (over-rate majority AND shed churn) must hold before
/// the grabber is declared over-rate. `60` windows = ~5 minutes: far longer than #1128's 6-window
/// (~30 s) STUCK confirm — deliberately, because the over-rate+shed signature is SOFTER than
/// over-rate+corrupted (both inputs are ordinary pacing signals, just elevated), so it warrants
/// more confirmation — yet well short of the #971 chronic band's 180 windows (15 min). No benign
/// transient over-rate wobble (which self-recovers within seconds-to-a-minute, per #666) can hold
/// both bands for 5 continuous minutes.
pub const OVER_RATE_CONFIRM_WINDOWS: u32 = 60;

/// Minimum seconds between over-rate self-heal ATTEMPTS — a per-trigger cooldown FLOOR stricter than
/// the shared 10-min `capture_rate_selfheal` throttle, checked in `src/main.rs` against the SHARED
/// self-heal state file's `last_heal_epoch_s` (by ANY trigger) BEFORE `attempt_self_heal`, so the
/// shared throttle is left UNCHANGED (the other two triggers are untouched). `1800` (30 min): the
/// over-rate cure decays in ~2 h, so a healthy cycle re-triggers only ~every 2 h — this floor only
/// bites the pathological case where a reset fails to hold, bounding it to at most 2 attempts/hour.
pub const OVERRATE_MIN_HEAL_INTERVAL_S: u64 = 1800;

/// This window's verdict from [`CaptureOverRateTracker::observe`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CaptureOverRateVerdict {
    /// Neither band is currently building — nothing to act on.
    Healthy,
    /// At least one band is building toward confirmation, but not both are confirmed yet.
    Watching {
        over_rate_windows: u32,
        churn_windows: u32,
    },
    /// BOTH bands have held for [`OVER_RATE_CONFIRM_WINDOWS`] consecutive windows — the grabber is
    /// in the sustained over-rate + shed-churn state. `captured_max_bucket` is the peak 1-second
    /// capture count this window, `dupe_shed` this window's shed churn, `windows` the (equal-or-
    /// shorter) confirmed run length — all for the report line.
    OverRate {
        captured_max_bucket: u32,
        dupe_shed: u64,
        windows: u32,
    },
}

/// Per-process tracker: one instance lives for the lifetime of a capture loop. State is deliberately
/// NOT persisted — a self-heal USB reset exits the process, so a fresh process (fresh tracker, fresh
/// runs) is exactly the right reset of these counters.
#[derive(Debug, Clone)]
pub struct CaptureOverRateTracker {
    confirm_windows: u32,
    bucket_fps_floor: u32,
    min_buckets_present: usize,
    churn_min: u64,
    over_rate_run: u32,
    churn_run: u32,
}

impl Default for CaptureOverRateTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureOverRateTracker {
    /// A tracker with the production thresholds ([`OVER_RATE_CONFIRM_WINDOWS`],
    /// [`OVER_RATE_BUCKET_FPS_FLOOR`], [`OVER_RATE_MIN_BUCKETS_PRESENT`], [`OVER_RATE_SHED_CHURN_MIN`]).
    pub fn new() -> Self {
        Self::with_thresholds(
            OVER_RATE_CONFIRM_WINDOWS,
            OVER_RATE_BUCKET_FPS_FLOOR,
            OVER_RATE_MIN_BUCKETS_PRESENT,
            OVER_RATE_SHED_CHURN_MIN,
        )
    }

    /// A tracker with explicit thresholds (tests use short windows; `confirm_windows` is floored to
    /// 1 so a degenerate 0 can never make an empty run "confirmed", and `min_buckets_present` to 1).
    pub fn with_thresholds(
        confirm_windows: u32,
        bucket_fps_floor: u32,
        min_buckets_present: usize,
        churn_min: u64,
    ) -> Self {
        Self {
            confirm_windows: confirm_windows.max(1),
            bucket_fps_floor,
            min_buckets_present: min_buckets_present.max(1),
            churn_min,
            over_rate_run: 0,
            churn_run: 0,
        }
    }

    /// Feed one 5 s report window: the window's per-second capture buckets
    /// (`emit_rate_ring::EmitRateRing::capture_buckets`, oldest first) and the drained dupe-victim
    /// shed count (`dupe_decimation::DecimationGate::take_shed_counts().0`). Returns this window's
    /// [`CaptureOverRateVerdict`].
    ///
    /// Band bookkeeping (both must be CONTINUOUS — a single failing window resets that band's run):
    /// - over-rate run: `+1` while a MAJORITY of the present buckets are `>= bucket_fps_floor`
    ///   (needs `>= min_buckets_present` buckets present so a cold-start 1–2-bucket ring cannot
    ///   qualify), else reset to 0.
    /// - churn run: `+1` while `dupe_shed >= churn_min`, else reset to 0 — the discriminator that
    ///   keeps a benign over-rate wobble (0 shed) from ever confirming.
    pub fn observe(&mut self, cap_buckets: &[u32], dupe_shed: u64) -> CaptureOverRateVerdict {
        let present = cap_buckets.len();
        let over_count = cap_buckets
            .iter()
            .filter(|&&b| b >= self.bucket_fps_floor)
            .count();
        // "most buckets" = a strict MAJORITY of the buckets present, with a minimum-present guard.
        let over_rate_window = present >= self.min_buckets_present && over_count * 2 > present;
        if over_rate_window {
            self.over_rate_run = self.over_rate_run.saturating_add(1);
        } else {
            self.over_rate_run = 0;
        }

        let churn_window = dupe_shed >= self.churn_min;
        if churn_window {
            self.churn_run = self.churn_run.saturating_add(1);
        } else {
            self.churn_run = 0;
        }

        // OVER-RATE only when BOTH bands are confirmed. The churn band is the discriminator that
        // keeps a benign over-rate wobble (0 shed; absorbed by the decimation gate) from ever
        // reaching a reset.
        if self.over_rate_run >= self.confirm_windows && self.churn_run >= self.confirm_windows {
            return CaptureOverRateVerdict::OverRate {
                captured_max_bucket: cap_buckets.iter().copied().max().unwrap_or(0),
                dupe_shed,
                windows: self.over_rate_run.min(self.churn_run),
            };
        }
        if self.over_rate_run > 0 || self.churn_run > 0 {
            return CaptureOverRateVerdict::Watching {
                over_rate_windows: self.over_rate_run,
                churn_windows: self.churn_run,
            };
        }
        CaptureOverRateVerdict::Healthy
    }
}

/// The report-only WARN line the appliance emits on a [`CaptureOverRateVerdict::OverRate`]. The
/// exact substring `#1193 grabber OVER-RATE` is the marker a future dev1 alert watchdog would grep
/// — keep it stable, and distinct from every existing anchor (`#1128 grabber STUCK`, `#656
/// …DEFECTIVE`, `#971 …CHRONIC`, `#717 …SUSTAINED`, `#663 self-heal`). Pure formatting, directly
/// unit-testable.
pub fn over_rate_warn_message(
    video_device_path: &str,
    captured_max_bucket: u32,
    dupe_shed: u64,
    windows: u32,
) -> String {
    format!(
        "#1193 grabber OVER-RATE: {video_device_path} captured >= {OVER_RATE_BUCKET_FPS_FLOOR} fps in the majority of 1-second buckets (peak {captured_max_bucket} fps) WITH dupe-victim shed churn ({dupe_shed}/window >= {OVER_RATE_SHED_CHURN_MIN}) \
         sustained for {windows} consecutive report windows (~{}s) — the ShadowCast sustained-over-rate state whose manual USB re-auth cure decays in ~2h (see #1193, #1170); a `systemctl restart` does NOT clear it, only a USB re-enumeration does. \
         Report-only unless CAMERA_BOX_GRABBER_OVERRATE_SELFHEAL is set.",
        windows as u64 * 5
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full window of 5 buckets all at `fps`.
    fn window(fps: u32) -> Vec<u32> {
        vec![fps; 5]
    }

    /// Drive `n` identical windows through the tracker, returning the last verdict.
    fn drive(
        t: &mut CaptureOverRateTracker,
        cap_buckets: &[u32],
        dupe_shed: u64,
        n: u32,
    ) -> CaptureOverRateVerdict {
        let mut last = CaptureOverRateVerdict::Healthy;
        for _ in 0..n {
            last = t.observe(cap_buckets, dupe_shed);
        }
        last
    }

    fn is_over_rate(v: CaptureOverRateVerdict) -> bool {
        matches!(v, CaptureOverRateVerdict::OverRate { .. })
    }

    #[test]
    fn healthy_60fps_zero_shed_never_over_rate() {
        // Healthy band: 60/second buckets, 0 dupe-victim shed. Must stay Healthy indefinitely.
        let mut t = CaptureOverRateTracker::new();
        for _ in 0..(OVER_RATE_CONFIRM_WINDOWS + 20) {
            assert_eq!(t.observe(&window(60), 0), CaptureOverRateVerdict::Healthy);
        }
    }

    #[test]
    fn sick_band_triggers_after_confirm_windows() {
        // Sick band: [61,61,61,61,61] + 6 dupe-victim sheds/window. OverRate fires after exactly
        // OVER_RATE_CONFIRM_WINDOWS consecutive windows, not before.
        let mut t = CaptureOverRateTracker::new();
        let buckets = window(61);
        for w in 1..=OVER_RATE_CONFIRM_WINDOWS {
            let v = t.observe(&buckets, 6);
            if w < OVER_RATE_CONFIRM_WINDOWS {
                assert!(
                    !is_over_rate(v),
                    "must not be OverRate before window {OVER_RATE_CONFIRM_WINDOWS}: window {w} -> {v:?}"
                );
            } else {
                match v {
                    CaptureOverRateVerdict::OverRate {
                        captured_max_bucket,
                        dupe_shed,
                        windows,
                    } => {
                        assert_eq!(captured_max_bucket, 61);
                        assert_eq!(dupe_shed, 6);
                        assert_eq!(windows, OVER_RATE_CONFIRM_WINDOWS);
                    }
                    other => panic!(
                        "expected OverRate at window {OVER_RATE_CONFIRM_WINDOWS}, got {other:?}"
                    ),
                }
            }
        }
    }

    #[test]
    fn benign_over_rate_wobble_with_zero_shed_never_over_rate() {
        // THE discriminator test (the #909/#1128 anti-reset-spam lesson): a card over-rating at 61
        // fps but shedding ZERO dupe victims (the decimation gate absorbs it cleanly) must NEVER be
        // declared OverRate, no matter how long it holds.
        let mut t = CaptureOverRateTracker::new();
        let buckets = window(61);
        for _ in 0..(OVER_RATE_CONFIRM_WINDOWS + 40) {
            let v = t.observe(&buckets, 0);
            assert!(
                !is_over_rate(v),
                "over-rate with 0 shed churn must never be OverRate: {v:?}"
            );
        }
        // The over-rate run IS building (Watching), the churn run is pinned at 0.
        match t.observe(&buckets, 0) {
            CaptureOverRateVerdict::Watching {
                over_rate_windows,
                churn_windows,
            } => {
                assert!(over_rate_windows >= OVER_RATE_CONFIRM_WINDOWS);
                assert_eq!(churn_windows, 0);
            }
            other => panic!("expected Watching with churn_windows=0, got {other:?}"),
        }
    }

    #[test]
    fn shed_churn_at_normal_rate_is_a_different_state_never_over_rate() {
        // Shed churn at a NORMAL 60 fps rate is not the over-rate state this detector exists to
        // catch — the over-rate band never builds, so it stays silent.
        let mut t = CaptureOverRateTracker::new();
        let v = drive(&mut t, &window(60), 6, OVER_RATE_CONFIRM_WINDOWS + 10);
        assert!(
            !is_over_rate(v),
            "shed churn without over-rate must not be OverRate: {v:?}"
        );
    }

    #[test]
    fn churn_just_below_threshold_never_over_rate() {
        // A window with dupe_shed just under OVER_RATE_SHED_CHURN_MIN (the benign wobble that sheds
        // 1–2 victims) never confirms the churn band, even at a full over-rate.
        let mut t = CaptureOverRateTracker::new();
        let v = drive(
            &mut t,
            &window(61),
            OVER_RATE_SHED_CHURN_MIN - 1,
            OVER_RATE_CONFIRM_WINDOWS + 10,
        );
        assert!(
            !is_over_rate(v),
            "sub-threshold churn must not be OverRate: {v:?}"
        );
    }

    #[test]
    fn a_single_healthy_window_breaks_the_over_rate_streak() {
        // Both bands must be CONTINUOUS: one window at healthy rate resets the over-rate run, so a
        // subsequent sick spell must re-accumulate from scratch.
        let mut t = CaptureOverRateTracker::with_thresholds(
            3,
            OVER_RATE_BUCKET_FPS_FLOOR,
            3,
            OVER_RATE_SHED_CHURN_MIN,
        );
        for _ in 0..2 {
            assert!(!is_over_rate(t.observe(&window(61), 6)));
        }
        // one healthy-rate window (still shedding) breaks the over-rate streak:
        assert!(!is_over_rate(t.observe(&window(60), 6)));
        // now it needs 3 fresh over-rate windows again:
        for w in 1..=3 {
            let v = t.observe(&window(61), 6);
            if w < 3 {
                assert!(
                    !is_over_rate(v),
                    "streak must restart after the dip: window {w}"
                );
            } else {
                assert!(is_over_rate(v), "OverRate after 3 fresh continuous windows");
            }
        }
    }

    #[test]
    fn a_zero_shed_window_breaks_the_churn_streak() {
        // Symmetric to the rate dip: one window with 0 shed churn resets the churn run.
        let mut t = CaptureOverRateTracker::with_thresholds(
            3,
            OVER_RATE_BUCKET_FPS_FLOOR,
            3,
            OVER_RATE_SHED_CHURN_MIN,
        );
        for _ in 0..2 {
            assert!(!is_over_rate(t.observe(&window(61), 6)));
        }
        // over-rate but a clean (0-shed) window: churn run resets, over-rate run holds.
        assert!(!is_over_rate(t.observe(&window(61), 0)));
        for w in 1..=3 {
            let v = t.observe(&window(61), 6);
            assert_eq!(
                is_over_rate(v),
                w == 3,
                "OverRate only on the 3rd fresh window (w={w})"
            );
        }
    }

    #[test]
    fn a_minority_of_over_rate_buckets_is_not_a_majority() {
        // "most buckets" = a strict majority. Two 61-buckets out of five (with jitter) is a
        // minority — the over-rate band must not build.
        let mut t = CaptureOverRateTracker::new();
        let jittery = vec![61u32, 61, 60, 60, 60]; // 2 of 5 over -> minority
        for _ in 0..(OVER_RATE_CONFIRM_WINDOWS + 5) {
            let v = t.observe(&jittery, 6);
            assert!(
                !is_over_rate(v),
                "a minority of over-rate buckets must not confirm: {v:?}"
            );
        }
    }

    #[test]
    fn a_bare_majority_of_over_rate_buckets_does_confirm() {
        // Three 61-buckets of five IS a majority ("most"), so with churn present it confirms.
        let mut t = CaptureOverRateTracker::new();
        let majority = vec![61u32, 61, 61, 60, 60]; // 3 of 5 over -> majority
        let v = drive(&mut t, &majority, 6, OVER_RATE_CONFIRM_WINDOWS);
        match v {
            CaptureOverRateVerdict::OverRate {
                captured_max_bucket,
                ..
            } => {
                assert_eq!(captured_max_bucket, 61);
            }
            other => panic!("a bare majority + churn must confirm OverRate, got {other:?}"),
        }
    }

    #[test]
    fn a_cold_start_ring_with_too_few_buckets_never_confirms() {
        // A cold-start ring holding fewer than OVER_RATE_MIN_BUCKETS_PRESENT completed buckets must
        // never let a single 61-bucket masquerade as the majority.
        let mut t = CaptureOverRateTracker::new();
        let one_bucket = vec![61u32]; // present=1 < min-present 3
        for _ in 0..(OVER_RATE_CONFIRM_WINDOWS + 5) {
            let v = t.observe(&one_bucket, 6);
            assert!(
                !is_over_rate(v),
                "too few buckets present must not confirm: {v:?}"
            );
        }
    }

    #[test]
    fn an_empty_bucket_window_is_healthy_on_the_over_rate_band() {
        // No completed buckets (0 present) can never be over-rate; with 0 shed too it is Healthy.
        let mut t = CaptureOverRateTracker::new();
        assert_eq!(t.observe(&[], 0), CaptureOverRateVerdict::Healthy);
    }

    #[test]
    fn recovery_after_over_rate_is_not_latched() {
        // After an OverRate spell, a return to healthy rate + no shed clears the verdict — no latch
        // (the caller / systemd restart owns the action, not a sticky flag here).
        let mut t = CaptureOverRateTracker::with_thresholds(
            3,
            OVER_RATE_BUCKET_FPS_FLOOR,
            3,
            OVER_RATE_SHED_CHURN_MIN,
        );
        assert!(is_over_rate(drive(&mut t, &window(61), 6, 3)));
        assert_eq!(t.observe(&window(60), 0), CaptureOverRateVerdict::Healthy);
    }

    #[test]
    fn with_thresholds_floors_a_degenerate_zero_confirm_window_to_one() {
        // confirm_windows(0) must not make an empty run "confirmed" — floored to 1.
        let mut t = CaptureOverRateTracker::with_thresholds(
            0,
            OVER_RATE_BUCKET_FPS_FLOOR,
            3,
            OVER_RATE_SHED_CHURN_MIN,
        );
        // one sick window now meets the floored threshold of 1:
        assert!(is_over_rate(t.observe(&window(61), 6)));
    }

    #[test]
    fn over_rate_warn_message_carries_the_grep_marker_and_key_values() {
        let m = over_rate_warn_message("/dev/video0", 61, 6, 60);
        assert!(
            m.contains("#1193 grabber OVER-RATE"),
            "a future watchdog greps this exact marker: {m}"
        );
        assert!(m.contains("/dev/video0"));
        assert!(m.contains("peak 61 fps"));
        assert!(m.contains("6/window"));
        assert!(m.contains("60 consecutive report windows"));
        assert!(m.contains("~300s"));
        assert!(m.contains("CAMERA_BOX_GRABBER_OVERRATE_SELFHEAL"));
        // Must NOT collide with any existing dev1-watchdog grep anchor.
        assert!(!m.contains("#1128 grabber STUCK"));
        assert!(!m.contains("#663 self-heal"));
        assert!(!m.contains("DEFECTIVE"));
        assert!(!m.contains("CHRONIC"));
    }
}
