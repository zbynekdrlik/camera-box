"""#769 -- imag projector openers must be idempotent (count-first).

obs-websocket's OpenVideoMixProjector ALWAYS opens a NEW window (no "is it open" query), and OBS's
own CloseExistingProjectors replace-loop only closes projectors whose internal GetMonitor()==target,
so a launch-restore stray survives and every blind seed stacks one more (live "3x Multiview, gate
refuse", 2026-07-15). `imag_scenes.py::projector()` (the SEEDER, run on every boot via
imag-obs-start.sh AND every watchdog tier-a relaunch via imag-obs-watchdog.py) must, after opening,
close the OLDER stray windows -- keeping the NEWEST (== the one it just opened on the correct
monitor) -- so the stack never forms on the LIVE box between gate runs. The watchdog inherits this
for free (it calls the same seed). The gate preflight [0/8] already heals post-hoc
(scripts/lib/imag-projector-heal.sh, merged #769).

Covers, with NO live OBS/rig/X (mirrors test_imag_scenes_clear_burns_866.py's fake-ws + importlib
load convention; the pure decision is offline-testable against a captured `wmctrl -l` fixture, the
#791 pure-seam pattern):
  a. projector_window_ids() -- pure: ids of every `Projector - <kind>` window in a wmctrl dump.
  b. projector_strays_to_close() -- pure: ids to close so exactly one survives (all but the highest
     numeric X id); [] when 0 or 1 present -> proves idempotence (a 2nd run is a no-op).
  c. _heal_projector_strays() -- closes exactly the strays over local wmctrl; a missing wmctrl warns
     LOUD by name and NEVER raises (runs under imag-obs-start.sh set -euo pipefail) and NEVER reads a
     missing tool as "no windows" (imag-ssh-remote-tool-preflight rule).
  d. projector() -- opens both projectors then converges to exactly 1+1, idempotent across runs.
"""
import importlib.util
import pathlib
import sys

REPO = pathlib.Path(__file__).resolve().parents[2]


def _load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


def _scenes_module():
    return _load(REPO / "scripts" / "imag_scenes.py", "imag_scenes_projector_769_under_test")


# A realistic `wmctrl -l` dump: id, desktop, host, title.
_CLEAN = (
    "0x01a00003  0 imag-nb Ubuntu\n"
    "0x00c00006  0 imag-nb OBS Studio 32.1.2 - newlevel.media build unknown - Profile: imag-60fps\n"
    "0x00c0c883  0 imag-nb Projector - Multiview\n"
    "0x00c0c88a  0 imag-nb Projector - Program\n"
)
# 3 Multiview strays (the live incident) + 1 Program.
_STACKED = (
    "0x01a00003  0 imag-nb Ubuntu\n"
    "0x00c00006  0 imag-nb OBS Studio 32.1.2\n"
    "0x00c0c101  0 imag-nb Projector - Multiview\n"
    "0x00c0c202  0 imag-nb Projector - Multiview\n"
    "0x00c0cfff  0 imag-nb Projector - Multiview\n"
    "0x00c0c88a  0 imag-nb Projector - Program\n"
)


# ---------------------------------------------------------------------------
# a. projector_window_ids -- pure
# ---------------------------------------------------------------------------

def test_window_ids_finds_matching_kind_in_order():
    mod = _scenes_module()
    assert mod.projector_window_ids(_STACKED, "Multiview") == [
        "0x00c0c101", "0x00c0c202", "0x00c0cfff"]
    assert mod.projector_window_ids(_STACKED, "Program") == ["0x00c0c88a"]


def test_window_ids_does_not_cross_contaminate_kinds():
    mod = _scenes_module()
    # a Program search must never pick up Multiview windows and vice-versa
    assert mod.projector_window_ids(_CLEAN, "Program") == ["0x00c0c88a"]
    assert mod.projector_window_ids(_CLEAN, "Multiview") == ["0x00c0c883"]


