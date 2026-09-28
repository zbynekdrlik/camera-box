"""issue 1367 -- the restart-matrix orchestrator (scripts/av-restart-matrix.sh) + its pure lib
(scripts/lib/av-restart-matrix.sh), driven end to end with FAKES (Tier-0: no rig, no cargo).

Fakes behind the documented seams:
  - AV_MATRIX_SOAK: a fake `av-soak.sh` -- every window is one soak run (`--run --hours 0
    --lease-run-id <the matrix's lease>`); the fake checks that lease, writes the soak's own CSV
    row (scripts/av_soak_decision.py row_from_verdict) + recording.state, and exits like the soak;
  - AV_SOAK_OBS_DIR/obs_phase2.py: rig-busy-check + program-scene (the WebSocket reads);
  - sshpass + curl on PATH: the strih-lx / cambox restart, health and ensure-running texts, the
    dantesync :8898/status read;
  - RIG_LEASE_DIR in a tmp dir -- the REAL dev1 lease is never touched (a soak/E2E may hold it).
"""
import base64
import json
import os
import re
import signal
import subprocess
import sys
import threading
import time

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
MATRIX = os.path.join(REPO, "scripts", "av-restart-matrix.sh")
LIB = os.path.join(REPO, "scripts", "lib", "av-restart-matrix.sh")
STRIH = "10.77.9.202"
STREAM = "10.77.9.204"
CAM1 = "10.77.9.61"

FAKE_SOAK_PY = r'''
import json, os, signal, sys, time
sys.path.insert(0, os.environ["FAKE_SCRIPTS"])
import av_soak_decision as soak
a = sys.argv[1:]
log = os.environ["FAKE_LOG"]
with open(log, "a") as f:
    f.write("soak " + json.dumps(a) + "\n")
def arg(name, default=""):
    return a[a.index(name) + 1] if name in a else default
if "--stop-leftovers" in a:
    d = arg("--stop-leftovers")
    st = os.path.join(d, "recording.state")
    if os.path.exists(st):
        txt = open(st).read().replace("strih=1", "strih=0").replace("stream=1", "stream=0")
        open(st, "w").write(txt)
    sys.exit(int(os.environ.get("FAKE_LEFTOVERS_RC", "0")))
state = os.environ["FAKE_STATE"]
cnt = os.path.join(state, "windows")
n = (int(open(cnt).read()) if os.path.exists(cnt) else 0) + 1
open(cnt, "w").write(str(n))
d = arg("--run-dir")
lease = arg("--lease-run-id")
try:
    holder = json.load(open(os.path.join(os.environ["RIG_LEASE_DIR"], "holder.json")))["run_id"]
except OSError:
    holder = ""
if not lease or holder != lease:
    sys.stderr.write("fake soak: the caller's lease is not held\n")
    sys.exit(4)
os.makedirs(d, exist_ok=True)
open(os.path.join(d, "pid"), "w").write(str(os.getpid()))
def on_term(*_):
    with open(log, "a") as f:
        f.write("soak-term %d\n" % n)
    sys.exit(5)
signal.signal(signal.SIGTERM, on_term)
if os.environ.get("FAKE_SOAK_SLEEP_WINDOW") == str(n):
    for _ in range(600):
        time.sleep(0.1)
if os.environ.get("FAKE_SOAK_TOUCH_STOP_WINDOW") == str(n):
    open(os.path.join(os.environ["FAKE_MATRIX_DIR"], "STOP"), "w").close()
rcmap = dict(x.split(":") for x in os.environ.get("FAKE_SOAK_RC_MAP", "").split(",") if x)
rc = int(rcmap.get(str(n), "2"))
stuck = os.environ.get("FAKE_SOAK_STUCK_WINDOW") == str(n)
open(os.path.join(d, "recording.state"), "w").write(
    "strih=%d\nstrih_since=\nstream=0\nstream_since=\nlease=\nstart_window_s=60\n" % int(stuck))
if rc in (0, 1, 2) or stuck:
    cams = os.environ.get("FAKE_CAMS", "cam1 cam3").split()
    bad = str(n) in os.environ.get("FAKE_SOAK_BAD_WINDOWS", "").split(",")
    v = {"all_cambox_av_sync": {"expected_ms": 0.0, "judged_cameras": len(cams)},
         "all_cambox_latency": {"cross_camera_spread_ms": None},
         "all_cambox_delivery_latency": {"cross_camera_spread_ms": None},
         "all_cambox_continuity": {"segments": [], "overall_pass": True},
         "full_chain": {"loss": {"strih": {"zero_loss": True, "real_drops": 0},
                                 "stream": {"zero_loss": True, "real_drops": 0}}}}
    for i, c in enumerate(cams):
        off = 99.0 if (bad and i == 0) else 3.0 + i
        v["all_cambox_av_sync"][c] = {"verdict": "measured", "av_offset_ms": off, "gate_pass": True}
        v["all_cambox_continuity"]["segments"].append(
            {"cambox": c.upper(), "pass": True, "relaxed_pass": True, "copies": 0, "gaps": 0,
             "undecodable": 0, "frames": 60})
    row = soak.row_from_verdict(v, cams, {"epoch_s": time.time(), "slot": 0, "slot_s": 600,
                                          "window_s": 60, "outcome": "ok"})
    soak.append_row(os.path.join(d, "soak.csv"), row, cams)
sys.exit(rc)
'''

