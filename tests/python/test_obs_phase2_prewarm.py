"""#343 — unit tests for the prod_scene pre-warm fix.

## The bug (#343)

When prod_scene is called with --ensure-source (building the ephemeral REC-STRIH-TMP scene
over NDI 2ME PGM on the stream box), the subsequent SetCurrentProgramScene call freshly
activates the 450 ms genlock FIFO on NDI 2ME PGM. That activation blocks OBS from sending
the websocket response for >60 s — the #328 op-timeout fires and the proof can't run.

## The fix these tests lock (#343)

Two mitigations, both locked here (no live OBS):

  (a) SKIP SetCurrentProgramScene when the box is already on the target scene. The source is
      already warm (NDI 2ME PGM was previously activated), so there is zero activation cost.
      SetCurrentProgramScene must NOT be called in this case.

  (b) When a switch IS needed and --ensure-source is in play, call SetCurrentProgramScene with
      timeout_s=0 (no deadline) so the one-time FIFO init can complete without being cut off
      at 60 s. This is explicitly NOT a band-aid: it opts out of the #328 deadline only for
      this one legitimately unbounded op; all subsequent ops keep their 60 s cap.

Without --ensure-source (strih side, prod scene switch) neither mitigation applies — the
existing bounded switch is correct and must be preserved.
"""
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_prewarm", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_prewarm"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


class FakeWS:
    """Minimal websocket stand-in (prod_scene calls ws.close() in its finally block)."""

    def close(self):
        pass


def _make_fake_rpc(curr_prog, target_scene, ensure_source, scenes):
    """Return a (fake_rpc, rpc_calls) pair.

    *rpc_calls* accumulates every (op, payload, kwargs) tuple so tests can inspect
    what was called, with what arguments, including timeout_s.
    """
    rpc_calls = []

    def fake_rpc(ws, op, payload=None, ignore_err=False, timeout_s=None):
        rpc_calls.append({"op": op, "payload": payload or {}, "timeout_s": timeout_s})
        if op == "GetCurrentProgramScene":
            return {"currentProgramSceneName": curr_prog}
        if op == "GetStudioModeEnabled":
            return {"studioModeEnabled": False}
        if op == "GetCurrentPreviewScene":
            return {"currentPreviewSceneName": curr_prog}
        if op == "GetSceneList":
            return {"scenes": [{"sceneName": s} for s in scenes]}
        if op == "GetSceneItemList":
            # Pretend the ensure-source item is already in the scene so no CreateSceneItem.
            if ensure_source:
                return {"sceneItems": [{"sourceName": ensure_source}]}
            return {"sceneItems": []}
        if op == "GetVideoSettings":
            return {"baseWidth": 1920, "baseHeight": 1080}
        if op == "GetSceneItemId":
            return {"sceneItemId": 1}
        if op == "GetOutputSettings":
            return {"outputSettings": {"ndi_name": "STREAM (FAKED)"}}
        # CreateScene, SetCurrentProgramScene, SetCurrentPreviewScene, etc. → no-op {}
        return {}

    return fake_rpc, rpc_calls


def _make_args(target, ensure_source="", upstream="", test_preload=1):
    """Minimal argparse-namespace-like object for prod_scene(a)."""
    import types
    a = types.SimpleNamespace()
    a.host = "10.77.9.204"
    a.password = ""
    a.program_scene = target
    a.ensure_source = ensure_source
    a.upstream = upstream
    a.test_preload = test_preload
    return a


def _patch_helpers(monkeypatch):
    """Patch helpers that touch disk / the live program luma so the test is pure unit."""
    monkeypatch.setattr(obs_phase2, "_load_state", lambda: {})
    monkeypatch.setattr(obs_phase2, "_save_state", lambda state: None)
    monkeypatch.setattr(obs_phase2, "_force_test_preload",
                        lambda ws, host, upstream, tp, state: None)
    monkeypatch.setattr(obs_phase2, "_assert_program_nonblack",
                        lambda ws, host, scene, label, black_hint, min_mean=None: None)


# ---------------------------------------------------------------------------
# (a) skip SetCurrentProgramScene when already on the target scene
# ---------------------------------------------------------------------------

