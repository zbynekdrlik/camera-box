"""issue 1371 -- the development handover check reports the test camera's production exposure.

The E2E snapshots the test camera's ISO + shutter (the owner's production exposure) before its
first set of a development period; `rig-mode.sh event` restores it and moves the snapshot aside.
At the handover (the owner gives the rig back after a production) the check says which of the
three states the snapshot is in, read by `camera_test_settings.py snapshot-state`:

  none      -> OK          nothing was changed / nothing is waiting
  restored  -> OK          an EVENT switch put the production exposure back (the newest consumed file)
  pending   -> OK          rig in TEST: a snapshot of this development period, waiting for its
                           EVENT switch
  pending   -> SUPERVISOR  rig in EVENT (the handover moment): the EVENT switch never restored it
  pending   -> UNKNOWN     rig mode unreadable
  pending + restore_failed
            -> SUPERVISOR  an EVENT switch tried and did NOT restore it (camera absent, read-back
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
PENDING_FAILED = ("exposure state=pending box=cam1 taken=2026-09-26T15:00:00Z iso=800 d002=36000 "
                  "restore_failed=2026-09-26T18:00:00Z\n")
INVALID = "exposure state=invalid path=/h/.camera-box/camera-prod-exposure.json reason=not-json\n"


def _item():
    return next(i for i in d.ITEMS if i.key == "exposure")


def test_the_exposure_item_is_in_the_checklist():
    item = _item()
    # it also reads the rig-mode capture: a pending snapshot means something different in EVENT
    assert item.captures == ["exposure", "mode"]
    assert "expozícia" in item.label


def test_no_snapshot_and_a_restored_snapshot_are_ok():
    for mode in ("TEST\n", "EVENT\n", ""):
        assert _item().decide({"exposure": (NONE, 0), "mode": (mode, 0)})["status"] == d.OK
    e = _item().decide({"exposure": (RESTORED, 0), "mode": ("EVENT\n", 0)})
    assert e["status"] == d.OK
    assert "vrátená" in e["message"] and "iso=800 d002=36000" in e["message"]
    # review round 1: a consumed file does not prove it was the LAST EVENT switch
    assert "pri poslednom EVENT" not in e["message"]


def test_a_snapshot_still_waiting_for_its_event_switch_is_ok():
    # review round 1: an E2E earlier in THIS development period leaves a pending snapshot; that is
    # the expected state, never a false supervisor alarm
    e = _item().decide({"exposure": (PENDING, 0), "mode": ("TEST\n", 0)})
    assert e["status"] == d.OK
    assert "čaká" in e["message"] and "cam1" in e["message"]


def test_a_snapshot_still_pending_while_the_rig_is_in_event_was_never_restored():
    # review round 2: the restore-failed marker only exists when the restore RAN and failed; an EVENT
    # switch that aborted before it leaves none. At the handover the rig is in EVENT, so a pending
    # snapshot there means production ran on the TEST exposure.
    e = _item().decide({"exposure": (PENDING, 0), "mode": ("EVENT\n", 0)})
    assert e["status"] == d.SUPERVISOR
    assert "NEVRÁTILA" in e["message"] and "cam1" in e["message"]
    # the rig mode unreadable (cam2 down): a pending snapshot cannot be judged -> UNKNOWN, never OK
    for mode in (("UNKNOWN\n", 0), ("", d.RC_MISSING)):
        assert _item().decide({"exposure": (PENDING, 0), "mode": mode})["status"] == d.UNKNOWN


def test_a_failed_restore_is_a_supervisor_problem_naming_what_was_not_restored():
    e = _item().decide({"exposure": (PENDING_FAILED, 0), "mode": ("TEST\n", 0)})
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
    exp = _item().decide({"exposure": (PENDING_FAILED, 0)})
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
    cli = ["python3", str(_SCRIPTS / "camera_test_settings.py")]
    r = subprocess.run(cli + ["snapshot-state"], capture_output=True, text=True, env=env)
    assert r.returncode == 0, r.stderr
    assert _item().decide({"exposure": (r.stdout, r.returncode), "mode": ("TEST\n", 0)})["status"] == d.OK
    # an EVENT switch that failed to restore it marks the snapshot -> SUPERVISOR
    assert subprocess.run(cli + ["restore-failed", "--reason", "camera absent"], env=env).returncode == 0
    r = subprocess.run(cli + ["snapshot-state"], capture_output=True, text=True, env=env)
    assert r.returncode == 0, r.stderr
    e = _item().decide({"exposure": (r.stdout, r.returncode)})
    assert e["status"] == d.SUPERVISOR and "iso=800 d002=36000" in e["message"]