def test_window_ids_empty_and_malformed():
    mod = _scenes_module()
    assert mod.projector_window_ids("", "Multiview") == []
    assert mod.projector_window_ids(None, "Multiview") == []
    # a line mentioning the marker but with no leading 0x window id is skipped
    assert mod.projector_window_ids("not-an-id Projector - Multiview\n", "Multiview") == []


# ---------------------------------------------------------------------------
# b. projector_strays_to_close -- pure decision (keep newest = highest id)
# ---------------------------------------------------------------------------

def test_strays_close_all_but_highest_id():
    mod = _scenes_module()
    # keeps 0x00c0cfff (highest), closes the two lower ones
    assert sorted(mod.projector_strays_to_close(_STACKED, "Multiview")) == [
        "0x00c0c101", "0x00c0c202"]


def test_strays_empty_when_zero_or_one():
    mod = _scenes_module()
    assert mod.projector_strays_to_close(_CLEAN, "Multiview") == []   # exactly 1 -> nothing
    assert mod.projector_strays_to_close(_CLEAN, "Program") == []
    assert mod.projector_strays_to_close("", "Multiview") == []       # 0 -> nothing


def test_strays_survivor_chosen_numerically_not_lexicographically():
    mod = _scenes_module()
    # 0x9 < 0x10 numerically but ">" lexicographically -- must keep 0x10 (the newer window)
    dump = ("0x00000009  0 h Projector - Program\n"
            "0x00000010  0 h Projector - Program\n")
    assert mod.projector_strays_to_close(dump, "Program") == ["0x00000009"]


def test_idempotence_second_pass_is_noop():
    mod = _scenes_module()
    # After a heal leaves exactly 1, a repeated seed must close nothing (the acceptance criterion:
    # 2x seed + relaunch = still exactly 1+1).
    survivor = max(mod.projector_window_ids(_STACKED, "Multiview"), key=lambda x: int(x, 16))
    healed = ("0x01a00003  0 imag-nb Ubuntu\n"
              "%s  0 imag-nb Projector - Multiview\n"
              "0x00c0c88a  0 imag-nb Projector - Program\n" % survivor)
    assert mod.projector_strays_to_close(healed, "Multiview") == []
    assert mod.projector_strays_to_close(healed, "Program") == []


# ---------------------------------------------------------------------------
# c. _heal_projector_strays -- glue, monkeypatched local wmctrl
# ---------------------------------------------------------------------------

def test_heal_closes_exactly_the_strays_over_local_wmctrl(monkeypatch):
    mod = _scenes_module()
    closed = []
    monkeypatch.setattr(mod, "_wmctrl_list_local", lambda: _STACKED)
    monkeypatch.setattr(mod, "_wmctrl_close_local", lambda win_id: closed.append(win_id))
    monkeypatch.setattr(mod.time, "sleep", lambda *_a, **_k: None)
    mod._heal_projector_strays("127.0.0.1", ["Program", "Multiview"])
    # only the two OLDER Multiview strays closed; the 1 Program + the newest Multiview kept
    assert sorted(closed) == ["0x00c0c101", "0x00c0c202"]


def test_heal_missing_wmctrl_warns_by_name_and_never_raises(monkeypatch, capsys):
    mod = _scenes_module()
    closed = []
    # a missing tool -> list returns None -> warn LOUD by name, skip dedup, NEVER read as "no windows"
    monkeypatch.setattr(mod, "_wmctrl_list_local", lambda: None)
    monkeypatch.setattr(mod, "_wmctrl_close_local", lambda win_id: closed.append(win_id))
    monkeypatch.setattr(mod.time, "sleep", lambda *_a, **_k: None)
    mod._heal_projector_strays("127.0.0.1", ["Program", "Multiview"])  # must NOT raise
    assert closed == []
    out = capsys.readouterr().out
    assert "wmctrl" in out and "#769" in out


