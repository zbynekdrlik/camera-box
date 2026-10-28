"""Issue 1460 slice 1 -- the VBAN receiver bench orchestrator (scripts/vban-receiver-bench.sh).

The bench is driven end to end with fakes behind its seams, never the rig:
  * a fake `sshpass` + `ssh` on PATH answer the cam2 TEST-mode probe, the strih-snv port / ffmpeg /
    OBS-log reads and the audio pull from per-test files;
  * VBAN_BENCH_OBS_DIR holds a fake obs_phase2.py (rig-busy-check + record, with the bench's own
    recording reflected in the busy read) and a fake vban_bench_obs.py (preflight / setup /
    teardown, an "input" marker for what setup created);
  * VBAN_BENCH_EMULATOR is a wrapper that runs the REAL emulator in --dry-run (the schedule file is
    the real one) or hangs until it is signalled;
  * RIG_LEASE_DIR is a temp dir (never the real /var/tmp/rig-lease, an E2E may hold it).
The sine analysis and the log grade are the real scripts, on a recording synthesized from the
real planner's pilot. Every refusal must happen before anything changed (exit 4, no setup, no
record start), every abort after a change must clean up (exit 5), and the plan touches nothing.
"""
import json
import os
import pathlib
import signal
import site
import subprocess
import sys
import time
import wave

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / "scripts"
BENCH = SCRIPTS / "vban-receiver-bench.sh"
sys.path.insert(0, str(SCRIPTS))

import vban_emulator_plan as vp  # noqa: E402

INPUT = "VBAN bench (issue 1460)"
CAM_PW = "fake-cam-word"
STRIH_PW = "fake-strih-word"
REC_PATH = "/srv/_REC/2026-10-22 10-00-00.mkv"

FAKE_SSHPASS = r"""#!/bin/bash
# fake sshpass: log the call shape (never what follows -p), then run the rest
case "$1" in
  -e) [ -n "${SSHPASS:-}" ] || { echo "fake sshpass: -e without SSHPASS" >&2; exit 97; }
      printf 'sshpass -e %s\n' "$*" >> "$FAKE_LOG"; shift ;;
  -p) printf 'sshpass -p\n' >> "$FAKE_LOG"; shift 2 ;;
  *) echo "fake sshpass: unexpected $1" >&2; exit 98 ;;
esac
exec "$@"
"""

FAKE_SSH = r"""#!@PY@
import os, sys
args = sys.argv[1:]
cmd = args[-1]
rest, i = [], 0
while i < len(args) - 1:
    if args[i] == "-o":
        i += 2
        continue
    rest.append(args[i])
    i += 1
host = rest[-1] if rest else ""
d = os.environ["FAKE_DIR"]
with open(os.environ["FAKE_LOG"], "a") as f:
    f.write("ssh %s %s\n" % (host, cmd.replace("\n", " | ")))
if "RIG_MODE_PROBE_OK" in cmd:
    seq = os.environ.get("FAKE_MODE", "TEST").split(",")
    counter = os.path.join(d, "mode-probes")
    n = int(open(counter).read()) if os.path.exists(counter) else 0
    with open(counter, "w") as f:
        f.write(str(n + 1))
    mode = seq[min(n, len(seq) - 1)]
    if mode == "NOANSWER":
        sys.exit(255)
    on = "1" if mode == "TEST" else "0"
    print("RIG_MODE_PROBE_OK\nPID_PRESENT|0\nPID_ALIVE|0\nSVC_ENABLED|%s\nSVC_ACTIVE|%s" % (on, on))
elif "ss -Hlun" in cmd:
    if os.environ.get("FAKE_PORT_BOUND"):
        print("UNCONN 0 0 0.0.0.0:6995 0.0.0.0:*")
    print("SS_RC=0")
elif "command -v ffmpeg" in cmd:
    sys.exit(0 if os.environ.get("FAKE_HAS_FFMPEG", "1") == "1" else 1)
elif "wc -l" in cmd:
    print("100")
elif "tail -n +" in cmd:
    with open(os.path.join(d, "obs.log")) as f:
        sys.stdout.write(f.read())
elif "ffmpeg" in cmd and "-map 0:a:" in cmd:
    with open(os.path.join(d, "audio.wav"), "rb") as f:
        sys.stdout.buffer.write(f.read())
else:
    sys.exit(255)
"""