FAKE_SOAK_SH = r'''#!/usr/bin/env bash
exec python3 "$FAKE_DIR/fake_soak.py" "$@"
'''

FAKE_OBS = r'''#!/usr/bin/env python3
import json, os, sys
a = sys.argv[1:]
state = os.environ["FAKE_STATE"]
with open(os.environ["FAKE_LOG"], "a") as f:
    f.write("obs " + json.dumps(a) + "\n")
def arg(name, default=""):
    return a[a.index(name) + 1] if name in a else default
def windows():
    p = os.path.join(state, "windows")
    return int(open(p).read()) if os.path.exists(p) else 0
def bump(name):
    p = os.path.join(state, "count-" + name)
    n = int(open(p).read()) + 1 if os.path.exists(p) else 1
    open(p, "w").write(str(n))
    return n
cmd = a[0]
if cmd == "rig-busy-check":
    after = int(os.environ.get("FAKE_LIVE_AFTER_WINDOWS", "0") or 0)
    live = after > 0 and windows() >= after
    print(json.dumps({"busy": live, "diagnostics": [
        {"host": "strih", "streaming": False, "recording": False, "recordTimecode": None},
        {"host": "stream", "streaming": live, "recording": False, "recordTimecode": None}]}))
elif cmd == "stream-detail":
    pass
elif cmd == "program-scene":
    host = arg("--host")
    if host == os.environ["FAKE_STREAM_HOST"]:
        n = bump("stream-reads")
        if n <= int(os.environ.get("FAKE_STREAM_WS_DOWN_READS", "0") or 0):
            sys.exit("fake: stream OBS WebSocket is not answering")
        print("Development")
    else:
        print("Cam 1")
else:
    sys.exit(f"fake obs_phase2: unexpected {a}")
'''

FAKE_SSHPASS = r'''#!/usr/bin/env bash
set -euo pipefail
text="${!#}"
host=""
for x in "$@"; do case "$x" in *@*) host="${x#*@}" ;; esac; done
printf 'sshpass %s %s\n' "$host" "$(printf '%s' "$text" | base64 -w0)" >> "$FAKE_LOG"
case "$text" in
  *"restart strih-obs.service"*)
    printf 'AV_MATRIX_RESTART_AT=1790000000\n'
    if [ -n "${FAKE_STRIH_NO_UNIT:-}" ]; then
      echo "MV_REVERIFY_NO_UNIT: strih-obs.service is not installed on strih-lx"; exit 2
    fi
    echo "MV_REVERIFY_OBS_RESTART: strih-obs.service restarted" ;;
  *"systemctl restart camera-box"* | *"systemctl restart dantesync"*)
    printf 'AV_MATRIX_RESTART_AT=1790000000\n'
    if [ -n "${FAKE_RESTART_FAIL:-}" ]; then echo AV_MATRIX_RESTART_FAILED; exit 1; fi
    echo AV_MATRIX_RESTART_OK ;;
  *"start strih-obs.service"* | *"systemctl start camera-box"* | *"systemctl start dantesync"*)
    echo "active=active" ;;
  *"journalctl -u camera-box"*)
    printf 'active=active\nstreaming=%s\n' "${FAKE_CAMBOX_STREAMING:-2}" ;;
  *"is-active strih-obs.service"*)
    printf 'active=%s\n' "${FAKE_STRIH_ACTIVE:-active}" ;;
  *) echo "fake sshpass: unexpected remote text" >&2; exit 3 ;;
esac
'''

