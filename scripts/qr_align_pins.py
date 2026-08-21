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

The floor-3 model: each camera's on-air LATENCY is the age of the painter frame it shows at its
latch instant, latency_i = (t_send_i - gen_ts_ns_i) (t_send = dev1 monotonic ns at request-send;
the barrier equalizes it, and round_deltas compensates the residual stagger cross-clock-safely, the
mv-skew-measurement.md model). m_i = current_pin_i - latency_i; the MAX-transport camera (slowest
chain that owes it to transport, not to its own pin) has the MIN m_i. new_pin_i = 3 + (m_i -
min_k m_k): the min-m camera floors to 3, every other gets 3 + its relative delta, so total latency
equalizes at the MINIMUM (relative-only, never deep). Medianed across rounds for robustness; a
median delta above a sanity bound = a degraded/underrun card -> FAIL rather than ship a deep pin.

DOMAINS (never crossed): this tool writes ONLY the strih per-source genlock_latency_ms_src pins for
the align set it is given. The stream `NDI 2ME PGM` hold (operator A/V-align domain) and imag's 3 ms
floor are NEVER touched -- they are simply never in the align set. Writes go through
apply_latency_pins.apply_pins (read-back-verified, fail-loud, idempotent).

Tier-0: the pure functions (pick_painter_tick, frame_id_spread, round_deltas, robust_deltas,
floor3_pins, sanity_ok, alignment_ok, and the #1160 stable-tail decision _stable_tail_start /
measure_tail_status) do NO I/O and are unit-tested with no rig (tests/python/test_qr_align_pins_1003
.py + test_qr_align_tail_1160.py). cv2/threading/obs plumbing is imported LOCALLY inside the live
functions so the pure logic (and its tests) never need a display or a rig.

CLI:
    qr_align_pins.py --host 10.77.9.202 --sources "NDI cam1,NDI cam2,NDI cam3,NDI cam4"  # DRY-RUN
    qr_align_pins.py --host 10.77.9.202 --sources "..." --execute                        # ALIGN
"""
from __future__ import annotations

import argparse
import collections
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
from mv_skew_snapshot import dominant_run_id, parse_payload  # noqa: E402

DEFAULT_MIN_VALID_ROUNDS = 5
DEFAULT_MIN_PARITY_ROUNDS = 3  # full rounds needed to CONFIRM already-aligned (cheaper than re-derive)
DEFAULT_PARITY_TOL_IDS = 1
# #1160 -- measure to a STABLE TAIL, never the post-restart convergence transient. The rig backlog
# (issue 1145) drains at ~0.3 frame/s, so a fresh restart / receiver reconnect / burn toggle leaves
# a camera MINUTES over the align bound while it catches up; a fixed window judged mid-drain aborts a
# rig whose steady state is healthy seconds later. Keep measuring until the last K rounds are MUTUALLY
# stable (their cross-camera spreads within STABLE_TOL of each other -- the pairwise form, which
# subsumes round-to-round <=1 AND rejects a slow monotonic ramp), then judge the tail ONLY. All the
# verdict thresholds (66 ms sanity, <=1-id parity, min-valid/parity rounds) are UNCHANGED, applied to
# the tail. All calibration, live-re-measurable like the other consts.
DEFAULT_STABLE_TAIL_ROUNDS = 3    # K: consecutive mutually-stable rounds that prove convergence
DEFAULT_STABLE_TOL_IDS = 1        # tail spreads must lie within this many frame_ids of each other
DEFAULT_MEASURE_BUDGET_S = 90.0   # total wall-clock bound on the measure phase (never runs away)
DEFAULT_MAX_MEASURE_ROUNDS = 30   # hard round cap (secondary bound; ~4 s/round => ~90 s at ~22)
# A median relative delta above this = a degraded/underrun card, NOT a real inter-card difference:
# FAIL rather than ship a deep pin. Must be BELOW the owner's cited "94 ms between identical cards is
# nonsense" (a 100 ms default would silently re-enable the exact rejected deep-pin behavior). 66 ms
# = ~2 frames @30fps -- rejects a 94 ms degraded-card blowout while passing legitimate floor-3
# deltas (the owner's "1-2 frame real spread"; the supervisor's live cam3 delta was ~42 ms).
DEFAULT_MAX_DELTA_MS = 66.0
DEFAULT_FLOOR_MS = 3          # imag-min-latency floor; the slowest strih camera anchors here
DEFAULT_WIDTH = 1920
DEFAULT_HEIGHT = 1080
DEFAULT_SETTLE_S = 4.0        # let the genlock FIFO re-lock after a pin change before re-measuring

# The reserved node-burn run_ids -- a MIRROR of src/probe/recording.rs::NODE_BURN_RUN_IDS
# (BURN_RUN_ID_* in src/probe/recording_latency.rs; keep in sync -- they are load-bearing consts,
# not tunables). The measurement burn (vendor/distroav/src/ndi-burn-filter.cpp) emits its QR in the
# BYTE-IDENTICAL painter wire format `P{run_id}.{frame_id}.{gen_ts_ns}.{crc}`, differing ONLY in
# run_id (derived from the host role: strih=911002 on EVERY strih input, stream=911004, imag=911003,
# plus the per-camera capture burns). So a "filter by payload SHAPE" cannot tell a burn from the
# painter; the discriminator is the run_id. Under E2E the align step runs after the burns are added,
# so the strih burn (911002) rides every screenshot alongside the painter dual-QR and would hijack
# run_id auto-detect / the decode recovery-ladder guard unless excluded here (#1159).
NODE_BURN_RUN_IDS = frozenset({
    911001,  # BURN_RUN_ID_CAM1
    911002,  # BURN_RUN_ID_STRIH
    911003,  # BURN_RUN_ID_IMAG
    911004,  # BURN_RUN_ID_STREAM
    911007,  # BURN_RUN_ID_CAM4
    911008,  # BURN_RUN_ID_CAM3
    911009,  # BURN_RUN_ID_CAM2
    911010,  # BURN_RUN_ID_CAM5
    911011,  # BURN_RUN_ID_CAM6
    911012,  # BURN_RUN_ID_CAM7
})


class AlignmentImpossible(SystemExit):
    """Raised when the run CANNOT be aligned (too few decodable rounds, or a delta beyond the
    sanity bound). A SystemExit subclass so the CLI exits non-zero and the E2E step ABORTS with a
    named per-camera reason -- never a silent proceed on a misaligned rig."""


# ---------------------------------------------------------------------------
# PURE logic (no I/O, unit-tested with no rig)
# ---------------------------------------------------------------------------
def is_burn_run_id(run_id):
    """True when `run_id` is one of the reserved node-burn ids (NODE_BURN_RUN_IDS) -- a digitally
    burned QR (vendor/distroav/src/ndi-burn-filter.cpp), never the painter's optical dual-QR."""
    return run_id in NODE_BURN_RUN_IDS


