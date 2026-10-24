"""Issue 1404 -- scripts/youtube_leg_e2e.py, the full-path E2E's BLOCKING YouTube stage
(main design 6383793524). The four outside steps (restreamer's session client, the stream OBS output,
the program-audio channel guard, the YouTube-leg verdict tool) are fakes; the orchestration, the
state, the windows and the fold into the verdict JSON are real."""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from youtube_leg_e2e_fakes_1404 import Fakes  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
TOOL = REPO / "scripts" / "youtube_leg_e2e.py"
sys.path.insert(0, str(REPO / "scripts"))
import youtube_leg_e2e as yle  # noqa: E402

T0 = 1_790_000_000.0


def _run(fk, d, *args, timeout=120):
    r = subprocess.run([sys.executable, str(TOOL), "--dir", str(d), "--host", "10.77.9.204", "--user", "newlevel",
                        *args], capture_output=True, text=True, env=fk.env, timeout=timeout)
    return r


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


# ------------------------------------------------------------------ start / stop

def test_start_runs_guard_session_streamstart_ready_in_that_order(fk, tmp_path):
    d = tmp_path / "yl"
    r = _run(fk, d, "start")
    assert r.returncode == 0, r.stdout + r.stderr
    assert fk.names() == ["guard", "session.start", "obs.stream-start", "session.wait-ready"]
    st = _state(d)
    assert st["phase"] == "live" and st["session_id"] == "s1" and st["publish_utc"] == T0
    assert st["stream_started"] is True and not st.get("failure")


def test_a_guard_refusal_starts_nothing(fk, tmp_path):
    fk.script("guard", "guard", 1, None)
    r = _run(fk, tmp_path / "yl", "start")
    assert r.returncode == 1
    assert fk.names() == ["guard"]
    st = _state(tmp_path / "yl")
    assert st["phase"] == "failed" and "channel guard refused" in st["failure"]


def test_a_refused_delivery_never_starts_the_stream(fk, tmp_path):
    (fk.d / "session.start.q").unlink()
    fk.script("session", "start", 2, {"ok": False, "reason": "HTTP 503 not_provisioned: no oauth file"})
    r = _run(fk, tmp_path / "yl", "start")
    assert r.returncode == 1
    assert fk.names() == ["guard", "session.start"]
    assert "not_provisioned" in _state(tmp_path / "yl")["failure"]


def test_a_foreign_stream_is_never_stopped_but_our_session_is(fk, tmp_path):
    (fk.d / "obs.stream-start.q").unlink()
    fk.script("obs", "stream-start", 2, {"ok": False, "foreign": True, "reason": "already streaming"})
    r = _run(fk, tmp_path / "yl", "start")
    assert r.returncode == 1
    names = fk.names()
    assert "obs.stream-stop" not in names
    assert names[-1] == "session.stop"


def test_a_broadcast_that_never_goes_live_stops_the_stream_and_the_session(fk, tmp_path):
    (fk.d / "session.wait-ready.q").unlink()
    fk.script("session", "wait-ready", 2, {"ok": False, "reason": "did not go live within 420 s"})
    d = tmp_path / "yl"
    r = _run(fk, d, "start")
    assert r.returncode == 1
    assert fk.names()[-2:] == ["obs.stream-stop", "session.stop"]
    st = _state(d)
    assert st["phase"] == "failed" and "never went live" in st["failure"]
    assert st["stream_stopped"] and st["session_stopped"]


def test_stop_is_idempotent(fk, tmp_path):
    d = tmp_path / "yl"
    assert _run(fk, d, "start").returncode == 0
    assert _run(fk, d, "stop").returncode == 0
    assert _run(fk, d, "stop").returncode == 0
    names = fk.names()
    assert names.count("obs.stream-stop") == 1 and names.count("session.stop") == 1
    st = _state(d)
    assert st["phase"] == "stopped" and st["unpublish_utc"] == T0 + 400


def test_a_failed_stop_is_retried_by_the_next_stop(fk, tmp_path):
    d = tmp_path / "yl"
    assert _run(fk, d, "start").returncode == 0
    fk.script("session", "stop", 2, {"ok": False, "reason": "ssh down"})
    fk.script("session", "stop", 0, {"ok": True, "http": 202})
    assert _run(fk, d, "stop").returncode == 1
    assert _run(fk, d, "stop").returncode == 0
    assert fk.names().count("session.stop") == 2 and fk.names().count("obs.stream-stop") == 1


