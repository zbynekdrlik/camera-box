#!/usr/bin/env python3
"""#286 — 4-camera MUTUAL phase-sync auto-set controller: measured per-camera cam->strih
latencies -> per-camera genlock video-delay, applied over the OBS WebSocket, with the
calibrated set PERSISTED.

What is landed vs what this adds:
  - MEASUREMENT: `camera_box::probe::recording_latency::{n_camera_strih_samples,
    n_camera_median_latency_ms}` (src/probe/recording_latency.rs) reuse the proven
    `cam_strih_samples` pairing, once per camera's distinct capture-burn run_id, to compute
    each camera's cam->strih latency from ONE strih recording. The offset math itself is the
    pure Tier-0 kernel `camera_box::phase_sync::compute_phase_sync_offsets` (src/phase_sync.rs)
    -- already locked by its own Rust unit tests. #438: this script no longer duplicates that
    kernel in Python -- it shells out to the compiled `phase-sync-gate` Rust binary
    (src/bin/phase-sync-gate.rs), the SAME pattern `render-budget-gate.py` uses for
    `render_budget::classify`, so there is exactly ONE implementation of the offset formula and
    the two can never silently drift (closes #438, an EPIC #406 gate-coverage audit finding).
  - CONTROLLER (this script, the actual gap #286 closes): nothing turns the measured per-camera
    latencies into applied per-source genlock latencies, and nothing persists the calibrated
    set. This script reads each camera's measured latency, computes its target genlock
    latency, applies it to that camera's strih NDI source over the OBS WebSocket (reusing the
    obs_phase2.py connection/RPC helpers -- same as `av_sync_calibrate.py`, #427), verifies via
    read-back, and on success persists the whole calibrated set to phase-sync-last.json for the
    #390 drift-guard pin to read.

Sign convention: the SLOWEST camera (max measured latency) is pinned at the floor; every
other camera's genlock latency = floor + (slowest - own), i.e. it is held back by exactly how
much earlier it would otherwise present -- so ALL cameras release the SAME captured instant at
the SAME wall-clock time. (This convention is now implemented ONCE, in
`camera_box::phase_sync::compute_phase_sync_offsets` -- see `compute_phase_sync_offsets` below,
which just pipes JSON to the compiled gate and back.)

Apply safety (#358 pattern, same as av_sync_calibrate.py): the PRE-CHANGE latency is read
first (the snapshot) for EACH source. After SetInputSettings, a GetInputSettings read-back
MUST match what was set; on a mismatch (the #292 force-drain class) that source is ROLLED BACK
to its pre-change value and the run FAILS LOUD -- a source is never left half-set. Each
camera's apply+verify is independent (mirrors av_sync_calibrate.py's single-source scope,
just run once per camera here); a later camera's failure does not un-apply an earlier one.

Usage:
    phase_sync_calibrate.py --host <ip> [--password P] \
        --measured-json <path to {strih_source_name: measured_latency_ms}> \
        [--apply] [--json-path <path>] [--gate-bin <path to phase-sync-gate>]

Without --apply this is a DRY RUN: prints the plan, changes nothing on the OBS box.

Env:
  PHASE_SYNC_GATE_BIN  — path to the phase-sync-gate Rust binary (#438)
                         (default: probed from PROBE_BIN_DIR or target/release, same
                         search order as RENDER_GATE_BIN / FROZEN_GATE_BIN)
"""
import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

# Reuse the proven obs-websocket v5 connection + RPC helpers (obs_burn_filter.py convention,
# same as av_sync_calibrate.py).
from obs_phase2 import _conn, _rpc  # noqa: E402

# OBS property name for the per-source genlock latency (PROP_GENLOCK_LATENCY_MS_SRC in
# ndi-source.cpp, DistroAV fork -- the "Latency (ms)" slider per source). Identical knob
# av_sync_calibrate.py drives; #286 just sets it on FOUR sources instead of one.
GENLOCK_SRC_LATENCY_KEY = "genlock_latency_ms_src"