FAKE_CURL = r'''#!/usr/bin/env bash
printf 'curl %s\n' "$*" >> "$FAKE_LOG"
case "$*" in
  *":8898/status"*)
    locked=true; [ -n "${FAKE_DANTE_UNLOCKED:-}" ] && locked=false
    printf '{"is_locked": %s, "mode": "LOCK", "gm_source_ip": "10.77.9.230", "ntp_step_storm": false, "updated_ts": %s}\n' "$locked" "$(date +%s)" ;;
  *) exit 7 ;;
esac
'''


def _write(path, text, mode=0o755):
    with open(path, "w") as f:
        f.write(text)
    os.chmod(path, mode)


@pytest.fixture
def rig(tmp_path):
    fake = tmp_path / "fake"
    obs_dir = tmp_path / "obs"
    bin_dir = tmp_path / "bin"
    probe = tmp_path / "probe"
    state = tmp_path / "state"
    for d in (fake, obs_dir, bin_dir, probe, state):
        d.mkdir()
    _write(fake / "fake_soak.py", FAKE_SOAK_PY)
    _write(fake / "av-soak.sh", FAKE_SOAK_SH)
    _write(obs_dir / "obs_phase2.py", FAKE_OBS)
    _write(bin_dir / "sshpass", FAKE_SSHPASS)
    _write(bin_dir / "curl", FAKE_CURL)
    _write(probe / "recording-verdict", "#!/bin/sh\nexit 0\n")
    exe = tmp_path / "recording-verdict.exe"
    exe.write_text("MZ")
    log = tmp_path / "fake.log"
    log.write_text("")
    run = tmp_path / "matrix"
    env = {k: v for k, v in os.environ.items()
           if k not in ("OBS_PASSWORD", "CAMBOX_OFFLINE_ACK", "AV_SOAK_SPREAD_COLUMNS")
           and not k.startswith("AV_MATRIX_")}
    env.update({
        "PATH": f"{bin_dir}:{env['PATH']}",
        "FAKE_LOG": str(log), "FAKE_STATE": str(state), "FAKE_DIR": str(fake),
        "FAKE_SCRIPTS": os.path.join(REPO, "scripts"), "FAKE_STREAM_HOST": STREAM,
        "FAKE_MATRIX_DIR": str(run),
        "AV_MATRIX_SOAK": str(fake / "av-soak.sh"),
        "AV_SOAK_OBS_DIR": str(obs_dir),
        "AV_SOAK_CAMS": "cam1 cam3",
        "AV_MATRIX_RUN_DIR": str(run),
        "AV_MATRIX_KINDS": "strih-obs cambox dantesync",
        "AV_MATRIX_SETTLE_SECS": "0", "AV_MATRIX_POLL_S": "1", "AV_MATRIX_KEEPALIVE_S": "1",
        "AV_MATRIX_HEALTHY_TIMEOUT_S": "20",
        "AV_SOAK_BROADCAST_RETRY_S": "0",
        "PROBE_BIN_DIR": str(probe), "WIN_VERDICT_EXE_LOCAL": str(exe),
        "RIG_LEASE_DIR": str(tmp_path / "lease"),
        "RIG_GRANDMASTER_IP": "10.77.9.230",
        "RIG_FLEET_ACK_FILE": str(tmp_path / "no-acks.txt"),
        "CAM_PW": "x", "STREAM_USER": "u", "STREAM_PW": "y", "STRIH_USER": "su", "STRIH_PW": "sp",
        "STRIH_HOST": STRIH, "STREAM_HOST": STREAM,
    })
    return env, {"log": log, "run": run, "lease": tmp_path / "lease", "state": state}


