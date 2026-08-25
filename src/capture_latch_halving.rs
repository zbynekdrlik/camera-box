//! #1200 — cam3 ShadowCast LATCH-HALVING detector (the 4th self-heal trigger).
//!
//! ## What this catches that the other three triggers do not
//!
//! The GENKI ShadowCast 2 grabber on cam3 drifts into a LATCH-HALVING state: it delivers a
//! byte-perfect 60 fps at a correct pace, but sources only **15 distinct camera frames per second**
//! into that stream — it latches every second frame of the 30 fps camera and re-clocks each
//! surviving unique frame **4×** (an exact 2:1 halving of the camera). Live-confirmed 2026-08-25
//! (issue 1198 latest comment + issue 1110 last two comments; 3 full E2E runs, RUN_ID 1717119205 /
//! 121536020 / 1915710172): cam3 leg uniformity 0.46–0.49, delta histogram `{0:~425, 4:~417}` =
//! 15 unique/s in a 60 fps stream. The painter ticks a clean 60/s, the network delivered a full
//! `received` 60.0/s (run 2), cam3's service emits 60/60 — so the halving is in the CAPTURE layer,
//! not the camera (the exact 2:1 ratio excludes a native 15 fps mode), the painter, or the network.
//! A `systemctl restart` re-opens the V4L2 node but does not re-negotiate the grabber clock; a USB
//! re-enumeration would — and even that did NOT cure cam3 on 2026-08-25, so this detector's value is
//! DETECTION/alerting, not cure.
//!
//! None of the existing triggers fire on it:
//! - the #1193 sustained OVER-RATE detector ([`crate::capture_overrate`]) needs an over-rate
//!   majority AND dupe-victim shed churn — cam3 captures exactly 60 fps (not over-rate) and the
//!   decimation gate sheds nothing, so neither band builds;
//! - the #1128 grabber-STUCK detector ([`crate::grabber_stuck`]) needs a NONZERO corrupted-frame
//!   delta — the harm here is repetition, not corruption (0 corrupted);
//! - the #656/#971 capture-rate bands never fire — 60 fps is dead-centre in the negotiated rate.
//!
//! ## The discriminator: the capture-side byte-identical dupe FRACTION
//!
//! The `#889` dupe-preferring decimation path already computes a byte-exact `content_hash`
//! ([`crate::dupe_decimation::dupe_content_sig`], FNV-1a over sampled rows) for every captured frame
//! and compares it to the previous frame's (`exact_dupe`). Reuse that signal: over a 5 s report
//! window, count how many captured frames were byte-identical to their predecessor, and take the
//! FRACTION. It separates the two states cleanly, INDEPENDENT of the decimation gate's own
//! shed/emit decisions:
//! - HEALTHY 30 fps-camera-into-60 fps-capture: each unique frame captured 2× → dupe fraction ~0.5.
//! - LATCH-HALVED cam3 sick: each unique frame captured 4× (15 unique/s in 60 fps) → dupe fraction
//!   ~0.75.
//!
//! A window is HALVED when the dupe fraction is in the CLOSED band from
//! [`HALVED_DUPE_FRACTION_MIN`] (0.70) to [`HALVED_DUPE_FRACTION_MAX`] (0.90), AND the window has
//! at least [`HALVING_MIN_CAPTURES_PER_WINDOW`] captures (a cold-start / stalled-capture guard),
//! held for [`HALVING_CONFIRM_WINDOWS`] consecutive report windows. The floor separates it from the
//! HEALTHY band (`<= `[`HEALTHY_DUPE_FRACTION_MAX`]` (0.55)`, a dead-zone between); the CEILING
//! excludes a FROZEN / no-signal source (fraction ~1.0 — a powered-off camera behind the splitter or
//! a wedged HDMI feed), which is a DIFFERENT failure (frozen_leg / capture_wedge, #945). These are
//! exactly the discriminator roles the #1128 corrupted band and the #1193 shed-churn band play, so
//! neither a healthy card's ~0.5 fraction NOR a frozen source's ~1.0 can confirm (the whole reason
//! the #909/#914 reset-spam class is not re-introduced). Both bounds are enforced at compile time by
//! `const _: () = assert!(...)` next to the constants.
//!
//! **Scope limits (both are why the reset ships default-OFF):**
//! - GENLOCK-PACED CAPTURE ONLY: the dupe fraction is counted in `src/main.rs` only where the
//!   decimation path computes `content_hash` (inside `if out_interval_ns > 0`), so on an unpaced box
//!   the tracker is never fed and stays `Healthy`. Correct on the rig (every camera-box is paced).
//! - The bands assume a 30 fps camera captured at 60 fps (healthy 0.5 / halved 0.75). A native-60p
//!   camera halved reads 0.5 (invisible) and a native-15 fps camera mode reads 0.75 (indistinguishable
//!   from the defect); the band is not yet per-`GrabberModel`/camera-fps. Detection-only default is
//!   safe; a LIVE reset gate would need a per-model band first.
//!
//! ## Tier-0 pure
//!
//! No probe deps, no I/O, no sysfs, no `tracing` — pure decision + formatting (the cooldown
//! predicate moved to `capture_rate_selfheal` in #1201), so it unit-tests on default features.
//! `src/main.rs`'s capture loop counts the
//! capture-side byte-identical dupe fraction (reusing the SAME `content_hash` the #889 path already
//! computes — NO change to the decimation gate), feeds each 5 s window's `(dupe_captures,
//! total_captures)` into one [`CaptureLatchHalvingTracker`], logs the report-only
//! [`latch_halving_warn_message`] marker on a [`CaptureLatchHalvingVerdict::Halved`], and — only
//! when the opt-in `CAMERA_BOX_GRABBER_HALVING_SELFHEAL` env is set AND the shared
//! `capture_rate_selfheal::cooldown_elapsed` floor permits (#1201 moved the predicate there)
//! — funnels that into the shared `capture_rate_selfheal::attempt_self_heal` throttle + USB-reset
//! path via the `LATCH_HALVING_SELF_HEAL_MESSAGES` const. A future dev1 alert watchdog can grep the
//! exact `#1200 grabber LATCH-HALVING` marker, keeping ONE source of truth (Rust decides; the
//! watchdog would only relay), the same shape as the #1128 `#1128 grabber STUCK` marker.