# Mirrors `camera_box::phase_sync::PHASE_SYNC_FLOOR_MS` / `PHASE_SYNC_CAP_MS`
# (src/phase_sync.rs), which themselves mirror the DistroAV clamp
# PROP_GENLOCK_LATENCY_MS_MIN=3 / PROP_GENLOCK_SOURCE_LATENCY_MS_MAX=2000 (same range
# av_sync_calibrate.py's LATENCY_MIN/LATENCY_MAX use). Keep all three in lock-step. (Only used
# here as `read_current_latency`'s sane display default when a source has no genlock setting
# yet -- the FORMULA itself, #438, now lives in exactly one place: the compiled
# `phase-sync-gate` binary this script shells out to below.)
PHASE_SYNC_FLOOR_MS = 3
PHASE_SYNC_CAP_MS = 2000


def default_last_json_path() -> Path:
    """Where the controller persists the last-applied calibration for the #390 drift-guard pin
    to read (mirrors `av_sync_calibrate.default_last_json_path`, same %PROGRAMDATA%/camera-box
    ProgramData directory, different filename).

    On the real Windows OBS box, %PROGRAMDATA% resolves to `C:\\ProgramData` and the file lands
    at `C:\\ProgramData\\camera-box\\phase-sync-last.json`. When PROGRAMDATA is unset (dev/test
    off-rig), falls back to a local path under the user's home directory so this stays fully
    testable off-rig. Override with --json-path when the write needs to land somewhere else.
    """
    programdata = os.environ.get("PROGRAMDATA")
    if programdata:
        return Path(programdata) / "camera-box" / "phase-sync-last.json"
    return Path.home() / ".camera-box" / "phase-sync-last.json"


def _find_gate_bin(explicit: "str | None") -> str:
    """Locate the phase-sync-gate Rust binary (#438) -- same search order as
    render-budget-gate.py's `_find_verdict_bin` / frozen-camera-gate.py's helper:
    explicit arg > PHASE_SYNC_GATE_BIN env > $PROBE_BIN_DIR/phase-sync-gate > local
    target/release/phase-sync-gate."""
    if explicit:
        return explicit
    env_bin = os.environ.get("PHASE_SYNC_GATE_BIN", "")
    if env_bin and os.path.isfile(env_bin):
        return env_bin
    probe_dir = os.environ.get("PROBE_BIN_DIR", "")
    if probe_dir:
        candidate = os.path.join(probe_dir, "phase-sync-gate")
        if os.path.isfile(candidate):
            return candidate
    here = os.path.dirname(os.path.abspath(__file__))
    local = os.path.normpath(os.path.join(here, "..", "target", "release", "phase-sync-gate"))
    if os.path.isfile(local):
        return local
    raise SystemExit(
        "[phase-sync] ERROR: phase-sync-gate binary not found. "
        "Pass --gate-bin, set PHASE_SYNC_GATE_BIN, or set PROBE_BIN_DIR. "
        "To build: cargo build --release --bin phase-sync-gate  # airuleset:build-ok"
    )


def _run_gate_bin(measured: dict, gate_bin: "str | None" = None) -> dict:
    """Invoke the compiled `phase-sync-gate` Rust binary -- the SINGLE source of truth for the
    offset formula (#438) -- via stdin/stdout JSON, mirroring render-budget-gate.py's
    subprocess convention EXACTLY (`subprocess.run([bin], input=json.dumps(...).encode(),
    capture_output=True)`). This function's only job is I/O: locate the binary, feed it the
    measured latencies as JSON, parse its JSON reply, and fail LOUD (never silently guess) on
    a non-zero exit or malformed output -- it does NOT reimplement any part of the formula.
    """
    bin_path = _find_gate_bin(gate_bin)
    result = subprocess.run(
        [bin_path], input=json.dumps(measured).encode(), capture_output=True
    )
    if result.returncode != 0:
        raise SystemExit(
            f"[phase-sync] phase-sync-gate failed (exit {result.returncode}): "
            f"{result.stderr.decode().strip()}"
        )
    try:
        return json.loads(result.stdout.decode())
    except json.JSONDecodeError as e:
        raise SystemExit(
            f"[phase-sync] phase-sync-gate produced invalid JSON on stdout: {e} "
            f"(stdout={result.stdout!r})"
        ) from e