# ------------------------------------------------------------------ watch

def _live(fk, tmp_path):
    d = tmp_path / "yl"
    assert _run(fk, d, "start").returncode == 0
    return d


def test_watch_stops_the_broadcast_at_once_on_foreign_audio(fk, tmp_path):
    d = _live(fk, tmp_path)
    fk.script("guard", "guard", 0, None)
    fk.script("guard", "guard", 1, None)
    r = _run(fk, d, "watch", "--parent", str(os.getpid()), "--poll", "0.05", "--max-s", "20")
    assert r.returncode == 0
    st = _state(d)
    assert st["phase"] == "failed" and "protect the channel" in st["failure"]
    assert st["stream_stopped"] and st["session_stopped"]
    assert "record_start_utc" in st and st["record_start_utc"] == T0 + 60


def test_watch_ends_when_the_stage_was_stopped(fk, tmp_path):
    d = _live(fk, tmp_path)
    assert _run(fk, d, "stop").returncode == 0
    t = time.monotonic()
    r = _run(fk, d, "watch", "--parent", str(os.getpid()), "--poll", "5", "--max-s", "60")
    assert r.returncode == 0 and time.monotonic() - t < 4


def test_watch_stops_everything_when_the_harness_died(fk, tmp_path):
    d = _live(fk, tmp_path)
    dead = subprocess.Popen(["true"])
    dead.wait()
    r = _run(fk, d, "watch", "--parent", str(dead.pid), "--poll", "0.05", "--max-s", "20")
    assert r.returncode == 0
    st = _state(d)
    assert st["stream_stopped"] and st["session_stopped"]
    assert any("harness" in e for e in st["events"])


# ------------------------------------------------------------------ windows + measure

def test_windows_are_the_schedule_segments_trimmed():
    sched = [{"cambox": "CAM5", "start_ns": int(100e9), "end_ns": int(130e9)},
             {"cambox": "CAM2", "start_ns": int(130e9), "end_ns": int(141e9)},
             {"cambox": "CAM3", "start_ns": int(141e9), "end_ns": int(171e9)}]
    ws, dropped = yle.windows_from_schedule(sched)
    assert ws == [("w01-CAM5", 103.0, 128.0), ("w03-CAM3", 144.0, 169.0)]
    assert dropped == ["w02-CAM2"]
    assert yle.window_args(ws)[0] == "w01-CAM5:103.000:128.000"


def _measure_inputs(tmp_path, overall=True):
    rec = tmp_path / "stream-recording.mkv"
    rec.write_bytes(b"x")
    markers = tmp_path / "markers.csv"
    markers.write_text("tick,ts\n")
    sched = tmp_path / "schedule.json"
    sched.write_text(json.dumps([{"cambox": "CAM5", "start_ns": int((T0 + 70) * 1e9), "end_ns": int((T0 + 100) * 1e9)},
                                 {"cambox": "CAM3", "start_ns": int((T0 + 100) * 1e9), "end_ns": int((T0 + 130) * 1e9)}]))
    verdict = tmp_path / "verdict.json"
    if overall is not None:
        verdict.write_text(json.dumps({"overall_pass": overall}))
    return ["measure", "--verdict-json", str(verdict), "--recording", str(rec), "--markers", str(markers),
            "--schedule", str(sched), "--probe-bin", "/x/recording-verdict"], verdict


def _stopped(fk, tmp_path):
    d = _live(fk, tmp_path)
    fk.script("guard", "guard", 0, None)
    # the watch reads the record start; then the E2E stops the stage
    _run(fk, d, "watch", "--parent", str(os.getpid()), "--poll", "0.05", "--max-s", "0.1")
    assert _run(fk, d, "stop").returncode == 0
    return d


PASS_RESULT = {"schema": 1, "overall": "PASS", "reasons": [],
               "criteria": {"status": {"av": "PASS", "dupskip": "PASS", "publish": "PASS", "audio": "PASS",
                                       "coverage": "PASS"}},
               "windows": [{"name": "w01-CAM5", "errors": [], "dupskip": {"dup": 0, "skip": 0},
                            "audio": {"lag_jumps": 0, "low_corr": 0, "silent": 0},
                            "av": {"delta_ms": 12.0, "rec_ms": 10.0, "vod_ms": 22.0}}]}