def has_painter_payload(qr_texts):
    """True iff any decoded text is a VALID painter (NON-burn) QR. The measurement burn shares the
    exact painter wire format (`P{run_id}...`) but a fixed burn run_id, so a plain `startswith("P")`
    check is satisfied by a decoded BURN -- which made decode_qr_texts skip its upscale/threshold
    recovery pass while a missed painter QR was still recoverable (#1159). parse_payload validates
    CRC + shape; is_burn_run_id drops the burns."""
    for t in qr_texts:
        p = parse_payload(t)
        if p is not None and not is_burn_run_id(p[0]):
            return True
    return False


def painter_run_id(tick_maps):
    """The painter's universal run_id from per-screenshot {run_id: gen_ts_ns} maps, with the node
    BURNS excluded FIRST. The measurement burn shares the painter wire format but a fixed per-node
    run_id (NODE_BURN_RUN_IDS): under E2E the strih burn (911002) is present on EVERY on-air input,
    so it TIES the painter on screenshot-count -- and since dominant_run_id breaks ties to the
    SMALLEST id (911002 << the painter's ~1.8e9 epoch), the burn would win and be mistaken for the
    painter (#1159). Stripping the burn ids first leaves the painter (the only universal NON-burn
    run_id) to win unambiguously. Returns None when no non-burn run_id decoded anywhere."""
    filtered = [{r: g for r, g in m.items() if not is_burn_run_id(r)} for m in tick_maps]
    return dominant_run_id(filtered)


