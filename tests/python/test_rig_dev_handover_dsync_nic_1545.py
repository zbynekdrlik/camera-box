"""issue 1545 -- the development handover grades dev1's dsync NIC against the fleet fact.

20.11.2026 the handover's clock item read FAIL: dev1's dsync had joined the down enp39s0 and ran on
NTP. The clock item saw only the lost LOCK. The `dsync_nic` item runs the read-only
`scripts/dev1-dsync-iface.sh --check` and grades its EXIT code: 0 = OK, 1 = dsync configured
elsewhere / not configured / joined elsewhere = SUPERVISOR (the owner cannot fix a unit file; the
line names `scripts/dev1-dsync-iface.sh --apply`), anything else (2 = unreadable /status, a timeout,
a missing probe) = UNKNOWN, never OK.
"""
import json
import os
import pathlib
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(REPO / "scripts"))

import rig_dev_handover_decision as d  # noqa: E402

ORCH = REPO / "scripts" / "rig-dev-handover-check.sh"
OK_LINE = "dsync-iface: node=dev1 declared=enp38s0 verdict=ok:enp38s0\n"
BAD_LINE = ("dsync-iface: node=dev1 declared=enp38s0 verdict=not-configured\n"
            "fix: scripts/dev1-dsync-iface.sh --apply\n")


def _item():
    return next(i for i in d.ITEMS if i.key == "dsync_nic")


def test_the_item_exists_and_reads_one_capture():
    it = _item()
    assert it.captures == ["dsync_nic"]


def test_exit_0_is_ok():
    e = _item().decide({"dsync_nic": (OK_LINE, 0)})
    assert e["status"] == d.OK
    assert "enp38s0" in e["message"]


def test_exit_1_is_a_supervisor_problem_naming_the_fix_and_the_verdict():
    e = _item().decide({"dsync_nic": (BAD_LINE, 1)})
    assert e["status"] == d.SUPERVISOR
    assert "scripts/dev1-dsync-iface.sh --apply" in e["message"]
    assert "not-configured" in e["message"]
    _lines, summary, code = d.build_checklist([e])
    assert code == 1
    assert summary.startswith("supervisor musí vyriešiť:"), summary
    assert "zabudol si" not in summary


def test_anything_else_is_unknown_never_ok():
    for text, rc in (("dsync-iface: node=dev1 declared=enp38s0 verdict=unreadable\n", 2),
                     ("", 124), ("MISSING: x\n", d.RC_MISSING), (OK_LINE, 3)):
        assert _item().decide({"dsync_nic": (text, rc)})["status"] == d.UNKNOWN, (text, rc)
    assert _item().decide({})["status"] == d.UNKNOWN


def test_the_orchestrator_runs_the_check_read_only():
    s = ORCH.read_text()
    assert 'DSYNC_NIC_PROBE="${RDH_DSYNC_NIC_PROBE:-$HERE/dev1-dsync-iface.sh}"' in s
    assert 'run_probe dsync_nic bash "$DSYNC_NIC_PROBE" --check' in s
    assert "--apply" not in "\n".join(ln for ln in s.splitlines() if not ln.lstrip().startswith("#"))


def _exe(path, body):
    path.write_text(body)
    path.chmod(0o755)
    return path


def test_the_orchestrator_capture_reaches_the_item(tmp_path):
    stub = _exe(tmp_path / "dev1-dsync-iface.sh",
                f'#!/usr/bin/env bash\n[ "$1" = --check ] || exit 9\nprintf %s "{BAD_LINE.strip()}"\nexit 1\n')
    missing = str(tmp_path / "missing")
    env = {"PATH": os.environ["PATH"], "HOME": str(tmp_path), "TMPDIR": str(tmp_path), "RDH_TIMEOUT": "10",
           "RDH_IMAG_RETIRED": "1", "RDH_DSYNC_NIC_PROBE": str(stub)}
    for item in ("MIC", "MIC_GUARD", "PAINTER", "CLOCK", "OBS", "NET", "AUDIOLAG", "GENLOCK", "BURN",
                 "MAPPING", "PINS", "DANTESYNC", "CAMBOX", "AVLATENCY", "EXPOSURE", "JXS", "JXS_LIVE_LIB",
                 "RIGMODE_LIB", "WATCHDOG_ROSTER", "CAMSET_LIB"):
        key = f"RDH_{item}" if item.endswith("_LIB") or item == "WATCHDOG_ROSTER" else f"RDH_{item}_PROBE"
        env[key] = missing
    r = subprocess.run(["bash", str(ORCH), "--json"], capture_output=True, text=True, env=env, timeout=120)
    out = json.loads(r.stdout)
    e = next(x for x in out["items"] if x["key"] == "dsync_nic")
    assert e["status"] == d.SUPERVISOR, (e, r.stderr)
