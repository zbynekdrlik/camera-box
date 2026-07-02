#!/usr/bin/env python3
"""#405 / EPIC #406 — render-budget strict gate (Python OBS-WS harness).

Snapshots OBS WS `GetStats` twice (a delta window) on each broadcast box while
burns are ON and the Multiview is open — the exact state that choked strih
60→27fps on 2026-07-02 — computes the REAL render-loop health per box
(activeFps + averageFrameRenderTime + renderSkipped delta, NOT the encoder
outputFps which duplicates to target and stays green while render chokes), and
feeds it to the `render-budget-gate` Rust binary for the strict verdict.

The decision (frame-deadline budget) lives ONLY in the Rust binary
(`render_budget::classify`) — this front just measures and pipes, so there is a
single source of truth for the threshold.

Usage (both boxes in one call):
  python3 scripts/render-budget-gate.py \
    --box strih=10.77.9.202:60 --box stream=10.77.9.204:30 --window-s 6

  `--box LABEL=HOST[:TARGET_FPS]` (repeatable). TARGET_FPS defaults 60.

Exit codes (propagated from the Rust binary):
  0  PASS  — every box holds its render budget
  1  FAIL  — one or more boxes miss the frame-time budget / fps / render-skip
  2  ERROR — bad args / OBS WS failure / binary not found

Env:
  OBS_PASSWORD_<LABEL>  — OBS WS password for that box (e.g. OBS_PASSWORD_STRIH)
  RENDER_GATE_BIN       — path to the render-budget-gate Rust binary
                          (default: probed from PROBE_BIN_DIR or target/release)
  OBS_OP_TIMEOUT_S      — per-request OBS WS timeout in seconds (default 60)
"""
import argparse
import base64
import hashlib
import json
import os
import subprocess
import sys
import time

PORT = 4455

try:
    from websocket import WebSocketTimeoutException, create_connection
except ImportError:
    sys.exit("missing dep: pip install websocket-client")

OBS_OP_TIMEOUT_S = float(os.environ.get("OBS_OP_TIMEOUT_S", "60"))


# ─── pure helpers (unit-testable without OBS) ────────────────────────────────

def _render_sample(s0: dict, s1: dict, target_fps: float) -> dict:
    """Build a render-budget sample from two GetStats snapshots.

    activeFps + averageFrameRenderTime are instantaneous gauges → take from s1.
    renderSkipped/renderTotal are cumulative → take the delta over the window so
    a long-running box isn't judged on lifetime counters (matches the obs-ops
    'GetStats deltas' measurement recipe)."""
    r_tot = float(s1.get("renderTotalFrames", 0)) - float(s0.get("renderTotalFrames", 0))
    r_skip = float(s1.get("renderSkippedFrames", 0)) - float(s0.get("renderSkippedFrames", 0))
    frac = (r_skip / r_tot) if r_tot > 0 else 0.0
    return {
        "active_fps": float(s1.get("activeFps", 0.0)),
        "avg_render_time_ms": float(s1.get("averageFrameRenderTime", 0.0)),
        "render_skipped_frac": frac,
        "target_fps": float(target_fps),
    }


def _parse_box(spec: str) -> "tuple[str, str, float]":
    """Parse `LABEL=HOST[:TARGET_FPS]` → (label, host, target_fps)."""
    if "=" not in spec:
        raise ValueError(f"--box must be LABEL=HOST[:TARGET_FPS], got {spec!r}")
    label, rest = spec.split("=", 1)
    label = label.strip()
    target = 60.0
    host = rest.strip()
    if ":" in rest:
        host, tgt = rest.rsplit(":", 1)
        host = host.strip()
        target = float(tgt)
    if not label or not host:
        raise ValueError(f"--box must be LABEL=HOST[:TARGET_FPS], got {spec!r}")
    return label, host, target


