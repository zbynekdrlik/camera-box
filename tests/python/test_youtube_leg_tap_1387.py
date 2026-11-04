"""Issue 1387 -- scripts/youtube_leg_tap.py, the restreamer-INPUT capture on the stream box (a REPORT-ONLY
point of the YouTube stage). The pure builders are checked directly; the remote bash (probe, start,
stop, purge) RUNS for real behind a fake ssh (YOUTUBE_LEG_TAP_SSH_CMD) with a fake HOME, a fake root
(YOUTUBE_LEG_TAP_ROOT: the cpu list + the OBS mask) and stub ffmpeg / chrt / taskset / ionice that log
their argv and exec the rest (real setsid, nohup, nice, kill)."""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
TAP = REPO / "scripts" / "youtube_leg_tap.py"
sys.path.insert(0, str(REPO / "scripts"))
import youtube_leg_tap as tap  # noqa: E402

URL = "rtmp://127.0.0.1:1234/live/stream-snv"
STREAM_BOX_LOGIN = "fake-login-words-of-the-test"


# ------------------------------------------------------------------ pure builders

def test_the_capture_cpus_leave_the_obs_mask_and_cpu_15_out():
    # stream-snv, read 4.11.2026: online 0-19, /etc/strih-isolated-cpus.conf 2-11
    assert tap.tap_cpus("0-19", ["2-11"]) == "0-1,12-14,16-19"
    assert 15 not in tap.cpulist_expand(tap.tap_cpus("0-19", ["2-11"]))
    assert tap.tap_cpus("0-19", ["2-11", "12-13"]) == "0-1,14,16-19"  # every mask is left out


def test_no_mask_or_no_free_cpu_or_a_bad_list_is_no_capture():
    assert tap.tap_cpus("0-19", []) is None            # never every CPU
    assert tap.tap_cpus("0-15", ["0-14"]) is None      # only CPU 15 left
    assert tap.tap_cpus("0-19", ["2-x"]) is None
    assert tap.tap_cpus("", ["2-11"]) is None
    with pytest.raises(ValueError):
        tap.cpulist_expand("5-2")


def test_the_cpu_set_is_the_background_slices_set_without_cpu_15():
    # parity with the box baseline's own derivation (scripts/lib/obs-box-background.sh, the set
    # rig-background.slice runs on), sourced under the setup scripts' strict mode
    lib = REPO / "scripts" / "lib" / "obs-box-background.sh"
    for online, mask in (("0-19", "2-11"), ("0-15", "2-11"), ("0-7", "0-3"), ("0-3,8-11", "1-2"), ("0-23", "0-11,13")):
        r = subprocess.run(["bash", "-c", 'set -euo pipefail; . "$1"; obs_box_background_cpus "$2" "$3"', "t",
                            str(lib), online, mask], capture_output=True, text=True)
        assert r.returncode == 0, r.stderr
        want = tap.cpulist_expand(r.stdout.strip()) - {15}
        assert tap.tap_cpus(online, [mask]) == (tap.cpulist_compress(want) if want else None), (online, mask)


def test_only_restreamers_local_inpoint_is_recorded():
    assert tap.local_inpoint_url("rtmp://127.0.0.1:1234/live", "stream-snv") == URL
    assert tap.local_inpoint_url("rtmp://localhost:1234/live/", "stream-pp") == "rtmp://localhost:1234/live/stream-pp"
    assert tap.local_inpoint_url("rtmp://a.rtmp.youtube.com/live2", "abcd-efgh") is None
    assert tap.local_inpoint_url("rtmp://127.0.0.1:1935/live", "stream-snv") is None
    assert tap.local_inpoint_url("rtmp://127.0.0.1:1234/live", "x; rm -rf /") is None
    assert tap.local_inpoint_url(None, "k") is None


def test_the_capture_is_a_copy():
    argv = tap.capture_argv(URL, "inpoint.mkv")
    assert argv[0] == "ffmpeg" and argv[argv.index("-i") + 1] == URL
    assert argv[argv.index("-c") + 1] == "copy" and argv[argv.index("-map") + 1] == "0"
    # no -copyts: both tracks shift by the same start offset, the tool reads it like a VOD starting at 0
    assert "-copyts" not in argv
    assert argv[argv.index("-f") + 1] == "matroska"  # a killed ffmpeg still leaves a readable file
    assert not any(a.startswith(("-c:", "-vf", "-af", "-ar", "-b:", "-preset")) for a in argv)  # nothing re-encoded


