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
  prefetch right after the stop, in the background (design 6387919956 parts b + c): the stream
           recording goes from the stream box straight to the MEASUREMENT HOST (dev2,
           scripts/youtube_leg_host.py) and its tick map is decoded there (`--prime-ticks`), while
           the E2E merges and YouTube processes the VOD. The outcome lands in the state.
  measure  after the merge: the VOD wait, the windows (the switch schedule, trimmed), the YouTube-leg
           verdict tool (scripts/youtube_leg_verdict.py) ON THE MEASUREMENT HOST against the stream
           recording (it finds the early tick map in its cache; a pull or a decode the prefetch did
           not finish is done again here, once), the verdict JSON + tick maps back to dev1, the
           host's stage dir deleted, and the fold of a `youtube_leg` block into the run's verdict JSON
           (a crash folds UNKNOWN). A measurement host that cannot be reached is UNKNOWN by name,
           never a measurement on dev1.
  purge    cleanup(): stop a prefetch still running and delete the host's stage dir (idempotent).

Every outcome other than a verdict PASS blocks the run (a refused delivery, a guard refusal, no VOD,
a tool UNKNOWN, a window left unmeasured): never a skip. State: <dir>/state.json (+ state.lock).
Exit 0 = OK / PASS, 1 = the stage failed (the reason is in the state and the block).

Test seams (an executable taking the same arguments): YOUTUBE_LEG_SESSION_CMD, YOUTUBE_LEG_OBS_CMD,
YOUTUBE_LEG_GUARD_CMD, YOUTUBE_LEG_SSH_CMD (ssh to the measurement host); YOUTUBE_LEG_MARKER (the dev1
marker path).
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

import youtube_leg_host as ylh  # noqa: E402
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
PREFETCH_POLL_S = 2.0
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

    def measure_host(self, stage):
        return ylh.Host(self.a.measure_host, ylh.stage_key(stage.dir), self.a.measure_workers)

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
    # a provisional publish time: a cancel inside StartStream still leaves recover a time to prove
    # the stream ours by (the real start lands within the request's bound, inside OURS_TOLERANCE_S)
    marker_write(ctx.marker, {"session_id": sid, "host": a.host, "dir": stage.dir, "publish_utc": time.time()})
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
            "measure_host": st.get("measure_host"), "early": st.get("prefetch"), "events": st.get("events", [])}


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


def _verdict_args(a, st, windows, host, rec):
    """The verdict tool's arguments on the measurement host (its paths in the host's stage dir)."""
    args = ["--vod", st["vod_id"], "--recording", f"{rec}@{st['record_start_utc']:.3f}",
            "--markers", host.path("in", "markers.csv"), "--out", host.path("verdict"),
            "--probe-bin", host.path("in", "recording-verdict")]
    for w in window_args(windows):
        args += ["--windows", w]
    if st.get("publish_utc") is not None:
        args += ["--publish", f"{st['publish_utc']:.3f}"]
    if st.get("unpublish_utc") is not None:
        args += ["--unpublish", f"{st['unpublish_utc']:.3f}"]
    return args


def recording_name(stream_path):
    """The stream recording's file name on the measurement host (its extension kept)."""
    base = os.path.basename(stream_path or "")
    ext = base.rsplit(".", 1)[1] if "." in base else "mkv"
    return "stream-recording." + ("".join(c for c in ext if c.isalnum()) or "mkv")


def prefetch_bound(decode_timeout):
    """How long a prefetch can take by its own bounds (check, sweep, code, pull, early decode)."""
    return (ylh.CHECK_TIMEOUT_S + 2 * ylh.STAGE_TIMEOUT_S + ylh.PULL_TIMEOUT_S + decode_timeout
            + 3 * ylh.LOCAL_SLACK_S)


