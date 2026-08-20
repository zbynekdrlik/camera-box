#!/usr/bin/env python3
"""#1003 -- floor-3 per-run camera aligner: measure the SIMULTANEOUS painter-QR spread across the
on-air strih inputs, derive floor-3 pins, apply, RE-MEASURE, and FAIL the run if it cannot align.

The owner's binding rework mandate (issue 1003, ODMIETNUTÉ + REVERTNUTÉ, 2026-08-20):
  1. The NAJPOMALŠIA (max-transport) camera gets pin 3 (floor); the others get 3 + their RELATIVE
     delivery delta. Alignment compensates only RELATIVE differences -- never absolute depth (the
     rejected 90/160/184 added ~180 ms of needless chain latency).
  2. Deltas must be RE-DERIVED robustly -- many simultaneous rounds, MEDIAN per camera, excluding
     undecodable / underrun outlier rounds (the rejected MEQ single delivery-p50 sample baked in a
     degraded cam1 grabber; a 94 ms delta between identical cards on one switch is nonsense).
  3. cam4 is on-air, so it MUST be in the alignment set (the offline-ack "outside-measured-set"
     covers only the E2E measurement sweep, NOT production alignment).
  4. Alignment is an AUTOMATIC per-run process: measure -> align (floor 3) -> verify -> FAIL if it
     cannot align.

WHY the painter QR is the signal, and why gen_ts_ns is exact:
  The painter QR wire string is `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}` (src/probe/payload.rs).
  ONE camera is optically split to every cam box, so the SAME painted frame reaches all boxes; a
  SIMULTANEOUS barrier `GetSourceScreenshot` of every strih input decodes a DIFFERENT frame per box,
  and gen_ts_ns (the painter's own per-frame emission timestamp, IDENTICAL across boxes for a given
  frame) encodes each box's delivery latency EXACTLY: a box showing an older gen_ts_ns is more
  delayed by that ns difference -- no frame-rate assumption. frame_id spread <= 1 is the owner's
  "ak spravím screenshot, musím vidieť rovnaké monotonic a time v KAŽDOM QR" parity gate.

The floor-3 model: per camera m_i = gen_ts_ns_i/1e6 + current_pin_i (ms). The MAX-transport camera
(slowest chain that owes it to transport, not to its own pin) has the MIN m_i. new_pin_i =
3 + (m_i - min_k m_k): the min-m camera floors to 3, every other gets 3 + its relative delta, so the
total latency equalizes at the MINIMUM (relative-only, never deep). Medianed across rounds for
robustness; a median delta above a sanity bound = a degraded/underrun card -> FAIL rather than ship
a deep pin.

DOMAINS (never crossed): this tool writes ONLY the strih per-source genlock_latency_ms_src pins for
the align set it is given. The stream `NDI 2ME PGM` hold (operator A/V-align domain) and imag's 3 ms
floor are NEVER touched -- they are simply never in the align set. Writes go through
apply_latency_pins.apply_pins (read-back-verified, fail-loud, idempotent).

Tier-0: the pure functions (pick_painter_tick, frame_id_spread, round_deltas, robust_deltas,
floor3_pins, sanity_ok, alignment_ok) do NO I/O and are unit-tested with no rig
(tests/python/test_qr_align_pins_1003.py). cv2/threading/obs plumbing is imported LOCALLY inside the
live functions so the pure logic (and its tests) never need a display or a rig.

CLI:
    qr_align_pins.py --host 10.77.9.202 --sources "NDI cam1,NDI cam2,NDI cam3,NDI cam4"  # DRY-RUN
    qr_align_pins.py --host 10.77.9.202 --sources "..." --execute                        # ALIGN
"""
from __future__ import annotations

import argparse
import json
import os
import statistics
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

# parse_payload is the PURE, CRC-validating painter-QR decoder already used by mv_skew_snapshot --
# reuse it (never a second copy). mv_skew_snapshot imports cv2/numpy LOCALLY, so this import is safe
# for the pure tests (no display needed).
from mv_skew_snapshot import parse_payload  # noqa: E402

DEFAULT_ROUNDS = 9
DEFAULT_MIN_VALID_ROUNDS = 5
DEFAULT_PARITY_TOL_IDS = 1
DEFAULT_MAX_DELTA_MS = 100.0  # a median relative delta above this = a degraded/underrun card
DEFAULT_FLOOR_MS = 3          # imag-min-latency floor; the slowest strih camera anchors here
DEFAULT_WIDTH = 1920
DEFAULT_HEIGHT = 1080
DEFAULT_VERIFY_ROUNDS = 5
DEFAULT_SETTLE_S = 4.0        # let the genlock FIFO re-lock after a pin change before re-measuring


