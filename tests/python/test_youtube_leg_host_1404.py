"""Issue 1404 (main design 6387919956, parts b + c) -- the YouTube stage measures on dev2.

scripts/youtube_leg_e2e.py `prefetch` (started in the background right after the stream StopRecord)
pulls the stream recording from the stream box STRAIGHT to the measurement host and decodes its tick
map there while the E2E merges and YouTube processes the VOD; `measure` then runs the verdict tool ON
the host (the early map is in its cache: only the VOD is decoded after the VOD wait), brings back only
the verdict JSON + the tick maps, and deletes the host's stage dir (the videos with it). Pinned with
the fakes of youtube_leg_e2e_fakes_1404.py (a fake ssh that RUNS the remote bash under a fake dev2
home, stubs for the tool and the pull):
  - the pull goes stream box -> host, the password on ssh's stdin (never in an argv), nothing to dev1;
  - an early pull or decode that failed is done again once in the normal path, and recorded;
  - an unreachable host is UNKNOWN by name, never a measurement on dev1;
  - every remote step is bounded (a tool that runs over is UNKNOWN, its process group killed, the
    stage dir deleted); a prefetch still running is waited for, and `purge` stops it;
  - a killed run's day-old stage dir is swept.
"""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_youtube_leg_e2e_1404 import (PASS_RESULT, STREAM_PATH, TOOL, _measure_inputs, _state,  # noqa: E402
                                       _stopped, scripted_fakes)

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts"))
import youtube_leg_host as ylh  # noqa: E402

STREAM_BOX_LOGIN = "fake-login-words-of-the-test"
STREAM_HOST = "10.77.9.204"


@pytest.fixture
def fk(tmp_path):
    return scripted_fakes(tmp_path)


def _env(fk):
    return dict(fk.env, AV_GATE_SSH_PASSWORD=STREAM_BOX_LOGIN, FAKE_SECRET=STREAM_BOX_LOGIN)


def _run(fk, d, *args, timeout=120):
    return subprocess.run([sys.executable, str(TOOL), "--dir", str(d), "--host", STREAM_HOST, "--user", "newlevel",
                           *args], capture_output=True, text=True, env=_env(fk), timeout=timeout)


def _spawn(fk, d, *args):
    """The background prefetch, as scripts/lib/youtube-leg.sh starts it (its pid in prefetch.pid)."""
    p = subprocess.Popen([sys.executable, str(TOOL), "--dir", str(d), "--host", STREAM_HOST, "--user", "newlevel",
                          *args], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=_env(fk))
    (Path(d) / "prefetch.pid").write_text(str(p.pid))
    return p


def _stage(fk, tmp_path):
    return fk.home / "youtube-leg-1404" / tmp_path.name


def _no_secret_anywhere(fk):
    log = (fk.d / "calls.log").read_text()
    assert STREAM_BOX_LOGIN not in log
    assert all(c[2] == "-" for c in fk.calls() if c[0] == "ssh")  # never in ssh's argv
    assert all(c[3] == "-" for c in fk.calls() if c[0] == "sshpass")  # never in sshpass's argv


def _videos_on_dev1(d):
    return [p for p in Path(d).rglob("*") if p.suffix in (".mkv", ".mp4")]


