//! #656 — capture-delivery-rate sanity check (pure decision, prevention item 1).
//!
//! camera-box already logs `Streaming: X fps emitted / Y fps captured` every ~5s
//! (`src/main.rs`'s capture loop). A capture device that silently starts delivering frames at
//! the WRONG rate is easy to miss in a scrolling log stream unless something explicitly flags
//! it. This module is the PURE decision logic (no I/O) for that flag: given the measured
//! captured fps and the box's configured/negotiated capture rate, decide whether THIS report
//! window is "deviant" — and, given a run of consecutive deviant windows, whether a WARN
//! should fire (a single blip is noise; a SUSTAINED deviation for N consecutive report windows
//! is a real defect).
//!
//! Root cause this guards against (#656, live 2026-07-09/07-10): cam1's Genki ShadowCast 2
//! USB capture dongle silently began delivering ~64 fps instead of its negotiated 60.000 fps,
//! producing a persistent ~4 Hz content-duplicate judder downstream (caught only via
//! after-the-fact tick-pattern archaeology on a recorded E2E run). The live defective log line
//! read `Streaming: 59.4 fps emitted / 63.2 fps captured` — captured deviates from the
//! negotiated 60.0 fps by ~5.3%, sustained for the WHOLE session, not a one-off blip. A WARN
//! fired at the time would have pinpointed this in one run instead of via tick-pattern
//! archaeology across two recordings (see the #656 root-cause comment).

/// Default tolerance: captured fps may deviate from the negotiated/configured capture rate by
/// up to this many PERCENT before a window counts as "deviant". 1% keeps normal measurement
/// jitter (a healthy box reads ~60.0-60.1 fps) well clear of the floor while still catching a
/// real several-percent rate defect (#656's live ~5.3-6.7%).
pub const CAPTURE_RATE_TOLERANCE_PCT: f64 = 1.0;

/// Consecutive 5s report windows a deviation must persist before it's a WARN, not noise.
/// 6 windows @ 5s/window = 30s — long enough to reject a single momentary blip, short enough
/// to catch a real rate-negotiation regression well before a 300s+ E2E recording completes.
pub const CAPTURE_RATE_WARN_WINDOWS: u32 = 6;

/// #666 — EMIT-vs-CAPTURE health tolerance: how many PERCENT the emitted fps may deviate from
/// the box's configured genlock send rate before a report window counts as "deviant". This is a
/// SEPARATE, looser tolerance than `CAPTURE_RATE_TOLERANCE_PCT` (1%, device-level captured-vs-
/// negotiated) because the emit path has more natural per-window jitter (discrete genlock gate
/// boundaries, frame-count rounding over a 5s window) — normal healthy readings observed live
/// (cam5/cam6, 2026-07-11) sit within ~0.5-3% of the target rate. 5% keeps that jitter clear of
/// the floor while still catching a real defect with wide margin: the live #666 finding was a
/// SUSTAINED ~20% deficit (~45-50 fps emitted vs a configured 60 fps send rate) — cam5/cam6
/// transiently emitting well below their healthy captured rate while capture itself stayed clean
/// (0 capture-dropped), self-recovering after ~5 minutes with no restart. Reuses the SAME pure
/// `is_rate_deviant`/`next_consecutive_breaches`/`should_warn` decision as the #656 capture check
/// — only the reference rate (configured SEND fps, not the negotiated CAPTURE fps) and the
/// tolerance differ.
pub const EMIT_RATE_TOLERANCE_PCT: f64 = 5.0;

/// #685 — grabber HARDWARE MODEL, used to select a per-model capture-rate deviation tolerance
/// (`tolerance_pct_for_model`) and to label the #663 self-heal CRITICAL-escalation message
/// honestly (`capture_rate_selfheal::critical_escalation_message`) instead of reflexively
/// recommending hardware replacement for behavior that is actually normal for the model.
///
/// Fleet mapping is OPERATIONALLY known (not device-probed) — see `grabber_model_for_hostname`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrabberModel {
    /// Genki ShadowCast 2 (CAM1-CAM3, live fleet, 2026-07). Its USB output clock free-runs even
    /// against its own HDMI input (internal resampling), producing a CHARACTERISTIC quantized
    /// capture-rate wobble on a perfectly healthy unit — live-observed captured rates from ~55fps
    /// to 64.02fps against a negotiated 60.000fps (up to ~8.3% deviation), sustained for minutes
    /// at a time (#656, #663, #665, #685 forensics). ALL 3 deployed ShadowCast 2 units show this
    /// pattern; 0/3 other-model grabbers do — a MODEL characteristic, not a per-unit defect.
    ShadowCast2,
    /// NZXT Signal HD60 (CAM4, live fleet). No live evidence of rate instability — keeps the
    /// strict default tolerance.
    NzxtSignalHd60,
    /// Elgato 4K S (CAM5-CAM6, live fleet). No live evidence of rate instability — keeps the
    /// strict default tolerance.
    Elgato4kS,
    /// #1034 — Elgato Cam Link 4K, the replacement card the fleet is being swapped ONTO to retire
    /// the ShadowCast 2's characteristic capture-rate wobble (cam3 is the first swapped box, live
    /// 2026-08-14: a genuinely stable device, 0.33% measured spread over 6h). A DISTINCT device
    /// from the `Elgato4kS` above (different product), so it gets its own variant + honest Display
    /// label rather than being folded into it. Keeps the STRICT default tolerance — the whole
    /// point of the swap is that this card does NOT need ShadowCast's wide allowance.
    CamLink4k,
    /// Hostname didn't match a known cambox (unprovisioned box, renamed host, test harness) —
    /// falls back to the STRICT default tolerance rather than guessing the wide ShadowCast-
    /// specific one; an unknown grabber deserves the tighter floor, not the benefit of the doubt.
    Unknown,
}

impl std::fmt::Display for GrabberModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            GrabberModel::ShadowCast2 => "ShadowCast 2",
            GrabberModel::NzxtSignalHd60 => "NZXT Signal HD60",
            GrabberModel::Elgato4kS => "Elgato 4K S",
            GrabberModel::CamLink4k => "Cam Link 4K",
            GrabberModel::Unknown => "unknown grabber model",
        })
    }
}

