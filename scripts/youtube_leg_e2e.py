#!/usr/bin/env python3
"""Issue 1404 -- the full-path E2E's BLOCKING YouTube stage (main design 6383793524, Approach 1).

The owner's acceptance point (ROZHODNUTE 5.10.2026) is YouTube's CI test stream "e2e rtmp", read back
as a viewer. The E2E (scripts/recording-e2e.sh, through scripts/lib/youtube-leg.sh) runs:

  start    before StartRecord: a leftover of an earlier run is recovered first, then the
           program-audio channel guard, restreamer's av-gate session (scripts/av_gate_session.py),
           StartStream on the stream OBS (scripts/youtube_leg_obs.py), and the bounded wait until
           the broadcast is live. Any refusal stops what was started.
  watch    in the background while live: the channel guard every --poll s (anything but a clean
           reading stops the stream + the session at once), the recording's start time, and a
           harness that died without its cleanup.
  stop     after the stream StopRecord, in cleanup(), and from the watch: StopStream + the session
           stop; idempotent, under a lock (--stream-only: the quick half cleanup() runs first).
  recover  a broadcast a cancelled run left live: the dev1 marker (--marker) names it; its stream is
           stopped only when the stream's own age proves it is ours, its session when the ssh
           password is there. Called by rig-busy-gate.sh (a stream we left would block the next
           run there) and by `start`. Always exit 0; it prints what it did.
  measure  after the merge: the VOD wait, the windows (the switch schedule, trimmed), the YouTube-leg
           verdict tool (scripts/youtube_leg_verdict.py, on dev1) against the stream recording, and
           the fold of a `youtube_leg` block into the run's verdict JSON (a crash folds UNKNOWN).

Every outcome other than a verdict PASS blocks the run (a refused delivery, a guard refusal, no VOD,
a tool UNKNOWN, a window left unmeasured): never a skip. State: <dir>/state.json (+ state.lock).
Exit 0 = OK / PASS, 1 = the stage failed (the reason is in the state and the block).

Test seams (an executable taking the same arguments): YOUTUBE_LEG_SESSION_CMD, YOUTUBE_LEG_OBS_CMD,
YOUTUBE_LEG_GUARD_CMD, YOUTUBE_LEG_VERDICT_CMD; YOUTUBE_LEG_MARKER (the dev1 marker path).
"""
import argparse
import contextlib
import fcntl
import json
import os
import subprocess
import sys
import time
import traceback

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

from youtube_leg_proc import install_cleanup, run_bounded  # noqa: E402

SCHEMA = 1
BLOCK_KEY = "youtube_leg"
WINDOW_HEAD_S = 3.0   # skip the start of each segment (the cut + the genlock settle)
WINDOW_TAIL_S = 2.0   # end before the next cut / the StopStream (the tool wants >= 1 s)
WINDOW_MIN_S = 10.0
GUARD_TIMEOUT_S, STEP_TIMEOUT_S = 20, 90
OURS_TOLERANCE_S = 120.0  # a leftover stream is ours when its own start is this close to our publish
BUSY_WAIT_S = 300.0       # restreamer keeps the slot while it tears our recovered session down
DEFAULT_MARKER = os.path.join(os.path.expanduser("~"), ".camera-box", "youtube-leg-live.json")
FOREIGN_REASON = ("non-measurement audio on the stream program -- the YouTube broadcast was stopped at "
                  "once to protect the channel")


def _cmd(env_name, script):
    override = os.environ.get(env_name)
    return [override] if override else [sys.executable, os.path.join(_HERE, script)]


def run_json(cmd, timeout):
    """(rc, decoded last JSON line | {}, text) of a step, its whole process group bounded; a
    timeout or a launch error = rc 2 with a reason."""
    try:
        r = run_bounded(cmd, timeout, text=True)
    except subprocess.TimeoutExpired:
        name = os.path.basename(cmd[1] if len(cmd) > 1 else cmd[0])
        return 2, {"reason": f"{name} timed out after {timeout} s"}, ""
    except OSError as e:
        return 2, {"reason": f"cannot run {cmd[0]}: {e}"}, ""
    data = {}
    for line in reversed((r.stdout or "").strip().splitlines()):
        try:
            data = json.loads(line)
            break
        except ValueError:
            continue
    if not isinstance(data, dict):
        data = {}
    if r.returncode != 0 and not data.get("reason"):
        data["reason"] = ((r.stderr or "") or (r.stdout or "")).strip()[-300:] or f"exit {r.returncode}"
    return r.returncode, data, (r.stdout or "") + (r.stderr or "")


