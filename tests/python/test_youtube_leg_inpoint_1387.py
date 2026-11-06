"""Issue 1387 -- the YouTube stage's REPORT-ONLY restreamer-input point (scripts/youtube_leg_e2e.py with
`start --tap-inpoint`, scripts/youtube_leg_points.py, the report table). The outside steps are the
issue-1404 fakes (+ a `tap` fake for scripts/youtube_leg_tap.py); the orchestration, the state, the
remote bash on the fake dev2 and the fold are real. The point never changes the YouTube verdict."""
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_youtube_leg_e2e_1404 import (PASS_RESULT, STREAM_PATH, _measure_inputs, _run, _state,  # noqa: E402
                                       scripted_fakes)

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts"))
import e2e_report_youtube as rep  # noqa: E402
import youtube_leg_e2e as yle  # noqa: E402
import youtube_leg_obs as ylo  # noqa: E402
import youtube_leg_points as ylp  # noqa: E402

URL = "rtmp://127.0.0.1:1234/live/stream-snv"
CAPTURE = "/home/newlevel/youtube-leg-tap-1387/run/inpoint.mkv"
TAP_OK = {"ok": True, "started": True, "path": CAPTURE, "url": URL, "cpus": "0-1,12-14,16-19",
          "priority": "rig-background.slice + SCHED_IDLE"}
STOP_OK = {"ok": True, "stopped": True, "how": "ended", "bytes": 912345678, "path": CAPTURE, "ffmpeg_log": ""}
# the restreamer-input point's tool result: the same window, a step at recording time 42.47 s
INPUT_RESULT = {"schema": 1, "overall": "FAIL", "reasons": ["w01-CAM5: audio lag_jumps 1"],
                "criteria": {"status": {"audio": "FAIL"}},
                "windows": [{"name": "w01-CAM5", "errors": [], "dupskip": {"dup": 0, "skip": 0},
                             "audio": {"lag_jumps": 1, "lag_vs_video_ms": 6.7,
                                       "details": {"lag_jumps": [[42.47, 11.06]]}},
                             "av": {"delta_ms": -6.7, "rec_ms": -63.0, "vod_ms": -69.7}}]}


def _fakes(tmp_path):
    fk = scripted_fakes(tmp_path)
    fk.script("obs", "stream-service", 0, {"ok": True, "url": URL})
    fk.script("tap", "start", 0, TAP_OK)
    fk.script("tap", "stop", 0, STOP_OK)
    fk.script("tap", "purge", 0, {"ok": True, "purged": True})
    return fk


def _start(fk, tmp_path, *extra):
    d = tmp_path / "yl"
    r = _run(fk, d, "start", *extra)
    return d, r


# ------------------------------------------------------------------ start / stop

def test_tap_inpoint_starts_the_capture_once_the_broadcast_is_live(tmp_path):
    # `ready` needs our media on restreamer's input, and StartRecord (the windows) comes after it
    fk = _fakes(tmp_path)
    d, r = _start(fk, tmp_path, "--tap-inpoint")
    assert r.returncode == 0, r.stdout + r.stderr
    assert fk.names() == ["guard", "session.start", "obs.stream-start", "session.wait-ready", "obs.stream-service",
                          "tap.start"]
    tap = _state(d)["tap"]
    assert tap["started"] is True and tap["cpus"] == "0-1,12-14,16-19" and tap["url"] == URL
    call = [c for c in fk.calls() if c[0] == "tap"][0][1:]
    assert call[call.index("--url") + 1] == URL and call[call.index("--key") + 1] == tmp_path.name
    assert call[call.index("--host") + 1] == "10.77.9.204"


def test_without_the_flag_nothing_is_tapped(tmp_path):
    fk = _fakes(tmp_path)
    d, r = _start(fk, tmp_path)
    assert r.returncode == 0 and "tap" not in _state(d)
    assert not any(n.startswith("tap.") or n == "obs.stream-service" for n in fk.names())