/// The dupe-fraction floor (byte-identical captures / total captures per window). At or above this
/// the window is "halved". Calibrated from the ticket's disjoint bands: healthy 30fps-into-60fps
/// ~0.5 (each unique 2×), latch-halved ~0.75 (each unique 4×). `0.70` sits in the wide gap between
/// them, well above the healthy ~0.5 and just below the sick ~0.75, so neither band's normal spread
/// crosses it.
pub const HALVED_DUPE_FRACTION_MIN: f64 = 0.70;

/// The healthy-band ceiling — documentary + the non-overlap invariant. A healthy
/// 30fps-into-60fps card's dupe fraction is ~0.5, comfortably below this; a fraction in the
/// `(HEALTHY_DUPE_FRACTION_MAX, HALVED_DUPE_FRACTION_MIN)` dead-zone is neither — the detector never
/// acts on it. Kept strictly below [`HALVED_DUPE_FRACTION_MIN`] by the compile-time assert below.
pub const HEALTHY_DUPE_FRACTION_MAX: f64 = 0.55;

// Non-overlapping-band invariant, enforced at COMPILE time (mirrors the wedge-watchdog
// `const _: () = assert!(...)` pattern — stronger than a runtime test and clippy-clean). If a
// future tune ever crosses the two bands, the BUILD fails here.
const _: () = assert!(HEALTHY_DUPE_FRACTION_MAX < HALVED_DUPE_FRACTION_MIN);

