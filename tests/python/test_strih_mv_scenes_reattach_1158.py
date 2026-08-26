"""#1158 — strih_mv_scenes.reattach()'s vanished-branch must NO LONGER leave the input cleared to ""
(a permanent wedge: an empty ndi_source_name stops the DistroAV receiver thread, which the in-loop
#767/#1096 watchdogs can never revive). When the bound sender vanishes during the clear-settle, it
re-enforces the CANONICAL #399 baseline sender (via the shared, read-back-verified
obs_phase2.reenforce_ndi_name) when the baseline IS discoverable; only if the baseline is ALSO
offline does it leave "" (and SCREAM #1158).
"""
import importlib.util
import pathlib
import sys

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "strih_mv_scenes.py"
_spec = importlib.util.spec_from_file_location("strih_mv_scenes_1158", _MOD_PATH)
strih_mv_scenes = importlib.util.module_from_spec(_spec)
sys.modules["strih_mv_scenes_1158"] = strih_mv_scenes
_spec.loader.exec_module(strih_mv_scenes)


class _FakeObs:
    """Tracks the input's current ndi_source_name (set/read) plus a SCRIPTED per-call finder-list
    queue (so the up-front guard can see the bound name present, then the set-back re-check can see
    it vanished, then reenforce_ndi_name's own check can see the baseline present/absent)."""

    def __init__(self, current, finder_queue):
        self.current = current
        self.finder_queue = [list(x) for x in finder_queue]
        self._i = 0
        self.set_calls = []

    def rpc(self, _obs, rtype, rdata=None, ignore_err=False):
        if rtype == "GetInputSettings":
            return {"inputSettings": {"ndi_source_name": self.current}}
        if rtype == "GetInputPropertiesListPropertyItems":
            idx = min(self._i, len(self.finder_queue) - 1)
            self._i += 1
            return {"propertyItems": [{"itemValue": v} for v in self.finder_queue[idx]]}
        if rtype == "SetInputSettings":
            self.set_calls.append(rdata["inputSettings"]["ndi_source_name"])
            self.current = rdata["inputSettings"]["ndi_source_name"]
            return {}
        raise AssertionError(f"unexpected rpc: {rtype}")


def _wire(monkeypatch, fake, baseline):
    monkeypatch.setattr(strih_mv_scenes.op, "_rpc", fake.rpc)
    monkeypatch.setattr(strih_mv_scenes, "_baseline_sender_for", lambda _inp: baseline)


def test_reattach_heals_to_baseline_when_bound_vanishes_but_baseline_discoverable(monkeypatch):
    # bound 'CAM3 (30p)' present up-front, vanished at set-back re-check; baseline 'CAM3 (usb)' IS
    # discoverable -> re-enforce the baseline (NOT the stale bound name, NOT "").
    fake = _FakeObs(
        current="CAM3 (30p)",
        finder_queue=[
            ["CAM3 (30p)", "CAM3 (usb)"],  # up-front guard: bound present
            ["CAM3 (usb)"],                # set-back re-check: bound VANISHED
            ["CAM3 (usb)"],                # reenforce_ndi_name discoverability: baseline present
        ],
    )
    _wire(monkeypatch, fake, baseline="CAM3 (usb)")
    result = strih_mv_scenes.reattach(
        object(), 3, finder_retries=3, finder_wait_s=0, sleep=lambda *_a, **_k: None)
    assert result == "CAM3 (usb)"
    # clear to "" then set the BASELINE (never left empty, never re-applied the stale bound name)
    assert fake.set_calls == ["", "CAM3 (usb)"]
    assert fake.current == "CAM3 (usb)"


def test_reattach_restores_original_when_baseline_also_offline_never_empty(monkeypatch):
    # issue 1197 (smoking gun, gh run 32743557703): bound present up-front (possibly a STALE finder
    # listing), then EVERYTHING (bound AND baseline) gone at set-back -> the CLEAR already stopped the
    # receiver thread. Leaving "" here is the self-inflicted PERMANENT wedge. The reattach must instead
    # RESTORE the original bound name so the receiver thread RESTARTS and the input ends exactly as it
    # started (never in the stopped-thread empty state). Still returns NDI_SOURCE_NOT_DISCOVERABLE (it
    # could not re-lock) — the caller's bounded finder-warm poll re-enforces the baseline later.
    fake = _FakeObs(
        current="CAM3 (usb)",
        finder_queue=[
            ["CAM3 (usb)", "CAM1 (usb)"],  # up-front guard: present
            ["CAM1 (usb)"],                # set-back re-check: vanished
            ["CAM1 (usb)"],                # reenforce discoverability: baseline also absent
        ],
    )
    _wire(monkeypatch, fake, baseline="CAM3 (usb)")
    result = strih_mv_scenes.reattach(
        object(), 3, finder_retries=3, finder_wait_s=0, sleep=lambda *_a, **_k: None)
    assert result is strih_mv_scenes.NDI_SOURCE_NOT_DISCOVERABLE
    # cleared, then the ORIGINAL is RESTORED (never left empty) — the #1197 stopped-thread-wedge fix
    assert fake.set_calls == ["", "CAM3 (usb)"]
    assert fake.current == "CAM3 (usb)"


def test_reattach_never_clears_when_bound_name_never_discoverable(monkeypatch):
    # issue 1197 priority 1 / #795: when the bound name is NEVER in the finder (absent through the whole
    # up-front pre-check), the CLEAR must NOT fire at all — the input's ORIGINAL binding is left
    # untouched (never cleared into the stopped-thread state) and the caller waits for rediscovery.
    fake = _FakeObs(
        current="CAM3 (usb)",
        finder_queue=[
            ["CAM1 (usb)"],  # pre-check attempt 1: bound absent
            ["CAM1 (usb)"],  # pre-check attempt 2: still absent
            ["CAM1 (usb)"],  # pre-check attempt 3: still absent
        ],
    )
    _wire(monkeypatch, fake, baseline="CAM3 (usb)")
    result = strih_mv_scenes.reattach(
        object(), 3, finder_retries=3, finder_wait_s=0, sleep=lambda *_a, **_k: None)
    assert result is strih_mv_scenes.NDI_SOURCE_NOT_DISCOVERABLE
    assert fake.set_calls == []          # the CLEAR never fired — original binding untouched
    assert fake.current == "CAM3 (usb)"  # left exactly as it started


def test_reattach_happy_path_reapplies_bound_name_unchanged(monkeypatch):
    # The normal reconnect nudge (bound name still discoverable at set-back) is UNCHANGED by #1158.
    fake = _FakeObs(
        current="CAM3 (usb)",
        finder_queue=[
            ["CAM3 (usb)", "CAM1 (usb)"],  # up-front guard
            ["CAM3 (usb)", "CAM1 (usb)"],  # set-back re-check: still present
        ],
    )
    _wire(monkeypatch, fake, baseline="CAM3 (usb)")
    result = strih_mv_scenes.reattach(
        object(), 3, finder_retries=3, finder_wait_s=0, sleep=lambda *_a, **_k: None)
    assert result == "CAM3 (usb)"
    assert fake.set_calls == ["", "CAM3 (usb)"]  # CLEAR-then-SET of the bound name (issue 1114)
