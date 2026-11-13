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


def test_a_purged_drop_in_systemd_still_has_loaded_is_reloaded_and_its_unit_restarted(tmp_path):
    # review of the release PR: the EVENT purge removes a leftover /run drop-in with a plain rm; without a
    # daemon-reload systemd keeps the loaded override (cam2's intercom stays OFF on air) while item 8
    # reads the file gone and passes. The net after the purge reloads + try-restarts every unit whose
    # drop-in dir changed under it (NeedDaemonReload=yes), and nothing else.
    log = tmp_path / "systemctl.log"
    stub = tmp_path / "bin"
    stub.mkdir()
    (stub / "systemctl").write_text(
        "#!/bin/bash\necho \"$*\" >> \"%s\"\n"
        "if [ \"$1\" = show ]; then case \"$*\" in *camera-box.service*) echo yes ;; *) echo no ;; esac; fi\n" % log)
    (stub / "systemctl").chmod(0o755)
    r = _bash('bash -c "$(rig_test_feed_dropin_reload_cmds $(rig_test_feed_dropin_paths))"',
              env={"PATH": "%s:%s" % (stub, os.environ["PATH"])})
    assert r.returncode == 0, r.stderr
    calls = log.read_text().splitlines()
    assert "show -p NeedDaemonReload --value camera-box.service" in calls, calls
    assert "show -p NeedDaemonReload --value cam2-painter.service" in calls, calls
    assert "daemon-reload" in calls, calls
    assert "try-restart camera-box.service" in calls, calls
    assert "try-restart cam2-painter.service" not in calls, calls


def test_nothing_reloads_when_systemd_has_no_stale_drop_in(tmp_path):
    log = tmp_path / "systemctl.log"
    stub = tmp_path / "bin"
    stub.mkdir()
    (stub / "systemctl").write_text("#!/bin/bash\necho \"$*\" >> \"%s\"\n[ \"$1\" = show ] && echo no\nexit 0\n" % log)
    (stub / "systemctl").chmod(0o755)
    r = _bash('bash -c "$(rig_test_feed_dropin_reload_cmds $(rig_test_feed_dropin_paths))"',
              env={"PATH": "%s:%s" % (stub, os.environ["PATH"])})
    assert r.returncode == 0, r.stderr
    calls = log.read_text().splitlines()
    assert "daemon-reload" not in calls and not [c for c in calls if c.startswith("try-restart")], calls


def test_event_runs_the_reload_net_right_after_the_purge():
    s = RIG.read_text()
    purge = s.index("cam_ssh \"$(event_artifact_purge_cmds ")
    net = s.index("rig_test_feed_dropin_reload_cmds", purge)
    assert net - purge < 400, net - purge