def test_a_tap_that_cannot_start_never_fails_the_stage(tmp_path):
    fk = _fakes(tmp_path)
    (fk.d / "tap.start.q").unlink()
    fk.script("tap", "start", 2, {"ok": False, "reason": "no CPU outside the OBS mask"})
    d, r = _start(fk, tmp_path, "--tap-inpoint")
    assert r.returncode == 0
    st = _state(d)
    assert st["phase"] == "live" and st["tap"] == {"started": False, "attempted": True,
                                                   "reason": "no CPU outside the OBS mask"}
    # a refused (or timed-out) start may still have left a capture: cleanup's purge-tap removes it
    assert _run(fk, d, "purge-tap").returncode == 0 and fk.names().count("tap.purge") == 1


def test_a_stream_obs_not_publishing_to_restreamer_is_never_tapped(tmp_path):
    fk = _fakes(tmp_path)
    (fk.d / "obs.stream-service.q").unlink()
    fk.script("obs", "stream-service", 2, {"ok": False, "reason": "not restreamer's local inpoint"})
    d, r = _start(fk, tmp_path, "--tap-inpoint")
    assert r.returncode == 0 and "tap.start" not in fk.names()
    assert "not restreamer's local inpoint" in _state(d)["tap"]["reason"]


def test_stop_ends_the_capture_after_the_stream_and_the_session(tmp_path):
    fk = _fakes(tmp_path)
    d, _ = _start(fk, tmp_path, "--tap-inpoint")
    assert _run(fk, d, "stop").returncode == 0
    names = fk.names()
    assert names.index("obs.stream-stop") < names.index("session.stop") < names.index("tap.stop")
    tap = _state(d)["tap"]
    assert tap["stopped"] is True and tap["bytes"] == 912345678 and tap["how"] == "ended"
    assert _run(fk, d, "stop").returncode == 0
    assert fk.names().count("tap.stop") == 1  # once


def test_the_quick_cleanup_stop_never_waits_for_the_capture(tmp_path):
    fk = _fakes(tmp_path)
    d, _ = _start(fk, tmp_path, "--tap-inpoint")
    assert _run(fk, d, "stop", "--stream-only").returncode == 0
    assert "tap.stop" not in fk.names()


def test_a_failed_capture_stop_keeps_the_stage_stopped(tmp_path):
    fk = _fakes(tmp_path)
    (fk.d / "tap.stop.q").unlink()
    fk.script("tap", "stop", 2, {"ok": False, "reason": "stream-snv is unreachable"})
    d, _ = _start(fk, tmp_path, "--tap-inpoint")
    assert _run(fk, d, "stop").returncode == 0
    st = _state(d)
    assert st["phase"] == "stopped" and "unreachable" in st["tap"]["stop_reason"]


# ------------------------------------------------------------------ measure

def _stopped_with_tap(fk, tmp_path):
    d, _ = _start(fk, tmp_path, "--tap-inpoint")
    fk.script("guard", "guard", 0, None)
    _run(fk, d, "watch", "--parent", str(os.getpid()), "--poll", "0.05", "--max-s", "0.1")
    assert _run(fk, d, "stop").returncode == 0
    fk.stream_file(CAPTURE, b"a matroska capture")
    return d


def test_measure_judges_the_capture_with_the_same_tool_and_reports_it_only(tmp_path):
    fk = _fakes(tmp_path)
    d = _stopped_with_tap(fk, tmp_path)
    fk.script("verdict", "verdict", 0, PASS_RESULT)
    fk.script("verdict", "verdict", 1, INPUT_RESULT)  # the point's own FAIL never reaches the verdict
    args, verdict = _measure_inputs(tmp_path, fk)
    r = _run(fk, d, *args)
    assert r.returncode == 0, r.stdout + r.stderr  # the YouTube verdict alone decides
    calls = [c[1:] for c in fk.calls() if c[0] == "verdict"]
    assert len(calls) == 2
    vod, point = calls
    stage = f"{fk.home}/youtube-leg-1404/{tmp_path.name}"
    assert point[point.index("--vod") + 1] == f"{stage}/{ylp.CAPTURE_NAME}"
    assert point[point.index("--out") + 1] == f"{stage}/{ylp.SUB}"
    for flag in ("--recording", "--markers", "--probe-bin", "--publish", "--unpublish"):
        assert point[point.index(flag) + 1] == vod[vod.index(flag) + 1]  # the same reference + inputs
    assert [point[i + 1] for i, a in enumerate(point) if a == "--windows"] == \
        [vod[i + 1] for i, a in enumerate(vod) if a == "--windows"]
    pulls = [c[1] for c in fk.calls() if c[0] == "sshpass"]
    assert f"newlevel@10.77.9.204:{CAPTURE}" in pulls
    assert fk.names().index("tap.purge") > 0  # deleted on the stream box once dev2 has it
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert b["verdict"] == "PASS" and b["pass"] is True
    p = b["points"]["restreamer_input"]
    assert p["measured"] is True and p["report_only"] is True and p["tool_overall"] == "FAIL"
    w = p["windows"][0]
    assert w["audio_lag_jumps"] == 1 and w["audio_lag_jump_details"] == [[42.47, 11.06]]
    assert w["av_delta_ms"] == -6.7 and w["av_point_ms"] == -69.7
    assert p["capture"]["cpus"] == "0-1,12-14,16-19" and p["capture"]["bytes"] == 912345678
    assert b["points"]["restreamer_output"]["measured"] is False
    assert "delivery VPS" in b["points"]["restreamer_output"]["reason"]
    assert json.loads((Path(d) / "inpoint" / "youtube-leg-verdict.json").read_text())["overall"] == "FAIL"
    assert fk.host_stage_dirs() == []  # the dev2 stage dir is gone, the capture with it


