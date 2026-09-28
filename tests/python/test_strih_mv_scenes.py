"""#730 / #758 — unit tests for strih_mv_scenes.py: the stats delta, the sender-bounce reattach,
and (issue 1242) that the removed low-bandwidth twin seed can never run again. No live OBS
connection — mirrors the existing tests/python/test_obs_phase2_*.py style (importlib module load,
fakes for the WS calls).
"""
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "strih_mv_scenes.py"
_spec = importlib.util.spec_from_file_location("strih_mv_scenes", _MOD_PATH)
strih_mv_scenes = importlib.util.module_from_spec(_spec)
sys.modules["strih_mv_scenes"] = strih_mv_scenes
_spec.loader.exec_module(strih_mv_scenes)


# --- stats_delta ----------------------------------------------------------------------------

def test_stats_delta_computes_render_and_output_deltas():
    before = {
        "renderSkippedFrames": 100, "renderTotalFrames": 10000,
        "outputSkippedFrames": 5, "outputTotalFrames": 9000,
        "activeFps": 30.0, "averageFrameRenderTime": 5.0,
    }
    after = {
        "renderSkippedFrames": 130, "renderTotalFrames": 10600,
        "outputSkippedFrames": 5, "outputTotalFrames": 9600,
        "activeFps": 30.0, "averageFrameRenderTime": 4.2,
    }
    d = strih_mv_scenes.stats_delta(before, after)
    assert d["renderSkipped_delta"] == 30
    assert d["renderTotal_delta"] == 600
    assert d["renderSkip_pct"] == 5.0
    assert d["outputSkipped_delta"] == 0
    assert d["outputTotal_delta"] == 600
    assert d["averageFrameRenderTime"] == 4.2


def test_stats_delta_zero_total_frames_never_divides_by_zero():
    same = {
        "renderSkippedFrames": 0, "renderTotalFrames": 0,
        "outputSkippedFrames": 0, "outputTotalFrames": 0,
        "activeFps": 0.0, "averageFrameRenderTime": 0.0,
    }
    d = strih_mv_scenes.stats_delta(same, same)
    assert d["renderSkip_pct"] == 0.0


# --- module wiring (reuses obs_phase2's ONE ws client — never a 4th one, #650 convention) ------

def test_reuses_obs_phase2_conn_and_rpc():
    assert strih_mv_scenes.op.__name__ == "obs_phase2"
    assert callable(strih_mv_scenes.op._conn)
    assert callable(strih_mv_scenes.op._rpc)


def test_main_stats_and_reattach_functions_exist():
    assert callable(strih_mv_scenes.measure_stats)
    assert callable(strih_mv_scenes.reattach)


def test_the_low_bandwidth_twin_seed_is_gone_and_a_bare_run_never_connects(monkeypatch):
    # issue 1242 (28.9.2026, owner order): strih has no low-bandwidth multiview twins -- the old
    # no-flag seed of per-camera twin scenes + low-bandwidth inputs must not exist, and a run
    # without --reattach / --stats is a usage error BEFORE any WebSocket connect.
    for gone in ("seed", "rewire_multiview_scene", "mv_replacement_plan", "mv_scene_name"):
        assert not hasattr(strih_mv_scenes, gone), gone
    monkeypatch.setattr(strih_mv_scenes.op, "_conn",
                        lambda *a, **k: pytest.fail("a bare run must never connect"))
    monkeypatch.setattr(sys, "argv", ["strih_mv_scenes.py", "--host", "10.77.9.202"])
    with pytest.raises(SystemExit) as e:
        strih_mv_scenes.main()
    assert e.value.code == 2


# --- #758 item 2 — reattach(): re-applies the camera input's OWN current ndi_source_name ------


class _FakeObsRpc:
    """Minimal fake standing in for the live obs-websocket connection: records every _rpc call
    and returns a scripted response per request type — mirrors this file's own "no live OBS
    connection" convention (the pure-logic functions are unit-tested; the thin live-WS wrapper
    around them is proven here with a fake instead of a real socket, same spirit as
    tests/python/test_obs_phase2_*.py's own fakes)."""

    def __init__(self, get_settings_response):
        self.calls = []
        self._get_settings_response = get_settings_response

    def rpc(self, _obs, rtype, rdata=None, ignore_err=False):
        self.calls.append((rtype, rdata))
        if rtype == "GetInputSettings":
            return self._get_settings_response
        if rtype == "SetInputSettings":
            return {}
        raise AssertionError(f"unexpected rpc call: {rtype}")


