"""issue 1367 -- the 8 h stream-output A/V soak orchestrator (scripts/av-soak.sh) + its pure lib
(scripts/lib/av-soak.sh), driven end to end with FAKES (Tier-0: no rig, no cargo, no network).

Fakes on PATH / behind the documented seams:
  - obs_phase2.py + obs_burn_filter.py in AV_SOAK_OBS_DIR (log every call; answer rig-busy-check,
    program-scene, record start/stop/status, switch, burn check/add/remove),
  - sshpass (cam2 painter probe + marker-log tail, the stream-box New-Item/scp/Stop-Process) and
    curl (the record-volume free space) on PATH,
  - AV_SOAK_STRIH_DECODE / AV_SOAK_STREAM_DECODE (write the partial the wrappers would pull back),
  - PROBE_BIN_DIR/recording-verdict (writes a merged verdict JSON, exits 1 like a failing gate),
  - RIG_LEASE_DIR + CAMERA_BOX_RIG_HEARTBEAT in a tmp dir -- the REAL dev1 lease and heartbeat are
    never touched (an E2E may hold the real lease while this runs).
"""
import base64
import json
import re
import os
import signal
import subprocess
import sys
import textwrap
import time

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
SOAK = os.path.join(REPO, "scripts", "av-soak.sh")
LIB = os.path.join(REPO, "scripts", "lib", "av-soak.sh")
STRIH = "10.77.9.202"
STREAM = "10.77.9.204"

sys.path.insert(0, os.path.join(REPO, "scripts"))
import bundle_state_gather as bsg  # noqa: E402

FAKE_OBS = r'''#!/usr/bin/env python3
import json, os, sys, time
a = sys.argv[1:]
state = os.environ["FAKE_STATE"]
with open(os.environ["FAKE_LOG"], "a") as f:
    f.write("obs " + json.dumps(a) + "\n")
def arg(name, default=""):
    return a[a.index(name) + 1] if name in a else default
def bump(name):
    p = os.path.join(state, "count-" + name)
    n = int(open(p).read()) + 1 if os.path.exists(p) else 1
    open(p, "w").write(str(n))
    return n
def count(name):
    p = os.path.join(state, "count-" + name)
    return int(open(p).read()) if os.path.exists(p) else 0
def reached(env_name, n):
    k = int(os.environ.get(env_name, "0") or 0)
    return k > 0 and n >= k
cmd = a[0]
host = arg("--host")
if cmd == "rig-busy-check":
    n = bump("busy")
    live = (n > int(os.environ.get("FAKE_LIVE_AFTER", "0") or 0) > 0
            or reached("FAKE_LIVE_AFTER_STOPS", count("stops"))
            or reached("FAKE_LIVE_AFTER_SWITCHES", count("switches"))
            or reached("FAKE_LIVE_AFTER_STARTS", count("starts"))
            or reached("FAKE_LIVE_AFTER_PROGRAM_READS", count("stream-program")))
    unreadable = (reached("FAKE_UNREADABLE_AFTER_SWITCHES", count("switches"))
                  or reached("FAKE_UNREADABLE_AFTER_PROGRAM_READS", count("stream-program")))
    times = int(os.environ.get("FAKE_UNREADABLE_TIMES", "0") or 0)
    if unreadable and times and count("unreadable") >= times:
        unreadable = False
    if unreadable:
        bump("unreadable")
        print(json.dumps({"busy": None, "reasons": ["stream unreachable"],
                          "diagnostics": [{"host": "strih", "streaming": False, "recording": False,
                                           "recordTimecode": None}]}))
        sys.exit(3)
    busy = os.environ.get("FAKE_BUSY") == "1"
    # a recording the soak did not start (the Companion orphan auto-record, a rehearsal): the real
    # rig-busy-check reports busy on recording OR streaming, the soak's broadcast read only on streaming
    foreign = reached("FAKE_FOREIGN_RECORDING_AFTER_PROGRAM_READS", count("stream-program"))
    diags = []
    for box in ("strih", "stream"):
        if os.environ.get("FAKE_BUSY_UNREADABLE") == box:
            continue
        streaming = busy or os.environ.get("FAKE_STREAMING_BOX") == box or (live and box == "stream")
        flag = os.path.join(state, "recording-" + box)
        recording = os.path.exists(flag) or (foreign and box == "strih")
        tc = None
        if recording:
            if os.path.exists(flag):
                txt = open(flag).read().strip()
                age = time.time() - float(txt) if txt else 0.0
            else:
                age = 600.0  # the foreign recording: running for 10 min already
            age = float(os.environ.get("FAKE_RECORD_AGE_" + box, age))
            ms = int(round(age * 1000))
            tc = "%02d:%02d:%02d.%03d" % (ms // 3600000, ms // 60000 % 60, ms // 1000 % 60, ms % 1000)
        diags.append({"host": box, "streaming": streaming, "recording": recording,
                      "recordTimecode": tc})
    if os.environ.get("FAKE_BUSY_UNREADABLE"):
        print(json.dumps({"busy": None, "reasons": ["unreachable"], "diagnostics": diags}))
        sys.exit(3)
    print(json.dumps({"busy": busy or live or foreign, "diagnostics": diags}))
elif cmd == "stream-detail":
    pass
elif cmd == "program-scene":
    if host == os.environ["FAKE_STREAM_HOST"]:
        n = bump("stream-program")
        after = int(os.environ.get("FAKE_STREAM_PROGRAM_DRIFT_AFTER", "0") or 0)
        drifted = after and n > after
        blank = int(os.environ.get("FAKE_STREAM_PROGRAM_BLANK_AFTER", "0") or 0)
        if blank and n > blank:
            sys.exit("fake: program-scene read failed")
        print("Other scene" if drifted else os.environ.get("FAKE_STREAM_PROGRAM", "Development"))
    else:
        print(os.environ.get("FAKE_STRIH_PROGRAM", "Cam 1"))
elif cmd == "record":
    act = arg("--action")
    box = "stream" if host == os.environ["FAKE_STREAM_HOST"] else "strih"
    flag = os.path.join(state, "recording-" + box)
    if act == "start":
        bump("starts")
        with open(flag, "w") as fh:
            fh.write(str(time.time()))
        if os.environ.get("FAKE_START_FAIL") == box:
            sys.exit("fake: StartRecord verify failed")
    elif act == "stop":
        bump("stops")
        never = os.environ.get("FAKE_STOP_NEVER") == box
        if not never and (os.environ.get("FAKE_STOP_STICKY") != box or bump("stop-" + box) > 1):
            if os.path.exists(flag):
                os.remove(flag)
        if os.environ.get("FAKE_STOP_LAGGY") == box:
            # a real OBS stops asynchronously: the first status read after StopRecord still says active
            with open(os.path.join(state, "lag-" + box), "w") as fh:
                fh.write("1")
        if os.environ.get("FAKE_TOUCH_STOP"):
            open(os.environ["FAKE_TOUCH_STOP"], "w").close()
        print("/srv/_REC/2026-09-27 21-00-00.mkv" if box == "strih" else "C:/_REC/2026-09-27 21-00-00.mp4")
    elif act == "status":
        lag = os.path.join(state, "lag-" + box)
        if os.path.exists(lag) and open(lag).read().strip() not in ("", "0"):
            with open(lag, "w") as fh:
                fh.write("0")  # one lagging read only
            print("active=True path=")
        else:
            print(f"active={os.path.exists(flag)} path=x")
elif cmd == "switch":
    scene = arg("--program-scene")
    bump("switches")
    if os.environ.get("FAKE_SWITCH_FAIL") == scene:
        sys.exit("fake: scene renders black")
    if os.environ.get("FAKE_RESTORE_DELAY") and "--prod-floor" in a:
        time.sleep(float(os.environ["FAKE_RESTORE_DELAY"]))
    if os.environ.get("FAKE_RESTORE_FAIL") and "--prod-floor" in a:
        sys.exit("fake: the restored scene is dim (non-black check)")
    print(time.time_ns())
else:
    sys.exit(f"fake obs_phase2: unexpected {a}")
'''

