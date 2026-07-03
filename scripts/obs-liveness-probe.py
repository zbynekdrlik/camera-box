#!/usr/bin/env python3
"""#391 — broadcast-OBS liveness/wedge probe (Python OBS-WS measurement front).

Measures OBS WS `GetStats` twice (a short delta window) on each broadcast box —
always reachable from dev1 with NO ssh/MCP needed — builds an
`obs_watchdog::ObsHealthSample`-shaped JSON per box, OPTIONALLY layers in
agent/MCP-only Windows process signals (obs64 count / `Responding` / CPU%,
passed in via `--process-state` since a plain dev1 systemd timer cannot read
them), and pipes the combined sample to the `obs-watchdog-gate` Rust binary for
the strict verdict (HEALTHY / WEDGED-RENDER-LAG / WS-DEAD / FPS-ZERO /
OBS-COUNT-WRONG). The decision lives ONLY in `camera_box::obs_watchdog::classify`
— this front just measures and pipes (mirrors `render-budget-gate.py`).

A box whose WS connection fails entirely (unreachable, wedged hard enough that
even the WS thread is gone) is reported as `ws_reachable: false` rather than
raising — the watchdog must still gate the OTHER box.

Usage (both boxes, WS-only pass — what a dev1 systemd timer runs; Topology v2, #459: strih is
now cut-to-stream only at 30fps, the 60fps IMAG role moved to imag-nb):
  python3 scripts/obs-liveness-probe.py \\
    --box strih=10.77.9.202:30 --box stream=10.77.9.204:30 --window-s 4

With agent/MCP-sampled process state layered in (repeatable, one per box):
  python3 scripts/obs-liveness-probe.py --box stream=10.77.9.204:30 \\
    --process-state stream=count:1,responding:false,cpu:168.0

Exit codes (propagated from the Rust binary):
  0  every box HEALTHY
  1  one or more boxes NOT healthy
  2  bad args / OBS WS setup failure / binary not found

Env:
  OBS_PASSWORD_<LABEL>  — OBS WS password for that box (e.g. OBS_PASSWORD_STRIH)
  OBS_WATCHDOG_GATE_BIN — path to the obs-watchdog-gate Rust binary
                          (default: probed from PROBE_BIN_DIR or target/release)
  OBS_OP_TIMEOUT_S      — per-request OBS WS timeout in seconds (default 15;
                          kept short — a watchdog pass must never itself hang)
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

# websocket-client is imported LAZILY (inside the WS helpers), NOT at module top: the
# pure helpers (_parse_box / _parse_process_state / _render_sample / _find_verdict_bin)
# must be importable + unit-testable WITHOUT the WS dependency — a top-level import here
# broke tests/harness_rig_ndi_mapping.rs's sibling on the Rust test job's runner (no
# websocket-client there), fixed in #399 (set-ndi-mapping.py). Only the actual OBS-WS
# connect needs it.
def _ws():
    try:
        from websocket import WebSocketTimeoutException, create_connection

        return WebSocketTimeoutException, create_connection
    except ImportError:
        sys.exit("missing dep: pip install websocket-client")


OBS_OP_TIMEOUT_S = float(os.environ.get("OBS_OP_TIMEOUT_S", "15"))


# ─── pure helpers (unit-testable without OBS / without websocket-client) ─────

def _parse_box(spec: str) -> "tuple[str, str, float]":
    """Parse `LABEL=HOST[:TARGET_FPS]` -> (label, host, target_fps)."""
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


def _parse_process_state(spec: str) -> "tuple[str, dict]":
    """Parse `LABEL=count:N,responding:true|false,cpu:N.N` (any subset of the three
    keys, any order) -> (label, {"obs64_count": int|None, "responding": bool|None,
    "cpu_percent": float|None}). Unknown keys are rejected loudly (never silently
    dropped) so a typo'd flag fails fast instead of shipping a no-op override."""
    if "=" not in spec:
        raise ValueError(f"--process-state must be LABEL=key:val,..., got {spec!r}")
    label, rest = spec.split("=", 1)
    label = label.strip()
    state = {"obs64_count": None, "responding": None, "cpu_percent": None}
    if not label:
        raise ValueError(f"--process-state must be LABEL=key:val,..., got {spec!r}")
    for tok in (t for t in rest.split(",") if t.strip()):
        if ":" not in tok:
            raise ValueError(f"--process-state entry must be key:val, got {tok!r}")
        key, val = tok.split(":", 1)
        key = key.strip()
        val = val.strip()
        if key == "count":
            state["obs64_count"] = int(val)
        elif key == "responding":
            if val.lower() not in ("true", "false"):
                raise ValueError(f"responding must be true/false, got {val!r}")
            state["responding"] = val.lower() == "true"
        elif key == "cpu":
            state["cpu_percent"] = float(val)
        else:
            raise ValueError(f"unknown --process-state key {key!r} (want count/responding/cpu)")
    return label, state


def _render_sample(s0: dict, s1: dict, target_fps: float) -> dict:
    """Build an obs_watchdog-shaped WS-only sample from two GetStats snapshots.

    activeFps + averageFrameRenderTime are instantaneous gauges -> take from s1.
    renderSkipped/renderTotal are cumulative -> take the delta over the window
    (same 'GetStats deltas' recipe as render-budget-gate.py's _render_sample)."""
    r_tot = float(s1.get("renderTotalFrames", 0)) - float(s0.get("renderTotalFrames", 0))
    r_skip = float(s1.get("renderSkippedFrames", 0)) - float(s0.get("renderSkippedFrames", 0))
    frac = (r_skip / r_tot) if r_tot > 0 else 0.0
    return {
        "ws_reachable": True,
        "active_fps": float(s1.get("activeFps", 0.0)),
        "avg_render_time_ms": float(s1.get("averageFrameRenderTime", 0.0)),
        "render_skipped_frac": frac,
        "target_fps": float(target_fps),
    }