def test_reattach_reapplies_the_inputs_own_current_ndi_source_name(monkeypatch):
    # #761: reattach() targets the MAIN "NDI camN" input -- the one the sender-bounce probe checks.
    # #795/#759: reattach() now ALSO consults the DistroAV finder list (op._ndi_source_list) before
    # re-applying, and only sets once it is non-empty — so the recorded rpc sequence is now
    # GetInputSettings -> GetInputPropertiesListPropertyItems -> SetInputSettings.
    fake = _FakeObsRpcWithFinder(
        {"inputSettings": {"ndi_source_name": "CAM3 (usb)"}},
        finder_items=[{"itemValue": "CAM3 (usb)"}],
    )
    monkeypatch.setattr(strih_mv_scenes.op, "_rpc", fake.rpc)

    result = strih_mv_scenes.reattach(object(), 5)

    assert result == "CAM3 (usb)"
    assert fake.calls[0] == ("GetInputSettings", {"inputName": "NDI cam5"})
    assert (
        "GetInputPropertiesListPropertyItems",
        {"inputName": "NDI cam5", "propertyName": "ndi_source_name"},
    ) in fake.calls
    assert fake.calls[-1] == (
        "SetInputSettings",
        {"inputName": "NDI cam5", "inputSettings": {"ndi_source_name": "CAM3 (usb)"}},
    )


def test_reattach_returns_none_when_the_input_has_no_ndi_source_name(monkeypatch):
    fake = _FakeObsRpc({"inputSettings": {}})
    monkeypatch.setattr(strih_mv_scenes.op, "_rpc", fake.rpc)

    result = strih_mv_scenes.reattach(object(), 3)

    assert result is None
    # Never re-applies a fabricated/fallback source name -- must call GetInputSettings only.
    assert fake.calls == [("GetInputSettings", {"inputName": "NDI cam3"})]


# --- #795/#759: reattach() must GUARD the SetInputSettings against an EMPTY DistroAV finder list --


class _FakeObsRpcWithFinder:
    """#759/#795: a fake that ALSO answers GetInputPropertiesListPropertyItems — the DistroAV
    finder-list read reattach() now consults before re-applying ndi_source_name (via
    obs_phase2._ndi_source_list). Lets the empty-finder-list guard be exercised without a live OBS,
    same spirit as _FakeObsRpc above."""

    def __init__(self, get_settings_response, finder_items):
        self.calls = []
        self._get_settings_response = get_settings_response
        self._finder_items = finder_items

    def rpc(self, _obs, rtype, rdata=None, ignore_err=False):
        self.calls.append((rtype, rdata))
        if rtype == "GetInputSettings":
            return self._get_settings_response
        if rtype == "GetInputPropertiesListPropertyItems":
            return {"propertyItems": self._finder_items}
        if rtype == "SetInputSettings":
            return {}
        raise AssertionError(f"unexpected rpc call: {rtype}")


def test_reattach_skips_the_set_when_the_finder_list_is_empty(monkeypatch):
    # #795 (event review 2026-07-18): re-applying ndi_source_name via SetInputSettings against an
    # EMPTY DistroAV finder list MANGLES the value (OBS drops a name absent from the combo's live
    # item list). reattach() must therefore SKIP the SetInputSettings entirely when the finder list
    # stays empty, leave the input bound as-is, and return the NDI_SOURCE_NOT_DISCOVERABLE sentinel —
    # so the caller (especially the WARN-only #759 cleanup path, which never fails the run loud) can
    # never silently point a camera leg at garbage.
    fake = _FakeObsRpcWithFinder(
        {"inputSettings": {"ndi_source_name": "CAM3 (usb)"}}, finder_items=[]
    )
    monkeypatch.setattr(strih_mv_scenes.op, "_rpc", fake.rpc)

    result = strih_mv_scenes.reattach(
        object(), 3, finder_retries=3, finder_wait_s=0, sleep=lambda *_a, **_k: None
    )

    assert result is strih_mv_scenes.NDI_SOURCE_NOT_DISCOVERABLE
    set_calls = [c for c in fake.calls if c[0] == "SetInputSettings"]
    assert set_calls == [], (
        "#795: reattach must NOT SetInputSettings against an empty finder list (would mangle the "
        f"name); got {set_calls}"
    )