/// Map a box's `config.hostname` to its known grabber model, per the live fleet layout (CAM1-3 =
/// ShadowCast 2, CAM4 = NZXT Signal HD60, CAM5-6 = Elgato 4K S — `scripts/setup-device.sh`'s
/// documented uppercase `CAM<N>` hostname convention). Case-insensitive + trims whitespace so a
/// stray-cased hostname still resolves; anything that doesn't match a known cambox is
/// `GrabberModel::Unknown` (never guessed).
pub fn grabber_model_for_hostname(hostname: &str) -> GrabberModel {
    match hostname.trim().to_ascii_uppercase().as_str() {
        "CAM1" | "CAM2" | "CAM3" => GrabberModel::ShadowCast2,
        "CAM4" => GrabberModel::NzxtSignalHd60,
        "CAM5" | "CAM6" => GrabberModel::Elgato4kS,
        _ => GrabberModel::Unknown,
    }
}

/// #728/#729 — map a V4L2 `VIDIOC_QUERYCAP` `card` string (e.g. `"Elgato 4K S: Elgato 4K S"`,
/// `"ShadowCast 2: ShadowCast 2"`, `"NZXT Signal HD60 Video: NZXT Si..."` — all live-observed
/// verbatim, 2026-07-12) to its [`GrabberModel`]. This is the RUNTIME counterpart of
/// [`grabber_model_for_hostname`]'s OPERATIONAL (hostname-convention) mapping — a physical
/// card swap changes what's plugged into a box without changing its hostname, so the static
/// table alone silently desyncs from reality the moment a card moves (the #728 cam1<->cam5
/// reshuffle: cam1's hostname still says "ShadowCast 2" per the table, but the box now
/// physically has an Elgato 4K S). Case-insensitive substring match (driver `card` strings
/// vary in exact punctuation/repetition across firmware, e.g. the model name appearing twice
/// separated by `: `) — never an exact-string match, which would be brittle to a firmware
/// string tweak. Unrecognized/empty input is `GrabberModel::Unknown`, same never-guess
/// contract as the hostname mapping.
pub fn grabber_model_from_card_name(card: &str) -> GrabberModel {
    let lower = card.to_ascii_lowercase();
    // #1034 — whitespace-collapsed so cam3's live DOUBLE-spaced `CAM  LINK 4K` still matches
    // `cam link` (a bare `contains("cam link")` would miss the double space). Checked FIRST: a
    // Cam Link 4K string never contains any other model's marker, so order is not load-bearing,
    // but keeping it first documents that this is the intended fleet-replacement card.
    let collapsed = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.contains("cam link") {
        GrabberModel::CamLink4k
    } else if lower.contains("shadowcast") {
        GrabberModel::ShadowCast2
    } else if lower.contains("elgato") {
        GrabberModel::Elgato4kS
    } else if lower.contains("nzxt") || lower.contains("signal hd60") {
        GrabberModel::NzxtSignalHd60
    } else {
        GrabberModel::Unknown
    }
}

/// #728/#729 — resolve a box's grabber model, preferring LIVE hardware truth over the static
/// hostname convention whenever it's available and recognized. This is the single shared seam
/// both tickets asked for: #728's self-heal envelope selection and #729's colour-control
/// policy both key off whatever this function returns, so a physical card swap can never
/// desync the two from each other or from reality.
///
/// - `detected_card` is `Some` and [`grabber_model_from_card_name`] recognizes it -> that
///   model wins, EVEN IF it disagrees with `grabber_model_for_hostname(hostname)` (the swap
///   case — runtime hardware is always more current than the hostname convention).
/// - `detected_card` is `None` (the query failed/unavailable) OR doesn't match any known
///   model -> fall back to `grabber_model_for_hostname(hostname)`, same never-guess-past-
///   Unknown contract as before. An unrecognized card string is NOT trusted enough to
///   override a known hostname mapping, but also never fabricates a match.
///
/// Pure — logging a detected mismatch (so a swap is visible in the journal, per #729's "no
/// ceremony, just log it plainly") is the caller's job, not this function's.
pub fn resolve_grabber_model(hostname: &str, detected_card: Option<&str>) -> GrabberModel {
    if let Some(card) = detected_card {
        let from_card = grabber_model_from_card_name(card);
        if from_card != GrabberModel::Unknown {
            return from_card;
        }
    }
    grabber_model_for_hostname(hostname)
}

/// #685 — ShadowCast 2's per-model capture-rate tolerance (percent). Set to cover the FULL live-
/// observed characteristic-wobble range with margin (~55fps-64.02fps against a 60.000fps
/// negotiated rate = up to ~8.3% deviation — #656, #663, #665) while still catching a genuinely
/// severe deviation well outside that envelope. Deliberately a SEPARATE named constant (not a
/// widening of the shared `CAPTURE_RATE_TOLERANCE_PCT`) so a future `GrabberModel` variant added
/// later doesn't silently inherit ShadowCast's allowance.
pub const SHADOWCAST2_CAPTURE_RATE_TOLERANCE_PCT: f64 = 10.0;

/// The capture-rate deviation tolerance (percent) to use with `is_rate_deviant`, given this box's
/// grabber model. `CAPTURE_RATE_TOLERANCE_PCT` (1%, unchanged) for every model except
/// `ShadowCast2`; see `SHADOWCAST2_CAPTURE_RATE_TOLERANCE_PCT`'s doc for why that one is wider.
pub fn tolerance_pct_for_model(model: GrabberModel) -> f64 {
    match model {
        GrabberModel::ShadowCast2 => SHADOWCAST2_CAPTURE_RATE_TOLERANCE_PCT,
        GrabberModel::NzxtSignalHd60
        | GrabberModel::Elgato4kS
        | GrabberModel::CamLink4k
        | GrabberModel::Unknown => CAPTURE_RATE_TOLERANCE_PCT,
    }
}

