#!/usr/bin/env python3
"""#1312 — the I/O half of the measurement-chain-latency check: sample the stream `mbc` input's peak
LEVEL off stream OBS via the obs-websocket `InputVolumeMeters` high-volume event over ~30 s and emit a
TIMESTAMPED `sample <recv_ts_ns> <peak_db>` stream — no recording, no disk, no rig mutation (safe to
run between productions). The pure kernel `scripts/measurement_chain_latency.py` detects the marker
burst ONSETS in this sample stream and pairs them to the cam2 marker emit times to compute the mbc
chain latency.

Each `mbc` `InputVolumeMeters` event is stamped with dev1's `time.time_ns()` at receipt (dev1's
CLOCK_REALTIME is DanteSync-disciplined via the #1313 `local` dantesync node, the same clock the cam2
painter's wall-clock emit_ts would be on), so the onset times are directly comparable to the marker
emit times. The receipt stamp carries small network+processing jitter, but the check takes the MEDIAN
over ≥ 6 markers and the ±90 ms tolerance absorbs it.

Output (stdout):
    sample <recv_ts_ns> <peak_db>     one line per InputVolumeMeters event carrying `mbc`
    sampled=1                          (at least one line was produced; 0 if `mbc` never appeared)

EXIT CODE = the box-reachable signal the orchestrator keys on: 0 iff the WS connected + the v5
handshake completed (box reachable — even if `mbc` never appeared → sampled=0 → the pure kernel reads
UNKNOWN); non-zero iff the connect/handshake failed (→ box_reachable=0 → SKIP, deferring a dead stream
box to #1001/#732, never a false drift page).

Reuses the WS handshake helpers of the sibling #1310 probe (`measurement_audio_meter_probe`), adding
per-event timestamping. This is I/O only (a live OBS is required); it has no local test path — the
supervisor's live baseline capture + first live read cover it.
"""
import argparse
import json
import os
import sys
import time

try:
    from websocket import WebSocketTimeoutException
except ImportError:
    sys.exit("missing dep: pip install websocket-client")

# The sibling #1310 probe already implements the exact v5 op-0/op-1/op-5 InputVolumeMeters handshake
# and the per-event max-peak → dBFS conversion; reuse it rather than duplicate the WS protocol.
from measurement_audio_meter_probe import _connect, _max_peak_for_input, _peak_to_db

DEFAULT_PORT = 4455
DEFAULT_INPUT = os.environ.get("MEASUREMENT_AUDIO_INPUT", "mbc")
# ~32 s covers ≥ 6 markers at the ~5 s permanent-unit cadence (300 ticks @ 60 Hz) with margin.
DEFAULT_SAMPLE_S = float(os.environ.get("MEASUREMENT_CHAIN_SAMPLE_S", "32.0"))
DEFAULT_CONNECT_TIMEOUT_S = float(os.environ.get("MEASUREMENT_AUDIO_WS_TIMEOUT", "8"))
DEFAULT_DB_FLOOR = float(os.environ.get("MEASUREMENT_AUDIO_DB_FLOOR", "-100"))
_WS_PASSWORD = os.environ.get("MEASUREMENT_AUDIO_WS_PASSWORD", "")


def stream_samples(ws, input_name, sample_s, db_floor, out):
    """Drain InputVolumeMeters for ~sample_s and print `sample <recv_ts_ns> <peak_db>` for each event
    carrying `input_name`. Returns the number of sample lines emitted."""
    n = 0
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
        peak = _max_peak_for_input(d.get("eventData", {}), input_name)
        if peak is None:
            continue
        recv_ts_ns = time.time_ns()  # dev1 CLOCK_REALTIME (DanteSync-disciplined) at receipt
        print(f"sample {recv_ts_ns} {_peak_to_db(peak, db_floor)}", file=out)
        n += 1
    return n


def _main(argv):
    ap = argparse.ArgumentParser(description="mbc measurement-chain-latency sampler (#1312)")
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
        print(f"measurement-chain-latency probe: WS connect/handshake to {ns.host}:{ns.port} failed: {e}",
              file=sys.stderr)
        return 1
    try:
        n = stream_samples(ws, ns.input, ns.sample_s, ns.db_floor, sys.stdout)
    finally:
        try:
            ws.close()
        except OSError as e:
            print(f"measurement-chain-latency probe: ws.close() failed (ignored): {e}", file=sys.stderr)

    print(f"sampled={1 if n else 0}")
    return 0


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
