"""issue 1482 -- the EVENT contract names a direct-feed drop-in left on cam2.

do_event removes the drop-ins, but a removal that failed (cam2 down, the rw window refused) left
`zz-rig-direct-intercom-off.conf` in /etc with nothing naming it: cam2's intercom stays OFF on air.
The contract's item 8 (test artifacts cleared) checks every drop-in path the lib declares.
"""
import os
import re
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib" / "rig-test-feed.sh"
EVENT_LIB = REPO / "scripts" / "lib" / "event-assert.sh"
RIG = REPO / "scripts" / "rig-mode.sh"


def _bash(body, env=None):
    e = {"PATH": os.environ["PATH"], "HOME": os.environ.get("HOME", "/tmp")}
    e.update(env or {})
    return subprocess.run(["bash", "-c", 'set -euo pipefail\n. "$1"\n. "$2"\n' + body, "h", str(LIB), str(EVENT_LIB)],
                          capture_output=True, text=True, env=e, timeout=30)


def test_the_lib_declares_every_drop_in_path_once():
    r = _bash("rig_test_feed_dropin_paths")
    assert r.returncode == 0, r.stderr
    paths = r.stdout.split()
    assert paths == [
        "/etc/systemd/system/cam2-painter.service.d/zz-rig-direct-marker.conf",
        "/etc/systemd/system/camera-box.service.d/zz-rig-direct-intercom-off.conf",
        "/run/systemd/system/cam2-painter.service.d/zz-rig-direct-marker.conf",
        "/run/systemd/system/camera-box.service.d/zz-rig-direct-intercom-off.conf",
    ], paths


def test_the_artifacts_check_names_a_leftover_drop_in(tmp_path):
    left = tmp_path / "etc" / "camera-box.service.d" / "zz-rig-direct-intercom-off.conf"
    left.parent.mkdir(parents=True)
    left.write_text("[Service]\nEnvironment=CAMERA_BOX_INTERCOM=off\n")
    gone = tmp_path / "etc" / "cam2-painter.service.d" / "zz-rig-direct-marker.conf"
    env = {"RIG_TEST_FEED_INTERCOM_DROPIN": str(left), "RIG_TEST_FEED_PAINTER_DROPIN": str(gone),
           "RIG_TEST_FEED_INTERCOM_DROPIN_LEGACY": str(tmp_path / "run1"),
           "RIG_TEST_FEED_PAINTER_DROPIN_LEGACY": str(tmp_path / "run2")}
    r = _bash('bash -c "$(event_assert_artifacts_check_cmds $(rig_test_feed_dropin_paths))"', env)
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == [str(left)], r.stdout


def test_the_event_contract_checks_the_drop_ins_on_cam2():
    text = RIG.read_text()
    line = next(l for l in text.splitlines()
                if "artifacts_remote=" in l and "event_assert_artifacts_check_cmds" in l)
    assert re.search(r"\$\(rig_test_feed_dropin_paths\)", line), line