# ------------------------------------------------------------------ state + marker

class Stage:
    def __init__(self, d):
        self.dir = d
        os.makedirs(d, exist_ok=True)
        self.path = os.path.join(d, "state.json")

    def read(self):
        try:
            with open(self.path) as f:
                st = json.load(f)
            return st if isinstance(st, dict) else {}
        except (OSError, ValueError):
            return {}

    def write(self, st):
        tmp = self.path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(st, f, indent=1)
        os.replace(tmp, self.path)

    @contextlib.contextmanager
    def locked(self):
        with open(os.path.join(self.dir, "state.lock"), "w") as lk:
            fcntl.flock(lk, fcntl.LOCK_EX)
            st = self.read()
            yield st
            self.write(st)


def _note(st, text):
    st.setdefault("events", []).append(f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())} {text}")
    print(f"    [youtube-leg] {text}", flush=True)


def _fail(st, reason):
    if not st.get("failure"):
        st["failure"] = reason
    st["phase"] = "failed"
    _note(st, f"FAILED: {reason}")


def _set(stage, note=None, failure=None, **kv):
    """One short locked update (never held across a slow step: a stop from cleanup() or the watch
    must always get the lock within seconds)."""
    with stage.locked() as st:
        st.update(kv)
        if note:
            _note(st, note)
        if failure:
            _fail(st, failure)
        return dict(st)


def marker_write(path, data):
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(data, f)
    os.replace(tmp, path)


def marker_read(path):
    try:
        with open(path) as f:
            m = json.load(f)
        return m if isinstance(m, dict) else None
    except (OSError, ValueError):
        return None


def marker_clear(path, session_id):
    """Remove the marker only when it still names this stage's session (a later run's is kept)."""
    m = marker_read(path)
    if m is not None and m.get("session_id") == session_id:
        with contextlib.suppress(OSError):
            os.remove(path)


# ------------------------------------------------------------------ the steps

class Ctx:
    def __init__(self, a):
        self.a = a
        self.session = _cmd("YOUTUBE_LEG_SESSION_CMD", "av_gate_session.py") + [
            "--host", a.host, "--user", a.user]
        self.obs = _cmd("YOUTUBE_LEG_OBS_CMD", "youtube_leg_obs.py") + ["--host", a.host]
        self.guard = _cmd("YOUTUBE_LEG_GUARD_CMD", "program_audio_guard.py")
        self.marker = a.marker or os.environ.get("YOUTUBE_LEG_MARKER") or DEFAULT_MARKER

    def guard_ok(self):
        rc, _, text = run_json(self.guard, GUARD_TIMEOUT_S)
        return rc == 0, " ".join(text.split())[:300]


def do_stop(ctx, stage, why, stream_only=False, record_stopped=False):
    """StopStream + the session stop, each once; True when both are done (or never began)."""
    with stage.locked() as st:
        if record_stopped:
            st.setdefault("record_stop_utc", time.time())
        ok = True
        if st.get("stream_started") and not st.get("stream_stopped"):
            rc, d, _ = run_json(ctx.obs + ["stream-stop"], STEP_TIMEOUT_S)
            if rc == 0:
                st["stream_stopped"] = True
                st["unpublish_utc"] = d.get("unpublish_utc")
                _note(st, f"StopStream done ({why})")
            else:
                ok = False
                _note(st, f"StopStream FAILED ({why}): {d.get('reason')}")
        if stream_only:
            return ok
        if st.get("session_id") and not st.get("session_stopped"):
            rc, d, _ = run_json(ctx.session + ["stop", "--id", st["session_id"]], STEP_TIMEOUT_S)
            if rc == 0:
                st["session_stopped"] = True
                _note(st, f"restreamer session {st['session_id']} stop requested ({why})")
            else:
                ok = False
                _note(st, f"restreamer session stop FAILED ({why}): {d.get('reason')}")
        if ok and st.get("phase") in ("starting", "live"):
            st["phase"] = "stopped"
        if ok and st.get("session_id"):
            marker_clear(ctx.marker, st["session_id"])
        return ok


