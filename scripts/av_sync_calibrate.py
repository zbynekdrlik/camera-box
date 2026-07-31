#!/usr/bin/env python3
"""#427 (#188 Task 6) — A/V-sync auto-set controller: measured offset -> genlock video-delay,
applied over the OBS WebSocket, with the calibrated absolute value PERSISTED.

What is landed vs what this adds:
  - MEASUREMENT (PR #397): `recording-verdict --av-sync` (src/bin/recording-verdict.rs) decodes
    the cam2 QPSK marker + audio track and prints a JSON with a top-level `av_offset_ms` field.
    The sign+clamp controller math it documents is ALREADY the pure Tier-0 kernel
    `camera_box::qpsk_marker::required_delay_ms` (src/qpsk_marker.rs) — already locked by its own
    Rust unit test (`required_delay_sign_and_clamp`). This script does NOT duplicate that kernel;
    it MIRRORS it in Python (Rust and Python can't share code across the WS boundary) and keeps
    the two in lock-step (see `required_delay_ms` below — same formula, same test values).
  - CONTROLLER (this script, the actual gap #427 closes): nothing turned the measured offset into
    an applied genlock video-delay, and nothing persisted the calibrated absolute value. This
    script reads the offset, computes the new delay, applies it to the stream 'NDI 2ME PGM'
    source over the OBS WebSocket (reusing the obs_phase2.py connection/RPC helpers), verifies
    via read-back, and on success persists {source, offset_ms, applied_latency_ms, ts} to
    av-sync-last.json for the #188 OBS dock (Task 8) and the #390 drift-guard pin to read.

Sign convention: `offset_ms = video_time - audio_time`; positive = video LAGS audio -> REDUCE the
genlock video-delay so the video catches up.

Apply safety (#358 pattern): the PRE-CHANGE latency is read first (the snapshot). After
SetInputSettings, a GetInputSettings read-back MUST match what was set; on a mismatch (the #292
force-drain class) the source is ROLLED BACK to the pre-change value and the run FAILS LOUD — the
source is never left half-set.

Usage:
    av_sync_calibrate.py --host <ip> [--password P] [--source "NDI 2ME PGM"] \
        (--offset-ms <f> | --verdict-json <path to recording-verdict --av-sync JSON output>) \
        [--apply] [--json-path <path>]

Without --apply this is a DRY RUN: prints the plan, changes nothing on the OBS box.
"""
import argparse
import json
import os
import sys
import time
from pathlib import Path

# Reuse the proven obs-websocket v5 connection + RPC helpers (obs_burn_filter.py convention).
from obs_phase2 import _conn, _rpc  # noqa: E402

# OBS property name for the per-source genlock latency (PROP_GENLOCK_LATENCY_MS_SRC in
# ndi-source.cpp, DistroAV fork — the "Latency (ms)" slider per source).
GENLOCK_SRC_LATENCY_KEY = "genlock_latency_ms_src"

# DistroAV clamp range: PROP_GENLOCK_LATENCY_MS_MIN=3, PROP_GENLOCK_SOURCE_LATENCY_MS_MAX=2000.
LATENCY_MIN = 3
LATENCY_MAX = 2000

# #871: the max ms one run's required_delay_ms() may move the applied latency AWAY from its
# current value, applied BEFORE the LATENCY_MIN/MAX hardware clamp. Env-overridable so the
# operator keeps the knob without a rebuild. Default 50ms: damage-limits a single untrustworthy
# measurement (a delivery defect corrupting the video timebase, #707) to an inaudible shift that
# self-corrects on the next good run, rather than jumping the whole raw distance in one step (the
# incident this fixes moved genlock_latency_ms_src 920 -> 1845ms in ONE run).
AV_SYNC_MAX_STEP_MS = int(os.environ.get("AV_SYNC_MAX_STEP_MS", "50"))

DEFAULT_SOURCE = "NDI 2ME PGM"

# Canonical Windows-side destination this script's payload MUST end up at for the #390
# drift-guard `av_sync_calibrated_ms` best-effort cross-check to read it (see
# `.claude/commands/drift-guard.md` step 1e / `vendor/README.md`'s genlock_source_latency row).
REMOTE_PROGRAMDATA_JSON_PATH = r"C:\ProgramData\camera-box\av-sync-last.json"

# Known rig hosts -> their win-* MCP tool name (obs-self-heal-install.sh's --box mapping).
# Only used to make the printed push plan concrete/copy-pasteable; an unrecognized host still
# gets a usable plan (destination + content), just without a resolved MCP tool name.
_KNOWN_MCP_HOSTS = {
    "10.77.9.202": "win-strih",
    "10.77.9.204": "win-stream-snv",
}


def mcp_name_for_host(host: str) -> "str | None":
    """Resolve a rig host IP to its win-* MCP tool name, or None if not a known rig box."""
    return _KNOWN_MCP_HOSTS.get(host)