def test_heal_noop_when_already_one_each(monkeypatch):
    mod = _scenes_module()
    closed = []
    monkeypatch.setattr(mod, "_wmctrl_list_local", lambda: _CLEAN)
    monkeypatch.setattr(mod, "_wmctrl_close_local", lambda win_id: closed.append(win_id))
    monkeypatch.setattr(mod.time, "sleep", lambda *_a, **_k: None)
    mod._heal_projector_strays("127.0.0.1", ["Program", "Multiview"])
    assert closed == []


# ---------------------------------------------------------------------------
# d. projector() -- full flow: open both, then converge to exactly 1+1
# ---------------------------------------------------------------------------

class FakeObs:
    """Two monitors: an HDMI projector + a non-HDMI panel. Records OpenVideoMixProjector calls."""

    def __init__(self):
        self.opens = []

    def req(self, rtype, payload=None, ignore_err=False):
        if rtype == "GetMonitorList":
            return {"monitors": [
                {"monitorIndex": 0, "monitorName": "DP-0(0)"},      # panel -> Multiview
                {"monitorIndex": 1, "monitorName": "HDMI-0(1)"},    # projector -> Program
            ]}
        if rtype == "OpenVideoMixProjector":
            self.opens.append((payload["videoMixType"], payload["monitorIndex"]))
        return {}


def test_projector_opens_both_then_dedups(monkeypatch):
    mod = _scenes_module()
    obs = FakeObs()
    closed = []
    # issue 1152 M4 hermeticity: pin the drm-output config DORMANT so this test never depends on
    # a stray ~/.camera-box/drm-output.json on the test machine (raising=False keeps it a no-op
    # on a pre-1152 module).
    monkeypatch.setattr(mod, "_drm_output_config_text", lambda host: "", raising=False)
    monkeypatch.setattr(mod, "_wmctrl_list_local", lambda: _STACKED)
    monkeypatch.setattr(mod, "_wmctrl_close_local", lambda win_id: closed.append(win_id))
    monkeypatch.setattr(mod.time, "sleep", lambda *_a, **_k: None)
    mod.projector(obs, "127.0.0.1")
    # opened Program on HDMI (index 1) and Multiview on panel (index 0)
    kinds = {t for t, _ in obs.opens}
    assert kinds == {"OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM", "OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW"}
    assert ("OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM", 1) in obs.opens
    assert ("OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW", 0) in obs.opens
    # and healed the stray Multiviews afterward
    assert sorted(closed) == ["0x00c0c101", "0x00c0c202"]


# ---------------------------------------------------------------------------
# e. REMOTE (non-local) host branch -- the rare dev1-invoked `--host <ip>` case (#769 review #2)
# ---------------------------------------------------------------------------

def test_heal_remote_host_uses_remote_wmctrl(monkeypatch):
    mod = _scenes_module()
    closed = []
    # a non-loopback host must route through the REMOTE (sshpass) wmctrl helpers, never the local ones
    monkeypatch.setattr(mod, "_wmctrl_list_remote", lambda host: _STACKED)
    monkeypatch.setattr(mod, "_wmctrl_close_remote", lambda host, win_id: closed.append((host, win_id)))
    monkeypatch.setattr(mod, "_wmctrl_list_local", lambda: (_ for _ in ()).throw(AssertionError("must not use local wmctrl for a remote host")))
    monkeypatch.setattr(mod.time, "sleep", lambda *_a, **_k: None)
    mod._heal_projector_strays("10.77.9.182", ["Multiview", "Program"])
    assert sorted(closed) == [("10.77.9.182", "0x00c0c101"), ("10.77.9.182", "0x00c0c202")]


