#!/usr/bin/env python3
"""#799 — dev1-side imag OBS render-stats reader for the render-degradation CAUSE discriminator.

Snapshots imag OBS WS `GetStats` twice over a short delta window and emits ONE
`RENDER|<active_fps>|<avg_ms>|<render_skipped_frac>|<render_advanced>` line on stdout — the render
half of `imag_power_envelope_alert_watchdog`'s #799 discriminator (`imag_render_cause_from_signals`
in scripts/lib/imag-power-envelope.sh reads it). The GPU half (throttle burst) is read separately
over ssh; fusing "render degraded" with "GPU throttle clean vs clamped" names churn-leak (#799) vs
power-clamp (issue 880/1043).

Mirrors scripts/obs-liveness-probe.py::_render_sample (the render-signal source of truth, #391/#935):
`render_advanced` = did renderTotalFrames advance at all over the window — because `activeFps` LIES
during a full stall (it keeps reading the configured canvas fps). true = advancing, false = a full
stall, unknown = counter reset (OBS restarted between snapshots) / not computable.

FAIL-SAFE: any failure (WS unreachable, missing dep, auth) prints a LOUD diagnostic to stderr and
emits NOTHING on stdout + exits 0, so the watchdog reads an empty RENDER line -> the discriminator
returns `unknown` -> no false alert. Never crashes the watchdog pass.

#399 lazy-import discipline: `from websocket import ...` is deferred into the connect path only, so
the pure `render_line` helper (and this module) import cleanly on a host with no websocket-client
(the Rust test-job runner). Do NOT hoist it to module scope.

Usage:
  python3 scripts/imag-render-stats.py --host 10.77.9.182 [--port 4455] [--window-s 4] [--target-fps 60]
Env:
  OBS_PASSWORD_IMAG (falls back to OBS_PASSWORD) — the imag OBS WS password.
  OBS_OP_TIMEOUT_S  — per-request OBS WS timeout in seconds (default 30).
"""
import argparse
import base64
import hashlib
import json
import os
import sys
import time

OBS_OP_TIMEOUT_S = float(os.environ.get("OBS_OP_TIMEOUT_S", "30"))


# ─── pure helper (unit-testable without OBS / websocket-client) ───────────────

def render_line(s0: dict, s1: dict, target_fps: float) -> str:
    """Build the `RENDER|<active_fps>|<avg_ms>|<render_skipped_frac>|<render_advanced>` line from two
    GetStats snapshots. activeFps + averageFrameRenderTime are instantaneous gauges (take from s1);
    renderSkipped/renderTotal are cumulative (take the delta over the window). render_advanced =
    (r_tot > 0) if r_tot >= 0 else "unknown" — a negative delta means OBS restarted between snapshots
    (counter reset), never a false "stalled". target_fps is accepted for parity with the sibling
    readers; the imag budget lives in the pure classifier, not here."""
    _ = target_fps
    r_tot = float(s1.get("renderTotalFrames", 0)) - float(s0.get("renderTotalFrames", 0))
    r_skip = float(s1.get("renderSkippedFrames", 0)) - float(s0.get("renderSkippedFrames", 0))
    frac = (r_skip / r_tot) if r_tot > 0 else 0.0
    if r_tot > 0:
        adv = "true"
    elif r_tot == 0:
        adv = "false"
    else:
        adv = "unknown"
    afps = float(s1.get("activeFps", 0.0))
    avg = float(s1.get("averageFrameRenderTime", 0.0))
    return "RENDER|%.2f|%.2f|%.4f|%s" % (afps, avg, frac, adv)


# ─── OBS WebSocket helpers (same auth shape as render-budget-gate.py) ─────────

def _conn(host, port, password=""):
    # #399: lazy import so the pure helper + this module load without websocket-client present.
    from websocket import create_connection
    ws = create_connection("ws://%s:%d" % (host, port), timeout=10)
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


def _get_stats(ws):
    from websocket import WebSocketTimeoutException
    ws.send(json.dumps({"op": 6, "d": {
        "requestType": "GetStats", "requestId": "GetStats", "requestData": {}}}))
    t0 = time.monotonic()
    while True:
        if OBS_OP_TIMEOUT_S > 0 and (time.monotonic() - t0) >= OBS_OP_TIMEOUT_S:
            raise TimeoutError("obs-websocket GetStats timed out")
        try:
            m = json.loads(ws.recv())
        except WebSocketTimeoutException:
            continue
        if m["op"] == 7 and m["d"]["requestId"] == "GetStats":
            st = m["d"]["requestStatus"]
            if not st["result"]:
                raise RuntimeError("GetStats failed: %s" % st)
            return m["d"].get("responseData") or {}


def main():
    ap = argparse.ArgumentParser(description="#799 imag OBS render-stats reader")
    ap.add_argument("--host", required=True, help="imag-nb IP/host (e.g. 10.77.9.182)")
    ap.add_argument("--port", type=int, default=4455)
    ap.add_argument("--window-s", type=float, default=4.0)
    ap.add_argument("--target-fps", type=float, default=60.0)
    args = ap.parse_args()

    password = os.environ.get("OBS_PASSWORD_IMAG", os.environ.get("OBS_PASSWORD", ""))

    # FAIL-SAFE: on ANY error, print a LOUD diagnostic to stderr and emit nothing on stdout (exit 0)
    # so the watchdog reads an empty RENDER line -> the discriminator returns `unknown` -> no false
    # alert. This is a deliberate, NON-silent catch (the diagnostic is printed), per
    # script-failure-policy.md — a measurement front must never crash the watchdog on an
    # unreachable/wedged box, exactly like obs-liveness-probe.py's ws_reachable:false.
    ws = None
    try:
        ws = _conn(args.host, args.port, password)
        s0 = _get_stats(ws)
        time.sleep(args.window_s)
        s1 = _get_stats(ws)
    except Exception as e:  # noqa: BLE001 — deliberate fail-safe, diagnostic printed below
        print("imag-render-stats: could not read imag OBS WS render stats (%s) — "
              "emitting no RENDER line (discriminator reads it as unknown)" % e, file=sys.stderr)
        return 0
    finally:
        if ws is not None:
            try:
                ws.close()
            except Exception as e:  # noqa: BLE001
                print("imag-render-stats: ws close warning: %s" % e, file=sys.stderr)

    print(render_line(s0, s1, args.target_fps))
    return 0


if __name__ == "__main__":
    sys.exit(main())
