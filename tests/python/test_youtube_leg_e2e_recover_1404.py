"""Issue 1404 review round 1 -- the YouTube stage's robustness on the bad paths:
- a cancelled run leaves its broadcast live: the dev1 marker names it, `recover` stops the stream only
  when the stream's own age proves it is ours, and the next start waits out restreamer's teardown;
- cleanup()'s quick StopStream half (`stop --stream-only`);
- a crash or an unmeasured window is a RED (UNKNOWN) block, never a missing one or a PASS;
- the no-sweep window ends at the recording's stop."""
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from youtube_leg_e2e_fakes_1404 import Fakes  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
TOOL = REPO / "scripts" / "youtube_leg_e2e.py"
T0 = 1_790_000_000.0


def _run(fk, d, *args, timeout=120):
    return subprocess.run([sys.executable, str(TOOL), "--dir", str(d), "--host", "10.77.9.204", "--user", "newlevel",
                           *args], capture_output=True, text=True, env=fk.env, timeout=timeout)


def _state(d):
    return json.loads((Path(d) / "state.json").read_text())


@pytest.fixture
def fk(tmp_path):
    f = Fakes(tmp_path)
    f.script("session", "start", 0, {"ok": True, "session_id": "s1", "broadcast_id": "b1"})
    f.script("obs", "stream-start", 0, {"ok": True, "publish_utc": T0})
    f.script("obs", "stream-stop", 0, {"ok": True, "unpublish_utc": T0 + 400, "was_active": True})
    f.script("obs", "record-start", 0, {"ok": True, "record_start_utc": T0 + 60})
    f.script("session", "wait-ready", 0, {"ok": True, "state": "ready"})
    f.script("session", "wait-done", 0, {"ok": True, "state": "done", "vod_id": "AbCdEfGhIjK"})
    return f


# ------------------------------------------------------------------ the marker + recover

def test_a_live_stage_is_marked_and_a_clean_stop_clears_the_marker(fk, tmp_path):
    d = tmp_path / "yl"
    assert _run(fk, d, "start").returncode == 0
    m = json.loads(fk.marker.read_text())
    assert m["session_id"] == "s1" and m["publish_utc"] == T0 and m["host"] == "10.77.9.204"
    assert _run(fk, d, "stop").returncode == 0
    assert not fk.marker.exists()


def test_the_marker_carries_a_provisional_publish_time_while_startstream_runs(fk, tmp_path):
    # a cancel inside StartStream must still leave recover a publish time to prove the stream ours by
    assert _run(fk, tmp_path / "yl", "start").returncode == 0
    m = json.loads((fk.d / "marker-at-stream-start.json").read_text())
    assert isinstance(m["publish_utc"], float) and m["session_id"] == "s1"


def test_a_failed_stop_keeps_the_marker(fk, tmp_path):
    d = tmp_path / "yl"
    assert _run(fk, d, "start").returncode == 0
    fk.script("session", "stop", 2, {"ok": False, "reason": "ssh down"})
    assert _run(fk, d, "stop").returncode == 1
    assert fk.marker.exists()


def _leftover(fk, since, sid="old1"):
    fk.marker.write_text(json.dumps({"session_id": sid, "host": "10.77.9.204", "dir": "/gone", "publish_utc": T0}))
    fk.script("obs", "stream-status", 0, {"ok": True, "active": True, "since_utc": since})


def test_recover_stops_our_leftover_stream_and_keeps_the_marker_for_the_session(fk, tmp_path):
    _leftover(fk, T0 + 1.5)
    r = _run(fk, tmp_path / "x", "recover")
    assert r.returncode == 0, r.stderr
    assert fk.names() == ["obs.stream-status", "obs.stream-stop"]
    assert "OUR leftover" in r.stdout
    assert fk.marker.exists(), "the session half is still owed: the next start (with ssh) stops it"


def test_recover_never_touches_a_stream_that_is_not_ours(fk, tmp_path):
    _leftover(fk, T0 + 3600)
    r = _run(fk, tmp_path / "x", "recover", "--with-session")
    assert r.returncode == 0
    assert "obs.stream-stop" not in fk.names()
    assert "NOT our leftover" in r.stdout
    assert fk.names()[-1] == "session.stop" and not fk.marker.exists()


def test_recover_without_a_marker_does_nothing(fk, tmp_path):
    r = _run(fk, tmp_path / "x", "recover", "--with-session")
    assert r.returncode == 0 and fk.calls() == []


def test_an_unreadable_stream_keeps_the_marker(fk, tmp_path):
    _leftover(fk, T0)
    (fk.d / "obs.stream-status.q").unlink()
    fk.script("obs", "stream-status", 2, {"ok": False, "reason": "unreachable"})
    assert _run(fk, tmp_path / "x", "recover", "--with-session").returncode == 0
    assert "obs.stream-stop" not in fk.names() and fk.marker.exists()


