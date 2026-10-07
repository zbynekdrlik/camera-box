//! #1299 Part 4 + #1357 scope C + issue 1372 — the wall-clock STEP verdict against the monotonic
//! (QPC) timebase ([`qpc_drift_beyond_bound`], with the report-only rate [`qpc_window_rate_ppm`])
//! and the booking of a coordinated dantesync fleet date step ([`qpc_wall_step_rebase_ms`]).
//! Mirrored in `GenlockLockState.hpp`, parity-gated by `tests/genlock_lock_state_parity.rs` and
//! `tests/genlock_qpc_wall_step_parity_1372.rs`. Split out of `genlock_lock_state.rs` (issue 1302);
//! every item is re-exported at `crate::genlock_lock_state`.

// #1299 Part 4 + #1357 scope C — the wall-vs-monotonic `qpc_drift` term. The CUMULATIVE offset must
// never gate: on a dantesync-disciplined Windows box the wall ran at the grandmaster rate vs the free
// QPC crystal and the offset grew ~50 ms/h (38 false pages overnight 15./16.9.2026; issue 1372 has
// since disciplined the Windows `os_gettime_ns()`). The RATE must not
// gate either (#1357): on Linux `CLOCK_MONOTONIC` is kernel-disciplined together with `CLOCK_REALTIME`,
// so the measured wall-vs-monotonic rate is 0 by construction, while on Windows it was the free crystal —
// a rate check therefore meant a different thing on every box, and comparing a windowed rate with one
// instantaneous dantesync `f_ptp + f_phase` sample false-DEGRADED both (28 samples on strih-lx, 4 on
// stream, 24.9.2026, none a step). A rate is also no genlock hazard: the render tick re-derives every
// deadline from the wall clock and absorbs up to 2 ms per tick. The one clock hazard for genlock — the
// same on every box — is a wall STEP: it moves every wall-keyed FIFO release / ts-align deadline by more
// than a frame at once (the render tick itself only slews through it). The windowed rate stays
// report-only telemetry. (Since issue 1372 the Windows `os_gettime_ns()` also runs at the
// dantesync-disciplined rate, so the measured rate is ~0 on every box; the step verdict is unchanged.)
/// A single-sample wall STEP beyond this (ms) DEGRADES immediately — one 30 fps frame, the coarsest
/// fleet frame interval (same value as the audio-pairing bound), so a sub-frame wobble never trips.
pub const GENLOCK_QPC_STEP_BOUND_MS: i64 = 33;
/// The rolling window (s) the widget measures the report-only drift RATE telemetry over. 300 s so the
/// integer-ms cumulative drift resolves the rate: at 14 ppm the window accrues ≈ 4.2 ms.
pub const GENLOCK_QPC_WINDOW_S: i64 = 300;

/// #1299 Part 4 — the wall-vs-QPC drift verdict plus the measured rate it read (for telemetry).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QpcDriftVerdict {
    /// Whether the wall clock STEPPED by more than one frame (#1357: the step is the whole verdict).
    pub beyond_bound: bool,
    /// The windowed drift rate in ppm (0.0 until the rate window has filled).
    pub measured_ppm: f64,
}

/// #1299 Part 4 — the windowed drift RATE in ppm from an integer-ms cumulative-drift delta over an
/// integer-ms elapsed span. `delta_ms / elapsed_ms` is dimensionless; × 1e6 is ppm. 0.0 for a
/// non-positive span (not-ready / degenerate). Byte-for-byte the arithmetic the C mirror
/// `genlock_qpc_drift_beyond_bound` performs internally, kept as a named Tier-0-tested helper so the
/// JSON `qpc_drift_ppm` telemetry and the parity harness share one formula.
pub fn qpc_window_rate_ppm(drift_delta_ms: i64, elapsed_ms: i64) -> f64 {
    if elapsed_ms <= 0 {
        return 0.0;
    }
    drift_delta_ms as f64 / elapsed_ms as f64 * 1_000_000.0
}

