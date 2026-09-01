#!/usr/bin/env python3
"""rig-health-audit -- one read-only sweep over EVERY rig node, one PASS/WARN/FAIL line each (#787).

The user's mandate (2026-07-16, after a full overnight rig power-off): health must be provable
FROM LOGS, clearly enough that a human (or a future status page) can see at a glance that every
node is fully healthy. This script reads each node's OWN authoritative signals -- it never
mutates anything (pure ssh/WS reads):

  cam1..cam7  camera-box journal (emitted/captured fps, capture-dropped, chroma), dantesync
              NTP offset, / mounted ro, load
  imag        OBS GetStats over local WS (activeFps, avg render ms, skip%), per-source NDI
              arrival rate from the genlock-fifo audit log lines, isolcpus absence (#784),
              watchdog service, dantesync, load
  strih       obs64 process, OBS GetStats over LAN WS (auth), per-source arrival rates +
              audio-buffering peak from the newest OBS log (#786)
  stream      same as strih (no WS auth) + genlock latency_ms + guarded-launch log presence

Exit code: 0 = every line PASS, 1 = any WARN, 2 = any FAIL. Output is line-oriented on purpose:
`[VERDICT] node  key=value ...` -- greppable, journal-friendly, status-page-parseable.

Secrets: the strih OBS WS password is read from ~/.config/camera-box/obs-ws-pass (LOCAL file,
never committed). SSH uses the rig's standard throwaway creds (targets.md).
"""
import base64
import hashlib
import json
import os
import re
import subprocess
import sys

SSH_PW = "newlevel"
CAMS = {f"cam{n}": f"10.77.9.6{n}" for n in range(1, 8)}
IMAG = "10.77.9.182"
STRIH = "10.77.9.202"
STREAM = "10.77.9.204"
DANTE_BOUND_US = 2000          # clock-offset-guard verdict bound
AUDIO_BUF_BOUND_MS = 100       # #786 launch-gate bound (box standard 64/85)
# issue-1108 dantesync NTP step-rate facet: how often dantesync STEPPED the clock in the last hour.
# A step-storm on the strih NTP master jumps every box's genlock timecode -> per-source FIFO
# underruns -> the QR/burn ball skipping fleet-wide. Grade tiers from the issue's measured data:
NTP_STEP_HEALTHY_CEIL = 72     # steps/h; healthy ceiling (baseline was ~30-36/h) -> OK at or below
NTP_STEP_STORM_BOUND = 120     # steps/h; mirrors dantesync's OWN step-storm boundary (dantesync
                               # issue 91 / v1.8.45); the issue-1108 storm floor measured 129/h,
                               # strih peaked 147-180/h -> FAIL at or above (WARN in the 73-119 band)
# The Linux-node counter: count dantesync `[NTP] Stepped` events over the last hour, on the box (one
# bounded line of output regardless of storm size). ONE source of truth -- used verbatim in the
# check_cam + check_imag ssh commands AND pinned by a test that runs THIS exact awk against synthetic
# journals (the awk analogue of the CADENCE_LIB shell kernel), so there is no second Python copy of
# the match logic to drift. `\[NTP\]` matches the literal bracket; END always prints (0 on no match).
NTP_STEP_COUNT_AWK = r'/\[NTP\] Stepped/{c++} END{print "ntp_steps_1h=" c+0}'
AUDIT_RE = re.compile(r"^(\d+):(\d+):(\d+)\.(\d+): genlock-fifo audit '([^']+)': received=(\d+)")
BUF_RE = re.compile(r"total audio buffering is now (\d+) milliseconds")
# #794/#1089: the shared PURE cadence kernel (measure + classify), reused by shelling out so the
# issue-797 phantom-50 divisor lives in ONE tested place -- never a second Python divisor here.
CADENCE_LIB = os.path.join(os.path.dirname(os.path.abspath(__file__)), "lib", "cadence-health.sh")
# strih CAMERA source labels only (`NDI cam1..7`); excludes `NDI 2ME PGM (mv)` / `NDI 2ME PVW`,
# which are 30 fps by design and must NOT be graded against 60 fps.
CAMERA_SRC_RE = re.compile(r"^NDI\s+cam\d+$", re.I)

results = []


