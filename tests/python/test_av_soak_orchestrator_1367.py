"""issue 1367 -- the 8 h stream-output A/V soak orchestrator (scripts/av-soak.sh) + its pure lib
(scripts/lib/av-soak.sh), driven end to end with FAKES (Tier-0: no rig, no cargo, no network).

Fakes on PATH / behind the documented seams:
  - obs_phase2.py + obs_burn_filter.py in AV_SOAK_OBS_DIR (log every call; answer rig-busy-check,
    program-scene, record, switch, burn check/add/remove),
  - sshpass (cam2 painter probe + marker-log tail, the stream-box New-Item/scp) and curl (the
    record-volume free space) on PATH,
  - AV_SOAK_STRIH_DECODE / AV_SOAK_STREAM_DECODE (write the partial the wrappers would pull back),
  - PROBE_BIN_DIR/recording-verdict (writes a merged verdict JSON, exits 1 like a failing gate),
  - RIG_LEASE_DIR + CAMERA_BOX_RIG_HEARTBEAT in a tmp dir -- the REAL dev1 lease and heartbeat are
    never touched (an E2E may hold the real lease while this runs).
"""
import json
import os
import stat
import subprocess
import textwrap

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
SOAK = os.path.join(REPO, "scripts", "av-soak.sh")
LIB = os.path.join(REPO, "scripts", "lib", "av-soak.sh")
STRIH = "10.77.9.202"
STREAM = "10.77.9.204"

FAKE_OBS = r'''#!/usr/bin/env python3
import json, os, sys, time
a = sys.argv[1:]
with open(os.environ["FAKE_LOG"], "a") as f:
    f.write("obs " + json.dumps(a) + "\n")
def arg(name, default=""):
    return a[a.index(name) + 1] if name in a else default
cmd = a[0]
if cmd == "rig-busy-check":
    busy = os.environ.get("FAKE_BUSY") == "1"
    print(json.dumps({"busy": busy, "diagnostics": [{"host": "stream", "streaming": busy, "recording": False}]}))
elif cmd == "stream-detail":
    pass
elif cmd == "program-scene":
    host = arg("--host")
    print(os.environ.get("FAKE_STREAM_PROGRAM", "Development") if host == os.environ["FAKE_STREAM_HOST"] else "Cam 1")
elif cmd == "record":
    act = arg("--action")
    if act == "stop":
        host = arg("--host")
        print("/srv/_REC/2026-09-27 21-00-00.mkv" if host != os.environ["FAKE_STREAM_HOST"] else "C:/_REC/2026-09-27 21-00-00.mp4")
elif cmd == "switch":
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
state = os.path.join(os.environ["FAKE_STATE"], (host + "_" + inp).replace(" ", "_"))
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
last="${!#}"
case "$*" in
  *"systemctl is-active cam2-painter"*)
    printf 'active=%s\nrun_id=4242\nmarkers=10\nmarkers2=14\n' "${FAKE_PAINTER_ACTIVE:-active}" ;;
  *"head -n 1"*)
    printf 'index,frame_id,emit_ts_ns\n1,100,5\n2,130,6\n' ;;
  *) : ;;
esac
'''

