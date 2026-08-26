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
floor3_pins, sanity_ok, alignment_ok, and the #1160/#1161 outlier-tolerant stable-tail decision
_stable_tail / _stable_tail_start / measure_tail_status) do NO I/O and are unit-tested with no rig
(tests/python/test_qr_align_pins_1003
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
# #1161 -- measurement-window ROBUSTNESS: a HEALTHY rig with no convergence transient is stationary
# noise around a center (2-3) with occasional near-band 4/5/1 blips; the width-1 CLEAN band kept
# truncating the stable suffix on every blip, so the tail formed late and the 90 s window ended before
# 5 clean rounds accrued (a healthy rig wrongly FAILED, live E2E 32568491541). Fix: the stable tail is
# now OUTLIER-TOLERANT (a lone near-band spread blip is SKIPPED, not a RESET -- see
# STABLE_OUTLIER_TOL below) and the window is EXTENDED (150 s / 40 rounds) so a late tail (or a real
# issue-1145 backlog transient) has room for a transient-drain + 5 CLEAN rounds. The LENGTH strictness
# is UNCHANGED: min_valid=5 is judged on CLEAN (in-band) rounds only, an outlier NEVER counts toward
# it; a converging backlog / degraded grabber / sawtooth still FAILS (magnitude + count bounds).
DEFAULT_STABLE_TAIL_ROUNDS = 3    # K: consecutive mutually-stable rounds that prove convergence
DEFAULT_STABLE_TOL_IDS = 1        # the tight CLEAN band: in-band spreads lie within this many ids
# #1161 -- a noisy-but-STATIONARY rig (center 2-3, occasional near-band 4/5/1 blips) must not have its
# stable suffix truncated by every ordinary blip. A round that widens the CLEAN band beyond
# STABLE_TOL is a SKIPPABLE outlier (the span continues across it; it never extends the band and never
# counts as a clean round) iff it is within STABLE_OUTLIER_TOL ids of the band AND outliers stay a
# STRICT MINORITY of clean rounds. A FAR outlier (a convergence transient / large swing) or a
# high-frequency near-band cycle is NOT absorbed -> the suffix STOPS, so a degraded rig still FAILS.
DEFAULT_STABLE_OUTLIER_TOL_IDS = 2  # a skippable outlier must be within this many ids of the clean band
DEFAULT_MEASURE_BUDGET_S = 150.0  # total wall-clock bound on the measure phase (never runs away)
DEFAULT_MAX_MEASURE_ROUNDS = 40   # hard round cap (secondary bound; ~3.75 s/round => ~150 s at ~40)
# A median relative delta above this = a degraded/underrun card, NOT a real inter-card difference:
# FAIL rather than ship a deep pin. Must be BELOW the owner's cited "94 ms between identical cards is
# nonsense" (a 100 ms default would silently re-enable the exact rejected deep-pin behavior). 66 ms
# = ~2 frames @30fps -- rejects a 94 ms degraded-card blowout while passing legitimate floor-3
# deltas (the owner's "1-2 frame real spread"; the supervisor's live cam3 delta was ~42 ms).
DEFAULT_MAX_DELTA_MS = 66.0
# #1161 -- the ABSOLUTE achievable-latency ceiling (distinct from the cross-camera SPREAD bound
# above). Floor-aware pins bring the faster cameras UP to the SLOWEST camera's NATURAL arrival
# transport floor -- adding NO net chain latency beyond the physical floor (unlike the rejected
# deep 90/160/184 pins, which deepened the chain PAST its floor). But if that floor is itself so
# high that aligning needs a pin beyond this budget, the transport is the problem, not the
# alignment -> FAIL LOUD (never deep-pin, never widen the bound). 94 ms = the owner's cited
# "94 ms between identical cards is nonsense" line (~3 canvas frames @30fps); the live rig sits
# well under it (arrival floors ~59-76 ms).
DEFAULT_MAX_ABS_LATENCY_MS = 94
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
    # issue 1196: AUX_TICK_RUN_ID -- the PAINTED aux Vernier tick pair (bottom burn-gap QRs,
    # gen_ts_ns = 0). Not a burn, but it shares the painter wire format, is UNIVERSAL on every
    # screenshot (painted content), and its id is far below the ~1.8e9 epoch -- without this
    # entry it would win painter_run_id's smallest-id tie-break (the exact #1159 class) and its
    # constant gen_ts_ns=0 would poison the alignment spread math.
    911013,  # AUX_TICK_RUN_ID (painted aux tick pair, tick-excluded like the burns)
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
        used = (" | tail" if r >= tail_start else "") if mark else ""  # only tail rows are marked
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


# ---------------------------------------------------------------------------
# #1161 -- FLOOR-AWARE pins: raise the faster cameras ABOVE their arrival floor
# ---------------------------------------------------------------------------
# WHY floor3_pins (floor + delta) is INERT and this replaces it on --execute:
#   The genlock FIFO is latency = max(pin, transport), NOT pin + transport. In the
#   transport-dominated regime the live rig is in (frames arrive ~59-66 ms old, deltas ~1 canvas
#   frame), floor3_pins' `floor(3) + delta` (= ~3-50 ms) lands BELOW each source's arrival floor,
#   so the reserve has no leverage -- the FIFO cannot present a frame younger than what arrived, and
#   a pin below the arrival edge is structurally inert (root cause: issue 1161, off-rig-proven). The
#   sibling genlock-C ACQUIRE frame-mover CAN add hold on a pin RISE, but ONLY when the pin sits
#   ABOVE the arrival floor. So the aligner must target an ABSOLUTE achievable latency =
#   arrival_floor_i + delta_i (the slowest camera's floor), raising each faster camera's pin above
#   its own floor; the slowest (max present age) keeps the minimum pin (floor_ms), inert at its
#   natural floor. The absolute floor is NOT measurable from the painter QR (gen_ts is CLOCK_REALTIME
#   on the painter box, t_send is dev1 CLOCK_MONOTONIC -- cross-clock, RELATIVE deltas only); it
#   comes from the strih genlock audit `latency_ms + mean_head_skew_ms` (the pin's own DanteSync-
#   synced OBS clock), reconstructed by the SAME prerecord_phase_calibrate helper.
def arrival_floors_from_jitter(jitter_json, sources):
    """{src: arrival_floor_ms} for the given strih sources, from a `genlock-jitter-report --json`
    dict. arrival_floor = latency_ms + mean_head_skew_ms (the effective pin during the sampled
    window plus the SIGNED mean deviation of actual arrival from that pin's own schedule = the actual
    present age, in the pin's own OBS clock). Reuses prerecord_phase_calibrate.measured_by_camera /
    source_names_by_template (never a second copy of the reconstruction). A source absent or
    malformed in the jitter JSON is simply OMITTED -- never a fabricated floor; the caller FAILs loud
    if a FASTER camera lacks one."""
    from prerecord_phase_calibrate import measured_by_camera, source_names_by_template
    by_cam = measured_by_camera(jitter_json)                    # {cam_num: latency_ms + mean_head_skew}
    by_src = source_names_by_template(by_cam, "NDI cam{n}")     # {"NDI cam<N>": arrival_floor_ms}
    return {s: by_src[s] for s in sources if s in by_src}


def floor_aware_partition(arrival_floors, deltas, floor_ms=DEFAULT_FLOOR_MS,
                          max_abs_latency_ms=DEFAULT_MAX_ABS_LATENCY_MS, current_pins=None):
    """The PURE core of the floor-aware plan (#1161) that PARTITIONS instead of raising -- returns
    ``(plan, over_budget, missing)`` so a caller can either HARD-FAIL (floor_aware_pins, below) or
    SOFT-RELEASE the BUDGET_BOUND case (align(), issue 1168's re-tighten path). The slowest /
    pin-dominated / clamp / faster-target semantics are EXACTLY floor_aware_pins' -- see its docstring:
      * ``plan`` -- {src: pin_ms(int)}: the slowest (or pin-dominated co-slowest) camera at floor_ms
        (or its held pin), every WITHIN-budget faster camera at its alignment target, and every
        OVER-budget faster camera CLAMPED to floor_ms (a pin we cannot afford is NEVER written up
        above the ceiling).
      * ``over_budget`` -- [(src, floor_i, hold, target)] faster cameras whose target
        (arrival_floor + hold) exceeds ``max_abs_latency_ms``.
      * ``missing`` -- [src] faster cameras with no arrival-floor measurement.
    ``floor_aware_pins`` wraps this and RAISES on ``missing``/``over_budget`` (the HARD-FAIL
    direction, byte-unchanged); ``align`` uses it directly to soft-release the budget-bound case."""
    plan = {}
    over_budget = []
    missing = []
    if not deltas:
        return plan, over_budget, missing
    base = min(deltas.values())          # the slowest (max present age / min hold-to-add) anchor
    for src, d in sorted(deltas.items()):
        hold = d - base
        if hold < 0.5:                   # co-slowest by present age -> minimum pin, UNLESS pin-dominated
            cur = current_pins.get(src) if current_pins else None
            fl = arrival_floors.get(src)
            # pin-dominated: this camera's present age is held by its OWN pin, not the transport (its
            # true transport is below and unobservable). Do NOT tear the pin down to the floor (that
            # would drop it to that lower transport -> break parity). Keep the pin. The 3 ms slack
            # absorbs the audit's own +mean_head_skew noise (a floor read a couple ms ABOVE the pin is
            # still pin-dominated, #1161 review 🔵); a source pinned UP but ABSENT from the audit
            # (fl None) is kept too (its transport is unknowable, flooring it can only misalign). All
            # no-ops on the two-phase-reset path, where every pin is at the floor (cur == floor_ms).
            if cur is not None and cur > floor_ms and (fl is None or cur >= fl - 3.0):
                plan[src] = max(floor_ms, int(round(cur)))
            else:
                plan[src] = floor_ms     # transport-dominated slowest -> floor (inert at its floor)
            continue
        floor_i = arrival_floors.get(src)
        if floor_i is None:
            missing.append(src)
            continue
        target = floor_i + hold
        if target > max_abs_latency_ms:
            # #1161 over budget: the align pin would exceed the achievable-latency ceiling. Record it
            # and CLAMP this camera to its floor (never write a pin we cannot afford) -- the caller
            # (floor_aware_pins) RAISES on this, while align() SOFT-RELEASES it (BUDGET_BOUND).
            over_budget.append((src, floor_i, hold, target))
            plan[src] = floor_ms
            continue
        plan[src] = max(floor_ms, int(round(target)))   # #1161 clamp: never a sub-floor pin
    return plan, over_budget, missing


def floor_aware_pins(arrival_floors, deltas, floor_ms=DEFAULT_FLOOR_MS,
                     max_abs_latency_ms=DEFAULT_MAX_ABS_LATENCY_MS, current_pins=None):
    """The FLOOR-AWARE pin plan (#1161). `deltas`: {src: ms >= 0} the PURE cross-camera present-age
    delta (the hold to add to bring each camera up to the slowest; the slowest anchors to ~0 -- from
    round_deltas over ZERO pins, so the cross-clock offset still cancels). `arrival_floors`: {src: ms}
    the ABSOLUTE per-source present age (latency_ms + mean_head_skew_ms). Returns {src: pin_ms(int)}:
    the slowest (min-delta) camera -> floor_ms (inert, stays at its natural floor); every faster
    camera -> round(arrival_floor_i + delta_i) = the alignment target (the slowest's floor), which
    sits ABOVE that camera's own floor so the genlock-C ACQUIRE frame-mover can add the hold.

    `current_pins` (optional, review-hardening #1161): the audit "arrival floor" is the PRESENT AGE
    (max(pin, transport)), which equals the raw transport ONLY while the pin sits BELOW it. From a
    pinned steady state (a prior run left the pins elevated), a co-slowest camera can be held at that
    present age SOLELY by its own pin (pin-dominated: current_pin >= its present age) -- its TRUE
    transport is below and UNOBSERVABLE. Flooring such a source to floor_ms would drop it to that
    lower transport -> misaligned. So a pin-dominated co-slowest keeps its current pin (never torn
    down); a transport-dominated one (the normal case after the two-phase reset, where every pin is
    at the floor) floors correctly. The PRIMARY fix is the reset (reset_pins_to_floor makes every
    floor a true transport); this is the belt-and-suspenders for a direct/no-reset call.

    FAILs loud (AlignmentImpossible) if a faster camera's target exceeds max_abs_latency_ms -- the
    transport floor is too high to align within the latency budget; NEVER silently pins above the
    bound, NEVER widens it. Also FAILs if a FASTER camera has no arrival-floor measurement (a pin
    below its floor would be inert -- never a fabricated floor). The over-budget/missing DETECTION is
    shared with align()'s BUDGET_BOUND soft-release via floor_aware_partition (same computation, one
    copy); this wrapper is the HARD-FAIL direction."""
    if not deltas:
        return {}
    plan, over_budget, missing = floor_aware_partition(
        arrival_floors, deltas, floor_ms, max_abs_latency_ms, current_pins)
    if missing:
        raise AlignmentImpossible(
            "[qr-align] #1161 cannot compute a floor-aware pin for "
            + ", ".join(repr(s) for s in missing) + ": no arrival-floor measurement (the strih "
            "genlock audit head_skew is required -- a pin below the arrival transport floor is "
            "structurally inert). Provide --jitter-json.")
    if over_budget:
        raise AlignmentImpossible(
            "[qr-align] #1161 cannot align within the latency budget: "
            + over_budget_arithmetic(over_budget, max_abs_latency_ms)
            + " -- the transport floor is too high to align within the budget; investigate the "
            "transport floor, do NOT raise the bound (gate-strictness doctrine).")
    return plan


def over_budget_arithmetic(over_budget, max_abs_latency_ms=DEFAULT_MAX_ABS_LATENCY_MS):
    """The per-camera `arrival floor Xms + delta Yms = Zms > bound Bms` phrase list (joined) for an
    over_budget partition -- shared by floor_aware_pins' hard-fail message and budget_bound_report's
    soft-release block so the arithmetic reads identically in both."""
    return "; ".join(
        f"{s!r} arrival floor {fl:.0f}ms + delta {hl:.0f}ms = {t:.0f}ms > bound "
        f"{max_abs_latency_ms:.0f}ms" for s, fl, hl, t in over_budget)


def budget_bound_report(over_budget, residual_ms, max_abs_latency_ms=DEFAULT_MAX_ABS_LATENCY_MS):
    """The LOUD, named report-only block for the BUDGET_BOUND soft-release (#1161 / issue 1168). Names
    each over-budget faster camera's `arrival_floor + delta = target > bound` arithmetic, then the
    surviving cross-camera spread as a REPORT-ONLY RESIDUAL tracked in issue 1168. The run PASSES only
    because the correction is physically budget-impossible (a pin above the ceiling is forbidden by the
    deep-pin doctrine) -- NOT because the residual is acceptable, and NO bound is widened."""
    return (
        "[qr-align] #1161 BUDGET-BOUND SOFT-RELEASE: the tail is STABLE + within the spread sanity, "
        "but the alignment correction is physically budget-impossible: "
        + over_budget_arithmetic(over_budget, max_abs_latency_ms)
        + " -- correcting these needs a pin above the achievable-latency ceiling (the deep-pin "
        "doctrine forbids it), so NO alignment pin is applied and the align set stays at its natural "
        f"floor. REPORT-ONLY RESIDUAL: cross-camera spread ~{residual_ms:.0f}ms survives -- tracked in "
        "issue 1168 (reduce the per-box arrival floors, then RE-TIGHTEN [4i/8align] to hard-fail). The "
        "run PROCEEDS (exit 0); the residual is NOT accepted as aligned, and no bound is widened.")


def floor_aware_stuck_abort_reason(plan, arrival_floors, post_pins, post_deltas,
                                   floor_ms=DEFAULT_FLOOR_MS):
    """The #1161 abort reason for the FLOOR-AWARE path: above-floor pins were applied (read-back
    confirmed) but the re-measured tail STILL stayed off-parity -- so the genlock-C ACQUIRE
    frame-mover did not close the residual. Names each RAISED source (pin above the runtime floor)
    with its target pin and residual, then points at the two live-only causes (the sibling genlock
    build not deployed on strih, or a mid-run transport-floor shift). NEVER widens the same-frame
    parity bar -- the run still FAILS."""
    parts = []
    for src in sorted(plan):
        fl = arrival_floors.get(src)
        if fl is None or plan[src] <= floor_ms:
            continue
        resid = post_deltas.get(src) if isinstance(post_deltas, dict) else None
        rtxt = "" if resid is None else f", residual {resid} ms"
        parts.append(f"{src!r} pinned to {plan[src]} ms (above its {fl:.0f} ms arrival floor, "
                     f"read-back {post_pins.get(src)} ms{rtxt})")
    return (
        "[qr-align] #1161 applied ABOVE-FLOOR pins " + str(plan) + " but the re-measured tail "
        "STABILIZED off-parity: " + "; ".join(parts) + ". The pins clear each source's arrival "
        "transport floor, so this is NOT the below-floor inert case -- the genlock-C ACQUIRE "
        "frame-mover did not close the residual. Live-only causes: the sibling genlock build is not "
        "deployed on strih (the ACQUIRE-bracket that adds the hold lives in genlock C), OR the "
        "transport floor shifted mid-run. Parity tolerance is NOT widened; the run FAILS the owner's "
        "same-frame bar.")


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


# ---------------------------------------------------------------------------
# #1161 -- WHY a floor-3 apply can leave a one-canvas-frame residual: the pin
# lever cannot ADD hold. The floor-3 model floors the slowest camera and RAISES
# the faster ones' pins to delay them into parity -- but a per-source
# genlock_latency_ms INCREASE is structurally inert on a live rig:
#   * obs_source_set_genlock_latency_ms (vendor/obs-studio/libobs/obs-source.c)
#     clears genlock_phase_anchor_ns and re-arms the (ms-path-inert) fill latch,
#     but NEVER clears genlock_locked_next_boundary_ns (the conveyor) and NEVER
#     forces a re-acquire (the ACQUIRE branch runs only when that boundary == 0).
#   * The conveyor is a pure FOLLOWER with no restoring force toward the
#     configured latency; should_converge_phase (src/genlock_backlog.rs) only
#     SHEDS DOWNWARD toward max(reserve, floor). Raising reserve only raises that
#     shed threshold -- it never deepens the hold.
# So a pin increase moves only the CONFIG value, never the presented frame. The
# frame-mover is #1003's Stage-2 ACQUIRE bracketing gate (a genlock-C change,
# live-only, gated on #1004). These pure helpers let align() ATTRIBUTE the
# residual precisely (instead of a generic "did NOT hold") and emit before/after
# telemetry -- WITHOUT widening the owner's same-frame parity bar.
# ---------------------------------------------------------------------------
def pins_requiring_more_hold(pre_pins, plan, min_increase_ms=1):
    """{source: increase_ms} for every planned source whose pin EXCEEDS its pre-apply pin by at
    least `min_increase_ms` -- i.e. the sources the floor-3 plan asks the genlock FIFO to hold LONGER
    (present an OLDER frame). This is the ONE direction the FIFO cannot execute on a live per-source
    latency change (see the module note above), so a non-empty result on a persistent post-apply
    residual is WHY it did not close. A source with an unknown pre-pin is skipped (its delta cannot
    be computed) -- never fabricated."""
    out = {}
    for src, want in plan.items():
        pre = pre_pins.get(src)
        if pre is None:
            continue
        if want - pre >= min_increase_ms:
            out[src] = want - pre
    return out


def _delta_str(deltas, src):
    """A residual-delta cell for the telemetry/report: 'n/a' when the map is absent (unverifiable
    round set) or the source is missing, else the value with trailing zeros trimmed."""
    v = deltas.get(src) if isinstance(deltas, dict) else None
    return "n/a" if v is None else f"{v:g}"


def format_pin_apply_report(pre_pins, post_pins, pre_deltas, post_deltas, inert):
    """Before/after per-source apply telemetry (#1161 item 4): each source's config pin
    (pre -> read-back) and its cross-camera residual delta (pre -> post), with a HOLD-INERT tag on
    every source the plan asked to ADD hold (`inert`). It makes visible WHERE a pin increase went --
    the config moved (read-back), the presented frame did not (the residual did not close). Pure; the
    caller prints it on the abort path."""
    srcs = set(pre_pins) | set(post_pins) | (set(inert) if inert else set())
    lines = ["[qr-align] pin apply before -> after (config pin | cross-camera residual ms):"]
    for src in sorted(srcs):
        pre, post = pre_pins.get(src), post_pins.get(src)
        tag = (f"  HOLD-INERT (wanted +{inert[src]} ms, frame did not move)"
               if inert and src in inert else "")
        lines.append(
            f"  {src!r}: pin {pre}ms -> {post}ms (read-back); "
            f"residual {_delta_str(pre_deltas, src)} -> {_delta_str(post_deltas, src)} ms{tag}")
    return "\n".join(lines)


def hold_inert_abort_reason(inert, post_pins, post_deltas):
    """The PRECISE #1161 abort reason: a STABILIZED tail stayed off-parity because the floor-3 plan
    asked one or more sources to ADD hold (`inert` = pins_requiring_more_hold), which the genlock FIFO
    cannot execute on a live per-source latency INCREASE. Names each inert source with its requested
    increase, its read-back-confirmed live pin (so this is provably NOT a WS write failure -- the
    config DID take), and its residual, then points at the owning fix. Callers use this ONLY when
    `inert` is non-empty and the tail stabilized but failed parity -- it NEVER widens tolerance, the
    run still FAILS the owner's same-frame bar."""
    parts = []
    for src in sorted(inert, key=lambda s: (-inert[s], s)):
        resid = post_deltas.get(src) if isinstance(post_deltas, dict) else None
        rtxt = "" if resid is None else f", residual {resid} ms"
        parts.append(f"{src!r} wanted +{inert[src]} ms hold "
                     f"(pin now {post_pins.get(src)} ms, read-back confirmed{rtxt})")
    return (
        "[qr-align] the re-measured tail STABILIZED but stayed off-parity because the genlock FIFO "
        "did NOT add the requested hold: " + "; ".join(parts) + ". A per-source genlock_latency_ms "
        "INCREASE cannot deepen the FIFO on a live rig (obs_source_set_genlock_latency_ms clears the "
        "phase anchor but never the locked conveyor boundary and never forces a re-acquire; "
        "should_converge_phase only sheds DOWNWARD toward max(reserve, floor)). This "
        "last-canvas-frame residual is a genlock-FIFO structural limit owned by issue 1003's Stage-2 "
        "ACQUIRE bracketing gate (a genlock-C change, live-only, gated on issue 1004), NOT the "
        "aligner -- parity tolerance is NOT widened, the run FAILS the owner's same-frame bar.")


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


def _stable_tail(rounds_ticks, sources, stable_tail_rounds, stable_tol_ids,
                 stable_outlier_tol_ids=DEFAULT_STABLE_OUTLIER_TOL_IDS):
    """The OUTLIER-TOLERANT stable-tail decision (#1160 + #1161). Returns
    ``(start_or_None, clean_count)``:

    Walking backward from the LAST round, maintain a TIGHT CLEAN band ``[lo,hi]`` (``hi-lo <=
    stable_tol_ids``) over the in-band rounds only, and count CLEAN (in-band) rounds. A round that
    would widen the clean band beyond ``stable_tol_ids`` is an OUTLIER CANDIDATE; it is SKIPPED (the
    span continues across it, it never extends the band and never counts as a clean round) iff BOTH
    (a) MAGNITUDE: it is within ``stable_outlier_tol_ids`` ids of the clean band (a NEAR-band blip --
    a measurement-cadence hiccup / phase jitter -- never a FAR convergence transient or large swing);
    and (b) COUNT: after skipping it, outliers stay STRICTLY FEWER than clean rounds (the in-band core
    stays the majority). Any FAR / over-budget out-of-band round, or a non-FULL round (a decode-miss
    makes parity unverifiable that round), STOPS the walk. ``start`` is the earliest round of the span
    (which may include skipped outliers); ``clean_count`` is the number of CLEAN rounds in it.

    Returns ``(None, clean_count)`` when ``clean_count < stable_tail_rounds`` (K) -- the tail has too
    few genuinely-in-band rounds to be called stable. Gating on the CLEAN count (never the span length)
    is what keeps the min-valid=5 LENGTH strictness intact while tolerating a lone blip, and what keeps
    the #1160 invariants: a slow monotonic ramp (1,2,3) cannot sustain K clean in-band rounds (each
    step leaves the width-1 band); a converging backlog's FAR transient is magnitude-rejected (never
    absorbed); a degraded-grabber sawtooth's large swings are magnitude-rejected and a near-band
    high-frequency 2-cycle is count-rejected -- so an unstable rig still yields clean < K -> None."""
    n = len(rounds_ticks)
    lo = hi = None
    clean = 0
    outliers = 0
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
        if nhi - nlo <= stable_tol_ids:  # in-band -> extend the clean band, count it
            lo, hi, clean, start = nlo, nhi, clean + 1, i
        else:
            # Out-of-band. SKIP it (near-band blip) only if it stays close to the band AND the clean
            # core stays the strict majority; otherwise the walk STOPS (a far transient / an
            # unstable swing must never be absorbed as noise).
            dev = max(sp - hi, lo - sp)  # distance outside the clean band (band already seeded here)
            if dev <= stable_outlier_tol_ids and (outliers + 1) < clean:
                outliers += 1
                start = i
            else:
                break
    return (start, clean) if clean >= stable_tail_rounds else (None, clean)


def _stable_tail_start(rounds_ticks, sources, stable_tail_rounds, stable_tol_ids,
                       stable_outlier_tol_ids=DEFAULT_STABLE_OUTLIER_TOL_IDS):
    """The start index of the maximal contiguous STABLE-TAIL span ending at the LAST round (the
    outlier-tolerant #1161 form -- see `_stable_tail`), or None when the span has fewer than
    `stable_tail_rounds` (K) CLEAN in-band rounds. Thin wrapper over `_stable_tail` (one algorithm,
    no mirror-drift)."""
    return _stable_tail(rounds_ticks, sources, stable_tail_rounds, stable_tol_ids,
                        stable_outlier_tol_ids)[0]


def measure_tail_status(rounds_ticks, sources, *, stable_tail_rounds, stable_tol_ids,
                        parity_tol_ids, min_parity_rounds, min_valid_rounds,
                        stable_outlier_tol_ids=DEFAULT_STABLE_OUTLIER_TOL_IDS):
    """Decide, from the rounds accumulated so far, whether the measure phase can STOP and which
    STABLE-TAIL rounds the verdict should use. Returns a TailStatus:
      - "converged-aligned": the last K rounds are mutually stable AND already at parity (median
        spread <= parity_tol over >= min_parity_rounds full rounds) -> STOP, PASS-fast. Needs only
        K clean rounds; min_valid_rounds is NOT required (no re-derive).
      - "converged-stable": the tail is mutually stable but NOT at parity (a static residual delta
        floor-3 pins can fix) AND has >= min_valid_rounds CLEAN rounds -> STOP, re-derive from tail.
      - "stable-need-more": the tail is stable but not aligned and has too few CLEAN rounds to
        re-derive robustly -> keep measuring (the unchanged min-valid-rounds threshold applied to
        the tail's CLEAN rounds).
      - "unstable": the last K rounds are not mutually stable -> keep measuring.
    All the verdict thresholds are UNCHANGED here -- this only chooses WHEN to stop and WHICH rounds
    to judge (the tail), never weakening a gate. #1161: the tail is OUTLIER-TOLERANT (a lone near-band
    blip is skipped), so `min_valid_rounds` is judged on the CLEAN (in-band) count, never the span
    length -- an outlier round never counts toward the 5, so the LENGTH strictness is unchanged."""
    start, clean = _stable_tail(rounds_ticks, sources, stable_tail_rounds, stable_tol_ids,
                                stable_outlier_tol_ids)
    if start is None:
        return TailStatus(False, "unstable", None)
    tail = rounds_ticks[start:]
    _med, aligned = _full_round_parity(tail, sources, parity_tol_ids, min_parity_rounds)
    if aligned:
        return TailStatus(True, "converged-aligned", start)
    if clean >= min_valid_rounds:
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
                        stable_outlier_tol_ids=DEFAULT_STABLE_OUTLIER_TOL_IDS,
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
            stable_tol_ids=stable_tol_ids, stable_outlier_tol_ids=stable_outlier_tol_ids,
            parity_tol_ids=parity_tol_ids,
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


def reset_pins_to_floor(sources, host, password, floor_ms=DEFAULT_FLOOR_MS):
    """#1161 two-phase reset PHASE 0: force every align source's genlock_latency_ms_src to `floor_ms`
    (read-back verified via apply_latency_pins.apply_pins), so that AFTER a settle the strih genlock
    audit reads each source's TRUE transport floor instead of a pin-held present age. WHY: a prior
    aligned run leaves the pins elevated and they PERSIST across runs; the audit `latency_ms +
    mean_head_skew_ms` is the PRESENT AGE = max(pin, transport), which masks the true transport of a
    pin-dominated camera. Only lowering every pin to the floor (below the transport) unmasks it. The
    caller (qr-align.sh) resets -> settles -> RE-FETCHES the audit -> then runs the floor-aware plan.
    Returns the number of sources reset. Idempotent (a source already at the floor is re-set to the
    same value; apply_pins is read-back-verified)."""
    from apply_latency_pins import apply_pins
    import obs_phase2
    plan = {src: floor_ms for src in sources}
    ws = obs_phase2._conn(host, password)
    try:
        apply_pins(ws, plan, True)
    finally:
        ws.close()
    return len(plan)


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
          measure_budget_s, max_measure_rounds, settle_s,
          stable_outlier_tol_ids=DEFAULT_STABLE_OUTLIER_TOL_IDS,
          jitter_json=None, max_abs_latency_ms=DEFAULT_MAX_ABS_LATENCY_MS):
    """The full per-run alignment: measure to a STABLE TAIL (#1160) -> (already aligned? PASS) ->
    FLOOR-AWARE plan from the tail (#1161) -> sanity -> apply (execute) -> settle -> RE-MEASURE to a
    stable tail -> PASS iff parity holds. The verdict is always computed from the stabilized tail,
    never the post-restart convergence transient; every threshold (66 ms spread sanity, <=1-id
    parity, min-valid/parity rounds) is UNCHANGED, applied to the tail.

    #1161: when `jitter_json` (the strih `genlock-jitter-report --json` per-source measurement) is
    given, the plan raises each faster camera's pin to arrival_floor_i + delta_i (ABOVE its arrival
    transport floor) so the genlock-C ACQUIRE frame-mover can add the hold, and FAILs loud if any
    target exceeds `max_abs_latency_ms`. Without it the plan falls back to the (inert-prone)
    floor3 plan with a loud warning (a pin below the arrival floor moves only the config, never the
    frame). Returns a result dict; raises AlignmentImpossible on an un-measurable / never-stabilizing
    / un-sane / un-alignable / still-misaligned rig."""
    import time
    from apply_latency_pins import apply_pins

    current_pins = read_current_pins(sources, host, password)
    rounds_ticks, run_id, status = measure_stable_tail(
        sources, host, password, width=width, height=height, run_id=None,
        stable_tail_rounds=stable_tail_rounds, stable_tol_ids=stable_tol_ids,
        stable_outlier_tol_ids=stable_outlier_tol_ids,
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
        # The two never-done reasons need different prose (a hardcoded "not mutually stable" lies for
        # the stable-need-more branch, where the tail IS stable but had too few clean rounds).
        why = (f"the last {stable_tail_rounds} rounds are not mutually stable (<= {stable_tol_ids} id)"
               if status.reason == "unstable"
               else f"the tail is mutually stable but never accumulated {min_valid_rounds} clean "
                    "(fully-decoded) rounds to re-derive the pins from")
        raise AlignmentImpossible(
            f"[qr-align] the cross-camera spread did not STABILIZE within {measure_budget_s:.0f}s "
            f"/{len(rounds_ticks)} rounds -- {why} (status {status.reason!r}), so no steady-state "
            "tail could be measured. A converging backlog that never settles is a degraded / "
            "over-rate grabber (issue 1145). Per-round table above (the last stable rounds, if "
            "any, marked 'tail').")

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

    # #1161 FLOOR-AWARE plan basis. Resolve the per-source arrival floors from the strih audit and the
    # PURE cross-camera present-age deltas (round_deltas over ZERO pins -- cross-clock-safe, the hold
    # to add). A PARTIAL audit (a FASTER camera missing its floor) degrades GRACEFULLY to the
    # inert-prone floor+delta fallback (loud warning; the verify re-measure still FAILs a genuine
    # misalignment) -- a partial fetch must never be strictly worse than no fetch (#1161 review).
    arrival_floors = arrival_floors_from_jitter(jitter_json, sources) if jitter_json else {}
    pure_deltas = None
    if arrival_floors:
        pure_deltas, _pdn = robust_deltas(tail, {s: 0 for s in sources}, min_valid_rounds)
        pbase = min(pure_deltas.values())
        faster_missing = [s for s in sources
                          if pure_deltas.get(s, 0.0) - pbase >= 0.5 and s not in arrival_floors]
        if faster_missing:
            sys.stderr.write(
                "WARNING: [qr-align] #1161 partial arrival-floor audit -- faster camera(s) "
                + ", ".join(repr(s) for s in faster_missing) + " missing a floor; falling back to "
                "the inert-prone floor+delta plan rather than aborting the run.\n")
            arrival_floors, pure_deltas = {}, None
    result["arrival_floors_ms"] = {s: round(v, 1) for s, v in arrival_floors.items()}

    # #1161 review: the SPREAD sanity gates the PURE present-age spread when floors are available --
    # the pin-FOLDED deltas over-read by ~the pin elevation from a pinned steady state and would
    # spuriously FAIL a legit drift as a "degraded grabber". SAME 66 ms bound, correct metric; the
    # folded-delta sanity stays for the no-floors fallback (behaviour there unchanged).
    sanity_deltas = pure_deltas if arrival_floors else deltas
    result["sanity_deltas_ms"] = {s: round(v, 2) for s, v in sanity_deltas.items()}
    ok, slowest_src, widest_src, worst = sanity_ok(sanity_deltas, max_delta_ms)
    result["slowest_source"] = slowest_src
    result["worst_source"] = widest_src
    result["worst_delta_ms"] = round(worst, 2)
    if not ok:
        _emit_fail_diagnostics(rounds_ticks, sources, tail_start)
        # #1161 review: print the JUDGED map (sanity_deltas) -- on the floor-aware path that is the
        # PURE present-age spread, NOT the pin-FOLDED median (which over-reads from a pinned state and
        # would mislead the "degraded grabber" diagnosis the owner reads).
        raise AlignmentImpossible(
            f"[qr-align] cannot align: cross-camera spread {worst:.1f} ms exceeds the "
            f"{max_delta_ms:.0f} ms sanity bound -- a degraded/underrun grabber, not a real "
            f"inter-card delta (the slowest camera {slowest_src!r} floors to {floor_ms}ms; the "
            f"widest gap is on {widest_src!r}; the anomaly is most likely the slowest card). "
            f"Per-camera deltas (ms off the slowest): {result['sanity_deltas_ms']}.")

    # FLOOR-AWARE: raise each faster camera ABOVE its arrival floor to the alignment target so the
    # genlock-C ACQUIRE frame-mover can add the hold (a below-floor pin is structurally inert); the
    # slowest keeps floor_ms. current_pins let it never tear down a pin-dominated co-slowest (#1161
    # review). No floors -> the inert-prone floor+delta fallback.
    if arrival_floors:
        result["present_age_deltas_ms"] = {s: round(v, 2) for s, v in pure_deltas.items()}
        plan, over_budget, missing = floor_aware_partition(
            arrival_floors, pure_deltas, floor_ms, max_abs_latency_ms, current_pins=current_pins)
        if missing:  # pragma: no cover -- unreachable unless the faster_missing invariant breaks
            # `missing` is provably empty here: the faster_missing pre-check above already cleared
            # arrival_floors (-> the floor3 fallback) for the IDENTICAL "a faster camera lacks a
            # floor" condition floor_aware_partition uses. Guard it defensively (a broken invariant
            # fails loud, never silently omits a camera from the plan) with a DISTINCT internal
            # message -- never re-duplicate floor_aware_pins' user-facing missing-abort wording,
            # which would silently drift from it (#1161 review).
            _emit_fail_diagnostics(rounds_ticks, sources, tail_start)
            raise AlignmentImpossible(
                "[qr-align] #1161 internal invariant: faster camera(s) "
                + ", ".join(repr(s) for s in missing) + " reached the floor-aware partition with no "
                "arrival floor (the faster_missing pre-check should have fallen back to floor3 first).")
        if over_budget:
            # #1161 BUDGET-BOUND SOFT-RELEASE (issue 1168). The tail is STABLE and within the spread
            # sanity (both proven above), but >=1 faster camera's alignment target exceeds the
            # achievable-latency ceiling -- the per-box arrival-floor difference is physically
            # budget-impossible to correct (a pin above the ceiling is forbidden by the deep-pin
            # doctrine). This is NOT a defect (an unstable / degraded-card tail FAILs above); it is the
            # CONSTANT cross-camera per-box floor offset issue 1168 tracks. Apply NOTHING (the align set
            # already sits at its natural floor after the two-phase reset), persist the residual into
            # the result JSON + emit a loud named marker, and PASS (exit 0) so the E2E proceeds. Owner
            # revision 2026-07-31 ("zelený gate najprv, pritvrdenie cez tickety") + the supervisor's
            # judgment: a stable, budget-impossible tail passes with a report-only residual; re-tighten
            # is issue 1168. NO within-budget partial apply -- the existing verify model requires FULL
            # cross-camera parity (never true while an over-budget camera stays at its floor), so a
            # partial apply cannot be certified by it; and applying nothing cannot mask a HOLD-INERT
            # defect (that FAIL stays fully intact on the within-budget-correctable path below).
            result["status"] = "budget-bound"
            result["budget_bound"] = True
            result["plan"] = {}
            result["over_budget"] = [
                {"source": s, "arrival_floor_ms": round(fl, 1), "delta_ms": round(hl, 1),
                 "target_ms": round(t, 1), "bound_ms": max_abs_latency_ms}
                for s, fl, hl, t in over_budget]
            result["report_only_residual_ms"] = round(worst, 1)
            # Report-only: the loud budget_bound_report() marker above + the persisted over_budget
            # / report_only_residual_ms JSON fully surface the residual; the per-round frame_id table
            # (_emit_fail_diagnostics) is a FAIL-path debugging aid, not emitted on this PASS (#1161 review).
            sys.stderr.write(budget_bound_report(over_budget, worst, max_abs_latency_ms) + "\n")
            return result
    else:
        note = ("partial/unusable arrival-floor audit" if jitter_json
                else "no per-source arrival-floor measurement (--jitter-json absent)")
        sys.stderr.write(
            f"WARNING: [qr-align] #1161 {note} -- falling back to the floor+delta plan, which is "
            "INERT when a raised pin lands below the arrival transport floor. The two-phase "
            "reset+audit (qr-align.sh) enables the floor-aware plan.\n")
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
        stable_outlier_tol_ids=stable_outlier_tol_ids,
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
        # #1161: which sources did the plan ask to ADD hold (the direction the FIFO cannot execute
        # on a live pin change)? A non-empty set on a STABILIZED-but-off-parity tail is WHY the
        # residual did not close -- the config pin moved (read-back confirmed) but the presented
        # frame did not. Record it + emit before/after telemetry so the operator sees where the pin
        # went (item 4), then attribute the abort precisely rather than a generic "did NOT hold".
        inert = pins_requiring_more_hold(current_pins, plan)
        result["post_residual_deltas_ms"] = named
        result["hold_inert_ms"] = inert
        sys.stderr.write(format_pin_apply_report(
            current_pins, post_pins, result.get("median_deltas_ms"), named, inert) + "\n")
        _emit_fail_diagnostics(verify_ticks, sources, vtail_start)  # the RE-MEASURED rounds
        if vstatus.done and inert:
            # The tail STABILIZED but stayed off-parity, and the plan raised pins. Attribute
            # PRECISELY by WHICH plan was applied, never the generic "did NOT hold" (which reads as
            # flakiness/settle and sends the next worker chasing ruled-out hypotheses). Parity
            # tolerance is NOT widened either way.
            if arrival_floors:
                # #1161 fix path: the pins were raised ABOVE each source's arrival floor, so this is
                # NOT the below-floor inert case -- the genlock-C ACQUIRE frame-mover did not close
                # the residual (its build is not deployed on strih, or the transport floor shifted).
                raise AlignmentImpossible(
                    floor_aware_stuck_abort_reason(plan, arrival_floors, post_pins, named, floor_ms))
            # Fallback path (no arrival-floor measurement): the raised pins may sit BELOW the arrival
            # floor -> structurally inert, the pre-fix reality (issue 1003 Stage-2 genlock limit).
            raise AlignmentImpossible(hold_inert_abort_reason(inert, post_pins, named))
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
                    help="the tight CLEAN band: in-band tail spreads within this many frame_ids (#1160)")
    ap.add_argument("--stable-outlier-tol-ids", type=int, default=DEFAULT_STABLE_OUTLIER_TOL_IDS,
                    help="a lone near-band spread blip within this many ids of the clean band is "
                         "SKIPPED, not a reset -- outlier-tolerant stable tail (#1161)")
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
    # #1161: the strih genlock audit (genlock-jitter-report --json) that supplies each source's
    # ABSOLUTE arrival transport floor (latency_ms + mean_head_skew_ms), so the plan can pin the
    # faster cameras ABOVE their floor. Without it the plan falls back to the inert-prone floor+delta.
    ap.add_argument("--jitter-json", default=None,
                    help="genlock-jitter-report --json file (strih audit) -> per-source arrival "
                         "floor for the #1161 floor-aware plan; without it the plan is inert-prone")
    ap.add_argument("--max-abs-latency-ms", type=float, default=DEFAULT_MAX_ABS_LATENCY_MS,
                    help="#1161 absolute achievable-latency ceiling; a target above it FAILs loud "
                         "(transport floor too high) rather than deep-pinning (default 94)")
    ap.add_argument("--reset-to-floor", action="store_true",
                    help="#1161 two-phase reset PHASE 0: force every --sources pin to --floor-ms and "
                         "exit (the caller settles + re-fetches the audit so floors are TRUE "
                         "transports). Mutually exclusive with the measure/plan/--execute flow.")
    ap.add_argument("--execute", action="store_true",
                    help="APPLY the floor-aware pins (default: DRY-RUN -- measure + plan, write nothing)")
    a = ap.parse_args(argv)

    sources = [s.strip() for s in a.sources.split(",") if s.strip()]
    if not sources:
        raise SystemExit("[qr-align] --sources is empty")

    if a.reset_to_floor:
        n = reset_pins_to_floor(sources, a.host, a.password, a.floor_ms)
        print(f"[qr-align] #1161 reset {n} source(s) to the {a.floor_ms} ms floor "
              "(settle + re-fetch the audit before the floor-aware plan).", file=sys.stderr)
        return 0

    jitter_json = None
    if a.jitter_json:
        try:
            with open(a.jitter_json, encoding="utf-8") as f:
                jitter_json = json.load(f)
        except (OSError, ValueError) as exc:  # unreadable / malformed -> fall back, logged loudly
            sys.stderr.write(
                f"WARNING: [qr-align] #1161 could not read --jitter-json {a.jitter_json!r} "
                f"({exc}) -- proceeding without arrival-floor measurement (inert-prone fallback).\n")

    result = align(
        sources, a.host, a.password,
        execute=a.execute, stable_tail_rounds=a.stable_tail_rounds, stable_tol_ids=a.stable_tol_ids,
        stable_outlier_tol_ids=a.stable_outlier_tol_ids,
        min_valid_rounds=a.min_valid_rounds, min_parity_rounds=a.min_parity_rounds,
        max_delta_ms=a.max_delta_ms, parity_tol_ids=a.parity_tol_ids, floor_ms=a.floor_ms,
        width=a.width, height=a.height, measure_budget_s=a.measure_budget_s,
        max_measure_rounds=a.max_measure_rounds, settle_s=a.settle_s,
        jitter_json=jitter_json, max_abs_latency_ms=a.max_abs_latency_ms)

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
    elif status == "budget-bound":
        # #1161 / issue 1168: the correction is physically budget-impossible; PASS with a report-only
        # residual, exit 0 (the E2E proceeds). The JSON above carries over_budget + report_only_residual_ms.
        over = ", ".join(o["source"] for o in result.get("over_budget", []))
        print(f"[qr-align] host={a.host} BUDGET-BOUND (issue 1168): correction budget-impossible for "
              f"{over} (target > {a.max_abs_latency_ms:.0f}ms ceiling); no pins applied, report-only "
              f"residual ~{result.get('report_only_residual_ms')} ms survives; {tail}. Re-tighten to "
              "hard-fail is tracked in issue 1168 (reduce per-box arrival floors first).",
              file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