def _find_verdict_bin(explicit: "str | None") -> str:
    """Locate the render-budget-gate Rust binary (same search order the E2E uses
    for frozen-camera-gate)."""
    if explicit:
        return explicit
    env_bin = os.environ.get("RENDER_GATE_BIN", "")
    if env_bin and os.path.isfile(env_bin):
        return env_bin
    probe_dir = os.environ.get("PROBE_BIN_DIR", "")
    if probe_dir:
        candidate = os.path.join(probe_dir, "render-budget-gate")
        if os.path.isfile(candidate):
            return candidate
    here = os.path.dirname(os.path.abspath(__file__))
    local = os.path.normpath(os.path.join(here, "..", "target", "release", "render-budget-gate"))
    if os.path.isfile(local):
        return local
    sys.exit(
        "ERROR: render-budget-gate binary not found. "
        "Pass --verdict-bin, set RENDER_GATE_BIN, or set PROBE_BIN_DIR. "
        "To build: cargo build --release --bin render-budget-gate  # airuleset:build-ok"
    )


# ─── OBS WebSocket helpers (reuse _conn / _rpc from obs_phase2.py) ────────────

def _conn(host, password=""):
    ws = create_connection(f"ws://{host}:{PORT}", timeout=10)
    hello = json.loads(ws.recv())
    ident = {"op": 1, "d": {"rpcVersion": 1, "eventSubscriptions": 0}}
    auth = hello["d"].get("authentication")
    if auth:
        secret = base64.b64encode(
            hashlib.sha256((password + auth["salt"]).encode()).digest()
        ).decode()
        resp = base64.b64encode(
            hashlib.sha256((secret + auth["challenge"]).encode()).digest()
        ).decode()
        ident["d"]["authentication"] = resp
    ws.send(json.dumps(ident))
    json.loads(ws.recv())
    return ws


def _rpc(ws, rtype, rdata=None, ignore_err=False):
    ws.send(json.dumps({"op": 6, "d": {
        "requestType": rtype, "requestId": rtype, "requestData": rdata or {}}}))
    t0 = time.monotonic()
    while True:
        if OBS_OP_TIMEOUT_S > 0 and (time.monotonic() - t0) >= OBS_OP_TIMEOUT_S:
            raise TimeoutError(f"obs-websocket request {rtype!r} timed out")
        try:
            m = json.loads(ws.recv())
        except WebSocketTimeoutException:
            continue
        if m["op"] == 7 and m["d"]["requestId"] == rtype:
            st = m["d"]["requestStatus"]
            if not st["result"] and not ignore_err:
                raise RuntimeError(f"{rtype} failed: {st}")
            return m["d"].get("responseData") or {}


def _measure_box(label: str, host: str, target_fps: float, window_s: float) -> dict:
    """Connect, snapshot GetStats twice over window_s, return the render sample."""
    password = os.environ.get(f"OBS_PASSWORD_{label.upper()}", "")
    ws = _conn(host, password)
    try:
        s0 = _rpc(ws, "GetStats")
        time.sleep(window_s)
        s1 = _rpc(ws, "GetStats")
    finally:
        try:
            ws.close()
        except Exception:
            pass
    sample = _render_sample(s0, s1, target_fps)
    sys.stderr.write(
        f"  [{label}] activeFps={sample['active_fps']:.2f} "
        f"avgRenderTime={sample['avg_render_time_ms']:.2f}ms "
        f"renderSkip={sample['render_skipped_frac'] * 100:.2f}% "
        f"(target {target_fps:.0f}fps)\n"
    )
    return sample


def main():
    ap = argparse.ArgumentParser(description="#405 render-budget strict gate")
    ap.add_argument("--box", action="append", required=True,
                    help="LABEL=HOST[:TARGET_FPS] (repeatable)")
    ap.add_argument("--window-s", type=float, default=6.0)
    ap.add_argument("--verdict-bin", default=None)
    args = ap.parse_args()

    try:
        boxes = [_parse_box(b) for b in args.box]
    except ValueError as e:
        sys.exit(f"ERROR: {e}")

    verdict_bin = _find_verdict_bin(args.verdict_bin)

    samples = {}
    for label, host, target in boxes:
        try:
            samples[label] = _measure_box(label, host, target, args.window_s)
        except Exception as e:
            print(f"ERROR: measuring {label} @ {host}: {e}", file=sys.stderr)
            sys.exit(2)

    result = subprocess.run(
        [verdict_bin], input=json.dumps(samples).encode(), capture_output=True
    )
    out = result.stdout.decode().strip()
    if out:
        print(out)
    err = result.stderr.decode().strip()
    if err:
        print(err, file=sys.stderr)
    sys.exit(result.returncode)


if __name__ == "__main__":
    main()
