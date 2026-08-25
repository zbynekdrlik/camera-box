#!/usr/bin/env python3
"""#761 -- per-camera MV-clone-vs-main presentation-skew snapshot for the full-path E2E report.

WHAT it measures: for every active camera, the skew between what the operator sees in the multiview
cell (OBS scene "MV Cam N") and the program (scene "Cam N"). Both scenes carry the painter's QR
burn via the optical loop (one camera -> splitter -> every box), so each screenshot's frame identity
is decodable as `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}` (src/probe/payload.rs). `gen_ts_ns` is the
exact per-frame emission instant from ONE painter clock -> a direct ns->ms skew, no frame-rate
assumption and no cross-box clock skew.

WHY local-wall-gap compensation (the core technique -- calibrated LIVE on the rig, #761 2026-08-16):
capturing two screenshots is NOT simultaneous -- the WS round-trip advances the live NDI frame by a
wall gap between the two GetSourceScreenshot calls, which alone dwarfs the real skew (an uncompensated
run read a false -695ms on a shared-source, truly-~0 rig). The frame LATCHES essentially when OBS
RECEIVES the request (render is fast; the ~0.5s of RPC is PNG readback+transfer AFTER the latch), so
each capture's latch instant is timestamped locally at REQUEST-SEND (`t_send`, dev1 monotonic clock).
For a (main, MV) pair sharing a painter run_id:

    skew_ms = (gen_ts_main - gen_ts_mv) / 1e6  +  (t_send_mv - t_send_main) / 1e6

The gen_ts delta encodes `S - (t_latch_mv - t_latch_main)`; adding back the locally-measured wall gap
recovers S directly, regardless of the two scenes' very different (and variable) screenshot costs.
Median over many alternating-order samples. Live-validated floor: median within +-5ms of the true ~0
with ~10ms stdev at 960x540 -- so the 1-frame (16.7ms @60fps) alarm threshold is genuinely
resolvable. (Timestamping the RPC MIDPOINT instead of t_send was the original error source: it put
the noise floor at 50-180ms because the asymmetric PNG-readback time leaked in uncancelled.)

RESCOPE (#761 validation 2026-08-16): both strih and imag have converged to a SHARED-SOURCE
arrangement (scene "MV Cam N" references the SAME `NDI CAM{n}` input as "Cam N"; no separate low-bw
clone input exists), so the expected skew is ~0 and this snapshot is a REGRESSION GUARD -- exactly
the role the owner predefined for a shared-source box (comment 2026-07-15). It still measures a real
lag if a separate low-bw clone is ever re-introduced (an imag experiment, or #763's derived stream).

DESIGN (#756 Member 3 split, mirrored): this file is BOTH the impure gatherer (WS screenshots + cv2
decode + write JSON) AND the pure decision logic (parse/CRC/compensated-skew/flag -- unit-tested with
NO rig in tests/python/test_mv_skew_snapshot.py). The report side is a PURE formatter,
e2e_discord_report.py's `_section_mv_skew`, reading this file's JSON via `--mv-skew-json`. Heavy deps
(cv2/numpy/obs_phase2) are imported LOCALLY inside the I/O functions so the pure logic (and its tests)
never require them.

Usage:
  python3 scripts/mv_skew_snapshot.py --host 10.77.9.182 --password newlevel \
      --out /tmp/mv-skew-<run_id>.json [--rounds 8] [--width 960] [--height 540]
"""
from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
import time
import zlib
from datetime import datetime, timezone

# 60fps frame period in ms -- the "1 frame" alarm threshold (#761 spec: flag |skew| > 16.7ms).
FRAME_MS_60 = 1000.0 / 60.0


