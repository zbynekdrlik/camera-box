"""issue 1462: the genlock deploy's start step starts the session apps of the box's ROLE.

stream-snv became a Linux stream-role box on 22.10.2026. Its first deploy
(`deploy-genlock-fleet.sh --boxes stream-snv`) started the STRIH role's session apps on it (the
shading panel, Companion Satellite and its watch timer), which a stream box never installs, and
printed "the session apps ... did not start" although every stream-role app ran. setup-strih and
verify-strih take the stream list from `obs_box_role_apply`; the deploy's start command must use the
same list, read from the box's fact file, never the strih default.
"""
from __future__ import annotations

import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
FLEET = REPO / "scripts" / "deploy-genlock-fleet.sh"
STRIH_APPS = ("strih-browser-keeper.service", "bkshading-panel-app.service",
              "companion-satellite.service", "strih-satellite-watch.timer", "dantesync-tray.service")
STREAM_APPS = ("strih-browser-keeper.service", "strih-stream-monitor.service", "dantesync-tray.service")


def _start_cmd(box):
    script = 'set -euo pipefail\n. "%s"\nstrih_lx_remote_start_cmd /tmp/stage "$1"\n' % FLEET
    r = subprocess.run(["bash", "-c", script, "harness", box], capture_output=True, text=True,
                       cwd=REPO, env={"PATH": os.environ["PATH"], "HOME": "/home/newlevel",
                                      "LC_ALL": "C.UTF-8"}, timeout=60)
    assert r.returncode == 0, r.stderr
    return r.stdout


def _started_units(cmd):
    # the second `systemctl --user start` names the session apps
    tail = cmd.split("systemctl --user start strih-obs.service", 1)[1]
    units = tail.split("systemctl --user start ", 1)[1].split(" ||", 1)[0]
    return tuple(units.split())


def test_a_stream_role_box_starts_the_stream_session_apps():
    for box in ("stream-snv", "stream-pp"):
        assert _started_units(_start_cmd(box)) == STREAM_APPS, box


def test_a_strih_role_box_keeps_the_strih_session_apps():
    for box in ("strih-snv", "strih-pp"):
        assert _started_units(_start_cmd(box)) == STRIH_APPS, box


def test_the_warning_names_the_units_it_started():
    cmd = _start_cmd("stream-snv")
    assert "the session apps (%s) did not start" % " ".join(STREAM_APPS) in cmd
    assert "bkshading-panel-app" not in cmd and "companion-satellite" not in cmd