def test_heal_remote_missing_wmctrl_warns_by_name_and_never_raises(monkeypatch, capsys):
    mod = _scenes_module()
    closed = []
    # the #833-critical dev1 case: remote probe reports wmctrl MISSING -> list returns None ->
    # warn LOUD by name, skip dedup, never raise, never read as "no windows"
    monkeypatch.setattr(mod, "_wmctrl_list_remote", lambda host: None)
    monkeypatch.setattr(mod, "_wmctrl_close_remote", lambda host, win_id: closed.append((host, win_id)))
    monkeypatch.setattr(mod.time, "sleep", lambda *_a, **_k: None)
    mod._heal_projector_strays("10.77.9.182", ["Multiview", "Program"])  # must NOT raise
    assert closed == []
    out = capsys.readouterr().out
    assert "wmctrl" in out and "#769" in out and "10.77.9.182" in out


# ---------------------------------------------------------------------------
# f. projector() with NO panel -- opens/reconciles ONLY Program, warns no panel (#769 review #2)
# ---------------------------------------------------------------------------

class HdmiOnlyObs:
    """A box with ONLY an HDMI projector monitor (no panel) -- e.g. a headless/remote debug session.
    projector() must open + reconcile ONLY the Program kind, and never try to heal a Multiview it
    never opened."""

    def __init__(self):
        self.opens = []

    def req(self, rtype, payload=None, ignore_err=False):
        if rtype == "GetMonitorList":
            return {"monitors": [{"monitorIndex": 1, "monitorName": "HDMI-0(1)"}]}
        if rtype == "OpenVideoMixProjector":
            self.opens.append((payload["videoMixType"], payload["monitorIndex"]))
        return {}


def test_projector_no_panel_opens_and_heals_program_only(monkeypatch, capsys):
    mod = _scenes_module()
    obs = HdmiOnlyObs()
    healed_kinds = []
    # issue 1152 M4 hermeticity: dormant drm-output config (see the sibling note above).
    monkeypatch.setattr(mod, "_drm_output_config_text", lambda host: "", raising=False)
    monkeypatch.setattr(mod, "_heal_projector_strays",
                        lambda host, kinds: healed_kinds.extend(kinds))
    mod.projector(obs, "127.0.0.1")
    # only the PROGRAM projector opened; no MULTIVIEW open attempted
    assert obs.opens == [("OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM", 1)]
    # only Program is reconciled -- a Multiview that was never opened must not be healed
    assert healed_kinds == ["Program"]
    assert "no panel monitor detected" in capsys.readouterr().out


# ---------------------------------------------------------------------------
# g. the REAL subprocess branches of _wmctrl_list_local (#833 rc-check, NOT monkeypatched out)
# ---------------------------------------------------------------------------

class _FakeRun:
    def __init__(self, returncode, stdout=""):
        self.returncode = returncode
        self.stdout = stdout


def test_wmctrl_list_local_missing_tool_returns_none(monkeypatch):
    mod = _scenes_module()
    monkeypatch.setattr(mod.shutil, "which", lambda _n: None)  # wmctrl absent
    assert mod._wmctrl_list_local() is None


def test_wmctrl_list_local_rc_nonzero_returns_none_never_zero_windows(monkeypatch, capsys):
    # #833: a PRESENT-but-failing wmctrl (rc != 0, e.g. X unreachable) must return None + WARN,
    # NEVER be read as "0 windows". Exercises the real subprocess branch (not monkeypatched out).
    mod = _scenes_module()
    monkeypatch.setattr(mod.shutil, "which", lambda _n: "/usr/bin/wmctrl")
    monkeypatch.setattr(mod.subprocess, "run", lambda *a, **k: _FakeRun(1, ""))
    assert mod._wmctrl_list_local() is None
    assert "wmctrl" in capsys.readouterr().out


def test_wmctrl_list_local_rc_zero_returns_stdout(monkeypatch):
    mod = _scenes_module()
    monkeypatch.setattr(mod.shutil, "which", lambda _n: "/usr/bin/wmctrl")
    monkeypatch.setattr(mod.subprocess, "run", lambda *a, **k: _FakeRun(0, _CLEAN))
    assert mod._wmctrl_list_local() == _CLEAN