class AlignmentImpossible(SystemExit):
    """Raised when the run CANNOT be aligned (too few decodable rounds, or a delta beyond the
    sanity bound). A SystemExit subclass so the CLI exits non-zero and the E2E step ABORTS with a
    named per-camera reason -- never a silent proceed on a misaligned rig."""


# ---------------------------------------------------------------------------
# PURE logic (no I/O, unit-tested with no rig)
# ---------------------------------------------------------------------------
def pick_painter_tick(qr_texts, run_id):
    """From one screenshot's decoded QR texts, return (frame_id, gen_ts_ns) of the MAX-frame_id
    painter payload matching `run_id` (the dual-QR "ber max" rule), or None when no valid matching
    payload decoded. CRC-bad / foreign-run / garbage QRs are dropped (parse_payload validates)."""
    best = None
    for text in qr_texts:
        p = parse_payload(text)
        if p is None:
            continue
        r, frame_id, gen_ts_ns = p
        if r != run_id:
            continue
        if best is None or frame_id > best[0]:
            best = (frame_id, gen_ts_ns)
    return best


def frame_id_spread(round_ticks):
    """max-min frame_id over the DECODED cameras in one round, or None when fewer than two decoded
    (parity is unverifiable with <2 samples). `round_ticks`: {source: (frame_id, gen_ts_ns)|None}."""
    fids = [tk[0] for tk in round_ticks.values() if tk is not None]
    if len(fids) < 2:
        return None
    return max(fids) - min(fids)


def round_deltas(round_ticks, current_pins):
    """Per-round relative ms delta per camera: m_i = gen_ts_ns_i/1e6 + current_pin_i, d_i = m_i -
    min(m) (>= 0). Returns {source: d_i} or None when the round is INCOMPLETE -- any camera
    undecoded, or any current pin unknown (an incomplete round cannot give a full cross-camera
    spread, so it is excluded rather than half-measured). Numerically stable: the absolute gen_ts_ns
    baseline is subtracted BEFORE the ns->ms divide, so no catastrophic float cancellation."""
    have = {}
    for src, tk in round_ticks.items():
        if tk is None:
            return None
        pin = current_pins.get(src)
        if pin is None:
            return None
        have[src] = (tk[1], pin)  # (gen_ts_ns, pin_ms)
    if not have:
        return None
    g0 = min(g for g, _ in have.values())
    m = {src: (g - g0) / 1e6 + pin for src, (g, pin) in have.items()}
    m0 = min(m.values())
    return {src: v - m0 for src, v in m.items()}


def robust_deltas(rounds, current_pins, min_valid_rounds=DEFAULT_MIN_VALID_ROUNDS):
    """MEDIAN relative delta per camera over the VALID (fully-decoded) rounds. A round with any
    undecoded camera is dropped; a single underrun outlier round is absorbed by the median. Returns
    ({source: median_delta}, n_valid). Raises AlignmentImpossible when fewer than `min_valid_rounds`
    valid rounds exist -- the rig cannot be measured, so it must not be blindly "aligned"."""
    per_src = {}
    n_valid = 0
    for rnd in rounds:
        d = round_deltas(rnd, current_pins)
        if d is None:
            continue
        n_valid += 1
        for src, val in d.items():
            per_src.setdefault(src, []).append(val)
    if n_valid < min_valid_rounds:
        raise AlignmentImpossible(
            f"[qr-align] only {n_valid} fully-decodable measurement round(s) "
            f"(need >= {min_valid_rounds}) -- cannot measure the cross-camera spread; "
            "the painter QR is not reliably readable on every on-air strih input.")
    return {src: statistics.median(vals) for src, vals in per_src.items()}, n_valid


def floor3_pins(deltas, floor_ms=DEFAULT_FLOOR_MS):
    """The floor-3 pin plan from per-camera relative deltas: new_pin_i = round(floor + (d_i -
    min(d))). The min-delta (max-transport / slowest) camera floors to `floor`, every other gets
    floor + its relative delta -- so total latency equalizes at the MINIMUM (relative-only). Returns
    {source: pin_ms(int)} over EXACTLY the sources given (never an imag / stream-hold key -- the
    tool only ever knows the strih inputs it measured)."""
    if not deltas:
        return {}
    base = min(deltas.values())
    return {src: max(floor_ms, int(round(floor_ms + (d - base)))) for src, d in deltas.items()}


