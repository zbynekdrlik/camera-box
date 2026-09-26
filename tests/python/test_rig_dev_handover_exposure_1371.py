"""issue 1371 -- the development handover check reports the test camera's production exposure.

The E2E snapshots the test camera's ISO + shutter (the owner's production exposure) before its
first set of a development period; `rig-mode.sh event` restores it and moves the snapshot aside.
At the handover (the owner gives the rig back after a production) the check says which of the
three states the snapshot is in, read by `camera_test_settings.py snapshot-state`:

  none      -> OK          nothing was changed / nothing is waiting
  restored  -> OK          the last EVENT switch put the production exposure back
  pending   -> SUPERVISOR  the last EVENT switch did NOT restore it (camera absent, read-back
                           mismatch, a live broadcast): production ran on the TEST exposure
  invalid   -> SUPERVISOR  a snapshot file that cannot be read: the owner's values are stuck in it
  (no line) -> UNKNOWN     the probe did not run

Pure decision (pytest Tier-0) + a static check that the orchestrator runs the probe.
"""
import os
import pathlib
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import rig_dev_handover_decision as d  # noqa: E402

NONE = "exposure state=none\n"
RESTORED = ("exposure state=restored restored=20260926T160000Z box=cam1 taken=2026-09-26T15:00:00Z "
            "iso=800 d002=36000\n")
PENDING = "exposure state=pending box=cam1 taken=2026-09-26T15:00:00Z iso=800 d002=36000\n"
INVALID = "exposure state=invalid path=/h/.camera-box/camera-prod-exposure.json reason=not-json\n"


def _item():
    return next(i for i in d.ITEMS if i.key == "exposure")


def test_the_exposure_item_is_in_the_checklist():
    item = _item()
    assert item.captures == ["exposure"]
    assert "expozícia" in item.label


def test_no_snapshot_and_a_restored_snapshot_are_ok():
    e = _item().decide({"exposure": (NONE, 0)})
    assert e["status"] == d.OK
    e = _item().decide({"exposure": (RESTORED, 0)})
    assert e["status"] == d.OK
    assert "vrátená" in e["message"] and "iso=800 d002=36000" in e["message"]


def test_a_pending_snapshot_is_a_supervisor_problem_naming_what_was_not_restored():
    e = _item().decide({"exposure": (PENDING, 0)})
    assert e["status"] == d.SUPERVISOR
    assert "NEVRÁTILA" in e["message"]
    assert "cam1" in e["message"] and "iso=800 d002=36000" in e["message"]
    assert "rig-mode.sh event" in e["message"]


def test_an_invalid_snapshot_is_a_supervisor_problem():
    e = _item().decide({"exposure": (INVALID, 0)})
    assert e["status"] == d.SUPERVISOR
    assert "camera-prod-exposure.json" in e["message"]


def test_no_readable_line_is_unknown_never_ok():
    assert _item().decide({"exposure": ("", d.RC_MISSING)})["status"] == d.UNKNOWN
    assert _item().decide({"exposure": ("Traceback ...\n", 1)})["status"] == d.UNKNOWN
    assert _item().decide({})["status"] == d.UNKNOWN


def test_the_summary_names_the_exposure_without_calling_it_a_watchdog():
    exp = _item().decide({"exposure": (PENDING, 0)})
    lines, summary, code = d.build_checklist([exp])
    assert code == 1
    assert summary.startswith("supervisor musí vyriešiť: " + _item().label)
    assert "watchdog" not in summary
    assert lines[0].startswith(d.GLYPH[d.SUPERVISOR])

    wd = {"key": "watchdogs", "label": "dev1 watchdog timery", "status": d.SUPERVISOR,
          "message": "nezapnutý watchdog: av-step", "names": ["av-step"]}
    _, summary, code = d.build_checklist([wd, exp])
    assert code == 1
    assert "supervisor musí zapnúť watchdogy: av-step" in summary
    assert "supervisor musí vyriešiť: " + _item().label in summary

    forgot = {"key": "mic", "label": "merací mikrofón (mbc)", "status": d.FORGOT, "message": "m"}
    _, summary, code = d.build_checklist([forgot, exp])
    assert code == 1
    assert summary.startswith("zabudol si: merací mikrofón (mbc)")
    assert "(supervisor musí vyriešiť: " + _item().label + ")" in summary


def test_the_orchestrator_runs_the_snapshot_state_probe():
    s = (_SCRIPTS / "rig-dev-handover-check.sh").read_text(encoding="utf-8")
    assert 'EXPOSURE_PROBE="${RDH_EXPOSURE_PROBE:-$HERE/camera_test_settings.py}"' in s
    assert s.count('run_probe exposure python3 "$EXPOSURE_PROBE" snapshot-state') == 1
    # it runs before the decision engine reads the work dir
    assert s.index("run_probe exposure") < s.index('python3 "$DECIDE"')


def test_the_probe_line_the_orchestrator_captures_is_what_the_decision_parses(tmp_path):
    # end to end over the real CLI: a pending snapshot in a temp HOME -> SUPERVISOR
    import json
    import subprocess
    snap = tmp_path / "camera-prod-exposure.json"
    snap.write_text(json.dumps({"schema": 1, "box": "cam1", "taken_utc": "2026-09-26T15:00:00Z",
                                "values": {"iso": "800", "d002": "36000"}, "context": {"d007": "60"}}))
    env = dict(os.environ, CAMERA_PROD_EXPOSURE_SNAPSHOT=str(snap))
    r = subprocess.run(["python3", str(_SCRIPTS / "camera_test_settings.py"), "snapshot-state"],
                       capture_output=True, text=True, env=env)
    assert r.returncode == 0, r.stderr
    e = _item().decide({"exposure": (r.stdout, r.returncode)})
    assert e["status"] == d.SUPERVISOR and "iso=800 d002=36000" in e["message"]
