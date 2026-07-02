"""#391 — unit tests for the pure helpers in scripts/obs-liveness-probe.py.

Tests the OBS-independent logic: --box / --process-state parsing, the
GetStats-delta -> sample builder, process-state merging, and the #399 lazy-
import discipline (a top-level `from websocket import ...` broke
tests/harness_rig_ndi_mapping.rs on the Rust test job's runner, which has no
websocket-client — this module must import cleanly even when websocket is
unavailable, deferring the ImportError to the actual connect path only).
"""
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs-liveness-probe.py"


def _load_module(name="obs_liveness_probe_test"):
    spec = importlib.util.spec_from_file_location(name, _MOD_PATH)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# ---------------------------------------------------------------------------
# #399 lazy-import discipline — the module MUST import cleanly with no
# websocket-client available; only the actual WS connect path needs it.
# ---------------------------------------------------------------------------

def test_module_imports_cleanly_with_websocket_unavailable(monkeypatch):
    # Simulate "websocket-client is not installed" for THIS import: setting the
    # module to None in sys.modules makes any `import websocket` / `from websocket
    # import ...` raise ImportError immediately, without needing to uninstall the
    # real package. If the script had a top-level import (the #399 bug), exec_module
    # would raise here.
    monkeypatch.setitem(sys.modules, "websocket", None)
    mod = _load_module("obs_liveness_probe_no_ws")
    assert callable(mod.main)


def test_ws_helper_raises_missing_dep_when_websocket_unavailable(monkeypatch):
    mod = _load_module("obs_liveness_probe_ws_check_missing")
    monkeypatch.setitem(sys.modules, "websocket", None)
    with pytest.raises(SystemExit, match="missing dep"):
        mod._ws()


def test_ws_helper_returns_the_pair_when_websocket_available():
    mod = _load_module("obs_liveness_probe_ws_check_present")
    exc, connect = mod._ws()
    assert callable(connect)
    assert isinstance(exc, type)


# ---------------------------------------------------------------------------
# --box parsing
# ---------------------------------------------------------------------------

_mod = _load_module()


def test_parse_box_with_target_fps():
    label, host, target = _mod._parse_box("strih=10.77.9.202:60")
    assert (label, host, target) == ("strih", "10.77.9.202", 60.0)


def test_parse_box_defaults_target_fps_to_60():
    label, host, target = _mod._parse_box("stream=10.77.9.204")
    assert (label, host, target) == ("stream", "10.77.9.204", 60.0)


def test_parse_box_rejects_missing_equals():
    with pytest.raises(ValueError):
        _mod._parse_box("10.77.9.202:60")


def test_parse_box_rejects_empty_label_or_host():
    with pytest.raises(ValueError):
        _mod._parse_box("=10.77.9.202")
    with pytest.raises(ValueError):
        _mod._parse_box("strih=")


# ---------------------------------------------------------------------------
# --process-state parsing
# ---------------------------------------------------------------------------

def test_parse_process_state_all_keys():
    label, state = _mod._parse_process_state("stream=count:1,responding:false,cpu:168.0")
    assert label == "stream"
    assert state == {"obs64_count": 1, "responding": False, "cpu_percent": 168.0}


def test_parse_process_state_partial_keys_leave_others_none():
    label, state = _mod._parse_process_state("strih=responding:true")
    assert label == "strih"
    assert state == {"obs64_count": None, "responding": True, "cpu_percent": None}


def test_parse_process_state_rejects_unknown_key():
    with pytest.raises(ValueError, match="unknown"):
        _mod._parse_process_state("strih=bogus:1")


def test_parse_process_state_rejects_bad_responding_value():
    with pytest.raises(ValueError):
        _mod._parse_process_state("strih=responding:maybe")


def test_parse_process_state_rejects_missing_equals():
    with pytest.raises(ValueError):
        _mod._parse_process_state("count:1")


# ---------------------------------------------------------------------------
# _render_sample — GetStats delta -> obs_watchdog-shaped sample
# ---------------------------------------------------------------------------