FAKE_OBS_PHASE2 = r"""#!@PY@
import json, os, sys, time
d = os.environ["FAKE_DIR"]
args = sys.argv[1:]
with open(os.environ["FAKE_LOG"], "a") as f:
    f.write("obs_phase2 " + " ".join(args) + "\n")
rec = os.path.join(d, "recording")
def tc():
    age = time.time() - os.path.getmtime(rec)
    h, r = divmod(age, 3600)
    m, s = divmod(r, 60)
    return "%02d:%02d:%06.3f" % (h, m, s)
sub = args[0]
if sub == "rig-busy-check":
    mode = os.environ.get("FAKE_BUSY", "idle")
    if os.path.exists(os.path.join(d, "live")):
        mode = "streaming"
    if os.path.exists(os.path.join(d, "unreadable")):
        mode = "unreadable"
    if mode == "unreadable":
        print(json.dumps({"busy": None, "diagnostics": []}))
        sys.exit(3)
    strih_rec = os.path.exists(rec) or mode == "recording"
    streaming = mode == "streaming"
    diag = [{"host": "strih", "streaming": False, "recording": strih_rec,
             "recordTimecode": tc() if os.path.exists(rec) else None},
            {"host": "stream", "streaming": streaming, "recording": streaming}]
    print(json.dumps({"busy": strih_rec or streaming, "diagnostics": diag}))
elif sub == "record":
    action = args[args.index("--action") + 1]
    if action == "start":
        if os.environ.get("FAKE_RECORD_START_FAIL"):
            sys.exit(1)
        open(rec, "w").close()
    elif action == "stop":
        if not os.environ.get("FAKE_STOP_FAILS"):
            if os.path.exists(rec):
                os.remove(rec)
            if os.environ.get("FAKE_STOP_LAG"):
                open(os.path.join(d, "stop-lag"), "w").write(os.environ["FAKE_STOP_LAG"])
        print(os.environ.get("FAKE_REC_PATH", "/srv/_REC/2026-10-22 10-00-00.mkv"))
    elif action == "status":
        # FAKE_STOP_LAG: OBS still finishing the file for the first N status reads after a stop
        # (live strih-snv 28.10.2026).
        lag = os.path.join(d, "stop-lag")
        n = int(open(lag).read()) if os.path.exists(lag) else 0
        if n > 0:
            open(lag, "w").write(str(n - 1))
            print("active=True path=")
        else:
            print("active=%s path=" % os.path.exists(rec))
"""

FAKE_BENCH_OBS = r"""#!@PY@
import json, os, sys
d = os.environ["FAKE_DIR"]
args = sys.argv[1:]
with open(os.environ["FAKE_LOG"], "a") as f:
    f.write("vban_bench_obs " + " ".join(args) + "\n")
sub = args[0]
state = args[args.index("--state") + 1]
marker = os.path.join(d, "input")
if sub == "preflight":
    rc = int(os.environ.get("FAKE_PREFLIGHT_RC", "0"))
    print(json.dumps({"reasons": ["fake refusal"] if rc else [], "track_feeders": []}))
    sys.exit(rc)
if sub == "setup":
    rc = int(os.environ.get("FAKE_SETUP_RC", "0"))
    if rc in (2, 4):          # unreachable / refused: nothing created (vban_bench_obs.py's contract)
        sys.exit(rc)
    with open(state, "w") as f:
        json.dump({"created": True, "input_name": "x"}, f)
    open(marker, "w").close()
    if rc:
        sys.exit(rc)
    print(json.dumps({"audio_stream_index": 1, "program_scene": "Cam 1"}))
elif sub == "teardown":
    rc = int(os.environ.get("FAKE_TEARDOWN_RC", "0"))
    if not rc and os.path.exists(marker):
        os.remove(marker)
    if os.environ.get("FAKE_UNREADABLE_AFTER_TEARDOWN"):
        open(os.path.join(d, "unreadable"), "w").close()
    sys.exit(rc)
"""

