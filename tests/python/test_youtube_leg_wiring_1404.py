"""Issue 1404 -- the YouTube stage is wired into every verdict-executing full-path E2E
(scripts/lib/youtube-leg.sh through call lines in scripts/recording-e2e.sh, the issue-675 pattern),
and its report reaches the GitHub job summary + a PR comment. The lib runs under the caller's
`set -euo pipefail`."""
import json
import os
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from youtube_leg_e2e_fakes_1404 import Fakes  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib" / "youtube-leg.sh"
E2E = REPO / "scripts" / "recording-e2e.sh"
WF = REPO / ".github" / "workflows" / "full-path-e2e.yml"


def _e2e():
    return E2E.read_text()


def test_the_lib_is_sourced_once():
    assert _e2e().count('. "$HERE/lib/youtube-leg.sh"') == 1


def test_the_stage_goes_live_before_the_stream_starts_recording():
    s = _e2e()
    start = s.index('youtube_leg_start "$HERE" "$STRIH" "$STREAM" "$STREAM_USER" "$STREAM_PW" "$OUTDIR"')
    assert s.count("youtube_leg_start ") == 1
    stream_rec = s.index('record --host "$STREAM" --action start\nSTREAM_RECORDING_STARTED=1')
    assert s.index("\ntrap cleanup EXIT HUP INT TERM") < start < stream_rec
    assert s.index('echo "[4j/8settle]') < start  # after every pin write, right before the recording


def test_the_stage_stops_right_after_the_stream_stoprecord():
    s = _e2e()
    stop_rec = s.index('STREAM_HOST_PATH=$(python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action stop)')
    stop = s.index('youtube_leg_stop "$HERE" "$STREAM" "$STREAM_USER" "$STREAM_PW" "$OUTDIR"')
    assert stop_rec < stop < s.index('program_audio_continuity_mark stop "$OUTDIR" "$STREAM"')
    assert s.count("youtube_leg_stop ") == 1
    # design 6387919956 part b: the stop hands the recording's path on, so the early pull + decode
    # on the measurement host start right away
    line = s[stop:s.index("\n", stop)]
    assert 'youtube_leg_stop "$HERE" "$STREAM" "$STREAM_USER" "$STREAM_PW" "$OUTDIR" "${STREAM_HOST_PATH:-}"' in line


def test_cleanup_stops_a_live_broadcast_right_after_the_stoprecord_first_block():
    s = _e2e()
    body = s[s.index("\ncleanup() {"):s.index("\ntrap cleanup EXIT HUP INT TERM")]
    call = body.index('youtube_leg_cleanup_stop "$HERE" "$STREAM" "$STREAM_USER" "$STREAM_PW" "$OUTDIR"')
    assert body.index('record --host "$IMAG_IP" --action stop') < call < body.index("rig_heartbeat_stop")


def test_the_gate_folds_into_gate_in_both_tails_before_the_report_and_the_exit():
    s = _e2e()
    merge = s.index('"$VERDICT_BIN" "${MERGE_ARGS[@]}" || GATE=$?')
    calls = [m.start() for m in re.finditer(re.escape('youtube_leg_gate "$HERE" "$STREAM"'), s)]
    assert len(calls) == 2, calls
    first_exit = s.index('exit "$GATE"', merge)
    assert merge < calls[0] < s.index('e2e_discord_report_send "$REPORT_JSON"') < first_exit
    assert first_exit < calls[1] < s.rindex('exit "$GATE"')
    for c in calls:
        line = s[c:s.index("\n", c)]
        assert line.endswith('then [ "$GATE" -ne 0 ] || GATE=1; fi')
        assert '"$MARKER_CSV" "$SWITCH_SCHEDULE_JSON" "$PROBE_BIN_DIR"' in line


def test_the_camera_box_dead_man_margin_covers_the_youtube_pre_record_wait():
    # the dead-man's first fire must land after StopRecord; the stage adds up to ~15 min before it
    # (session start + a recovered-session busy wait + StartStream + the live wait)
    s = _e2e()
    floor = int(re.search(r"^CAMERA_BOX_DEADMAN_OVERHEAD_FLOOR_MIN=(\d+)$", s, re.M).group(1))
    default = int(re.search(r'^CAMERA_BOX_DEADMAN_OVERHEAD_MIN="\$\{CAMERA_BOX_DEADMAN_OVERHEAD_MIN:-(\d+)\}"$',
                            s, re.M).group(1))
    assert floor >= 30 and default >= floor + 5, (floor, default)


