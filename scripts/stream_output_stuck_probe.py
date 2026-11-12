#!/usr/bin/env python3
"""issue 1460: the I/O half of the dev1 stuck-output page. READ-ONLY obs-websocket probe of the stream
OBS: the program scene, the stream output state and the render/output counters, sampled twice
`--window-s` apart inside ONE session, printed as `key=value` lines for the pure
scripts/stream_output_stuck_decision.py:

    scene0=PRO  scene1=PRO   (read before the first and after the second snapshot)
    active0=1  render0=...  frames0=...  bytes0=...  reconnecting0=0
    active1=1  render1=...  frames1=...  bytes1=...  reconnecting1=0
    window_s=3.0

A request that fails or times out leaves its keys EMPTY (the decision reads unknown, never a page).

EXIT CODE (issue 1460 review round 2: only a HUNG OBS may be carried as `unresponsive`, never an
ABSENT one -- an operator closing OBS or powering the box off after a show must never page):
  0 -- the WS connected and identified; the lines above are printed. `timeouts=<n>` counts the
       requests that got NO answer in time (a closed connection or an error reply is not a timeout).
  1 -- ABSENT: the TCP connect to the port was refused / had no route / timed out (OBS not running,
       box off). Never carried.
  2 -- HUNG: the TCP connect succeeded but the obs-websocket hello/identify never came in time.

It sends only GetCurrentProgramScene / GetStreamStatus / GetStats through obs_phase2's own client
(`_conn` / `_rpc`), so it never selects a scene: the issue-1380 production-scene guard stays the one
choke point and is never bypassed. No recording, no disk, no rig mutation.
"""
import argparse
import os
import socket
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import obs_phase2  # noqa: E402  (the sibling WS client; websocket-client is its only dependency)

DEFAULT_PORT = 4455
DEFAULT_WINDOW_S = float(os.environ.get("STREAM_OUTPUT_STUCK_WINDOW_S", "3.0"))
# The socket timeout is set to this right after the handshake, so every recv (and _rpc's own
# deadline) is bounded by it. A healthy pass takes ~3 s. The worst case does NOT fit the watchdog's
# 45 s bound: obs_phase2._conn's 10 s socket timeout applies per read (upgrade reply, hello,
# identified), so a silent handshake alone can last ~30 s; 5 + 30 + 7 x 3 + 3 = 59 s. A run killed by
# the bound exits 124, which the watchdog reads as HUNG -- the right verdict for exactly that case.
REQUEST_TIMEOUT_S = float(os.environ.get("STREAM_OUTPUT_STUCK_REQUEST_TIMEOUT_S", "3"))
TCP_CONNECT_TIMEOUT_S = float(os.environ.get("STREAM_OUTPUT_STUCK_TCP_TIMEOUT_S", "5"))
_TIMEOUTS = [0]
_WS_PASSWORD = os.environ.get("STREAM_OUTPUT_STUCK_WS_PASSWORD", "")


def is_timeout(exc):
    """Pure: True when `exc` means "no answer in time" (a hung peer), False for a refused / closed
    connection or an error reply (an absent or answering peer)."""
    return isinstance(exc, (TimeoutError, obs_phase2.WebSocketTimeoutException))


def _request(ws, rtype):
    """One read request -> its responseData dict, or None on any failure (logged, never raised). A
    timeout is also counted in _TIMEOUTS (the decision's `unresponsive` evidence)."""
    try:
        return obs_phase2._rpc(ws, rtype, timeout_s=REQUEST_TIMEOUT_S) or {}
    except Exception as e:  # noqa: BLE001 -- a failed read is an unread fact, never a crash
        if is_timeout(e):
            _TIMEOUTS[0] += 1
        print(f"stream-output-stuck probe: {rtype} failed: {e}", file=sys.stderr)
        return None


def _flag(d, key):
    if d is None or key not in d:
        return ""
    return "1" if d.get(key) else "0"


def _num(d, key):
    if d is None or key not in d:
        return ""
    try:
        return str(int(float(d.get(key))))
    except (ValueError, TypeError):
        return ""


def snapshot(ws):
    """-> dict of the stream output state + counters (strings, empty = unread)."""
    stream = _request(ws, "GetStreamStatus")
    stats = _request(ws, "GetStats")
    return {
        "active": _flag(stream, "outputActive"),
        "render": _num(stats, "renderTotalFrames"),
        "frames": _num(stream, "outputTotalFrames"),
        "bytes": _num(stream, "outputBytes"),
        "reconnecting": _flag(stream, "outputReconnecting"),
    }


def program_scene(ws):
    d = _request(ws, "GetCurrentProgramScene")
    if d is None:
        return ""
    return str(d.get("currentProgramSceneName") or d.get("sceneName") or "")


def format_lines(scenes, s0, s1, window_s, timeouts=0):
    """Pure: the printed key=value lines (the program scene before + after, the two snapshots, the
    window)."""
    lines = [f"scene0={scenes[0]}", f"scene1={scenes[1]}"]
    for i, s in ((0, s0), (1, s1)):
        for k in ("active", "render", "frames", "bytes", "reconnecting"):
            lines.append(f"{k}{i}={s.get(k, '')}")
    lines.append(f"window_s={window_s}")
    lines.append(f"timeouts={timeouts}")
    return lines


def _main(argv):
    ap = argparse.ArgumentParser(description="stream OBS stuck-output probe (issue 1460), read-only")
    ap.add_argument("host")
    ap.add_argument("port", nargs="?", type=int, default=DEFAULT_PORT)
    ap.add_argument("--window-s", type=float, default=DEFAULT_WINDOW_S)
    ns = ap.parse_args(argv)
    try:
        socket.create_connection((ns.host, ns.port), timeout=TCP_CONNECT_TIMEOUT_S).close()
    except OSError as e:  # refused / no route / TCP timeout = OBS not running or box off: ABSENT
        print(f"stream-output-stuck probe: TCP connect to {ns.host}:{ns.port} failed (absent): {e}",
              file=sys.stderr)
        return 1
    try:
        ws = obs_phase2._conn(ns.host, _WS_PASSWORD, ns.port)
    except Exception as e:  # noqa: BLE001 -- the port answered TCP; classify the handshake failure
        hung = is_timeout(e)
        print(f"stream-output-stuck probe: WS handshake with {ns.host}:{ns.port} failed "
              f"({'hung' if hung else 'absent'}): {e}", file=sys.stderr)
        return 2 if hung else 1
    ws.settimeout(REQUEST_TIMEOUT_S)
    try:
        scene0 = program_scene(ws)
        s0 = snapshot(ws)
        time.sleep(max(0.0, ns.window_s))
        s1 = snapshot(ws)
        scene1 = program_scene(ws)
    finally:
        try:
            ws.close()
        except OSError as e:
            print(f"stream-output-stuck probe: ws.close() failed (ignored): {e}", file=sys.stderr)
    print("\n".join(format_lines((scene0, scene1), s0, s1, ns.window_s, _TIMEOUTS[0])))
    return 0


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