# --------------------------------------------------------------------------- #
# PURE decision logic (no I/O -- unit-tested in tests/python/test_mv_skew_snapshot.py)
# --------------------------------------------------------------------------- #
def parse_payload(qr_text: str) -> "tuple[int, int, int] | None":
    """Decode a painter QR wire string `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}` (src/probe/
    payload.rs) into `(run_id, frame_id, gen_ts_ns)`, validating the CRC-32 (ISO-HDLC == the
    standard CRC-32 == `zlib.crc32`, verified against live rig samples). Returns None on any
    malformed input or CRC mismatch -- a cv2 QR misread never becomes a fabricated tick."""
    if not qr_text or not qr_text.startswith("P"):
        return None
    parts = qr_text[1:].split(".")
    if len(parts) != 4:
        return None
    try:
        run_id = int(parts[0])
        frame_id = int(parts[1])
        gen_ts_ns = int(parts[2])
        crc = int(parts[3])
    except ValueError:
        return None
    body = f"{run_id}.{frame_id}.{gen_ts_ns}"
    if (zlib.crc32(body.encode()) & 0xFFFFFFFF) != crc:
        return None
    return (run_id, frame_id, gen_ts_ns)


def tick_map(qr_texts: "list[str]") -> "dict[int, int]":
    """{run_id: newest gen_ts_ns} across all decodable QRs in one screenshot. The painter emits a
    dual-QR (two frame_ids per frame, Vernier); taking the NEWEST gen_ts_ns per run_id is a single,
    consistent tick for that screenshot -- and being consistent is all that matters. Undecodable /
    CRC-bad QRs are dropped."""
    out: "dict[int, int]" = {}
    for text in qr_texts:
        p = parse_payload(text)
        if p is None:
            continue
        run_id, _frame_id, gen_ts_ns = p
        if run_id not in out or gen_ts_ns > out[run_id]:
            out[run_id] = gen_ts_ns
    return out


# The reserved node ids that must NEVER be auto-detected as "the painter" -- a mirror of
# qr_align_pins.NODE_BURN_RUN_IDS (itself mirroring src/probe/recording.rs::NODE_BURN_RUN_IDS).
# 911001-911012 are the digital node burns (#1159 class: universal-on-strih under E2E, id far
# below the painter's ~1.8e9 epoch, so they win a count tie via the smallest-id tie-break).
# 911013 (issue 1196) is the PAINTED aux Vernier tick pair: universal on EVERY screenshot even
# outside E2E burns, and its constant gen_ts_ns=0 would turn every skew sample into pure
# wall-gap garbage -- it must never be picked as the painter NOR used as a common-sample id.
RESERVED_RUN_IDS = frozenset({
    911001, 911002, 911003, 911004, 911007, 911008, 911009,
    911010, 911011, 911012, 911013,
})


def dominant_run_id(tick_maps: "list[dict]") -> "int | None":
    """The run_id present in the MOST screenshots -- the universal painter dual-QR (same run_id on
    every camera via the one-camera->splitter optical loop), as opposed to a camera-local burn
    (e.g. the cam1-burn's own run_id, present on cam1 only). The RESERVED ids (node burns + the
    issue-1196 aux tick pair, RESERVED_RUN_IDS) are excluded FIRST -- they are never the painter,
    and the universal aux marks would otherwise tie the painter and win the smallest-id
    tie-break (the #1159 class). Ties break to the smallest run_id for determinism. None when no
    non-reserved run_id was decoded anywhere."""
    counts: "dict[int, int]" = {}
    for m in tick_maps:
        for run_id in m:
            if run_id in RESERVED_RUN_IDS:
                continue
            counts[run_id] = counts.get(run_id, 0) + 1
    if not counts:
        return None
    return min(counts, key=lambda r: (-counts[r], r))


def pick_common_run_id(main_map: dict, mv_map: dict, preferred: "int | None") -> "int | None":
    """A run_id present in BOTH screenshots: `preferred` (the universal painter run_id) when it is
    common to both, otherwise the smallest common run_id. The issue-1196 aux tick pair
    (911013) is dropped from `common` outright -- its gen_ts_ns is a constant 0, so a "skew
    sample" from it would be pure wall-gap, never a real measurement (the node burns stay
    eligible as a fallback: their gen_ts is a real per-node render clock). None when the two
    share no usable decoded run_id -- so the caller drops that sample honestly rather than
    fabricating one."""
    common = (set(main_map) & set(mv_map)) - {911013}
    if not common:
        return None
    return preferred if (preferred in common) else min(common)


