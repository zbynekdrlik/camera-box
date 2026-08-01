"""#806 -- unit tests for scripts/av_sync_outer_loop_guard.py.

MIRRORS the test vectors in `src/asrc_outer_loop.rs`'s own `#[cfg(test)]` module literally (same
numbers, same expected outcomes) -- proving numeric parity between the Rust reference and this
Python port, the same convention `tests/python/test_av_sync_calibrate.py`'s
`TestRequiredDelayMs` already established for `required_delay_ms` vs `src/qpsk_marker.rs`.
"""

import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import av_sync_outer_loop_guard as guard_mod  # noqa: E402
from av_sync_outer_loop_guard import OuterLoopGuard, RESIDUAL_THRESHOLD_MS, STEP_PPM, WINDOW_N, OUTER_BIAS_MAX_PPM  # noqa: E402


class TestPartialWindow:
    def test_a_partial_window_never_triggers_a_correction(self):
        g = OuterLoopGuard()
        for _ in range(WINDOW_N - 1):
            assert g.observe(1000.0) is None
        assert g.bias_ppm == 0.0


class TestSubThreshold:
    def test_sub_threshold_residuals_never_trigger_a_correction(self):
        g = OuterLoopGuard()
        for _ in range(WINDOW_N):
            assert g.observe(10.0) is None
        assert g.bias_ppm == 0.0

    def test_residual_exactly_at_the_threshold_does_not_trigger(self):
        g = OuterLoopGuard()
        for _ in range(WINDOW_N):
            assert g.observe(RESIDUAL_THRESHOLD_MS) is None
        assert g.bias_ppm == 0.0

    def test_one_transient_outlier_averaged_with_quiet_samples_stays_under_threshold(self):
        g = OuterLoopGuard()
        g.observe(90.0)
        g.observe(0.0)
        assert g.observe(0.0) is None  # avg = 30.0, under RESIDUAL_THRESHOLD_MS
        assert g.bias_ppm == 0.0


class TestSustainedCorrection:
    def test_sustained_positive_residual_nudges_bias_up_by_one_step(self):
        g = OuterLoopGuard()
        g.observe(60.0)
        g.observe(60.0)
        event = g.observe(60.0)
        assert event is not None
        assert event.avg_residual_ms == 60.0
        assert event.previous_bias_ppm == 0.0
        assert event.new_bias_ppm == STEP_PPM
        assert g.bias_ppm == STEP_PPM

    def test_sustained_negative_residual_nudges_bias_down_by_one_step(self):
        g = OuterLoopGuard()
        g.observe(-60.0)
        g.observe(-60.0)
        event = g.observe(-60.0)
        assert event is not None
        assert event.avg_residual_ms == -60.0
        assert event.new_bias_ppm == -STEP_PPM
        assert g.bias_ppm == -STEP_PPM


class TestSaturation:
    def test_bias_never_exceeds_the_ticket_max_and_stops_reporting_once_saturated(self):
        g = OuterLoopGuard()
        for _ in range(WINDOW_N + 20):
            g.observe(60.0)
        assert g.bias_ppm == OUTER_BIAS_MAX_PPM
        g.observe(60.0)
        g.observe(60.0)
        assert g.observe(60.0) is None
        assert g.bias_ppm == OUTER_BIAS_MAX_PPM


class TestFromBiasPpm:
    def test_from_bias_ppm_restores_and_clamps(self):
        g = OuterLoopGuard.from_bias_ppm(9999.0)
        assert g.bias_ppm == OUTER_BIAS_MAX_PPM
        g = OuterLoopGuard.from_bias_ppm(3.0)
        assert g.bias_ppm == 3.0
        # The window is empty -- one sample alone must not be enough to trigger a correction.
        assert g.observe(1000.0) is None


class TestConstantsMirrorRust:
    """Pins the exact numeric values -- a future edit to either side that forgets the other is
    caught here, not just by behavior tests above."""

    def test_constants_match_the_ticket_and_the_rust_reference(self):
        assert guard_mod.WINDOW_N == 3
        assert guard_mod.RESIDUAL_THRESHOLD_MS == 40.0
        assert guard_mod.STEP_PPM == 1.0
        assert guard_mod.OUTER_BIAS_MAX_PPM == 10.0