def compute_phase_sync_offsets(measured: dict, gate_bin: "str | None" = None) -> dict:
    """measured: {source_name: cam->strih latency_ms}. Returns {source_name: offset_ms (int)}.

    #438: DELEGATES to the compiled `phase-sync-gate` Rust binary, which wraps the SAME
    `camera_box::phase_sync::compute_phase_sync_offsets` kernel `src/phase_sync.rs`'s 9 unit
    tests already lock -- so there is exactly ONE implementation of the offset formula (the
    SLOWEST source pinned at the floor, every other source held back by exactly how much
    earlier it would otherwise present, clamped to [floor, cap]), never two to keep in
    lock-step by hand.

    Empty input -> empty output (no cameras, nothing to align) WITHOUT invoking the binary --
    matches the kernel's own empty-input behavior.
    """
    if not measured:
        return {}
    return _run_gate_bin(measured, gate_bin)


def load_measured_json(path: str) -> dict:
    """Load {source_name: latency_ms} from a JSON file -- the per-camera measured cam->strih
    latencies (e.g. hand-assembled from `n_camera_median_latency_ms` output, keyed by the
    strih NDI source name each camera's burn maps to).

    Fails LOUD (SystemExit) on an empty/non-object mapping -- never silently computes offsets
    from zero cameras, and never guesses a latency for a source that isn't in the file.
    """
    with open(path) as f:
        data = json.load(f)
    if not isinstance(data, dict) or not data:
        raise SystemExit(
            f"[phase-sync] {path}: expected a non-empty {{source: latency_ms}} JSON object -- "
            "measurement UNRESOLVED, refusing to guess"
        )
    return {str(k): float(v) for k, v in data.items()}


def read_current_latency(ws, source: str) -> int:
    """Read the CURRENT genlock_latency_ms_src on `source` (the pre-change snapshot)."""
    settings = _rpc(ws, "GetInputSettings", {"inputName": source}).get("inputSettings", {})
    current = int(settings.get(GENLOCK_SRC_LATENCY_KEY, PHASE_SYNC_FLOOR_MS))
    print(f"[phase-sync] source='{source}' current genlock_latency_ms_src={current}ms")
    return current


def apply_latency(ws, source: str, current_ms: int, new_ms: int) -> int:
    """Set genlock_latency_ms_src=`new_ms` on `source`, verify via read-back (#358 pattern).

    Identical safety shape to `av_sync_calibrate.apply_latency` -- kept as its OWN copy (not
    imported) so a future divergence in one controller's safety behavior can never silently
    leak into the other. On a read-back mismatch (the #292 force-drain class), ROLLS BACK to
    `current_ms` and FAILS LOUD -- the source is never left half-set. If even the rollback
    read-back mismatches, prints a LOUD warning (manual check required) before still failing
    loud.
    """
    print(f"[phase-sync] SET '{source}' {GENLOCK_SRC_LATENCY_KEY}: {current_ms} -> {new_ms}")
    _rpc(ws, "SetInputSettings", {
        "inputName": source,
        "inputSettings": {GENLOCK_SRC_LATENCY_KEY: new_ms},
        "overlay": True,
    })
    back = _rpc(ws, "GetInputSettings", {"inputName": source}).get("inputSettings", {})
    actual = back.get(GENLOCK_SRC_LATENCY_KEY)
    if actual == new_ms:
        print(f"[phase-sync] VERIFIED '{source}' {GENLOCK_SRC_LATENCY_KEY}={actual}")
        return actual

    sys.stderr.write(
        f"[phase-sync] read-back mismatch on '{source}': set {new_ms}, got {actual!r} -- "
        f"rolling back to {current_ms}\n"
    )
    _rpc(ws, "SetInputSettings", {
        "inputName": source,
        "inputSettings": {GENLOCK_SRC_LATENCY_KEY: current_ms},
        "overlay": True,
    })
    rollback_back = _rpc(ws, "GetInputSettings", {"inputName": source}).get("inputSettings", {})
    rollback_actual = rollback_back.get(GENLOCK_SRC_LATENCY_KEY)
    if rollback_actual != current_ms:
        sys.stderr.write(
            f"[phase-sync] WARN rollback ALSO mismatched on '{source}': expected {current_ms}, "
            f"got {rollback_actual!r} -- manual check required!\n"
        )
    raise SystemExit(
        f"[phase-sync] FAILED to apply {GENLOCK_SRC_LATENCY_KEY}={new_ms} on '{source}' "
        f"(read-back={actual!r}); rolled back to {current_ms} "
        f"(rollback read-back={rollback_actual!r}) -- source never left half-set"
    )