def format_round_table(rounds_ticks, sources, tail_start=None):
    """A per-round x per-camera decoded painter frame_id table (undecoded cell = '--') plus a
    per-camera 'decoded N/R' summary, for the FAIL diagnostics (#1159). It lets the operator tell
    "undecodable" (mostly '--') from "unstable spread" (decoded but scattered frame_ids) from "one
    dead camera" (one column all '--'). `rounds_ticks`: [{source: (frame_id, gen_ts_ns,
    t_send_ns) | None}]; only frame_id (index 0) is shown.

    When `tail_start` is given (#1160), a trailing 'used' column marks the STABLE-TAIL rounds
    (index >= tail_start) that the verdict was actually computed from -- ALL rounds are still
    printed, so the discarded convergence-transient rounds are visible too. `tail_start` None keeps
    the original format byte-for-byte (existing 2-arg callers/tests unchanged)."""
    mark = tail_start is not None
    short = [s.replace("NDI ", "") for s in sources]
    w = max([6] + [len(s) for s in short])
    header = " round | " + " | ".join(s.rjust(w) for s in short) + " | spread" + \
        (" | used" if mark else "")
    lines = ["[qr-align] per-round painter frame_id table (-- = undecoded):", header,
             "-" * len(header)]
    decoded = {s: 0 for s in sources}
    for r, rnd in enumerate(rounds_ticks):
        cells, fids = [], []
        for s in sources:
            tk = rnd.get(s) if rnd else None
            if tk is None:
                cells.append("--".rjust(w))
            else:
                decoded[s] += 1
                fids.append(tk[0])
                cells.append(str(tk[0]).rjust(w))
        spread = str(max(fids) - min(fids)) if len(fids) >= 2 else "n/a"
        used = (" | " + ("tail" if r >= tail_start else "")) if mark else ""
        lines.append(f"{r:>6} | " + " | ".join(cells) + f" | {spread}" + used)
    n = len(rounds_ticks)
    lines.append("decoded per camera: " + "  ".join(f"{sh}={decoded[s]}/{n}"
                                                     for s, sh in zip(sources, short)))
    return "\n".join(lines)


def _emit_fail_diagnostics(rounds_ticks, sources, tail_start=None):
    """Print the per-round frame_id diagnostics table to stderr before an AlignmentImpossible abort
    (#1159), so every FAIL path carries the per-camera/per-round detail, not just the verdict. The
    #1160 `tail_start` marks which rounds fed the verdict (the stable tail)."""
    sys.stderr.write(format_round_table(rounds_ticks, sources, tail_start) + "\n")


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
        if is_burn_run_id(r):
            continue  # a node burn shares the painter wire format; never latch it as a painter tick
        if r != run_id:
            continue
        if best is None or frame_id > best[0]:
            best = (frame_id, gen_ts_ns)
    return best


def frame_id_spread(round_ticks):
    """max-min frame_id over the DECODED cameras in one round, or None when fewer than two decoded
    (parity is unverifiable with <2 samples). `round_ticks`: {source: (frame_id, gen_ts_ns,
    t_send_ns)|None} -- only frame_id (index 0) is read here."""
    fids = [tk[0] for tk in round_ticks.values() if tk is not None]
    if len(fids) < 2:
        return None
    return max(fids) - min(fids)


def round_deltas(round_ticks, current_pins):
    """Per-round relative ms delta per camera. `round_ticks`: {source: (frame_id, gen_ts_ns,
    t_send_ns) | None}. Returns {source: d_i >= 0} or None when the round is INCOMPLETE (any camera
    undecoded, or any current pin unknown -- an incomplete round cannot give a full cross-camera
    spread, so it is excluded rather than half-measured).

    Each camera's on-air LATENCY is the age of the painter frame it shows at its own LATCH instant:
    `latency_i = (t_send_i - gen_ts_i)`. A barrier releases all four GetSourceScreenshot sends
    together, but the sends still stagger by a few thread-scheduling / graphics-thread-serialization
    ms, so a later-served camera latches a NEWER frame (higher gen_ts) and would look falsely
    "faster" -- the exact t_send-vs-latch trap `.claude/rules/mv-skew-measurement.md` documents. We
    compensate the SAME way: t_send (dev1 monotonic ns at request-send) is the frame's latch instant.
    Cross-clock safe: gen_ts is the painter's clock and t_send is dev1's, so we only ever take
    SAME-clock DIFFERENCES (gen_ts_i - g0 in painter ns; t_send_i - t0 in dev1 ns) -- the cross-clock
    offset cancels, exactly as mv_skew_snapshot.skew_sample_ms does. Both differences are small, so
    no catastrophic float cancellation. transport_i = latency_i - pin_i; m_i = pin_i - latency_i
    (the min-m camera is the MAX-transport / slowest); d_i = m_i - min(m)."""
    have = {}
    for src, tk in round_ticks.items():
        if tk is None:
            return None
        pin = current_pins.get(src)
        if pin is None:
            return None
        have[src] = (tk[1], tk[2], pin)  # (gen_ts_ns, t_send_ns, pin_ms)
    if not have:
        return None
    g0 = min(g for g, _t, _p in have.values())
    t0 = min(t for _g, t, _p in have.values())
    # latency_i (relative, ms) = (t_send_i - t0) - (gen_ts_i - g0); m_i = pin_i - latency_i
    m = {src: pin - (((t - t0) - (g - g0)) / 1e6) for src, (g, t, pin) in have.items()}
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
    """(ok, slowest_source, widest_source, worst_ms): the cross-camera spread (max-min delta) must
    be <= max_delta_ms. A spread beyond it is a degraded/underrun card, not a real inter-card
    difference (the owner's "94 ms between identical cards is nonsense") -- FAIL rather than ship a
    deep pin. Both ends are named so the abort does not wrongly blame a healthy camera: `slowest`
    is the min-delta (max-transport) camera that anchors to the floor -- a degraded grabber ADDS
    latency, so it is usually THIS one; `widest` is the max-delta camera (the biggest gap from the
    slowest). worst_ms is the spread (widest_delta - slowest_delta)."""
    if not deltas:
        return True, None, None, 0.0
    slowest_src = min(deltas, key=lambda s: deltas[s])
    widest_src = max(deltas, key=lambda s: deltas[s])
    worst = deltas[widest_src] - deltas[slowest_src]
    return (worst <= max_delta_ms), slowest_src, widest_src, worst


