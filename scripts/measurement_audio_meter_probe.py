#!/usr/bin/env python3
"""#1310 — the I/O half of the dev1 measurement-audio presence watchdog: read the `mbc` input's
peak LEVEL off stream OBS via the obs-websocket `InputVolumeMeters` high-volume event, WITHOUT any
recording (no StartRecord, no disk, no rig mutation — safe to run every 60 s near a live show).

It connects to stream OBS `ws://<host>:4455` (LAN, no auth by default), subscribes to ONLY the
InputVolumeMeters event (the op-1 `eventSubscriptions` high-volume bit `1<<16`), samples ~2 s, takes
the MAX peak multiplier seen for input `mbc` across all channels, converts it to dBFS
(20·log10(peak), clamped to a floor so digital silence — peak 0.0 → −∞ — reads a finite deep-negative
number), and prints the probe result as `key=value` lines the pure decision module reads:

    meter_present=1        (mbc appeared in the meter stream this window; 0 if it never did)
    peak_db=-4.7           (the peak in dBFS; empty when meter_present=0)

EXIT CODE = the box-reachable signal the orchestrator keys on: 0 iff the WS connected + the v5
handshake completed (box reachable — even if `mbc` never appeared → meter_present=0 → UNKNOWN);
non-zero iff the connect/handshake failed (→ the orchestrator reads box_reachable=0 → SKIP, deferring
a dead stream box to #1001/#732, never a false silent page).

This is I/O only (a live OBS is required); the VERDICT lives in the pure, unit-tested
scripts/measurement_audio_decision.py. The orchestrator invokes this behind the
MEASUREMENT_AUDIO_FETCH_CMD seam so a --dry-run stubs it with no live box.

WS handshake shape mirrors scripts/obs_phase2.py::_conn (op-0 Hello → op-1 Identify → op-2 Identified),
adding the InputVolumeMeters subscription obs_phase2 deliberately omits (it is pure request/response).
Requires: pip install websocket-client (already used by obs_phase2.py / obs-liveness-probe.py).
"""
import argparse
import base64
import hashlib
import json
import math
import os
import sys
import time

try:
    from websocket import WebSocketTimeoutException, create_connection
except ImportError:
    sys.exit("missing dep: pip install websocket-client")

# obs-websocket v5 EventSubscription: InputVolumeMeters is the high-volume bit 1<<16 (65536). We
# subscribe to ONLY it (not All) so no other event floods the read loop.
_EVENTSUB_INPUT_VOLUME_METERS = 1 << 16

DEFAULT_PORT = 4455
DEFAULT_INPUT = os.environ.get("MEASUREMENT_AUDIO_INPUT", "mbc")
DEFAULT_SAMPLE_S = float(os.environ.get("MEASUREMENT_AUDIO_SAMPLE_S", "2.0"))
DEFAULT_CONNECT_TIMEOUT_S = float(os.environ.get("MEASUREMENT_AUDIO_WS_TIMEOUT", "8"))
# Digital silence is peak multiplier 0.0 -> 20*log10(0) = -inf; clamp to a finite deep-negative dB so
# the decision compares real numbers. -100 dB is comfortably below the -60 dB silence bar and below
# true digital silence's ~-91 dB volumedetect reading, so it always classifies SILENT.
DEFAULT_DB_FLOOR = float(os.environ.get("MEASUREMENT_AUDIO_DB_FLOOR", "-100"))
_WS_PASSWORD = os.environ.get("MEASUREMENT_AUDIO_WS_PASSWORD", "")


def _connect(host, port, password, connect_timeout_s):
    """op-0 Hello -> op-1 Identify (subscribing to InputVolumeMeters only) -> op-2 Identified.
    Raises on any failure (the caller turns that into a non-zero exit = box unreachable)."""
    ws = create_connection(f"ws://{host}:{port}", timeout=connect_timeout_s)
    hello = json.loads(ws.recv())
    ident = {"op": 1, "d": {"rpcVersion": 1,
                            "eventSubscriptions": _EVENTSUB_INPUT_VOLUME_METERS}}
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
    json.loads(ws.recv())  # op-2 Identified (raises if the socket died mid-handshake)
    return ws


def _max_peak_for_input(event_data, input_name):
    """The MAX level multiplier across all channels of `input_name` in one InputVolumeMeters event,
    or None if this event does not carry that input. `inputLevelsMul` is a per-channel list of float
    multipliers (magnitude/peak/inputPeak); take the max of every value seen."""
    best = None
    for inp in event_data.get("inputs", []):
        if inp.get("inputName") != input_name:
            continue
        for channel in inp.get("inputLevelsMul", []):
            for v in channel:
                try:
                    fv = float(v)
                except (ValueError, TypeError):
                    continue
                if best is None or fv > best:
                    best = fv
    return best


def _peak_to_db(peak_mul, db_floor):
    """A linear peak multiplier -> dBFS (20*log10), clamped to `db_floor`. 0/negative -> floor."""
    if peak_mul is None or peak_mul <= 0.0:
        return db_floor
    db = 20.0 * math.log10(peak_mul)
    return db if db > db_floor else db_floor


def sample_peak(ws, input_name, sample_s, db_floor):
    """Drain InputVolumeMeters events for ~sample_s wall-clock; return `(peak_db_or_None,
    meter_present_bool)`. meter_present is True iff `input_name` appeared in at least one event
    (present-but-silent -> peak_db == db_floor, meter_present True -> SILENT, not UNKNOWN)."""
    best_peak = None
    seen = False
    deadline = time.monotonic() + sample_s
    ws.settimeout(0.5)
    while time.monotonic() < deadline:
        try:
            msg = json.loads(ws.recv())
        except WebSocketTimeoutException:
            continue
        except (ValueError, OSError):
            break
        if msg.get("op") != 5:
            continue
        d = msg.get("d", {})
        if d.get("eventType") != "InputVolumeMeters":
            continue
        p = _max_peak_for_input(d.get("eventData", {}), input_name)
        if p is not None:
            seen = True
            if best_peak is None or p > best_peak:
                best_peak = p
    if not seen:
        return (None, False)
    return (_peak_to_db(best_peak, db_floor), True)


def _main(argv):
    ap = argparse.ArgumentParser(description="mbc measurement-audio peak-level probe (#1310)")
    ap.add_argument("host")
    ap.add_argument("port", nargs="?", type=int, default=DEFAULT_PORT)
    ap.add_argument("--input", default=DEFAULT_INPUT)
    ap.add_argument("--sample-s", type=float, default=DEFAULT_SAMPLE_S)
    ap.add_argument("--connect-timeout-s", type=float, default=DEFAULT_CONNECT_TIMEOUT_S)
    ap.add_argument("--db-floor", type=float, default=DEFAULT_DB_FLOOR)
    ns = ap.parse_args(argv)

    try:
        ws = _connect(ns.host, ns.port, _WS_PASSWORD, ns.connect_timeout_s)
    except Exception as e:  # noqa: BLE001 — any connect/handshake failure = box unreachable = SKIP
        print(f"measurement-audio probe: WS connect/handshake to {ns.host}:{ns.port} failed: {e}",
              file=sys.stderr)
        return 1
    try:
        peak_db, present = sample_peak(ws, ns.input, ns.sample_s, ns.db_floor)
    finally:
        try:
            ws.close()
        except OSError as e:
            # best-effort teardown; a failed close never changes the reading — log, never swallow.
            print(f"measurement-audio probe: ws.close() failed (ignored): {e}", file=sys.stderr)

    print(f"meter_present={1 if present else 0}")
    print(f"peak_db={'' if peak_db is None else peak_db}")
    return 0


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
