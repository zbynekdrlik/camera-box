"""#328 — unit tests for the obs_phase2 hard overall-timeout on blocking obs-websocket ops.

## The bug (#328, RUN_ID 312001)

The #312 all-cambox rig run HUNG ~28 min on `obs_phase2.py prod-scene --host <stream>` and then
its cleanup-trap `teardown --host <stream>` ALSO hung on the same path. stream's OBS was healthy
the whole time (WS :4455 listening) — the hang was SCRIPT-SIDE: `_rpc`'s read loop drains op-5
EVENTS until the matching op-7 response arrives, and while OBS renegotiates an NDI source it can
flood events, so every `recv()` keeps succeeding within the socket timeout yet the response NEVER
comes and the loop spins forever (no overall wall-clock deadline). The hung teardown then never
reached the cam1 device restore, so cam1's burn binary kept holding /dev/video0 (#281 class).

## The fix these tests lock

`_rpc` now bounds every request by a hard wall-clock deadline (`OBS_OP_TIMEOUT_S`, default 60 s,
env-overridable) via the pure `_rpc_timed_out(elapsed_s, timeout_s)` decision helper. Past the
deadline `_rpc` raises (fails loud, non-zero exit) instead of blocking the run indefinitely.

These tests pin the PURE decision helper + the named/env-overridable constant (no live WebSocket).
"""
import importlib.util
import os
import pathlib
import sys

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"


def _load(modname="obs_phase2_timeout"):
    spec = importlib.util.spec_from_file_location(modname, _MOD_PATH)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[modname] = mod
    spec.loader.exec_module(mod)
    return mod


obs_phase2 = _load()


def test_timeout_helper_exists():
    # The whole #328 fix hinges on this pure decision helper existing.
    assert callable(obs_phase2._rpc_timed_out)


def test_not_timed_out_before_deadline():
    # Just started, and just shy of the deadline: keep waiting (False).
    assert obs_phase2._rpc_timed_out(0.0, 60.0) is False
    assert obs_phase2._rpc_timed_out(59.9, 60.0) is False


def test_timed_out_at_and_past_deadline():
    # AT the deadline and beyond: the op MUST fail loud (True). This is the bound that
    # turns the #328 ~28-min event-flood spin into a bounded, loud failure.
    assert obs_phase2._rpc_timed_out(60.0, 60.0) is True
    assert obs_phase2._rpc_timed_out(120.0, 60.0) is True


def test_nonpositive_timeout_disables_the_bound():
    # A non-positive timeout is the explicit opt-out (wait indefinitely) — even far past any
    # elapsed time it is never "timed out".
    assert obs_phase2._rpc_timed_out(10_000.0, 0.0) is False
    assert obs_phase2._rpc_timed_out(10_000.0, -1.0) is False


def test_default_timeout_constant_is_60s():
    # Named constant, sensible 60 s default (the #328 acceptance value).
    assert isinstance(obs_phase2.OBS_OP_TIMEOUT_S, float)
    assert obs_phase2.OBS_OP_TIMEOUT_S == 60.0


def test_timeout_constant_is_env_overridable(monkeypatch):
    # Operators must be able to widen/narrow the bound without editing code (a legitimately
    # slow op, or a tighter rig). A fresh module load under the env reflects the override.
    monkeypatch.setenv("OBS_OP_TIMEOUT_S", "12.5")
    try:
        reloaded = _load("obs_phase2_timeout_env")
        assert reloaded.OBS_OP_TIMEOUT_S == 12.5
    finally:
        os.environ.pop("OBS_OP_TIMEOUT_S", None)
        sys.modules.pop("obs_phase2_timeout_env", None)