/// The halved-band CEILING — a frozen / no-signal-source EXCLUSION. A powered-off camera behind the
/// HDMI splitter (a static no-signal pattern) or any wedged HDMI feed delivers byte-identical
/// consecutive frames → a dupe fraction near 1.0. That is a DIFFERENT failure (frozen_leg /
/// capture_wedge, #945), NOT latch-halving, so the halved band is a CLOSED interval
/// `[HALVED_DUPE_FRACTION_MIN, HALVED_DUPE_FRACTION_MAX]` — exactly the false-positive-exclusion role
/// the #1128 corrupted band and #1193 shed-churn band play. `0.90` keeps the real latch-halving
/// ratios in (4×→0.75, 5×→0.80, 6×→0.833) while excluding a frozen source (≥~0.99). Fixed sanity
/// ceiling, not a per-deployment tunable.
pub const HALVED_DUPE_FRACTION_MAX: f64 = 0.90;

// The halved band must itself be a non-empty interval (floor strictly below ceiling), locked at
// COMPILE time like the healthy/halved non-overlap above.
const _: () = assert!(HALVED_DUPE_FRACTION_MIN < HALVED_DUPE_FRACTION_MAX);

/// Minimum captured frames a window must contain before its dupe fraction is judged — a cold-start /
/// stalled-capture guard so a window with only a handful of captures (a genuine capture stall,
/// handled by the #707/#945 paths, NOT latch-halving) can never produce a spurious high fraction.
/// A healthy genlock box captures ~300 frames per 5 s window (60 fps); `150` is half of that, well
/// clear of the steady rate yet high enough to reject a stalled/cold window.
pub const HALVING_MIN_CAPTURES_PER_WINDOW: u64 = 150;

/// Consecutive 5 s report windows the halved band must hold before the grabber is declared
/// latch-halving. `60` windows = ~5 minutes — the same confirmation depth as the #1193 over-rate
/// trigger (the latch-halving signature is a steady-state capture defect, not a transient), far
/// longer than #1128's 6-window (~30 s) STUCK confirm, and well short of the #971 chronic band's
/// 180 windows.
pub const HALVING_CONFIRM_WINDOWS: u32 = 60;

/// Minimum seconds between latch-halving self-heal ATTEMPTS — a per-trigger cooldown FLOOR stricter
/// than the shared 10-min `capture_rate_selfheal` throttle, checked in `src/main.rs` against the
/// SHARED self-heal state file's `last_heal_epoch_s` (set by ANY trigger) BEFORE `attempt_self_heal`,
/// so the shared throttle is left UNCHANGED (the other three triggers are untouched). `1800` (30 min)
/// mirrors the #1193 over-rate floor: the USB re-auth cure is unproven for latch-halving (it did NOT
/// cure cam3 on 2026-08-25), so this floor bounds a default-OFF canary to at most ~2 attempts/hour.
pub const HALVING_MIN_HEAL_INTERVAL_S: u64 = 1800;

/// This window's verdict from [`CaptureLatchHalvingTracker::observe`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CaptureLatchHalvingVerdict {
    /// The halved band is not currently building — nothing to act on.
    Healthy,
    /// The halved band is building toward confirmation but has not held long enough yet.
    Watching { halved_windows: u32 },
    /// The halved band has held for [`HALVING_CONFIRM_WINDOWS`] consecutive windows — the grabber
    /// is delivering each unique frame ~4× at a correct pace. `dupe_fraction` / `dupe_captures` /
    /// `total_captures` are this window's values and `windows` the confirmed run length — all for
    /// the report line.
    Halved {
        dupe_fraction: f64,
        dupe_captures: u64,
        total_captures: u64,
        windows: u32,
    },
}

/// Per-process tracker: one instance lives for the lifetime of a capture loop. State is deliberately
/// NOT persisted — a self-heal USB reset exits the process, so a fresh process (fresh tracker) is
/// exactly the right reset of this counter.
#[derive(Debug, Clone)]
pub struct CaptureLatchHalvingTracker {
    confirm_windows: u32,
    halved_fraction_min: f64,
    halved_fraction_max: f64,
    min_captures: u64,
    halved_run: u32,
}

