//! Issue 1372 — the ASRC's confirmed-step recovery (the booking of a measured sample loss and its
//! 1000 ppm payment), tested against [`RealtimeAsrcCompensator`]. A child of `asrc_bench`'s test
//! build only.

use super::*;

/// Issue 1372 (review round 1): a confirmed step books the MEASURED loss (the regression
/// residual), never the noisier per-callback level deficit; the level only confirms it, SIGNED —
/// a sample loss while the buffer went UP books nothing — and the total owed is capped at
/// ±`STEP_RECOVER_MAX_MS`, so no single step can queue more than ~100 s at 1000 ppm.
#[test]
fn a_confirmed_step_books_the_measured_loss_capped_and_signed_1372() {
    const TRUE_PPM: f64 = -5.0;
    const BLOCK_S: f64 = 1.0;
    const START_BUF_MS: f64 = 100.0;
    // Consecutive step windows of (samples lost, ms; how the live buffer level moved on that
    // window, ms) → the owed amount after the last one (before its own window's payment).
    fn booked(steps: &[(f64, f64)]) -> f64 {
        let mut c = RealtimeAsrcCompensator::new();
        let clock = DriftingAudioClock::new(TRUE_PPM);
        let mut buffer_ms = START_BUF_MS;
        let mut t = 0.0;
        while t < 200.0 {
            let raw = clock.raw_advance(BLOCK_S);
            let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
            buffer_ms += (corrected - BLOCK_S) * 1000.0;
            t += BLOCK_S;
        }
        for &(step_ms, buffer_move_ms) in steps {
            buffer_ms += buffer_move_ms;
            let raw_step = clock.raw_advance(BLOCK_S) - step_ms / 1000.0;
            let corrected = c.compensate_with_level(raw_step, BLOCK_S, buffer_ms);
            buffer_ms += (corrected - raw_step) * 1000.0;
        }
        c.step_recover_ms() - c.step_recover_ppm() * BLOCK_S / 1000.0
    }
    // The level read only 30 ms low for a 50 ms loss (≥ half: confirmed): the MEASURED 50 ms.
    let partial = booked(&[(50.0, -30.0)]);
    assert!(
        (partial - 50.0).abs() < 1.0,
        "issue 1372: a confirmed 50 ms loss must book the measured 50 ms, got {partial:.3}"
    );
    // Two 80 ms losses back to back (a single window loses at most what the window acceptance
    // lets through): the total owed is capped at STEP_RECOVER_MAX_MS.
    let big = booked(&[(80.0, -80.0), (80.0, -80.0)]);
    assert!(
        (big - STEP_RECOVER_MAX_MS).abs() < 1e-6,
        "issue 1372: the owed amount must be capped at {STEP_RECOVER_MAX_MS} ms, got {big:.3}"
    );
    // Samples lost but the buffer went UP by as much: not confirmed, nothing booked.
    let opposite = booked(&[(50.0, 30.0)]);
    assert!(
        opposite == 0.0,
        "issue 1372: a loss the buffer contradicts must book nothing, got {opposite:.3}"
    );
}
