"""issue 1452 (owner 10.11.2026): a cambox on the REST relay pin runs its relay in EVERY rig mode.

Owner: "relay ma stale bezat no matter what ... vsetko ma bezat na cameraboxoch, su to produkcne
zariadenia". The issue-1311 TEST-mode stop+disable and the issue-808 E2E pause exist because the
gphoto2 relay v0.1.x opens a PTP session on the xHCI hub it shares with the boot disk and the
grabber. The REST relay v0.2.x opens no PTP session: it only speaks HTTP over the camera's cdc_ncm
link. So a box listed on scripts/shading-release-rest.pin in scripts/shading-release-boxes.txt:
  * `bkshading_relay_box_always_on` answers yes (a gphoto2 box, an unknown name: no);
  * `rig-mode.sh test` ENABLES + STARTS its relay (the EVENT program), never stop + disable;
  * the E2E pause never stops it (was-active "0", so the restore stays a no-op) and makes no ssh call;
  * setup-device / verify-device (ao) expect it `enabled` in TEST mode.
A gphoto2 box keeps the old behaviour (the cam2 painter box stays stop + disable in TEST).
"""
import os
import pathlib
import subprocess

REPO = pathlib.Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib"


def _bash(script, args=(), env=None):
    e = dict(os.environ)
    e.pop("SHADING_RELEASE_BOXES_FILE", None)
    e.pop("SHADING_RELEASE_PIN_FILE", None)
    if env:
        e.update(env)
    return subprocess.run(["bash", "-c", script, "harness", *args], capture_output=True, text=True,
                          env=e, timeout=60)


def _map(tmp_path, text):
    m = tmp_path / "shading-release-boxes.txt"
    m.write_text(text)
    for pin in ("shading-release.pin", "shading-release-rest.pin"):
        (tmp_path / pin).write_text((REPO / "scripts" / pin).read_text())
    return str(m)


def test_always_on_follows_the_real_pin_map():
    r = _bash(f'set -euo pipefail; . "{LIB}/bkshading-relay-runtime.sh"; '
              'for b in "$@"; do if bkshading_relay_box_always_on "$b"; then echo "$b yes"; else echo "$b no"; fi; done',
              args=["cam3", "CAM5", "cam2", "cam1", "bogus/x", ""])
    assert r.returncode == 0, r.stderr
    assert r.stdout.split("\n")[:6] == ["cam3 yes", "CAM5 yes", "cam2 no", "cam1 no", "bogus/x no", " no"], r.stdout


def test_always_on_reads_the_map_not_a_literal(tmp_path):
    m = _map(tmp_path, "cam2 shading-release-rest.pin\n")
    r = _bash(f'set -euo pipefail; . "{LIB}/bkshading-relay-runtime.sh"; '
              'for b in cam2 cam3; do if bkshading_relay_box_always_on "$b"; then echo "$b yes"; else echo "$b no"; fi; done',
              env={"SHADING_RELEASE_BOXES_FILE": m})
    assert r.stdout.split() == ["cam2", "yes", "cam3", "no"], r.stdout + r.stderr


def _fake_sshpass(tmp_path):
    log = tmp_path / "ssh.log"
    d = tmp_path / "bin"
    d.mkdir()
    f = d / "sshpass"
    # The remote program is the LAST argument; record which half it is, answer like a healthy box.
    f.write_text("#!/bin/bash\n"
                 f'log="{log}"\n'
                 'host=""; for a in "${@:1:$#-1}"; do case "$a" in root@*) host="${a#root@}" ;; esac; done\n'
                 'cmd="${@: -1}"\n'
                 f'printf "%s" "$cmd" >"{tmp_path}/ssh.cmd"\n'
                 'case "$cmd" in\n'
                 '  *"systemctl enable"*) echo "$host enable+start" >>"$log"; echo RELAY_ENABLED=enabled ;;\n'
                 '  *"systemctl disable"*) echo "$host stop+disable" >>"$log"; echo RELAY_ENABLED=disabled ;;\n'
                 '  *) echo "$host other" >>"$log" ;;\n'
                 'esac\n'
                 'exit 0\n')
    f.chmod(0o755)
    return str(d), log


