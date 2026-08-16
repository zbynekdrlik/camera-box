"""#799 — unit tests for the pure helper in scripts/imag-render-stats.py.

The dev1-side reader that snapshots imag OBS WS `GetStats` twice and emits ONE
`RENDER|<active_fps>|<avg_ms>|<render_skipped_frac>|<render_advanced>` line for the
imag-power-envelope-alert-watchdog's render-degradation discriminator (#799). Tests the pure,
OBS-independent line builder + the #399 lazy-import discipline (the module MUST import cleanly
with no websocket-client available — only the actual WS connect path needs it, mirroring
scripts/obs-liveness-probe.py). RED before the script exists (import fails); GREEN after.
"""
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "imag-render-stats.py"


def _load_module(name="imag_render_stats_test"):
    spec = importlib.util.spec_from_file_location(name, _MOD_PATH)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_module_imports_cleanly_with_websocket_unavailable(monkeypatch):
    # #399 discipline: a top-level `from websocket import ...` would break the Rust test job's
    # runner (no websocket-client). The module must import with websocket unavailable; only the
    # connect path needs it.
    monkeypatch.setitem(sys.modules, "websocket", None)
    mod = _load_module("imag_render_stats_no_ws")
    assert callable(mod.main)
    assert callable(mod.render_line)


def test_render_line_healthy():
    mod = _load_module()
    s0 = {"activeFps": 60.0, "averageFrameRenderTime": 5.3,
          "renderTotalFrames": 1000, "renderSkippedFrames": 0}
    s1 = {"activeFps": 60.0, "averageFrameRenderTime": 5.3,
          "renderTotalFrames": 1240, "renderSkippedFrames": 0}
    line = mod.render_line(s0, s1, 60.0)
    assert line == "RENDER|60.00|5.30|0.0000|true"


def test_render_line_the_799_degrade_curve():
    mod = _load_module()
    # 52.8fps / 17.2ms / ~1.6% skip over the window — the ticket's own degrade numbers.
    s0 = {"activeFps": 52.8, "averageFrameRenderTime": 17.2,
          "renderTotalFrames": 100000, "renderSkippedFrames": 5000}
    s1 = {"activeFps": 52.8, "averageFrameRenderTime": 17.2,
          "renderTotalFrames": 100211, "renderSkippedFrames": 5003}
    line = mod.render_line(s0, s1, 60.0)
    fields = line.split("|")
    assert fields[0] == "RENDER"
    assert fields[1] == "52.80"
    assert fields[2] == "17.20"
    assert 0.0 < float(fields[3]) < 0.05  # 3/211 ≈ 1.4%
    assert fields[4] == "true"


def test_render_line_full_stall_advanced_false():
    mod = _load_module()
    # renderTotalFrames did NOT advance over the window -> a full render-loop stall (#935).
    s0 = {"activeFps": 30.0, "averageFrameRenderTime": 0.0,
          "renderTotalFrames": 500, "renderSkippedFrames": 0}
    s1 = {"activeFps": 30.0, "averageFrameRenderTime": 0.0,
          "renderTotalFrames": 500, "renderSkippedFrames": 0}
    assert mod.render_line(s0, s1, 60.0).split("|")[4] == "false"


def test_render_line_counter_reset_is_unknown_advancement():
    mod = _load_module()
    # A negative delta = OBS restarted between snapshots -> advancement unknown, never a false
    # "stalled". (render_advanced = (r_tot>0) if r_tot>=0 else None, per obs-liveness-probe.py.)
    s0 = {"activeFps": 60.0, "averageFrameRenderTime": 5.0,
          "renderTotalFrames": 900, "renderSkippedFrames": 0}
    s1 = {"activeFps": 60.0, "averageFrameRenderTime": 5.0,
          "renderTotalFrames": 5, "renderSkippedFrames": 0}
    assert mod.render_line(s0, s1, 60.0).split("|")[4] == "unknown"


def test_render_line_zero_total_delta_no_div_by_zero():
    mod = _load_module()
    s0 = {"activeFps": 60.0, "averageFrameRenderTime": 5.0,
          "renderTotalFrames": 500, "renderSkippedFrames": 0}
    s1 = {"activeFps": 60.0, "averageFrameRenderTime": 5.0,
          "renderTotalFrames": 500, "renderSkippedFrames": 0}
    # r_tot == 0 -> frac 0.0, no ZeroDivisionError.
    assert mod.render_line(s0, s1, 60.0).split("|")[3] == "0.0000"