def test_a_red_youtube_verdict_still_measures_the_point(tmp_path):
    fk = _fakes(tmp_path)
    d = _stopped_with_tap(fk, tmp_path)
    fk.script("verdict", "verdict", 1, dict(PASS_RESULT, overall="FAIL", reasons=["w01-CAM5: audio lag_jumps 1"]))
    fk.script("verdict", "verdict", 0, dict(INPUT_RESULT, overall="PASS", reasons=[]))
    args, verdict = _measure_inputs(tmp_path, fk)
    assert _run(fk, d, *args).returncode == 1
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert b["verdict"] == "FAIL" and b["points"]["restreamer_input"]["tool_overall"] == "PASS"


def test_a_point_that_fails_to_run_is_named_and_the_verdict_stays(tmp_path):
    fk = _fakes(tmp_path)
    d = _stopped_with_tap(fk, tmp_path)
    fk.script("verdict", "verdict", 0, PASS_RESULT)
    fk.script("verdict", "verdict", 2, None)  # the point's run wrote no verdict
    args, verdict = _measure_inputs(tmp_path, fk)
    assert _run(fk, d, *args).returncode == 0
    b = json.loads(verdict.read_text())["youtube_leg"]
    p = b["points"]["restreamer_input"]
    assert b["verdict"] == "PASS" and p["measured"] is False and "failed" in p["reason"]


def test_an_unpulled_capture_is_kept_on_the_stream_box(tmp_path):
    fk = _fakes(tmp_path)
    d = _stopped_with_tap(fk, tmp_path)
    (fk.stream / CAPTURE.lstrip("/")).unlink()  # the pull finds nothing
    fk.script("verdict", "verdict", 0, PASS_RESULT)
    args, verdict = _measure_inputs(tmp_path, fk)
    assert _run(fk, d, *args).returncode == 0
    p = json.loads(verdict.read_text())["youtube_leg"]["points"]["restreamer_input"]
    assert p["measured"] is False and "pull" in p["reason"]
    assert "tap.purge" not in fk.names()  # the gate never had it; cleanup()'s purge-tap deletes it
    assert _run(fk, d, "purge-tap").returncode == 0 and _run(fk, d, "purge-tap").returncode == 0
    assert fk.names().count("tap.purge") == 1  # once
    assert _state(d)["tap"]["purged"] is True


def test_no_capture_is_reported_as_not_measured(tmp_path):
    fk = scripted_fakes(tmp_path)
    d = tmp_path / "yl"
    assert _run(fk, d, "start").returncode == 0
    fk.script("guard", "guard", 0, None)
    _run(fk, d, "watch", "--parent", str(os.getpid()), "--poll", "0.05", "--max-s", "0.1")
    assert _run(fk, d, "stop").returncode == 0
    fk.script("verdict", "verdict", 0, PASS_RESULT)
    args, verdict = _measure_inputs(tmp_path, fk)
    assert _run(fk, d, *args).returncode == 0
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert b["points"]["restreamer_input"] == {"measured": False, "report_only": True,
                                               "reason": "no restreamer-input capture in this run"}
    assert len([c for c in fk.calls() if c[0] == "verdict"]) == 1