def do_prefetch(ctx, stage):
    """Pull the stream recording to the measurement host and decode its tick map there, now (it
    overlaps the merge and the VOD wait). Every outcome lands in the state; never raises."""
    a = ctx.a
    st = stage.read()
    if st.get("phase") == "failed" or st.get("record_start_utc") is None:
        return 0  # nothing will be measured: the measure step names why
    host = ctx.measure_host(stage)
    name = recording_name(a.stream_path)
    info = {"host": host.host, "recording": name}
    _set(stage, f"the stream recording goes to the measurement host {host.host} and is decoded there now",
         prefetch=dict(info), measure_host=host.host)
    try:
        host.check()
        host.stage_code()
        t0 = time.monotonic()
        host.pull(a.user, os.environ.get("AV_GATE_SSH_PASSWORD", ""), a.host, a.stream_path, name)
        info.update(pulled=True, pull_s=round(time.monotonic() - t0, 1))
        _set(stage, f"the stream recording is on {host.host} ({info['pull_s']} s); its early decode starts",
             prefetch=dict(info))
        t0 = time.monotonic()
        rc, out = host.prime(host.path(name), a.decode_timeout)
        info.update(decode_rc=rc, decode_s=round(time.monotonic() - t0, 1))
        if rc != 0:
            info["reason"] = (f"ran over {a.decode_timeout:.0f} s" if rc in ylh.TIMED_OUT else
                              " ".join(out.split())[-300:] or f"exit {rc}")
    except ylh.HostError as e:
        info.update(reason=str(e), unreachable=e.unreachable)
    note = (f"the early decode on {host.host} finished in {info.get('decode_s')} s" if info.get("decode_rc") == 0
            else f"the early measurement did not finish: {info.get('reason')}")
    _set(stage, note, prefetch=dict(info))
    return 0


def _prefetch_pid(stage):
    try:
        with open(os.path.join(stage.dir, "prefetch.pid")) as f:
            return int(f.read().strip())
    except (OSError, ValueError):
        return None


def _end_prefetch(stage, why):
    """Stop a prefetch that still runs (its own cleanup kills its ssh steps); True if it did."""
    pid = _prefetch_pid(stage)
    if pid is None or not _alive(pid):
        return False
    with contextlib.suppress(OSError):
        os.kill(pid, 15)
    _set(stage, f"the prefetch was still running and was stopped ({why})")
    return True


def _await_prefetch(ctx, stage):
    """Wait (bounded by the prefetch's own bounds) for a prefetch still pulling or decoding."""
    pid = _prefetch_pid(stage)
    if pid is None or not _alive(pid):
        return
    _set(stage, "waiting for the early decode on the measurement host")
    end = time.monotonic() + prefetch_bound(ctx.a.decode_timeout)
    while _alive(pid) and time.monotonic() < end:
        time.sleep(PREFETCH_POLL_S)
    _end_prefetch(stage, "it ran past its own bounds")


def _measure_on_host(ctx, stage, host, windows, dropped):
    """The verdict tool on the measurement host: (verdict, reasons, result, tool_exit, windows, dropped)."""
    a = ctx.a
    _await_prefetch(ctx, stage)
    host.check()
    host.stage_code()
    name = recording_name(a.stream_path)
    pre = stage.read().get("prefetch") or {}
    if not (pre.get("pulled") and host.has(name)):
        _set(stage, f"the early pull did not deliver the recording ({pre.get('reason') or 'it never ran'}): "
                    "pulling it now (the one retry)")
        host.pull(a.user, os.environ.get("AV_GATE_SSH_PASSWORD", ""), a.host, a.stream_path, name)
        pre = dict(pre, retried_pull=True)
    if pre.get("decode_rc") != 0:
        _set(stage, f"the early decode did not finish ({pre.get('reason') or 'it never ran'}): the verdict run "
                    "decodes the recording again (the one retry)")
        pre = dict(pre, retried_decode=True)
    _set(stage, prefetch=pre)
    host.put_inputs(a.markers, a.probe_bin)
    st = stage.read()
    rc, out = host.run_tool(_verdict_args(a, st, windows, host, host.path(name)), a.tool_timeout)
    print(out[-3000:], flush=True)
    if rc in ylh.TIMED_OUT:
        return "UNKNOWN", [f"the YouTube-leg verdict tool ran over {a.tool_timeout:.0f} s"], None, None, windows, dropped
    got = host.fetch(os.path.join(a.dir, "verdict"))
    _set(stage, f"from {host.host}: {', '.join(got) or 'nothing'}")
    return _read_result(a, rc, windows, dropped)