def remote_push_plan(host: str, payload: dict) -> str:
    """#465 -- an explicit, copy-pasteable plan to place `payload` on the stream box's
    ProgramData, for the operator/agent to execute via the win-* MCP FileWrite tool.

    Why this exists instead of the script doing the push itself: `av_sync_calibrate.py`
    connects to `--host` over the OBS WebSocket and does NOT need to run ON that box, so
    `default_last_json_path()` normally falls back to a LOCAL path (no PROGRAMDATA env var on a
    Linux control host) that nothing on the stream box can read -- confirmed live on #465 (no
    av-sync-last.json anywhere under C:\\ProgramData\\camera-box on the stream box after an
    off-box --apply run). scp/ssh to the Windows boxes was historically believed DENIED on this
    rig (`recording-fetch-windows.sh`, `obs-self-heal-install.sh` use the same PLAN convention);
    #701 proved plain scp/ssh actually reaches strih/stream with the targets.md creds, but this
    script has no ssh/MCP access of its own (or MCP access at all) -- it just prints the plan. So
    instead of silently leaving an unreachable local file, print the exact
    destination + content (same PLAN convention as `obs-self-heal-install.sh`) so the caller
    can paste it straight into a FileWrite call.
    """
    mcp = mcp_name_for_host(host)
    mcp_line = mcp if mcp else "<unknown host -- resolve the win-* MCP tool manually>"
    content = json.dumps(payload, indent=2)
    return (
        "[av-sync] REMOTE PUSH REQUIRED -- this file was persisted LOCALLY, not on the OBS box.\n"
        "[av-sync]   this script has no MCP/ssh access -- push it via the win-* MCP FileWrite tool:\n"
        f"[av-sync]   host={host}  mcp={mcp_line}\n"
        f"[av-sync]   dest={REMOTE_PROGRAMDATA_JSON_PATH}\n"
        f"[av-sync]   content:\n{content}"
    )


def default_last_json_path() -> Path:
    """Where the controller persists the last-applied calibration for the #188 OBS dock
    (vendor/av-sync-dock, Task 8) and the #390 drift-guard pin to read.

    On the real Windows OBS box, %PROGRAMDATA% resolves to `C:\\ProgramData` and the file lands
    at `C:\\ProgramData\\camera-box\\av-sync-last.json` — the same `camera-box` ProgramData
    directory other rig tooling already uses (e.g. obs-self-heal.ps1). When PROGRAMDATA is unset
    (dev/test off-rig, e.g. this script run from a Linux control host), falls back to a local
    path under the user's home directory so this stays fully testable off-rig. Override with
    --json-path when the write needs to land somewhere else (e.g. a rig runner that copies the
    result onto the OBS box's ProgramData over a separate channel).
    """
    programdata = os.environ.get("PROGRAMDATA")
    if programdata:
        return Path(programdata) / "camera-box" / "av-sync-last.json"
    return Path.home() / ".camera-box" / "av-sync-last.json"


def required_delay_ms(current_delay_ms: int, offset_ms: float) -> int:
    """Required genlock video-delay to zero the measured offset.

    MIRRORS `camera_box::qpsk_marker::required_delay_ms` (src/qpsk_marker.rs) EXACTLY — same
    sign, same step clamp, same hardware clamp, same rounding. Keep the two in lock-step; do not
    diverge.

    `offset_ms = video_time - audio_time`; positive (video lags) -> REDUCE the delay. #871: the
    per-run STEP is clamped to `current_delay_ms +/- AV_SYNC_MAX_STEP_MS` BEFORE the DistroAV
    hardware genlock range [3, 2000] ms clamp -- a single run may only move the applied latency by
    a small correction, never the whole raw distance in one step. When the step clamp actually
    bites, prints a LOUD line (raw target, applied value, remaining residual) to stderr so a
    persistent large residual is never silent.

    #871 [red]: deliberate stub -- AV_SYNC_MAX_STEP_MS is NOT yet applied, exactly today's bug
    behavior (only the hardware [3, 2000] range is enforced). The [green] commit implements the
    step clamp.
    """
    raw = round(current_delay_ms - offset_ms)
    return max(LATENCY_MIN, min(LATENCY_MAX, raw))


def offset_from_verdict_json(path: str) -> float:
    """Read the measured `av_offset_ms` from a `recording-verdict --av-sync` JSON.

    This is the ACTUAL top-level field `run_av_sync` (src/bin/recording-verdict.rs) prints — not
    nested under an "av_sync" key. Fails LOUD (SystemExit) if the field is missing or null: an
    unresolved measurement must never silently be treated as a zero offset.
    """
    with open(path) as f:
        j = json.load(f)
    offset = j.get("av_offset_ms")
    if offset is None:
        raise SystemExit(
            f"[av-sync] {path}: no 'av_offset_ms' -- measurement UNRESOLVED, refusing to guess"
        )
    return float(offset)


def read_current_latency(ws, source: str) -> int:
    """Read the CURRENT genlock_latency_ms_src on `source` (the pre-change snapshot)."""
    settings = _rpc(ws, "GetInputSettings", {"inputName": source}).get("inputSettings", {})
    current = int(settings.get(GENLOCK_SRC_LATENCY_KEY, LATENCY_MIN))
    print(f"[av-sync] source='{source}' current genlock_latency_ms_src={current}ms")
    return current


