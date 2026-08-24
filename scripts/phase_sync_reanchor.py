#!/usr/bin/env python3
"""#900 -- pre-[4h/8] phase-sync RE-ANCHOR: the automatic ESTABLISHER the [4h/8] active-floor
gate never had.

`[4h/8]` (phase_sync_active_floor_check.py) fails the run unless at least one ACTIVE camera sits at
the 3ms floor. The only upstream establisher, `[4g/8]` (#757 auto-pin), is OFF by default (its
preview-skew RE-MEASUREMENT was demoted after 3 live regressions). So the gate is always on with no
establisher: a correct fleet change that moves the anchor (issue 898 retired the box at the floor)
red-lights a healthy rig on the very next run with no automatic remedy.

This step RE-ANCHORS -- which is NOT a re-measurement:

  * It introduces NO new measurement. It reads the transits ALREADY persisted in
    phase-sync-last.json (each camera's pin-INDEPENDENT `latency_ms`, the same numbers that
    produced the currently-working pins -- see .claude/rules/phase-sync-calibrator-testing.md),
    restricts them to CAMERA_ACTIVE_SET, and re-runs the UNCHANGED compute_phase_sync_offsets
    kernel (phase_sync_calibrate.py -> the compiled phase-sync-gate binary). No new kernel.
  * When the active set is unchanged it is a provable NO-OP: same transits -> same pins ->
    live==desired -> zero writes. When a camera leaves/joins, the surviving pins move by a pure
    CONSTANT (the common anchor shifts; mutual differences are preserved exactly).

Contract:
  * ON by default (the harness gates it on ALL_CAMBOX, opt-out PHASE_REANCHOR=0), FAIL-LOUD (never
    best-effort like [4g/8]): a missing/malformed persisted file, or one that does not cover the
    active set, is a genuine "no calibration basis" state -- exit nonzero so [4h/8] is never reached
    behind pins nobody established.
  * NO-OP when the live pins already satisfy the convention (already == the re-derived set): write
    nothing, so a healthy rig's pins never churn run to run.
  * Reads phase-sync-last.json but NEVER clobbers it (that stays the read-only transit basis for all
    cameras); the applied set is optionally recorded to a run-scoped --out-json.
  * [4h/8] stays byte-for-byte as strict as today; PRERECORD_PHASE_CALIBRATE stays off.

"Stale" is deliberately structural (unparseable / no cameras / non-numeric latency), NOT a
wall-clock TTL: physical transits do not drift hour-to-hour, and an age gate that fails a healthy
rig would re-introduce the very landmine this step removes.

Usage:
  python3 scripts/phase_sync_reanchor.py --host 10.77.9.202 [--password PW]
      --active-set "cam2 cam3" [--persisted-json <path>] [--out-json <path>]
      [--gate-bin <phase-sync-gate>] [--apply]

Without --apply this is a DRY RUN (prints the plan, changes nothing on the OBS box).

Exit codes:
  0   PASS -- re-anchor was a no-op, or applied + verified the establishing pin set
  1   FAIL -- no usable calibration basis (missing/malformed/uncovered), or an apply failure
  2   ERROR -- bad args / OBS WS connection failure
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

# Reuse the EXISTING phase-sync helpers verbatim -- no new kernel, no duplicated apply/persist.
from phase_sync_calibrate import (  # noqa: E402
    PHASE_SYNC_FLOOR_MS,
    apply_latency,
    compute_phase_sync_offsets,
    default_last_json_path,
    enforce_jitter_floor_ms,
    read_current_latency,
    write_last_json,
)
from obs_phase2 import _conn  # noqa: E402
from latency_pins_snapshot import read_pin  # honest-None live read  # noqa: E402


def _read_cameras(path: str) -> list:
    """Read + validate the phase-sync-last.json `cameras` list. FAIL LOUD (SystemExit) on any "no
    usable basis" condition -- missing file, unparseable JSON, or no cameras."""
    p = Path(path)
    if not p.is_file():
        raise SystemExit(
            f"ERROR: #900 persisted calibration basis not found: {path} -- no basis to re-anchor "
            f"from. Recalibrate first: python3 scripts/phase_sync_calibrate.py --host $STRIH "
            f"--measured-json <path> --apply"
        )
    try:
        data = json.loads(p.read_text())
    except (json.JSONDecodeError, OSError) as e:
        raise SystemExit(f"ERROR: #900 persisted calibration basis {path} is unreadable/malformed: {e}")
    cameras = data.get("cameras") if isinstance(data, dict) else None
    if not isinstance(cameras, list) or not cameras:
        raise SystemExit(
            f"ERROR: #900 persisted calibration basis {path} has no cameras -- unusable as a "
            f"re-anchor basis"
        )
    return cameras