def test_reattach_skips_the_set_when_the_bound_source_is_absent_from_a_non_empty_list(monkeypatch):
    # #795 review refinement: the mangle happens whenever the bound name is ABSENT from the combo's
    # item list — NOT only when the list is empty. A non-empty finder list that offers OTHER sources
    # but not THIS input's bound "CAM3 (usb)" must ALSO skip the set (the sender is still bouncing),
    # never re-apply a name the combo can't resolve.
    fake = _FakeObsRpcWithFinder(
        {"inputSettings": {"ndi_source_name": "CAM3 (usb)"}},
        finder_items=[{"itemValue": "CAM1 (usb)"}, {"itemValue": "CAM2 (usb)"}],
    )
    monkeypatch.setattr(strih_mv_scenes.op, "_rpc", fake.rpc)

    result = strih_mv_scenes.reattach(
        object(), 3, finder_retries=3, finder_wait_s=0, sleep=lambda *_a, **_k: None
    )

    assert result is strih_mv_scenes.NDI_SOURCE_NOT_DISCOVERABLE
    set_calls = [c for c in fake.calls if c[0] == "SetInputSettings"]
    assert set_calls == [], (
        "#795: reattach must NOT re-apply a bound source absent from a non-empty finder list "
        f"(would still mangle it); got {set_calls}"
    )


def test_reattach_sets_once_the_finder_list_is_non_empty(monkeypatch):
    # The happy path with a populated finder list nudges the input's own current name — the #795
    # guard must only skip the EMPTY case, never the normal reconnect nudge.
    # issue 1114: the nudge is now a CLEAR-then-SET (was a single same-name re-apply, which is a
    # no-op for the receiver — see test_reattach_clears_name_then_resets_to_force_a_fresh_receiver_1114
    # and reattach()'s docstring). The finder-list guard still applies to the SET-back only.
    fake = _FakeObsRpcWithFinder(
        {"inputSettings": {"ndi_source_name": "CAM3 (usb)"}},
        finder_items=[{"itemValue": "CAM3 (usb)"}, {"itemValue": "CAM1 (usb)"}],
    )
    monkeypatch.setattr(strih_mv_scenes.op, "_rpc", fake.rpc)

    result = strih_mv_scenes.reattach(
        object(), 3, finder_retries=3, finder_wait_s=0, sleep=lambda *_a, **_k: None
    )

    assert result == "CAM3 (usb)"
    set_calls = [c for c in fake.calls if c[0] == "SetInputSettings"]
    assert set_calls == [
        (
            "SetInputSettings",
            {"inputName": "NDI cam3", "inputSettings": {"ndi_source_name": ""}},
        ),
        (
            "SetInputSettings",
            {"inputName": "NDI cam3", "inputSettings": {"ndi_source_name": "CAM3 (usb)"}},
        ),
    ]


def test_reattach_clears_name_then_resets_to_force_a_fresh_receiver_1114(monkeypatch):
    # issue 1114 (E2E burn-deploy handover race): re-applying the SAME ndi_source_name via
    # SetInputSettings is a NO-OP for the receiver -- vendored ndi_source_update() computes
    # reset_ndi_receiver from a NAME CHANGE (safe_strcmp(config.ndi_source_name, new) != 0), so an
    # unchanged name leaves reset_ndi_receiver=false and (the receiver thread being alive after the
    # issue-1096 retry-in-place) the update does nothing. The receiver stays stuck on the DEAD
    # pre-bounce sender until the passive ~2min fresh-finder timer, which the [2/8] ~52s budget
    # never covers -> false "camera leg dead" + the heavy #1093 strih-OBS force-kill.
    #
    # The cure is a CLEAR-then-SET: first SetInputSettings {ndi_source_name: ""} (=>
    # ndi_source_thread_stop: the stuck receiver is torn down cleanly, s->running=false), THEN
    # SetInputSettings {ndi_source_name: X} (=> ndi_source_thread_start: a FRESH receiver thread
    # whose reset_ndi_receiver=true runs the issue-1096 fresh finder and resolves the live
    # post-bounce sender by URL). So the recorded SetInputSettings sequence must be exactly
    # ["" , X] -- the targeted per-input equivalent of the OBS force-kill, without killing OBS.
    fake = _FakeObsRpcWithFinder(
        {"inputSettings": {"ndi_source_name": "CAM3 (usb)"}},
        finder_items=[{"itemValue": "CAM3 (usb)"}, {"itemValue": "CAM1 (usb)"}],
    )
    monkeypatch.setattr(strih_mv_scenes.op, "_rpc", fake.rpc)

    result = strih_mv_scenes.reattach(
        object(), 3, finder_retries=3, finder_wait_s=0, sleep=lambda *_a, **_k: None
    )

    assert result == "CAM3 (usb)"
    set_calls = [c for c in fake.calls if c[0] == "SetInputSettings"]
    assert set_calls == [
        (
            "SetInputSettings",
            {"inputName": "NDI cam3", "inputSettings": {"ndi_source_name": ""}},
        ),
        (
            "SetInputSettings",
            {"inputName": "NDI cam3", "inputSettings": {"ndi_source_name": "CAM3 (usb)"}},
        ),
    ], (
        "issue 1114: reattach must CLEAR the name to '' (forces ndi_source_thread_stop) then SET it "
        f"back to the real name (forces a fresh ndi_source_thread_start) -- got {set_calls}"
    )