def test_the_capture_always_runs_at_idle_priority_pinned():
    for helper in (True, False):
        p = tap.priority_prefix("0-1,12-14,16-19", helper)
        i = p.index("chrt")
        assert p[i:i + 3] == ["chrt", "--idle", "0"] and p[p.index("taskset") + 1:p.index("taskset") + 3] == ["-c", "0-1,12-14,16-19"]
    assert tap.priority_prefix("0-1", True)[:3] == ["sudo", "-n", tap.BACKGROUND_RUN]  # rig-background.slice
    p = tap.priority_prefix("0-1", False)
    assert p[:3] == ["nice", "-n", "19"] and "ionice" not in p  # a held write would grow restreamer's channel


def test_a_stage_key_is_never_shell_text_nor_a_parent_dir():
    for bad in ("a b", "../x", "..", ".", "x;y", ""):
        with pytest.raises(ValueError):
            tap.stop_script(bad)
    assert "recording-e2e-77" in tap.purge_script("recording-e2e-77")


def test_the_url_kept_in_the_block_carries_no_stream_key():
    assert tap.redact_url(URL) == "rtmp://127.0.0.1:1234/live/<key>"


# ------------------------------------------------------------------ the remote bash, run for real

SSH = r'''#!{py}
import json, os, subprocess, sys
d = {d!r}
host, cmd = sys.argv[-2], sys.argv[-1]
open(os.path.join(d, "ssh.log"), "a").write(json.dumps([host, "pw" if os.environ.get("SSHPASS") else "nopw"]) + "\n")
env = {{"HOME": {home!r}, "PATH": {bindir!r} + ":/usr/bin:/bin", "LANG": "C.UTF-8",
        "YOUTUBE_LEG_TAP_ROOT": {root!r}, "FAKE_DIR": d}}
for k in ("FAKE_FFMPEG_MODE",):
    if k in os.environ:
        env[k] = os.environ[k]
r = subprocess.run(["bash", "-c", cmd], env=env, cwd={home!r})
sys.exit(r.returncode)
'''

PASSTHRU = r'''#!{py}
import json, os, sys
name = {name!r}
argv = sys.argv[1:]
open(os.path.join(os.environ["FAKE_DIR"], "argv.log"), "a").write(json.dumps([name] + argv) + "\n")
rest = argv[2:]  # chrt --idle 0 / taskset -c L / ionice -c 3, then the command
os.execvp(rest[0], rest)
'''

FFMPEG = r'''#!{py}
import json, os, signal, sys, time
d = os.environ["FAKE_DIR"]
open(os.path.join(d, "argv.log"), "a").write(json.dumps(["ffmpeg"] + sys.argv[1:]) + "\n")
mode = os.environ.get("FAKE_FFMPEG_MODE", "run")
if mode == "die":
    print("rtmp://127.0.0.1:1234/live/stream-snv: Input/output error", file=sys.stderr)
    sys.exit(1)
out = sys.argv[-1]
with open(out, "wb") as f:
    f.write(b"\x1a\x45\xdf\xa3 a matroska capture")


def _int(*_):
    open(os.path.join(d, "ffmpeg.int"), "w").write("finalized")
    sys.exit(255)


signal.signal(signal.SIGINT, _int)
end = time.time() + (2.5 if mode == "eof" else 120)  # eof: the publisher stops, ffmpeg ends by itself
while time.time() < end:
    time.sleep(0.05)
'''


@pytest.fixture
def box(tmp_path):
    d = tmp_path / "fake"
    home, root, bindir = tmp_path / "streambox-home", tmp_path / "root", tmp_path / "bin"
    for p in (d, home, root / "sys/devices/system/cpu", root / "etc", bindir):
        p.mkdir(parents=True, exist_ok=True)
    (root / "sys/devices/system/cpu/online").write_text("0-19\n")
    (root / "etc/strih-isolated-cpus.conf").write_text("2-11\n")
    for name, text in (("chrt", PASSTHRU), ("taskset", PASSTHRU), ("ionice", PASSTHRU), ("ffmpeg", FFMPEG)):
        (bindir / name).write_text(text.format(py=sys.executable, name=name))
        (bindir / name).chmod(0o755)
    ssh = d / "ssh"
    ssh.write_text(SSH.format(py=sys.executable, d=str(d), home=str(home), bindir=str(bindir), root=str(root)))
    ssh.chmod(0o755)
    env = dict(os.environ, YOUTUBE_LEG_TAP_SSH_CMD=str(ssh), AV_GATE_SSH_PASSWORD=STREAM_BOX_LOGIN)
    return {"d": d, "home": home, "root": root, "env": env}