def sanity_ok(deltas, max_delta_ms=DEFAULT_MAX_DELTA_MS):
    """(ok, worst_source, worst_ms): the widest relative delta (max-min) must be <= max_delta_ms.
    A delta beyond it is a degraded/underrun card, not a real inter-card difference (the owner's
    "94 ms between identical cards is nonsense") -- FAIL rather than ship a deep pin. worst_source is
    the camera carrying that widest delta (the one to name in the abort report)."""
    if not deltas:
        return True, None, 0.0
    lo = min(deltas.values())
    worst_src = max(deltas, key=lambda s: deltas[s])
    worst = deltas[worst_src] - lo
    return (worst <= max_delta_ms), worst_src, worst


def alignment_ok(round_ticks, tol_frame_ids=DEFAULT_PARITY_TOL_IDS):
    """The owner's parity gate: True iff the round's frame_id spread <= tol_frame_ids. An
    unverifiable round (fewer than two decoded -> spread None) is NOT a pass -- parity must be
    PROVEN, never assumed."""
    spread = frame_id_spread(round_ticks)
    if spread is None:
        return False
    return spread <= tol_frame_ids


# ---------------------------------------------------------------------------
# LIVE decode (cv2 + a preprocessing fallback for raw 1920px reads)
# ---------------------------------------------------------------------------
def decode_qr_texts(png_bytes):
    """Every QR text in a PNG's bytes. cv2.detectAndDecodeMulti first; if it yields NO painter-shaped
    payload, retry on a 2x-upscaled, autocontrast-stretched, threshold-swept copy (110/130/150) --
    raw 1920px screenshots are sometimes missed by cv2's multi-detector. Returns a de-duplicated
    list of decoded strings (possibly empty). Errors are logged, never silently swallowed."""
    import cv2
    import numpy as np

    arr = np.frombuffer(png_bytes, dtype=np.uint8)
    img = cv2.imdecode(arr, cv2.IMREAD_COLOR)
    if img is None:
        return []
    detector = cv2.QRCodeDetector()
    found_texts = set()

    def _collect(mat):
        try:
            ok, decoded, _pts, _straight = detector.detectAndDecodeMulti(mat)
        except cv2.error as exc:  # noqa: BLE001 -- logged, never silent; a bad frame is a miss
            sys.stderr.write(f"WARNING: qr_align decode: cv2 error: {exc}\n")
            return
        if ok:
            for t in decoded:
                if t:
                    found_texts.add(t)

    _collect(img)
    # If nothing painter-shaped decoded, try the preprocessing ladder before giving up on this shot.
    if not any(t.startswith("P") for t in found_texts):
        gray = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
        up = cv2.resize(gray, None, fx=2.0, fy=2.0, interpolation=cv2.INTER_CUBIC)
        # autocontrast stretch
        lo, hi = int(up.min()), int(up.max())
        if hi > lo:
            up = ((up.astype(np.float32) - lo) * (255.0 / (hi - lo))).clip(0, 255).astype(np.uint8)
        _collect(cv2.cvtColor(up, cv2.COLOR_GRAY2BGR))
        for thr in (110, 130, 150):
            _bw = cv2.threshold(up, thr, 255, cv2.THRESH_BINARY)[1]
            _collect(cv2.cvtColor(_bw, cv2.COLOR_GRAY2BGR))
            if any(t.startswith("P") for t in found_texts):
                break
    return list(found_texts)


def _extract_png_bytes(image_data):
    """OBS GetSourceScreenshot.imageData (bare base64 or 'data:image/png;base64,...') -> raw PNG
    bytes, or None. Mirrors mv_skew_snapshot / obs_phase2 parsing."""
    import base64
    if not image_data:
        return None
    b64 = image_data.split(",", 1)[1] if image_data.startswith("data:") else image_data
    return base64.b64decode(b64)