def test_no_switch_when_already_on_target_with_ensure_source(monkeypatch):
    """#343(a): stream box is already on REC-STRIH-TMP (curr_prog == target).
    SetCurrentProgramScene must NOT be called — no activation cost, source is warm."""
    target = "REC-STRIH-TMP"
    fake_rpc, calls = _make_fake_rpc(
        curr_prog=target, target_scene=target,
        ensure_source="NDI 2ME PGM",
        scenes=[target, "PRO"],
    )
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, pwd: FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    _patch_helpers(monkeypatch)

    obs_phase2.prod_scene(_make_args(target, ensure_source="NDI 2ME PGM"))

    scene_switches = [c for c in calls if c["op"] == "SetCurrentProgramScene"]
    assert scene_switches == [], (
        "SetCurrentProgramScene must NOT be called when the box is already on the target "
        "scene — the FIFO is warm and a redundant switch would trigger re-activation (#343(a))"
    )


def test_no_switch_when_already_on_target_without_ensure_source(monkeypatch):
    """#343(a): strih box is already on 'Cam 5' (curr_prog == target, no ensure-source).
    SetCurrentProgramScene must NOT be called either."""
    target = "Cam 5"
    fake_rpc, calls = _make_fake_rpc(
        curr_prog=target, target_scene=target,
        ensure_source="",
        scenes=[target, "PRO"],
    )
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, pwd: FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    _patch_helpers(monkeypatch)

    obs_phase2.prod_scene(_make_args(target, ensure_source=""))

    scene_switches = [c for c in calls if c["op"] == "SetCurrentProgramScene"]
    assert scene_switches == [], (
        "SetCurrentProgramScene must NOT be called when already on target (no-op switch, "
        "no benefit, avoid spurious activation risk) (#343(a))"
    )


# ---------------------------------------------------------------------------
# (b) no deadline when ensure-source and switch IS needed
# ---------------------------------------------------------------------------

def test_ensure_source_switch_uses_no_deadline(monkeypatch):
    """#343(b): stream box is on 'PRO', target is 'REC-STRIH-TMP' with --ensure-source.
    SetCurrentProgramScene must be called with timeout_s=0 (no #328 deadline) so the
    450 ms genlock FIFO on NDI 2ME PGM can complete without being cut off at 60 s."""
    curr_prog = "PRO"
    target = "REC-STRIH-TMP"
    fake_rpc, calls = _make_fake_rpc(
        curr_prog=curr_prog, target_scene=target,
        ensure_source="NDI 2ME PGM",
        scenes=[target, curr_prog],
    )
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, pwd: FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    _patch_helpers(monkeypatch)

    obs_phase2.prod_scene(_make_args(target, ensure_source="NDI 2ME PGM"))

    scene_switches = [c for c in calls if c["op"] == "SetCurrentProgramScene"]
    assert len(scene_switches) == 1, (
        f"expected exactly one SetCurrentProgramScene call, got {scene_switches}"
    )
    ts = scene_switches[0]["timeout_s"]
    assert ts is not None and ts <= 0, (
        f"SetCurrentProgramScene with --ensure-source must use timeout_s<=0 (no #328 "
        f"deadline) so the heavy FIFO init is not cut off at 60 s, got timeout_s={ts!r} "
        f"(#343(b))"
    )


# ---------------------------------------------------------------------------
# Regression: without --ensure-source, the normal bounded switch is preserved
# ---------------------------------------------------------------------------

def test_no_ensure_source_switch_uses_default_timeout(monkeypatch):
    """Without --ensure-source (strih/prod side), SetCurrentProgramScene must use the
    default timeout (None → OBS_OP_TIMEOUT_S) — the #328 deadline must stay active."""
    curr_prog = "PRO"
    target = "Cam 5"
    fake_rpc, calls = _make_fake_rpc(
        curr_prog=curr_prog, target_scene=target,
        ensure_source="",
        scenes=[target, curr_prog],
    )
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, pwd: FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    _patch_helpers(monkeypatch)

    obs_phase2.prod_scene(_make_args(target, ensure_source=""))

    scene_switches = [c for c in calls if c["op"] == "SetCurrentProgramScene"]
    assert len(scene_switches) == 1, (
        f"expected exactly one SetCurrentProgramScene call, got {scene_switches}"
    )
    ts = scene_switches[0]["timeout_s"]
    assert ts is None, (
        f"SetCurrentProgramScene WITHOUT --ensure-source must use timeout_s=None (the "
        f"default #328 deadline), got {ts!r} — the bounded protection must be preserved"
    )