def skew_sample_ms(gen_ts_main: int, gen_ts_mv: int, t_send_main_ns: int, t_send_mv_ns: int) -> float:
    """One local-wall-gap-compensated skew sample, in ms. `gen_ts_*` are painter-clock ns (same
    run_id, so the cross-clock offset cancels); `t_send_*` are dev1 monotonic ns at REQUEST-SEND
    (the frame's latch instant). Positive => the MV (multiview) frame is OLDER than the program
    frame, i.e. the operator sees this camera LATER than the program by that many ms."""
    return ((gen_ts_main - gen_ts_mv) + (t_send_mv_ns - t_send_main_ns)) / 1_000_000.0


def skew_ms_from_samples(samples_ms: "list[float]") -> dict:
    """Aggregate compensated skew samples -> median (robust to a slow-RPC outlier) + spread. The
    median_ms is None (never fabricated) when there are no usable samples."""
    if not samples_ms:
        return {"median_ms": None, "n_samples": 0, "samples_ms": [], "min_ms": None, "max_ms": None, "stdev_ms": None}
    return {
        "median_ms": round(statistics.median(samples_ms), 3),
        "n_samples": len(samples_ms),
        "samples_ms": [round(s, 3) for s in samples_ms],
        "min_ms": round(min(samples_ms), 3),
        "max_ms": round(max(samples_ms), 3),
        "stdev_ms": round(statistics.pstdev(samples_ms), 3) if len(samples_ms) > 1 else 0.0,
    }


def is_skew_alarming(median_ms: "float | None", frame_ms: float = FRAME_MS_60) -> bool:
    """True iff |median_ms| exceeds one frame (#761: 'strihač vidí kameru N o X ms neskôr než
    program'). None (unmeasured) is NOT alarming -- absence of a number is reported as N/A, never
    as a breach."""
    return median_ms is not None and abs(median_ms) > frame_ms


def finalize_camera_skew(
    captures: "list[tuple[str, dict, int]]",
    preferred_run_id: "int | None",
    frame_ms: float = FRAME_MS_60,
) -> dict:
    """Compose the tested primitives into one camera's result. `captures` is the alternating
    capture sequence -- each `(kind, tick_map, t_send_ns)` with `kind` in {"main","mv"}. Every
    ADJACENT cross-source pair (main->MV AND MV->main -- the order-alternation) sharing a run_id
    contributes one compensated sample; a leg with no common tick is skipped, not zero-filled.
    Pure -- no I/O."""
    samples: "list[float]" = []
    for i in range(len(captures) - 1):
        (ka, ma, ta), (kb, mb, tb) = captures[i], captures[i + 1]
        if ka == kb:
            continue
        (main_map, main_t), (mv_map, mv_t) = ((ma, ta), (mb, tb)) if ka == "main" else ((mb, tb), (ma, ta))
        rid = pick_common_run_id(main_map, mv_map, preferred_run_id)
        if rid is None:
            continue
        samples.append(skew_sample_ms(main_map[rid], mv_map[rid], main_t, mv_t))
    result = skew_ms_from_samples(samples)
    result["alarming"] = is_skew_alarming(result["median_ms"], frame_ms)
    result["run_id_used"] = preferred_run_id
    result["captures"] = len(captures)
    return result


# --------------------------------------------------------------------------- #
# IMPURE gatherer (WS + cv2 -- never reached by the pure unit tests)
# --------------------------------------------------------------------------- #
def _extract_png_bytes(image_data: "str | None") -> "bytes | None":
    """OBS `GetSourceScreenshot.imageData` (bare base64 or `data:image/png;base64,...`) -> raw PNG
    bytes; None for empty/missing. Mirrors qr_screenshot_check.extract_png_bytes exactly."""
    if not image_data:
        return None
    import base64

    b64 = image_data.split(",", 1)[1] if image_data.startswith("data:") else image_data
    return base64.b64decode(b64)