FAKE_CURL = r"""#!@PY@
import os, sys
with open(os.environ["FAKE_LOG"], "a") as f:
    f.write("curl " + " ".join(sys.argv[1:]) + "\n")
print('{"free_bytes": %s}' % os.environ.get("FAKE_FREE_BYTES", "500000000000"))
"""

FAKE_EMULATOR = r"""#!@PY@
import json, os, signal, subprocess, sys, time
d = os.environ["FAKE_DIR"]
with open(os.path.join(d, "emulator.pid"), "w") as f:
    f.write(str(os.getpid()))
rc = subprocess.run([sys.executable, os.environ["REAL_EMULATOR"], *sys.argv[1:], "--dry-run"],
                    stdout=subprocess.DEVNULL).returncode
if rc:
    sys.exit(rc)
# the real sender's stats file (--dry-run writes none): its send lateness
stats = sys.argv[sys.argv.index("--stats-out") + 1]
with open(stats, "w") as f:
    json.dump({"sent": 1, "lost": 0, "max_late_ms": float(os.environ.get("FAKE_EMU_LATE_MS", "0.3"))}, f)
if os.environ.get("FAKE_EMU_HANG"):
    def stop(signum, frame):
        open(os.path.join(d, "emulator.terminated"), "w").close()
        sys.exit(5)
    signal.signal(signal.SIGTERM, stop)
    open(os.path.join(d, "emulator.running"), "w").close()
    time.sleep(600)
"""


def _write_exe(path, text):
    path.write_text(text.replace("@PY@", sys.executable))
    path.chmod(0o755)


@pytest.fixture
def rig(tmp_path):
    fake = tmp_path / "fake"
    stub = tmp_path / "bin"
    obsdir = tmp_path / "obs"
    for p in (fake, stub, obsdir):
        p.mkdir()
    _write_exe(stub / "sshpass", FAKE_SSHPASS)
    _write_exe(stub / "ssh", FAKE_SSH)
    _write_exe(stub / "curl", FAKE_CURL)
    _write_exe(obsdir / "obs_phase2.py", FAKE_OBS_PHASE2)
    _write_exe(obsdir / "vban_bench_obs.py", FAKE_BENCH_OBS)
    _write_exe(tmp_path / "emulator.py", FAKE_EMULATOR)
    (fake / "obs.log").write_text("")
    log = tmp_path / "calls.log"
    log.write_text("")
    env = dict(os.environ)
    for k in list(env):
        if k.startswith(("VBAN_BENCH_", "FAKE_", "RIG_LEASE_", "AV_SOAK_")) or k in ("OBS_PASSWORD", "STRIH_PLATFORM"):
            del env[k]
    env.update({
        "PATH": "%s:%s" % (stub, os.environ["PATH"]),
        "HOME": str(tmp_path / "home"), "PYTHONUSERBASE": site.getuserbase(),
        "FAKE_DIR": str(fake), "FAKE_LOG": str(log),
        "VBAN_BENCH_OBS_DIR": str(obsdir), "VBAN_BENCH_EMULATOR": str(tmp_path / "emulator.py"),
        "REAL_EMULATOR": str(SCRIPTS / "vban_emulator.py"),
        "RIG_LEASE_DIR": str(tmp_path / "lease"),
        "STRIH_HOST": "192.0.2.10", "STREAM_HOST": "192.0.2.11", "PAINTER_IP": "192.0.2.12",
        "STRIH_PLATFORM": "linux", "CAM_PW": CAM_PW, "STRIH_USER": "newlevel", "STRIH_PW": STRIH_PW,
        "AV_SOAK_BROADCAST_RETRY_S": "0", "VBAN_BENCH_POLL_S": "1", "VBAN_BENCH_MODE_RETRY_S": "0",
    })
    (tmp_path / "home").mkdir()

    class Rig:
        dir = tmp_path
        fake_dir = fake
        run_dir = tmp_path / "run"
        lease = tmp_path / "lease"

        def __init__(self):
            self.env = env

        def calls(self):
            return log.read_text()

        def run(self, *args, timeout=240, **extra):
            e = dict(self.env, **extra)
            return subprocess.run(["bash", str(BENCH), *args], env=e, capture_output=True, text=True,
                                  timeout=timeout)

        def popen(self, *args, **extra):
            e = dict(self.env, **extra)
            return subprocess.Popen(["bash", str(BENCH), *args], env=e, stdout=subprocess.PIPE,
                                    stderr=subprocess.STDOUT, text=True, start_new_session=True)

    return Rig()