/// #717 — SUSTAINED-band capture-rate deviation tolerance (percent) for ShadowCast 2. #685's wide
/// `SHADOWCAST2_CAPTURE_RATE_TOLERANCE_PCT` (10%) correctly tolerates ShadowCast 2's characteristic
/// SHORT-LIVED quantization jitter (band (a): deviation bursts that self-recover within ~60s) —
/// but it ALSO swallowed a genuinely SUSTAINED, reset-fixable defect (band (b): the #674 closure
/// evidence — cam1 held a rock-steady 63.9-64.0fps, a 6.5-6.67% deviation, for an ENTIRE 10-window
/// gate recording, producing 6.67% duplicate frames / visible program judder that imag's own
/// density gate correctly failed on). A USB reset PROVABLY fixes this class of defect (#642: a
/// reset took cam1 from 64.02fps to a stable 60.1-60.5fps) — a fixable defect is not "a model
/// characteristic to tolerate" (see the project's calibrate-artifact-vs-fix-robustness playbook
/// note). This SEPARATE, narrower floor is what the SUSTAINED check (`SUSTAINED_WARN_WINDOWS`,
/// 60s) uses instead of the wide jitter floor, so a deviation this size that actually PERSISTS
/// still trips self-heal even though it never reaches the wide 10% jitter floor.
pub const SHADOWCAST2_SUSTAINED_TOLERANCE_PCT: f64 = 2.0;

/// #717 — number of consecutive 5s report windows a SUSTAINED-band deviation must hold before it
/// counts as a real, reset-worthy defect rather than ShadowCast 2's short-lived quantization
/// jitter. 12 windows @ 5s = 60s — double `CAPTURE_RATE_WARN_WINDOWS`'s 30s (the jitter-band/#656
/// WARN cadence, left unchanged) — per #717's explicit "sustained >= 60s" acceptance bar: long
/// enough that a burst which self-recovers inside 60s (band (a), tolerated) never trips it, short
/// enough that a genuinely chronic defect (band (b)) is caught well before a 300s+ E2E recording
/// completes.
pub const SUSTAINED_WARN_WINDOWS: u32 = 12;

/// The SUSTAINED-band capture-rate deviation tolerance (percent) to use for a self-heal decision,
/// given this box's grabber model. `GrabberModel::ShadowCast2` gets the narrow
/// `SHADOWCAST2_SUSTAINED_TOLERANCE_PCT` (2%) — a SEPARATE, TIGHTER floor than its own
/// (jitter-band) `tolerance_pct_for_model` (10%) — so a sustained-but-under-10%-deviation defect
/// (#674's chronic 64fps) still trips self-heal even though it never reaches the wide jitter
/// floor. Every other model returns the SAME value as `tolerance_pct_for_model` — their
/// single-band strict tolerance already IS the sustained tolerance, so pairing it with
/// `SUSTAINED_WARN_WINDOWS` (60s) makes the sustained check a strict SUPERSET of their existing
/// 30s jitter-band trigger: it can never fire before the jitter-band one already did, so their
/// self-heal cadence is UNCHANGED by #717.
pub fn sustained_tolerance_pct_for_model(model: GrabberModel) -> f64 {
    match model {
        GrabberModel::ShadowCast2 => SHADOWCAST2_SUSTAINED_TOLERANCE_PCT,
        GrabberModel::NzxtSignalHd60
        | GrabberModel::Elgato4kS
        | GrabberModel::CamLink4k
        | GrabberModel::Unknown => tolerance_pct_for_model(model),
    }
}

// Both sides are `const`-derived; these are compile-time assertions (clippy: assertions_on_constants)
// pinning the #717 two-band invariant: the sustained floor sits strictly BETWEEN the strict
// default and ShadowCast 2's own wide jitter floor, and its required window count is strictly
// longer than the jitter band's.
const _: () = assert!(SHADOWCAST2_SUSTAINED_TOLERANCE_PCT < SHADOWCAST2_CAPTURE_RATE_TOLERANCE_PCT);
const _: () = assert!(SHADOWCAST2_SUSTAINED_TOLERANCE_PCT > CAPTURE_RATE_TOLERANCE_PCT);
const _: () = assert!(SUSTAINED_WARN_WINDOWS > CAPTURE_RATE_WARN_WINDOWS);

/// #971 — number of consecutive 5s report windows a SUSTAINED-band deviation must hold before it
/// is genuinely CHRONIC — the bar `capture_rate_selfheal::should_trigger_selfheal` requires before
/// arming an automatic USB reset off the sustained band alone (issue 909 disabled that entirely;
/// issue 971's live evidence — 2026-08-04, cam2, 63.75-64.0fps sustained for 458 report windows —
/// showed a sustained-only over-rate manufactures a real, visible dup+skip motion-cadence defect
/// that the genlock decimation gate does NOT cleanly absorb, so it can no longer be tolerated
/// forever). 180 windows @ 5s = 15 minutes — 15x `SUSTAINED_WARN_WINDOWS`'s 60s informational-only
/// bar, so a device only ever gets automatically reset once the sustained deviation has PROVEN
/// itself chronic, never on the same 60s signal that merely logs informational-only. The
/// hysteresis this needs comes for free from `next_consecutive_breaches`'s existing "any healthy
/// window resets the streak to 0" behavior (unchanged) — a borderline/flapping device can never
/// accumulate to this bar, so it can never reset-loop on the sustained band alone.
pub const CHRONIC_SUSTAINED_WARN_WINDOWS: u32 = 180;

// #971 — the chronic bar must sit strictly beyond the 60s informational-only sustained bar (never
// equal, never shorter) so the two log lines (informational vs reset-triggering) can never both
// describe the exact same window.
const _: () = assert!(CHRONIC_SUSTAINED_WARN_WINDOWS > SUSTAINED_WARN_WINDOWS);

/// True when `captured_fps` deviates from `configured_fps` by more than `tolerance_pct`
/// percent. `configured_fps <= 0.0` (or any non-finite input) is treated as "no valid
/// configured rate to check against" — never deviant; callers only invoke this once a real
/// negotiated/configured rate is known.
pub fn is_rate_deviant(captured_fps: f64, configured_fps: f64, tolerance_pct: f64) -> bool {
    if configured_fps <= 0.0 || !captured_fps.is_finite() || !configured_fps.is_finite() {
        return false;
    }
    let deviation_pct = ((captured_fps - configured_fps).abs() / configured_fps) * 100.0;
    deviation_pct > tolerance_pct
}

/// Rolling breach counter: pure state transition. Call once per report window with whether
/// THIS window was deviant; returns the new consecutive-breach count (any healthy window
/// resets the count to 0 — this flags a SUSTAINED defect, not a cumulative noisy total).
pub fn next_consecutive_breaches(prev_consecutive: u32, deviant_this_window: bool) -> u32 {
    if deviant_this_window {
        prev_consecutive.saturating_add(1)
    } else {
        0
    }
}