def _tap(box, *args, mode=None, timeout=90):
    env = dict(box["env"])
    if mode:
        env["FAKE_FFMPEG_MODE"] = mode
    r = subprocess.run([sys.executable, str(TAP), "--host", "10.77.9.204", "--user", "newlevel",
                        "--key", "recording-e2e-77", *args], capture_output=True, text=True, env=env, timeout=timeout)
    out = json.loads(r.stdout.strip().splitlines()[-1]) if r.stdout.strip() else {}
    return r.returncode, out, r


def _argv(box):
    p = box["d"] / "argv.log"
    return [json.loads(x) for x in p.read_text().splitlines()] if p.exists() else []


def test_start_records_restreamers_input_off_the_obs_cores_and_stop_signals_it(box):
    rc, out, r = _tap(box, "start", "--url", URL)
    assert rc == 0, r.stdout + r.stderr
    cap = box["home"] / "youtube-leg-tap-1387" / "recording-e2e-77"
    assert out["started"] is True and out["path"] == f"{cap}/inpoint.mkv"
    assert out["cpus"] == "0-1,12-14,16-19" and "SCHED_IDLE" in out["priority"]
    calls = _argv(box)
    names = [c[0] for c in calls]
    assert names == ["chrt", "taskset", "ffmpeg"]  # nice 19 is the real one, first
    assert calls[0][1:3] == ["--idle", "0"] and calls[1][1:3] == ["-c", "0-1,12-14,16-19"]
    ff = calls[2]
    assert ff[ff.index("-i") + 1] == URL and ff[ff.index("-c") + 1] == "copy"
    assert ff[-1] == f"{cap}/inpoint.mkv"  # an absolute path: the stop finds the capture by it
    assert out["url"] == "rtmp://127.0.0.1:1234/live/<key>"
    assert json.loads((box["d"] / "ssh.log").read_text().splitlines()[0])[1] == "pw"
    rc, out, r = _tap(box, "stop", "--grace", "1")
    assert rc == 0, r.stdout + r.stderr
    assert out["how"] == "signalled" and out["bytes"] > 0 and out["path"].endswith("/inpoint.mkv")
    assert (box["d"] / "ffmpeg.int").read_text() == "finalized"  # SIGINT: ffmpeg writes its file's end


def test_a_capture_that_ended_with_the_stream_is_not_signalled(box):
    assert _tap(box, "start", "--url", URL, mode="eof")[0] == 0
    time.sleep(3.0)
    rc, out, _ = _tap(box, "stop", "--grace", "5")
    assert rc == 0 and out["how"] == "ended" and out["bytes"] > 0
    assert not (box["d"] / "ffmpeg.int").exists()


def test_a_capture_that_dies_at_once_is_refused_by_name_and_leaves_nothing(box):
    rc, out, _ = _tap(box, "start", "--url", URL, mode="die")
    assert rc == 2 and "did not keep running" in out["reason"] and "Input/output error" in out["reason"]
    assert "stream-snv" not in out["reason"] and "live/<key>" in out["reason"]  # the key never in the block
    assert not (box["home"] / "youtube-leg-tap-1387" / "recording-e2e-77").exists()


def test_a_stream_key_is_redacted_from_any_text():
    assert tap.redact_text(f"{URL}: Input/output error") == "rtmp://127.0.0.1:1234/live/<key>: Input/output error"
    assert tap.redact_text("no url here") == "no url here" and tap.redact_text(None) == ""


def test_no_cpu_outside_the_obs_mask_starts_nothing(box):
    (box["root"] / "etc/strih-isolated-cpus.conf").write_text("0-14,16-19\n")
    rc, out, _ = _tap(box, "start", "--url", URL)
    assert rc == 2 and "never captured on the OBS cores" in out["reason"]
    assert _argv(box) == []


def test_no_url_starts_nothing(box):
    rc, out, _ = _tap(box, "start")
    assert rc == 2 and "no restreamer inpoint URL" in out["reason"]
    assert not (box["d"] / "ssh.log").exists()