def test_measure_runs_the_tool_on_the_vod_and_folds_a_pass(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    fk.script("verdict", "verdict", 0, PASS_RESULT)
    args, verdict = _measure_inputs(tmp_path)
    r = _run(fk, d, *args)
    assert r.returncode == 0, r.stdout + r.stderr
    call = [c for c in fk.calls() if c[0] == "verdict"][0][1:]
    assert call[call.index("--vod") + 1] == "AbCdEfGhIjK"
    assert call[call.index("--recording") + 1].endswith(f"stream-recording.mkv@{T0 + 60:.3f}")
    assert call[call.index("--probe-bin") + 1] == "/x/recording-verdict"
    wins = [call[i + 1] for i, a in enumerate(call) if a == "--windows"]
    assert wins == [f"w01-CAM5:{T0 + 73:.3f}:{T0 + 98:.3f}", f"w02-CAM3:{T0 + 103:.3f}:{T0 + 128:.3f}"]
    assert call[call.index("--publish") + 1] == f"{T0:.3f}"
    assert call[call.index("--unpublish") + 1] == f"{T0 + 400:.3f}"
    v = json.loads(verdict.read_text())
    b = v["youtube_leg"]
    assert b["verdict"] == "PASS" and b["pass"] is True and b["gates_overall_pass"] is True
    assert b["vod_id"] == "AbCdEfGhIjK" and b["windows"][0]["av_delta_ms"] == 12.0
    assert v["overall_pass"] is True  # the merge's own value is left alone


def test_a_tool_fail_is_a_red_stage(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    fk.script("verdict", "verdict", 1, dict(PASS_RESULT, overall="FAIL", reasons=["w01-CAM5: 3 downstream dup"]))
    args, verdict = _measure_inputs(tmp_path)
    assert _run(fk, d, *args).returncode == 1
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert b["verdict"] == "FAIL" and b["pass"] is False and "3 downstream dup" in b["reasons"][0]


def test_a_tool_that_writes_nothing_or_contradicts_its_exit_is_unknown(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    fk.script("verdict", "verdict", 2, None)
    args, verdict = _measure_inputs(tmp_path)
    assert _run(fk, d, *args).returncode == 1
    assert json.loads(verdict.read_text())["youtube_leg"]["verdict"] == "UNKNOWN"
    (fk.d / "verdict.verdict.q").unlink()
    fk.script("verdict", "verdict", 2, PASS_RESULT)  # "PASS" in the JSON but exit 2
    assert _run(fk, d, *args).returncode == 1
    assert json.loads(verdict.read_text())["youtube_leg"]["verdict"] == "UNKNOWN"


def test_no_vod_is_a_red_stage_never_a_skip(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    (fk.d / "session.wait-done.q").unlink()
    fk.script("session", "wait-done", 2, {"ok": False, "reason": "the YouTube VOD was not processed within 1800 s"})
    args, verdict = _measure_inputs(tmp_path)
    assert _run(fk, d, *args).returncode == 1
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert b["verdict"] == "UNKNOWN" and "no YouTube VOD" in b["reasons"][0]
    assert "verdict" not in fk.names()


def test_a_failed_start_folds_its_reason(fk, tmp_path):
    fk.script("guard", "guard", 1, None)
    d = tmp_path / "yl"
    _run(fk, d, "start")
    args, verdict = _measure_inputs(tmp_path)
    assert _run(fk, d, *args).returncode == 1
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert b["verdict"] == "UNKNOWN" and "channel guard refused" in b["reasons"][0]


def test_a_stage_that_never_started_is_red(fk, tmp_path):
    args, verdict = _measure_inputs(tmp_path)
    assert _run(fk, tmp_path / "never", *args).returncode == 1
    assert "never started" in json.loads(verdict.read_text())["youtube_leg"]["reasons"][0]


def test_a_missing_recording_or_record_start_is_red(fk, tmp_path):
    d = _live(fk, tmp_path)
    assert _run(fk, d, "stop").returncode == 0  # no watch ran: no record start
    args, verdict = _measure_inputs(tmp_path)
    assert _run(fk, d, *args).returncode == 1
    assert "start time was never read" in json.loads(verdict.read_text())["youtube_leg"]["reasons"][0]


def test_without_a_verdict_json_the_block_is_still_written(fk, tmp_path):
    d = _stopped(fk, tmp_path)
    fk.script("verdict", "verdict", 0, PASS_RESULT)
    args, verdict = _measure_inputs(tmp_path, overall=None)
    assert _run(fk, d, *args).returncode == 0
    assert not verdict.exists()
    assert json.loads((d / "youtube-leg-block.json").read_text())["verdict"] == "PASS"