/// #1299 Part 4 + #1357 scope C — decide whether the wall clock STEPPED against the monotonic sleep
/// timebase, and report the measured windowed rate as telemetry. DEGRADED only when a single-sample
/// STEP exceeds `step_bound_ms` (judged as soon as two samples exist, whether or not the rate window
/// has filled). The rate (`measured_ppm`, reported once `rate_ready`) never feeds the verdict — it
/// means a different thing on Linux (disciplined monotonic, 0 by construction) and on Windows (free
/// QPC crystal), and it is no genlock hazard. One semantics on every box.
///
/// Byte-for-byte mirror of `genlock_qpc_drift_beyond_bound` in `GenlockLockState.hpp` — the committed
/// parity gate `tests/genlock_lock_state_parity.rs` keeps the two numerically identical over a spread
/// of int vectors (verdict + measured rate).
pub fn qpc_drift_beyond_bound(
    rate_ready: bool,
    drift_delta_ms: i64,
    elapsed_ms: i64,
    max_step_ms: i64,
    step_bound_ms: i64,
) -> QpcDriftVerdict {
    let measured_ppm = if rate_ready {
        qpc_window_rate_ppm(drift_delta_ms, elapsed_ms)
    } else {
        0.0
    };
    // A STEP is the one clock hazard for genlock, judged even before the rate window fills.
    let beyond_bound = max_step_ms.saturating_abs() > step_bound_ms;
    QpcDriftVerdict {
        beyond_bound,
        measured_ppm,
    }
}

/// Issue 1372 — the largest single-sample wall jump (ms) the widget BOOKS as a coordinated
/// dantesync fleet DATE step: two 30 fps frames. dantesync 1.9.0 steps the fleet date when its error
/// passes 50 ms, so a date step lands at ~50 ms plus the drift of one poll (the live one was
/// 51.039 ms); 66 ms keeps that with margin and nothing more. A bigger jump (a clock SET, an NTP
/// fallback step, another clock writer) stays in the history and DEGRADES — the hazard the step
/// verdict exists for (#1357).
pub const GENLOCK_QPC_WALL_STEP_BOOK_MAX_MS: i64 = 66;
/// Issue 1372 — how many wall steps the widget books inside one [`GENLOCK_QPC_WINDOW_S`]. A second
/// step in the window is a step STORM (dantesync steps the date every ~1.8 h) and keeps DEGRADING.
pub const GENLOCK_QPC_WALL_STEPS_PER_WINDOW: i64 = 1;