FAKE_CURL = r'''#!/usr/bin/env bash
printf 'curl %s\n' "$*" >> "$FAKE_LOG"
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
     "all_cambox_continuity": {"segments": []}}
for i, c in enumerate(cams):
    v["all_cambox_av_sync"][c] = {"verdict": "measured", "av_offset_ms": 3.0 + i, "gate_pass": True}
    v["all_cambox_continuity"]["segments"].append(
        {"cambox": c.upper(), "pass": True, "copies": 0, "gaps": 0, "undecodable": 0, "frames": 60})
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
           if k not in ("OBS_PASSWORD", "CAMBOX_OFFLINE_ACK", "STREAM_PROG_SCENE")}
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
        "CAM_PW": "x", "STREAM_USER": "u", "STREAM_PW": "y",
        "STRIH_HOST": STRIH, "STREAM_HOST": STREAM,
        "E2E_ONBOX_DECODE_PRIORITY": "BelowNormal",
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


# --- --plan touches nothing -----------------------------------------------------------------------


def test_plan_is_the_default_and_touches_nothing(rig):
    env, p = rig
    r = _soak(env)
    assert r.returncode == 0, r.stderr
    assert p["log"].read_text() == "", "plan mode must not call OBS, ssh, curl or a decode"
    assert not p["lease"].exists() and not p["hb"].exists() and not p["run"].exists()
    out = r.stdout
    for needle in ("rig_lease_acquire", "stray_session_check_assert", "record --host 10.77.9.202 --action start",
                   "switch --host 10.77.9.202 --program-scene Cam\\ 1", "record --host 10.77.9.204 --action stop",
                   "recording-verdict-on", "--merge-partials", "av_soak_decision.py row",
                   "--extract-partial strih", "--extract-partial stream", "av_tolerance_ms="):
        assert needle in out or needle.replace("recording-verdict-on", "decode.sh") in out, needle


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
    env = dict(env, STREAM_PROG_SCENE="PRO")
    r = _soak(env, "--plan")
    assert r.returncode == 3
    assert p["log"].read_text() == ""


def test_a_window_that_does_not_fit_the_slot_is_refused(rig):
    env, _ = rig
    r = _soak(dict(env, AV_SOAK_SEGMENT_SECS="400"), "--plan")
    assert r.returncode == 3
    assert "does not fit" in r.stderr


# --- one full window with fakes -------------------------------------------------------------------


def test_one_window_end_to_end(rig):
    env, p = rig
    r = _soak(env, "--run")
    # one window cannot give a slope -> UNKNOWN (exit 2), never a false PASS
    assert r.returncode == 2, r.stdout + r.stderr
    csv_rows = (p["run"] / "soak.csv").read_text().splitlines()
    assert len(csv_rows) == 2
    header = csv_rows[0].split(",")
    row = dict(zip(header, csv_rows[1].split(",")))
    assert row["outcome"] == "ok" and row["painter_run_id"] == "4242"
    assert row["av_cam1_ms"] == "3.000" and row["av_cam3_ms"] == "4.000"
    assert row["av_spread_ms"] == "1.000"
    assert (p["run"] / "report.txt").exists() and (p["run"] / "report.json").exists()
    assert "VERDICT: UNKNOWN" in r.stdout

    obs = _calls(p["log"], "obs")
    starts = [c for c in obs if c[:1] == ["record"] and "start" in c]
    stops = [c for c in obs if c[:1] == ["record"] and "stop" in c]
    assert {c[c.index("--host") + 1] for c in starts} == {STRIH, STREAM}
    assert {c[c.index("--host") + 1] for c in stops} == {STRIH, STREAM}
    switches = [c for c in obs if c[:1] == ["switch"]]
    assert all(c[c.index("--host") + 1] == STRIH for c in switches), "the stream program is never switched"
    assert [c[c.index("--program-scene") + 1] for c in switches] == ["Cam 1", "Cam 3", "Cam 1"], \
        "the sweep, then the strih program restored to its snapshot"
    assert not any("PRO" in c for c in obs)
    # the guard ran before the first mutation and before the StartRecord
    kinds = [c[0] for c in obs]
    assert kinds.count("rig-busy-check") >= 2
    assert kinds.index("rig-busy-check") < kinds.index("record")

    burns = _calls(p["log"], "burn")
    added = {c[c.index("--input") + 1] for c in burns if c[0] == "add"}
    removed = {c[c.index("--input") + 1] for c in burns if c[0] == "remove"}
    assert added == {"NDI cam1", "NDI cam3", "NDI 2ME PGM"} and removed == added
    assert os.listdir(p["state"]) == [], "every burn the soak turned on is off again"

    decodes = [line for line in p["log"].read_text().splitlines() if line.startswith("decode ")]
    assert any("--extract-partial strih" in d for d in decodes)
    assert any("--extract-partial stream" in d and "--cam2-run-id 4242" in d for d in decodes)
    merge = _calls(p["log"], "verdict")[0]
    assert "--merge-partials" in merge and "--switch-schedule" in merge
    assert merge[merge.index("--cam2-run-id") + 1] == "4242"
    assert "--av-expected-ms" not in merge, "the verdict's own default is the single source"
    assert not any(a.startswith("--burn-") for a in merge)

    assert not p["lease"].exists(), "the lease is released"
    assert not p["hb"].exists(), "the heartbeat is cleared"
    plan = (p["run"] / "cleanup-plan.txt").read_text()
    assert "rm -f -- '/srv/_REC/2026-09-27 21-00-00.mkv'" in plan
    assert "Remove-Item -Force -LiteralPath 'C:/_REC/2026-09-27 21-00-00.mp4'" in plan


def test_burns_already_on_are_left_on(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_BURNS_ON="1"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    burns = _calls(p["log"], "burn")
    assert not [c for c in burns if c[0] in ("add", "remove")]


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
    for banned in ("systemctl stop", "systemctl start", "systemctl restart", "rm ", "kill", ">", "tee"):
        body = out.replace("2>/dev/null", "").replace("< '", "")
        assert banned not in body, banned
    assert out.rstrip().endswith(";")