def alignment_ok(round_ticks, tol_frame_ids=DEFAULT_PARITY_TOL_IDS):
    """The owner's parity gate: True iff the round's frame_id spread <= tol_frame_ids. An
    unverifiable round (fewer than two decoded -> spread None) is NOT a pass -- parity must be
    PROVEN, never assumed."""
    spread = frame_id_spread(round_ticks)
    if spread is None:
        return False
    return spread <= tol_frame_ids


# ---------------------------------------------------------------------------
# #1160 -- stable-tail measurement (PURE decision; no I/O, unit-tested with no rig)
# ---------------------------------------------------------------------------
# The measure loop's stop decision. `tail_start` indexes the first round of the STABLE TAIL that the
# verdict is computed from (rounds_ticks[tail_start:]); None when no stable tail exists yet.
TailStatus = collections.namedtuple("TailStatus", "done reason tail_start")


def _is_full_round(round_ticks, sources):
    """True iff EVERY align source decoded a painter tick this round (a FULL round). Stability is
    judged over full rounds only, so a decode-miss round never widens/narrows a spread comparison
    against a different camera set (it simply breaks the contiguous stable suffix). Mirrors the
    full-round predicate _full_round_parity uses."""
    return bool(round_ticks) and len(round_ticks) == len(sources) and \
        all(round_ticks.get(s) is not None for s in sources)


def _stable_tail_start(rounds_ticks, sources, stable_tail_rounds, stable_tol_ids):
    """The start index of the maximal contiguous suffix of FULL rounds, ending at the LAST round,
    whose cross-camera frame_id spreads all lie within `stable_tol_ids` of each other (max-min <=
    tol -- the pairwise "mutually stable" form). Returns None when that suffix is shorter than
    `stable_tail_rounds` (K) -- i.e. the last K rounds are not yet mutually stable. This is a
    STRONGER test than the ticket's literal "round-to-round <=1": a slow monotonic ramp (spreads
    1,2,3) has round-to-round deltas <=1 but max-min 2, so it is correctly rejected as still
    diverging."""
    n = len(rounds_ticks)
    lo = hi = None
    start = n
    for i in range(n - 1, -1, -1):
        r = rounds_ticks[i]
        if not _is_full_round(r, sources):
            break
        sp = frame_id_spread(r)
        if sp is None:  # defensive: a full round of >=2 cams always has a spread
            break
        nlo = sp if lo is None else min(lo, sp)
        nhi = sp if hi is None else max(hi, sp)
        if nhi - nlo > stable_tol_ids:
            break
        lo, hi, start = nlo, nhi, i
    return start if (n - start) >= stable_tail_rounds else None