def test_test_mode_keeps_a_rest_box_running_and_stops_a_gphoto2_box(tmp_path):
    d, log = _fake_sshpass(tmp_path)
    r = _bash(f'set -euo pipefail; . "{LIB}/bkshading-relay-mode.sh"; '
              'bkshading_relay_mode_apply test pw cam3=192.0.2.63 cam2=192.0.2.62',
              env={"PATH": d + ":" + os.environ["PATH"]})
    assert r.returncode == 0, r.stdout + r.stderr
    assert log.read_text().splitlines() == ["192.0.2.63 enable+start", "192.0.2.62 stop+disable"]
    assert "kept enabled+running (TEST mode -- REST relay v0.2.x" in r.stdout and "on cam3" in r.stdout, r.stdout
    assert "stopped+disabled (TEST mode" in r.stdout and "on cam2" in r.stdout, r.stdout


def test_event_mode_unchanged_for_both(tmp_path):
    d, log = _fake_sshpass(tmp_path)
    r = _bash(f'set -euo pipefail; . "{LIB}/bkshading-relay-mode.sh"; '
              'bkshading_relay_mode_apply event pw cam3=192.0.2.63 cam2=192.0.2.62',
              env={"PATH": d + ":" + os.environ["PATH"]})
    assert r.returncode == 0, r.stderr
    assert log.read_text().splitlines() == ["192.0.2.63 enable+start", "192.0.2.62 enable+start"]


def test_e2e_pause_leaves_a_rest_box_running_and_clears_an_old_marker(tmp_path):
    d, log = _fake_sshpass(tmp_path)
    r = _bash(f'set -euo pipefail; . "{LIB}/bkshading-e2e-pause.sh"; '
              'v="$(bkshading_e2e_pause_stop cam5 192.0.2.65 pw)"; echo "WAS=$v"',
              env={"PATH": d + ":" + os.environ["PATH"]})
    assert r.returncode == 0, r.stderr
    assert "WAS=0" in r.stdout, r.stdout
    assert "left running (REST relay v0.2.x" in r.stderr, r.stderr
    # review finding: a marker an earlier (cancelled) run left still owes a start -- one call that
    # clears it and starts the relay, never a stop
    assert log.read_text().splitlines() == ["192.0.2.65 other"], log.read_text()
    cmd = (tmp_path / "ssh.cmd").read_text()
    assert "rm -f /run/bkshading-e2e-paused" in cmd and "systemctl start bkshading-relay.service" in cmd, cmd
    assert "systemctl stop" not in cmd, cmd


def _fake_systemctl(tmp_path, state):
    d = tmp_path / "sbin"
    d.mkdir()
    st = tmp_path / "relay.state"
    st.write_text(state)
    log = tmp_path / "systemctl.log"
    f = d / "systemctl"
    f.write_text("#!/bin/bash\n"
                 f'st="{st}"; echo "$*" >>"{log}"\n'
                 'case "$1" in\n'
                 '  is-active) s="$(cat "$st")"; [ "${2:-}" = --quiet ] || echo "$s"; [ "$s" = active ] ;;\n'
                 '  stop) echo inactive >"$st" ;;\n'
                 '  start) echo active >"$st" ;;\n'
                 'esac\n')
    f.chmod(0o755)
    return str(d), st, log


def _rest_session(tmp_path, label, state):
    d, st, log = _fake_systemctl(tmp_path, state)
    net = tmp_path / "net"
    net.mkdir()
    # build the session text, then RUN it with the fake systemctl (no camera link: exit 95)
    r = _bash(f'set -euo pipefail; . "{LIB}/camera-test-settings.sh"; CTS_FOUND_LABEL="$1"; '
              'CAMERA_TEST_SETTINGS_NET_SYSFS="$2" camera_test_settings_rest_cmd get video/iso',
              args=[label, str(net)])
    assert r.returncode == 0, r.stderr
    run = subprocess.run(["bash", "-c", r.stdout], capture_output=True, text=True, timeout=30,
                         env=dict(os.environ, PATH=d + ":" + os.environ["PATH"]))
    return run, st.read_text().strip(), log.read_text().splitlines()