impl Default for CaptureLatchHalvingTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureLatchHalvingTracker {
    /// A tracker with the production thresholds ([`HALVING_CONFIRM_WINDOWS`],
    /// [`HALVED_DUPE_FRACTION_MIN`], [`HALVED_DUPE_FRACTION_MAX`], [`HALVING_MIN_CAPTURES_PER_WINDOW`]).
    pub fn new() -> Self {
        Self::with_thresholds(
            HALVING_CONFIRM_WINDOWS,
            HALVED_DUPE_FRACTION_MIN,
            HALVED_DUPE_FRACTION_MAX,
            HALVING_MIN_CAPTURES_PER_WINDOW,
        )
    }

    /// A tracker with explicit thresholds (tests use short windows; `confirm_windows` is floored to
    /// 1 so a degenerate 0 can never make an empty run "confirmed", and `min_captures` to 1).
    pub fn with_thresholds(
        confirm_windows: u32,
        halved_fraction_min: f64,
        halved_fraction_max: f64,
        min_captures: u64,
    ) -> Self {
        Self {
            confirm_windows: confirm_windows.max(1),
            halved_fraction_min,
            halved_fraction_max,
            min_captures: min_captures.max(1),
            halved_run: 0,
        }
    }

    /// Feed one 5 s report window: the count of captured frames that were byte-identical to their
    /// predecessor (`dupe_captures`) and the total captures that window (`total_captures`). Returns
    /// this window's [`CaptureLatchHalvingVerdict`].
    ///
    /// The halved run is CONTINUOUS — a single non-halved window resets it. A window is halved when
    /// it has at least `min_captures` captures (cold-start / stalled-capture guard) AND its dupe
    /// fraction is in the CLOSED band `[halved_fraction_min, halved_fraction_max]` — the upper bound
    /// excludes a frozen / no-signal source (~1.0), which is a different failure (frozen_leg /
    /// capture_wedge). The min-captures guard also makes the division safe
    /// (`total_captures >= min_captures >= 1`).
    pub fn observe(
        &mut self,
        dupe_captures: u64,
        total_captures: u64,
    ) -> CaptureLatchHalvingVerdict {
        let frac = if total_captures >= self.min_captures {
            dupe_captures as f64 / total_captures as f64
        } else {
            0.0
        };
        let halved_window = total_captures >= self.min_captures
            && frac >= self.halved_fraction_min
            && frac <= self.halved_fraction_max;
        if halved_window {
            self.halved_run = self.halved_run.saturating_add(1);
        } else {
            self.halved_run = 0;
        }

        if self.halved_run >= self.confirm_windows {
            // Reached only when this window was halved (a non-halved window zeroes halved_run and
            // confirm_windows >= 1), so `frac` is the real fraction here.
            return CaptureLatchHalvingVerdict::Halved {
                dupe_fraction: frac,
                dupe_captures,
                total_captures,
                windows: self.halved_run,
            };
        }
        if self.halved_run > 0 {
            return CaptureLatchHalvingVerdict::Watching {
                halved_windows: self.halved_run,
            };
        }
        CaptureLatchHalvingVerdict::Healthy
    }
}