def measure_tail_status(rounds_ticks, sources, *, stable_tail_rounds, stable_tol_ids,
                        parity_tol_ids, min_parity_rounds, min_valid_rounds):
    """Decide, from the rounds accumulated so far, whether the measure phase can STOP and which
    STABLE-TAIL rounds the verdict should use. Returns a TailStatus:
      - "converged-aligned": the last K rounds are mutually stable AND already at parity (median
        spread <= parity_tol over >= min_parity_rounds full rounds) -> STOP, PASS-fast. Needs only
        K rounds; min_valid_rounds is NOT required (no re-derive).
      - "converged-stable": the tail is mutually stable but NOT at parity (a static residual delta
        floor-3 pins can fix) AND has >= min_valid_rounds valid rounds -> STOP, re-derive from tail.
      - "stable-need-more": the tail is stable but not aligned and has too few rounds to re-derive
        robustly -> keep measuring (the unchanged min-valid-rounds threshold applied to the tail).
      - "unstable": the last K rounds are not mutually stable -> keep measuring.
    All the verdict thresholds are UNCHANGED here -- this only chooses WHEN to stop and WHICH rounds
    to judge (the tail), never weakening a gate."""
    start = _stable_tail_start(rounds_ticks, sources, stable_tail_rounds, stable_tol_ids)
    if start is None:
        return TailStatus(False, "unstable", None)
    tail = rounds_ticks[start:]
    _med, aligned = _full_round_parity(tail, sources, parity_tol_ids, min_parity_rounds)
    if aligned:
        return TailStatus(True, "converged-aligned", start)
    if len(tail) >= min_valid_rounds:
        return TailStatus(True, "converged-stable", start)
    return TailStatus(False, "stable-need-more", start)


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
    if not has_painter_payload(found_texts):
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
            if has_painter_payload(found_texts):
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
    threading.Barrier so the GetSourceScreenshot requests leave with minimal send skew. Returns
    {source: (qr_texts, t_send_ns)} -- `t_send_ns` is dev1's monotonic clock captured JUST BEFORE
    that source's RPC send (the frame's latch instant; round_deltas compensates the residual
    stagger, see mv-skew-measurement.md). An empty texts list = decoded nothing. If fewer than two
    connections open, returns {} (a barrier needs >= 1 party AND a spread needs >= 2 cameras)."""
    import threading
    import time
    import obs_phase2

    if len(sources) < 1:
        return {}
    conns = {}
    results = {src: ([], None) for src in sources}
    for src in sources:
        conns[src] = obs_phase2._conn(host, password)
    try:
        # Barrier with a timeout so a stuck connection can never wedge the whole round forever.
        barrier = threading.Barrier(len(sources))

        def _shoot(src):
            ws = conns[src]
            try:
                barrier.wait(timeout=15.0)
            except threading.BrokenBarrierError:
                sys.stderr.write(f"WARNING: qr_align: barrier broke before {src!r} shot\n")
                return
            t_send = time.monotonic_ns()
            try:
                res = obs_phase2._rpc(
                    ws, "GetSourceScreenshot",
                    {"sourceName": src, "imageFormat": "png",
                     "imageWidth": width, "imageHeight": height},
                    ignore_err=True)
            except Exception as exc:  # noqa: BLE001 -- per-source miss, logged, never abort
                sys.stderr.write(f"WARNING: qr_align: screenshot {src!r}: {exc}\n")
                results[src] = ([], t_send)
                return
            png = _extract_png_bytes(res.get("imageData") if isinstance(res, dict) else None)
            results[src] = (decode_qr_texts(png) if png is not None else [], t_send)

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


def measure_stable_tail(sources, host, password, *, width, height, run_id=None,
                        stable_tail_rounds=DEFAULT_STABLE_TAIL_ROUNDS,
                        stable_tol_ids=DEFAULT_STABLE_TOL_IDS,
                        parity_tol_ids=DEFAULT_PARITY_TOL_IDS,
                        min_parity_rounds=DEFAULT_MIN_PARITY_ROUNDS,
                        min_valid_rounds=DEFAULT_MIN_VALID_ROUNDS,
                        budget_s=DEFAULT_MEASURE_BUDGET_S,
                        max_rounds=DEFAULT_MAX_MEASURE_ROUNDS,
                        inter_round_s=0.15):
    """Barrier-screenshot round by round until the cross-camera spread has STABILIZED to a judgeable
    tail (#1160), or the time/round budget is hit. After each round measure_tail_status decides
    whether we can stop (converged-aligned / converged-stable) and which rounds are the stable tail.
    The source ORDER is ROTATED each round so no camera is systematically served first/last (residual
    render-order bias averages out, on top of round_deltas' t_send compensation). The painter run_id
    is auto-detected (present on the MOST cameras) unless pinned. Returns (rounds_ticks, run_id,
    TailStatus) -- ALL measured rounds (so the diagnostics table shows the discarded transient too),
    the resolved run_id, and the final stop decision (its tail_start indexes the stable tail)."""
    import time

    n = len(sources)
    raw = []
    rounds_ticks, rid = [], run_id
    status = TailStatus(False, "unstable", None)
    t0 = time.monotonic()
    r = 0
    while True:
        order = sources[r % n:] + sources[:r % n] if n else sources
        raw.append(barrier_screenshot(order, host, password, width, height))
        r += 1
        rounds_ticks, rid = ticks_from_raw(raw, run_id)
        status = measure_tail_status(
            rounds_ticks, sources, stable_tail_rounds=stable_tail_rounds,
            stable_tol_ids=stable_tol_ids, parity_tol_ids=parity_tol_ids,
            min_parity_rounds=min_parity_rounds, min_valid_rounds=min_valid_rounds)
        if status.done:
            break
        if r >= max_rounds or (time.monotonic() - t0) >= budget_s:
            break
        if inter_round_s:
            time.sleep(inter_round_s)
    return rounds_ticks, rid, status


def ticks_from_raw(raw, run_id=None):
    """PURE: resolve the painter run_id (when unset) and map each round's decoded screenshots to
    {source: (frame_id, gen_ts_ns, t_send_ns) | None}. `raw` is a list of rounds, each
    {source: (qr_texts, t_send_ns)}. Extracted from the measure loop (measure_stable_tail) so the
    run_id-resolution + tick-selection path is Tier-0 testable with synthetic decoded-text lists
    (no rig, no cv2)."""
    from mv_skew_snapshot import tick_map
    if run_id is None:
        maps = [tick_map(texts) for shot in raw for (texts, _t) in shot.values()]
        run_id = painter_run_id(maps)  # #1159: exclude node burns so a burn never wins run_id
    rounds_ticks = []
    for shot in raw:
        rnd = {}
        for src, (texts, t_send) in shot.items():
            tk = pick_painter_tick(texts, run_id) if run_id is not None else None
            rnd[src] = (tk[0], tk[1], t_send) if (tk is not None and t_send is not None) else None
        rounds_ticks.append(rnd)
    return rounds_ticks, run_id


def read_current_pins(sources, host, password):
    """{source: current genlock_latency_ms_src (int) | None} read live over WS."""
    import obs_phase2
    ws = obs_phase2._conn(host, password)
    try:
        return {src: obs_phase2.read_current_pin(ws, src) for src in sources}
    finally:
        ws.close()


def _full_round_parity(rounds_ticks, sources, tol_frame_ids, min_parity_rounds):
    """Parity over FULL rounds only (every align source decoded), which is what proves ALL cameras
    (incl. cam4) are aligned. Returns (median_spread|None, aligned_bool). aligned iff there are
    >= min_parity_rounds full rounds AND their median frame_id spread <= tol. Confirming alignment
    needs FEWER rounds than re-deriving pins, so a rig aligned on a few full rounds is not aborted
    just because robust_deltas' larger min_valid_rounds is not met (#1003 review finding)."""
    full = [r for r in rounds_ticks if r and all(v is not None for v in r.values())
            and len(r) == len(sources)]
    if len(full) < min_parity_rounds:
        return None, False
    med = statistics.median([frame_id_spread(r) for r in full])
    return med, (med <= tol_frame_ids)


def align(sources, host, password, *, execute, stable_tail_rounds, stable_tol_ids, min_valid_rounds,
          min_parity_rounds, max_delta_ms, parity_tol_ids, floor_ms, width, height,
          measure_budget_s, max_measure_rounds, settle_s):
    """The full per-run alignment: measure to a STABLE TAIL (#1160) -> (already aligned? PASS) ->
    floor-3 plan from the tail -> sanity -> apply (execute) -> settle -> RE-MEASURE to a stable tail
    -> PASS iff parity holds. The verdict is always computed from the stabilized tail, never the
    post-restart convergence transient; every threshold (66 ms sanity, <=1-id parity, min-valid/
    parity rounds) is UNCHANGED, applied to the tail. Returns a result dict; raises
    AlignmentImpossible on an un-measurable / never-stabilizing / un-sane / still-misaligned rig."""
    import time
    from apply_latency_pins import apply_pins

    current_pins = read_current_pins(sources, host, password)
    rounds_ticks, run_id, status = measure_stable_tail(
        sources, host, password, width=width, height=height, run_id=None,
        stable_tail_rounds=stable_tail_rounds, stable_tol_ids=stable_tol_ids,
        parity_tol_ids=parity_tol_ids, min_parity_rounds=min_parity_rounds,
        min_valid_rounds=min_valid_rounds, budget_s=measure_budget_s, max_rounds=max_measure_rounds)
    tail_start = status.tail_start
    tail = rounds_ticks[tail_start:] if tail_start is not None else rounds_ticks
    pre_spread, pre_ok = _full_round_parity(tail, sources, parity_tol_ids, min_parity_rounds)

    result = {
        "sources": sources, "run_id": run_id, "current_pins": current_pins,
        "measure_rounds_total": len(rounds_ticks), "tail_rounds": len(tail),
        "tail_start_index": tail_start, "stable": status.done, "measure_reason": status.reason,
        "pre_spread_ids": pre_spread, "execute": execute,
    }

    if run_id is None:
        _emit_fail_diagnostics(rounds_ticks, sources, tail_start)  # all "--" = nothing decoded
        raise AlignmentImpossible(
            "[qr-align] no painter QR decoded on the on-air strih inputs -- cannot measure "
            f"alignment (sources={sources}). Is the painter running and every input on-air?")

    # #1160: the cross-camera spread never STABILIZED within the budget -- a converging backlog that
    # never settles (a degraded / over-rate grabber, issue 1145). FAIL with the full table (marking
    # the last stable rounds, if any) rather than judge a transient or an unstable window.
    if not status.done:
        _emit_fail_diagnostics(rounds_ticks, sources, tail_start)
        raise AlignmentImpossible(
            f"[qr-align] the cross-camera spread did not STABILIZE within {measure_budget_s:.0f}s "
            f"/{len(rounds_ticks)} rounds -- the last {stable_tail_rounds} rounds are not mutually "
            f"stable (<= {stable_tol_ids} id) with enough clean rounds to judge (status "
            f"{status.reason!r}), so no steady-state tail could be measured. A converging backlog "
            "that never settles is a degraded / over-rate grabber (issue 1145). Per-round table "
            "above (the last stable rounds, if any, marked 'tail').")

    # From here the verdict is judged over the STABLE TAIL only.
    # ALREADY ALIGNED: confirmed on the tail -> return WITHOUT requiring robust_deltas (which would
    # raise on a rig that IS aligned but has fewer full tail rounds than min_valid_rounds -- the
    # confirm bar is deliberately lower than the re-derive bar, #1003 review finding).
    if pre_ok:
        result["status"] = "already-aligned"
        result["plan"] = {}
        try:
            deltas, n_valid = robust_deltas(tail, current_pins, min_valid_rounds)
            result["median_deltas_ms"] = {s: round(v, 2) for s, v in deltas.items()}
            result["valid_rounds"] = n_valid
        except AlignmentImpossible as exc:  # best-effort report; the rig is already proven aligned
            sys.stderr.write(
                f"NOTE: qr_align: already-aligned; per-camera delta report skipped ({exc})\n")
        return result

    # NOT aligned but STABLE -> re-derive to align FROM THE TAIL; robust_deltas raises if the tail is
    # un-measurable (min_valid_rounds unchanged, applied to the tail).
    try:
        deltas, n_valid = robust_deltas(tail, current_pins, min_valid_rounds)
    except AlignmentImpossible:
        _emit_fail_diagnostics(rounds_ticks, sources, tail_start)  # per-camera/per-round on abort
        raise
    result["median_deltas_ms"] = {s: round(v, 2) for s, v in deltas.items()}
    result["valid_rounds"] = n_valid

    ok, slowest_src, widest_src, worst = sanity_ok(deltas, max_delta_ms)
    result["slowest_source"] = slowest_src
    result["worst_source"] = widest_src
    result["worst_delta_ms"] = round(worst, 2)
    if not ok:
        _emit_fail_diagnostics(rounds_ticks, sources, tail_start)
        raise AlignmentImpossible(
            f"[qr-align] cannot align: cross-camera spread {worst:.1f} ms exceeds the "
            f"{max_delta_ms:.0f} ms sanity bound -- a degraded/underrun grabber, not a real "
            f"inter-card delta (the slowest camera {slowest_src!r} floors to {floor_ms}ms; the "
            f"widest gap is on {widest_src!r}; the anomaly is most likely the slowest card). "
            f"Per-camera deltas (ms off the slowest): {result['median_deltas_ms']}.")

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
    # Re-measure to a STABLE TAIL too, so a pin-change transient is not re-caught (#1160).
    verify_ticks, _, vstatus = measure_stable_tail(
        sources, host, password, width=width, height=height, run_id=run_id,
        stable_tail_rounds=stable_tail_rounds, stable_tol_ids=stable_tol_ids,
        parity_tol_ids=parity_tol_ids, min_parity_rounds=min_parity_rounds,
        min_valid_rounds=min_valid_rounds, budget_s=measure_budget_s, max_rounds=max_measure_rounds)
    vtail_start = vstatus.tail_start
    vtail = verify_ticks[vtail_start:] if vtail_start is not None else verify_ticks
    post_spread, post_ok = _full_round_parity(vtail, sources, parity_tol_ids, min_parity_rounds)
    result["post_spread_ids"] = post_spread
    result["verify_stable"] = vstatus.done
    if not (vstatus.done and post_ok):
        # Name the still-offending cameras (post-apply per-camera deltas over the verify tail).
        post_pins = read_current_pins(sources, host, password)
        post_deltas = {}
        for rnd in vtail:
            d = round_deltas(rnd, post_pins)
            if d:
                for s, v in d.items():
                    post_deltas.setdefault(s, []).append(v)
        named = {s: round(statistics.median(v), 1) for s, v in post_deltas.items()} if post_deltas \
            else "unverifiable"
        _emit_fail_diagnostics(verify_ticks, sources, vtail_start)  # the RE-MEASURED rounds
        why = ("did not STABILIZE" if not vstatus.done
               else f"stabilized at frame_id spread {post_spread} (> {parity_tol_ids})")
        raise AlignmentImpossible(
            f"[qr-align] applied floor-3 pins {plan} but the re-measured tail {why} -- alignment "
            f"did NOT hold. Per-camera residual deltas (ms): {named}.")
    result["status"] = "aligned"
    return result


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default=os.environ.get("OBS_PASSWORD", ""))
    ap.add_argument("--sources", required=True,
                    help="comma-separated strih inputs to align, e.g. 'NDI cam1,NDI cam2,NDI cam4'")
    # #1160 stable-tail measurement knobs (replace the old fixed --rounds / --verify-rounds):
    ap.add_argument("--stable-tail-rounds", type=int, default=DEFAULT_STABLE_TAIL_ROUNDS,
                    help="K: consecutive mutually-stable rounds that prove convergence (#1160)")
    ap.add_argument("--stable-tol-ids", type=int, default=DEFAULT_STABLE_TOL_IDS,
                    help="tail spreads must lie within this many frame_ids of each other (#1160)")
    ap.add_argument("--measure-budget-s", type=float, default=DEFAULT_MEASURE_BUDGET_S,
                    help="total wall-clock bound on the measure phase (#1160)")
    ap.add_argument("--max-measure-rounds", type=int, default=DEFAULT_MAX_MEASURE_ROUNDS,
                    help="hard round cap on the measure phase (#1160)")
    ap.add_argument("--min-valid-rounds", type=int, default=DEFAULT_MIN_VALID_ROUNDS)
    ap.add_argument("--min-parity-rounds", type=int, default=DEFAULT_MIN_PARITY_ROUNDS)
    ap.add_argument("--max-delta-ms", type=float, default=DEFAULT_MAX_DELTA_MS)
    ap.add_argument("--parity-tol-ids", type=int, default=DEFAULT_PARITY_TOL_IDS)
    ap.add_argument("--floor-ms", type=int, default=DEFAULT_FLOOR_MS)
    ap.add_argument("--width", type=int, default=DEFAULT_WIDTH)
    ap.add_argument("--height", type=int, default=DEFAULT_HEIGHT)
    ap.add_argument("--settle-s", type=float, default=DEFAULT_SETTLE_S)
    ap.add_argument("--execute", action="store_true",
                    help="APPLY the floor-3 pins (default: DRY-RUN -- measure + plan, write nothing)")
    a = ap.parse_args(argv)

    sources = [s.strip() for s in a.sources.split(",") if s.strip()]
    if not sources:
        raise SystemExit("[qr-align] --sources is empty")

    result = align(
        sources, a.host, a.password,
        execute=a.execute, stable_tail_rounds=a.stable_tail_rounds, stable_tol_ids=a.stable_tol_ids,
        min_valid_rounds=a.min_valid_rounds, min_parity_rounds=a.min_parity_rounds,
        max_delta_ms=a.max_delta_ms, parity_tol_ids=a.parity_tol_ids, floor_ms=a.floor_ms,
        width=a.width, height=a.height, measure_budget_s=a.measure_budget_s,
        max_measure_rounds=a.max_measure_rounds, settle_s=a.settle_s)

    print(json.dumps(result, default=str))
    status = result.get("status")
    tail = (f"stable tail {result.get('tail_rounds')}/{result.get('measure_rounds_total')} rounds, "
            f"{result.get('measure_reason')}")
    if status == "already-aligned":
        print(f"[qr-align] host={a.host} ALREADY ALIGNED (spread {result['pre_spread_ids']} id; "
              f"{tail}).", file=sys.stderr)
    elif status == "plan-only":
        print(f"[qr-align] host={a.host} DRY-RUN floor-3 plan (spread {result['pre_spread_ids']} id; "
              f"{tail} -> would set {result['plan']}); re-run with --execute to apply.",
              file=sys.stderr)
    elif status == "aligned":
        print(f"[qr-align] host={a.host} ALIGNED: set {result['plan']}, re-measured spread "
              f"{result['post_spread_ids']} id (<= {a.parity_tol_ids}); {tail}.", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