def load_persisted_transits(path: str) -> dict:
    """Load {source_name: latency_ms} from the persisted phase-sync-last.json. `latency_ms` is the
    pin-INDEPENDENT cam->strih transit (the calibration basis), NOT the applied offset.

    FAIL LOUD (SystemExit) on any "no usable basis" condition -- missing file, unparseable JSON,
    no cameras, or a camera missing/with a non-numeric latency_ms. Never guesses a transit."""
    transits: dict = {}
    for cam in _read_cameras(path):
        if not isinstance(cam, dict):
            raise SystemExit(f"ERROR: #900 persisted basis {path}: malformed camera entry {cam!r}")
        source = cam.get("source")
        latency = cam.get("latency_ms")
        if not isinstance(source, str) or not source:
            raise SystemExit(f"ERROR: #900 persisted basis {path}: camera entry missing 'source': {cam!r}")
        if not isinstance(latency, (int, float)) or isinstance(latency, bool):
            raise SystemExit(
                f"ERROR: #900 persisted basis {path}: source {source!r} has non-numeric "
                f"latency_ms={latency!r} -- unusable transit basis"
            )
        transits[source] = float(latency)
    return transits


def load_persisted_offsets(path: str) -> dict:
    """Load {source_name: offset_ms} (the APPLIED pin) from the persisted phase-sync-last.json --
    the input to recover_uniform_margin(). Best-effort: only entries carrying a numeric integer
    offset_ms are returned (an older/partial file simply yields fewer, and recover_uniform_margin
    then treats it as margin-free); never fails on a missing offset_ms (latency_ms is the required
    basis, validated separately by load_persisted_transits)."""
    offsets: dict = {}
    for cam in _read_cameras(path):
        if not isinstance(cam, dict):
            continue
        source = cam.get("source")
        off = cam.get("offset_ms")
        if isinstance(source, str) and source and isinstance(off, (int, float)) and not isinstance(off, bool):
            offsets[source] = int(off)
    return offsets


def recover_uniform_margin(persisted_offsets: dict) -> int:
    """Recover the UNIFORM #757 jitter-headroom margin the persisted calibration applied, so a
    re-anchor PRESERVES it instead of silently stripping it.

    `phase_sync_calibrate.apply_margin` adds the SAME margin to every camera's kernel offset, and
    the kernel pins the slowest camera at PHASE_SYNC_FLOOR_MS -- so the slowest camera's applied
    pin is floor+margin and is the GLOBAL MINIMUM offset. margin = min(offset_ms) - floor.
    Because round(int + margin_float) == int + round(margin_float) for the integer kernel offsets,
    this integer margin reproduces every persisted offset exactly (a true no-op when the active
    set is unchanged). Returns 0 for a margin-free calibration (the standing default) or an empty
    set. Clamped >= 0 (a persisted pin can never sit below the floor)."""
    if not persisted_offsets:
        return 0
    return max(0, min(persisted_offsets.values()) - PHASE_SYNC_FLOOR_MS)


def restrict_to_active(transits: dict, active_sources: list) -> dict:
    """Keep only the sources in `active_sources` (drop any inactive/stale entry). FAIL LOUD
    (SystemExit) if the active set is empty or an active camera is NOT covered by the persisted
    basis -- there is nothing to re-anchor it from, exactly the "no calibration basis" state the
    gate must not be reached behind."""
    if not active_sources:
        raise SystemExit("ERROR: #900 active set resolved to empty -- nothing to re-anchor")
    missing = [s for s in active_sources if s not in transits]
    if missing:
        raise SystemExit(
            f"ERROR: #900 persisted calibration basis does not cover active camera(s): "
            f"{', '.join(missing)} -- recalibrate so every active camera has a transit basis "
            f"before this gate is reached"
        )
    return {s: transits[s] for s in active_sources}


def plan_reanchor(desired: dict, current: dict) -> tuple:
    """Pure no-op-vs-apply decision. `desired` = {source: pin_ms} the re-anchor would set;
    `current` = {source: live_pin_ms_or_None}. Returns (is_noop, changes) where changes is
    [(source, current_val, desired_val), ...] for every source whose live pin differs from desired
    (an unreadable None counts as a difference -- it must be established). is_noop == (changes==[]);
    a no-op means the live pins already satisfy the convention and NOTHING is written.

    Caveat: OBS's GetInputSettings OMITS a setting still at its registered default, so a source
    genuinely at the floor but never explicitly Set reads None here and is (idempotently) re-applied
    on the FIRST run -- after which it reads its explicit value and every later run is a true
    zero-write no-op. The re-anchor is idempotent, so this is harmless; it just isn't literally
    zero-write on that first defaulted run."""
    changes = []
    for source in sorted(desired):
        want = desired[source]
        have = current.get(source)
        if have != want:
            changes.append((source, have, want))
    return (not changes, changes)