/// True once `consecutive_breaches` has reached (or passed) `warn_windows` — the point (and
/// every window thereafter, per comprehensive-logging: keep warning while a real defect
/// persists) at which a WARN should fire. `warn_windows == 0` never warns (an explicitly
/// disabled check, not "always deviant").
pub fn should_warn(consecutive_breaches: u32, warn_windows: u32) -> bool {
    warn_windows > 0 && consecutive_breaches >= warn_windows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_rate_is_not_deviant() {
        assert!(!is_rate_deviant(60.04, 60.0, 1.0));
        assert!(!is_rate_deviant(59.97, 60.0, 1.0));
    }

    #[test]
    fn real_656_defect_rate_is_deviant() {
        // #656 live incident: captured 63.2 / 64.02 fps against a negotiated 60.0 fps.
        assert!(is_rate_deviant(63.2, 60.0, 1.0));
        assert!(is_rate_deviant(64.02, 60.0, 1.0));
    }

    #[test]
    fn exactly_at_tolerance_boundary_is_not_deviant() {
        // 100.0 -> 101.0 is EXACTLY a 1.0% deviation (float-exact, unlike a 60.0 base) -> NOT
        // strictly greater than a 1.0% tolerance -> not deviant.
        assert!(!is_rate_deviant(101.0, 100.0, 1.0));
        // Just over the boundary IS deviant.
        assert!(is_rate_deviant(101.01, 100.0, 1.0));
    }

    #[test]
    fn zero_or_negative_configured_fps_never_deviant() {
        assert!(!is_rate_deviant(64.0, 0.0, 1.0));
        assert!(!is_rate_deviant(64.0, -1.0, 1.0));
    }

    #[test]
    fn non_finite_inputs_never_deviant() {
        assert!(!is_rate_deviant(f64::NAN, 60.0, 1.0));
        assert!(!is_rate_deviant(60.0, f64::NAN, 1.0));
        assert!(!is_rate_deviant(f64::INFINITY, 60.0, 1.0));
        assert!(!is_rate_deviant(60.0, f64::INFINITY, 1.0));
    }

    #[test]
    fn consecutive_breaches_accumulate_and_reset() {
        let mut c = 0;
        c = next_consecutive_breaches(c, true);
        assert_eq!(c, 1);
        c = next_consecutive_breaches(c, true);
        assert_eq!(c, 2);
        c = next_consecutive_breaches(c, false);
        assert_eq!(c, 0);
    }

    #[test]
    fn consecutive_breaches_saturate_never_panic() {
        let c = next_consecutive_breaches(u32::MAX, true);
        assert_eq!(c, u32::MAX);
    }

    #[test]
    fn should_warn_fires_at_and_after_threshold() {
        assert!(!should_warn(5, 6));
        assert!(should_warn(6, 6));
        assert!(should_warn(7, 6));
    }

    #[test]
    fn should_warn_never_fires_when_threshold_is_zero() {
        assert!(!should_warn(0, 0));
        assert!(!should_warn(100, 0));
    }

    #[test]
    fn real_656_scenario_warns_exactly_at_the_configured_window() {
        // Simulate consecutive 5s windows at the #656 defect rate (63.2 vs negotiated 60.0),
        // confirming should_warn fires exactly at window CAPTURE_RATE_WARN_WINDOWS, never
        // earlier, and keeps firing on every window after (comprehensive-logging: don't go
        // silent while the defect persists).
        let mut consecutive = 0u32;
        for i in 1..=(CAPTURE_RATE_WARN_WINDOWS + 2) {
            let deviant = is_rate_deviant(63.2, 60.0, CAPTURE_RATE_TOLERANCE_PCT);
            assert!(deviant);
            consecutive = next_consecutive_breaches(consecutive, deviant);
            let warn = should_warn(consecutive, CAPTURE_RATE_WARN_WINDOWS);
            if i < CAPTURE_RATE_WARN_WINDOWS {
                assert!(!warn, "window {i} should not warn yet");
            } else {
                assert!(warn, "window {i} should warn");
            }
        }
    }

    #[test]
    fn a_single_blip_never_reaches_the_warn_threshold() {
        // One deviant window surrounded by healthy ones never accumulates to the threshold.
        let mut consecutive = 0u32;
        for deviant in [false, false, true, false, false] {
            consecutive = next_consecutive_breaches(consecutive, deviant);
            assert!(!should_warn(consecutive, CAPTURE_RATE_WARN_WINDOWS));
        }
    }

    #[test]
    fn recovery_after_a_sustained_defect_stops_the_warn() {
        // A defect that persists past the threshold, then recovers, must stop warning (not
        // latch permanently) — the next healthy window resets the run to zero.
        let mut consecutive = 0u32;
        for _ in 0..(CAPTURE_RATE_WARN_WINDOWS + 1) {
            consecutive = next_consecutive_breaches(consecutive, true);
        }
        assert!(should_warn(consecutive, CAPTURE_RATE_WARN_WINDOWS));
        consecutive = next_consecutive_breaches(consecutive, false);
        assert!(!should_warn(consecutive, CAPTURE_RATE_WARN_WINDOWS));
    }

    // -----------------------------------------------------------------------------------------
    // #666 — EMIT-vs-CAPTURE health (reuses the SAME pure decision, looser tolerance + a
    // different reference rate: configured SEND fps instead of negotiated CAPTURE fps).
    // -----------------------------------------------------------------------------------------

    #[test]
    fn real_666_emit_deficit_is_deviant_under_the_emit_tolerance() {
        // The live #666 finding: cam5/cam6 emitted ~45-50 fps against a configured 60 fps send
        // rate while captured stayed healthy — a ~17-25% deficit, far past the 5% emit floor.
        assert!(is_rate_deviant(45.0, 60.0, EMIT_RATE_TOLERANCE_PCT));
        assert!(is_rate_deviant(48.0, 60.0, EMIT_RATE_TOLERANCE_PCT));
        assert!(is_rate_deviant(50.0, 60.0, EMIT_RATE_TOLERANCE_PCT));
    }

    #[test]
    fn healthy_emit_jitter_within_5_percent_is_not_deviant() {
        // Live healthy readings (cam5/cam6, 2026-07-11): emitted sits within ~0.5-3% of the
        // configured 60 fps send rate on a normal report window — none of these should WARN.
        for emitted in [60.0, 59.8, 59.6, 60.1, 58.5, 57.5] {
            assert!(
                !is_rate_deviant(emitted, 60.0, EMIT_RATE_TOLERANCE_PCT),
                "emitted={emitted} vs configured=60.0 should be within the {EMIT_RATE_TOLERANCE_PCT}% emit floor"
            );
        }
    }

    // The whole point of a SEPARATE emit constant: the emit path has more natural jitter than
    // the device-level capture check, so its floor must be strictly wider. Both sides are
    // `const`, so this is a compile-time assertion (clippy: assertions_on_constants) rather than
    // a runtime `#[test]`.
    const _: () = assert!(EMIT_RATE_TOLERANCE_PCT > CAPTURE_RATE_TOLERANCE_PCT);

    // -----------------------------------------------------------------------------------------
    // #685 — per-model capture-rate tolerance: ShadowCast 2's characteristic quantization wobble
    // must NOT be flagged deviant (so it can never fire the #663 self-heal USB reset on its
    // own); every other known model keeps the original strict 1% floor unchanged.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn grabber_model_maps_the_live_fleet_hostnames() {
        assert_eq!(
            grabber_model_for_hostname("CAM1"),
            GrabberModel::ShadowCast2
        );
        assert_eq!(
            grabber_model_for_hostname("CAM2"),
            GrabberModel::ShadowCast2
        );
        assert_eq!(
            grabber_model_for_hostname("CAM3"),
            GrabberModel::ShadowCast2
        );
        assert_eq!(
            grabber_model_for_hostname("CAM4"),
            GrabberModel::NzxtSignalHd60
        );
        assert_eq!(grabber_model_for_hostname("CAM5"), GrabberModel::Elgato4kS);
        assert_eq!(grabber_model_for_hostname("CAM6"), GrabberModel::Elgato4kS);
    }

    #[test]
    fn grabber_model_is_case_insensitive_and_trims_whitespace() {
        assert_eq!(
            grabber_model_for_hostname("cam1"),
            GrabberModel::ShadowCast2
        );
        assert_eq!(
            grabber_model_for_hostname("  Cam4  "),
            GrabberModel::NzxtSignalHd60
        );
    }

    #[test]
    fn grabber_model_unknown_hostname_falls_back_to_unknown_not_a_guess() {
        assert_eq!(
            grabber_model_for_hostname("camera-box"),
            GrabberModel::Unknown
        );
        assert_eq!(grabber_model_for_hostname(""), GrabberModel::Unknown);
        assert_eq!(grabber_model_for_hostname("CAM7"), GrabberModel::Unknown);
    }

    // -----------------------------------------------------------------------------------------
    // #728/#729 — runtime card-name detection + the shared resolve_grabber_model seam. Live
    // `card` strings captured 2026-07-12 via `v4l2-ctl --info` on the real fleet.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn grabber_model_from_card_name_matches_live_fleet_strings() {
        assert_eq!(
            grabber_model_from_card_name("Elgato 4K S: Elgato 4K S"),
            GrabberModel::Elgato4kS
        );
        assert_eq!(
            grabber_model_from_card_name("ShadowCast 2: ShadowCast 2"),
            GrabberModel::ShadowCast2
        );
        assert_eq!(
            grabber_model_from_card_name("NZXT Signal HD60 Video: NZXT Si"),
            GrabberModel::NzxtSignalHd60
        );
    }

    /// #1034 — the Cam Link 4K card swap (issue-728 lineage) has already landed on cam3, whose
    /// live `card` string is `CAM  LINK 4K: CAM  LINK 4K` (note the DOUBLE space between CAM and
    /// LINK — the match must be whitespace-robust). It must be recognized as its own model so
    /// `resolve_grabber_model` hands it the STRICT base tolerance, instead of falling through to
    /// the hostname table's `CAM3 → ShadowCast2` (wide 10%) mapping that wrongly gave a
    /// rock-steady 0.33%-spread device the ShadowCast grabber allowance.
    #[test]
    fn grabber_model_from_card_name_recognizes_cam_link_4k_1034() {
        assert_eq!(
            grabber_model_from_card_name("CAM  LINK 4K: CAM  LINK 4K"),
            GrabberModel::CamLink4k,
            "cam3's live double-spaced Cam Link 4K card string must be recognized"
        );
        assert_eq!(
            grabber_model_from_card_name("Cam Link 4K"),
            GrabberModel::CamLink4k
        );
        assert_eq!(
            grabber_model_from_card_name("CAM LINK 4K"),
            GrabberModel::CamLink4k
        );
    }

    /// #1034 — cam3's decisive scenario: hostname still says `CAM3` (→ ShadowCast2 in the static
    /// table) but the box physically has a Cam Link 4K now. Detection must win and hand it the
    /// strict base tolerance (1%), NOT the wide ShadowCast 10% floor.
    #[test]
    fn resolve_grabber_model_cam_link_4k_wins_over_shadowcast_hostname_1034() {
        assert_eq!(
            resolve_grabber_model("CAM3", Some("CAM  LINK 4K: CAM  LINK 4K")),
            GrabberModel::CamLink4k
        );
        assert_eq!(
            tolerance_pct_for_model(GrabberModel::CamLink4k),
            CAPTURE_RATE_TOLERANCE_PCT,
            "a Cam Link 4K is a genuinely stable device — strict base tolerance, not ShadowCast's \
             wide floor"
        );
        assert_eq!(
            sustained_tolerance_pct_for_model(GrabberModel::CamLink4k),
            CAPTURE_RATE_TOLERANCE_PCT
        );
        assert_eq!(GrabberModel::CamLink4k.to_string(), "Cam Link 4K");
    }

    #[test]
    fn grabber_model_from_card_name_is_case_insensitive() {
        assert_eq!(
            grabber_model_from_card_name("elgato 4k s"),
            GrabberModel::Elgato4kS
        );
        assert_eq!(
            grabber_model_from_card_name("SHADOWCAST 2"),
            GrabberModel::ShadowCast2
        );
    }

    #[test]
    fn grabber_model_from_card_name_unrecognized_is_unknown_never_guessed() {
        assert_eq!(
            grabber_model_from_card_name("Some Other Webcam"),
            GrabberModel::Unknown
        );
        assert_eq!(grabber_model_from_card_name(""), GrabberModel::Unknown);
    }

    /// The #728 decisive scenario: cam1's hostname convention still says ShadowCast 2, but
    /// the box now physically has an Elgato 4K S (the live card swap). Runtime truth must
    /// win — this is the whole point of the shared detection seam.
    #[test]
    fn resolve_grabber_model_runtime_detection_wins_over_stale_hostname_728() {
        assert_eq!(
            resolve_grabber_model("CAM1", Some("Elgato 4K S: Elgato 4K S")),
            GrabberModel::Elgato4kS,
            "a physically swapped card must resolve to what's ACTUALLY plugged in, not what \
             the hostname convention used to say"
        );
        // The other half of the same reshuffle: cam5's hostname says Elgato, but the
        // ShadowCast 2 physically sits there now.
        assert_eq!(
            resolve_grabber_model("CAM5", Some("ShadowCast 2: ShadowCast 2")),
            GrabberModel::ShadowCast2
        );
    }

    #[test]
    fn resolve_grabber_model_falls_back_to_hostname_when_detection_unavailable() {
        // Detection failed (device query error, or called before the device is open) — the
        // hostname convention is still the best available answer, never a hard failure.
        assert_eq!(
            resolve_grabber_model("CAM4", None),
            GrabberModel::NzxtSignalHd60
        );
    }

    #[test]
    fn resolve_grabber_model_falls_back_to_hostname_when_card_name_unrecognized() {
        // A detected-but-unrecognized card string (a brand-new grabber model, or a firmware
        // string this mapping doesn't know yet) is not trusted enough to override a KNOWN
        // hostname mapping — but also never invents a match.
        assert_eq!(
            resolve_grabber_model("CAM2", Some("Some Brand New Grabber")),
            GrabberModel::ShadowCast2
        );
    }

    #[test]
    fn resolve_grabber_model_no_stored_state_is_zero_touch_unknown() {
        // Neither the hostname nor the detected card resolves to anything known — the
        // "no stored state" fixture: must land on Unknown, never a guessed model.
        assert_eq!(
            resolve_grabber_model("dev-laptop", None),
            GrabberModel::Unknown
        );
        assert_eq!(
            resolve_grabber_model("dev-laptop", Some("Unbranded HDMI Grabber")),
            GrabberModel::Unknown
        );
    }

    #[test]
    fn tolerance_is_strict_1pct_for_every_model_except_shadowcast2() {
        assert_eq!(
            tolerance_pct_for_model(GrabberModel::NzxtSignalHd60),
            CAPTURE_RATE_TOLERANCE_PCT
        );
        assert_eq!(
            tolerance_pct_for_model(GrabberModel::Elgato4kS),
            CAPTURE_RATE_TOLERANCE_PCT
        );
        assert_eq!(
            tolerance_pct_for_model(GrabberModel::Unknown),
            CAPTURE_RATE_TOLERANCE_PCT
        );
        assert_eq!(
            tolerance_pct_for_model(GrabberModel::ShadowCast2),
            SHADOWCAST2_CAPTURE_RATE_TOLERANCE_PCT
        );
    }

    // Both sides are `const`, so this is a compile-time assertion (clippy: assertions_on_constants)
    // rather than a runtime `#[test]` — mirrors the EMIT_RATE_TOLERANCE_PCT one below.
    const _: () = assert!(SHADOWCAST2_CAPTURE_RATE_TOLERANCE_PCT > CAPTURE_RATE_TOLERANCE_PCT);

    #[test]
    fn live_shadowcast2_characteristic_readings_are_not_deviant_under_its_model_tolerance() {
        // #656 (cam1 defect episode), #663 (cam3's steady-state wobble), #665 (cam2's
        // under-delivery episode, later reclassified as the SAME model class) — the full range
        // of live-observed "healthy but wobbly" ShadowCast 2 readings.
        let tol = tolerance_pct_for_model(GrabberModel::ShadowCast2);
        for captured in [
            60.1, 60.5, 61.0, 61.77, 62.06, 62.8, 63.2, 64.02, 55.0, 58.2, 59.5,
        ] {
            assert!(
                !is_rate_deviant(captured, 60.0, tol),
                "captured={captured} vs 60.0 configured must be within ShadowCast 2's {tol}% \
                 characteristic-wobble tolerance (must never trip #663 self-heal on its own)"
            );
        }
    }

    #[test]
    fn the_same_live_shadowcast2_readings_would_have_tripped_the_old_strict_default() {
        // Proves the #685 widening is a real change, not a no-op: every value below DID cross
        // the original 1% default that every other model still uses.
        for captured in [61.0, 61.77, 62.06, 62.8, 63.2, 64.02, 55.0, 58.2] {
            assert!(
                is_rate_deviant(captured, 60.0, CAPTURE_RATE_TOLERANCE_PCT),
                "captured={captured} should have been deviant under the OLD strict 1% default"
            );
        }
    }

    #[test]
    fn a_genuinely_extreme_deviation_still_trips_the_shadowcast2_tolerance() {
        // A deviation well beyond the observed characteristic envelope (e.g. a near-total rate
        // collapse) must still be caught even on ShadowCast 2's wider floor — #685's recalibration
        // widens the floor, it does not disable the check.
        let tol = tolerance_pct_for_model(GrabberModel::ShadowCast2);
        assert!(
            is_rate_deviant(45.0, 60.0, tol),
            "45fps vs 60fps (25% deviation) must still trip"
        );
        assert!(
            is_rate_deviant(75.0, 60.0, tol),
            "75fps vs 60fps (25% deviation) must still trip"
        );
    }

    /// #1034 — the ShadowCast jitter/reset floor is tightened 10% -> 9% off a fresh live
    /// re-measure (2026-08-14: cam1 max 4.67% / cam2 max 2.83% over ~8h; the ShadowCast CLASS's
    /// historical characteristic envelope tops at ~8.33%, the 55.0 fps low-side). 9% stays above
    /// that class envelope so no existing rig-forensic reading trips and no issue-909 reset-spam
    /// regresses, while catching a genuinely-severe sustained deviation JUST beyond the envelope
    /// that the old 10% floor let slip. (The REAL shrink to base 1% is delivered per-box by the
    /// Cam Link 4K swap — cam3 already there.)
    #[test]
    fn shadowcast2_jitter_floor_tightened_to_9pct_catches_beyond_class_envelope_1034() {
        let tol = tolerance_pct_for_model(GrabberModel::ShadowCast2);
        // 65.7 fps vs 60.0 = 9.5% — beyond the ~8.33% class envelope. NOT deviant under the old
        // 10% floor (RED), deviant under 9% (GREEN).
        assert!(
            is_rate_deviant(65.7, 60.0, tol),
            "a 9.5% sustained deviation must trip the tightened 9% ShadowCast floor"
        );
        assert!(
            is_rate_deviant(54.3, 60.0, tol),
            "the symmetric 9.5% low-side deviation must trip too"
        );
        // Over-tightening guard: cam1's live measured max (62.80 fps = 4.67%) and the class's
        // historical characteristic readings (55.0 / 64.02 fps, up to 8.33%) must STAY within the
        // floor so E2E gates stay green and no reset-spam regresses (issue-909).
        for captured in [62.80, 64.02, 55.0] {
            assert!(
                !is_rate_deviant(captured, 60.0, tol),
                "captured={captured} is within the ShadowCast characteristic envelope — must not \
                 trip the reset floor (would re-introduce issue-909 reset-spam)"
            );
        }
    }

    #[test]
    fn nzxt_and_elgato_still_flag_the_same_deviation_shadowcast2_now_tolerates() {
        // The whole point of a PER-MODEL tolerance: only ShadowCast 2 gets the wider floor. A
        // NZXT/Elgato box reading the SAME 62fps (now fine for ShadowCast 2) must still be
        // flagged — #685 must not accidentally loosen the check fleet-wide.
        let nzxt_tol = tolerance_pct_for_model(GrabberModel::NzxtSignalHd60);
        let elgato_tol = tolerance_pct_for_model(GrabberModel::Elgato4kS);
        assert!(is_rate_deviant(62.0, 60.0, nzxt_tol));
        assert!(is_rate_deviant(62.0, 60.0, elgato_tol));
    }

    // -----------------------------------------------------------------------------------------
    // #717 — two-band ShadowCast 2 envelope: a SEPARATE, narrower SUSTAINED-band tolerance
    // (2%, `SUSTAINED_WARN_WINDOWS`=60s) catches a genuinely chronic defect that #685's wide
    // 10%/30s jitter band correctly tolerates as short-lived characteristic wobble.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn real_674_chronic_64fps_is_sustained_deviant_under_shadowcast2() {
        // #674 closure evidence: cam1 held 63.9-64.0fps rock-steady for an entire recording —
        // 6.5-6.67% deviation from the 60.0 negotiated rate. Must trip the SUSTAINED band even
        // though it stays comfortably inside ShadowCast 2's wide 10% jitter floor.
        let sustained_tol = sustained_tolerance_pct_for_model(GrabberModel::ShadowCast2);
        assert!(is_rate_deviant(63.9, 60.0, sustained_tol));
        assert!(is_rate_deviant(64.0, 60.0, sustained_tol));
        // Confirm it is NOT deviant under the wide jitter floor — #717 must not disturb the
        // jitter band itself (that's #685's own, still-correct behavior), only ADD the
        // sustained one.
        let jitter_tol = tolerance_pct_for_model(GrabberModel::ShadowCast2);
        assert!(!is_rate_deviant(63.9, 60.0, jitter_tol));
        assert!(!is_rate_deviant(64.0, 60.0, jitter_tol));
    }

    #[test]
    fn short_lived_quantization_readings_within_2pct_stay_healthy_under_sustained_band() {
        let tol = sustained_tolerance_pct_for_model(GrabberModel::ShadowCast2);
        for captured in [60.0, 60.1, 60.5, 59.5, 61.1, 58.9] {
            assert!(
                !is_rate_deviant(captured, 60.0, tol),
                "captured={captured} should be within ShadowCast 2's {tol}% sustained-band \
                 tolerance"
            );
        }
    }

    #[test]
    fn sustained_tolerance_is_narrower_than_jitter_tolerance_for_shadowcast2_only() {
        assert!(
            sustained_tolerance_pct_for_model(GrabberModel::ShadowCast2)
                < tolerance_pct_for_model(GrabberModel::ShadowCast2)
        );
    }

    #[test]
    fn sustained_tolerance_equals_jitter_tolerance_for_every_other_model() {
        for model in [
            GrabberModel::NzxtSignalHd60,
            GrabberModel::Elgato4kS,
            GrabberModel::Unknown,
        ] {
            assert_eq!(
                sustained_tolerance_pct_for_model(model),
                tolerance_pct_for_model(model),
                "{model} must not get a separate sustained floor — its single-band strict \
                 tolerance already covers both roles, so #717 must not change its behavior"
            );
        }
    }

    #[test]
    fn sustained_warn_windows_is_60_seconds_double_the_jitter_warn_windows() {
        assert_eq!(SUSTAINED_WARN_WINDOWS, CAPTURE_RATE_WARN_WINDOWS * 2);
    }

    #[test]
    fn real_674_scenario_sustained_warn_fires_exactly_at_60s_never_earlier() {
        // Mirrors `real_656_scenario_warns_exactly_at_the_configured_window` but for the
        // sustained band, fed #674's actual live chronic reading (64.0fps).
        let tol = sustained_tolerance_pct_for_model(GrabberModel::ShadowCast2);
        let mut consecutive = 0u32;
        for i in 1..=(SUSTAINED_WARN_WINDOWS + 2) {
            let deviant = is_rate_deviant(64.0, 60.0, tol);
            assert!(deviant);
            consecutive = next_consecutive_breaches(consecutive, deviant);
            let warn = should_warn(consecutive, SUSTAINED_WARN_WINDOWS);
            if i < SUSTAINED_WARN_WINDOWS {
                assert!(!warn, "window {i} should not warn yet");
            } else {
                assert!(warn, "window {i} should warn");
            }
        }
    }

    #[test]
    fn a_burst_shorter_than_60s_never_reaches_the_sustained_threshold() {
        // Band (a): a deviation burst that self-recovers before SUSTAINED_WARN_WINDOWS (60s) is
        // tolerated — the whole point of splitting the two bands (#717).
        let tol = sustained_tolerance_pct_for_model(GrabberModel::ShadowCast2);
        let mut consecutive = 0u32;
        // 8 deviant windows (40s) then recovery, well short of the 12-window (60s) threshold.
        for _ in 0..8 {
            let deviant = is_rate_deviant(64.0, 60.0, tol);
            consecutive = next_consecutive_breaches(consecutive, deviant);
            assert!(!should_warn(consecutive, SUSTAINED_WARN_WINDOWS));
        }
        let healthy = is_rate_deviant(60.05, 60.0, tol); // well within 2% -> not deviant
        assert!(!healthy);
        consecutive = next_consecutive_breaches(consecutive, healthy);
        assert_eq!(
            consecutive, 0,
            "a healthy window must reset the sustained-band streak"
        );
    }

    #[test]
    fn real_666_scenario_warns_exactly_at_the_configured_window() {
        // Sustained ~20% emit deficit (48 fps vs configured 60 fps) must warn exactly at
        // CAPTURE_RATE_WARN_WINDOWS (shared cadence with #656 — reject a blip, catch a real
        // sustained defect), never earlier, and keep warning every window after.
        let mut consecutive = 0u32;
        for i in 1..=(CAPTURE_RATE_WARN_WINDOWS + 2) {
            let deviant = is_rate_deviant(48.0, 60.0, EMIT_RATE_TOLERANCE_PCT);
            assert!(deviant);
            consecutive = next_consecutive_breaches(consecutive, deviant);
            let warn = should_warn(consecutive, CAPTURE_RATE_WARN_WINDOWS);
            if i < CAPTURE_RATE_WARN_WINDOWS {
                assert!(!warn, "window {i} should not warn yet");
            } else {
                assert!(warn, "window {i} should warn");
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // #971 — the CHRONIC bar: a MUCH longer consecutive-window requirement than the 60s
    // informational-only `SUSTAINED_WARN_WINDOWS`, so a sustained-alone deviation only arms an
    // automatic USB reset once it has PROVEN itself chronic (issue 971's live evidence: a
    // sustained-alone 63.75-64.0fps over-rate manufactures a real, visible dup+skip motion-cadence
    // defect the genlock decimation gate cannot cleanly absorb).
    // -----------------------------------------------------------------------------------------

    #[test]
    fn chronic_sustained_warn_windows_is_15x_the_sustained_confirm_971() {
        assert_eq!(CHRONIC_SUSTAINED_WARN_WINDOWS, SUSTAINED_WARN_WINDOWS * 15);
    }

    #[test]
    fn real_971_scenario_chronic_sustained_fires_exactly_at_the_configured_window_never_earlier() {
        // #971 live incident reading: cam2 ShadowCast 2 held 63.75-64.0fps, well beyond the 2%
        // sustained-band floor, for 458 consecutive report windows before manual intervention.
        // Confirm the CHRONIC bar fires exactly at CHRONIC_SUSTAINED_WARN_WINDOWS, never earlier
        // — including a run that has ALREADY passed the 60s informational-only bar but not yet
        // the chronic one (borderline/short sustained must NOT trigger).
        let tol = sustained_tolerance_pct_for_model(GrabberModel::ShadowCast2);
        let mut consecutive = 0u32;
        for i in 1..=(CHRONIC_SUSTAINED_WARN_WINDOWS + 2) {
            let deviant = is_rate_deviant(63.75, 60.0, tol);
            assert!(deviant);
            consecutive = next_consecutive_breaches(consecutive, deviant);
            let chronic = should_warn(consecutive, CHRONIC_SUSTAINED_WARN_WINDOWS);
            if i < CHRONIC_SUSTAINED_WARN_WINDOWS {
                assert!(!chronic, "window {i} must not be chronic yet");
            } else {
                assert!(chronic, "window {i} must be chronic");
            }
        }
    }

    #[test]
    fn sustained_confirmed_at_60s_is_not_yet_chronic_971() {
        // A run that reaches (and holds at) the 60s informational-only SUSTAINED_WARN_WINDOWS bar
        // — the exact borderline the #909 informational log fires on — must NOT also count as
        // CHRONIC. Borderline/short sustained does not arm self-heal.
        let tol = sustained_tolerance_pct_for_model(GrabberModel::ShadowCast2);
        let mut consecutive = 0u32;
        for _ in 0..SUSTAINED_WARN_WINDOWS {
            let deviant = is_rate_deviant(64.0, 60.0, tol);
            consecutive = next_consecutive_breaches(consecutive, deviant);
        }
        assert!(
            should_warn(consecutive, SUSTAINED_WARN_WINDOWS),
            "sanity: the 60s informational bar must have fired by now"
        );
        assert!(
            !should_warn(consecutive, CHRONIC_SUSTAINED_WARN_WINDOWS),
            "60s of sustained deviation alone must not yet count as CHRONIC (15 min bar)"
        );
    }

    #[test]
    fn flapping_device_never_reaches_the_chronic_bar_971() {
        // Hysteresis proof: a borderline device that alternates long deviant runs with single
        // healthy windows (never holding continuously long enough) must NEVER reach the chronic
        // bar, even though its CUMULATIVE deviant-window count over time far exceeds
        // CHRONIC_SUSTAINED_WARN_WINDOWS. This is what prevents a flapping device from ever
        // reset-looping on the sustained band alone.
        let tol = sustained_tolerance_pct_for_model(GrabberModel::ShadowCast2);
        let mut consecutive = 0u32;
        let mut any_chronic = false;
        let run_len = CHRONIC_SUSTAINED_WARN_WINDOWS - 1; // always just short of chronic
        for _cycle in 0..10 {
            for _ in 0..run_len {
                let deviant = is_rate_deviant(64.0, 60.0, tol);
                consecutive = next_consecutive_breaches(consecutive, deviant);
                any_chronic |= should_warn(consecutive, CHRONIC_SUSTAINED_WARN_WINDOWS);
            }
            // One healthy window resets the streak — the device "flaps" back to healthy briefly.
            let healthy = is_rate_deviant(60.0, 60.0, tol);
            assert!(!healthy);
            consecutive = next_consecutive_breaches(consecutive, healthy);
            assert_eq!(consecutive, 0, "a healthy window must reset the streak");
        }
        assert!(
            !any_chronic,
            "a device that never holds deviant continuously for the full chronic bar must \
             never be classified chronic, no matter how many cumulative deviant windows it logs"
        );
    }
}