def test_the_workflow_posts_the_per_point_report_and_has_the_time():
    w = WF.read_text()
    step = w[w.index("- name: Per-point E2E report"):]
    step = step[:step.index("\n      - name:")]
    assert "if: always() && github.event_name == 'pull_request'" in step
    assert 'scripts/e2e_report_youtube.py --verdict "/tmp/recording-e2e-${RUN_ID}/verdict-${RUN_ID}.json"' in step
    assert '>> "$GITHUB_STEP_SUMMARY"' in step and 'gh pr comment "$PR_NUMBER"' in step
    perms = w[w.index("permissions:"):w.index("\njobs:")]
    assert "pull-requests: write" in perms
    assert "timeout-minutes: 105" in w
    assert w.index("- name: Per-point E2E report") > w.index("- name: Recording-based 4-node")


# ------------------------------------------------------------------ the lib, run

def _bash(body, env):
    script = "set -euo pipefail\n. \"$1\"\nshift\n" + body
    return subprocess.run(["bash", "-c", script, "h", str(LIB)], capture_output=True, text=True, env=env, timeout=120)


def test_disabled_mode_runs_nothing_and_says_so(tmp_path):
    env = dict(os.environ, E2E_EXECUTE_VERDICT="0", YOUTUBE_LEG_E2E_PY="/nonexistent")
    r = _bash(f'youtube_leg_start /x 1.1.1.1 2.2.2.2 u p "{tmp_path}"; '
              f'youtube_leg_stop /x 2.2.2.2 u p "{tmp_path}"; youtube_leg_cleanup_stop /x 2.2.2.2 u p "{tmp_path}"; '
              f'youtube_leg_gate /x 2.2.2.2 u p "{tmp_path}" r h m s b; echo END', env)
    assert r.returncode == 0 and r.stdout.endswith("END\n"), r.stderr
    assert "not run in this mode" in r.stdout and "NOT measured" in r.stdout
    assert not (tmp_path / "youtube-leg").exists()


def test_start_runs_the_rig_busy_guard_first_then_goes_live_and_spawns_the_watch(tmp_path):
    fk = Fakes(tmp_path)
    fk.script("session", "start", 0, {"ok": True, "session_id": "s1", "broadcast_id": "b1"})
    fk.script("obs", "stream-start", 0, {"ok": True, "publish_utc": 1.0})
    fk.script("session", "wait-ready", 0, {"ok": True, "state": "ready"})
    env = dict(fk.env, E2E_EXECUTE_VERDICT="1", YOUTUBE_LEG_WATCH_POLL_S="0.05")
    order = tmp_path / "order"
    body = (f'stray_session_check_assert() {{ echo "guard:$4" >> "{order}"; }}\n'
            f'youtube_leg_start "{REPO}/scripts" 1.1.1.1 2.2.2.2 u p "{tmp_path}"\n'
            f'test -s "{tmp_path}/youtube-leg/watch.pid"\n'
            f'youtube_leg_stop "{REPO}/scripts" 2.2.2.2 u p "{tmp_path}"\n'
            f'test ! -e "{tmp_path}/youtube-leg/watch.pid"\n'
            f'test ! -e "{tmp_path}/youtube-leg/prefetch.pid"\necho END')  # no recording path: no prefetch
    r = _bash(body, env)
    assert r.returncode == 0 and r.stdout.endswith("END\n"), r.stdout + r.stderr
    assert order.read_text() == "guard:the YouTube stage StartStream\n"
    names = fk.names()
    assert names[:4] == ["guard", "session.start", "obs.stream-start", "session.wait-ready"]
    assert "obs.stream-stop" in names and "session.stop" in names
    st = json.loads((tmp_path / "youtube-leg" / "state.json").read_text())
    assert st["phase"] == "stopped"


def test_the_stop_with_the_recording_path_runs_the_prefetch_on_the_host(tmp_path):
    fk = Fakes(tmp_path)
    fk.script("session", "start", 0, {"ok": True, "session_id": "s1", "broadcast_id": "b1"})
    fk.script("obs", "stream-start", 0, {"ok": True, "publish_utc": 1.0})
    fk.script("obs", "record-start", 0, {"ok": True, "record_start_utc": 5.0})
    fk.script("session", "wait-ready", 0, {"ok": True, "state": "ready"})
    rec = fk.stream_file("/home/newlevel/2026-10-24 19-27-46.mp4")
    env = dict(fk.env, E2E_EXECUTE_VERDICT="1", YOUTUBE_LEG_WATCH_POLL_S="0.05")
    d = tmp_path / "youtube-leg"
    body = ('stray_session_check_assert() { :; }\n'
            f'youtube_leg_start "{REPO}/scripts" 1.1.1.1 2.2.2.2 u p "{tmp_path}"\n'
            'sleep 0.5\n'  # the watch reads the record start
            f'youtube_leg_stop "{REPO}/scripts" 2.2.2.2 u p "{tmp_path}" "{rec}"\n'
            f'test -s "{d}/prefetch.pid"\n'
            f'pid="$(cat "{d}/prefetch.pid")"; for _ in $(seq 1 300); do kill -0 "$pid" 2>/dev/null || break; '
            'sleep 0.1; done\necho END')
    r = _bash(body, env)
    assert r.returncode == 0 and r.stdout.endswith("END\n"), r.stdout + r.stderr
    pre = json.loads((d / "state.json").read_text())["prefetch"]
    assert pre["pulled"] is True and pre["decode_rc"] == 0, pre
    assert "sshpass" in fk.names() and "prime" in fk.names()