def _read_result(a, tool_exit, windows, dropped):
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
    if not a.stream_path:
        return "UNKNOWN", ["the stream recording's path on the stream box is unknown (its StopRecord returned none)"], \
            None, None, [], []
    if not a.markers or not os.path.isfile(a.markers):
        return "UNKNOWN", [f"the cam2 marker log is missing ({a.markers or 'no path'})"], None, None, [], []
    if not os.path.isfile(a.probe_bin):
        return "UNKNOWN", [f"the recording-verdict probe binary is missing on dev1 ({a.probe_bin})"], None, None, [], []
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
    host = ctx.measure_host(stage)
    _set(stage, f"measuring on {host.host}", measure_host=host.host)
    try:
        return _measure_on_host(ctx, stage, host, windows, dropped)
    except ylh.HostError as e:
        return "UNKNOWN", [str(e)], None, None, windows, dropped


def _cleanup_host(ctx, stage):
    """Stop a running prefetch and delete the measurement host's stage dir; never raises."""
    _end_prefetch(stage, "the measurement is over")
    host = ctx.measure_host(stage)
    try:
        host.cleanup()
        _set(stage, f"the stage dir on {host.host} is deleted (the videos with it)")
        return True
    except ylh.HostError as e:
        _set(stage, f"the stage dir on {host.host} could NOT be deleted: {e}")
        return False


def read_decode_reads(verdict_dir):
    """The decodes' read counters (`<tick map>.reads.json`) by tick map name."""
    out = {}
    try:
        names = sorted(os.listdir(verdict_dir))
    except OSError:
        return out
    for n in names:
        if n.endswith(".reads.json"):
            with contextlib.suppress(OSError, ValueError):
                with open(os.path.join(verdict_dir, n)) as f:
                    out[n[:-len(".reads.json")]] = json.load(f)
    return out


def do_measure(ctx, stage):
    try:
        verdict, reasons, result, tool_exit, windows, dropped = _measure(ctx, stage)
    except Exception as e:  # noqa: BLE001 -- any crash is a red UNKNOWN block, never a missing one
        traceback.print_exc()
        verdict, reasons, result, tool_exit, windows, dropped = (
            "UNKNOWN", [f"the YouTube stage's measurement crashed: {type(e).__name__}: {e}"], None, None, [], [])
    if stage.read().get("measure_host"):  # the host was used: its stage dir goes, the videos with it
        _cleanup_host(ctx, stage)
    if dropped:
        reasons = reasons + [f"windows shorter than {WINDOW_MIN_S:.0f} s after the trim, not measured: "
                             + ", ".join(dropped)]
    block = make_block(verdict, reasons, stage.read(), result, tool_exit, windows, dropped)
    block["decode_reads"] = read_decode_reads(os.path.join(stage.dir, "verdict"))
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
    ap.add_argument("--measure-host", default=os.environ.get("YOUTUBE_LEG_MEASURE_HOST", ylh.DEFAULT_HOST),
                    help="where the recording + VOD are pulled, decoded and judged (ssh, key auth)")
    ap.add_argument("--measure-workers", type=int,
                    default=int(os.environ.get("YOUTUBE_LEG_MEASURE_WORKERS", ylh.DEFAULT_WORKERS)))
    tool_timeout = float(os.environ.get("YOUTUBE_LEG_TOOL_TIMEOUT_S", 1500))
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
    p = sub.add_parser("prefetch")
    p.add_argument("--stream-path", required=True, help="the stream recording on the stream box (StopRecord)")
    p.add_argument("--decode-timeout", type=float, default=tool_timeout)
    sub.add_parser("purge")
    p = sub.add_parser("measure")
    p.add_argument("--verdict-json", default="")
    p.add_argument("--stream-path", default="", help="the stream recording on the stream box (StopRecord)")
    p.add_argument("--markers", default="")
    p.add_argument("--schedule", default="")
    p.add_argument("--probe-bin", default="recording-verdict")
    p.add_argument("--done-deadline", type=float, default=float(os.environ.get("YOUTUBE_LEG_DONE_DEADLINE_S", 900)))
    p.add_argument("--tool-timeout", type=float, default=tool_timeout)
    p.add_argument("--decode-timeout", type=float, default=tool_timeout)
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
    if a.cmd == "prefetch":
        return do_prefetch(ctx, stage)
    if a.cmd == "purge":
        if stage.read().get("measure_host"):
            _cleanup_host(ctx, stage)
        return 0
    return do_measure(ctx, stage)


if __name__ == "__main__":
    sys.exit(main())