def apply_latency(ws, source: str, current_ms: int, new_ms: int) -> int:
    """Set genlock_latency_ms_src=`new_ms` on `source`, verify via read-back (#358 pattern).

    On a read-back mismatch (the #292 force-drain class, where a configured value never actually
    takes), ROLLS BACK to `current_ms` and FAILS LOUD — the source is never left half-set. If even
    the rollback read-back mismatches, prints a LOUD warning (manual check required) before still
    failing loud, mirroring the #358 `_restore_test_latency` pattern.
    """
    print(f"[av-sync] SET '{source}' {GENLOCK_SRC_LATENCY_KEY}: {current_ms} -> {new_ms}")
    _rpc(ws, "SetInputSettings", {
        "inputName": source,
        "inputSettings": {GENLOCK_SRC_LATENCY_KEY: new_ms},
        "overlay": True,
    })
    back = _rpc(ws, "GetInputSettings", {"inputName": source}).get("inputSettings", {})
    actual = back.get(GENLOCK_SRC_LATENCY_KEY)
    if actual == new_ms:
        print(f"[av-sync] VERIFIED '{source}' {GENLOCK_SRC_LATENCY_KEY}={actual}")
        return actual

    sys.stderr.write(
        f"[av-sync] read-back mismatch on '{source}': set {new_ms}, got {actual!r} -- "
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
            f"[av-sync] WARN rollback ALSO mismatched on '{source}': expected {current_ms}, "
            f"got {rollback_actual!r} -- manual check required!\n"
        )
    raise SystemExit(
        f"[av-sync] FAILED to apply {GENLOCK_SRC_LATENCY_KEY}={new_ms} on '{source}' "
        f"(read-back={actual!r}); rolled back to {current_ms} "
        f"(rollback read-back={rollback_actual!r}) -- source never left half-set"
    )


def write_last_json(json_path: Path, source: str, offset_ms: float, applied_latency_ms: int) -> dict:
    """Persist the calibrated absolute value: {source, offset_ms, applied_latency_ms, ts}.

    Read by the #390 drift-guard `av_sync_calibrated_ms` best-effort pin to track the calibrated
    value instead of a stale hardcoded constant (see `.claude/commands/drift-guard.md` step 1e).
    Written atomically (write-tmp + replace) so a reader never observes a partial file. Returns
    the persisted payload so the caller can also feed it to `remote_push_plan()` when this write
    landed on a local (off-box) path instead of the real stream-box ProgramData.
    """
    json_path.parent.mkdir(parents=True, exist_ok=True)
    payload = {
        "source": source,
        "offset_ms": offset_ms,
        "applied_latency_ms": applied_latency_ms,
        "ts": time.time(),
    }
    tmp = json_path.with_suffix(json_path.suffix + ".tmp")
    tmp.write_text(json.dumps(payload, indent=2))
    tmp.replace(json_path)
    print(f"[av-sync] persisted {json_path}: {payload}")
    return payload


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default="")
    ap.add_argument("--source", default=DEFAULT_SOURCE)
    group = ap.add_mutually_exclusive_group(required=True)
    group.add_argument("--offset-ms", type=float, help="measured offset in ms (video - audio)")
    group.add_argument(
        "--verdict-json", type=str,
        help="path to a `recording-verdict --av-sync` JSON output to read av_offset_ms from",
    )
    ap.add_argument("--apply", action="store_true", help="actually set (default: dry-run)")
    ap.add_argument(
        "--json-path", type=str, default=None,
        help="override the av-sync-last.json write path "
             "(default: %%PROGRAMDATA%%/camera-box/av-sync-last.json)",
    )
    args = ap.parse_args()

    offset = (
        args.offset_ms if args.offset_ms is not None
        else offset_from_verdict_json(args.verdict_json)
    )

    ws = _conn(args.host, args.password)
    current = read_current_latency(ws, args.source)
    new_ms = required_delay_ms(current, offset)
    print(
        f"[av-sync] source='{args.source}' current={current}ms offset={offset:.1f}ms "
        f"-> new={new_ms}ms"
    )

    if not args.apply:
        print("[av-sync] dry-run (pass --apply to set)")
        return

    applied = apply_latency(ws, args.source, current, new_ms)
    used_default_path = args.json_path is None
    json_path = Path(args.json_path) if args.json_path else default_last_json_path()
    payload = write_last_json(json_path, args.source, offset, applied)
    print(
        f"[av-sync] APPLIED + verified: '{args.source}' {GENLOCK_SRC_LATENCY_KEY}={applied}; "
        f"persisted {json_path}"
    )

    # #465: when this landed on the LOCAL off-box fallback (no PROGRAMDATA -> we are not
    # running ON the Windows box, and the caller did not take control via --json-path), nothing
    # on the stream box can read it yet -- print the remote push plan so the operator/agent
    # completes the transfer via the win-* MCP FileWrite tool.
    if used_default_path and os.environ.get("PROGRAMDATA") is None:
        print(remote_push_plan(args.host, payload))


if __name__ == "__main__":
    main()