FAKE_BURN = r'''#!/usr/bin/env python3
import json, os, sys
a = sys.argv[1:]
with open(os.environ["FAKE_LOG"], "a") as f:
    f.write("burn " + json.dumps(a) + "\n")
act = a[0]
host = a[a.index("--host") + 1]
inp = a[a.index("--input") + 1]
state = os.path.join(os.environ["FAKE_STATE"], "burn-" + (host + "_" + inp).replace(" ", "_"))
if act == "check":
    on = os.path.exists(state) or os.environ.get("FAKE_BURNS_ON") == "1"
    print(f"[burn] burn_on={on} genlock_burn={on} filter_on_input=True filter_enabled=True kind_registered=True input='{inp}'")
elif act == "add":
    open(state, "w").close()
    print(f"[burn] ON genlock_burn=true on '{inp}'")
elif act == "remove":
    if os.path.exists(state):
        os.remove(state)
    print(f"[burn] OFF genlock_burn=false on '{inp}'")
'''

FAKE_SSHPASS = r'''#!/usr/bin/env bash
set -euo pipefail
# One append per line: the strih and stream decodes run concurrently, and three separate appends
# interleave between the two processes on a slow runner (the line loses its arguments).
line="$(printf 'sshpass'; printf ' %q' "$@")"; printf '%s\n' "$line" >> "$FAKE_LOG"
case "$*" in
  *"systemctl is-active cam2-painter"*)
    n=$(( $(cat "$FAKE_STATE/count-probe" 2>/dev/null || echo 0) + 1 ))
    echo "$n" > "$FAKE_STATE/count-probe"
    active="${FAKE_PAINTER_ACTIVE:-active}"
    if [ -n "${FAKE_PAINTER_FAIL_AFTER:-}" ] && [ "$n" -gt "$FAKE_PAINTER_FAIL_AFTER" ]; then active=inactive; fi
    rid=4242; [ -n "${FAKE_PAINTER_NO_RUN_ID:-}" ] && rid=""
    m2=14
    if [ -n "${FAKE_PAINTER_STUCK_AFTER:-}" ] && [ "$n" -gt "$FAKE_PAINTER_STUCK_AFTER" ]; then m2=10; fi
    if [ -n "${FAKE_PAINTER_UNREADABLE_AFTER:-}" ] && [ "$n" -gt "$FAKE_PAINTER_UNREADABLE_AFTER" ]; then exit 255; fi
    printf 'active=%s\nrun_id=%s\nmarkers=10\nmarkers2=%s\n' "$active" "$rid" "$m2" ;;
  *"tail -n +2"* | *"grep '^[0-9]'"*)
    # the marker-log snapshot (whatever its head/tail shape): the emitter's real format
    printf '# qpsk-params sr=48000 carrier=442 c=1 q=2 vr=60/1\nindex,frame_id,emit_ts_ns\n1,100,5\n2,130,6\n' ;;
  *) : ;;
esac
'''

FAKE_CURL = r'''#!/usr/bin/env bash
printf 'curl %s\n' "$*" >> "$FAKE_LOG"
[ -n "${FAKE_CURL_FAIL:-}" ] && exit 7
printf '{"free_bytes": %s}\n' "${FAKE_FREE_BYTES:-500000000000}"
'''

FAKE_DECODE = r'''#!/usr/bin/env bash
set -euo pipefail
# One append per line (see FAKE_SSHPASS): the two decodes run concurrently.
line="$(printf 'decode %s' "$(basename "$0")"; printf ' %q' "$@")"; printf '%s\n' "$line" >> "$FAKE_LOG"
ldir=""; out=""; prev=""
for a in "$@"; do
  [ "$prev" = "--local-out-dir" ] && ldir="$a"
  [ "$prev" = "--out" ] && out="$a"
  prev="$a"
done
case "$(basename "$0")" in
  strih-*) [ -n "${FAKE_DECODE_SLEEP_STRIH:-}" ] && sleep "$FAKE_DECODE_SLEEP_STRIH" ;;
  stream-*) [ -n "${FAKE_DECODE_SLEEP_STREAM:-}" ] && sleep "$FAKE_DECODE_SLEEP_STREAM" ;;
esac
name="${out##*/}"; name="${name##*\\}"
printf '{}\n' > "$ldir/$name"
'''

FAKE_VERDICT = r'''#!/usr/bin/env python3
import json, os, sys
a = sys.argv[1:]
with open(os.environ["FAKE_LOG"], "a") as f:
    f.write("verdict " + json.dumps(a) + "\n")
out = a[a.index("--json") + 1]
cams = os.environ.get("FAKE_CAMS", "cam1 cam3").split()
v = {"all_cambox_av_sync": {"expected_ms": 0.0, "judged_cameras": len(cams)},
     "all_cambox_latency": {"cross_camera_spread_ms": None},
     "all_cambox_delivery_latency": {"cross_camera_spread_ms": None},
     "all_cambox_continuity": {"segments": []},
     "full_chain": {"loss": {"strih": {"zero_loss": True, "real_drops": 0},
                             "stream": {"zero_loss": True, "real_drops": 0}}}}
for i, c in enumerate(cams):
    v["all_cambox_av_sync"][c] = {"verdict": "measured", "av_offset_ms": 3.0 + i, "gate_pass": True}
    v["all_cambox_continuity"]["segments"].append(
        {"cambox": c.upper(), "pass": True, "relaxed_pass": True, "copies": 0, "gaps": 0,
         "undecodable": 0, "frames": 60})
json.dump(v, open(out, "w"))
sys.exit(1)
'''

def _write(path, text, mode=0o755):
    with open(path, "w") as f:
        f.write(text)
    os.chmod(path, mode)


@pytest.fixture
def rig(tmp_path):
    """A fully faked rig; returns (env, paths)."""
    obs_dir = tmp_path / "obs"
    bin_dir = tmp_path / "bin"
    probe = tmp_path / "probe"
    state = tmp_path / "state"
    for d in (obs_dir, bin_dir, probe, state):
        d.mkdir()
    _write(obs_dir / "obs_phase2.py", FAKE_OBS)
    _write(obs_dir / "obs_burn_filter.py", FAKE_BURN)
    _write(bin_dir / "sshpass", FAKE_SSHPASS)
    _write(bin_dir / "curl", FAKE_CURL)
    _write(tmp_path / "strih-decode.sh", FAKE_DECODE)
    _write(tmp_path / "stream-decode.sh", FAKE_DECODE)
    _write(probe / "recording-verdict", FAKE_VERDICT)
    exe = tmp_path / "recording-verdict.exe"
    exe.write_text("MZ")
    log = tmp_path / "fake.log"
    log.write_text("")
    env = {k: v for k, v in os.environ.items()
           if k not in ("OBS_PASSWORD", "CAMBOX_OFFLINE_ACK", "STREAM_PROG_SCENE",
                        "AV_SOAK_SPREAD_COLUMNS")}
    env.update({
        "PATH": f"{bin_dir}:{env['PATH']}",
        "FAKE_LOG": str(log), "FAKE_STATE": str(state), "FAKE_STREAM_HOST": STREAM,
        "FAKE_CAMS": "cam1 cam3",
        "AV_SOAK_OBS_DIR": str(obs_dir),
        "AV_SOAK_STRIH_DECODE": str(tmp_path / "strih-decode.sh"),
        "AV_SOAK_STREAM_DECODE": str(tmp_path / "stream-decode.sh"),
        "PROBE_BIN_DIR": str(probe), "WIN_VERDICT_EXE_LOCAL": str(exe),
        "AV_SOAK_CAMS": "cam1 cam3", "AV_SOAK_SEGMENT_SECS": "1", "AV_SOAK_MIN_SEGMENT_SECS": "0",
        "AV_SOAK_HOURS": "0", "AV_SOAK_RUN_DIR": str(tmp_path / "run"),
        "RIG_LEASE_DIR": str(tmp_path / "lease"),
        "CAMERA_BOX_RIG_HEARTBEAT": str(tmp_path / "heartbeat"),
        "RIG_FLEET_ACK_FILE": str(tmp_path / "no-acks.txt"),
        "CAM_PW": "x", "STREAM_USER": "u", "STREAM_PW": "y", "STRIH_USER": "su", "STRIH_PW": "sp",
        "STRIH_HOST": STRIH, "STREAM_HOST": STREAM,
        "E2E_ONBOX_DECODE_PRIORITY": "BelowNormal",
        "AV_SOAK_BROADCAST_RETRY_S": "0",
    })
    return env, {"log": log, "run": tmp_path / "run", "lease": tmp_path / "lease",
                 "hb": tmp_path / "heartbeat", "state": state}