# ---------------------------------------------------------------------------
# #343 teardown half — a same-scene restore must be SKIPPED.
#
# Live-confirmed on the stream box (2026-06-30): SetCurrentProgramScene to the scene ALREADY on
# program HANGS (>60 s, no ws response) when that scene carries the heavy NDI 2ME PGM source
# mid-renegotiation (off-air) — the #328 flood. prod_scene already skips the switch when already
# on target (tests above), but teardown restored prev_scene UNCONDITIONALLY, so it re-triggered
# the same hang at run end (the "same block recurs in teardown" the issue warned about). The fix:
# teardown reads the current program/preview and only switches when it actually differs from the
# restore target.
# ---------------------------------------------------------------------------

def _make_teardown_args():
    import types
    a = types.SimpleNamespace()
    a.host = "10.77.9.204"
    a.password = ""
    return a


def _make_teardown_rpc(curr_prog, curr_preview, studio=True):
    """fake_rpc for teardown: program/preview reads return the given current scenes; all
    setters/clears are recorded no-ops."""
    rpc_calls = []

    def fake_rpc(ws, op, payload=None, ignore_err=False, timeout_s=None):
        rpc_calls.append({"op": op, "payload": payload or {}, "timeout_s": timeout_s})
        if op == "GetCurrentProgramScene":
            return {"currentProgramSceneName": curr_prog}
        if op == "GetCurrentPreviewScene":
            return {"currentPreviewSceneName": curr_preview}
        if op == "GetStudioModeEnabled":
            return {"studioModeEnabled": studio}
        return {}

    return fake_rpc, rpc_calls


def _patch_teardown_helpers(monkeypatch, prev_scene, prev_preview):
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, pwd: FakeWS())
    monkeypatch.setattr(
        obs_phase2, "_load_state",
        lambda: {"10.77.9.204": {"prev_scene": prev_scene, "prev_preview": prev_preview}},
    )
    monkeypatch.setattr(obs_phase2, "_save_state", lambda s: None)
    monkeypatch.setattr(obs_phase2, "_restore_test_preload", lambda ws, host, state: None)


def test_teardown_skips_program_switch_when_already_on_prev(monkeypatch):
    """#343 teardown: program already on prev_scene → SetCurrentProgramScene must NOT be called
    (a same-scene switch hangs on the heavy NDI 2ME PGM, the teardown half of the proof hang)."""
    fake_rpc, calls = _make_teardown_rpc(curr_prog="PRO", curr_preview="PRO")
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    _patch_teardown_helpers(monkeypatch, prev_scene="PRO", prev_preview="PRO")

    obs_phase2.teardown(_make_teardown_args())

    prog = [c for c in calls if c["op"] == "SetCurrentProgramScene"]
    assert prog == [], (
        "#343 teardown: SetCurrentProgramScene must NOT be called when program is already on "
        "prev_scene — a same-scene switch HANGS on the heavy NDI 2ME PGM (no ws response, #328)."
    )
    prev = [c for c in calls if c["op"] == "SetCurrentPreviewScene"]
    assert prev == [], (
        "#343 teardown: SetCurrentPreviewScene must NOT be called when preview is already on "
        "prev_preview (same heavy-source hang risk)."
    )


def test_teardown_switches_program_when_different(monkeypatch):
    """Regression: when program is NOT on prev_scene, teardown MUST restore it (the legit
    restore path is preserved — only the redundant same-scene switch is skipped)."""
    fake_rpc, calls = _make_teardown_rpc(curr_prog="REC-LEFTOVER", curr_preview="REC-LEFTOVER")
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    _patch_teardown_helpers(monkeypatch, prev_scene="PRO", prev_preview="PRO")

    obs_phase2.teardown(_make_teardown_args())

    prog = [c for c in calls if c["op"] == "SetCurrentProgramScene"]
    assert len(prog) == 1 and prog[0]["payload"].get("sceneName") == "PRO", (
        f"#343 teardown: when program differs from prev_scene it MUST be restored to 'PRO'; "
        f"got {prog}"
    )
