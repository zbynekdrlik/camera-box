"""issue 1367 -- the 8 h stream-output A/V soak orchestrator (scripts/av-soak.sh) + its pure lib
(scripts/lib/av-soak.sh), driven end to end with FAKES (Tier-0: no rig, no cargo, no network).

Fakes on PATH / behind the documented seams:
  - obs_phase2.py + obs_burn_filter.py in AV_SOAK_OBS_DIR (log every call; answer rig-busy-check,
    program-scene, record start/stop/status, switch, connect-on-show, burn check/add/remove),
  - sshpass (cam2 painter probe + marker-log tail, the stream-box New-Item/scp/Stop-Process) and
    curl (the record-volume free space) on PATH,
  - AV_SOAK_STRIH_DECODE / AV_SOAK_STREAM_DECODE (write the partial the wrappers would pull back),
  - PROBE_BIN_DIR/recording-verdict (writes a merged verdict JSON, exits 1 like a failing gate),
  - RIG_LEASE_DIR + CAMERA_BOX_RIG_HEARTBEAT + CONNECT_ON_SHOW_HOLD_STATE in a tmp dir and the
    CONNECT_ON_SHOW_MARKER_CMD / CONNECT_ON_SHOW_LOG_READ_CMD seams -- the REAL dev1 lease, heartbeat
    and hold state are never touched (an E2E may hold the real lease while this runs).
"""
import base64
import json
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
cmd = a[0]
host = arg("--host")
if cmd == "rig-busy-check":
    busy = os.environ.get("FAKE_BUSY") == "1"
    diags = []
    for box in ("strih", "stream"):
        streaming = busy or os.environ.get("FAKE_STREAMING_BOX") == box
        diags.append({"host": box, "streaming": streaming,
                      "recording": os.path.exists(os.path.join(state, "recording-" + box))})
    print(json.dumps({"busy": busy, "diagnostics": diags}))
elif cmd == "stream-detail":
    pass
elif cmd == "program-scene":
    if host == os.environ["FAKE_STREAM_HOST"]:
        n = bump("stream-program")
        after = int(os.environ.get("FAKE_STREAM_PROGRAM_DRIFT_AFTER", "0") or 0)
        drifted = after and n > after
        print("Other scene" if drifted else os.environ.get("FAKE_STREAM_PROGRAM", "Development"))
    else:
        print("Cam 1")
elif cmd == "record":
    act = arg("--action")
    box = "stream" if host == os.environ["FAKE_STREAM_HOST"] else "strih"
    flag = os.path.join(state, "recording-" + box)
    if act == "start":
        open(flag, "w").close()
        if os.environ.get("FAKE_START_FAIL") == box:
            sys.exit("fake: StartRecord verify failed")
    elif act == "stop":
        never = os.environ.get("FAKE_STOP_NEVER") == box
        if not never and (os.environ.get("FAKE_STOP_STICKY") != box or bump("stop-" + box) > 1):
            if os.path.exists(flag):
                os.remove(flag)
        if os.environ.get("FAKE_TOUCH_STOP"):
            open(os.environ["FAKE_TOUCH_STOP"], "w").close()
        print("/srv/_REC/2026-09-27 21-00-00.mkv" if box == "strih" else "C:/_REC/2026-09-27 21-00-00.mp4")
    elif act == "status":
        print(f"active={os.path.exists(flag)} path=x")
elif cmd == "switch":
    scene = arg("--program-scene")
    if os.environ.get("FAKE_SWITCH_FAIL") == scene:
        sys.exit("fake: scene renders black")
    if os.environ.get("FAKE_RESTORE_DELAY") and "--prod-floor" in a:
        time.sleep(float(os.environ["FAKE_RESTORE_DELAY"]))
    if os.environ.get("FAKE_RESTORE_FAIL") and "--prod-floor" in a:
        sys.exit("fake: the restored scene is dim (non-black check)")
    print(time.time_ns())
elif cmd == "connect-on-show":
    if "--hold" in a:
        json.dump(["NDI cam1", "NDI cam3"], open(arg("--hold"), "w"))
        print("issue 1242 connect-on-show held (connect-on-show OFF for the run): NDI cam1, NDI cam3")
    else:
        print("issue 1242 connect-on-show restored (connect-on-show ON): NDI cam1, NDI cam3")
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
printf 'sshpass' >> "$FAKE_LOG"; printf ' %q' "$@" >> "$FAKE_LOG"; printf '\n' >> "$FAKE_LOG"
case "$*" in
  *"systemctl is-active cam2-painter"*)
    n=$(( $(cat "$FAKE_STATE/count-probe" 2>/dev/null || echo 0) + 1 ))
    echo "$n" > "$FAKE_STATE/count-probe"
    active="${FAKE_PAINTER_ACTIVE:-active}"
    if [ -n "${FAKE_PAINTER_FAIL_AFTER:-}" ] && [ "$n" -gt "$FAKE_PAINTER_FAIL_AFTER" ]; then active=inactive; fi
    rid=4242; [ -n "${FAKE_PAINTER_NO_RUN_ID:-}" ] && rid=""
    printf 'active=%s\nrun_id=%s\nmarkers=10\nmarkers2=14\n' "$active" "$rid" ;;
  *"head -n 1"*)
    printf 'index,frame_id,emit_ts_ns\n1,100,5\n2,130,6\n' ;;
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
printf 'decode %s' "$(basename "$0")" >> "$FAKE_LOG"; printf ' %q' "$@" >> "$FAKE_LOG"; printf '\n' >> "$FAKE_LOG"
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