def _synth_recording(path, duration_s, losses=(), lead_s=3.0, tail_s=2.0, pilot_hz=997.0):
    """What the receiver should record: lead silence, the planner's own pilot (a lost packet =
    silence for exactly its slot), tail silence. WAV stereo PCM16 48 kHz."""
    amp = vp.amplitude(vp.DEFAULT_LEVEL_DBFS)
    n = vp.packet_count(duration_s, vp.ppm_units(0.0))
    lost = set()
    for lo in losses:
        lost.update(range(lo["packet"], lo["packet"] + lo["count"]))
    silence = bytes(4 * vp.FRAMES_PER_PACKET)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(vp.SAMPLE_RATE)
        w.writeframes(bytes(int(4 * vp.SAMPLE_RATE * lead_s)))
        for i in range(n):
            w.writeframes(silence if i in lost else
                          vp.pilot_block(i * vp.FRAMES_PER_PACKET, vp.FRAMES_PER_PACKET, pilot_hz, amp))
        w.writeframes(bytes(int(4 * vp.SAMPLE_RATE * tail_s)))


ASRC_LINE = ("13:01:02.345: asrc: source '%s' estimated=%s applied=0.10ppm starved_blocks=0 level=0.3ms "
             "target=0.0ms steps=0 last_step_ms=0.0 level_avg=0.20ms (#1367) timecode=1 place_jumps=0 "
             "last_jump_ms=0.0 basis=stamp (issue 1425)\n")


# --- usage + plan ---------------------------------------------------------------------------------

@pytest.mark.parametrize("args", [
    ["--port", "6980"], ["--port", "6981"], ["--port", "6990"], ["--port", "80"], ["--port", "x"],
    ["--track", "1"], ["--track", "7"], ["--minutes", "1"], ["--minutes", "15"],
    ["--profiles", "loud"], ["--profiles", "stress:fast"], ["--profiles", ""],
    ["--stream-name", "seventeen-chars-x"], ["--stream-name", "two words"], ["--bogus"],
    ["--extra-setting", "port=6980"], ["--extra-setting", "stream_name=x"], ["--extra-setting", "novalue"],
])
def test_usage_errors_exit_3(rig, args):
    r = rig.run("--plan", *args)
    assert r.returncode == 3, (args, r.stdout, r.stderr)


def test_the_plan_touches_nothing_and_lists_every_profile(rig):
    r = rig.run("--run-dir", str(rig.run_dir))
    assert r.returncode == 0, r.stderr
    out = r.stdout
    assert "nothing below has run" in out
    assert "--profile clean" in out and "--profile lv1" in out
    assert "--profile stress --ppm +50" in out and "--profile stress --ppm -50" in out
    assert "port 6995" in out and "track 6 ONLY" in out and INPUT in out
    assert "--target 192.0.2.10" in out and "--duration-s 600" in out
    assert not rig.run_dir.exists() and not rig.lease.exists()
    assert rig.calls() == ""


def test_the_plan_carries_the_extra_setting_and_the_options(rig):
    r = rig.run("--plan", "--profiles", "lv1", "--minutes", "3", "--port", "7001", "--track", "5",
                "--extra-setting", "receive_reserve_ms=40", "--run-dir", str(rig.run_dir))
    assert r.returncode == 0, r.stderr
    assert "--extra-setting receive_reserve_ms=40" in r.stdout
    assert "--port 7001" in r.stdout and "--track 5" in r.stdout and "--duration-s 180" in r.stdout
    assert "--profile stress" not in r.stdout


# --- refusals before any change -------------------------------------------------------------------

def _assert_untouched(rig, setup_asked=False):
    calls = rig.calls()
    assert ("vban_bench_obs setup" in calls) == setup_asked, calls
    assert "--action start" not in calls and "vban_bench_obs teardown" not in calls, calls
    assert not (rig.fake_dir / "input").exists() and not (rig.fake_dir / "recording").exists()