def test_the_skip_reasons_name_each_missing_step():
    assert "did not start: x" in ylp.input_skip_reason({"started": False, "reason": "x"}, True)
    assert "never stopped" in ylp.input_skip_reason({"started": True}, True)
    assert "empty" in ylp.input_skip_reason({"started": True, "stopped": True, "bytes": 0}, True)
    assert "never ran" in ylp.input_skip_reason({"started": True, "stopped": True, "bytes": 5}, False)
    assert ylp.input_skip_reason({"started": True, "stopped": True, "bytes": 5}, True) is None


# ------------------------------------------------------------------ the stream OBS + the report

def _service_rpc(server, key):
    def rpc(rtype, rdata=None, ignore_err=False):
        assert rtype == "GetStreamServiceSettings"
        return {"streamServiceSettings": {"server": server, "key": key}}
    return rpc


def test_stream_service_returns_restreamers_inpoint_only():
    assert ylo.stream_service(_service_rpc("rtmp://127.0.0.1:1234/live", "stream-snv")) == {"url": URL}
    try:
        ylo.stream_service(_service_rpc("rtmp://a.rtmp.youtube.com/live2", "secret-key-words"))
        raise AssertionError("a foreign server must be refused")
    except ylo.Refused as e:
        assert "secret-key-words" not in str(e)  # a real stream key is never printed


def test_the_per_point_table_places_the_step_by_its_recording_time():
    block = {"verdict": "FAIL", "pass": False, "vod_id": "jDso1W4LLb4",
             "windows": [{"name": "w01-CAM5", "av_delta_ms": -16.9, "audio_lag_jumps": 1,
                          "audio_lag_jump_details": [[42.47, 11.06]]}],
             "points": ylp.points(ylp.point_digest({"windows": [{"name": "w01-CAM5", "audio": {"lag_jumps": 0},
                                                                 "av": {"delta_ms": -0.4}}]}, 0))}
    md = rep.points_markdown({"overall_pass": True, "youtube_leg": block})
    assert "Kde vzniká zvukový schod (len report, neblokuje)" in md
    assert "| vstup restreamera (lokálne RTMP na stream boxe) | w01-CAM5 | -0.4 ms | žiadne |" in md
    assert "| YouTube VOD jDso1W4LLb4 | w01-CAM5 | -16.9 ms | 42.47 s: +11.06 ms |" in md
    assert "| výstup restreamera (delivery VPS → YouTube) | — | nemerané | " in md
    assert "✅" not in md.split("Kde vzniká")[1] and "❌" not in md.split("Kde vzniká")[1]  # report-only


def test_a_block_without_points_renders_no_point_section():
    md = rep.points_markdown({"overall_pass": True, "youtube_leg": {"verdict": "PASS", "pass": True, "windows": []}})
    assert "Kde vzniká" not in md


def test_the_lib_starts_the_stage_with_the_restreamer_input_tap():
    lib = (REPO / "scripts" / "lib" / "youtube-leg.sh").read_text()
    fn = lib[lib.index("youtube_leg_start() {"):]
    fn = fn[:fn.index("\n}\n")]
    assert "start --tap-inpoint --title" in fn


def test_cleanup_deletes_a_capture_the_gate_never_measured():
    lib = (REPO / "scripts" / "lib" / "youtube-leg.sh").read_text()
    fn = lib[lib.index("youtube_leg_cleanup_stop() {"):]
    fn = fn[:fn.index("\n}\n")]
    line = fn[fn.index("purge-tap)") - 300:fn.index("purge-tap)")]
    assert 'AV_GATE_SSH_PASSWORD="$pw" timeout 90' in line  # bounded, the password in the env only
    assert fn[fn.index("purge-tap)"):].split("\n")[0].rstrip().endswith("&")  # in the background


# ------------------------------------------------------------------ the point never costs the verdict

def test_the_point_gets_only_what_the_measure_budget_left():
    assert yle.point_bound(1500, 3600, 1000) == 1500
    assert yle.point_bound(1500, 3600, 2900) == 3600 - 2900 - yle.POINT_SLACK_S
    assert yle.point_bound(1500, 3600, 3600 - yle.POINT_SLACK_S - yle.POINT_MIN_S + 1) is None