def write_last_json(json_path: Path, cameras: list) -> None:
    """Persist the calibrated set: {"cameras": [{source, latency_ms, offset_ms,
    applied_latency_ms}, ...], "ts": <epoch>}.

    Read by the #390 drift-guard pin to track the calibrated per-camera offsets instead of a
    stale hardcoded constant. Written atomically (write-tmp + replace, same as
    av_sync_calibrate.write_last_json) so a reader never observes a partial file.
    """
    json_path.parent.mkdir(parents=True, exist_ok=True)
    payload = {"cameras": cameras, "ts": time.time()}
    tmp = json_path.with_suffix(json_path.suffix + ".tmp")
    tmp.write_text(json.dumps(payload, indent=2))
    tmp.replace(json_path)
    print(f"[phase-sync] persisted {json_path}: {payload}")


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default="")
    ap.add_argument(
        "--measured-json", required=True,
        help="path to a JSON object {strih_source_name: measured_cam_to_strih_latency_ms}",
    )
    ap.add_argument("--apply", action="store_true", help="actually set (default: dry-run)")
    ap.add_argument(
        "--json-path", type=str, default=None,
        help="override the phase-sync-last.json write path "
             "(default: %%PROGRAMDATA%%/camera-box/phase-sync-last.json)",
    )
    ap.add_argument(
        "--gate-bin", type=str, default=None,
        help="path to the phase-sync-gate Rust binary (#438) "
             "(default: probed from PHASE_SYNC_GATE_BIN or PROBE_BIN_DIR)",
    )
    args = ap.parse_args()

    measured = load_measured_json(args.measured_json)
    offsets = compute_phase_sync_offsets(measured, gate_bin=args.gate_bin)

    ws = _conn(args.host, args.password)
    plan = []
    for source, latency_ms in measured.items():
        new_ms = offsets[source]
        current = read_current_latency(ws, source)
        print(
            f"[phase-sync] source='{source}' measured_latency={latency_ms:.1f}ms "
            f"current={current}ms -> new={new_ms}ms"
        )
        plan.append((source, latency_ms, current, new_ms))

    if not args.apply:
        print("[phase-sync] dry-run (pass --apply to set)")
        return

    cameras = []
    for source, latency_ms, current, new_ms in plan:
        applied = apply_latency(ws, source, current, new_ms)
        cameras.append({
            "source": source,
            "latency_ms": latency_ms,
            "offset_ms": new_ms,
            "applied_latency_ms": applied,
        })

    json_path = Path(args.json_path) if args.json_path else default_last_json_path()
    write_last_json(json_path, cameras)
    print(f"[phase-sync] APPLIED + verified {len(cameras)} camera(s); persisted {json_path}")


if __name__ == "__main__":
    main()
