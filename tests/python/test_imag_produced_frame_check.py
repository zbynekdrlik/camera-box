"""#887 — unit tests for scripts/imag_produced_frame_check.py's pure formatter.

Part of the imag "produced vs presented" comparison (see the sibling DRM-side
scripts/lib/imag-presented-frame-check.sh and its own tests). This file covers ONLY the
compositor-produced half: `produced_line` must format a GetStats responseData dict into the
one-line report scripts/recording-e2e.sh parses, using the correct fields (renderTotalFrames/
renderSkippedFrames/outputTotalFrames/outputSkippedFrames — the SAME render-health fields
render-budget-gate.py and obs-liveness-probe.py already trust, per obs-render-health-metric.md),
never the encoder-only outputTotalFrames alone (that stays green even when the render loop
itself chokes).
"""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import imag_produced_frame_check  # noqa: E402


class TestProducedLine:
    def test_formats_all_four_fields(self):
        stats = {
            "renderTotalFrames": 1000,
            "renderSkippedFrames": 3,
            "outputTotalFrames": 998,
            "outputSkippedFrames": 0,
        }
        line = imag_produced_frame_check.produced_line(stats)
        assert line == (
            "PRODUCED renderTotalFrames=1000 renderSkippedFrames=3 "
            "outputTotalFrames=998 outputSkippedFrames=0"
        )

    def test_missing_fields_default_to_zero_never_raises(self):
        # A malformed/partial GetStats response (e.g. a transient WS hiccup) must never crash
        # this report-only diagnostic — missing fields read as 0, not an exception.
        line = imag_produced_frame_check.produced_line({})
        assert line == (
            "PRODUCED renderTotalFrames=0 renderSkippedFrames=0 "
            "outputTotalFrames=0 outputSkippedFrames=0"
        )

    def test_coerces_float_stats_to_int(self):
        # obs-websocket returns JSON numbers; some fields can arrive as floats. The frame counts
        # this feeds into (a subtraction of two snapshots) must be whole numbers.
        stats = {
            "renderTotalFrames": 1000.0,
            "renderSkippedFrames": 3.0,
            "outputTotalFrames": 998.0,
            "outputSkippedFrames": 0.0,
        }
        line = imag_produced_frame_check.produced_line(stats)
        assert "renderTotalFrames=1000 " in line
        assert "renderSkippedFrames=3 " in line