FAKE_MARKER = r'''#!/usr/bin/env bash
printf 'marker %s\n' "$*" >> "$FAKE_LOG"
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
    _write(tmp_path / "marker.sh", FAKE_MARKER)
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
        "CONNECT_ON_SHOW_HOLD_STATE": str(tmp_path / "hold.json"),
        "CONNECT_ON_SHOW_MARKER_CMD": str(tmp_path / "marker.sh"),
        "CONNECT_ON_SHOW_LOG_READ_CMD": "true",
        "CONNECT_ON_SHOW_LIVE_WAIT_S": "0",
        "RIG_FLEET_ACK_FILE": str(tmp_path / "no-acks.txt"),
        "CAM_PW": "x", "STREAM_USER": "u", "STREAM_PW": "y", "STRIH_USER": "su", "STRIH_PW": "sp",
        "STRIH_HOST": STRIH, "STREAM_HOST": STREAM,
        "E2E_ONBOX_DECODE_PRIORITY": "BelowNormal",
    })
    return env, {"log": log, "run": tmp_path / "run", "lease": tmp_path / "lease",
                 "hb": tmp_path / "heartbeat", "state": state, "hold": tmp_path / "hold.json"}


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
    assert not p["hold"].exists()
    out = r.stdout
    for needle in ("rig_lease_acquire", "stray_session_check_assert", "connect_on_show_e2e_hold",
                   "record --host 10.77.9.202 --action start",
                   "switch --host 10.77.9.202 --program-scene Cam\\ 1",
                   "record --host 10.77.9.204 --action status", "--merge-partials",
                   "av_soak_decision.py row", "--slot-s 600", "--extract-partial strih",
                   "--extract-partial stream", "av_tolerance_ms=", "slot budget:"):
        assert needle in out, needle


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
    r = _soak(dict(env, AV_SOAK_SEGMENT_SECS="200"), "--plan")
    assert r.returncode == 3
    assert "slot budget does not fit" in r.stderr
    r = _soak(dict(env, AV_SOAK_DECODE_TIMEOUT_S="590"), "--plan")
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
    assert row["outcome"] == "ok" and row["painter_run_id"] == "4242" and row["slot_s"] == "600"
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
    assert [c[c.index("--program-scene") + 1] for c in switches] == ["Cam 1", "Cam 3", "Cam 1"], \
        "the sweep, then the strih program restored to its snapshot"
    assert "--prod-floor" in switches[-1]
    assert not any("PRO" in c for c in obs)
    kinds = [c[0] for c in obs]
    assert kinds.count("rig-busy-check") >= 3, "setup reads, setup mutations, the slot"
    assert kinds.index("rig-busy-check") < kinds.index("connect-on-show") < kinds.index("record")
    cos = [c for c in obs if c[0] == "connect-on-show"]
    assert "--hold" in cos[0] and "--restore" in cos[-1], "the connect-on-show hold is restored"
    markers = [line for line in p["log"].read_text().splitlines() if line.startswith("marker ")]
    assert markers[0] == f"marker set {STRIH}" and markers[-1] == f"marker clear {STRIH}"
    assert len([m for m in markers if m.startswith("marker set")]) >= 2, "re-asserted per slot"

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


def test_sigterm_mid_slot_restores_everything(rig):
    env, p = rig
    proc = subprocess.Popen(["bash", SOAK, "--run"], env=dict(env, AV_SOAK_SEGMENT_SECS="20"),
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    deadline = time.time() + 60
    while time.time() < deadline and '"switch"' not in p["log"].read_text():
        time.sleep(0.2)
    assert '"switch"' in p["log"].read_text(), "the sweep never started"
    proc.send_signal(signal.SIGTERM)
    out, err = proc.communicate(timeout=60)
    assert proc.returncode == 5, out + err
    assert "SIGTERM" in out
    stops = [c for c in _calls(p["log"], "obs") if c[:1] == ["record"] and "stop" in c]
    assert {_host(c) for c in stops} == {STRIH, STREAM}
    _assert_rig_restored(p)
    cos = [c for c in _calls(p["log"], "obs") if c[0] == "connect-on-show"]
    assert "--restore" in cos[-1]


def test_a_second_ctrl_c_to_the_process_group_cannot_cut_the_restore_short(rig):
    env, p = rig
    proc = subprocess.Popen(["bash", SOAK, "--run"],
                            env=dict(env, AV_SOAK_SEGMENT_SECS="20", FAKE_RESTORE_DELAY="3"),
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                            start_new_session=True)
    deadline = time.time() + 60
    while time.time() < deadline and '"switch"' not in p["log"].read_text():
        time.sleep(0.2)
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
    assert any("Stop-Process" in c for c in _ps_commands(p["log"]))
    _assert_rig_restored(p)


def test_a_recording_that_never_stops_is_a_loud_exit_5(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_STOP_NEVER="stream"), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    assert "RECORDING MAY STILL BE RUNNING on stream" in r.stdout + r.stderr
    state = (p["run"] / "recording.state").read_text()
    assert "stream=1" in state and "strih=0" in state


def test_stop_leftovers_stops_only_the_soaks_own_recording(rig):
    env, p = rig
    p["run"].mkdir()
    (p["run"] / "recording.state").write_text("strih=0\nstream=1\n")
    (p["state"] / "recording-stream").write_text("")
    r = _soak(env, "--stop-leftovers", str(p["run"]))
    assert r.returncode == 0, r.stdout + r.stderr
    stops = [c for c in _calls(p["log"], "obs") if c[:1] == ["record"] and "stop" in c]
    assert [_host(c) for c in stops] == [STREAM]
    assert not (p["state"] / "recording-stream").exists()
    assert "stream=0" in (p["run"] / "recording.state").read_text()


def test_stop_leftovers_never_stops_a_streaming_box(rig):
    env, p = rig
    p["run"].mkdir()
    (p["run"] / "recording.state").write_text("strih=0\nstream=1\n")
    (p["state"] / "recording-stream").write_text("")
    r = _soak(dict(env, FAKE_STREAMING_BOX="stream"), "--stop-leftovers", str(p["run"]))
    assert r.returncode == 0, r.stdout + r.stderr
    assert not [c for c in _calls(p["log"], "obs") if c[:1] == ["record"] and "stop" in c]
    assert "NOT stopping" in r.stdout + r.stderr


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
    r = _soak(dict(env, AV_SOAK_SEGMENT_SECS="300"), "--plan")
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


def test_a_painter_that_stops_emitting_gives_a_skipped_row(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_PAINTER_FAIL_AFTER="1"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert _csv_rows(p["run"])[0]["outcome"] == "skipped:painter_not_emitting"
    assert not [c for c in _calls(p["log"], "obs") if c[:1] == ["record"]]
    _assert_rig_restored(p)


def test_a_stream_program_that_left_the_development_scene_gives_a_skipped_row(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_STREAM_PROGRAM_DRIFT_AFTER="1"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert _csv_rows(p["run"])[0]["outcome"] == "skipped:stream_not_dev_scene"
    assert not [c for c in _calls(p["log"], "obs") if c[:1] == ["record"]]


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
    assert not [c for c in obs if c[0] in ("record", "switch", "connect-on-show")]
    assert not [c for c in _calls(p["log"], "burn") if c[0] == "add"]
    assert not p["lease"].exists()


def test_a_busy_rig_is_refused_before_any_mutation(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_BUSY="1"), "--run")
    assert r.returncode == 4, r.stdout + r.stderr
    obs = _calls(p["log"], "obs")
    assert not [c for c in obs if c[0] in ("record", "switch", "program-scene", "connect-on-show")]
    assert not _calls(p["log"], "burn")
    assert not p["lease"].exists()


def test_a_dead_painter_is_refused(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_PAINTER_ACTIVE="inactive"), "--run")
    assert r.returncode == 4, r.stdout + r.stderr
    assert not [c for c in _calls(p["log"], "obs") if c[0] in ("record", "connect-on-show")]


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
    head_only = tmp_path / "h.csv"
    head_only.write_text("index,frame_id,emit_ts_ns\n")
    wrong = tmp_path / "w.csv"
    wrong.write_text("tick,gen_ts_ns\n1,2\n")
    out = _lib(tmp_path, f"""
        for f in {good} {head_only} {wrong} {tmp_path}/absent.csv; do
          av_soak_marker_csv_ok "$f" && echo ok || echo bad
        done
    """)
    assert out.split() == ["ok", "bad", "bad", "bad"]


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


def test_lib_probe_and_snapshot_commands_are_read_only(tmp_path):
    out = _lib(tmp_path, """
        av_soak_painter_probe_cmd /run/rig-qpsk-markers.csv
        av_soak_marker_snapshot_cmd /run/rig-qpsk-markers.csv 1320
    """)
    body = out.replace("2>/dev/null", "").replace("< '", "")
    for banned in ("systemctl stop", "systemctl start", "systemctl restart", "rm ", "kill", ">", "tee"):
        assert banned not in body, banned
    assert out.rstrip().endswith(";")