@pytest.mark.parametrize("extra,why", [
    ({"FAKE_MODE": "EVENT"}, "not in TEST mode"),
    ({"FAKE_MODE": "NOANSWER"}, "not in TEST mode"),
    ({"FAKE_BUSY": "streaming"}, "not provably idle"),
    ({"FAKE_BUSY": "recording"}, "not provably idle"),
    ({"FAKE_BUSY": "unreadable"}, "not provably idle"),
    ({"FAKE_PREFLIGHT_RC": "4"}, "OBS preflight refused"),
    ({"FAKE_PORT_BOUND": "1"}, "already bound"),
    ({"FAKE_SETUP_RC": "4"}, "refused before creating"),
    ({"FAKE_SETUP_RC": "2"}, "unreachable for the setup"),
    ({"FAKE_HAS_FFMPEG": "0"}, "ffmpeg is not on strih-snv"),
    ({"FAKE_FREE_BYTES": "20000000000"}, "low on space"),
])
def test_an_unsafe_rig_is_refused_before_any_change(rig, extra, why):
    r = rig.run("--run", "--profiles", "clean", "--minutes", "2", "--run-dir", str(rig.run_dir), **extra)
    assert r.returncode == 4, r.stdout + r.stderr
    assert why in r.stdout + r.stderr
    _assert_untouched(rig, setup_asked="FAKE_SETUP_RC" in extra)
    assert "cat -- " not in rig.calls(), "the whole program recording is never pulled"
    assert not rig.lease.exists(), "the lease must be released on a refusal"