class _FakeObsRpcWithChangingFinder:
    """issue 1114: a fake whose finder list CHANGES across successive
    GetInputPropertiesListPropertyItems calls (a scripted queue) — so the mangle-window re-check
    (source present at the up-front guard, then vanished right before the set-back) can be exercised
    without a live OBS."""

    def __init__(self, get_settings_response, finder_items_sequence):
        self.calls = []
        self._get_settings_response = get_settings_response
        self._finder_items_sequence = list(finder_items_sequence)
        self._finder_idx = 0

    def rpc(self, _obs, rtype, rdata=None, ignore_err=False):
        self.calls.append((rtype, rdata))
        if rtype == "GetInputSettings":
            return self._get_settings_response
        if rtype == "GetInputPropertiesListPropertyItems":
            idx = min(self._finder_idx, len(self._finder_items_sequence) - 1)
            self._finder_idx += 1
            return {"propertyItems": self._finder_items_sequence[idx]}
        if rtype == "SetInputSettings":
            return {}
        raise AssertionError(f"unexpected rpc call: {rtype}")


def test_reattach_skips_setback_if_source_vanishes_during_the_clear_settle_1114(monkeypatch):
    # issue 1114 review (#795 mangle window): the CLEAR + settle widened the window between the
    # up-front finder-list guard and the SET-back. If the sender drops out of the finder list DURING
    # that window, re-applying its name via SetInputSettings would MANGLE it — so reattach re-checks
    # right before the set-back and SKIPS the same-name set-back on a vanish.
    #
    # issue 1197 (smoking gun, gh run 32743557703): but the input must NEVER be left cleared to "" —
    # an empty ndi_source_name STOPS the DistroAV receiver thread (a permanent wedge the in-loop
    # #767/#1096 watchdogs can never revive). When the #399 baseline is ALSO offline (here the fake's
    # finder never re-lists CAM3 (usb)), reattach RESTORES the original bound name so the receiver
    # thread restarts and the input ends exactly as it started — the recorded SetInputSettings are the
    # CLEAR then the RESTORE, never a bare clear-to-empty. It still returns NDI_SOURCE_NOT_DISCOVERABLE
    # (it could not re-lock); the caller's bounded finder-warm poll re-enforces the baseline later.
    fake = _FakeObsRpcWithChangingFinder(
        {"inputSettings": {"ndi_source_name": "CAM3 (usb)"}},
        # 1st call (up-front guard): present. 2nd (pre-set-back re-check): vanished. 3rd (the #1158
        # baseline discoverability check inside reenforce_ndi_name): baseline also absent.
        finder_items_sequence=[
            [{"itemValue": "CAM3 (usb)"}, {"itemValue": "CAM1 (usb)"}],
            [{"itemValue": "CAM1 (usb)"}],
            [{"itemValue": "CAM1 (usb)"}],
        ],
    )
    monkeypatch.setattr(strih_mv_scenes.op, "_rpc", fake.rpc)

    result = strih_mv_scenes.reattach(
        object(), 3, finder_retries=3, finder_wait_s=0, sleep=lambda *_a, **_k: None
    )

    assert result is strih_mv_scenes.NDI_SOURCE_NOT_DISCOVERABLE
    set_calls = [c for c in fake.calls if c[0] == "SetInputSettings"]
    assert set_calls == [
        (
            "SetInputSettings",
            {"inputName": "NDI cam3", "inputSettings": {"ndi_source_name": ""}},
        ),
        (
            "SetInputSettings",
            {"inputName": "NDI cam3", "inputSettings": {"ndi_source_name": "CAM3 (usb)"}},
        ),
    ], (
        "issue 1197: on a mid-reattach vanish with the baseline also offline, the input must be "
        f"RESTORED to its original name (never left empty — a stopped-thread wedge); got {set_calls}"
    )