def barrier_screenshot(sources, host, password, width, height):
    """ONE simultaneous round: a dedicated WS connection per source, all released together on a
    threading.Barrier so the GetSourceScreenshot requests leave with ~0 ms send skew (the true
    cross-camera latch instant). Returns {source: [qr_texts]} (empty list = decoded nothing)."""
    import threading
    import obs_phase2

    conns = {}
    results = {src: [] for src in sources}
    for src in sources:
        conns[src] = obs_phase2._conn(host, password)
    try:
        barrier = threading.Barrier(len(sources))

        def _shoot(src):
            ws = conns[src]
            barrier.wait()
            try:
                res = obs_phase2._rpc(
                    ws, "GetSourceScreenshot",
                    {"sourceName": src, "imageFormat": "png",
                     "imageWidth": width, "imageHeight": height},
                    ignore_err=True)
            except Exception as exc:  # noqa: BLE001 -- per-source miss, logged, never abort
                sys.stderr.write(f"WARNING: qr_align: screenshot {src!r}: {exc}\n")
                return
            png = _extract_png_bytes(res.get("imageData") if isinstance(res, dict) else None)
            if png is not None:
                results[src] = decode_qr_texts(png)

        threads = [threading.Thread(target=_shoot, args=(src,)) for src in sources]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
    finally:
        for ws in conns.values():
            try:
                ws.close()
            except Exception as exc:  # noqa: BLE001 -- logged, never silent (script-failure-policy)
                sys.stderr.write(f"WARNING: qr_align: ws.close() failed: {exc}\n")
    return results


def measure_rounds(sources, host, password, rounds, width, height, run_id=None):
    """`rounds` simultaneous barrier screenshots -> [{source: (frame_id, gen_ts_ns)|None}]. The
    painter run_id is auto-detected (the run present on the MOST cameras) unless pinned. Returns
    (rounds_ticks, run_id)."""
    import time
    from mv_skew_snapshot import tick_map, dominant_run_id

    raw = []
    for _ in range(rounds):
        raw.append(barrier_screenshot(sources, host, password, width, height))
        time.sleep(0.15)
    if run_id is None:
        maps = [tick_map(texts) for shot in raw for texts in shot.values()]
        run_id = dominant_run_id(maps)
    rounds_ticks = []
    for shot in raw:
        rounds_ticks.append({src: (pick_painter_tick(texts, run_id) if run_id is not None else None)
                             for src, texts in shot.items()})
    return rounds_ticks, run_id


def read_current_pins(sources, host, password):
    """{source: current genlock_latency_ms_src (int) | None} read live over WS."""
    import obs_phase2
    ws = obs_phase2._conn(host, password)
    try:
        return {src: obs_phase2.read_current_pin(ws, src) for src in sources}
    finally:
        ws.close()


def _measured_spread(rounds_ticks, tol_frame_ids):
    """The representative frame_id spread across rounds (median of the per-round spreads), and
    whether it is within tolerance. A round with <2 decoded contributes no spread; if NO round is
    verifiable the spread is None (unverifiable)."""
    spreads = [s for s in (frame_id_spread(r) for r in rounds_ticks) if s is not None]
    if not spreads:
        return None, False
    med = statistics.median(spreads)
    return med, (med <= tol_frame_ids)