def do_recover(ctx, with_session):
    """Stop what a cancelled earlier run left live (the marker names it); returns the recovered
    session id (so a 409 busy on that holder can be waited out) or None. Never raises."""
    m = marker_read(ctx.marker)
    if not m:
        return None
    sid, publish = m.get("session_id"), m.get("publish_utc")
    print(f"    [youtube-leg] recover: a previous run left the YouTube stage marked live (session {sid})", flush=True)
    stream_done = True
    rc, d, _ = run_json(ctx.obs + ["stream-status"], STEP_TIMEOUT_S)
    if rc != 0:
        stream_done = False
        print(f"    [youtube-leg] recover: stream status unreadable, kept the marker: {d.get('reason')}", flush=True)
    elif d.get("active"):
        since = d.get("since_utc")
        if isinstance(publish, (int, float)) and isinstance(since, (int, float)) \
                and abs(since - publish) <= OURS_TOLERANCE_S:
            rc, d2, _ = run_json(ctx.obs + ["stream-stop"], STEP_TIMEOUT_S)
            stream_done = rc == 0
            print("    [youtube-leg] recover: " + ("StopStream of OUR leftover stream done" if stream_done else
                  f"StopStream of our leftover stream FAILED: {d2.get('reason')}"), flush=True)
        else:
            print(f"    [youtube-leg] recover: the stream box streams, but it is NOT our leftover (started "
                  f"{since}, ours published {publish}): never touched", flush=True)
    session_done = not sid
    if sid and with_session:
        rc, d, _ = run_json(ctx.session + ["stop", "--id", sid], STEP_TIMEOUT_S)
        session_done = rc == 0
        print(f"    [youtube-leg] recover: session {sid} stop " + ("requested" if session_done else
              f"FAILED: {d.get('reason')}"), flush=True)
    if stream_done and session_done:
        with contextlib.suppress(OSError):
            os.remove(ctx.marker)
    return sid


def _session_start(ctx, recovered):
    a = ctx.a
    args = ctx.session + ["start", "--requester", a.requester, "--title", a.title]
    end = time.monotonic() + (BUSY_WAIT_S if recovered else 0)
    while True:
        rc, d, _ = run_json(args, STEP_TIMEOUT_S)
        if rc == 0 and d.get("session_id"):
            return rc, d
        if recovered and d.get("holder_session_id") == recovered and time.monotonic() < end:
            print(f"    [youtube-leg] restreamer still tears down the recovered session {recovered}, waiting", flush=True)
            time.sleep(a.busy_poll)
            continue
        return rc, d


def do_start(ctx, stage):
    a = ctx.a
    with stage.locked() as st:
        st.clear()
        st.update({"schema": SCHEMA, "phase": "starting", "host": a.host})
    recovered = do_recover(ctx, with_session=True)
    clean, line = ctx.guard_ok()
    if not clean:
        _set(stage, failure=f"the program-audio channel guard refused before the broadcast: {line}")
        return 1
    rc, d = _session_start(ctx, recovered)
    if rc != 0 or not d.get("session_id"):
        _set(stage, failure=f"restreamer delivery refused: {d.get('reason')}")
        return 1
    sid = d["session_id"]
    _set(stage, f"restreamer session {sid} (broadcast {d.get('broadcast_id')}) starting",
         session_id=sid, broadcast_id=d.get("broadcast_id"))
    marker_write(ctx.marker, {"session_id": sid, "host": a.host, "dir": stage.dir, "publish_utc": None})
    # stream_started is set BEFORE the request: a StartStream whose answer is lost must still be stopped
    if _set(stage, stream_started=True).get("phase") != "starting":  # cleanup() stopped the stage meanwhile
        return 1
    rc, d, _ = run_json(ctx.obs + ["stream-start"], STEP_TIMEOUT_S)
    if rc != 0:
        # a stream that was already running is not ours: never stop it
        _set(stage, failure=f"StartStream on the stream OBS failed: {d.get('reason')}",
             stream_started=not d.get("foreign"))
    else:
        marker_write(ctx.marker, {"session_id": sid, "host": a.host, "dir": stage.dir,
                                  "publish_utc": d.get("publish_utc")})
        st = _set(stage, "StartStream done, waiting for the broadcast to go live", publish_utc=d.get("publish_utc"))
        if st.get("phase") != "starting":  # a stop raced the StartStream: stop the output we just started
            rc2, d2, _ = run_json(ctx.obs + ["stream-stop"], STEP_TIMEOUT_S)
            _set(stage, "StopStream after a stop raced the StartStream: " + ("done" if rc2 == 0 else
                 f"FAILED: {d2.get('reason')}"), stream_stopped=rc2 == 0)
            return 1
        rc, d, _ = run_json(ctx.session + ["wait-ready", "--id", sid, "--deadline", str(a.ready_deadline)],
                            a.ready_deadline + 120)
        if rc != 0:
            _set(stage, failure=f"the YouTube broadcast never went live: {d.get('reason')}")
        else:
            with stage.locked() as st:
                if st.get("phase") != "starting":  # a stop (cleanup) or the guard came first
                    return 1
                st["phase"] = "live"
                _note(st, "the YouTube broadcast is LIVE")
            return 0
    do_stop(ctx, stage, "the start failed")
    return 1


