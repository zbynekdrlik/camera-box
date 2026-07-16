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
AUDIT_RE = re.compile(r"^(\d+):(\d+):(\d+)\.(\d+): genlock-fifo audit '([^']+)': received=(\d+)")
BUF_RE = re.compile(r"total audio buffering is now (\d+) milliseconds")

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


def check_cam(name: str, ip: str) -> None:
    out = ssh(ip, "systemctl is-active camera-box; "
                  "journalctl -u camera-box -n 120 --no-pager | grep -E 'Streaming:|capture chroma' | tail -4; "
                  "journalctl -u dantesync -n 40 --no-pager | grep -oE 'offset:[+-][0-9]+us' | tail -1; "
                  "awk '$2==\"/\"{print $4}' /proc/mounts | cut -d, -f1; "
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
    hard = [x for x in problems if not x.startswith("warn:")]
    verdict = "PASS" if not problems else ("WARN" if not hard else "FAIL")
    detail = (f"svc={svc} fps={fps_s} chroma={chroma} dante={off_us:+d}us root={ro} load={load}"
              if off_us is not None else f"svc={svc} fps={fps_s} chroma={chroma} dante=? root={ro} load={load}")
    if problems:
        detail += "  <<" + " ".join(problems) + ">>"
    emit(verdict, name, detail)


def check_imag() -> None:
    out = ssh(IMAG, "pgrep -x obs >/dev/null && echo obs=up || echo obs=DOWN; "
                    "systemctl is-active imag-obs-watchdog 2>/dev/null; "
                    "grep -o isolcpus /proc/cmdline || echo cmdline-clean; "
                    "journalctl -u dantesync -n 40 --no-pager | grep -oE 'offset:[+-][0-9]+us' | tail -1; "
                    "cut -d' ' -f1 /proc/loadavg; "
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
    verdict = "PASS" if not problems else "FAIL"
    detail = f"render={render} arrivals[{fmt_rates(rates)}] isolcpus=none dante={off_us:+d}us" if off_us is not None else f"render={render} arrivals[{fmt_rates(rates)}]"
    if problems:
        detail += "  <<" + " ".join(problems) + ">>"
    emit(verdict, "imag", detail)


def windows_obs_log_tail(ip: str, tail: int = 500) -> str | None:
    # HEAD first (the launch-time audio-buffering burst lives in the first ~200 lines -- a
    # tail-only read would report a false-clean audio_buf=0 on a long session), then the tail
    # (the live genlock-fifo audit lines).
    cmd = ("powershell -NoProfile -Command \"$l = Get-ChildItem $env:APPDATA\\obs-studio\\logs\\*.txt | "
           "Sort-Object LastWriteTime -Descending | Select-Object -First 1; "
           "Get-Content $l.FullName -TotalCount 600; "
           f"Get-Content $l.FullName -Tail {tail}\"")
    return ssh(ip, cmd, user="newlevel", timeout=30)


def check_windows_box(name: str, ip: str, ws_password: str | None, program_fps: float,
                      expect_latency: bool) -> None:
    procs = ssh(ip, "powershell -NoProfile -Command \"(Get-Process obs64 -ErrorAction SilentlyContinue).Count\"",
                user="newlevel", timeout=20)
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
    verdict = "PASS" if not problems else "FAIL"
    detail = f"obs64={obs_count} render={render} audio_buf={buf_peak}ms arrivals[{fmt_rates(rates)}]{lat}"
    if problems:
        detail += "  <<" + " ".join(problems) + ">>"
    emit(verdict, name, detail)


def main() -> int:
    pw_file = os.path.expanduser("~/.config/camera-box/obs-ws-pass")
    strih_pw = open(pw_file).read().strip() if os.path.exists(pw_file) else None
    for name, ip in CAMS.items():
        check_cam(name, ip)
    check_imag()
    check_windows_box("strih", STRIH, strih_pw, program_fps=30.0, expect_latency=False)
    check_windows_box("stream", STREAM, strih_pw, program_fps=30.0, expect_latency=True)
    fails = results.count("FAIL")
    warns = results.count("WARN")
    print(f"\n=== RIG AUDIT: {results.count('PASS')} PASS / {warns} WARN / {fails} FAIL "
          f"({'ALL NODES HEALTHY' if not fails and not warns else 'PROBLEMS ABOVE'}) ===")
    return 2 if fails else (1 if warns else 0)


if __name__ == "__main__":
    sys.exit(main())
