#!/usr/bin/env python3
"""#893 -- live preflight: at least one ACTIVE strih camera must sit at the phase-sync floor.

Reads the CURRENTLY-CONFIGURED genlock_latency_ms_src for every CAMERA_ACTIVE_SET camera's
strih "NDI cam<N>" source over OBS WebSocket -- LIVE, never the persisted
~/.camera-box/phase-sync-last.json (that file is exactly what let #893's live-vs-file
divergence go unnoticed: it kept showing the healthy 2026-07-09 calibration while the live
pins had all drifted away from it) -- and shells the result to the compiled
`phase-sync-active-floor-gate` Rust binary for the verdict
(camera_box::phase_sync_active_floor -- the single source of truth for the decision, never
re-derived here).

Usage:
  python3 scripts/phase_sync_active_floor_check.py --host 10.77.9.202 [--password PW]
      [--gate-bin path/to/phase-sync-active-floor-gate]

Exit codes (mirrors the gate binary exactly):
  0   PASS -- at least one active camera sits at the floor
  1   FAIL -- every measured active camera drifted above the floor, or none could be read
  2   ERROR -- bad args / OBS WS connection failure / gate binary not found
"""
import argparse
import json
import os
import subprocess
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

from obs_phase2 import _conn  # noqa: E402
from latency_pins_snapshot import read_pin  # reuse the proven honest-None read # noqa: E402


def active_camera_names(explicit: "str | None" = None) -> list:
    """#893 -- the active camera names, split from `explicit` if given, else CAMERA_ACTIVE_SET
    (env var, default "cam1 cam2 cam3 cam4 cam5 cam6 cam7" -- issue 1198 (2026-08-27): cam1 + cam2
    RESTORED, both healthy on a live journal check, owner refused the physical card swap; issue
    1216 (2026-08-28): cam5/cam6/cam7 also restored, bigger splitter fitted; issue 1217 (same
    day): cam5 OUT again -- a DEAD_PORT leg on the new splitter (flat static frame, siblings
    cam6/cam7 read colour); issue 1216 completion (2026-08-30, owner directive "kamery od 1-7
    bezia" after a physical cable reseat): cam4 (#947) and cam5 (DEAD_PORT) BOTH rejoin -- the
    full seven-camera fleet) -- NEVER a literal range (.claude/rules/camera-active-set.md). Read
    fresh on every call.

    `explicit` mirrors set-ndi-mapping.py's `--active` flag: the caller (recording-e2e.sh)
    passes `--active-set "$CAMERA_ACTIVE_SET"` explicitly on the command line rather than
    relying on the shell variable happening to be an EXPORTED env var reaching this Python
    subprocess -- the same safer convention that script's own CLI already established.
    """
    raw = explicit if explicit is not None else os.environ.get("CAMERA_ACTIVE_SET", "cam1 cam2 cam3 cam4 cam5 cam6 cam7")
    return [tok.strip() for tok in raw.replace(",", " ").split() if tok.strip()]


def _find_gate_bin(explicit: "str | None") -> str:
    """Same search order as phase_sync_calibrate._find_gate_bin: explicit arg >
    PHASE_SYNC_ACTIVE_FLOOR_GATE_BIN env > $PROBE_BIN_DIR/phase-sync-active-floor-gate > local
    target/release/phase-sync-active-floor-gate."""
    if explicit:
        return explicit
    env_bin = os.environ.get("PHASE_SYNC_ACTIVE_FLOOR_GATE_BIN", "")
    if env_bin and os.path.isfile(env_bin):
        return env_bin
    probe_dir = os.environ.get("PROBE_BIN_DIR", "")
    if probe_dir:
        candidate = os.path.join(probe_dir, "phase-sync-active-floor-gate")
        if os.path.isfile(candidate):
            return candidate
    local = os.path.normpath(
        os.path.join(_HERE, "..", "target", "release", "phase-sync-active-floor-gate")
    )
    if os.path.isfile(local):
        return local
    raise SystemExit(
        "ERROR: phase-sync-active-floor-gate binary not found. Pass --gate-bin, set "
        "PHASE_SYNC_ACTIVE_FLOOR_GATE_BIN, or set PROBE_BIN_DIR. "
        "To build: cargo build --release --bin phase-sync-active-floor-gate  # airuleset:build-ok"
    )


def read_active_pins(host: str, password: str, active_set: list) -> dict:
    """Connect to `host`, read genlock_latency_ms_src for each active camera's "NDI camN" strih
    source. Returns {cam_name: pin_ms} -- a camera whose read fails/is-missing is simply absent
    (honest, never a fabricated default), mirroring read_pin's own convention."""
    ws = _conn(host, password)
    try:
        pins = {}
        for cam in active_set:
            v = read_pin(ws, f"NDI {cam}")
            if v is not None:
                pins[cam] = v
        return pins
    finally:
        ws.close()


def run_gate_bin(active_set: list, pins: dict, gate_bin: "str | None" = None):
    """Shell the decision to the compiled gate binary. Returns the completed subprocess.Popen
    result (never parsed here) -- main() just relays its stdout/stderr/exit code verbatim."""
    bin_path = _find_gate_bin(gate_bin)
    payload = json.dumps({"active_set": active_set, "pins": pins}).encode()
    return subprocess.run([bin_path], input=payload, capture_output=True)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default="")
    ap.add_argument("--gate-bin", default=None)
    ap.add_argument(
        "--active-set", default=None,
        help="space/comma-separated camera names (default: $CAMERA_ACTIVE_SET, "
             "same convention as set-ndi-mapping.py's --active)",
    )
    args = ap.parse_args(argv)

    active_set = active_camera_names(args.active_set)
    if not active_set:
        print("ERROR: CAMERA_ACTIVE_SET resolved to an empty list -- nothing to gate", file=sys.stderr)
        return 2

    try:
        pins = read_active_pins(args.host, args.password, active_set)
    except Exception as e:  # noqa: BLE001 -- any WS/network failure -> loud ERROR, never a silent pass
        print(f"ERROR: could not read live pins from {args.host}: {e}", file=sys.stderr)
        return 2

    result = run_gate_bin(active_set, pins, gate_bin=args.gate_bin)
    if result.stdout:
        sys.stdout.write(result.stdout.decode())
    if result.stderr:
        sys.stderr.write(result.stderr.decode())
    return result.returncode


if __name__ == "__main__":
    sys.exit(main())