def test_start_recovers_first_and_waits_out_restreamers_teardown_of_it(fk, tmp_path):
    _leftover(fk, T0 + 1)
    (fk.d / "session.start.q").unlink()
    fk.script("session", "start", 2, {"ok": False, "reason": "HTTP 409 busy", "holder_session_id": "old1"})
    fk.script("session", "start", 0, {"ok": True, "session_id": "s2", "broadcast_id": "b2"})
    (fk.d / "obs.stream-start.q").unlink()
    fk.script("obs", "stream-start", 0, {"ok": True, "publish_utc": T0 + 9000})
    d = tmp_path / "yl"
    r = _run(fk, d, "start", "--busy-poll", "0.05")
    assert r.returncode == 0, r.stdout + r.stderr
    names = fk.names()
    assert names[:4] == ["obs.stream-status", "obs.stream-stop", "session.stop", "guard"]
    assert names.count("session.start") == 2
    assert _state(d)["session_id"] == "s2"
    assert json.loads(fk.marker.read_text())["session_id"] == "s2"


def test_a_busy_holder_that_is_not_ours_is_not_waited_for(fk, tmp_path):
    (fk.d / "session.start.q").unlink()
    fk.script("session", "start", 2, {"ok": False, "reason": "HTTP 409 busy", "holder_session_id": "restreamer-ci"})
    r = _run(fk, tmp_path / "yl", "start", "--busy-poll", "0.05")
    assert r.returncode == 1 and fk.names().count("session.start") == 1


# ------------------------------------------------------------------ stop --stream-only

def test_the_quick_cleanup_half_stops_only_the_stream(fk, tmp_path):
    d = tmp_path / "yl"
    assert _run(fk, d, "start").returncode == 0
    assert _run(fk, d, "stop", "--stream-only", "--why", "cleanup()").returncode == 0
    assert fk.names()[-1] == "obs.stream-stop" and "session.stop" not in fk.names()
    assert fk.marker.exists()
    assert _run(fk, d, "stop").returncode == 0
    assert fk.names().count("obs.stream-stop") == 1 and fk.names()[-1] == "session.stop"
    assert not fk.marker.exists()


# ------------------------------------------------------------------ measure: never a missing or false PASS block

def _stopped(fk, tmp_path):
    d = tmp_path / "yl"
    assert _run(fk, d, "start").returncode == 0
    _run(fk, d, "watch", "--parent", str(os.getpid()), "--poll", "0.05", "--max-s", "0.1")
    assert _run(fk, d, "stop", "--record-stopped").returncode == 0
    return d


def _inputs(tmp_path, schedule, fk):
    fk.stream_file("/srv/rec/run.mkv")
    probe = tmp_path / "recording-verdict"
    probe.write_bytes(b"a probe binary")
    markers = tmp_path / "m.csv"
    markers.write_text("x\n")
    sched = tmp_path / "sched.json"
    sched.write_text(schedule if isinstance(schedule, str) else json.dumps(schedule))
    verdict = tmp_path / "verdict.json"
    verdict.write_text(json.dumps({"overall_pass": True}))
    return ["measure", "--verdict-json", str(verdict), "--stream-path", "/srv/rec/run.mkv", "--markers", str(markers),
            "--schedule", str(sched), "--probe-bin", str(probe)], verdict


PASS = {"overall": "PASS", "reasons": [], "criteria": {"status": {"av": "PASS"}}, "windows": []}


def test_a_crash_in_the_measurement_folds_an_unknown_block(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    args, verdict = _inputs(tmp_path, "{not json", fk)
    r = _run(fk, d, *args)
    assert r.returncode == 1
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert b["verdict"] == "UNKNOWN" and b["pass"] is False and "crashed" in b["reasons"][0]


def test_a_window_left_unmeasured_turns_a_pass_into_unknown(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    fk.script("verdict", "verdict", 0, PASS)
    sched = [{"cambox": "CAM5", "start_ns": int((T0 + 70) * 1e9), "end_ns": int((T0 + 100) * 1e9)},
             {"cambox": "CAM2", "start_ns": int((T0 + 100) * 1e9), "end_ns": int((T0 + 108) * 1e9)}]
    args, verdict = _inputs(tmp_path, sched, fk)
    assert _run(fk, d, *args).returncode == 1
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert b["verdict"] == "UNKNOWN" and b["windows_dropped"] == ["w02-CAM2"]


def test_the_no_sweep_window_ends_at_the_recording_stop(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    rec_stop = _state(d)["record_stop_utc"]
    fk.script("verdict", "verdict", 0, PASS)
    args, _ = _inputs(tmp_path, [], fk)
    assert _run(fk, d, *args).returncode == 0
    call = [c for c in fk.calls() if c[0] == "verdict"][0][1:]
    w = call[call.index("--windows") + 1]
    assert w == f"w01-steady:{T0 + 65:.3f}:{rec_stop - 3:.3f}"