def _soak(env, *args, timeout=120):
    return subprocess.run(["bash", SOAK, *args], env=env, capture_output=True, text=True,
                          timeout=timeout)


def _calls(log, kind):
    out = []
    for line in log.read_text().splitlines():
        if line.startswith(kind + " "):
            rest = line[len(kind) + 1:]
            out.append(json.loads(rest) if rest.startswith("[") else rest)
    return out


def _host(c):
    return c[c.index("--host") + 1]


def _csv_rows(run):
    lines = (run / "soak.csv").read_text().splitlines()
    header = lines[0].split(",")
    return [dict(zip(header, line.split(","))) for line in lines[1:]]


def _ps_commands(log):
    """The PowerShell text of every win_ssh_run -EncodedCommand the fake sshpass saw."""
    out = []
    for line in log.read_text().splitlines():
        if line.startswith("sshpass") and "-EncodedCommand" in line:
            # the fake logs its argv with printf %q, so the one "powershell ... -EncodedCommand B"
            # argument arrives with its spaces backslash-escaped
            rest = line.split("-EncodedCommand", 1)[1].replace("\\ ", " ").strip()
            blob = rest.split()[0].strip("'\"")
            out.append(base64.b64decode(blob).decode("utf-16-le"))
    return out


QUICK_TWO_WINDOWS = {"AV_SOAK_SLOT_SECS": "12", "AV_SOAK_HOURS": "0.0033334",
                     "AV_SOAK_OVERHEAD_S": "0", "AV_SOAK_MERGE_TIMEOUT_S": "3",
                     "AV_SOAK_DECODE_TIMEOUT_S": "3", "AV_SOAK_MIN_DECODE_S": "0"}


