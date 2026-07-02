#!/usr/bin/env python3
"""#399 — enforce the strih OBS NDI-input→camera mapping (OBS-WS harness).

The strih NDI-input→camera-box bindings drift from the pins (the recurring bug: two inputs both on
CAM4, so a camera shows twice and another is missing). A pure hot WS rebind does NOT survive a
force-kill OBS relaunch (a distroav.dll swap reverts to the stale saved scene). So rig activation
(scripts/rig-mode.sh) must ENFORCE the correct 4-distinct mapping every time — set it + verify every
input is bound to a DISTINCT camera — instead of the operator/agent re-doing it by hand.

The mapping is Claude-owned + fixed (never a user question). The pins (offset per the rig-ndi-source
label convention): NDI cam5→CAM1, NDI cam1→CAM3, NDI cam3→CAM4, NDI cam2→CAM2. CAM3 is the down box —
its input binds correctly for when it returns; the other 3 are distinct live feeds.

Exit codes:
  0  PASS  — every input set to its pin AND all 4 senders distinct
  1  FAIL  — could not set an input, or a duplicate binding remains
  2  ERROR — OBS WS connection / request failure

Usage:
  python3 scripts/set-ndi-mapping.py --host 10.77.9.202 [--password PW]
  python3 scripts/set-ndi-mapping.py --host 10.77.9.202 --verify-only   # check, do not set
  python3 scripts/set-ndi-mapping.py --map "NDI cam5=CAM1 (usb)" ...     # override the pins
"""
import argparse
import base64
import hashlib
import json
import sys
import time

PORT = 4455

# #399 — the fixed 4-distinct strih NDI mapping (Claude-owned; never a user question).
DEFAULT_MAP = [
    ("NDI cam5", "CAM1 (usb)"),
    ("NDI cam1", "CAM3 (usb)"),
    ("NDI cam3", "CAM4 (usb)"),
    ("NDI cam2", "CAM2 (usb)"),
]

try:
    from websocket import WebSocketTimeoutException, create_connection
except ImportError:
    sys.exit("missing dep: pip install websocket-client")


# ─── pure helpers (unit-testable without OBS) ────────────────────────────────

def parse_map_args(items):
    """Parse repeated `--map "INPUT=SENDER"` into [(input, sender), ...]; default pins if none."""
    if not items:
        return list(DEFAULT_MAP)
    out = []
    for it in items:
        if "=" not in it:
            raise ValueError(f"--map must be INPUT=SENDER, got {it!r}")
        k, v = it.split("=", 1)
        out.append((k.strip(), v.strip()))
    return out


def duplicates(bindings):
    """Given {input: sender}, return {sender: [inputs...]} for any sender bound to >1 input."""
    by_sender = {}
    for inp, snd in bindings.items():
        by_sender.setdefault(snd, []).append(inp)
    return {s: v for s, v in by_sender.items() if len(v) > 1}


# ─── OBS WebSocket helpers (same _conn/_rpc pattern as obs_burn_filter.py) ────

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


def _rpc(ws, rtype, rdata=None):
    ws.send(json.dumps({"op": 6, "d": {
        "requestType": rtype, "requestId": rtype, "requestData": rdata or {}}}))
    t0 = time.monotonic()
    while True:
        if time.monotonic() - t0 >= 30:
            raise TimeoutError(f"obs-websocket request {rtype!r} timed out")
        try:
            m = json.loads(ws.recv())
        except WebSocketTimeoutException:
            continue
        if m["op"] == 7 and m["d"]["requestId"] == rtype:
            st = m["d"]["requestStatus"]
            if not st["result"]:
                raise RuntimeError(f"{rtype} failed: {st}")
            return m["d"].get("responseData") or {}


def _get_binding(ws, inp):
    return _rpc(ws, "GetInputSettings", {"inputName": inp}) \
        .get("inputSettings", {}).get("ndi_source_name", "")


def main():
    ap = argparse.ArgumentParser(description="#399 enforce strih NDI mapping")
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default="")
    ap.add_argument("--map", action="append", help='"INPUT=SENDER" (repeatable; default = the pins)')
    ap.add_argument("--verify-only", action="store_true", help="check + report, do not set")
    args = ap.parse_args()

    try:
        want = parse_map_args(args.map)
    except ValueError as e:
        sys.exit(f"ERROR: {e}")

    try:
        ws = _conn(args.host, args.password)
    except Exception as e:
        print(f"ERROR: OBS WS connect {args.host}: {e}", file=sys.stderr)
        sys.exit(2)

    try:
        for inp, snd in want:
            cur = _get_binding(ws, inp)
            if cur == snd:
                print(f"  {inp!r:12} already -> {snd!r}")
                continue
            if args.verify_only:
                print(f"  {inp!r:12} DRIFT: {cur!r} (want {snd!r})")
                continue
            _rpc(ws, "SetInputSettings",
                 {"inputName": inp, "inputSettings": {"ndi_source_name": snd}, "overlay": True})
            print(f"  {inp!r:12} set: {cur!r} -> {snd!r}")

        bindings = {inp: _get_binding(ws, inp) for inp, _ in want}
    except Exception as e:
        print(f"ERROR: OBS WS request: {e}", file=sys.stderr)
        sys.exit(2)
    finally:
        try:
            ws.close()
        except Exception:
            pass

    dups = duplicates(bindings)
    wrong = [(inp, bindings[inp], snd) for inp, snd in want if bindings[inp] != snd]
    if dups:
        print(f"FAIL: duplicate camera bindings remain: {dups}", file=sys.stderr)
        sys.exit(1)
    if wrong and not args.verify_only:
        print(f"FAIL: inputs not bound to their pin: {wrong}", file=sys.stderr)
        sys.exit(1)
    if wrong and args.verify_only:
        print(f"DRIFT: {len(wrong)} input(s) off their pin (verify-only)", file=sys.stderr)
        sys.exit(1)
    print(f"PASS: {len(want)} inputs bound to distinct cameras "
          f"({', '.join(f'{i}->{s}' for i, s in want)})")
    sys.exit(0)


if __name__ == "__main__":
    main()
