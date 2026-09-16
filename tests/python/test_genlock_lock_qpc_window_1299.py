"""#1299 Part 4 — the windowed wall-vs-QPC drift verdict.

RED→GREEN for the reopen bug: the in-OBS genlock LOCK indicator's `qpc_drift` term used to gate on
the CUMULATIVE `wall_qpc_drift_ms` (an accumulator since OBS start). On a dantesync-disciplined box the
wall clock legitimately runs at the grandmaster rate (`f_ptp + f_phase` ≈ +10…20 ppm) vs the free QPC
crystal, so that accumulator grows unbounded (~50 ms/h) and crossed the 100 ms bound after ~2 h on
every box — 38 false Discord pages overnight 15./16.9.2026.

The fix redefines the term as a WINDOWED RATE compared with the slew the clock reports it is applying,
plus a STEP detector (the genuine genlock hazard). This test feeds the pure python mirror the SAME
shapes the Rust authority + C parity gate use and asserts the three-state precedence:
  - the overnight strih shape (cumulative 101 → 742 over 12.5 h at a steady ≈14 ppm, expected ≈12) must
    NOT trip qpc_drift -> LOCKED (the reopen scenario);
  - a 40 ms STEP within one window -> DEGRADED/qpc_drift;
  - a 120 ppm rate mismatch -> DEGRADED/qpc_drift.

These functions do not exist on the pre-fix tree, so this file is RED until the GREEN commit adds them.
"""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import genlock_lock_decision as gld  # noqa: E402

# The window/bound constants the widget uses (mirrored here so the test asserts the real gate).
PPM_BOUND = gld.GENLOCK_QPC_DRIFT_PPM_BOUND
STEP_BOUND = gld.GENLOCK_QPC_STEP_BOUND_MS


def test_window_rate_ppm_matches_the_overnight_strih_slope():
    # 742 − 101 = 641 ms accrued over 12.5 h = 45_000_000 ms -> ≈ 14.24 ppm.
    ppm = gld.qpc_window_rate_ppm(641, 45_000_000)
    assert abs(ppm - 14.244) < 0.01


def test_window_rate_ppm_is_zero_for_a_nonpositive_span():
    assert gld.qpc_window_rate_ppm(5, 0) == 0.0
    assert gld.qpc_window_rate_ppm(5, -10) == 0.0


def test_steady_disciplined_slew_does_not_degrade_and_box_reads_locked():
    # The reopen scenario: measured ≈14 ppm, the clock reports it is applying ≈12 ppm (f_ptp+f_phase).
    beyond, measured = gld.qpc_drift_beyond_bound(
        rate_ready=True, drift_delta_ms=641, elapsed_ms=45_000_000,
        expected_ppm=12.0, ppm_bound=PPM_BOUND, max_step_ms=0, step_bound_ms=STEP_BOUND,
    )
    assert abs(measured - 14.244) < 0.01
    assert beyond is False
    # …and the full three-state decision with that term false + every other term healthy = LOCKED.
    state, reason = gld.decide(
        n_inputs=10, n_locked=10, recent_event=False, qpc_drift_beyond_bound=beyond,
        clock_present=True, clock_locked=True, clock_ntp_failed=False,
        output_present=True, output_stamping=True, n_absent=0,
    )
    assert (state, reason) == (gld.ST_LOCKED, gld.R_NONE)


def test_a_step_within_the_window_degrades_qpc():
    # A 40 ms single-sample jump (an NTP RTC step) > one 30 fps frame (33 ms) -> DEGRADED, immediately
    # (judged even before the rate window has filled).
    beyond, _ = gld.qpc_drift_beyond_bound(
        rate_ready=False, drift_delta_ms=0, elapsed_ms=0,
        expected_ppm=12.0, ppm_bound=PPM_BOUND, max_step_ms=40, step_bound_ms=STEP_BOUND,
    )
    assert beyond is True
    state, reason = gld.decide(
        n_inputs=10, n_locked=10, recent_event=False, qpc_drift_beyond_bound=beyond,
        clock_present=True, clock_locked=True, clock_ntp_failed=False,
        output_present=True, output_stamping=True, n_absent=0,
    )
    assert (state, reason) == (gld.ST_DEGRADED, gld.R_QPC_DRIFT)


def test_a_large_rate_mismatch_degrades_qpc():
    # measured ≈134 ppm over a filled window while the clock reports ≈12 ppm -> |134−12| ≫ 50 ppm.
    beyond, measured = gld.qpc_drift_beyond_bound(
        rate_ready=True, drift_delta_ms=6, elapsed_ms=45_000,
        expected_ppm=12.0, ppm_bound=PPM_BOUND, max_step_ms=1, step_bound_ms=STEP_BOUND,
    )
    assert measured > 120.0
    assert beyond is True


def test_a_small_step_and_matching_rate_stays_green():
    # a benign 1 ms accrual over the window (steady 14 ppm-ish) and a sub-frame step -> not beyond.
    beyond, _ = gld.qpc_drift_beyond_bound(
        rate_ready=True, drift_delta_ms=4, elapsed_ms=300_000,
        expected_ppm=13.0, ppm_bound=PPM_BOUND, max_step_ms=1, step_bound_ms=STEP_BOUND,
    )
    assert beyond is False


def test_not_ready_window_never_degrades_on_rate_alone():
    # Before the window fills, the RATE branch must not fire (only a real STEP can).
    beyond, measured = gld.qpc_drift_beyond_bound(
        rate_ready=False, drift_delta_ms=999, elapsed_ms=1000,
        expected_ppm=12.0, ppm_bound=PPM_BOUND, max_step_ms=0, step_bound_ms=STEP_BOUND,
    )
    assert measured == 0.0
    assert beyond is False