def test_purge_ends_a_capture_and_deletes_it(box):
    assert _tap(box, "start", "--url", URL)[0] == 0
    cap = box["home"] / "youtube-leg-tap-1387" / "recording-e2e-77"
    assert (cap / "inpoint.mkv").exists()
    rc, out, _ = _tap(box, "purge")
    assert rc == 0 and out["purged"] is True and not cap.exists()


def test_stop_without_a_capture_reads_absent(box):
    rc, out, _ = _tap(box, "stop", "--grace", "1")
    assert rc == 0 and out["how"] == "absent" and out["bytes"] == 0


def test_a_day_old_capture_of_a_killed_run_is_swept_at_the_next_start(box):
    old = box["home"] / "youtube-leg-tap-1387" / "recording-e2e-1"
    old.mkdir(parents=True)
    (old / "inpoint.mkv").write_bytes(b"x")
    t = time.time() - 2 * 86400
    os.utime(old, (t, t))
    assert _tap(box, "start", "--url", URL)[0] == 0
    assert not old.exists()
    _tap(box, "purge")


# sudo with Defaults use_pty: the command runs in ANOTHER session than sudo's own pid, while the parent
# waits; `-n -l <helper>` = the sudoers rule allows the helper
SUDO = r'''#!{py}
import json, os, sys
argv = sys.argv[1:]
open(os.path.join(os.environ["FAKE_DIR"], "argv.log"), "a").write(json.dumps(["sudo"] + argv) + "\n")
if argv[:2] == ["-n", "-l"]:
    sys.exit(0)
assert argv[0] == "-n" and argv[1].endswith("/obs-box-background-run"), argv
pid = os.fork()
if pid == 0:
    os.setsid()
    os.execvp(argv[2], argv[2:])
os.execv({py!r}, [{py!r}, "-c", "import os; os.wait()"])  # the waiting parent, without the capture path
'''


def test_the_helper_path_runs_in_another_session_and_the_stop_still_ends_the_capture(box, tmp_path):
    helper = box["root"] / "usr/local/sbin/obs-box-background-run"
    helper.parent.mkdir(parents=True)
    helper.write_text("#!/bin/sh\nexit 0\n")
    helper.chmod(0o755)
    (tmp_path / "bin" / "sudo").write_text(SUDO.format(py=sys.executable))
    (tmp_path / "bin" / "sudo").chmod(0o755)
    rc, out, r = _tap(box, "start", "--url", URL)
    assert rc == 0, r.stdout + r.stderr
    assert out["priority"] == "rig-background.slice + SCHED_IDLE"
    assert [c[0] for c in _argv(box)] == ["sudo", "sudo", "chrt", "taskset", "ffmpeg"]  # the probe, the launch
    rc, out, r = _tap(box, "stop", "--grace", "1")
    assert rc == 0 and out["how"] == "signalled", r.stdout + r.stderr  # found by owner + path, not sudo's pid
    assert (box["d"] / "ffmpeg.int").read_text() == "finalized"


def test_a_capture_that_survives_the_stop_is_never_reported_stopped(box, tmp_path):
    assert _tap(box, "start", "--url", URL)[0] == 0
    noop = tmp_path / "bin" / "pkill"  # a signal that reaches nothing (another user's process, a sudo session)
    noop.write_text("#!/bin/sh\nexit 0\n")
    noop.chmod(0o755)
    rc, out, _ = _tap(box, "stop", "--grace", "1", timeout=120)
    assert rc == 2 and "survived INT and KILL" in out["reason"]
    rc, out, _ = _tap(box, "purge", timeout=120)
    assert rc == 2 and "not deleted" in out["reason"]
    assert (box["home"] / "youtube-leg-tap-1387" / "recording-e2e-77" / "inpoint.mkv").exists()
    noop.unlink()
    assert _tap(box, "purge")[0] == 0


def test_a_control_byte_in_ffmpegs_log_never_breaks_the_stop(box):
    assert _tap(box, "start", "--url", URL, mode="eof")[0] == 0
    log = box["home"] / "youtube-leg-tap-1387" / "recording-e2e-77" / "ffmpeg.log"
    log.write_bytes(b'[flv] "bad"\x01\x1b[31m tag \\ size\n')
    time.sleep(3.0)
    rc, out, r = _tap(box, "stop", "--grace", "5")
    assert rc == 0, r.stdout + r.stderr
    assert out["how"] == "ended" and "bad" in out["ffmpeg_log"] and "\x01" not in out["ffmpeg_log"]
