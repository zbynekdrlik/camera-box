#!/usr/bin/env python3
"""#887 -- ONE GetStats snapshot of imag's own compositor render/output frame counters.

Part of the honest "produced vs presented" comparison this ticket adds (see
scripts/lib/imag-presented-frame-check.sh for the independent, kernel-side "presented on
HDMI-1" counter this is compared against). This script measures ONLY the "produced" half --
what OBS's own render loop reports about ITSELF via obs-websocket `GetStats`. It never claims
anything about what actually reached the connector; the presented-frame counter is what proves
that half.

Reuses obs_phase2's proven websocket connection/auth handshake (same import-reuse pattern as
scripts/obs_burn_filter.py) rather than re-implementing it.

Usage:
  imag_produced_frame_check.py --host <ip> [--password P]

Prints exactly one line:
  PRODUCED renderTotalFrames=<n> renderSkippedFrames=<n> outputTotalFrames=<n> outputSkippedFrames=<n>
"""
import argparse

from obs_phase2 import _conn, _rpc  # noqa: E402


def produced_line(stats):
    """Pure formatter: a GetStats responseData dict -> the one-line PRODUCED report.

    Split out from main() so it's testable without a live OBS connection (see
    tests/python/test_imag_produced_frame_check.py).
    """
    return (
        "PRODUCED "
        f"renderTotalFrames={int(stats.get('renderTotalFrames', 0))} "
        f"renderSkippedFrames={int(stats.get('renderSkippedFrames', 0))} "
        f"outputTotalFrames={int(stats.get('outputTotalFrames', 0))} "
        f"outputSkippedFrames={int(stats.get('outputSkippedFrames', 0))}"
    )


def main():
    ap = argparse.ArgumentParser(description="#887 -- one GetStats snapshot (compositor-produced frames)")
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default="")
    a = ap.parse_args()
    ws = _conn(a.host, a.password)
    try:
        stats = _rpc(ws, "GetStats")
    finally:
        ws.close()
    print(produced_line(stats))


if __name__ == "__main__":
    main()