def emit(verdict: str, node: str, detail: str) -> None:
    results.append(verdict)
    print(f"[{verdict}] {node:<7} {detail}", flush=True)


def ssh(host: str, cmd: str, user: str = "root", timeout: int = 20, pw: str = SSH_PW) -> str | None:
    try:
        out = subprocess.run(
            ["sshpass", "-p", pw, "ssh", "-o", "StrictHostKeyChecking=no",
             "-o", "ConnectTimeout=6", f"{user}@{host}", cmd],
            capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return None
    # decode defensively: Windows OBS logs carry raw cp1252/mojibake bytes (the audit lines'
    # decorative fragments) that are not valid UTF-8
    return out.stdout.decode("utf-8", errors="replace") if out.returncode == 0 else None


def obs_ws_stats(host: str, password: str | None) -> dict | None:
    """GetStats over obs-websocket 5.x. Returns responseData or None."""
    try:
        from websocket import create_connection
        ws = create_connection(f"ws://{host}:4455", timeout=8)
        hello = json.loads(ws.recv())["d"]
        ident = {"op": 1, "d": {"rpcVersion": 1}}
        if "authentication" in hello:
            if not password:
                ws.close()
                return None
            auth = hello["authentication"]
            secret = base64.b64encode(
                hashlib.sha256((password + auth["salt"]).encode()).digest()).decode()
            ident["d"]["authentication"] = base64.b64encode(
                hashlib.sha256((secret + auth["challenge"]).encode()).digest()).decode()
        ws.send(json.dumps(ident))
        json.loads(ws.recv())
        ws.send(json.dumps({"op": 6, "d": {"requestType": "GetStats", "requestId": "1",
                                           "requestData": {}}}))
        while True:
            msg = json.loads(ws.recv())
            if msg["op"] == 7:
                ws.close()
                return msg["d"].get("responseData", {})
    except Exception as e:
        print(f"    (ws {host}: {type(e).__name__}: {e})", file=sys.stderr)
        return None


def arrival_rates(log_text: str) -> dict[str, float]:
    """Per-source NDI arrival fps from the last two genlock-fifo audit lines of each source."""
    samples: dict[str, list[tuple[float, int]]] = {}
    for line in log_text.splitlines():
        m = AUDIT_RE.match(line)
        if not m:
            continue
        t = int(m.group(1)) * 3600 + int(m.group(2)) * 60 + int(m.group(3)) + int(m.group(4)) / 1000
        samples.setdefault(m.group(5), []).append((t, int(m.group(6))))
    rates = {}
    for src, pts in samples.items():
        if len(pts) >= 2:
            (t0, r0), (t1, r1) = pts[-2], pts[-1]
            if t1 > t0:
                rates[src] = (r1 - r0) / (t1 - t0)
    return rates


def fmt_rates(rates: dict[str, float]) -> str:
    return ",".join(f"{src.replace('NDI ', '')}={fps:.0f}" for src, fps in sorted(rates.items()))


def audit_samples(log_text: str) -> dict[str, list[tuple[str, int]]]:
    """Per-source (raw_ts, received) samples from EVERY genlock-fifo audit line, in file
    (chronological) order. raw_ts is the 'HH:MM:SS.mmm' clock prefix, passed VERBATIM to the
    cadence-health.sh kernel -- no seconds math here (the kernel's cadence_ts_to_seconds owns it)."""
    out: dict[str, list[tuple[str, int]]] = {}
    for line in log_text.splitlines():
        m = AUDIT_RE.match(line)
        if not m:
            continue
        raw_ts = f"{m.group(1)}:{m.group(2)}:{m.group(3)}.{m.group(4)}"
        out.setdefault(m.group(5), []).append((raw_ts, int(m.group(6))))
    return out


def cadence_verdict(prev_ts: str, prev_recv: int, curr_ts: str, curr_recv: int,
                    expected: int = 60, tol: int = 3,
                    min_window: int = 60) -> tuple[str, str]:
    """REUSE the tested bash kernel scripts/lib/cadence-health.sh -- no second divisor lives here.
    cadence_measure_fps derives the delivered fps from the two samples' OWN timestamps (the #797
    phantom-50 avoidance -- NEVER a wall-clock divisor); cadence_classify grades it against the
    expected +/- tol band. Returns (verdict, fps_str), verdict in OK|WRONG|UNKNOWN|SKIP; any failure
    to run the kernel degrades to ('UNKNOWN', '') -- never a false page."""
    script = (
        'set -eu\n'
        '. "$1"\n'
        'm=$(cadence_measure_fps "$2" "$3" "$4" "$5")\n'
        'fps=; win=; adv=\n'
        'for tok in $m; do case "$tok" in\n'
        '  fps=*) fps=${tok#fps=} ;; window_s=*) win=${tok#window_s=} ;;'
        ' advanced=*) adv=${tok#advanced=} ;;\n'
        'esac; done\n'
        'v=$(cadence_classify "$fps" "$win" "$adv" "$6" "$7" "$8" 1 1)\n'
        'printf "%s %s\\n" "$v" "$fps"\n'
    )
    try:
        out = subprocess.run(
            ["bash", "-c", script, "bash", CADENCE_LIB,
             str(prev_ts), str(prev_recv), str(curr_ts), str(curr_recv),
             str(expected), str(tol), str(min_window)],
            capture_output=True, text=True, timeout=10)
    except (subprocess.TimeoutExpired, OSError):
        return "UNKNOWN", ""
    if out.returncode != 0:
        return "UNKNOWN", ""
    parts = out.stdout.split()
    return (parts[0] if parts else "UNKNOWN"), (parts[1] if len(parts) > 1 else "")


def cadence_check(log_text: str, expected: int = 60, tol: int = 3,
                  min_window: int = 60) -> tuple[dict[str, str], list[str]]:
    """Grade the delivered cadence of every CAMERA source in the strih OBS log against
    `expected` +/- `tol` fps, via the cadence-health.sh kernel (per source: its FIRST + LAST audit
    sample = the widest trustable window; the kernel's own guards map a > CADENCE_MAX_WINDOW_S span,
    a counter reset, a < min_window span, or a frozen source to UNKNOWN -- never a false page).
    Returns (display, problems): `display` maps each OK/WRONG camera to its rounded fps; `problems`
    carries a `warn:cadence <cam>=<fps>fps(!=<expected>)` SOFT entry per source measuring a sustained
    non-60 cadence (WRONG). Non-camera sources (2ME PGM/PVW, 30 fps by design) are never graded;
    UNKNOWN/SKIP never produce a row or a problem."""
    display: dict[str, str] = {}
    problems: list[str] = []
    for src, pts in sorted(audit_samples(log_text).items()):
        if not CAMERA_SRC_RE.match(src) or not pts:
            continue
        (p_ts, p_rc), (c_ts, c_rc) = pts[0], pts[-1]
        verdict, fps = cadence_verdict(p_ts, p_rc, c_ts, c_rc, expected, tol, min_window)
        if verdict not in ("OK", "WRONG"):
            continue
        label = src.replace("NDI ", "")
        shown = f"{float(fps):.0f}" if fps else "?"
        display[label] = shown
        if verdict == "WRONG":
            problems.append(f"warn:cadence {label}={shown}fps(!={expected})")
    return display, problems


def box_verdict(problems: list[str]) -> str:
    """PASS when clean, WARN when only SOFT ('warn:'-prefixed) problems remain, FAIL on any HARD
    problem. Shared by check_cam / check_imag / check_windows_box so the three-tier split lives once."""
    hard = [p for p in problems if not p.startswith("warn:")]
    if not problems:
        return "PASS"
    return "WARN" if not hard else "FAIL"


def parse_ntp_status(json_text: str | None) -> tuple[int | None, bool | None]:
    """Pull (ntp_steps_last_hour, ntp_step_storm) from a dantesync :8898 status JSON body (the
    Windows-node signal). Both fields are ADDITIVE (dantesync >= 1.8.45); a body that LACKS them
    (older version -- the live reality on strih/stream today) OR an unparseable/empty/wrong-shape
    body yields (None, None) -- UNKNOWN, never a false 0 and never a false alarm (issue-833 missing-
    tool trap + the additive-JSON tolerance)."""
    try:
        d = json.loads(json_text or "")
    except (ValueError, TypeError):
        return None, None
    if not isinstance(d, dict):
        return None, None
    raw_steps = d.get("ntp_steps_last_hour")
    raw_storm = d.get("ntp_step_storm")
    # a JSON bool is an int in Python -- reject it as a count (True must not read as 1 step).
    steps = int(raw_steps) if isinstance(raw_steps, (int, float)) and not isinstance(raw_steps, bool) else None
    storm = bool(raw_storm) if isinstance(raw_storm, bool) else None
    return steps, storm


def grade_ntp_steprate(steps: int | None, storm: bool | None = None) -> tuple[str, str, list[str]]:
    """Grade a node's dantesync NTP step-rate for the issue-787 status page (issue-1108 observability).
    Returns (verdict, display, problems), verdict in OK|WARN|FAIL|UNKNOWN:
      * steps >= NTP_STEP_STORM_BOUND -> FAIL, mirroring dantesync's own 120/h step-storm boundary
        (issue-1108 storm floor 129/h); the problem cites the count that crossed the bound.
      * an explicit storm flag (Windows :8898 >= 1.8.45) with a count UNDER the bound -> FAIL too, but
        the problem names the dantesync FLAG as the authoritative trigger (never a misleading
        `(>=120/h)` on a sub-120 count -- issue-1108 review).
      * NTP_STEP_HEALTHY_CEIL < steps < NTP_STEP_STORM_BOUND -> WARN (elevated, not yet a storm).
      * steps <= NTP_STEP_HEALTHY_CEIL -> OK (baseline ~30-36/h, healthy ceiling ~72/h).
      * steps is None (unreadable journal / absent additive field) -> UNKNOWN, surfaced BY NAME
        (`n/a`), never a false 0 or a false alarm.
    The returned VERDICT is the per-FACET classification -- distinct from box_verdict()'s per-NODE
    aggregate (which folds ALL of a node's problems). The production wiring drives the node color off
    `problems` via box_verdict, but the facet verdict is the tested contract of the grading itself and
    the natural signal for a future dedicated step-rate column / dev1 alert-watchdog (mirroring how
    cadence_verdict returns a verdict cadence_check consumes). `problems` uses the feeder's soft/hard
    convention -- a `warn:`-prefixed entry folds to WARN, a bare entry to FAIL."""
    if steps is not None and steps >= NTP_STEP_STORM_BOUND:
        return "FAIL", f"{steps}/h", [f"ntp-step-storm={steps}/h(>={NTP_STEP_STORM_BOUND}/h)"]
    if storm is True:
        shown = f"{steps}/h" if steps is not None else "flag"
        return "FAIL", shown, ["ntp-step-storm=dantesync-flag"]
    if steps is None:
        return "UNKNOWN", "n/a", []
    if steps > NTP_STEP_HEALTHY_CEIL:
        return "WARN", f"{steps}/h", [f"warn:ntp-step-rate={steps}/h(>{NTP_STEP_HEALTHY_CEIL})"]
    return "OK", f"{steps}/h", []


def http_get(url: str, timeout: int = 6) -> str | None:
    """Read-only HTTP GET FROM dev1 (the audit's box), used for the Windows dantesync :8898 status
    JSON. Returns the body text, or None on ANY error (unreachable / timeout / non-200) -- the caller
    treats None as UNKNOWN, never a crash."""
    import urllib.request
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            return r.read().decode("utf-8", errors="replace")
    except Exception as e:
        print(f"    (http {url}: {type(e).__name__}: {e})", file=sys.stderr)
        return None


def check_cam(name: str, ip: str) -> None:
    out = ssh(ip, "systemctl is-active camera-box; "
                  "journalctl -u camera-box -n 120 --no-pager | grep -E 'Streaming:|capture chroma' | tail -4; "
                  "journalctl -u dantesync -n 40 --no-pager | grep -oE 'offset:[+-][0-9]+us' | tail -1; "
                  "awk '$2==\"/\"{print $4}' /proc/mounts | cut -d, -f1; "
                  "journalctl -u dantesync --since '-1 hour' --no-pager 2>/dev/null | awk '" + NTP_STEP_COUNT_AWK + "'; "
                  "cut -d' ' -f1 /proc/loadavg")
    if out is None:
        emit("FAIL", name, "unreachable over ssh")
        return
    lines = out.splitlines()
    svc = lines[0].strip() if lines else "?"
    stream_m = re.search(r"Streaming: ([\d.]+) fps emitted / ([\d.]+) fps captured \((\d+) sent, (\d+) captured, (\d+) capture-dropped", out)
    chroma_m = re.findall(r"-> (colour|grayscale)", out)
    off_m = re.search(r"offset:([+-]\d+)us", out)
    ro = "ro" if re.search(r"^ro$", out, re.M) else "rw!"
    load = lines[-1].strip() if lines else "?"
    problems = []
    if svc != "active":
        problems.append(f"svc={svc}")
    fps_s = "?"
    if stream_m:
        emitted, captured, dropped = float(stream_m.group(1)), float(stream_m.group(2)), int(stream_m.group(5))
        fps_s = f"{emitted:.1f}/{captured:.1f}"
        if emitted < 59.0 or captured < 59.0:
            problems.append("fps-low")
        if dropped >= 5:
            problems.append(f"capture-dropped={dropped}")
        elif dropped > 0:
            problems.append(f"warn:capture-dropped={dropped}")
    else:
        problems.append("no-streaming-report")
    chroma = chroma_m[-1] if chroma_m else "?"
    off_us = int(off_m.group(1)) if off_m else None
    if off_us is None or abs(off_us) > DANTE_BOUND_US:
        problems.append(f"dante={off_us}us")
    if ro != "ro":
        problems.append("root=rw")
    # issue-1108 dantesync NTP step-rate facet. Same-journal proxy for a read failure: an unreadable
    # dantesync journal also nulls the offset above, so off_us is None -> UNKNOWN, never a false 0.
    steps_m = re.search(r"ntp_steps_1h=(\d+)", out)
    ntp_steps = int(steps_m.group(1)) if (steps_m and off_us is not None) else None
    _, steprate_disp, steprate_problems = grade_ntp_steprate(ntp_steps)
    problems += steprate_problems
    verdict = box_verdict(problems)
    detail = (f"svc={svc} fps={fps_s} chroma={chroma} dante={off_us:+d}us root={ro} load={load} steprate={steprate_disp}"
              if off_us is not None else f"svc={svc} fps={fps_s} chroma={chroma} dante=? root={ro} load={load} steprate={steprate_disp}")
    if problems:
        detail += "  <<" + " ".join(problems) + ">>"
    emit(verdict, name, detail)


def check_imag() -> None:
    out = ssh(IMAG, "pgrep -x obs >/dev/null && echo obs=up || echo obs=DOWN; "
                    "systemctl is-active imag-obs-watchdog 2>/dev/null; "
                    "grep -o isolcpus /proc/cmdline || echo cmdline-clean; "
                    "journalctl -u dantesync -n 40 --no-pager | grep -oE 'offset:[+-][0-9]+us' | tail -1; "
                    "cut -d' ' -f1 /proc/loadavg; "
                    "journalctl -u dantesync --since '-1 hour' --no-pager 2>/dev/null | awk '" + NTP_STEP_COUNT_AWK + "'; "
                    "tail -400 \"$(ls -t ~/.config/obs-studio/logs/*.txt | head -1)\" | grep 'genlock-fifo audit'",
              user="newlevel", timeout=25)
    if out is None:
        emit("FAIL", "imag", "unreachable over ssh")
        return
    stats = obs_ws_stats(IMAG, None)
    rates = arrival_rates(out)
    problems = []
    if "obs=up" not in out:
        problems.append("obs-down")
    if "isolcpus" in out.replace("cmdline-clean", ""):
        problems.append("ISOLCPUS-IN-CMDLINE(#784)")
    off_m = re.search(r"offset:([+-]\d+)us", out)
    off_us = int(off_m.group(1)) if off_m else None
    if off_us is None or abs(off_us) > DANTE_BOUND_US:
        problems.append(f"dante={off_us}us")
    render = "ws-unreachable"
    if stats:
        fps = stats.get("activeFps", 0.0)
        rt = stats.get("averageFrameRenderTime", 99.0)
        skipped = stats.get("renderSkippedFrames", 0)
        total = max(stats.get("renderTotalFrames", 1), 1)
        render = f"{fps:.1f}fps/{rt:.1f}ms skip={100 * skipped / total:.2f}%"
        if fps < 59.5:
            problems.append("render-fps-low")
        if rt > 12.0:
            problems.append("render-time-high")
        if 100 * skipped / total > 1.0:
            problems.append("render-skips")
    else:
        problems.append("ws-stats-missing")
    cam_rates = {s: r for s, r in rates.items() if re.match(r"NDI CAM\d", s)}
    low = [s for s, r in cam_rates.items() if r < 58.0]
    if low:
        problems.append("arrivals-low:" + ",".join(low))
    if len(cam_rates) < 7:
        problems.append(f"cam-arrivals-seen={len(cam_rates)}/7")
    # issue-1108 dantesync NTP step-rate facet (same-journal off_us proxy for a read failure).
    steps_m = re.search(r"ntp_steps_1h=(\d+)", out)
    ntp_steps = int(steps_m.group(1)) if (steps_m and off_us is not None) else None
    _, steprate_disp, steprate_problems = grade_ntp_steprate(ntp_steps)
    problems += steprate_problems
    verdict = box_verdict(problems)
    detail = (f"render={render} arrivals[{fmt_rates(rates)}] isolcpus=none dante={off_us:+d}us steprate={steprate_disp}"
              if off_us is not None else f"render={render} arrivals[{fmt_rates(rates)}] steprate={steprate_disp}")
    if problems:
        detail += "  <<" + " ".join(problems) + ">>"
    emit(verdict, "imag", detail)


def _ps_encoded(ps: str) -> str:
    # #1259: base64 UTF-16LE encode a PowerShell command for `-EncodedCommand`. strih/stream run
    # Win32-OpenSSH whose default shell is cmd.exe; a naive `-Command "...| Sort-Object ..."` leaks
    # its `|` pipes (and `$`/`;`/`()`) to cmd.exe BEFORE PowerShell parses them -> a mangled/blind
    # read (the issue-1258 root cause). The base64 blob is pure ASCII with no shell-special char, so
    # cmd.exe cannot touch it. Python cannot source scripts/lib/ps-encoded.sh, so this mirrors it
    # (same base64 UTF-16LE encoding as ps_encoded_command / win_ssh_ps_encoded_command).
    return base64.b64encode(ps.encode("utf-16-le")).decode("ascii")


def _windows_obs_log_tail_cmd(tail: int = 500) -> str:
    # numeric-only tail -> a caller value can never inject shell/PS metachars into the encoded payload.
    # Reject negatives too (a `-Tail -5` is an invalid PowerShell arg), matching the bash `*[!0-9]*`
    # ps_clamp_numeric guard (#1259 review).
    try:
        n = int(tail)
    except (TypeError, ValueError):
        n = 500
    if n < 0:
        n = 500
    # HEAD first (the launch-time audio-buffering burst lives in the first ~200 lines -- a
    # tail-only read would report a false-clean audio_buf=0 on a long session), then the tail
    # (the live genlock-fifo audit lines).
    ps = ("$l = Get-ChildItem $env:APPDATA\\obs-studio\\logs\\*.txt | "
          "Sort-Object LastWriteTime -Descending | Select-Object -First 1; "
          "Get-Content $l.FullName -TotalCount 600; "
          f"Get-Content $l.FullName -Tail {n}")
    return f"powershell -NoProfile -NonInteractive -EncodedCommand {_ps_encoded(ps)}"


def _windows_obs_count_cmd() -> str:
    # #1259: -EncodedCommand (cmd.exe-proof) -- the `()` grouping + nested quotes in the naive form
    # are cmd.exe metacharacters (see _ps_encoded).
    return "powershell -NoProfile -NonInteractive -EncodedCommand " + _ps_encoded(
        "(Get-Process obs64 -ErrorAction SilentlyContinue).Count")


def windows_obs_log_tail(ip: str, tail: int = 500) -> str | None:
    return ssh(ip, _windows_obs_log_tail_cmd(tail), user="newlevel", timeout=30)


def check_windows_box(name: str, ip: str, ws_password: str | None, program_fps: float,
                      expect_latency: bool, check_camera_cadence: bool = False) -> None:
    procs = ssh(ip, _windows_obs_count_cmd(), user="newlevel", timeout=20)
    if procs is None:
        emit("FAIL", name, "unreachable over ssh")
        return
    obs_count = int(procs.strip() or 0)
    log = windows_obs_log_tail(ip) or ""
    stats = obs_ws_stats(ip, ws_password)
    rates = arrival_rates(log)
    buf_peak = max((int(m.group(1)) for m in BUF_RE.finditer(log)), default=0)
    buf_maxed = "Max audio buffering reached" in log
    problems = []
    if obs_count != 1:
        problems.append(f"obs64x{obs_count}")
    render = "ws-unreachable"
    if stats:
        fps = stats.get("activeFps", 0.0)
        rt = stats.get("averageFrameRenderTime", 99.0)
        skipped = stats.get("renderSkippedFrames", 0)
        total = max(stats.get("renderTotalFrames", 1), 1)
        render = f"{fps:.1f}fps/{rt:.1f}ms skip={100 * skipped / total:.2f}%"
        if fps < program_fps - 0.5:
            problems.append("render-fps-low")
        if rt > (1000.0 / program_fps) - 3.0:
            problems.append("render-time-high")
        if 100 * skipped / total > 1.0:
            problems.append("render-skips")
    else:
        problems.append("ws-stats-missing")
    if buf_peak > AUDIO_BUF_BOUND_MS or buf_maxed:
        problems.append(f"AUDIO-BUF={buf_peak}ms(#786)")
    low = [s for s, r in rates.items() if r < 28.0]  # every ingest must at least sustain program rate
    if low:
        problems.append("arrivals-low:" + ",".join(low))
    lat = ""
    if expect_latency:
        # the operator's A/V-align latency lives on 'NDI 2ME PGM' specifically -- a bare
        # last-match would report another source's 3 ms default (live miss, 2026-07-16)
        lm = re.findall(r"audit 'NDI 2ME PGM'.*?latency_ms=(\d+)", log)
        lat = f" pgm_latency_ms={lm[-1]}" if lm else " pgm_latency_ms=?"
    # #1089: surface the per-camera DELIVERED cadence tier -- strih receives the raw camera NDI at
    # its native rate, so a source mis-set to 50/43 fps (duplication-masked to a clean 60 in every
    # canvas-fps counter) shows here as a WRONG-cadence WARN. stream sees only 2ME PGM (30 fps) +
    # interkom, so it never runs this check.
    cad = ""
    if check_camera_cadence:
        cad_display, cad_problems = cadence_check(log)
        problems += cad_problems
        cad = " cadence[" + ",".join(f"{s}={v}" for s, v in sorted(cad_display.items())) + "]"
    # issue-1108 dantesync NTP step-rate facet -- read the ADDITIVE ntp_steps_last_hour/ntp_step_storm
    # from the box's dantesync :8898 status JSON (dev1-side HTTP read). Absent fields (dantesync <
    # 1.8.45, the live reality on strih/stream today) or an unreachable port -> UNKNOWN (`n/a`), never
    # a false alarm.
    w_steps, w_storm = parse_ntp_status(http_get(f"http://{ip}:8898/"))
    _, steprate_disp, steprate_problems = grade_ntp_steprate(w_steps, w_storm)
    problems += steprate_problems
    verdict = box_verdict(problems)
    detail = f"obs64={obs_count} render={render} audio_buf={buf_peak}ms arrivals[{fmt_rates(rates)}]{cad} steprate={steprate_disp}{lat}"
    if problems:
        detail += "  <<" + " ".join(problems) + ">>"
    emit(verdict, name, detail)


def main() -> int:
    pw_file = os.path.expanduser("~/.config/camera-box/obs-ws-pass")
    strih_pw = open(pw_file).read().strip() if os.path.exists(pw_file) else None
    for name, ip in CAMS.items():
        check_cam(name, ip)
    check_imag()
    check_windows_box("strih", STRIH, strih_pw, program_fps=30.0, expect_latency=False,
                      check_camera_cadence=True)
    check_windows_box("stream", STREAM, strih_pw, program_fps=30.0, expect_latency=True)
    fails = results.count("FAIL")
    warns = results.count("WARN")
    print(f"\n=== RIG AUDIT: {results.count('PASS')} PASS / {warns} WARN / {fails} FAIL "
          f"({'ALL NODES HEALTHY' if not fails and not warns else 'PROBLEMS ABOVE'}) ===")
    return 2 if fails else (1 if warns else 0)


if __name__ == "__main__":
    sys.exit(main())