def _alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    try:  # a zombie answers kill -0: read its state
        with open(f"/proc/{pid}/stat") as f:
            return f.read().rsplit(")", 1)[1].split()[0] != "Z"
    except (OSError, IndexError):
        return True


def do_watch(ctx, stage):
    a = ctx.a
    for _ in range(int(a.max_s / max(a.poll, 0.05)) + 1):
        st = stage.read()
        if st.get("phase") != "live":
            return 0
        if not _alive(a.parent):
            with stage.locked() as st:
                _note(st, "the E2E harness is gone without its cleanup")
            do_stop(ctx, stage, "the harness died")
            return 0
        clean, line = ctx.guard_ok()
        if not clean:
            with stage.locked() as st:
                if st.get("phase") == "live":
                    _fail(st, f"{FOREIGN_REASON}: {line}")
            do_stop(ctx, stage, "the channel guard refused")
            return 0
        if st.get("record_start_utc") is None:
            rc, d, _ = run_json(ctx.obs + ["record-start"], STEP_TIMEOUT_S)
            if rc == 0 and d.get("record_start_utc") is not None:
                with stage.locked() as st:
                    st.setdefault("record_start_utc", d["record_start_utc"])
                    _note(st, "the stream recording start read from OBS")
        time.sleep(a.poll)
    return 0


# ------------------------------------------------------------------ measure + fold

def windows_from_schedule(schedule, head_s=WINDOW_HEAD_S, tail_s=WINDOW_TAIL_S, min_s=WINDOW_MIN_S):
    """[(name, start_s, end_s)] from the switch schedule's [{cambox, start_ns, end_ns}], each
    segment trimmed; a segment shorter than min_s after the trim is left out (and named)."""
    out, dropped = [], []
    for i, w in enumerate(schedule or []):
        a = int(w["start_ns"]) / 1e9 + head_s
        b = int(w["end_ns"]) / 1e9 - tail_s
        name = f"w{i + 1:02d}-{w.get('cambox', '?')}"
        (out if b - a >= min_s else dropped).append((name, a, b))
    return out, [d[0] for d in dropped]


def window_args(windows):
    return [f"{n}:{a:.3f}:{b:.3f}" for n, a, b in windows]


def summarize(result):
    """The block's per-window digest of the tool's youtube-leg-verdict.json."""
    rows = []
    for w in result.get("windows") or []:
        ds, au, av = w.get("dupskip") or {}, w.get("audio") or {}, w.get("av") or {}
        rows.append({"name": w.get("name"), "errors": w.get("errors") or [],
                     "dup": ds.get("dup"), "skip": ds.get("skip"),
                     "audio_lag_jumps": au.get("lag_jumps"), "audio_low_corr": au.get("low_corr"),
                     "audio_silent": au.get("silent"), "av_delta_ms": av.get("delta_ms"),
                     "av_vod_ms": av.get("vod_ms"), "av_rec_ms": av.get("rec_ms")})
    return rows


def make_block(verdict, reasons, st, result=None, tool_exit=None, windows=None, dropped=None):
    return {"schema": SCHEMA, "verdict": verdict, "pass": verdict == "PASS", "gates_overall_pass": True,
            "end_point": "YouTube VOD (e2e rtmp)", "reasons": reasons,
            "session_id": st.get("session_id"), "broadcast_id": st.get("broadcast_id"),
            "vod_id": st.get("vod_id"), "publish_utc": st.get("publish_utc"),
            "unpublish_utc": st.get("unpublish_utc"), "record_start_utc": st.get("record_start_utc"),
            "tool_exit": tool_exit, "windows_measured": len(windows or []), "windows_dropped": dropped or [],
            "windows": summarize(result or {}), "criteria": ((result or {}).get("criteria") or {}).get("status"),
            "events": st.get("events", [])}