def _matrix(env, *args, timeout=180):
    return subprocess.run(["bash", MATRIX, *args], env=env, capture_output=True, text=True,
                          timeout=timeout)


def _events(log):
    """(kind, detail) per fake call, in order: ("soak", argv) / ("ssh", (host, text)) / ..."""
    out = []
    for line in log.read_text().splitlines():
        if line.startswith("soak "):
            out.append(("soak", json.loads(line[5:])))
        elif line.startswith("sshpass "):
            _, host, blob = line.split(" ", 2)
            out.append(("ssh", (host, base64.b64decode(blob).decode())))
        elif line.startswith("obs "):
            out.append(("obs", json.loads(line[4:])))
        elif line.startswith("curl "):
            out.append(("curl", line[5:]))
        elif line.startswith("soak-term"):
            out.append(("soak-term", line))
    return out


def _windows(log):
    return [d for k, d in _events(log) if k == "soak" and "--run" in d]


def _restart_kinds(log):
    kinds = []
    for k, d in _events(log):
        if k != "ssh":
            continue
        host, text = d
        if "systemctl --user restart strih-obs.service" in text:
            kinds.append(("strih-obs", host))
        elif "systemctl restart camera-box" in text:
            kinds.append(("cambox", host))
        elif "systemctl restart dantesync" in text:
            kinds.append(("dantesync", host))
    return kinds


def _steps(run):
    lines = (run / "matrix.tsv").read_text().splitlines()
    header = lines[0].split("\t")
    return [dict(zip(header, line.split("\t"))) for line in lines[1:]]


# --- --plan touches nothing ------------------------------------------------------------------------


def test_plan_is_the_default_and_touches_nothing(rig):
    env, p = rig
    r = _matrix(dict(env, AV_MATRIX_KINDS="strih-obs cambox dantesync stream-obs"))
    assert r.returncode == 0, r.stderr
    assert p["log"].read_text() == "", "plan mode must not call OBS, ssh, curl or a window"
    assert not p["lease"].exists() and not p["run"].exists()
    out = r.stdout
    for needle in ("rig_lease_acquire", "camera-box-av-restart-matrix", "stray_session_check_assert",
                   "--run --hours 0", "--lease-run-id av-matrix-", "w-00-baseline",
                   "systemctl --user restart strih-obs.service", "systemctl restart camera-box",
                   "systemctl restart dantesync", "journalctl -u camera-box",
                   ":8898/status", "dantesync_clock_decision.py analyze",
                   "launch-obs-genlock.sh --box stream --force", "win-stream-snv",
                   "confirm-stream-obs-r1", "av_tolerance_ms=", "x 3 repeats",
                   f"cambox = cam1 {CAM1}", f"dantesync = cam1 {CAM1}",
                   "av_restart_matrix_decision.py report"):
        assert needle in out, needle


def test_plan_never_names_a_reboot_a_writer_or_the_production_scene(rig):
    env, _ = rig
    r = _matrix(dict(env, AV_MATRIX_KINDS="strih-obs cambox dantesync stream-obs"))
    assert r.returncode == 0 and "NEVER:" in r.stdout, r.stderr
    body = r.stdout.split("NEVER:")[0].lower()
    for banned in ("reboot", "shutdown", "restart-computer", "av_sync_calibrate", "qr_align",
                   "set-ndi-mapping", "schtasks /run", "'pro'", " pro\n"):
        assert banned not in body, banned


def test_the_lib_builds_no_reboot_and_ends_every_remote_text_with_a_separator():
    for kind in ("strih-obs", "cambox", "dantesync"):
        for fn in ("av_matrix_restart_remote_cmd", "av_matrix_ensure_running_remote_cmd"):
            txt = subprocess.run(["bash", "-c", f'. "{LIB}"; . "{REPO}/scripts/lib/mv-reverify-escalate.sh"; {fn} {kind}'],
                                 capture_output=True, text=True, timeout=30)
            assert txt.returncode == 0, txt.stderr
            assert "reboot" not in txt.stdout.lower() and "shutdown" not in txt.stdout.lower()
            assert txt.stdout.strip(), (fn, kind)


