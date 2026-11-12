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

EXIT CODE = the box-reachable signal: 0 iff the WS connected and identified; non-zero on a
connect/handshake failure (-> SKIP, the network-reach watchdog's page, never ours).

It sends only GetCurrentProgramScene / GetStreamStatus / GetStats through obs_phase2's own client
(`_conn` / `_rpc`), so it never selects a scene: the issue-1380 production-scene guard stays the one
choke point and is never bypassed. No recording, no disk, no rig mutation.
"""
import argparse
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import obs_phase2  # noqa: E402  (the sibling WS client; websocket-client is its only dependency)

DEFAULT_PORT = 4455
DEFAULT_WINDOW_S = float(os.environ.get("STREAM_OUTPUT_STUCK_WINDOW_S", "3.0"))
# 7 requests x 3 s + the window + the 10 s obs_phase2 connect stay inside the watchdog's 45 s bound.
REQUEST_TIMEOUT_S = float(os.environ.get("STREAM_OUTPUT_STUCK_REQUEST_TIMEOUT_S", "3"))
_WS_PASSWORD = os.environ.get("STREAM_OUTPUT_STUCK_WS_PASSWORD", "")


def _request(ws, rtype):
    """One read request -> its responseData dict, or None on any failure (logged, never raised)."""
    try:
        return obs_phase2._rpc(ws, rtype, timeout_s=REQUEST_TIMEOUT_S) or {}
    except Exception as e:  # noqa: BLE001 -- a failed read is an unread fact, never a crash
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


def format_lines(scenes, s0, s1, window_s):
    """Pure: the printed key=value lines (the program scene before + after, the two snapshots, the
    window)."""
    lines = [f"scene0={scenes[0]}", f"scene1={scenes[1]}"]
    for i, s in ((0, s0), (1, s1)):
        for k in ("active", "render", "frames", "bytes", "reconnecting"):
            lines.append(f"{k}{i}={s.get(k, '')}")
    lines.append(f"window_s={window_s}")
    return lines


def _main(argv):
    ap = argparse.ArgumentParser(description="stream OBS stuck-output probe (issue 1460), read-only")
    ap.add_argument("host")
    ap.add_argument("port", nargs="?", type=int, default=DEFAULT_PORT)
    ap.add_argument("--window-s", type=float, default=DEFAULT_WINDOW_S)
    ns = ap.parse_args(argv)
    try:
        ws = obs_phase2._conn(ns.host, _WS_PASSWORD, ns.port)
    except Exception as e:  # noqa: BLE001 -- any connect/handshake failure = box unreachable = SKIP
        print(f"stream-output-stuck probe: WS connect to {ns.host}:{ns.port} failed: {e}", file=sys.stderr)
        return 1
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
    print("\n".join(format_lines((scene0, scene1), s0, s1, ns.window_s)))
    return 0


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