def _unreachable_sample(target_fps: float) -> dict:
    """The sample for a box whose WS connection failed entirely this pass."""
    return {
        "ws_reachable": False,
        "active_fps": None,
        "avg_render_time_ms": None,
        "render_skipped_frac": None,
        "target_fps": float(target_fps),
    }


def _merge_process_state(sample: dict, state: "dict | None") -> dict:
    """Layer optional agent/MCP process-state fields onto a measured sample.
    Absent (None) is left OUT of the dict entirely so the Rust binary's `opt_*`
    JSON readers see a genuinely missing field, not an explicit null of no
    consequence either way — but explicit null is ALSO accepted, so pass
    whichever is set."""
    out = dict(sample)
    if state:
        for key in ("obs64_count", "responding", "cpu_percent"):
            if state.get(key) is not None:
                out[key] = state[key]
    return out


def _find_verdict_bin(explicit: "str | None") -> str:
    """Locate the obs-watchdog-gate Rust binary (same search order the E2E uses
    for frozen-camera-gate / render-budget-gate)."""
    if explicit:
        return explicit
    env_bin = os.environ.get("OBS_WATCHDOG_GATE_BIN", "")
    if env_bin and os.path.isfile(env_bin):
        return env_bin
    probe_dir = os.environ.get("PROBE_BIN_DIR", "")
    if probe_dir:
        candidate = os.path.join(probe_dir, "obs-watchdog-gate")
        if os.path.isfile(candidate):
            return candidate
    here = os.path.dirname(os.path.abspath(__file__))
    local = os.path.normpath(os.path.join(here, "..", "target", "release", "obs-watchdog-gate"))
    if os.path.isfile(local):
        return local
    sys.exit(
        "ERROR: obs-watchdog-gate binary not found. "
        "Pass --verdict-bin, set OBS_WATCHDOG_GATE_BIN, or set PROBE_BIN_DIR. "
        "To build: cargo build --release --bin obs-watchdog-gate  # airuleset:build-ok"
    )


# ─── OBS WebSocket helpers (same _conn/_rpc pattern as obs_phase2.py) ────────

def _conn(host, password=""):
    _, create_connection = _ws()
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


def _rpc(ws, rtype, rdata=None):
    ws_timeout_exc, _ = _ws()
    ws.send(json.dumps({"op": 6, "d": {
        "requestType": rtype, "requestId": rtype, "requestData": rdata or {}}}))
    t0 = time.monotonic()
    while True:
        if OBS_OP_TIMEOUT_S > 0 and (time.monotonic() - t0) >= OBS_OP_TIMEOUT_S:
            raise TimeoutError(f"obs-websocket request {rtype!r} timed out")
        try:
            m = json.loads(ws.recv())
        except ws_timeout_exc:
            continue
        if m["op"] == 7 and m["d"]["requestId"] == rtype:
            st = m["d"]["requestStatus"]
            if not st["result"]:
                raise RuntimeError(f"{rtype} failed: {st}")
            return m["d"].get("responseData") or {}


def _measure_box(label: str, host: str, target_fps: float, window_s: float) -> dict:
    """Connect, snapshot GetStats twice over window_s, return a WS-only sample.
    Never raises — a WS failure of ANY kind (connect, auth, RPC, timeout) becomes
    an `ws_reachable: false` sample so the watchdog still gates the OTHER box."""
    password = os.environ.get(f"OBS_PASSWORD_{label.upper()}", "")
    try:
        ws = _conn(host, password)
    except Exception as e:
        sys.stderr.write(f"  [{label}] WS unreachable: {e}\n")
        return _unreachable_sample(target_fps)
    try:
        s0 = _rpc(ws, "GetStats")
        time.sleep(window_s)
        s1 = _rpc(ws, "GetStats")
    except Exception as e:
        sys.stderr.write(f"  [{label}] GetStats failed: {e}\n")
        return _unreachable_sample(target_fps)
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
    ap = argparse.ArgumentParser(description="#391 broadcast-OBS liveness/wedge probe")
    ap.add_argument("--box", action="append", required=True,
                     help="LABEL=HOST[:TARGET_FPS] (repeatable)")
    ap.add_argument("--process-state", action="append", default=[],
                     help="LABEL=count:N,responding:true|false,cpu:N.N (repeatable, "
                          "agent/MCP-sampled Windows process signals)")
    ap.add_argument("--window-s", type=float, default=4.0)
    ap.add_argument("--verdict-bin", default=None)
    args = ap.parse_args()

    try:
        boxes = [_parse_box(b) for b in args.box]
        states = dict(_parse_process_state(p) for p in args.process_state)
    except ValueError as e:
        sys.exit(f"ERROR: {e}")

    verdict_bin = _find_verdict_bin(args.verdict_bin)

    samples = {}
    for label, host, target in boxes:
        measured = _measure_box(label, host, target, args.window_s)
        samples[label] = _merge_process_state(measured, states.get(label))

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