/// Issue 1372 — whether a single-sample wall jump is a BOOKED date step: returns the jump to
/// re-baseline the widget's qpc history by, or `0` to leave it in the history.
///
/// A coordinated dantesync fleet date step (−51 ms live, 25.9.2026 23:17:07 UTC) moves the wall
/// clock at once while the media clock follows only the dantesync RATE (issue 1372 part A, by
/// design), and the render tick re-grids onto the stepped wall in ONE tick (`crate::genlock_wall_step`).
/// Such a step is no genlock hazard any more, so the widget re-baselines its history by the jump,
/// logs one `genlock-wall-step:` line and stays LOCKED — before issue 1372 it read the step as a
/// `qpc_drift` hazard for the whole 300 s window (resolume DEGRADED from the step on). Booked only
/// when `step_bound_ms < |jump| <= book_max_ms` and fewer than `steps_per_window` steps were booked in
/// the window: a bigger jump and a step STORM stay in the history and still DEGRADE through
/// [`qpc_drift_beyond_bound`]; a jump within the bound never degrades, so it is not booked either.
///
/// Byte-for-byte mirror of `genlock_qpc_wall_step_rebase_ms` in `GenlockLockState.hpp` — the parity
/// gate `tests/genlock_qpc_wall_step_parity_1372.rs` lifts that function.
pub fn qpc_wall_step_rebase_ms(
    jump_ms: i64,
    step_bound_ms: i64,
    book_max_ms: i64,
    booked_in_window: i64,
    steps_per_window: i64,
) -> i64 {
    let mag = jump_ms.saturating_abs();
    if mag > step_bound_ms && mag <= book_max_ms && booked_in_window < steps_per_window {
        jump_ms
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- #1299 Part 4 / #1357 scope C: the qpc_drift term is a wall STEP, one semantics per box ----
    //
    // Live fixtures from 24.9.2026 (issue 1357 validation): every `qpc_drift` DEGRADED on both boxes
    // came from the removed RATE-vs-instantaneous-slew branch, none from a step. strih-lx (Linux,
    // `CLOCK_MONOTONIC` is kernel-disciplined): measured 0.0 on all 689 samples while dantesync's
    // `f_ptp + f_phase` swung -160..+171 ppm. stream (Windows, free QPC): measured 23.4 ppm while the
    // instantaneous `f_ptp + f_phase` read 109.8. Neither is a genlock hazard; a STEP is, on both.

    #[test]
    fn window_rate_ppm_matches_the_overnight_strih_slope() {
        // 742 − 101 = 641 ms over 12.5 h (45_000_000 ms) ≈ 14.24 ppm.
        assert!((qpc_window_rate_ppm(641, 45_000_000) - 14.2444).abs() < 0.001);
    }

    #[test]
    fn window_rate_ppm_is_zero_for_a_nonpositive_span() {
        assert_eq!(qpc_window_rate_ppm(5, 0), 0.0);
        assert_eq!(qpc_window_rate_ppm(5, -10), 0.0);
    }

    #[test]
    fn steady_disciplined_slew_does_not_degrade() {
        // A Windows box: the wall runs ≈14 ppm against the free QPC crystal, no step.
        let v = qpc_drift_beyond_bound(true, 641, 45_000_000, 0, GENLOCK_QPC_STEP_BOUND_MS);
        assert!((v.measured_ppm - 14.2444).abs() < 0.001);
        assert!(!v.beyond_bound);
    }

    #[test]
    fn a_step_within_the_window_degrades_even_before_ready() {
        // A 40 ms single-sample jump (an NTP RTC step) > one 30 fps frame (33 ms).
        let v = qpc_drift_beyond_bound(false, 0, 0, 40, GENLOCK_QPC_STEP_BOUND_MS);
        assert!(v.beyond_bound);
        let back = qpc_drift_beyond_bound(true, -40, 300_000, -40, GENLOCK_QPC_STEP_BOUND_MS);
        assert!(back.beyond_bound, "a backward step degrades too");
    }

    #[test]
    fn a_step_at_the_bound_is_not_beyond_and_one_over_is_1357() {
        assert!(
            !qpc_drift_beyond_bound(true, 0, 300_000, 33, GENLOCK_QPC_STEP_BOUND_MS).beyond_bound
        );
        assert!(
            qpc_drift_beyond_bound(true, 0, 300_000, 34, GENLOCK_QPC_STEP_BOUND_MS).beyond_bound
        );
    }

    #[test]
    fn a_large_rate_alone_no_longer_degrades_1357() {
        // ≈133 ppm over a filled window with a sub-frame step. The render tick re-derives every
        // deadline from the wall clock and absorbs up to 2 ms per tick, so a rate is not a genlock
        // hazard; it stays REPORT-ONLY telemetry (measured_ppm) and never feeds the verdict.
        let v = qpc_drift_beyond_bound(true, 6, 45_000, 1, GENLOCK_QPC_STEP_BOUND_MS);
        assert!(
            v.measured_ppm > 120.0,
            "the rate is still measured for telemetry"
        );
        assert!(!v.beyond_bound);
    }

    #[test]
    fn linux_disciplined_monotonic_window_never_degrades_1357() {
        // strih-lx 24.9. 01:10:49: `CLOCK_MONOTONIC` shares the kernel frequency discipline, so the
        // windowed wall-vs-monotonic delta is 0 by construction (dantesync reported -68.5 ppm, later
        // +170.9 — a servo excursion, not a wall-vs-monotonic hazard).
        let v = qpc_drift_beyond_bound(true, 0, 300_000, 0, GENLOCK_QPC_STEP_BOUND_MS);
        assert_eq!(v.measured_ppm, 0.0);
        assert!(!v.beyond_bound);
    }

    #[test]
    fn windows_and_linux_windows_give_the_same_verdict_1357() {
        // stream 24.9. 05:09:00 (Windows): 7 ms accrued over a 299 s window = 23.4 ppm, no step.
        // strih-lx the same minute (Linux): 0 ms accrued. ONE semantics: the same (no-step) verdict,
        // and the same (step) verdict once either window carries a 40 ms wall step.
        let win = qpc_drift_beyond_bound(true, 7, 299_000, 1, GENLOCK_QPC_STEP_BOUND_MS);
        let lx = qpc_drift_beyond_bound(true, 0, 299_000, 0, GENLOCK_QPC_STEP_BOUND_MS);
        assert!((win.measured_ppm - 23.4114).abs() < 0.001);
        assert_eq!(win.beyond_bound, lx.beyond_bound);
        assert!(!win.beyond_bound);
        let win_step = qpc_drift_beyond_bound(true, 47, 299_000, 41, GENLOCK_QPC_STEP_BOUND_MS);
        let lx_step = qpc_drift_beyond_bound(true, 40, 299_000, 40, GENLOCK_QPC_STEP_BOUND_MS);
        assert!(win_step.beyond_bound && lx_step.beyond_bound);
    }

    #[test]
    fn not_ready_window_reports_no_rate_and_never_degrades_on_it() {
        let v = qpc_drift_beyond_bound(false, 999, 1000, 0, GENLOCK_QPC_STEP_BOUND_MS);
        assert_eq!(v.measured_ppm, 0.0);
        assert!(!v.beyond_bound);
    }
}