def test_a_failed_start_keeps_the_run_going(tmp_path):
    fk = Fakes(tmp_path)
    fk.script("session", "start", 2, {"ok": False, "reason": "HTTP 503 not_provisioned"})
    env = dict(fk.env, E2E_EXECUTE_VERDICT="1")
    body = ('stray_session_check_assert() { :; }\n'
            f'youtube_leg_start "{REPO}/scripts" 1.1.1.1 2.2.2.2 u p "{tmp_path}"\necho END')
    r = _bash(body, env)
    assert r.returncode == 0 and r.stdout.endswith("END\n")
    assert "did NOT start" in r.stderr and not (tmp_path / "youtube-leg" / "watch.pid").exists()


def test_the_gate_is_red_on_a_failed_stage_and_folds_the_reason(tmp_path):
    fk = Fakes(tmp_path)
    fk.script("guard", "guard", 1, None)
    env = dict(fk.env, E2E_EXECUTE_VERDICT="1")
    verdict = tmp_path / "verdict.json"
    verdict.write_text(json.dumps({"overall_pass": True}))
    body = ('stray_session_check_assert() { :; }\n'
            f'youtube_leg_start "{REPO}/scripts" 1.1.1.1 2.2.2.2 u p "{tmp_path}"\n'
            f'if youtube_leg_gate "{REPO}/scripts" 2.2.2.2 u p "{tmp_path}" "{verdict}" "" m s b; '
            'then echo GATE-PASS; else echo GATE-RED; fi')
    r = _bash(body, env)
    assert r.returncode == 0 and "GATE-RED" in r.stdout, r.stdout + r.stderr
    b = json.loads(verdict.read_text())["youtube_leg"]
    assert b["pass"] is False and "channel guard refused" in b["reasons"][0]


def test_the_rig_busy_gate_recovers_a_leftover_broadcast_only_with_a_marker():
    g = (REPO / "scripts" / "rig-busy-gate.sh").read_text()
    call = g.index('"$HERE/youtube_leg_e2e.py" --dir /nonexistent --host "$STREAM_HOST"')
    guard = g.rindex('if [ -f "$YOUTUBE_LEG_MARKER" ]; then', 0, call)
    assert guard < call < g.index("\nfor ((i = 1; i <= MAX_ITERATIONS; i++)); do")
    assert "--with-session" not in g[guard:call + 200]  # the gate has no ssh password: the stream half only


def test_the_gate_measures_on_the_host_and_never_pulls_the_recording_to_dev1():
    lib = LIB.read_text()
    fn = lib[lib.index("youtube_leg_gate() {"):]
    fn = fn[:fn.index("\n}\n")]
    assert "stream_box_download" not in fn and "win-ssh-exec.sh" not in fn
    assert 'measure --verdict-json "$report" --stream-path "$host_path"' in fn
    # the measurement host pulls over scp: the stream box's class goes along (a Windows box is refused)
    assert '--stream-platform "$(_youtube_leg_stream_platform "$stream")"' in fn


def test_the_stop_starts_the_prefetch_in_the_background(tmp_path):
    lib = LIB.read_text()
    fn = lib[lib.index("youtube_leg_stop() {"):]
    fn = fn[:fn.index("\n}\n")]
    pre = fn.index('prefetch --stream-path "$host_path") >>"$d/prefetch.log" 2>&1 &')
    assert fn.index('stop --record-stopped') < pre < fn.index('echo "$!" >"$d/prefetch.pid"')
    assert '--stream-platform "$(_youtube_leg_stream_platform "$stream")" prefetch' in fn


def test_cleanup_purges_the_measurement_host_in_the_background(tmp_path):
    lib = LIB.read_text()
    fn = lib[lib.index("youtube_leg_cleanup_stop() {"):]
    fn = fn[:fn.index("\n}\n")]
    purge = fn.index(" purge) >>")
    bound = fn.rindex("(timeout ", 0, purge)  # the purge is bounded
    assert fn.index("stop --stream-only") < bound and "\n" not in fn[bound:purge].replace("\\\n", "")
    assert fn[purge:].lstrip().count("&") >= 1


def test_cleanup_stops_the_stream_first_in_the_foreground_then_the_session(tmp_path):
    lib = LIB.read_text()
    fn = lib[lib.index("youtube_leg_cleanup_stop() {"):]
    fn = fn[:fn.index("\n}\n")]
    quick = fn.index("stop --stream-only")
    bg = fn.index('stop --why "cleanup()") >>')
    assert quick < bg and "timeout 10" in fn[:quick]
    assert fn[bg:].lstrip().count("&") >= 1