def align(sources, host, password, *, execute, rounds, min_valid_rounds, max_delta_ms,
          parity_tol_ids, floor_ms, width, height, verify_rounds, settle_s):
    """The full per-run alignment: measure -> (already aligned? PASS) -> floor-3 plan -> sanity ->
    apply (execute) -> settle -> RE-MEASURE -> PASS iff parity holds. Returns a result dict; raises
    AlignmentImpossible on an un-measurable / un-sane / still-misaligned rig."""
    import time
    from apply_latency_pins import apply_pins

    current_pins = read_current_pins(sources, host, password)
    rounds_ticks, run_id = measure_rounds(sources, host, password, rounds, width, height)
    pre_spread, pre_ok = _measured_spread(rounds_ticks, parity_tol_ids)

    result = {
        "sources": sources, "run_id": run_id, "current_pins": current_pins,
        "pre_spread_ids": pre_spread, "execute": execute,
    }

    if run_id is None or pre_spread is None:
        raise AlignmentImpossible(
            "[qr-align] no painter QR decoded on the on-air strih inputs -- cannot measure "
            f"alignment (sources={sources}). Is the painter running and every input on-air?")

    deltas, n_valid = robust_deltas(rounds_ticks, current_pins, min_valid_rounds)
    result["median_deltas_ms"] = {s: round(v, 2) for s, v in deltas.items()}
    result["valid_rounds"] = n_valid

    if pre_ok:
        result["status"] = "already-aligned"
        result["plan"] = {}
        return result

    ok, worst_src, worst = sanity_ok(deltas, max_delta_ms)
    result["worst_source"] = worst_src
    result["worst_delta_ms"] = round(worst, 2)
    if not ok:
        raise AlignmentImpossible(
            f"[qr-align] cannot align: {worst_src!r} is {worst:.1f} ms off the slowest camera "
            f"(> {max_delta_ms:.0f} ms sanity bound) -- a degraded/underrun grabber, not a real "
            f"inter-card delta. Per-camera deltas: {result['median_deltas_ms']}.")

    plan = floor3_pins(deltas, floor_ms)
    result["plan"] = plan

    if not execute:
        result["status"] = "plan-only"
        return result

    import obs_phase2
    ws = obs_phase2._conn(host, password)
    try:
        result["applied"] = apply_pins(ws, plan, True)
    finally:
        ws.close()

    time.sleep(settle_s)
    verify_ticks, _ = measure_rounds(sources, host, password, verify_rounds, width, height,
                                     run_id=run_id)
    post_spread, post_ok = _measured_spread(verify_ticks, parity_tol_ids)
    result["post_spread_ids"] = post_spread
    if not post_ok:
        # Name the still-offending cameras (post-apply per-camera deltas) in the abort.
        post_pins = read_current_pins(sources, host, password)
        post_deltas = {}
        for rnd in verify_ticks:
            d = round_deltas(rnd, post_pins)
            if d:
                for s, v in d.items():
                    post_deltas.setdefault(s, []).append(v)
        named = {s: round(statistics.median(v), 1) for s, v in post_deltas.items()} if post_deltas \
            else "unverifiable"
        raise AlignmentImpossible(
            f"[qr-align] applied floor-3 pins {plan} but the re-measured frame_id spread is "
            f"{post_spread} (> {parity_tol_ids}) -- alignment did NOT hold. Per-camera residual "
            f"deltas (ms): {named}.")
    result["status"] = "aligned"
    return result


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default=os.environ.get("OBS_PASSWORD", ""))
    ap.add_argument("--sources", required=True,
                    help="comma-separated strih inputs to align, e.g. 'NDI cam1,NDI cam2,NDI cam4'")
    ap.add_argument("--rounds", type=int, default=DEFAULT_ROUNDS)
    ap.add_argument("--min-valid-rounds", type=int, default=DEFAULT_MIN_VALID_ROUNDS)
    ap.add_argument("--max-delta-ms", type=float, default=DEFAULT_MAX_DELTA_MS)
    ap.add_argument("--parity-tol-ids", type=int, default=DEFAULT_PARITY_TOL_IDS)
    ap.add_argument("--floor-ms", type=int, default=DEFAULT_FLOOR_MS)
    ap.add_argument("--width", type=int, default=DEFAULT_WIDTH)
    ap.add_argument("--height", type=int, default=DEFAULT_HEIGHT)
    ap.add_argument("--verify-rounds", type=int, default=DEFAULT_VERIFY_ROUNDS)
    ap.add_argument("--settle-s", type=float, default=DEFAULT_SETTLE_S)
    ap.add_argument("--execute", action="store_true",
                    help="APPLY the floor-3 pins (default: DRY-RUN -- measure + plan, write nothing)")
    a = ap.parse_args(argv)

    sources = [s.strip() for s in a.sources.split(",") if s.strip()]
    if not sources:
        raise SystemExit("[qr-align] --sources is empty")

    result = align(
        sources, a.host, a.password,
        execute=a.execute, rounds=a.rounds, min_valid_rounds=a.min_valid_rounds,
        max_delta_ms=a.max_delta_ms, parity_tol_ids=a.parity_tol_ids, floor_ms=a.floor_ms,
        width=a.width, height=a.height, verify_rounds=a.verify_rounds, settle_s=a.settle_s)

    print(json.dumps(result, default=str))
    status = result.get("status")
    if status == "already-aligned":
        print(f"[qr-align] host={a.host} ALREADY ALIGNED (spread {result['pre_spread_ids']} id).",
              file=sys.stderr)
    elif status == "plan-only":
        print(f"[qr-align] host={a.host} DRY-RUN floor-3 plan (spread {result['pre_spread_ids']} id "
              f"-> would set {result['plan']}); re-run with --execute to apply.", file=sys.stderr)
    elif status == "aligned":
        print(f"[qr-align] host={a.host} ALIGNED: set {result['plan']}, re-measured spread "
              f"{result['post_spread_ids']} id (<= {a.parity_tol_ids}).", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