@pytest.mark.parametrize("args,needle", [
    (("--kinds", "reboot"), "unknown restart kind"),
    (("--repeats", "x"), "--repeats"),
    (("--repeats", "0"), "--repeats"),
    (("--cambox", "cam2"), "cam2"),
    (("--cambox", "cam99"), "unknown camera"),
    (("--dantesync-node", "strih-lx"), "camera"),
    (("--settle-secs", "-1"), "--settle-secs"),
    (("--hours", "1"), "unknown argument"),
])
def test_usage_errors_are_exit_3_and_touch_nothing(rig, args, needle):
    env, p = rig
    r = _matrix(env, *args)
    assert r.returncode == 3, (args, r.stdout, r.stderr)
    assert needle in r.stderr
    assert p["log"].read_text() == ""


# --- --run -------------------------------------------------------------------------------------------


def test_run_measures_a_baseline_then_every_kind_three_times_under_one_lease(rig):
    env, p = rig
    r = _matrix(env, "--run")
    assert r.returncode == 0, r.stdout + r.stderr
    wins = _windows(p["log"])
    assert len(wins) == 10, "1 baseline + 3 kinds x 3 repeats"
    for w in wins:
        assert w[:3] == ["--run", "--hours", "0"], w
        assert w[w.index("--lease-run-id") + 1].startswith("av-matrix-")
    assert wins[0][wins[0].index("--run-dir") + 1].endswith("w-00-baseline")
    assert len({w[w.index("--run-dir") + 1] for w in wins}) == 10, "one run dir per window"
    # the order: baseline window, then per kind x repeat: restart, (health), window
    seq = []
    for k, d in _events(p["log"]):
        if k == "soak" and "--run" in d:
            seq.append("window")
        elif k == "ssh" and ("systemctl --user restart strih-obs" in d[1]
                             or "systemctl restart camera-box" in d[1]
                             or "systemctl restart dantesync" in d[1]):
            seq.append("restart")
    assert seq == ["window"] + ["restart", "window"] * 9
    assert _restart_kinds(p["log"]) == [("strih-obs", STRIH)] * 3 + [("cambox", CAM1)] * 3 \
        + [("dantesync", CAM1)] * 3
    steps = _steps(p["run"])
    assert [s["kind"] for s in steps] == ["baseline"] + ["strih-obs"] * 3 + ["cambox"] * 3 \
        + ["dantesync"] * 3
    assert all(s["outcome"] == "measured" for s in steps)
    assert all(s["healthy"] == "1" for s in steps[1:])
    assert "VERDICT: PASS" in r.stdout
    assert (p["run"] / "report.txt").exists() and (p["run"] / "report.json").exists()
    assert not p["lease"].exists(), "the matrix releases its own lease"


def test_nothing_ever_sends_a_reboot_or_touches_the_stream_box_over_ssh(rig):
    env, p = rig
    r = _matrix(dict(env, AV_MATRIX_KINDS="strih-obs cambox dantesync"), "--run")
    assert r.returncode == 0, r.stdout + r.stderr
    for k, d in _events(p["log"]):
        if k == "ssh":
            assert d[0] != STREAM
            assert "reboot" not in d[1].lower() and "shutdown" not in d[1].lower()


def test_the_health_checks_gate_each_window(rig):
    env, p = rig
    r = _matrix(dict(env, AV_MATRIX_KINDS="strih-obs cambox dantesync", AV_MATRIX_REPEATS="1"), "--run")
    assert r.returncode == 0, r.stdout + r.stderr
    ev = _events(p["log"])
    texts = [d[1] for k, d in ev if k == "ssh"]
    assert any("is-active strih-obs.service" in t for t in texts)
    assert any("journalctl -u camera-box" in t and "--since @1790000000" in t for t in texts), \
        "the cambox Streaming: line is read since the box's OWN restart time"
    assert any(k == "curl" and f"{CAM1}:8898/status" in d for k, d in ev)
    assert any(k == "obs" and d[:1] == ["program-scene"] and STRIH in d for k, d in ev)