def test_a_foreign_lease_is_refused_and_left_alone(rig):
    rig.lease.mkdir()
    holder = {"repo": "restreamer", "run_id": "42", "run_url": "u", "job": "j",
              "acquired_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "expected_release_at": "2099-01-01T00:00:00Z"}
    (rig.lease / "holder.json").write_text(json.dumps(holder))
    (rig.lease / "heartbeat").write_text("")
    r = rig.run("--run", "--profiles", "clean", "--minutes", "2", "--run-dir", str(rig.run_dir))
    assert r.returncode == 4 and "lease is held" in r.stdout + r.stderr
    assert json.loads((rig.lease / "holder.json").read_text())["repo"] == "restreamer"
    assert "RIG_MODE_PROBE_OK" not in rig.calls()     # nothing read or written on the rig
    _assert_untouched(rig)


@pytest.mark.parametrize("unset", ["CAM_PW", "STRIH_USER", "STRIH_PW"])
def test_missing_credentials_refuse(rig, unset):
    r = rig.run("--run", "--profiles", "clean", "--minutes", "2", "--run-dir", str(rig.run_dir), **{unset: ""})
    assert r.returncode == 4 and unset in r.stderr
    assert rig.calls() == ""


def test_a_windows_strih_is_refused(rig):
    r = rig.run("--run", "--profiles", "clean", "--minutes", "2", "--run-dir", str(rig.run_dir),
                STRIH_PLATFORM="windows")
    assert r.returncode == 4 and "not the Linux strih-snv" in r.stderr
    assert rig.calls() == ""


def test_a_used_run_dir_is_refused(rig):
    rig.run_dir.mkdir()
    (rig.run_dir / "bench.tsv").write_text("old\n")
    r = rig.run("--run", "--profiles", "clean", "--minutes", "2", "--run-dir", str(rig.run_dir))
    assert r.returncode == 4 and "already holds a run" in r.stderr
    assert (rig.run_dir / "bench.tsv").read_text() == "old\n"


# --- a full run ---------------------------------------------------------------------------------

def _clean_run(rig, log_lines, **extra):
    _synth_recording(rig.fake_dir / "audio.wav", 120.0)
    (rig.fake_dir / "obs.log").write_text("".join(log_lines))
    return rig.run("--run", "--profiles", "clean", "--minutes", "2", "--run-dir", str(rig.run_dir), **extra)


def test_a_recording_that_reads_active_for_a_moment_after_stoprecord_still_passes(rig):
    # Live 28.10.2026: one status read right after StopRecord said active=True while OBS finished
    # the file, and the bench aborted (exit 5) on a healthy rig.
    r = _clean_run(rig, [ASRC_LINE % (INPUT, "0.12ppm"),
                         "13:01:10.000: vban-source '%s': received=22500 lost=0 late=0 reanchors=0\n" % INPUT],
                   FAKE_STOP_LAG="2", VBAN_BENCH_STOP_POLL_S="0.05")
    out = r.stdout + r.stderr
    assert r.returncode == 0, out
    assert "still reads" not in out


def test_a_clean_profile_passes_end_to_end(rig):
    r = _clean_run(rig, [ASRC_LINE % (INPUT, "0.12ppm"),
                         "13:01:10.000: vban-source '%s': received=22500 lost=0 late=0 reanchors=0\n" % INPUT])
    out = r.stdout + r.stderr
    assert r.returncode == 0, out
    row = (rig.run_dir / "bench.tsv").read_text().splitlines()[1].split("\t")
    assert row[:6] == ["clean", "0.0", "PASS", "PASS", "0.30", "PASS"] and row[6] == REC_PATH
    analysis = json.loads((rig.run_dir / "p1-analysis.json").read_text())
    assert analysis["verdict"] == "PASS"
    calls = rig.calls()
    # the order the design asks for: guard, setup, StartRecord, emulator, StopRecord, teardown
    order = [calls.index(s) for s in ("vban_bench_obs preflight", "vban_bench_obs setup",
                                      "--action start", "--action stop", "vban_bench_obs teardown")]
    assert order == sorted(order)
    assert "-map 0:a:1 -c copy" in calls                    # the test track, by its stream index
    assert not (rig.fake_dir / "input").exists() and not (rig.fake_dir / "recording").exists()
    assert not rig.lease.exists()
    plan = (rig.run_dir / "cleanup-plan.txt").read_text()
    assert "rm -f -- '%s'" % REC_PATH in plan
    # the passwords travel in the environment, never on an sshpass -e command line
    for line in calls.splitlines():
        if line.startswith("sshpass -e"):
            assert CAM_PW not in line and STRIH_PW not in line, line


EMITTING_LINE = ("13:01:01.000: [obs-vban] obs-vban receive emitting: source '%s' frame=%d margin_ms=34.6 "
                 "after the first second's hold -- every slot goes out by 2 ms before its stamp\n")


def test_the_sine_grade_gets_the_receivers_own_hold(rig):
    # the concealed losses are placed on the recording from the frames the receiver held, read from
    # the profile's own OBS log -- never another source's line (the strih program receiver logs too)
    r = _clean_run(rig, [EMITTING_LINE % ("ASIO zvuk", 999), EMITTING_LINE % (INPUT, 200),
                         ASRC_LINE % (INPUT, "0.12ppm"),
                         "13:01:10.000: vban-source '%s': received=22500 lost=0 late=0 reanchors=0\n" % INPUT])
    assert r.returncode == 0, r.stdout + r.stderr
    assert json.loads((rig.run_dir / "p1-analysis.json").read_text())["hold_frames"] == 200
    assert "the receiver held 200 frames" in r.stdout + r.stderr


def test_without_the_emitting_line_the_measured_hold_is_used(rig):
    r = _clean_run(rig, [ASRC_LINE % (INPUT, "0.12ppm"),
                         "13:01:10.000: vban-source '%s': received=22500 lost=0 late=0 reanchors=0\n" % INPUT])
    assert r.returncode == 0, r.stdout + r.stderr
    assert json.loads((rig.run_dir / "p1-analysis.json").read_text())["hold_frames"] == 187
    # a fallback is named, so a renamed log line never goes unnoticed
    assert "with the default hold of 187 frames" in r.stdout + r.stderr


def test_a_buffering_guard_on_the_test_input_fails_the_profile(rig):
    r = _clean_run(rig, [ASRC_LINE % (INPUT, "0.12ppm"),
                         "13:02:00.000: buffering-guard: '%s' mixed late: its audio ran 40.0 ms behind\n" % INPUT])
    assert r.returncode == 1, r.stdout + r.stderr
    row = (rig.run_dir / "bench.tsv").read_text().splitlines()[1].split("\t")
    assert row[2:6] == ["PASS", "FAIL", "0.30", "FAIL"]
    assert not (rig.fake_dir / "input").exists() and not rig.lease.exists()


def test_a_late_emulator_makes_the_profile_invalid_never_a_receiver_pass(rig):
    r = _clean_run(rig, [ASRC_LINE % (INPUT, "0.12ppm")], FAKE_EMU_LATE_MS="9.5")
    assert r.returncode == 1, r.stdout + r.stderr
    row = (rig.run_dir / "bench.tsv").read_text().splitlines()[1].split("\t")
    assert row[2:6] == ["PASS", "PASS", "9.50", "INVALID"]
    assert not (rig.fake_dir / "input").exists() and not rig.lease.exists()


def _two_profiles(rig, **extra):
    _synth_recording(rig.fake_dir / "audio.wav", 120.0)
    (rig.fake_dir / "obs.log").write_text(ASRC_LINE % (INPUT, "0.12ppm"))
    return rig.popen("--run", "--profiles", "clean clean", "--minutes", "2", "--run-dir", str(rig.run_dir),
                     **extra)


def test_a_stop_before_every_profile_ran_is_incomplete_never_pass(rig):
    p = _two_profiles(rig)
    _wait_for(rig.fake_dir / "emulator.pid", p)      # profile 1 runs (and will pass)
    (rig.run_dir / "STOP").write_text("")
    out, _ = p.communicate(timeout=180)
    assert p.returncode == 1, out
    assert "verdict: INCOMPLETE (1 of 2 profiles ran)" in out
    assert rig.calls().count("vban_bench_obs setup") == 1
    assert not rig.lease.exists()


def test_a_rig_that_left_test_mode_ends_the_run_incomplete(rig):
    p = _two_profiles(rig, FAKE_MODE="TEST,EVENT")      # the reads, then before profile 2
    out, _ = p.communicate(timeout=180)
    assert p.returncode == 1, out
    assert "left TEST mode" in out and "INCOMPLETE (1 of 2 profiles ran)" in out
    assert rig.calls().count("vban_bench_obs setup") == 1
    assert not (rig.fake_dir / "input").exists() and not rig.lease.exists()


def test_one_unreadable_painter_read_is_retried_not_the_end_of_the_run(rig):
    p = _two_profiles(rig, FAKE_MODE="TEST,NOANSWER,TEST")   # the reads, a hiccup, the retry
    out, _ = p.communicate(timeout=180)
    assert p.returncode == 0, out
    assert "reading it again" in out and rig.calls().count("vban_bench_obs setup") == 2


def test_the_next_profile_needs_a_proven_idle_rig_before_its_setup(rig):
    # after profile 1 the rig turns unreadable: the guard alone would fail open, the idle read refuses
    _synth_recording(rig.fake_dir / "audio.wav", 120.0)
    (rig.fake_dir / "obs.log").write_text(ASRC_LINE % (INPUT, "0.12ppm"))
    r = rig.run("--run", "--profiles", "clean clean", "--minutes", "2", "--run-dir", str(rig.run_dir),
                FAKE_UNREADABLE_AFTER_TEARDOWN="1")
    assert r.returncode == 5, r.stdout + r.stderr
    assert "not provably idle before the VBAN bench test input setup" in r.stdout + r.stderr
    assert rig.calls().count("vban_bench_obs setup") == 1


# --- aborts after a change ------------------------------------------------------------------------

def _wait_for(path, proc, timeout=60):
    end = time.time() + timeout
    while time.time() < end:
        if path.exists():
            return
        if proc.poll() is not None:
            raise AssertionError("the bench ended early: %s" % proc.stdout.read())
        time.sleep(0.2)
    raise AssertionError("timed out waiting for %s" % path)


def test_sigterm_mid_profile_stops_everything_and_releases_the_lease(rig):
    p = rig.popen("--run", "--profiles", "clean", "--minutes", "2", "--run-dir", str(rig.run_dir),
                  FAKE_EMU_HANG="1")
    _wait_for(rig.fake_dir / "emulator.running", p)
    assert (rig.fake_dir / "recording").exists() and (rig.fake_dir / "input").exists()
    os.kill(p.pid, signal.SIGTERM)
    out, _ = p.communicate(timeout=120)
    assert p.returncode == 5, out
    assert (rig.fake_dir / "emulator.terminated").exists()
    assert not (rig.fake_dir / "recording").exists() and not (rig.fake_dir / "input").exists()
    assert not rig.lease.exists()
    assert "ABORTED: SIGTERM" in out


def test_a_broadcast_going_live_keeps_the_recording_and_the_lease_then_cleanup_finishes(rig):
    p = rig.popen("--run", "--profiles", "clean", "--minutes", "2", "--run-dir", str(rig.run_dir),
                  FAKE_EMU_HANG="1")
    _wait_for(rig.fake_dir / "emulator.running", p)
    (rig.fake_dir / "live").write_text("")
    out, _ = p.communicate(timeout=120)
    assert p.returncode == 5, out
    assert "a broadcast went live" in out
    assert (rig.fake_dir / "emulator.terminated").exists()
    assert (rig.fake_dir / "recording").exists(), "never stop a recording during a broadcast"
    assert not (rig.fake_dir / "input").exists(), "the silent test input is removed anyway"
    assert rig.lease.exists(), "the lease stays while a recording may be ours"
    # while live, --cleanup keeps everything
    r = rig.run("--cleanup", str(rig.run_dir))
    assert r.returncode == 5 and (rig.fake_dir / "recording").exists() and rig.lease.exists()
    # once idle, --cleanup proves the recording is the bench's, stops it and releases the lease
    (rig.fake_dir / "live").unlink()
    r = rig.run("--cleanup", str(rig.run_dir))
    assert r.returncode == 0, r.stdout + r.stderr
    assert not (rig.fake_dir / "recording").exists() and not rig.lease.exists()


def test_cleanup_keeps_the_lease_when_the_recording_does_not_stop(rig):
    p = rig.popen("--run", "--profiles", "clean", "--minutes", "2", "--run-dir", str(rig.run_dir),
                  FAKE_EMU_HANG="1")
    _wait_for(rig.fake_dir / "emulator.running", p)
    (rig.fake_dir / "live").write_text("")
    p.communicate(timeout=120)
    (rig.fake_dir / "live").unlink()
    r = rig.run("--cleanup", str(rig.run_dir), FAKE_STOP_FAILS="1")
    assert r.returncode == 5, r.stdout + r.stderr
    assert (rig.fake_dir / "recording").exists() and rig.lease.exists()
    assert "strih=1" in (rig.run_dir / "recording.state").read_text()


def test_a_teardown_that_fails_keeps_the_lease(rig):
    r = _clean_run(rig, [ASRC_LINE % (INPUT, "0.12ppm")], FAKE_TEARDOWN_RC="5")
    assert r.returncode == 5, r.stdout + r.stderr
    assert rig.lease.exists() and (rig.fake_dir / "input").exists()
    assert "--cleanup" in r.stdout + r.stderr


def test_a_setup_that_fails_after_creating_cleans_up(rig):
    r = rig.run("--run", "--profiles", "clean", "--minutes", "2", "--run-dir", str(rig.run_dir),
                FAKE_SETUP_RC="5")
    assert r.returncode == 5, r.stdout + r.stderr
    assert "--action start" not in rig.calls()
    assert not (rig.fake_dir / "input").exists() and not rig.lease.exists()


def test_a_failed_start_record_cleans_up(rig):
    r = rig.run("--run", "--profiles", "clean", "--minutes", "2", "--run-dir", str(rig.run_dir),
                FAKE_RECORD_START_FAIL="1")
    assert r.returncode == 5, r.stdout + r.stderr
    assert not (rig.fake_dir / "input").exists() and not rig.lease.exists()
    assert "emulator.pid" not in os.listdir(rig.fake_dir)


def test_cleanup_refuses_while_the_bench_runs(rig):
    p = rig.popen("--run", "--profiles", "clean", "--minutes", "2", "--run-dir", str(rig.run_dir),
                  FAKE_EMU_HANG="1")
    try:
        _wait_for(rig.fake_dir / "emulator.running", p)
        r = rig.run("--cleanup", str(rig.run_dir))
        assert r.returncode == 4 and "still runs" in r.stderr
        assert (rig.fake_dir / "input").exists()
    finally:
        os.kill(p.pid, signal.SIGTERM)
        p.communicate(timeout=120)