def _wait_for_sweep(p, timeout=60):
    """True once the slot's sweep has started: its first cut AFTER StartRecord. The first strih
    `switch` of a slot is the cut to the first sweep scene BEFORE StartRecord (issue 1367), so a
    signal sent on that one could land before any recording exists."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        seen_start = False
        for line in p["log"].read_text().splitlines():
            if line.startswith("obs ") and '"record"' in line and '"start"' in line:
                seen_start = True
            elif seen_start and line.startswith("obs ") and '"switch"' in line:
                return True
        time.sleep(0.2)
    return False


def _strih_program_at_strih_starts(obs, snapshot):
    """Replay the call log: the strih program scene at every strih StartRecord (the last strih
    cut before it, else the setup snapshot) and the scene of the first strih cut after each."""
    program, at_start, first_cut, started = snapshot, [], [], False
    for c in obs:
        if c[:1] == ["switch"] and _host(c) == STRIH:
            scene = c[c.index("--program-scene") + 1]
            if started:
                first_cut.append(scene)
                started = False
            program = scene
        elif c[:1] == ["record"] and "start" in c and _host(c) == STRIH:
            at_start.append(program)
            started = True
    return at_start, first_cut


def _assert_rig_restored(p):
    """Nothing the soak changed is left changed."""
    assert not [f for f in os.listdir(p["state"]) if f.startswith(("recording-", "burn-"))], \
        "a recording is still running or a burn the soak turned on is still on"
    assert not p["lease"].exists(), "the lease is released"
    assert not p["hb"].exists(), "the heartbeat is cleared"


# --- --plan touches nothing -----------------------------------------------------------------------


def test_plan_is_the_default_and_touches_nothing(rig):
    env, p = rig
    r = _soak(env)
    assert r.returncode == 0, r.stderr
    assert p["log"].read_text() == "", "plan mode must not call OBS, ssh, curl or a decode"
    assert not p["lease"].exists() and not p["hb"].exists() and not p["run"].exists()
    out = r.stdout
    for needle in ("rig_lease_acquire", "stray_session_check_assert",
                   "record --host 10.77.9.202 --action start",
                   "switch --host 10.77.9.202 --program-scene Cam\\ 1",
                   "record --host 10.77.9.204 --action status", "--merge-partials",
                   "av_soak_decision.py row", "--slot-s 1200", "--extract-partial strih",
                   "--extract-partial stream", "av_tolerance_ms=", "slot budget:"):
        assert needle in out, needle


def test_the_default_slot_leaves_the_measured_strih_lx_decode_time(tmp_path):
    # ROZHODNUTÉ 5861667625: strih-lx decodes at ~8.5 frames/s on its idle E-cores, so the
    # defaults are a 20 min slot and 20 s camera segments, and the decode budget must cover a
    # 7-camera window at that speed (with margin), not the 210 s the first design assumed.
    env = {k: v for k, v in os.environ.items() if not k.startswith("AV_SOAK_")}
    r = subprocess.run(["bash", SOAK, "--plan", "--hours", "1"], env=env,
                       capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "one window every 1200 s" in r.stdout
    assert "7 x 20 s = 140 s" in r.stdout
    m = re.search(r"decode <= (\d+) s", r.stdout)
    assert m, r.stdout
    frames = 140 * 30
    assert int(m.group(1)) >= frames / 8.5 * 1.5, "decode budget below 1.5x the measured strih-lx time"


def test_plan_never_names_a_writer_or_the_production_scene(rig):
    env, _ = rig
    out = _soak(env, "--plan").stdout
    body = out.split("NEVER:")[0]
    for banned in ("av_sync_calibrate", "qr_align", "--apply", "apply-measurement-pins",
                   "set-ndi-mapping", "switch --host 10.77.9.204", "PRO\n", "'PRO'"):
        assert banned not in body, banned


def test_plan_reports_missing_run_prerequisites(rig):
    env, _ = rig
    env = dict(env)
    for k in ("CAM_PW", "STREAM_PW"):
        env.pop(k)
    out = _soak(env, "--plan").stdout
    assert "--run would need: CAM_PW STREAM_PW" in out


def test_production_scene_as_the_stream_program_is_refused(rig):
    env, p = rig
    r = _soak(dict(env, STREAM_PROG_SCENE="PRO"), "--plan")
    assert r.returncode == 3
    assert p["log"].read_text() == ""


def test_a_slot_budget_that_does_not_fit_is_refused(rig):
    env, _ = rig
    # the budget arithmetic at an explicit 600 s slot (the default moved to 1200 s)
    r = _soak(dict(env, AV_SOAK_SLOT_SECS="600", AV_SOAK_SEGMENT_SECS="200"), "--plan")
    assert r.returncode == 3
    assert "slot budget does not fit" in r.stderr
    r = _soak(dict(env, AV_SOAK_SLOT_SECS="600", AV_SOAK_DECODE_TIMEOUT_S="590"), "--plan")
    assert r.returncode == 3


def test_a_flag_without_its_value_is_a_usage_error(rig):
    env, _ = rig
    for flag in ("--hours", "--report", "--slot-secs", "--run-dir"):
        r = _soak(env, "--plan", flag)
        assert r.returncode == 3, (flag, r.returncode, r.stderr)


def test_an_acked_camera_is_removed_even_from_an_explicit_camera_set(rig):
    env, _ = rig
    out = _soak(dict(env, CAMBOX_OFFLINE_ACK="cam3:down"), "--plan").stdout
    assert "cameras: cam1 " in out and "Cam 3" not in out


# --- one full window with fakes -------------------------------------------------------------------


def test_one_window_end_to_end(rig):
    env, p = rig
    r = _soak(env, "--run")
    # one window cannot give a slope -> UNKNOWN (exit 2), never a false PASS
    assert r.returncode == 2, r.stdout + r.stderr
    rows = _csv_rows(p["run"])
    assert len(rows) == 1
    row = rows[0]
    assert row["outcome"] == "ok" and row["painter_run_id"] == "4242" and row["slot_s"] == "1200"
    assert row["av_cam1_ms"] == "3.000" and row["av_cam3_ms"] == "4.000"
    assert row["av_spread_ms"] == "1.000"
    assert row["burn_stream_zero_loss"] == "true" and row["loss_cam1_pass"] == "true"
    assert (p["run"] / "report.txt").exists() and (p["run"] / "report.json").exists()
    assert "VERDICT: UNKNOWN" in r.stdout
    timing = (p["run"] / "timing.tsv").read_text().splitlines()
    assert timing[0].startswith("slot\tpre_s") and len(timing) == 2

    obs = _calls(p["log"], "obs")
    starts = [c for c in obs if c[:1] == ["record"] and "start" in c]
    stops = [c for c in obs if c[:1] == ["record"] and "stop" in c]
    assert {_host(c) for c in starts} == {STRIH, STREAM}
    assert {_host(c) for c in stops} == {STRIH, STREAM}
    switches = [c for c in obs if c[:1] == ["switch"]]
    assert all(_host(c) == STRIH for c in switches), "the stream program is never switched"
    assert [c[c.index("--program-scene") + 1] for c in switches] == ["Cam 1", "Cam 1", "Cam 3", "Cam 1"], \
        "the first sweep scene before StartRecord, the sweep, then the strih program restored"
    assert "--prod-floor" in switches[-1]
    assert not any("PRO" in c for c in obs)
    kinds = [c[0] for c in obs]
    assert kinds.count("rig-busy-check") >= 3, "setup reads, setup mutations, the slot"
    assert kinds.index("rig-busy-check") < kinds.index("record")

    burns = _calls(p["log"], "burn")
    added = {c[c.index("--input") + 1] for c in burns if c[0] == "add"}
    removed = {c[c.index("--input") + 1] for c in burns if c[0] == "remove"}
    assert added == {"NDI cam1", "NDI cam3", "NDI 2ME PGM"} and removed == added

    decodes = [line for line in p["log"].read_text().splitlines() if line.startswith("decode ")]
    assert any("--extract-partial strih" in d for d in decodes)
    assert any("--extract-partial stream" in d and "--cam2-run-id 4242" in d for d in decodes)
    merge = _calls(p["log"], "verdict")[0]
    assert "--merge-partials" in merge and "--switch-schedule" in merge
    assert merge[merge.index("--cam2-run-id") + 1] == "4242"
    assert "--av-expected-ms" not in merge, "the verdict's own default is the single source"
    assert not any(a.startswith("--burn-") for a in merge)

    _assert_rig_restored(p)
    plan = (p["run"] / "cleanup-plan.txt").read_text()
    assert "rm -f -- '/srv/_REC/2026-09-27 21-00-00.mkv'" in plan
    assert "Remove-Item -Force -LiteralPath 'C:/_REC/2026-09-27 21-00-00.mp4'" in plan


def test_burns_already_on_are_left_on(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_BURNS_ON="1"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert not [c for c in _calls(p["log"], "burn") if c[0] in ("add", "remove")]


# --- failure paths inside a slot ------------------------------------------------------------------


def test_a_failed_stream_start_stops_both_boxes(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_START_FAIL="stream"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert _csv_rows(p["run"])[0]["outcome"] == "skipped:start_record_failed"
    stops = [c for c in _calls(p["log"], "obs") if c[:1] == ["record"] and "stop" in c]
    assert {_host(c) for c in stops} == {STRIH, STREAM}, "the failed box is stopped too"
    _assert_rig_restored(p)


def test_a_stop_whose_status_lags_one_read_is_not_a_stuck_stop(rig):
    # 27.9.2026 live 1 h run: OBS reports active=True on the status read right after StopRecord,
    # then inactive; the soak must re-read (bounded) instead of flagging a stuck recording.
    env, p = rig
    r = _soak(dict(env, FAKE_STOP_LAGGY="stream"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert "still recording after StopRecord" not in r.stdout
    stops = [c for c in _calls(p["log"], "obs")
             if c[:1] == ["record"] and "stop" in c and _host(c) == STREAM]
    assert len(stops) == 1, "a lagging status is not a stop to retry in cleanup"
    _assert_rig_restored(p)


def test_a_stop_that_did_not_take_is_retried_in_cleanup(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_STOP_STICKY="stream"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    stops = [c for c in _calls(p["log"], "obs")
             if c[:1] == ["record"] and "stop" in c and _host(c) == STREAM]
    assert len(stops) == 2
    assert "still recording after StopRecord" in r.stdout
    _assert_rig_restored(p)
    assert "cleanup\tstream" in (p["run"] / "recordings.tsv").read_text()


def test_a_failed_switch_still_restores_the_strih_program(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_SWITCH_FAIL="Cam 3"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert _csv_rows(p["run"])[0]["outcome"] == "no_verdict:switch_failed_CAM3"
    switches = [c for c in _calls(p["log"], "obs") if c[:1] == ["switch"]]
    assert switches[-1][switches[-1].index("--program-scene") + 1] == "Cam 1"
    assert "--prod-floor" in switches[-1]
    _assert_rig_restored(p)


# --- issue 1367: every recording starts on the FIRST sweep scene -----------------------------------
# Every strih NDI camera input carries its OWN measurement-burn counter (one burn filter per input,
# vendor/distroav/src/ndi-burn-filter.cpp), and the frames recorded before the sweep's first cut
# belong to no schedule window. A recording that started on another camera (slot 0: the operator's
# program; later slots: the previous slot's LAST sweep scene, which stays on program between slots)
# therefore read one phantom strih real_drop at the first cut, in 24 of 25 windows of the 8 h run of
# 28.9.2026. The E2E never has it: its [4/8] routes the strih program to the camera under test, which
# is the first sweep scene, before [5/8] StartRecord.


def test_every_slot_records_from_the_first_sweep_scene(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_STRIH_PROGRAM="Grading", **QUICK_TWO_WINDOWS), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert [row["outcome"] for row in _csv_rows(p["run"])] == ["ok", "ok"]
    obs = _calls(p["log"], "obs")
    at_start, first_cut = _strih_program_at_strih_starts(obs, "Grading")
    assert at_start == ["Cam 1", "Cam 1"], \
        "each slot's recording starts on the first sweep scene, never the operator's program " \
        "or the previous slot's last camera"
    assert first_cut == ["Cam 1", "Cam 1"], "window 0 then opens on a same-input cut"
    pre_cuts = 0
    for i, c in enumerate(obs):
        nxt = next((d for d in obs[i + 1:] if d[:1] in (["switch"], ["record"])), None)
        if c[:1] == ["switch"] and nxt is not None and nxt[:1] == ["record"] and "start" in nxt:
            pre_cuts += 1
            assert i > 0 and obs[i - 1][:1] == ["rig-busy-check"], \
                "the cut before StartRecord directly follows a rig-busy read (the slot's guard)"
    assert pre_cuts == 2, "one first-scene cut per slot"
    switches = [c for c in obs if c[:1] == ["switch"]]
    assert switches[-1][switches[-1].index("--program-scene") + 1] == "Grading"
    assert "--prod-floor" in switches[-1], "cleanup still restores the operator's program"
    _assert_rig_restored(p)


def test_a_failed_first_scene_cut_records_nothing_and_restores_the_program(rig):
    # the first sweep scene renders black (the switch's own non-black check fails): no recording is
    # started on the wrong camera, the slot is a skipped row, cleanup still restores the program
    env, p = rig
    r = _soak(dict(env, FAKE_STRIH_PROGRAM="Grading", FAKE_SWITCH_FAIL="Cam 1"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert _csv_rows(p["run"])[0]["outcome"] == "skipped:first_scene_cut_failed"
    assert not [c for c in _calls(p["log"], "obs") if c[:1] == ["record"]]
    switches = [c for c in _calls(p["log"], "obs") if c[:1] == ["switch"]]
    assert switches[-1][switches[-1].index("--program-scene") + 1] == "Grading"
    assert "--prod-floor" in switches[-1]
    _assert_rig_restored(p)


def test_a_foreign_recording_at_a_slot_start_blocks_the_first_scene_cut(rig):
    # a box records (not streams) when slot 0 starts: the broadcast read reads idle (nothing
    # streams), but the rig-busy guard refuses on a recording too -- it runs before the first-scene
    # cut, so the soak aborts without touching the strih program at all
    env, p = rig
    r = _soak(dict(env, FAKE_FOREIGN_RECORDING_AFTER_PROGRAM_READS="2"), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    obs = _calls(p["log"], "obs")
    assert not [c for c in obs if c[:1] == ["switch"]], "no cut on a busy rig, and nothing to restore"
    assert not [c for c in obs if c[:1] == ["record"] and "start" in c]
    assert "first-scene cut" in r.stdout + r.stderr
    _assert_rig_restored(p)


def test_a_broadcast_right_after_the_first_scene_cut_starts_no_recording(rig):
    # the stream box goes live right after the first-scene cut: the StartRecord guard refuses, no
    # recording is started, and cleanup leaves the (now on-air) strih program alone
    env, p = rig
    r = _soak(dict(env, FAKE_LIVE_AFTER_SWITCHES="1"), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    obs = _calls(p["log"], "obs")
    assert len([c for c in obs if c[:1] == ["switch"]]) == 1, "only the first-scene cut, no restore"
    assert not [c for c in obs if c[:1] == ["record"] and "start" in c]
    assert "strih program NOT restored" in r.stdout + r.stderr
    assert not [f for f in os.listdir(p["state"]) if f.startswith("burn-")]
    # issue 1242: the connect-on-show hold is gone, so the soak never makes a hold call at all
    assert not [c for c in obs if c[:1] == ["connect-on-show"]]
    assert not p["lease"].exists()


def test_plan_cuts_to_the_first_sweep_scene_before_startrecord(rig):
    env, _ = rig
    out = _soak(env, "--plan").stdout
    cut = out.find("switch --host 10.77.9.202 --program-scene Cam\\ 1")
    start = out.find("record --host 10.77.9.202 --action start")
    assert cut != -1 and start != -1, out
    assert cut < start, "the plan shows the first-scene cut before the strih StartRecord"


def test_sigterm_mid_slot_restores_everything(rig):
    env, p = rig
    proc = subprocess.Popen(["bash", SOAK, "--run"], env=dict(env, AV_SOAK_SEGMENT_SECS="20"),
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    assert _wait_for_sweep(p), "the sweep never started"
    proc.send_signal(signal.SIGTERM)
    out, err = proc.communicate(timeout=60)
    assert proc.returncode == 5, out + err
    assert "SIGTERM" in out
    stops = [c for c in _calls(p["log"], "obs") if c[:1] == ["record"] and "stop" in c]
    assert {_host(c) for c in stops} == {STRIH, STREAM}
    _assert_rig_restored(p)


def test_a_second_ctrl_c_to_the_process_group_cannot_cut_the_restore_short(rig):
    env, p = rig
    proc = subprocess.Popen(["bash", SOAK, "--run"],
                            env=dict(env, AV_SOAK_SEGMENT_SECS="20", FAKE_RESTORE_DELAY="3"),
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                            start_new_session=True)
    assert _wait_for_sweep(p), "the sweep never started"
    os.killpg(proc.pid, signal.SIGINT)  # Ctrl-C at the terminal: the whole foreground group
    deadline = time.time() + 60
    while time.time() < deadline and "--prod-floor" not in p["log"].read_text():
        time.sleep(0.1)
    assert "--prod-floor" in p["log"].read_text(), "cleanup never reached the strih restore"
    os.killpg(proc.pid, signal.SIGINT)  # the second Ctrl-C, while the restore runs
    out, err = proc.communicate(timeout=60)
    assert proc.returncode == 5, out + err
    _assert_rig_restored(p)


def test_a_strih_decode_timeout_stops_the_decode_on_strih_lx(rig):
    env, p = rig
    r = _soak(dict(env, AV_SOAK_MIN_DECODE_S="0", AV_SOAK_DECODE_TIMEOUT_S="2",
                   FAKE_DECODE_SLEEP_STRIH="10"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert _csv_rows(p["run"])[0]["outcome"] == "no_verdict:decode_failed"
    kills = [line for line in p["log"].read_text().splitlines()
             if line.startswith("sshpass") and "pkill" in line]
    assert kills and all("su@10.77.9.202" in k for k in kills), kills
    assert "recording-verdic\\[t\\]\\ --extract-partial\\ strih" in kills[0] \
        or "recording-verdic[t] --extract-partial strih" in kills[0], kills[0]
    assert "av-soak-" in kills[0], "the kill is scoped to this run's own output names"
    _assert_rig_restored(p)


def test_sigterm_during_the_decode_stops_both_remote_decodes(rig):
    env, p = rig
    proc = subprocess.Popen(["bash", SOAK, "--run"],
                            env=dict(env, FAKE_DECODE_SLEEP_STRIH="30", FAKE_DECODE_SLEEP_STREAM="30"),
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    deadline = time.time() + 60
    while time.time() < deadline and "decode stream-decode.sh" not in p["log"].read_text():
        time.sleep(0.2)
    proc.send_signal(signal.SIGTERM)
    out, err = proc.communicate(timeout=60)
    assert proc.returncode == 5, out + err
    assert any(line.startswith("sshpass") and "pkill" in line
               for line in p["log"].read_text().splitlines())
    stops = [c for c in _ps_commands(p["log"]) if "Stop-Process" in c]
    assert stops, "the stream decode is stopped on the box"
    assert all("CommandLine" in c and "av-soak-" in c for c in stops), \
        "only this run's own recording-verdict (by its output name), never every one"
    _assert_rig_restored(p)


def test_a_recording_that_never_stops_is_a_loud_exit_5(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_STOP_NEVER="stream"), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    assert "RECORDING MAY STILL BE RUNNING on stream" in r.stdout + r.stderr
    state = (p["run"] / "recording.state").read_text()
    assert "stream=1" in state and "strih=0" in state
    assert "stream_since=" in state and "lease=av-soak-" in state
    assert "start_window_s=60\n" in state, "the ownership window (OBS timeout 30 s + 30 s) is persisted"
    assert p["lease"].exists(), "no E2E may start over a recording the soak may have left"
    assert "rig lease KEPT" in r.stdout + r.stderr


def _leftover_state(p, box, since=None, lease="av-soak-test-1"):
    """A run dir the soak left: `box` flagged, its flag set `since` (epoch, default now)."""
    p["run"].mkdir(exist_ok=True)
    since = time.time() if since is None else since
    lines = [f"strih={int(box == 'strih')}", f"strih_since={int(since) if box == 'strih' else ''}",
             f"stream={int(box == 'stream')}",
             f"stream_since={int(since) if box == 'stream' else ''}", f"lease={lease}"]
    (p["run"] / "recording.state").write_text("\n".join(lines) + "\n")


def _record_stops(p):
    return [c for c in _calls(p["log"], "obs") if c[:1] == ["record"] and "stop" in c]


def test_stop_leftovers_stops_only_the_soaks_own_recording(rig):
    env, p = rig
    _leftover_state(p, "stream")
    (p["state"] / "recording-stream").write_text(str(time.time()))
    (p["state"] / "recording-strih").write_text(str(time.time()))  # strih records, not flagged
    r = _soak(env, "--stop-leftovers", str(p["run"]))
    assert r.returncode == 0, r.stdout + r.stderr
    assert [_host(c) for c in _record_stops(p)] == [STREAM]
    assert not (p["state"] / "recording-stream").exists()
    assert (p["state"] / "recording-strih").exists(), "an unflagged box is never touched"
    assert "stream=0" in (p["run"] / "recording.state").read_text()


def test_stop_leftovers_never_stops_a_streaming_box(rig):
    env, p = rig
    _leftover_state(p, "stream")
    (p["state"] / "recording-stream").write_text(str(time.time()))
    r = _soak(dict(env, FAKE_STREAMING_BOX="stream"), "--stop-leftovers", str(p["run"]))
    assert r.returncode == 5, r.stdout + r.stderr
    assert not _record_stops(p)
    assert "NOT stopping" in r.stdout + r.stderr


def test_stop_leftovers_never_stops_strih_while_the_stream_box_broadcasts(rig):
    # strih never streams: "recording, not streaming" is strih's own broadcast state
    env, p = rig
    _leftover_state(p, "strih")
    (p["state"] / "recording-strih").write_text(str(time.time()))
    r = _soak(dict(env, FAKE_STREAMING_BOX="stream"), "--stop-leftovers", str(p["run"]))
    assert r.returncode == 5, r.stdout + r.stderr
    assert not _record_stops(p)
    assert "broadcast" in r.stdout + r.stderr
    assert "strih=1" in (p["run"] / "recording.state").read_text()


def test_stop_leftovers_touches_nothing_on_an_unreadable_rig(rig):
    env, p = rig
    _leftover_state(p, "strih")
    (p["state"] / "recording-strih").write_text(str(time.time()))
    r = _soak(dict(env, FAKE_BUSY_UNREADABLE="stream"), "--stop-leftovers", str(p["run"]))
    assert r.returncode == 5, r.stdout + r.stderr
    assert not _record_stops(p)
    assert "unreadable" in r.stdout + r.stderr


def test_stop_leftovers_retries_an_unreadable_read(rig):
    # one unreadable rig-busy read (a stream OBS restart) is retried, not a kept leftover
    env, p = rig
    _leftover_state(p, "stream")
    (p["state"] / "recording-stream").write_text(str(time.time()))
    (p["state"] / "count-switches").write_text("1")  # arms the fake's unreadable trigger
    r = _soak(dict(env, FAKE_UNREADABLE_AFTER_SWITCHES="1", FAKE_UNREADABLE_TIMES="1"),
              "--stop-leftovers", str(p["run"]))
    assert r.returncode == 0, r.stdout + r.stderr
    assert [_host(c) for c in _record_stops(p)] == [STREAM]
    assert (p["state"] / "count-unreadable").read_text() == "1"


def test_stop_leftovers_leaves_a_recording_the_soak_did_not_start(rig):
    env, p = rig
    _leftover_state(p, "strih")
    (p["state"] / "recording-strih").write_text(str(time.time() - 3600))
    r = _soak(env, "--stop-leftovers", str(p["run"]))
    assert r.returncode == 5, r.stdout + r.stderr
    assert not _record_stops(p)
    assert "cannot prove" in r.stdout + r.stderr


def test_stop_leftovers_refuses_while_the_soak_still_runs(rig):
    env, p = rig
    _leftover_state(p, "stream")
    (p["state"] / "recording-stream").write_text(str(time.time()))
    # a stand-in whose command line names av-soak.sh, like `bash scripts/av-soak.sh --run`; the
    # `; true` keeps bash from exec'ing sleep (which would replace that command line)
    live = subprocess.Popen(["bash", "-c", "sleep 30; true", "av-soak.sh"])
    try:
        (p["run"] / "pid").write_text(f"{live.pid}\n")
        r = _soak(env, "--stop-leftovers", str(p["run"]))
    finally:
        live.kill()
        live.wait()
    assert r.returncode == 4, r.stdout + r.stderr
    assert not _record_stops(p)
    assert "still running" in r.stdout + r.stderr


def test_a_stuck_recording_keeps_the_lease_until_stop_leftovers_clears_it(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_STOP_NEVER="stream"), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    assert p["lease"].exists()
    r = _soak(env, "--stop-leftovers", str(p["run"]))
    assert r.returncode == 0, r.stdout + r.stderr
    assert not (p["state"] / "recording-stream").exists()
    assert not p["lease"].exists(), "the leftover is stopped, so the soak's lease is released"
    assert "rig lease released" in r.stdout + r.stderr


def test_run_without_the_strih_credentials_is_refused(rig):
    env, p = rig
    env = dict(env)
    env.pop("STRIH_PW")
    r = _soak(env, "--run")
    assert r.returncode == 4
    assert not p["lease"].exists() and p["log"].read_text() == ""


def test_two_windows_retry_a_stuck_stop_and_log_an_unreadable_volume_once(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_STOP_STICKY="stream", FAKE_CURL_FAIL="1", **QUICK_TWO_WINDOWS), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    rows = _csv_rows(p["run"])
    assert [row["outcome"] for row in rows] == ["ok", "ok"]
    assert "a broadcast is live" not in r.stdout + r.stderr
    out = r.stdout + r.stderr
    assert out.count("free space unreadable") == 2, out  # once per box, not once per slot
    _assert_rig_restored(p)


def test_an_unpinned_painter_run_id_is_a_warning(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_PAINTER_NO_RUN_ID="1"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert "WARNING" in r.stdout and "unpinned" in r.stdout
    merge = _calls(p["log"], "verdict")[0]
    assert merge[merge.index("--cam2-run-id") + 1] == "0"


def test_a_broadcast_that_starts_mid_run_leaves_the_strih_program_alone(rig):
    # the stream box goes live right after slot 0's StopRecord: the wait for slot 1 sees it and
    # stops the soak, and its cleanup must not cut the (on-air) strih program; the burns still go
    # back to production
    env, p = rig
    r = _soak(dict(env, FAKE_LIVE_AFTER_STOPS="1", **QUICK_TWO_WINDOWS), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    out = r.stdout + r.stderr
    assert "a broadcast is live" in out
    assert not [c for c in _calls(p["log"], "obs") if c[:1] == ["switch"] and "--prod-floor" in c]
    assert "strih program NOT restored" in out
    assert not [f for f in os.listdir(p["state"]) if f.startswith("burn-")]
    assert not p["lease"].exists()


def test_a_broadcast_between_slots_aborts_the_soak_before_the_next_slot(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_LIVE_AFTER_STOPS="1", **QUICK_TWO_WINDOWS), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    assert "a broadcast went live" in r.stdout + r.stderr
    assert len(_csv_rows(p["run"])) == 1, "slot 1 never started"
    starts = [c for c in _calls(p["log"], "obs") if c[:1] == ["record"] and "start" in c]
    assert len(starts) == 2, "only slot 0's two StartRecords"


def test_a_broadcast_that_goes_live_mid_sweep_keeps_the_recordings_running(rig):
    # after the first cut the stream box streams: the soak stops cutting, and during a broadcast
    # its own file may already be the show's recording (Companion's StartRecord is a no-op on a
    # box that already records) -- nothing is stopped, the lease is kept for --stop-leftovers
    env, p = rig
    # (issue 1367: the slot's first strih cut is the one to the first sweep scene BEFORE
    # StartRecord, so the broadcast starts after the sweep's own first cut = the second switch)
    r = _soak(dict(env, FAKE_LIVE_AFTER_SWITCHES="2"), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    out = r.stdout + r.stderr
    assert "a broadcast went live" in out
    switches = [c for c in _calls(p["log"], "obs") if c[:1] == ["switch"]]
    assert len(switches) == 2, "no cut after the broadcast started, and no restore cut"
    assert not [c for c in _calls(p["log"], "obs") if c[:1] == ["record"] and "stop" in c]
    assert "NOT stopping" in out and "RECORDING MAY STILL BE RUNNING" in out
    assert (p["state"] / "recording-strih").exists() and (p["state"] / "recording-stream").exists()
    assert p["lease"].exists() and "rig lease KEPT" in out


def test_the_rig_leaving_test_mode_stops_the_run(rig):
    # slot 1 reads another stream program: the rig was handed to production, the soak ends (it
    # never runs on skipping slots while holding the lease)
    env, p = rig
    r = _soak(dict(env, FAKE_STREAM_PROGRAM_DRIFT_AFTER="2", **QUICK_TWO_WINDOWS), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert "left TEST mode" in r.stdout + r.stderr
    assert len(_csv_rows(p["run"])) == 1
    _assert_rig_restored(p)


def test_an_unreadable_stream_program_is_a_skipped_row(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_STREAM_PROGRAM_BLANK_AFTER="1"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert _csv_rows(p["run"])[0]["outcome"] == "skipped:stream_program_unreadable"
    _assert_rig_restored(p)


def test_an_unreadable_rig_before_stoprecord_keeps_the_recordings(rig):
    # the stream box stops answering after the last cut (switch 3 = the first-scene cut before
    # StartRecord + the two sweep cuts): the retried reads never prove an idle rig, so neither
    # the slot nor cleanup stops a recording that may be a show's
    env, p = rig
    r = _soak(dict(env, FAKE_UNREADABLE_AFTER_SWITCHES="3"), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    out = r.stdout + r.stderr
    assert not [c for c in _calls(p["log"], "obs") if c[:1] == ["record"] and "stop" in c]
    assert "NOT stopping" in out and "the rig state is unreadable" in out
    assert p["lease"].exists()


def test_cleanup_retries_an_unreadable_read_and_stops_the_recordings(rig):
    # the three reads before the StopRecords are unreadable (the slot aborts); cleanup's first
    # read is unreadable too, its retry reads an idle rig: cleanup stops both recordings
    env, p = rig
    r = _soak(dict(env, FAKE_UNREADABLE_AFTER_SWITCHES="3", FAKE_UNREADABLE_TIMES="4"), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    stops = [c for c in _calls(p["log"], "obs") if c[:1] == ["record"] and "stop" in c]
    assert {_host(c) for c in stops} == {STRIH, STREAM}
    assert (p["state"] / "count-unreadable").read_text() == "4"
    _assert_rig_restored(p)


def test_one_unreadable_read_before_stoprecord_is_retried(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_UNREADABLE_AFTER_SWITCHES="3", FAKE_UNREADABLE_TIMES="1"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert _csv_rows(p["run"])[0]["outcome"] == "ok"
    _assert_rig_restored(p)


def test_an_unreadable_rig_at_a_slot_start_records_nothing(rig):
    # never start a recording the soak could not prove it may stop again
    env, p = rig
    r = _soak(dict(env, FAKE_UNREADABLE_AFTER_PROGRAM_READS="2"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert _csv_rows(p["run"])[0]["outcome"] == "skipped:rig_state_unreadable"
    assert not [c for c in _calls(p["log"], "obs") if c[:1] == ["record"]]
    assert not [c for c in _calls(p["log"], "obs") if c[:1] == ["switch"]], \
        "no strih cut either (the first-scene cut needs the same proven idle rig)"
    _assert_rig_restored(p)


def test_an_unreadable_rig_at_setup_is_refused(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_BUSY_UNREADABLE="stream"), "--run")
    assert r.returncode == 4, r.stdout + r.stderr
    assert "unreadable" in r.stdout + r.stderr
    assert not [c for c in _calls(p["log"], "burn") if c[0] == "add"]
    assert not p["lease"].exists()


def test_a_failed_start_during_a_broadcast_stops_nothing(rig):
    # strih starts, the stream box goes live, the stream start fails: the soak's strih file may
    # now be the show's recording -- the start-failure path stops nothing
    env, p = rig
    r = _soak(dict(env, FAKE_LIVE_AFTER_STARTS="1", FAKE_START_FAIL="stream"), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    assert not [c for c in _calls(p["log"], "obs") if c[:1] == ["record"] and "stop" in c]
    assert "NOT stopping" in r.stdout + r.stderr
    assert p["lease"].exists()


def test_a_leftover_recording_is_not_stopped_while_a_broadcast_is_live(rig):
    # slot 0's stream StopRecord does not take; the stream box goes live when slot 1 starts (after
    # the between-slot check): slot 1 must not stop the leftover, cleanup neither
    env, p = rig
    r = _soak(dict(env, FAKE_STOP_STICKY="stream", FAKE_LIVE_AFTER_PROGRAM_READS="3",
                   **QUICK_TWO_WINDOWS), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    stream_stops = [c for c in _calls(p["log"], "obs")
                    if c[:1] == ["record"] and "stop" in c and _host(c) == STREAM]
    assert len(stream_stops) == 1, "only slot 0's own StopRecord, never one during the broadcast"
    assert "NOT stopping" in r.stdout + r.stderr


def test_a_run_dir_that_already_holds_a_run_is_refused(rig):
    env, p = rig
    p["run"].mkdir()
    (p["run"] / "soak.csv").write_text("a previous run\n")
    r = _soak(env, "--run")
    assert r.returncode == 4, r.stdout + r.stderr
    assert "already holds" in r.stdout + r.stderr
    assert (p["run"] / "soak.csv").read_text() == "a previous run\n"
    assert not p["lease"].exists() and p["log"].read_text() == ""


def test_a_restore_that_fails_its_brightness_check_is_confirmed_by_a_re_read(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_RESTORE_FAIL="1"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert "strih program restored to 'Cam 1'" in r.stdout
    assert "could not restore" not in r.stdout


def test_the_cleanup_plan_names_the_on_box_verdict_artifacts(rig):
    env, p = rig
    assert _soak(env, "--run").returncode == 2
    plan = (p["run"] / "cleanup-plan.txt").read_text()
    assert "verdict-out/av-soak-" in plan and "Remove-Item -Recurse" in plan
    assert "verdict-out\\av-soak-" in plan, "a literal backslash, never a printf escape"


def test_a_negative_decode_budget_names_the_slot_budget(rig):
    env, _ = rig
    r = _soak(dict(env, AV_SOAK_SLOT_SECS="600", AV_SOAK_SEGMENT_SECS="300"), "--plan")
    assert r.returncode == 3
    assert "slot budget does not fit" in r.stderr


def test_the_stop_file_ends_the_run_with_its_report(rig):
    env, p = rig
    stop = p["run"] / "STOP"
    r = _soak(dict(env, AV_SOAK_HOURS="1", FAKE_TOUCH_STOP=str(stop)), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert len(_csv_rows(p["run"])) == 1
    assert "STOP file found" in r.stdout
    _assert_rig_restored(p)


def test_a_low_record_volume_stops_before_recording(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_FREE_BYTES="1000"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert "record volume has only" in r.stdout
    assert not [c for c in _calls(p["log"], "obs") if c[:1] == ["record"]]
    _assert_rig_restored(p)


def test_a_painter_that_is_switched_off_stops_the_run(rig):
    # rig-mode.sh event stops the painter: TEST mode is gone, the soak ends before recording
    env, p = rig
    r = _soak(dict(env, FAKE_PAINTER_FAIL_AFTER="1"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert "left TEST mode" in r.stdout + r.stderr
    assert not (p["run"] / "soak.csv").exists()
    assert not [c for c in _calls(p["log"], "obs") if c[:1] == ["record"]]
    _assert_rig_restored(p)


def test_a_painter_whose_marker_log_stalls_gives_a_skipped_row(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_PAINTER_STUCK_AFTER="1"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert _csv_rows(p["run"])[0]["outcome"] == "skipped:painter_not_emitting"
    assert not [c for c in _calls(p["log"], "obs") if c[:1] == ["record"]]
    _assert_rig_restored(p)


def test_an_unreadable_painter_is_a_skipped_row(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_PAINTER_UNREADABLE_AFTER="1"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert _csv_rows(p["run"])[0]["outcome"] == "skipped:painter_not_emitting"
    _assert_rig_restored(p)


def test_the_help_describes_the_safe_stop_leftovers_rule(rig):
    env, _p = rig
    h = _soak(env, "--help")
    assert h.returncode == 0
    assert "streams" in h.stdout and "flag time" in h.stdout, "the fleet-aware ownership rule"
    assert "never while a broadcast" in h.stdout
    assert "lease" in h.stdout and "already holds a run" in h.stdout


# --- refusals before any rig change ---------------------------------------------------------------


def test_a_live_foreign_lease_is_refused_and_left_alone(rig):
    env, p = rig
    p["lease"].mkdir()
    holder = {"repo": "zbynekdrlik/camera-box", "run_id": "999", "run_url": "", "job": "full-path",
              "acquired_at": "2026-09-27T17:59:15Z", "expected_release_at": "2099-01-01T00:00:00Z"}
    (p["lease"] / "holder.json").write_text(json.dumps(holder))
    (p["lease"] / "heartbeat").write_text("")
    r = _soak(env, "--run")
    assert r.returncode == 4, r.stdout + r.stderr
    assert "lease is held" in r.stderr
    assert p["log"].read_text() == ""
    assert json.loads((p["lease"] / "holder.json").read_text())["run_id"] == "999"


def test_not_in_test_mode_is_refused_before_any_mutation(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_STREAM_PROGRAM="PRO"), "--run")
    assert r.returncode == 4, r.stdout + r.stderr
    obs = _calls(p["log"], "obs")
    assert not [c for c in obs if c[0] in ("record", "switch")]
    assert not [c for c in _calls(p["log"], "burn") if c[0] == "add"]
    assert not p["lease"].exists()


def test_a_busy_rig_is_refused_before_any_mutation(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_BUSY="1"), "--run")
    assert r.returncode == 4, r.stdout + r.stderr
    obs = _calls(p["log"], "obs")
    assert not [c for c in obs if c[0] in ("record", "switch", "program-scene")]
    assert not _calls(p["log"], "burn")
    assert not p["lease"].exists()


def test_a_dead_painter_is_refused(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_PAINTER_ACTIVE="inactive"), "--run")
    assert r.returncode == 4, r.stdout + r.stderr
    assert not [c for c in _calls(p["log"], "obs") if c[0] == "record"]


def test_run_without_credentials_is_refused_before_the_lease(rig):
    env, p = rig
    env = dict(env)
    env.pop("CAM_PW")
    r = _soak(env, "--run")
    assert r.returncode == 4
    assert not p["lease"].exists() and p["log"].read_text() == ""


def test_report_mode_regrades_a_run_dir(rig):
    env, p = rig
    assert _soak(env, "--run").returncode == 2
    r = _soak(env, "--report", str(p["run"]), "--hours", "0")
    assert r.returncode == 2
    assert "VERDICT: UNKNOWN" in r.stdout


# --- the shared free-space line (bundle_state_gather) ---------------------------------------------


def test_recordings_free_line():
    assert bsg.recordings_free_line('{"free_bytes": 60000000000}', 50) == "OK 60.0"
    assert bsg.recordings_free_line('{"free_bytes": 40000000000}', 50) == "WARN 40.0"
    assert bsg.recordings_free_line('{"free_bytes": null}', 50) == "UNKNOWN -1"
    assert bsg.recordings_free_line("not json", 50) == "UNKNOWN -1"
    assert bsg.recordings_free_line("[1]", 50) == "UNKNOWN -1"
    assert bsg.recordings_free_line("", 50) == "UNKNOWN -1"


# --- the pure lib ---------------------------------------------------------------------------------


def _lib(tmp_path, body):
    script = tmp_path / "drive.sh"
    script.write_text(textwrap.dedent(f"""\
        set -euo pipefail
        . {REPO}/scripts/lib/cambox-offline-ack.sh
        . {LIB}
        {body}
        """))
    r = subprocess.run(["bash", str(script)], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_lib_window_arithmetic(tmp_path):
    out = _lib(tmp_path, """
        av_soak_windows_count 28800 600
        av_soak_windows_count 3600 600
        av_soak_windows_count 0 600
        av_soak_windows_count 100 0
        av_soak_min_secs 210
        av_soak_marker_rows 210
    """)
    assert out.split() == ["49", "7", "1", "1", "189", "1320"]


def test_lib_painter_ok_needs_active_and_a_growing_log(tmp_path):
    out = _lib(tmp_path, """
        av_soak_painter_ok active 10 12 && echo yes1 || echo no1
        av_soak_painter_ok active 12 12 && echo yes2 || echo no2
        av_soak_painter_ok inactive 1 9 && echo yes3 || echo no3
        av_soak_painter_ok active "" 9 && echo yes4 || echo no4
        av_soak_kv run_id "$(printf 'active=active\\nrun_id=77\\n')"
    """)
    assert out.split() == ["yes1", "no2", "no3", "no4", "77"]


def test_lib_marker_csv_check(tmp_path):
    good = tmp_path / "g.csv"
    good.write_text("index,frame_id,emit_ts_ns\n1,2,3\n")
    # the real emitter log (src/qpsk_marker.rs) starts with a `# qpsk-params` line
    params = tmp_path / "p.csv"
    params.write_text("# qpsk-params sr=48000 carrier=442 c=1 q=2 vr=60/1\nindex,frame_id,emit_ts_ns\n1,2,3\n")
    params_no_header = tmp_path / "n.csv"
    params_no_header.write_text("# qpsk-params sr=48000 carrier=442 c=1 q=2 vr=60/1\n1,2,3\n")
    head_only = tmp_path / "h.csv"
    head_only.write_text("index,frame_id,emit_ts_ns\n")
    wrong = tmp_path / "w.csv"
    wrong.write_text("tick,gen_ts_ns\n1,2\n")
    out = _lib(tmp_path, f"""
        for f in {good} {head_only} {wrong} {tmp_path}/absent.csv {params} {params_no_header}; do
          av_soak_marker_csv_ok "$f" && echo ok || echo bad
        done
    """)
    assert out.split() == ["ok", "bad", "bad", "bad", "ok", "bad"]


def test_lib_marker_snapshot_keeps_the_params_line_and_the_header(tmp_path):
    # 27.9.2026 live 1 h run: the snapshot kept only line 1 (`# qpsk-params`) and every window
    # was graded marker_log_unreadable.
    log = tmp_path / "rig-qpsk-markers.csv"
    rows = "".join(f"{i},{88000 + i},{1790549977972122791 + i}\n" for i in range(1, 51))
    log.write_text("# qpsk-params sr=48000 carrier=442 c=1 q=2 vr=60/1\nindex,frame_id,emit_ts_ns\n" + rows)
    snap = tmp_path / "snap.csv"
    out = _lib(tmp_path, f"""
        bash -c "$(av_soak_marker_snapshot_cmd {log} 5)" > {snap}
        av_soak_marker_csv_ok {snap} && echo ok || echo bad
    """)
    assert out.split()[-1] == "ok"
    lines = snap.read_text().splitlines()
    assert lines[0].startswith("# qpsk-params")
    assert lines[1] == "index,frame_id,emit_ts_ns"
    assert [l.split(",")[0] for l in lines[2:]] == ["46", "47", "48", "49", "50"]


def test_lib_unacked_cams(tmp_path):
    out = _lib(tmp_path, """
        CAMBOX_OFFLINE_ACK="cam3:down,imag:away"
        av_soak_unacked_cams "cam1 cam2 cam3 cam4"
    """)
    assert out.strip() == "cam1 cam2 cam4"


def test_lib_merge_argv_passes_the_expected_offset_only_when_given(tmp_path):
    out = _lib(tmp_path, """
        av_soak_merge_argv a BIN S T 189 30 30 42 ACK SCHED PIX J
        printf '%s|' "${a[@]}"; echo
        av_soak_merge_argv b BIN S T 189 30 30 42 ACK SCHED PIX J -5
        printf '%s|' "${b[@]}"; echo
    """)
    first, second = out.splitlines()
    assert "--av-expected-ms" not in first
    assert second.endswith("--av-expected-ms|-5|")
    assert "--merge-partials|strih=S|--merge-partials|stream=T|--min-secs|189|" in first


def test_lib_decode_kills_are_scoped_to_the_run_stamp(tmp_path):
    out = subprocess.run(
        ["bash", "-c", f'. "{LIB}"; av_soak_strih_decode_kill_cmd 20260927T205047Z; echo; '
                       f'av_soak_stream_decode_kill_ps 20260927T205047Z'],
        capture_output=True, text=True, timeout=30)
    assert out.returncode == 0, out.stderr
    strih, stream = out.stdout.split("\n", 1)
    assert strih.startswith("pkill -f 'recording-verdic[t] --extract-partial strih")
    assert "av-soak-20260927T205047Z-s" in strih and strih.rstrip().endswith(";")
    assert "Win32_Process" in stream and "recording-verdict.exe" in stream
    assert "*av-soak-20260927T205047Z-s*" in stream and "Stop-Process" in stream


def test_lib_probe_and_snapshot_commands_are_read_only(tmp_path):
    out = _lib(tmp_path, """
        av_soak_painter_probe_cmd /run/rig-qpsk-markers.csv
        av_soak_marker_snapshot_cmd /run/rig-qpsk-markers.csv 1320
    """)
    body = out.replace("2>/dev/null", "").replace("< '", "")
    for banned in ("systemctl stop", "systemctl start", "systemctl restart", "rm ", "kill", ">", "tee"):
        assert banned not in body, banned
    assert out.rstrip().endswith(";")