def fold(verdict_json, block):
    """Put the block into the run's verdict JSON (when it exists); the merge's own overall_pass is
    left as it is, like the program-audio block: the E2E folds the stage into its exit."""
    if not verdict_json or not os.path.isfile(verdict_json):
        return False
    with open(verdict_json) as f:
        v = json.load(f)
    v[BLOCK_KEY] = block
    tmp = verdict_json + ".tmp"
    with open(tmp, "w") as f:
        json.dump(v, f, indent=1)
    os.replace(tmp, verdict_json)
    return True


def _verdict_cmd(a, st, windows):
    cmd = _cmd("YOUTUBE_LEG_VERDICT_CMD", "youtube_leg_verdict.py") + [
        "--vod", st["vod_id"], "--recording", f"{a.recording}@{st['record_start_utc']:.3f}",
        "--markers", a.markers, "--out", os.path.join(a.dir, "verdict"), "--probe-bin", a.probe_bin]
    for w in window_args(windows):
        cmd += ["--windows", w]
    if st.get("publish_utc") is not None:
        cmd += ["--publish", f"{st['publish_utc']:.3f}"]
    if st.get("unpublish_utc") is not None:
        cmd += ["--unpublish", f"{st['unpublish_utc']:.3f}"]
    return cmd


def _windows(a, st):
    schedule = []
    if a.schedule and os.path.isfile(a.schedule):
        with open(a.schedule) as f:
            schedule = json.load(f)
    if schedule:
        return windows_from_schedule(schedule)
    # no sweep: the one steady-state window inside the recording
    end = st.get("record_stop_utc") or st.get("unpublish_utc")
    return [("w01-steady", st["record_start_utc"] + 5.0, end - 3.0)], []


def _measure(ctx, stage):
    """(verdict, reasons, result, tool_exit, windows, dropped)."""
    a = ctx.a
    st = stage.read()
    if not st:
        return "UNKNOWN", ["the YouTube stage never started (no stage state)"], None, None, [], []
    if st.get("phase") == "failed":
        return "UNKNOWN", [st.get("failure") or "the stage failed"], None, None, [], []
    if not (st.get("stream_stopped") and st.get("session_stopped")):
        return "UNKNOWN", ["the broadcast was never stopped cleanly, the VOD has no defined end"], None, None, [], []
    if st.get("record_start_utc") is None:
        return "UNKNOWN", ["the stream recording's start time was never read from OBS"], None, None, [], []
    if not a.recording or not os.path.isfile(a.recording):
        return "UNKNOWN", [f"the stream recording did not reach dev1 ({a.recording or 'no path'})"], None, None, [], []
    if not a.markers or not os.path.isfile(a.markers):
        return "UNKNOWN", [f"the cam2 marker log is missing ({a.markers or 'no path'})"], None, None, [], []
    windows, dropped = _windows(a, st)
    if not windows:
        return "UNKNOWN", ["no measurable window"], None, None, [], dropped
    rc, d, _ = run_json(ctx.session + ["wait-done", "--id", st["session_id"], "--deadline", str(a.done_deadline)],
                        a.done_deadline + 120)
    if rc != 0 or not d.get("vod_id"):
        return "UNKNOWN", [f"no YouTube VOD: {d.get('reason')}"], None, None, windows, dropped
    with stage.locked() as st:
        st["vod_id"] = d["vod_id"]
        _note(st, f"the YouTube VOD {d['vod_id']} is processed")
    try:
        r = run_bounded(_verdict_cmd(a, st, windows), a.tool_timeout, text=True)
        tool_exit = r.returncode
        print(((r.stdout or "") + (r.stderr or ""))[-3000:], flush=True)
    except subprocess.TimeoutExpired:
        return "UNKNOWN", [f"the YouTube-leg verdict tool ran over {a.tool_timeout:.0f} s"], None, None, windows, dropped
    path = os.path.join(a.dir, "verdict", "youtube-leg-verdict.json")
    try:
        with open(path) as f:
            result = json.load(f)
    except (OSError, ValueError) as e:
        return "UNKNOWN", [f"the verdict tool wrote no verdict (exit {tool_exit}): {e}"], None, tool_exit, windows, dropped
    overall = result.get("overall")
    verdict = overall if overall in ("PASS", "FAIL") and tool_exit == {"PASS": 0, "FAIL": 1}[overall] else "UNKNOWN"
    reasons = list(result.get("reasons") or [])
    if verdict == "UNKNOWN" and not reasons:
        reasons = [f"the verdict tool said {overall!r} with exit {tool_exit}"]
    if dropped and verdict == "PASS":
        verdict = "UNKNOWN"  # a part of the recording was never judged on YouTube
    return verdict, reasons, result, tool_exit, windows, dropped