def _decode_qr_texts(png_bytes: bytes) -> "list[str]":
    """Every QR payload string in a PNG, via cv2.QRCodeDetector.detectAndDecodeMulti -- the SAME
    decoder scripts/qr_screenshot_check.py uses (no new runtime dependency). Local import so the
    pure logic (and its tests) never require cv2/numpy."""
    import cv2
    import numpy as np

    arr = np.frombuffer(png_bytes, dtype=np.uint8)
    img = cv2.imdecode(arr, cv2.IMREAD_COLOR)
    if img is None:
        return []
    found, decoded, _pts, _straight = cv2.QRCodeDetector().detectAndDecodeMulti(img)
    if not found:
        return []
    return [t for t in decoded if t]


def _capture(ws, source: str, width: int, height: int) -> "tuple[dict, int]":
    """One offscreen render of `source` -> (tick_map, t_send_ns). `t_send_ns` is dev1 monotonic ns
    captured JUST BEFORE the RPC (the frame's latch instant -- see the module header). Returns an
    empty tick_map on an RPC/transport/decode failure (honest miss, logged, never aborts the
    sweep)."""
    import obs_phase2

    t_send = time.monotonic_ns()
    try:
        res = obs_phase2._rpc(
            ws,
            "GetSourceScreenshot",
            {"sourceName": source, "imageFormat": "png", "imageWidth": width, "imageHeight": height},
            ignore_err=True,
        )
    except Exception as e:  # noqa: BLE001 -- best-effort per shot, logged (not silent), never abort
        print(f"WARNING: mv_skew_snapshot: GetSourceScreenshot({source!r}) failed: {e}", file=sys.stderr)
        return {}, t_send
    png = _extract_png_bytes(res.get("imageData") if isinstance(res, dict) else None)
    if png is None:
        print(f"WARNING: mv_skew_snapshot: no imageData for {source!r}", file=sys.stderr)
        return {}, t_send
    try:
        return tick_map(_decode_qr_texts(png)), t_send
    except Exception as e:  # noqa: BLE001 -- a cv2 decode failure is a per-shot miss, logged
        print(f"WARNING: mv_skew_snapshot: decode of {source!r} failed: {e}", file=sys.stderr)
        return {}, t_send


def measure_camera(
    ws, main_scene: str, mv_scene: str, rounds: int, width: int, height: int
) -> "list[tuple[str, dict, int]]":
    """Capture an alternating main,MV,main,MV,... sequence (`rounds` rounds) for one camera.
    Returns the capture list `[(kind, tick_map, t_send_ns), ...]` that finalize_camera_skew
    consumes -- the alternation supplies BOTH main->MV and MV->main adjacent samples."""
    captures: "list[tuple[str, dict, int]]" = []
    for _ in range(rounds):
        tm, ts = _capture(ws, main_scene, width, height)
        captures.append(("main", tm, ts))
        tm, ts = _capture(ws, mv_scene, width, height)
        captures.append(("mv", tm, ts))
    return captures