def _active_sources(explicit: "str | None") -> list:
    """Split the active-set string into "NDI camN" source names (the strih source naming the pins
    live under). Mirrors phase_sync_active_floor_check's --active-set convention: a caller passes
    "cam3" (issue 1170: cam2's camera-under-test role retired [grabber cure-decay]) (space/comma separated); we map each to its "NDI camN" source."""
    raw = explicit if explicit is not None else os.environ.get("CAMERA_ACTIVE_SET", "cam3")
    cams = [tok.strip() for tok in raw.replace(",", " ").split() if tok.strip()]
    return [f"NDI {c}" for c in cams]


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default="")
    ap.add_argument(
        "--active-set", default=None,
        help="space/comma-separated camera names (default: $CAMERA_ACTIVE_SET), mapped to "
             "'NDI camN' strih sources",
    )
    ap.add_argument(
        "--persisted-json", default=None,
        help="path to the durable phase-sync-last.json transit basis "
             "(default: %%PROGRAMDATA%%/camera-box or ~/.camera-box fallback)",
    )
    ap.add_argument(
        "--out-json", default=None,
        help="optional RUN-SCOPED path to record the applied pin set (NEVER the durable basis file)",
    )
    ap.add_argument("--gate-bin", default=None, help="path to the phase-sync-gate Rust binary")
    ap.add_argument("--apply", action="store_true", help="actually set (default: dry-run)")
    args = ap.parse_args(argv)

    active_sources = _active_sources(args.active_set)
    persisted_path = args.persisted_json or str(default_last_json_path())

    # Requirement 4 hardening: NEVER write the applied set back over the durable transit basis --
    # that would drop every currently-inactive camera's basis. A run-scoped --out-json only.
    if args.out_json and os.path.abspath(args.out_json) == os.path.abspath(persisted_path):
        raise SystemExit(
            f"ERROR: #900 --out-json must NOT be the durable persisted basis ({persisted_path}) -- "
            f"the applied set records to a run-scoped file, never clobbering the transit basis"
        )

    # 1. read the persisted transits + restrict to the active set (both FAIL LOUD -> exit 1)
    transits = load_persisted_transits(persisted_path)
    active_transits = restrict_to_active(transits, active_sources)

    # 2. re-derive the pin set via the UNCHANGED kernel (no new measurement, no new kernel), then
    # add back the UNIFORM jitter-headroom margin the persisted calibration already applied -- so a
    # re-anchor PRESERVES any #757 headroom rather than silently stripping it (margin 0 = the
    # standing margin-free default -> exact no-op).
    offsets = compute_phase_sync_offsets(active_transits, gate_bin=args.gate_bin)
    margin = recover_uniform_margin(load_persisted_offsets(persisted_path))
    desired = {s: enforce_jitter_floor_ms(offsets[s] + margin) for s in active_transits}
    print(
        f"[reanchor] #900 basis={persisted_path} active={sorted(active_sources)} "
        f"margin={margin}ms desired_pins={desired}"
    )

    # 3. read the CURRENT live pins and decide no-op vs apply
    try:
        ws = _conn(args.host, args.password)
    except Exception as e:  # noqa: BLE001 -- WS/network failure is an ERROR, never a silent pass
        print(f"ERROR: #900 could not connect to OBS at {args.host}: {e}", file=sys.stderr)
        return 2
    try:
        current = {s: read_pin(ws, s) for s in active_sources}
        is_noop, changes = plan_reanchor(desired, current)

        if is_noop:
            floor_src = min(desired, key=lambda s: desired[s])
            print(
                f"[reanchor] #900 NO-OP -- live pins already satisfy the convention "
                f"(floor {desired[floor_src]}ms at {floor_src!r}); nothing written"
            )
            return 0

        print("[reanchor] #900 re-anchor needed: " + ", ".join(
            f"{s}: {have}->{want}" for s, have, want in changes
        ))
        if not args.apply:
            print("[reanchor] #900 dry-run (pass --apply to set)")
            return 0

        cameras = []
        for source in active_transits:
            want = desired[source]
            # apply_latency needs a pre-change snapshot for its read-back/rollback contract --
            # use the canonical read_current_latency (int, sane default), exactly as
            # phase_sync_calibrate.main() does, rather than the honest-None read used for the
            # no-op decision above.
            current_ms = read_current_latency(ws, source)
            applied = apply_latency(ws, source, current_ms, want)
            cameras.append({
                "source": source,
                "latency_ms": active_transits[source],
                "offset_ms": want,
                "applied_latency_ms": applied,
            })

        if args.out_json:
            out = Path(args.out_json)
            out.parent.mkdir(parents=True, exist_ok=True)
            write_last_json(out, cameras)
            print(f"[reanchor] #900 recorded applied set -> {out}")
        print(f"[reanchor] #900 APPLIED + verified {len(cameras)} camera(s)")
        return 0
    except SystemExit:
        raise
    except Exception as e:  # noqa: BLE001 -- an apply/read-back failure must FAIL LOUD (exit 1)
        print(f"ERROR: #900 re-anchor apply failed: {e}", file=sys.stderr)
        return 1
    finally:
        try:
            ws.close()
        except Exception:  # noqa: BLE001
            pass


if __name__ == "__main__":
    sys.exit(main())