def do_measure(ctx, stage):
    try:
        verdict, reasons, result, tool_exit, windows, dropped = _measure(ctx, stage)
    except Exception as e:  # noqa: BLE001 -- any crash is a red UNKNOWN block, never a missing one
        traceback.print_exc()
        verdict, reasons, result, tool_exit, windows, dropped = (
            "UNKNOWN", [f"the YouTube stage's measurement crashed: {type(e).__name__}: {e}"], None, None, [], [])
    if dropped:
        reasons = reasons + [f"windows shorter than {WINDOW_MIN_S:.0f} s after the trim, not measured: "
                             + ", ".join(dropped)]
    block = make_block(verdict, reasons, stage.read(), result, tool_exit, windows, dropped)
    with open(os.path.join(stage.dir, "youtube-leg-block.json"), "w") as f:
        json.dump(block, f, indent=1)
    try:
        folded = fold(ctx.a.verdict_json, block)
    except (OSError, ValueError) as e:
        folded = False
        print(f"    [youtube-leg] could not fold the block into {ctx.a.verdict_json}: {e}", flush=True)
    print(f"    [youtube-leg] YouTube end point: {verdict}" + ("" if folded else " (no verdict JSON to fold into)"))
    for r in reasons:
        print(f"      - {r}")
    return 0 if verdict == "PASS" else 1


def main(argv=None):
    ap = argparse.ArgumentParser(description="issue 1404: the full-path E2E's YouTube stage")
    ap.add_argument("--dir", required=True, help="the stage's state dir")
    ap.add_argument("--host", default="", help="the stream box")
    ap.add_argument("--user", default="newlevel")
    ap.add_argument("--marker", default="", help=f"the dev1 live marker (default $YOUTUBE_LEG_MARKER, {DEFAULT_MARKER})")
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("start")
    p.add_argument("--requester", default="camera-box full-path E2E")
    p.add_argument("--title", default="camera-box E2E")
    p.add_argument("--ready-deadline", type=float, default=float(os.environ.get("YOUTUBE_LEG_READY_DEADLINE_S", 420)))
    p.add_argument("--busy-poll", type=float, default=15.0)
    p = sub.add_parser("stop")
    p.add_argument("--why", default="the E2E stopped recording")
    p.add_argument("--stream-only", action="store_true", help="only the StopStream half (cleanup()'s quick first step)")
    p.add_argument("--record-stopped", action="store_true", help="the stream StopRecord just returned: note the time")
    p = sub.add_parser("recover")
    p.add_argument("--with-session", action="store_true", help="also stop the leftover restreamer session (needs ssh)")
    p = sub.add_parser("watch")
    p.add_argument("--parent", type=int, required=True)
    p.add_argument("--poll", type=float, default=float(os.environ.get("YOUTUBE_LEG_WATCH_POLL_S", 10)))
    p.add_argument("--max-s", type=float, default=4 * 3600.0)
    p = sub.add_parser("measure")
    p.add_argument("--verdict-json", default="")
    p.add_argument("--recording", default="")
    p.add_argument("--markers", default="")
    p.add_argument("--schedule", default="")
    p.add_argument("--probe-bin", default="recording-verdict")
    p.add_argument("--done-deadline", type=float, default=float(os.environ.get("YOUTUBE_LEG_DONE_DEADLINE_S", 900)))
    p.add_argument("--tool-timeout", type=float, default=float(os.environ.get("YOUTUBE_LEG_TOOL_TIMEOUT_S", 1500)))
    a = ap.parse_args(argv)
    install_cleanup()
    ctx = Ctx(a)
    if a.cmd == "recover":  # needs no stage dir: the marker names everything
        do_recover(ctx, a.with_session)
        return 0
    stage = Stage(a.dir)
    if a.cmd == "start":
        return do_start(ctx, stage)
    if a.cmd == "stop":
        return 0 if do_stop(ctx, stage, a.why, a.stream_only, a.record_stopped) else 1
    if a.cmd == "watch":
        return do_watch(ctx, stage)
    return do_measure(ctx, stage)


if __name__ == "__main__":
    sys.exit(main())