def test_rest_session_on_an_always_on_box_holds_the_relay_and_starts_it_again(tmp_path):
    # review finding (critical): the E2E pause leaves the REST relay running, and the test-camera
    # REST session refused an active relay (exit 97). On a REST box it now stops the relay for its
    # own requests and the EXIT trap starts it again, whatever the session ends with.
    run, state, calls = _rest_session(tmp_path, "cam3", "active")
    assert "CTS_RELAY_ACTIVE" not in run.stdout, run.stdout
    assert run.returncode != 97, run.stdout
    assert state == "active", (state, calls)
    assert calls.index("stop bkshading-relay.service") < calls.index("start bkshading-relay.service"), calls
    assert "CTS_RELAY_HOLD 1" in run.stdout, run.stdout


def test_rest_session_on_a_gphoto2_box_still_refuses_an_active_relay(tmp_path):
    run, state, calls = _rest_session(tmp_path, "cam2", "active")
    assert run.returncode == 97 and "CTS_RELAY_ACTIVE active" in run.stdout, run.stdout
    assert state == "active" and not any(c.startswith(("stop", "start")) for c in calls), calls


def test_rest_session_on_an_always_on_box_with_a_stopped_relay_leaves_it_stopped(tmp_path):
    run, state, calls = _rest_session(tmp_path, "cam3", "inactive")
    assert state == "inactive" and "CTS_RELAY_HOLD 0" in run.stdout, (state, run.stdout)


def test_test_mode_start_program_names_test_in_its_close_hint():
    # review finding: a failed close inside TEST mode must tell the operator to re-run TEST
    r = _bash(f'set -euo pipefail; . "{LIB}/bkshading-relay-mode.sh"; bkshading_relay_mode_start_cmds test; '
              'echo ==; bkshading_relay_mode_start_cmds')
    test_txt, event_txt = r.stdout.split("==\n")
    assert "re-run rig-mode.sh test." in test_txt and "re-run rig-mode.sh event" not in test_txt, test_txt
    assert "re-run rig-mode.sh event." in event_txt, event_txt


def test_e2e_pause_still_stops_a_gphoto2_box(tmp_path):
    d, log = _fake_sshpass(tmp_path)
    r = _bash(f'set -euo pipefail; . "{LIB}/bkshading-e2e-pause.sh"; '
              'v="$(bkshading_e2e_pause_stop cam2 192.0.2.62 pw)"; echo "WAS=$v"',
              env={"PATH": d + ":" + os.environ["PATH"]})
    assert r.returncode == 0, r.stderr
    assert log.read_text().splitlines() == ["192.0.2.62 other"], log.read_text()


def test_expected_enable_state_is_enabled_for_a_rest_box_in_test_mode():
    r = _bash(f'set -euo pipefail; . "{LIB}/bkshading-relay-provision.sh"; '
              'for b in cam3 cam5 cam2; do echo "$b $(bkshading_relay_expected_enable_state "$b" test cam3 cam2)"; done; '
              'echo "cam3-empty-src $(bkshading_relay_expected_enable_state cam3 test "" cam2)"')
    assert r.returncode == 0, r.stderr
    assert r.stdout.splitlines() == ["cam3 enabled", "cam5 enabled", "cam2 disabled", "cam3-empty-src enabled"], r.stdout


def test_rest_session_holds_a_relay_caught_in_its_restart_window(tmp_path):
    # re-review: a relay in `activating` (its Restart= window) is stopped and started again too,
    # never left to fail the session with exit 97
    run, state, calls = _rest_session(tmp_path, "cam3", "activating")
    assert run.returncode != 97 and "CTS_RELAY_HOLD 1" in run.stdout, run.stdout
    assert state == "active", (state, calls)