def snapshot(host: str, password: str, rounds: int, width: int, height: int) -> dict:
    """Connect to imag, measure every camera in CAMERA_ACTIVE_SET that has BOTH a "Cam N" and an
    "MV Cam N" scene, and return the report dict. A connect failure or a camera whose scenes are
    absent contributes an honest note/None -- never a half-filled table."""
    import obs_phase2

    # DRY: reuse the ONE CAMERA_ACTIVE_SET convention (never a literal cam1..7 range,
    # .claude/rules/camera-active-set.md). Local import keeps the pure logic cv2/ws-free.
    from latency_pins_snapshot import active_camera_numbers

    result: dict = {
        "host": host,
        "frame_ms": FRAME_MS_60,
        "rounds_requested": rounds,
        "resolution": f"{width}x{height}",
        "measured_at": datetime.now(timezone.utc).isoformat(),
        "method": (
            "order-alternated paired GetSourceScreenshot of scene 'Cam N' vs 'MV Cam N', painter "
            "QR gen_ts_ns decode, local-wall-gap (t_send) compensation; median per camera. "
            "Live floor ~+-5ms median / ~10ms stdev @960x540 (#761)."
        ),
        "note": (
            "main-scene-vs-MV-scene presentation skew. Shared-source arrangement => expect ~0 "
            "(regression guard); |median| > 1 frame (16.7ms) => the multiview cell the operator "
            "sees presents at a different time than the program (#761)."
        ),
        "cameras": {},
    }
    try:
        ws = obs_phase2._conn(host, password)
    except Exception as e:  # noqa: BLE001
        print(f"WARNING: mv_skew_snapshot: could not connect to imag {host}: {e}", file=sys.stderr)
        result["error"] = f"connect failed: {e}"
        return result

    try:
        try:
            sl = obs_phase2._rpc(ws, "GetSceneList", {}, ignore_err=True)
            scenes_present = {s.get("sceneName", "") for s in (sl.get("scenes") or [])} if isinstance(sl, dict) else set()
        except Exception as e:  # noqa: BLE001
            print(f"WARNING: mv_skew_snapshot: GetSceneList failed: {e}", file=sys.stderr)
            scenes_present = set()

        all_maps: list = []
        raw: dict = {}
        for n in active_camera_numbers():
            cam = f"cam{n}"
            main_scene = f"Cam {n}"
            mv_scene = f"MV Cam {n}"
            if scenes_present and not (main_scene in scenes_present and mv_scene in scenes_present):
                print(
                    f"WARNING: mv_skew_snapshot: {cam}: missing scene "
                    f"({main_scene!r} and/or {mv_scene!r} absent) -- skipping (honest N/A).",
                    file=sys.stderr,
                )
                result["cameras"][cam] = {"median_ms": None, "n_samples": 0, "note": "scene(s) absent"}
                continue
            captures = measure_camera(ws, main_scene, mv_scene, rounds, width, height)
            raw[cam] = (captures, main_scene, mv_scene)
            all_maps.extend(tm for _kind, tm, _ts in captures)

        preferred = dominant_run_id(all_maps)
        result["preferred_run_id"] = preferred
        for cam, (captures, main_scene, mv_scene) in raw.items():
            cam_res = finalize_camera_skew(captures, preferred)
            cam_res["main_scene"] = main_scene
            cam_res["mv_scene"] = mv_scene
            result["cameras"][cam] = cam_res
        return result
    finally:
        try:
            ws.close()
        except Exception as e:  # noqa: BLE001
            print(f"WARNING: mv_skew_snapshot: ws.close() failed: {e}", file=sys.stderr)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", default=os.environ.get("IMAG_IP", "10.77.9.182"))
    ap.add_argument("--password", default="")
    ap.add_argument("--out", required=True)
    ap.add_argument("--rounds", type=int, default=8, help="capture rounds/camera (each => main+MV; #761 wants >=4)")
    ap.add_argument("--width", type=int, default=960)
    ap.add_argument("--height", type=int, default=540)
    args = ap.parse_args(argv)

    # OBS-WebSocket password, OBS_PASSWORD-first — same convention as imag_latency_enforce.py and
    # the #756 pins snapshot (NOT the SSH box password IMAG_PW; imag's OBS WS is auth-less today,
    # so this only matters the day WS auth is enabled with OBS_PASSWORD — but aligning now avoids
    # this becoming the ONE imag-WS consumer that silently goes dark then, #761 review).
    password = os.environ.get("OBS_PASSWORD", args.password)
    result = snapshot(args.host, password, args.rounds, args.width, args.height)
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(result, f, indent=2)
    print(f"mv_skew_snapshot: wrote {args.out}")
    # Report-only: never a non-zero exit for a measured skew (the report surfaces it; #761 is
    # measurement + visibility, NOT a gate -- pin policy is a separate decision AFTER real numbers).
    return 0


if __name__ == "__main__":
    sys.exit(main())