/// The report-only WARN line the appliance emits on a [`CaptureLatchHalvingVerdict::Halved`]. The
/// exact substring `#1200 grabber LATCH-HALVING` is the marker a future dev1 alert watchdog would
/// grep — keep it stable, and distinct from every existing anchor (`#1128 grabber STUCK`, `#1193
/// grabber OVER-RATE`, `#656 …DEFECTIVE`, `#971 …CHRONIC`, `#663 self-heal`). Pure formatting,
/// directly unit-testable.
pub fn latch_halving_warn_message(
    video_device_path: &str,
    dupe_fraction: f64,
    dupe_captures: u64,
    total_captures: u64,
    windows: u32,
) -> String {
    // copies-per-unique estimate (total / unique). Guarded: a fully-frozen window (unique == 0) is
    // a DIFFERENT failure (frozen_leg / capture_wedge), never this detector's ~0.75 signature — show
    // ">4" rather than dividing by zero.
    let unique = total_captures.saturating_sub(dupe_captures);
    let copies_per_unique = if unique > 0 {
        format!("~{:.1}", total_captures as f64 / unique as f64)
    } else {
        ">4".to_string()
    };
    format!(
        "#1200 grabber LATCH-HALVING: {video_device_path} captured a byte-identical-dupe fraction of {dupe_fraction:.2} ({dupe_captures}/{total_captures} frames, {copies_per_unique} copies per unique) >= {HALVED_DUPE_FRACTION_MIN:.2} \
         sustained for {windows} consecutive report windows (~{}s) — the cam3 ShadowCast latch-halving state (each unique camera frame delivered ~4x at a correct 60fps pace, 15 unique/s), which the #1193 over-rate and #1128 stuck detectors both miss. \
         A `systemctl restart` does NOT clear it; a USB re-enumeration did NOT cure cam3 on 2026-08-25 either, so this is DETECTION/alerting. Report-only unless CAMERA_BOX_GRABBER_HALVING_SELFHEAL is set.",
        windows as u64 * 5
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_halved(v: CaptureLatchHalvingVerdict) -> bool {
        matches!(v, CaptureLatchHalvingVerdict::Halved { .. })
    }

    /// Drive `n` identical windows through the tracker, returning the last verdict.
    fn drive(
        t: &mut CaptureLatchHalvingTracker,
        dupe_captures: u64,
        total_captures: u64,
        n: u32,
    ) -> CaptureLatchHalvingVerdict {
        let mut last = CaptureLatchHalvingVerdict::Healthy;
        for _ in 0..n {
            last = t.observe(dupe_captures, total_captures);
        }
        last
    }

    #[test]
    fn healthy_baseline_never_halved() {
        // Healthy 30fps-into-60fps: ~0.5 dupe fraction (150 dupes / 300 captures). Must stay Healthy
        // indefinitely.
        let mut t = CaptureLatchHalvingTracker::new();
        for _ in 0..(HALVING_CONFIRM_WINDOWS + 20) {
            assert_eq!(
                t.observe(150, 300),
                CaptureLatchHalvingVerdict::Healthy,
                "0.5 dupe fraction is the healthy baseline"
            );
        }
    }

    #[test]
    fn latch_halved_band_triggers_after_confirm_windows() {
        // Sick band: 0.75 dupe fraction (225 dupes / 300 captures). Halved fires after exactly
        // HALVING_CONFIRM_WINDOWS consecutive windows, not before.
        let mut t = CaptureLatchHalvingTracker::new();
        for w in 1..=HALVING_CONFIRM_WINDOWS {
            let v = t.observe(225, 300);
            if w < HALVING_CONFIRM_WINDOWS {
                assert!(
                    !is_halved(v),
                    "must not be Halved before window {HALVING_CONFIRM_WINDOWS}: window {w} -> {v:?}"
                );
            } else {
                match v {
                    CaptureLatchHalvingVerdict::Halved {
                        dupe_fraction,
                        dupe_captures,
                        total_captures,
                        windows,
                    } => {
                        assert!((dupe_fraction - 0.75).abs() < 1e-9);
                        assert_eq!(dupe_captures, 225);
                        assert_eq!(total_captures, 300);
                        assert_eq!(windows, HALVING_CONFIRM_WINDOWS);
                    }
                    other => {
                        panic!("expected Halved at window {HALVING_CONFIRM_WINDOWS}, got {other:?}")
                    }
                }
            }
        }
    }

    #[test]
    fn a_fraction_in_the_dead_zone_never_halved() {
        // 0.60 dupe fraction (180/300) sits in the (0.55, 0.70) dead-zone — neither healthy nor
        // halved. The detector must never act on it.
        let mut t = CaptureLatchHalvingTracker::new();
        for _ in 0..(HALVING_CONFIRM_WINDOWS + 10) {
            let v = t.observe(180, 300);
            assert!(
                !is_halved(v),
                "a dead-zone fraction must not confirm: {v:?}"
            );
        }
    }

    #[test]
    fn a_fraction_just_below_the_floor_never_halved() {
        // 0.69 (207/300) is just under HALVED_DUPE_FRACTION_MIN — must not confirm.
        let mut t = CaptureLatchHalvingTracker::new();
        let v = drive(&mut t, 207, 300, HALVING_CONFIRM_WINDOWS + 10);
        assert!(
            !is_halved(v),
            "sub-floor fraction must not be Halved: {v:?}"
        );
    }

    #[test]
    fn a_fraction_exactly_at_the_floor_confirms() {
        // Exactly HALVED_DUPE_FRACTION_MIN (0.70 = 210/300) is inclusive — it confirms.
        let mut t = CaptureLatchHalvingTracker::new();
        let v = drive(&mut t, 210, 300, HALVING_CONFIRM_WINDOWS);
        assert!(is_halved(v), "the floor is inclusive: {v:?}");
    }

    #[test]
    fn a_low_capture_window_never_halved() {
        // A window below HALVING_MIN_CAPTURES_PER_WINDOW captures, even at a 0.75 fraction (a genuine
        // capture stall, not latch-halving), must never confirm.
        let mut t = CaptureLatchHalvingTracker::new();
        for _ in 0..(HALVING_CONFIRM_WINDOWS + 10) {
            // 75/100: fraction 0.75 but only 100 captures < 150 floor.
            let v = t.observe(75, 100);
            assert!(
                !is_halved(v),
                "a low-capture window must not confirm: {v:?}"
            );
        }
    }

    #[test]
    fn an_empty_window_is_healthy() {
        let mut t = CaptureLatchHalvingTracker::new();
        assert_eq!(t.observe(0, 0), CaptureLatchHalvingVerdict::Healthy);
    }

    #[test]
    fn a_single_healthy_window_breaks_the_streak() {
        // The halved run must be CONTINUOUS: one healthy window resets it, so a subsequent sick spell
        // re-accumulates from scratch.
        let mut t = CaptureLatchHalvingTracker::with_thresholds(
            3,
            HALVED_DUPE_FRACTION_MIN,
            HALVED_DUPE_FRACTION_MAX,
            150,
        );
        for _ in 0..2 {
            assert!(!is_halved(t.observe(225, 300)));
        }
        // one healthy-fraction window breaks the streak:
        assert!(!is_halved(t.observe(150, 300)));
        // now it needs 3 fresh halved windows again:
        for w in 1..=3 {
            let v = t.observe(225, 300);
            assert_eq!(
                is_halved(v),
                w == 3,
                "Halved only on the 3rd fresh window (w={w})"
            );
        }
    }

    #[test]
    fn recovery_after_halved_is_not_latched() {
        // After a Halved spell, a return to the healthy fraction clears the verdict — no latch (the
        // caller / systemd restart owns the action, not a sticky flag here).
        let mut t = CaptureLatchHalvingTracker::with_thresholds(
            3,
            HALVED_DUPE_FRACTION_MIN,
            HALVED_DUPE_FRACTION_MAX,
            150,
        );
        assert!(is_halved(drive(&mut t, 225, 300, 3)));
        assert_eq!(t.observe(150, 300), CaptureLatchHalvingVerdict::Healthy);
    }

    #[test]
    fn with_thresholds_floors_a_degenerate_zero_confirm_window_to_one() {
        // confirm_windows(0) must not make an empty run "confirmed" — floored to 1.
        let mut t = CaptureLatchHalvingTracker::with_thresholds(
            0,
            HALVED_DUPE_FRACTION_MIN,
            HALVED_DUPE_FRACTION_MAX,
            150,
        );
        assert!(is_halved(t.observe(225, 300)));
    }

    #[test]
    fn watching_reports_the_building_run_length() {
        let mut t = CaptureLatchHalvingTracker::with_thresholds(
            5,
            HALVED_DUPE_FRACTION_MIN,
            HALVED_DUPE_FRACTION_MAX,
            150,
        );
        assert_eq!(
            t.observe(225, 300),
            CaptureLatchHalvingVerdict::Watching { halved_windows: 1 }
        );
        assert_eq!(
            t.observe(225, 300),
            CaptureLatchHalvingVerdict::Watching { halved_windows: 2 }
        );
    }

    #[test]
    fn latch_halving_warn_message_carries_the_grep_marker_and_key_values() {
        let m = latch_halving_warn_message("/dev/video0", 0.75, 225, 300, 60);
        assert!(
            m.contains("#1200 grabber LATCH-HALVING"),
            "a future watchdog greps this exact marker: {m}"
        );
        assert!(m.contains("/dev/video0"));
        assert!(m.contains("0.75"));
        assert!(m.contains("225/300"));
        assert!(m.contains("~4.0 copies per unique"));
        assert!(m.contains("60 consecutive report windows"));
        assert!(m.contains("~300s"));
        assert!(m.contains("CAMERA_BOX_GRABBER_HALVING_SELFHEAL"));
    }

    #[test]
    fn latch_halving_warn_message_has_no_colliding_watchdog_anchor() {
        // Must NOT collide with any existing dev1-watchdog grep anchor.
        let m = latch_halving_warn_message("/dev/video0", 0.75, 225, 300, 60);
        assert!(!m.contains("#1128 grabber STUCK"));
        assert!(!m.contains("#1193 grabber OVER-RATE"));
        assert!(!m.contains("#663 self-heal"));
        assert!(!m.contains("DEFECTIVE"));
        assert!(!m.contains("CHRONIC"));
    }

    #[test]
    fn latch_halving_warn_message_guards_a_fully_frozen_window() {
        // A degenerate all-dupe window (unique == 0) must not divide by zero in the copies estimate.
        let m = latch_halving_warn_message("/dev/video0", 1.0, 300, 300, 60);
        assert!(m.contains(">4 copies per unique"), "{m}");
    }

    #[test]
    fn a_static_source_at_1_0_never_halved() {
        // A frozen / no-signal source (every frame byte-identical → fraction 1.0) is a DIFFERENT
        // failure (frozen_leg / capture_wedge), NOT latch-halving. The halved band's ceiling must
        // exclude it, no matter how long it holds.
        let mut t = CaptureLatchHalvingTracker::new();
        for _ in 0..(HALVING_CONFIRM_WINDOWS + 10) {
            let v = t.observe(300, 300); // 300/300 = 1.0 > HALVED_DUPE_FRACTION_MAX
            assert!(
                !is_halved(v),
                "a frozen source at fraction 1.0 must not be Halved: {v:?}"
            );
        }
    }

    #[test]
    fn a_fraction_exactly_at_the_ceiling_confirms() {
        // Exactly HALVED_DUPE_FRACTION_MAX (0.90 = 270/300) is inclusive — it confirms.
        let mut t = CaptureLatchHalvingTracker::new();
        let v = drive(&mut t, 270, 300, HALVING_CONFIRM_WINDOWS);
        assert!(is_halved(v), "the ceiling is inclusive: {v:?}");
    }

    #[test]
    fn a_fraction_just_above_the_ceiling_never_halved() {
        // 0.94 (282/300) is above the ceiling — a near-frozen source, not latch-halving.
        let mut t = CaptureLatchHalvingTracker::new();
        let v = drive(&mut t, 282, 300, HALVING_CONFIRM_WINDOWS + 10);
        assert!(
            !is_halved(v),
            "above-ceiling fraction must not be Halved: {v:?}"
        );
    }
}
