"""#1299 Part 4 + #1357 scope C — the wall-vs-monotonic `qpc_drift` verdict.

History: the in-OBS genlock LOCK indicator's `qpc_drift` term first gated on the CUMULATIVE
`wall_qpc_drift_ms`, which grows unbounded on a dantesync-disciplined Windows box (~50 ms/h) and
false-paged the fleet overnight 15./16.9.2026. #1299 Part 4 replaced it with a WINDOWED RATE compared
against the slew dantesync reports (`f_ptp + f_phase`) plus a STEP detector.

#1357 scope C: that RATE branch compared a 300 s windowed rate against ONE instantaneous dantesync
sample, and it meant a different thing per box — on Linux `CLOCK_MONOTONIC` is kernel-disciplined
together with `CLOCK_REALTIME`, so the measured side is 0 by construction (live strih-lx 24.9.: 0.0 on
all 689 samples while `f_ptp + f_phase` swung -160..+171 ppm -> 28 false DEGRADED), on Windows it is
the free QPC crystal (stream: measured 23.4 vs an instantaneous 109.8 -> 4 false DEGRADED). The term is
now the wall STEP only — the one clock hazard for genlock, identical on every box — and the windowed
rate stays report-only telemetry. These tests feed the pure python mirror the same shapes the Rust
authority + the C parity gate use.
"""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import genlock_lock_decision as gld  # noqa: E402

# The step bound the widget uses (mirrored here so the test asserts the real gate).
STEP_BOUND = gld.GENLOCK_QPC_STEP_BOUND_MS


def _decide(beyond):
    return gld.decide(
        n_inputs=10, n_locked=10, recent_event=False, qpc_drift_beyond_bound=beyond,
        clock_present=True, clock_locked=True, clock_ntp_failed=False,
        output_present=True, output_stamping=True, n_absent=0,
    )


def test_window_rate_ppm_matches_the_overnight_strih_slope():
    # 742 − 101 = 641 ms accrued over 12.5 h = 45_000_000 ms -> ≈ 14.24 ppm.
    ppm = gld.qpc_window_rate_ppm(641, 45_000_000)
    assert abs(ppm - 14.244) < 0.01


def test_window_rate_ppm_is_zero_for_a_nonpositive_span():
    assert gld.qpc_window_rate_ppm(5, 0) == 0.0
    assert gld.qpc_window_rate_ppm(5, -10) == 0.0


def test_steady_disciplined_slew_does_not_degrade_and_box_reads_locked():
    # A Windows box: the wall runs ≈14 ppm against the free QPC crystal, no step.
    beyond, measured = gld.qpc_drift_beyond_bound(
        rate_ready=True, drift_delta_ms=641, elapsed_ms=45_000_000,
        max_step_ms=0, step_bound_ms=STEP_BOUND,
    )
    assert abs(measured - 14.244) < 0.01
    assert beyond is False
    assert _decide(beyond) == (gld.ST_LOCKED, gld.R_NONE)


def test_a_step_within_the_window_degrades_qpc():
    # A 40 ms single-sample jump (an NTP RTC step) > one 30 fps frame (33 ms) -> DEGRADED, immediately
    # (judged even before the rate window has filled).
    beyond, _ = gld.qpc_drift_beyond_bound(
        rate_ready=False, drift_delta_ms=0, elapsed_ms=0, max_step_ms=40, step_bound_ms=STEP_BOUND,
    )
    assert beyond is True
    assert _decide(beyond) == (gld.ST_DEGRADED, gld.R_QPC_DRIFT)


def test_a_step_at_the_bound_is_not_beyond_and_one_over_is_1357():
    at, _ = gld.qpc_drift_beyond_bound(
        rate_ready=True, drift_delta_ms=0, elapsed_ms=300_000, max_step_ms=33, step_bound_ms=STEP_BOUND,
    )
    over, _ = gld.qpc_drift_beyond_bound(
        rate_ready=True, drift_delta_ms=0, elapsed_ms=300_000, max_step_ms=-34, step_bound_ms=STEP_BOUND,
    )
    assert at is False
    assert over is True


def test_a_large_rate_alone_no_longer_degrades_1357():
    # ≈134 ppm over a filled window, sub-frame step. A rate is not a genlock hazard (the render tick
    # re-derives every deadline from the wall clock and absorbs 2 ms/tick); it stays telemetry.
    beyond, measured = gld.qpc_drift_beyond_bound(
        rate_ready=True, drift_delta_ms=6, elapsed_ms=45_000, max_step_ms=1, step_bound_ms=STEP_BOUND,
    )
    assert measured > 120.0
    assert beyond is False


def test_live_strih_lx_and_stream_windows_give_the_same_verdict_1357():
    # strih-lx (Linux, disciplined CLOCK_MONOTONIC): 0 ms over the window. stream (Windows, free QPC)
    # 24.9. 05:09:00: 7 ms over 299 s = 23.4 ppm. Both reported qpc_drift DEGRADED under the removed
    # rate branch; under the one step semantics both read LOCKED, and both DEGRADE on a real step.
    lx, lx_ppm = gld.qpc_drift_beyond_bound(
        rate_ready=True, drift_delta_ms=0, elapsed_ms=300_000, max_step_ms=0, step_bound_ms=STEP_BOUND,
    )
    win, win_ppm = gld.qpc_drift_beyond_bound(
        rate_ready=True, drift_delta_ms=7, elapsed_ms=299_000, max_step_ms=1, step_bound_ms=STEP_BOUND,
    )
    assert lx_ppm == 0.0
    assert abs(win_ppm - 23.411) < 0.01
    assert lx is False and win is False
    assert _decide(lx) == _decide(win) == (gld.ST_LOCKED, gld.R_NONE)
    lx_step, _ = gld.qpc_drift_beyond_bound(
        rate_ready=True, drift_delta_ms=40, elapsed_ms=300_000, max_step_ms=40, step_bound_ms=STEP_BOUND,
    )
    win_step, _ = gld.qpc_drift_beyond_bound(
        rate_ready=True, drift_delta_ms=47, elapsed_ms=299_000, max_step_ms=41, step_bound_ms=STEP_BOUND,
    )
    assert lx_step is True and win_step is True


def test_the_rate_bound_is_gone_1357():
    # A re-added rate bound would re-open the per-box divergence above.
    assert not hasattr(gld, "GENLOCK_QPC_DRIFT_PPM_BOUND")


def test_not_ready_window_reports_no_rate_and_never_degrades_on_it():
    beyond, measured = gld.qpc_drift_beyond_bound(
        rate_ready=False, drift_delta_ms=999, elapsed_ms=1000, max_step_ms=0, step_bound_ms=STEP_BOUND,
    )
    assert measured == 0.0
    assert beyond is False