def test_render_sample_computes_render_skip_delta_fraction():
    s0 = {"activeFps": 60.0, "averageFrameRenderTime": 11.3,
          "renderTotalFrames": 1000, "renderSkippedFrames": 10}
    s1 = {"activeFps": 60.0, "averageFrameRenderTime": 11.3,
          "renderTotalFrames": 1360, "renderSkippedFrames": 11}
    sample = _mod._render_sample(s0, s1, 60.0)
    assert sample["ws_reachable"] is True
    assert sample["active_fps"] == 60.0
    assert sample["avg_render_time_ms"] == 11.3
    # delta: (11-10) skipped / (1360-1000) total = 1/360
    assert sample["render_skipped_frac"] == pytest.approx(1.0 / 360.0)
    assert sample["target_fps"] == 60.0


def test_render_sample_zero_total_delta_gives_zero_frac_not_div_by_zero():
    s0 = {"renderTotalFrames": 500, "renderSkippedFrames": 5}
    s1 = {"renderTotalFrames": 500, "renderSkippedFrames": 5}
    sample = _mod._render_sample(s0, s1, 30.0)
    assert sample["render_skipped_frac"] == 0.0


def test_render_sample_real_incident_values():
    # The real #391 incident: 16.0% frames-missed-due-to-render-lag.
    s0 = {"activeFps": 25.0, "averageFrameRenderTime": 9.0,
          "renderTotalFrames": 0, "renderSkippedFrames": 0}
    s1 = {"activeFps": 25.0, "averageFrameRenderTime": 9.0,
          "renderTotalFrames": 100, "renderSkippedFrames": 16}
    sample = _mod._render_sample(s0, s1, 30.0)
    assert sample["render_skipped_frac"] == pytest.approx(0.16)


def test_unreachable_sample_has_no_measured_fields():
    sample = _mod._unreachable_sample(30.0)
    assert sample == {
        "ws_reachable": False,
        "active_fps": None,
        "avg_render_time_ms": None,
        "render_skipped_frac": None,
        "target_fps": 30.0,
    }


# ---------------------------------------------------------------------------
# _merge_process_state
# ---------------------------------------------------------------------------

def test_merge_process_state_layers_present_fields_only():
    measured = _mod._render_sample(
        {"renderTotalFrames": 0, "renderSkippedFrames": 0},
        {"renderTotalFrames": 100, "renderSkippedFrames": 0, "activeFps": 60.0,
         "averageFrameRenderTime": 10.0},
        60.0,
    )
    state = {"obs64_count": 1, "responding": False, "cpu_percent": None}
    merged = _mod._merge_process_state(measured, state)
    assert merged["obs64_count"] == 1
    assert merged["responding"] is False
    # cpu_percent stayed None in state -> not added (absent, matching a WS-only pass)
    assert "cpu_percent" not in merged


def test_merge_process_state_none_leaves_sample_unmodified():
    measured = _mod._unreachable_sample(30.0)
    merged = _mod._merge_process_state(measured, None)
    assert merged == measured


# ---------------------------------------------------------------------------
# _find_verdict_bin
# ---------------------------------------------------------------------------

def test_find_verdict_bin_prefers_explicit_arg(tmp_path):
    fake_bin = tmp_path / "obs-watchdog-gate"
    fake_bin.write_text("#!/bin/sh\n")
    assert _mod._find_verdict_bin(str(fake_bin)) == str(fake_bin)


def test_find_verdict_bin_uses_env_var(tmp_path, monkeypatch):
    fake_bin = tmp_path / "obs-watchdog-gate"
    fake_bin.write_text("#!/bin/sh\n")
    monkeypatch.setenv("OBS_WATCHDOG_GATE_BIN", str(fake_bin))
    assert _mod._find_verdict_bin(None) == str(fake_bin)


def test_find_verdict_bin_exits_when_not_found(monkeypatch, tmp_path):
    monkeypatch.delenv("OBS_WATCHDOG_GATE_BIN", raising=False)
    monkeypatch.delenv("PROBE_BIN_DIR", raising=False)
    with pytest.raises(SystemExit, match="not found"):
        _mod._find_verdict_bin(None)
