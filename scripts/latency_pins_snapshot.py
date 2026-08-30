#!/usr/bin/env python3
"""#756 Member 3 -- live per-source genlock latency pins snapshot + recommended pins.

The user's REPEATED request, verbatim: "vysledok syncu a nastavenych latencii pre jednotlive cam
sourcy... sucastou velkeho e2e testu kde sa... prepinaju sceny v strih obs aby sa mohla overit a
vypocitat latencia pre kazdy source" -- the fused E2E's own program scene-switching already
measures every source's delivery; this script turns that into the per-source latency calibrator
the Discord report (scripts/e2e_discord_report.py, _section_latency_pins) surfaces every run.

Gathers, over OBS WebSocket -- LIVE, never hardcoded -- the CURRENTLY-CONFIGURED
genlock_latency_ms_src for every strih 'NDI cam<N>' + 'MV NDI cam<N>' and imag 'NDI CAM<N>' +
'MV CAM<N>', for N in CAMERA_ACTIVE_SET (#893 -- env var, default cam1/cam2/cam4 (#898:
cam3 retired 2026-07-31, grabber card destroyed); NEVER a literal N=1..7 range, see
.claude/rules/camera-active-set.md -- a retired camera's stale pin must never be swept/reported
as if it meant anything), the stream 'NDI 2ME PGM' hold, reads
~/.camera-box/av-sync-last.json for the source-of-truth applied hold, and computes a RECOMMENDED
pin set from THIS run's own delivery-latency table by shelling out to the SAME compiled
phase-sync-gate binary phase_sync_calibrate.py (#286/#438) already uses -- one formula
(the slowest source pinned at the 3ms floor, every other source held back by exactly how much
earlier it would otherwise present), never re-derived here.

Output: a single JSON written to --out, consumed by e2e_discord_report.py via a NEW
`meta["pins"]` key the caller (scripts/lib/e2e-discord-report.sh) reads from this file -- see
_section_latency_pins's own docstring in e2e_discord_report.py for the exact shape.

A box/host that is unreachable, or a source that has no genlock_latency_ms_src key yet,
contributes an honest N/A (None) -- never a silently-defaulted or fabricated value. This
mirrors set-ndi-mapping.py / phase_sync_calibrate.py's own honesty conventions.

Usage:
  python3 scripts/latency_pins_snapshot.py \
      --strih-host 10.77.9.202 --imag-host 10.77.9.182 --stream-host 10.77.9.204 \
      --verdict-json path/to/verdict-<run_id>.json --out path/to/pins-<run_id>.json \
      [--password PW] [--gate-bin path/to/phase-sync-gate]
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

from obs_phase2 import _conn, _rpc  # noqa: E402
from phase_sync_calibrate import compute_phase_sync_offsets  # noqa: E402

GENLOCK_SRC_LATENCY_KEY = "genlock_latency_ms_src"


def active_camera_numbers() -> tuple:
    """#893 -- the camera numbers to sweep, derived from CAMERA_ACTIVE_SET (env var, default
    "cam1 cam2 cam3 cam4 cam5 cam6 cam7" -- issue 1198 (2026-08-27): cam1 + cam2 RESTORED, both
    healthy on a live journal check, owner refused the physical card swap; issue 1216
    (2026-08-28): cam5/cam6/cam7 also restored, bigger splitter fitted; issue 1217 (same day):
    cam5 OUT again -- a DEAD_PORT leg on the new splitter (flat static frame, siblings cam6/cam7
    read colour); issue 1216 completion (2026-08-30, owner directive "kamery od 1-7 bezia" after
    a physical cable reseat): cam4 (#947) and cam5 (DEAD_PORT) BOTH rejoin -- the full
    seven-camera fleet -- same read convention set-ndi-mapping.py's DEFAULT_ACTIVE_SET already
    uses), NEVER a literal N=1..7 range (.claude/rules/camera-active-set.md).

    This used to be a hardcoded `CAMERAS = (1, 2, 3, 4, 5, 6, 7)` tuple -- the exact bug shape
    that let this script sweep+report RETIRED cameras' pins (cam5/6/7) as if they meant
    anything, which is precisely what masked #893's live regression (a stale pin on retired
    cam5 sitting at the floor while every camera actually in CAMERA_ACTIVE_SET had drifted
    upward). Read FRESH on every call (never cached at import time) so a caller/test can
    override the env var per-invocation.
    """
    raw = os.environ.get("CAMERA_ACTIVE_SET", "cam1 cam2 cam3 cam4 cam5 cam6 cam7")
    out = []
    for tok in raw.replace(",", " ").split():
        tok = tok.strip()
        if tok.startswith("cam") and tok[3:].isdigit():
            out.append(int(tok[3:]))
    return tuple(out) if out else (1, 2, 3, 4)


def av_sync_last_path() -> Path:
    """Same resolution av_sync_calibrate.py uses (PROGRAMDATA on Windows boxes, ~/.camera-box on
    dev1/Linux) -- the source-of-truth applied stream hold this script reads (read-only)."""
    programdata = os.environ.get("PROGRAMDATA")
    if programdata:
        return Path(programdata) / "camera-box" / "av-sync-last.json"
    return Path.home() / ".camera-box" / "av-sync-last.json"


def read_pin(ws, source_name: str) -> "int | None":
    """Read genlock_latency_ms_src for `source_name` over an already-connected WS -- None if the
    source or the key itself is missing (honest N/A, never a silently-defaulted floor value)."""
    try:
        settings = _rpc(ws, "GetInputSettings", {"inputName": source_name}, ignore_err=True)
    except Exception as e:  # noqa: BLE001 -- any RPC/network failure -> honest N/A, never abort
        print(f"WARNING: latency_pins_snapshot: GetInputSettings({source_name!r}) failed: {e}", file=sys.stderr)
        return None
    if not isinstance(settings, dict):
        return None
    inp = settings.get("inputSettings", {})
    v = inp.get(GENLOCK_SRC_LATENCY_KEY) if isinstance(inp, dict) else None
    return int(v) if isinstance(v, (int, float)) else None


def snapshot_box_pins(host: str, password: str, main_fmt: str, mv_fmt: str) -> dict:
    """Connect to `host`, read main+MV pins for every camera in `active_camera_numbers()` (#893
    -- CAMERA_ACTIVE_SET, never a literal cam1..7 range) using the given name templates (e.g.
    "NDI cam{n}" / "MV NDI cam{n}" for strih, "NDI CAM{n}" / "MV CAM{n}" for imag). Returns
    {"cam<n>": {"main_ms": v_or_None, "mv_ms": v_or_None}, ...}, or {} on a connect failure --
    never a half-filled table that looks more complete than what was actually read."""
    out = {}
    try:
        ws = _conn(host, password)
    except Exception as e:  # noqa: BLE001
        print(f"WARNING: latency_pins_snapshot: could not connect to {host}: {e}", file=sys.stderr)
        return out
    try:
        for n in active_camera_numbers():
            main = read_pin(ws, main_fmt.format(n=n))
            mv = read_pin(ws, mv_fmt.format(n=n))
            out[f"cam{n}"] = {"main_ms": main, "mv_ms": mv}
    finally:
        ws.close()
    return out


def read_stream_hold(host: str, password: str) -> "int | None":
    try:
        ws = _conn(host, password)
    except Exception as e:  # noqa: BLE001
        print(f"WARNING: latency_pins_snapshot: could not connect to stream {host}: {e}", file=sys.stderr)
        return None
    try:
        return read_pin(ws, "NDI 2ME PGM")
    finally:
        ws.close()


def load_av_sync_last() -> dict:
    p = av_sync_last_path()
    if not p.exists():
        return {}
    try:
        with open(p, encoding="utf-8") as f:
            data = json.load(f)
            return data if isinstance(data, dict) else {}
    except (json.JSONDecodeError, OSError) as e:
        print(f"WARNING: latency_pins_snapshot: could not read {p}: {e}", file=sys.stderr)
        return {}


def delivery_p50_table(verdict: dict) -> dict:
    """Pure: {camN: p50_ms} from the fused verdict's all_cambox_delivery_latency block -- empty
    dict when the block is absent/empty (never a fabricated measurement). `verdict` is the
    ALREADY-PARSED dict (not a path) so this stays independently unit-testable."""
    block = verdict.get("all_cambox_delivery_latency") or {}
    out = {}
    if not isinstance(block, dict):
        return out
    for cam, node in block.items():
        if not isinstance(node, dict) or not cam.startswith("cam"):
            continue
        p50 = node.get("p50_ms")
        if isinstance(p50, (int, float)):
            out[cam] = float(p50)
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--strih-host", default=None)
    ap.add_argument("--imag-host", default=None)
    ap.add_argument("--stream-host", default=None)
    ap.add_argument("--password", default="")
    ap.add_argument("--verdict-json", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--gate-bin", default=None)
    args = ap.parse_args(argv)

    result: dict = {}
    if args.strih_host:
        result["strih"] = snapshot_box_pins(args.strih_host, args.password, "NDI cam{n}", "MV NDI cam{n}")
    if args.imag_host:
        result["imag"] = snapshot_box_pins(args.imag_host, args.password, "NDI CAM{n}", "MV CAM{n}")
    if args.stream_host:
        result["stream_hold_active_ms"] = read_stream_hold(args.stream_host, args.password)

    result["av_sync_last"] = load_av_sync_last()

    with open(args.verdict_json, encoding="utf-8") as f:
        verdict = json.load(f)
    p50_table = delivery_p50_table(verdict)
    try:
        result["recommended_pins_ms"] = compute_phase_sync_offsets(p50_table, args.gate_bin) if p50_table else {}
    except SystemExit as e:
        print(f"WARNING: latency_pins_snapshot: recommended-pins computation failed: {e}", file=sys.stderr)
        result["recommended_pins_ms"] = {}

    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(result, f, indent=2)
    print(f"latency_pins_snapshot: wrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