def test_an_exhausted_measure_budget_skips_the_point(tmp_path):
    fk = _fakes(tmp_path)
    d = _stopped_with_tap(fk, tmp_path)
    fk.script("verdict", "verdict", 0, PASS_RESULT)
    args, verdict = _measure_inputs(tmp_path, fk)
    assert _run(fk, d, *args, "--measure-budget", "100").returncode == 0
    p = json.loads(verdict.read_text())["youtube_leg"]["points"]["restreamer_input"]
    assert p["measured"] is False and "measure budget" in p["reason"]
    assert len([c for c in fk.calls() if c[0] == "verdict"]) == 1


def test_a_verdict_tool_that_ran_over_its_bound_skips_the_point(tmp_path):
    fk = _fakes(tmp_path)
    d = _stopped_with_tap(fk, tmp_path)
    fk.script("verdict", "verdict", 0, {"sleep": 4})
    args, verdict = _measure_inputs(tmp_path, fk)
    assert _run(fk, d, *args, "--tool-timeout", "1").returncode == 1
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert b["verdict"] == "UNKNOWN" and "never ran" in b["points"]["restreamer_input"]["reason"]
    assert len([c for c in fk.calls() if c[0] == "verdict"]) == 1


def test_the_blocking_block_is_folded_before_the_point_starts(tmp_path, monkeypatch):
    vj = tmp_path / "verdict.json"
    vj.write_text(json.dumps({"overall_pass": True}))
    seen = {}

    def fake_measure(ctx, stage):
        return "PASS", [], PASS_RESULT, 0, [("w01-CAM5", 1.0, 30.0)], []

    def fake_point(ctx, stage, windows, t_start):
        seen["folded"] = json.loads(vj.read_text())["youtube_leg"]  # what is on disk when the point starts
        raise RuntimeError("the point crashed")

    monkeypatch.setattr(yle, "install_cleanup", lambda: None)  # no signal handlers in the pytest process
    monkeypatch.setattr(yle, "_measure", fake_measure)
    monkeypatch.setattr(yle, "_measure_input_point", fake_point)
    rc = yle.main(["--dir", str(tmp_path / "yl"), "--host", "h", "measure", "--verdict-json", str(vj)])
    assert rc == 0  # a crashed point never changes the exit
    assert seen["folded"]["verdict"] == "PASS"
    assert seen["folded"]["points"]["restreamer_input"]["reason"] == ylp.PENDING
    assert json.loads(vj.read_text())["youtube_leg"]["verdict"] == "PASS"


def test_the_point_reuses_the_recordings_early_tick_map(tmp_path):
    fk = _fakes(tmp_path)
    d = _stopped_with_tap(fk, tmp_path)
    args, verdict = _measure_inputs(tmp_path, fk)
    assert _run(fk, d, "prefetch", "--stream-path", STREAM_PATH).returncode == 0  # the early decode
    fk.script("verdict", "verdict", 0, PASS_RESULT)
    fk.script("verdict", "verdict", 0, dict(INPUT_RESULT, overall="PASS", reasons=[]))
    assert _run(fk, d, *args).returncode == 0
    seeded = Path(d) / "inpoint" / "ticks-rec-1.tsv"  # copied into the point's dir on dev2, fetched back
    assert seeded.read_text() == (Path(d) / "verdict" / "ticks-rec-1.tsv").read_text()
    assert [c for c in fk.calls() if c[0] == "prime"] and \
        len([c for c in fk.calls() if c[0] == "prime"]) == 1  # the recording is decoded once


def test_a_malformed_lag_jump_detail_never_breaks_the_blocking_digest():
    assert ylp.lag_jump_details({"details": {"lag_jumps": [[1.5, 2.0], [1], "x", [True, 2.0], None]}}) == [[1.5, 2.0]]
    assert ylp.lag_jump_details({"details": "garbage"}) == []
    assert ylp.lag_jump_details(None) == []
    assert yle.summarize({"windows": [{"name": "w", "audio": {"lag_jumps": 1, "details": {"lag_jumps": 5}}}]})[0][
        "audio_lag_jump_details"] == []