def test_cleanup_leaves_every_restarted_component_running(rig):
    env, p = rig
    r = _matrix(env, "--run")
    assert r.returncode == 0, r.stdout + r.stderr
    texts = [d[1] for k, d in _events(p["log"]) if k == "ssh"]
    assert any("systemctl --user start strih-obs.service" in t for t in texts)
    assert any("systemctl start camera-box" in t for t in texts)
    assert any("systemctl start dantesync" in t for t in texts)


def test_a_window_outside_the_bounds_fails_its_kind_and_the_later_kinds_still_run(rig):
    env, p = rig
    # window 3 = strih-obs r2 (1 = baseline, 2 = strih-obs r1)
    r = _matrix(dict(env, FAKE_SOAK_BAD_WINDOWS="3"), "--run")
    assert r.returncode == 1, r.stdout + r.stderr
    assert len(_windows(p["log"])) == 10
    rep = json.loads((p["run"] / "report.json").read_text())
    assert rep["kinds"]["strih-obs"]["verdict"] == "FAIL"
    assert rep["kinds"]["cambox"]["verdict"] == "PASS"
    assert rep["verdict"] == "FAIL"


def test_a_component_that_never_comes_back_stops_the_matrix_and_is_left_running(rig):
    env, p = rig
    r = _matrix(dict(env, AV_MATRIX_KINDS="cambox dantesync", FAKE_CAMBOX_STREAMING="0",
                     AV_MATRIX_HEALTHY_TIMEOUT_S="2"), "--run")
    assert r.returncode == 1, r.stdout + r.stderr
    assert _restart_kinds(p["log"]) == [("cambox", CAM1)], "the matrix stops at the first dead component"
    assert len(_windows(p["log"])) == 1, "only the baseline was measured"
    steps = _steps(p["run"])
    assert steps[-1]["outcome"] == "not_healthy" and steps[-1]["healthy"] == "0"
    texts = [d[1] for k, d in _events(p["log"]) if k == "ssh"]
    assert any("systemctl start camera-box" in t for t in texts)
    assert not p["lease"].exists()


def test_a_failing_baseline_stops_before_any_restart_unless_keep_going(rig):
    env, p = rig
    r = _matrix(dict(env, FAKE_SOAK_BAD_WINDOWS="1"), "--run")
    assert r.returncode == 1, r.stdout + r.stderr
    assert _restart_kinds(p["log"]) == [], "nothing is restarted on a rig that already fails"
    assert "baseline" in r.stdout and "--keep-going" in r.stdout
    assert not p["lease"].exists()


def test_keep_going_runs_the_restarts_after_a_failing_baseline(rig, tmp_path):
    env, p = rig
    r = _matrix(dict(env, FAKE_SOAK_BAD_WINDOWS="1", AV_MATRIX_KINDS="cambox",
                     AV_MATRIX_REPEATS="1"), "--run", "--keep-going")
    assert r.returncode == 1, r.stdout + r.stderr
    assert _restart_kinds(p["log"]) == [("cambox", CAM1)]


def test_a_restart_that_fails_is_graded_fail_and_stops(rig):
    env, p = rig
    r = _matrix(dict(env, AV_MATRIX_KINDS="cambox", FAKE_RESTART_FAIL="1"), "--run")
    assert r.returncode == 1, r.stdout + r.stderr
    assert _steps(p["run"])[-1]["outcome"] == "restart_failed"
    assert len(_windows(p["log"])) == 1