def test_prefetch_pulls_the_recording_straight_to_the_host_and_decodes_it_there(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    fk.stream_file(STREAM_PATH)
    r = _run(fk, d, "prefetch", "--stream-path", STREAM_PATH)
    assert r.returncode == 0, r.stdout + r.stderr
    pre = _state(d)["prefetch"]
    assert pre["pulled"] is True and pre["decode_rc"] == 0 and pre["host"] == "newlevel@dev2", pre
    stage = _stage(fk, tmp_path)
    assert (stage / "stream-recording.mkv").read_bytes() == b"a stream recording"
    assert (stage / "verdict" / "ticks-rec-1.tsv").exists()
    assert sorted(p.name for p in (stage / "code").iterdir()) == sorted(ylh.TOOL_FILES)
    assert [c for c in fk.calls() if c[0] == "sshpass"] == [
        ["sshpass", f"newlevel@{STREAM_HOST}:{STREAM_PATH}", "pw", "-"]]  # stream box -> host, password arrived
    prime = [c for c in fk.calls() if c[0] == "prime"][0]
    assert prime[1] == f"{stage}/code/youtube_leg_verdict.py"
    assert prime[prime.index("--prime-ticks") + 1] == f"{stage}/stream-recording.mkv"
    assert prime[prime.index("--out") + 1] == f"{stage}/verdict"
    assert prime[prime.index("--workers") + 1] == str(ylh.DEFAULT_WORKERS)
    assert not list(stage.glob("*.pid"))  # every bounded step cleared its group's pid
    assert not _videos_on_dev1(d)
    _no_secret_anywhere(fk)


def test_measure_after_a_prefetch_decodes_only_the_vod_and_brings_back_only_the_verdict(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    args, verdict = _measure_inputs(tmp_path, fk)
    assert _run(fk, d, "prefetch", "--stream-path", STREAM_PATH).returncode == 0
    fk.script("verdict", "verdict", 0, PASS_RESULT)
    r = _run(fk, d, *args)
    assert r.returncode == 0, r.stdout + r.stderr
    names = fk.names()
    assert (names.count("sshpass"), names.count("prime"), names.count("verdict")) == (1, 1, 1)
    assert sorted(p.name for p in (Path(d) / "verdict").iterdir()) == [
        "ticks-rec-1.tsv", "ticks-rec-1.tsv.reads.json", "ticks-vod.tsv", "youtube-leg-verdict.json"]
    assert fk.host_stage_dirs() == []  # the host's stage dir is gone, the videos with it
    assert not _videos_on_dev1(d)
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert b["verdict"] == "PASS" and b["measure_host"] == "newlevel@dev2"
    assert b["early"]["pulled"] is True and b["early"]["decode_rc"] == 0
    assert not b["early"].get("retried_pull") and not b["early"].get("retried_decode")
    assert b["decode_reads"]["ticks-rec-1.tsv"]["known"] == 1
    assert any("is deleted" in e for e in b["events"])
    _no_secret_anywhere(fk)


def test_an_early_pull_that_failed_is_done_again_once(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    args, verdict = _measure_inputs(tmp_path, fk)
    fk.fail_one_pull()
    assert _run(fk, d, "prefetch", "--stream-path", STREAM_PATH).returncode == 0
    pre = _state(d)["prefetch"]
    assert not pre.get("pulled") and "pull" in pre["reason"] and "Connection closed" in pre["reason"]
    assert "prime" not in fk.names()  # nothing to decode
    fk.script("verdict", "verdict", 0, PASS_RESULT)
    assert _run(fk, d, *args).returncode == 0
    assert fk.names().count("sshpass") == 2
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert b["verdict"] == "PASS" and b["early"]["retried_pull"] is True and b["early"]["retried_decode"] is True
    assert any("pulling it now (the one retry)" in e for e in b["events"])


def test_an_early_decode_that_failed_is_done_again_by_the_verdict_run(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    args, verdict = _measure_inputs(tmp_path, fk)
    fk.script("prime", "prime", 2, None)
    assert _run(fk, d, "prefetch", "--stream-path", STREAM_PATH).returncode == 0
    pre = _state(d)["prefetch"]
    assert pre["pulled"] is True and pre["decode_rc"] == 2 and "early decode failed" in pre["reason"]
    fk.script("verdict", "verdict", 0, PASS_RESULT)
    assert _run(fk, d, *args).returncode == 0
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert b["verdict"] == "PASS" and b["early"]["retried_decode"] is True and not b["early"].get("retried_pull")
    assert any("decodes the recording again (the one retry)" in e for e in b["events"])
    assert fk.names().count("sshpass") == 1  # the recording was already there


def test_an_unreachable_measurement_host_is_unknown_by_name_never_a_measurement_on_dev1(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    args, verdict = _measure_inputs(tmp_path, fk)
    fk.host_down()
    assert _run(fk, d, "prefetch", "--stream-path", STREAM_PATH).returncode == 0
    assert _state(d)["prefetch"]["unreachable"] is True
    assert _run(fk, d, *args).returncode == 1
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert b["verdict"] == "UNKNOWN"
    assert "the measurement host newlevel@dev2 is unreachable" in b["reasons"][0], b["reasons"]
    assert not {"verdict", "prime", "sshpass"} & set(fk.names())  # nothing measured anywhere
    assert not (Path(d) / "verdict" / "youtube-leg-verdict.json").exists()
    assert any("could NOT be deleted" in e for e in b["events"])


def test_a_verdict_tool_that_runs_over_its_bound_is_unknown_and_its_stage_is_deleted(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    args, verdict = _measure_inputs(tmp_path, fk)
    fk.script("verdict", "verdict", 0, {"sleep": 30})
    t0 = time.monotonic()
    assert _run(fk, d, *args, "--tool-timeout", "2").returncode == 1
    assert time.monotonic() - t0 < 25  # the host's own `timeout` ended it, not the fake's sleep
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert b["verdict"] == "UNKNOWN" and b["reasons"] == ["the YouTube-leg verdict tool ran over 2 s"]
    assert fk.host_stage_dirs() == []


def test_measure_waits_for_an_early_decode_still_running(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    args, verdict = _measure_inputs(tmp_path, fk)
    fk.script("prime", "prime", 0, {"sleep": 3})
    p = _spawn(fk, d, "prefetch", "--stream-path", STREAM_PATH)
    deadline = time.monotonic() + 30
    while "prime" not in fk.names() and time.monotonic() < deadline:  # the decode is running
        time.sleep(0.1)
    fk.script("verdict", "verdict", 0, PASS_RESULT)
    assert _run(fk, d, *args).returncode == 0
    assert p.wait(timeout=10) == 0
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert any("waiting for the early decode" in e for e in b["events"])
    assert b["early"]["decode_rc"] == 0 and not b["early"].get("retried_decode")
    assert fk.names().count("prime") == 1 and fk.names().count("verdict") == 1


def _alive_under(path):
    """Processes whose command line names `path` (the fake host's stubs)."""
    found = []
    for pid in os.listdir("/proc"):
        if pid.isdigit():
            try:
                with open(f"/proc/{pid}/cmdline", "rb") as f:
                    if str(path).encode() in f.read():
                        found.append(int(pid))
            except OSError:
                continue
    return found


def test_purge_stops_a_running_prefetch_and_deletes_the_host_stage(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    fk.stream_file(STREAM_PATH)
    fk.script("prime", "prime", 0, {"sleep": 60})
    p = _spawn(fk, d, "prefetch", "--stream-path", STREAM_PATH)
    stage = _stage(fk, tmp_path)
    deadline = time.monotonic() + 30
    while not (stage / "prime.pid").exists() and time.monotonic() < deadline:
        time.sleep(0.1)
    assert (stage / "prime.pid").exists()
    r = _run(fk, d, "purge")
    assert r.returncode == 0, r.stdout + r.stderr
    assert p.wait(timeout=20) is not None
    assert fk.host_stage_dirs() == []
    deadline = time.monotonic() + 10
    while _alive_under(fk.home / ".local" / "bin" / "python3") and time.monotonic() < deadline:
        time.sleep(0.2)
    assert _alive_under(fk.home / ".local" / "bin" / "python3") == []  # the remote decode group is gone


def test_a_stage_without_the_stream_path_is_unknown_by_name(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    args, verdict = _measure_inputs(tmp_path, fk)
    args[args.index("--stream-path") + 1] = ""
    assert _run(fk, d, *args).returncode == 1
    assert "path on the stream box is unknown" in json.loads(verdict.read_text())["youtube_leg"]["reasons"][0]


def test_a_day_old_stage_dir_of_a_killed_run_is_swept(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    fk.stream_file(STREAM_PATH)
    old = fk.home / "youtube-leg-1404" / "recording-e2e-111"
    (old / "verdict").mkdir(parents=True)
    (old / "stream-recording.mkv").write_bytes(b"left behind")
    two_days = time.time() - 2 * 86400
    os.utime(old, (two_days, two_days))
    fresh = fk.home / "youtube-leg-1404" / "recording-e2e-222"
    fresh.mkdir()
    assert _run(fk, d, "prefetch", "--stream-path", STREAM_PATH).returncode == 0
    assert fk.host_stage_dirs() == sorted(["recording-e2e-222", tmp_path.name])
    assert fresh.exists()


def test_the_stage_key_is_the_run_dir_name_sanitized():
    assert ylh.stage_key("/tmp/recording-e2e-1843801084/youtube-leg") == "recording-e2e-1843801084"
    assert ylh.stage_key("/tmp/a b;$(x)/youtube-leg") == "a_b___x_"
    assert ylh.stage_key("/tmp/../youtube-leg") == "stage"