def test_a_strih_without_its_unit_is_not_restarted_and_is_unknown(rig):
    env, p = rig
    r = _matrix(dict(env, AV_MATRIX_KINDS="strih-obs", FAKE_STRIH_NO_UNIT="1"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert _steps(p["run"])[-1]["outcome"] == "not_performed"


def test_a_broadcast_before_a_restart_stops_the_matrix_without_restarting(rig):
    env, p = rig
    r = _matrix(dict(env, FAKE_LIVE_AFTER_WINDOWS="1"), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    assert _restart_kinds(p["log"]) == []
    assert "broadcast" in r.stdout + r.stderr
    assert not p["lease"].exists()


def test_a_live_foreign_lease_is_refused_and_left_alone(rig):
    env, p = rig
    p["lease"].mkdir()
    (p["lease"] / "holder.json").write_text(json.dumps(
        {"repo": "camera-box-av-soak", "run_id": "av-soak-1", "run_url": "", "job": "av-soak",
         "acquired_at": "2026-09-28T00:00:00Z", "expected_release_at": "2099-01-01T00:00:00Z"}))
    (p["lease"] / "heartbeat").write_text("")
    r = _matrix(env, "--run")
    assert r.returncode == 4, r.stdout + r.stderr
    assert "lease is held" in r.stderr
    assert json.loads((p["lease"] / "holder.json").read_text())["run_id"] == "av-soak-1"
    assert p["log"].read_text() == ""


def test_run_without_credentials_or_binaries_is_refused_before_the_lease(rig):
    env, p = rig
    for drop in ("CAM_PW", "STRIH_PW", "PROBE_BIN_DIR"):
        e = dict(env)
        e.pop(drop)
        r = _matrix(e, "--run")
        assert r.returncode == 4, (drop, r.stdout, r.stderr)
        assert not p["lease"].exists() and p["log"].read_text() == ""


def test_a_used_run_dir_is_refused(rig):
    env, p = rig
    p["run"].mkdir()
    (p["run"] / "matrix.tsv").write_text("step\n")
    r = _matrix(env, "--run")
    assert r.returncode == 4
    assert "already holds a run" in r.stderr


def test_a_refused_window_after_a_restart_stops_the_matrix_unknown(rig):
    env, p = rig
    r = _matrix(dict(env, FAKE_SOAK_RC_MAP="2:4"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    steps = _steps(p["run"])
    assert steps[-1]["outcome"] == "window_refused"
    assert len(_restart_kinds(p["log"])) == 1
    assert not p["lease"].exists()


def test_a_window_that_may_have_left_a_recording_keeps_the_lease_for_stop_leftovers(rig):
    env, p = rig
    r = _matrix(dict(env, FAKE_SOAK_RC_MAP="2:5", FAKE_SOAK_STUCK_WINDOW="2"), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    assert p["lease"].exists(), "no E2E may start over a recording a window may have left"
    assert "--stop-leftovers" in r.stdout + r.stderr
    r = _matrix(env, "--stop-leftovers", str(p["run"]))
    assert r.returncode == 0, r.stdout + r.stderr
    left = [d for k, d in _events(p["log"]) if k == "soak" and "--stop-leftovers" in d]
    assert len(left) == 1 and left[0][1].endswith("-r1")
    assert not p["lease"].exists(), "nothing is left, so the matrix's own lease is released"


def test_stop_leftovers_keeps_the_lease_while_a_leftover_is_kept(rig):
    env, p = rig
    r = _matrix(dict(env, FAKE_SOAK_RC_MAP="2:5", FAKE_SOAK_STUCK_WINDOW="2"), "--run")
    assert r.returncode == 5
    r = _matrix(dict(env, FAKE_LEFTOVERS_RC="5"), "--stop-leftovers", str(p["run"]))
    assert r.returncode == 5, r.stdout + r.stderr
    assert p["lease"].exists()


def test_stop_leftovers_refuses_while_the_matrix_still_runs(rig):
    env, p = rig
    p["run"].mkdir()
    live = subprocess.Popen(["bash", "-c", "sleep 30; true", "av-restart-matrix.sh"])
    try:
        (p["run"] / "pid").write_text(f"{live.pid}\n")
        r = _matrix(env, "--stop-leftovers", str(p["run"]))
    finally:
        live.kill()
        live.wait()
    assert r.returncode == 4, r.stdout + r.stderr
    assert "still running" in r.stderr


def test_sigterm_during_a_window_waits_for_the_windows_own_cleanup(rig):
    env, p = rig
    proc = subprocess.Popen(["bash", MATRIX, "--run"], env=dict(env, FAKE_SOAK_SLEEP_WINDOW="1"),
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    deadline = time.time() + 60
    while time.time() < deadline and not _windows(p["log"]):
        time.sleep(0.2)
    time.sleep(0.5)
    proc.send_signal(signal.SIGTERM)
    out, err = proc.communicate(timeout=60)
    assert proc.returncode == 5, out + err
    assert any(k == "soak-term" for k, _ in _events(p["log"])), "the running window got SIGTERM"
    assert _restart_kinds(p["log"]) == []
    assert not p["lease"].exists()


def test_the_stop_file_ends_the_run_with_the_report(rig):
    env, p = rig
    r = _matrix(dict(env, FAKE_SOAK_TOUCH_STOP_WINDOW="2"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert len(_windows(p["log"])) == 2
    assert "VERDICT: UNKNOWN" in r.stdout
    assert not p["lease"].exists()


def test_the_lease_is_kept_alive_while_a_window_runs(rig):
    env, p = rig
    proc = subprocess.Popen(["bash", MATRIX, "--run"],
                            env=dict(env, FAKE_SOAK_SLEEP_WINDOW="1", AV_MATRIX_KINDS="cambox",
                                     AV_MATRIX_REPEATS="1"),
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        deadline = time.time() + 60
        while time.time() < deadline and not _windows(p["log"]):
            time.sleep(0.2)
        hb = p["lease"] / "heartbeat"
        first = hb.stat().st_mtime
        time.sleep(3.5)
        assert hb.stat().st_mtime > first, "the heartbeat is bumped while the window runs"
    finally:
        proc.send_signal(signal.SIGTERM)
        proc.communicate(timeout=60)


# --- the stream OBS kind is a SUPERVISOR step ------------------------------------------------------


def _confirm_when_asked(run, repeat, delay=0.5):
    def go():
        step = run / f"supervisor-step-stream-obs-r{repeat}.txt"
        deadline = time.time() + 60
        while time.time() < deadline and not step.exists():
            time.sleep(0.2)
        time.sleep(delay)
        (run / f"confirm-stream-obs-r{repeat}").write_text(f"{int(time.time())}\n")
    t = threading.Thread(target=go, daemon=True)
    t.start()
    return t


def test_the_stream_obs_kind_waits_for_the_supervisor_and_is_graded(rig):
    env, p = rig
    e = dict(env, AV_MATRIX_KINDS="stream-obs", AV_MATRIX_REPEATS="1", FAKE_STREAM_WS_DOWN_READS="1")
    t = _confirm_when_asked(p["run"], 1)
    r = _matrix(e, "--run")
    t.join(timeout=5)
    assert r.returncode == 0, r.stdout + r.stderr
    step_txt = (p["run"] / "supervisor-step-stream-obs-r1.txt").read_text()
    assert "launch-obs-genlock.sh --box stream --force" in step_txt
    assert "win-stream-snv" in step_txt and "confirm-stream-obs-r1" in step_txt
    assert "SUPERVISOR STEP" in r.stdout
    assert not [d for k, d in _events(p["log"]) if k == "ssh"], "the stream box is never driven over ssh"
    steps = _steps(p["run"])
    assert steps[-1]["kind"] == "stream-obs" and steps[-1]["outcome"] == "measured"
    assert len(_windows(p["log"])) == 2


def test_an_unconfirmed_stream_restart_is_not_performed_and_unknown(rig):
    env, p = rig
    r = _matrix(dict(env, AV_MATRIX_KINDS="stream-obs", AV_MATRIX_REPEATS="1",
                     AV_MATRIX_SUPERVISOR_TIMEOUT_S="2"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert _steps(p["run"])[-1]["outcome"] == "not_performed"
    assert "stream OBS" in r.stdout + r.stderr


# --- --report --------------------------------------------------------------------------------------


def test_report_regrades_a_finished_run(rig):
    env, p = rig
    assert _matrix(dict(env, AV_MATRIX_KINDS="cambox", AV_MATRIX_REPEATS="1"), "--run").returncode == 0
    r = _matrix(env, "--report", str(p["run"]))
    assert r.returncode == 0, r.stdout + r.stderr
    assert "AV-RESTART-MATRIX" in r.stdout


# --- static checks -----------------------------------------------------------------------------------


def test_the_new_scripts_set_strict_mode_early_and_parse():
    head = open(MATRIX).read().splitlines()[:15]
    assert "set -euo pipefail" in head
    for path in (MATRIX, LIB):
        assert subprocess.run(["bash", "-n", path]).returncode == 0
    assert re.search(r"^# airuleset:script-ok", open(LIB).read(), re.M)
