#!/usr/bin/env python3
"""Issue 1460 slice 1 -- the SINE-CONTINUITY analysis of a recorded VBAN pilot.

The lv1 emulator (scripts/vban_emulator.py) sends a continuous pilot sine whose phase comes from the
SENDER's sample counter. Whatever the receiver does to the timeline shows in one recorded track:

  * a dropped or repeated sample, a re-anchor, a copy-padded loss = a PHASE STEP;
  * a lost packet played as silence for exactly its slot = a SILENCE SLOT across which the phase
    continues as if the packet had played (the issue-1439 receiver's contract);
  * the sender's clock offset, followed by the receiver and re-clocked by the genlock ASRC = a pilot
    whose frequency in the recording is f x (1 + ppm): the MEASURED rate offset.

THE MEASUREMENT (`analyze`, pure, numpy):
  1. Silence slots: RMS of 16-sample chunks (a third of a 997 Hz period, so a zero crossing never
     reads quiet) against the pilot's own level; a run of >= 4 chunks below SILENCE_DB inside the
     active span is a slot. An AAC round trip (OBS's recording encoder) keeps a 256-sample hole
     below -22 dB to 16-sample resolution (probed on dev1 with ffmpeg's aac at 160 kb/s).
  2. Phase: a least-squares fit of cos / sin / DC at the nominal pilot frequency on every 480-sample
     block; the residual phase psi = block phase - omega x start (omega from the nominal 48 kHz), so a
     perfect stream gives a constant psi and a rate offset a straight line. A block that touches a
     silence slot, or whose pilot amplitude is off by more than 2x, is not used.
  3. Steps: inside each run of usable blocks psi is unwrapped, detrended by a running median of its
     block-to-block increments (51 blocks, so the ASRC's slow rate changes never read as a step),
     and tested with a two-sided window mean (STEP_WINDOW blocks each side): AAC's per-frame phase
     wobble (+-0.2 samples, measured) averages out, a real step does not. A local maximum over
     `min_jump` samples is an event: a STEP when it is concentrated on one or two block boundaries,
     a SLEW (a resampler's correction spread over many blocks, reported, not a fault) otherwise.
  4. Bridges: across every gap between two runs (a silence slot, a damaged block, an excluded
     window) the phase is projected from both sides with the local slope; a mismatch over
     `min_jump` is a step. Each bridge reads the two runs' OWN phases (never the chained one), so
     a move is reported once, at its own gap. So a loss handled right reads 0 across its silence, a loss whose slot was
     DROPPED or padded reads its size. A CONCEALED loss (the issue-1439 receiver repeats the previous
     packet for a run of <= CONCEAL_MAX_SLOTS lost slots) has no silence, and the step detector
     cannot judge it (a repeat with the timeline intact reads +2..+6 samples inside it), so
     `analyze_scheduled` maps those losses onto the recording (r0 = the pilot onset - the frames the
     receiver held) and analyzes again with their windows EXCLUDED: the bridge across each window
     measures the timeline there (`conceal_windows` names the loss).
  5. Rate: one least-squares line through the continuous phase (every step and bridge removed)
     after `settle_s` -> the measured offset in ppm and its standard error.
  6. Cleanliness: the per-block fit residual relative to the pilot; another source mixed into the
     track reads as a high residual (AAC alone sits near -50 dB).

A step's size is in samples, positive = the content skipped AHEAD (samples dropped), negative =
samples repeated, and it is only known modulo one pilot period (48.14 samples at 997 Hz): the silence
slot's length tells the size of a loss.

THE VERDICT (`grade`, pure) against the emulator's schedule (scripts/vban_emulator_plan.py
`schedule_document`): FAIL on any step outside a scheduled loss; on a scheduled loss without a
silence slot of its length (+-LEN_TOL samples) at its place, or with a phase step across it -- a loss
of <= CONCEAL_MAX_SLOTS packets with no silence slot is CONCEALED when the bridge across its window
held (under `min_jump`), else "timeline moved"; on a
silence slot no loss explains; on a measured rate off the injected ppm by more than `ppm_bound`; on
a pilot missing for part of the run (coverage); on a dirty track; on a lead that was not silent before
the emulator started. Slews are reported. PASS otherwise.

Usage:
  vban_sine_continuity.py FILE [--schedule S.json] [--stream K] [--channel C] [--pilot-hz F]
                          [--ppm-bound P] [--expect-ppm X] [--lead-s S] [--settle-s S]
                          [--hold-frames N] [--json]
FILE is any audio/video file ffmpeg reads (the bench pulls an MKA of the test track); --stream picks
the K-th audio stream, --channel the channel inside it. Exit: 0 PASS, 1 FAIL, 2 the file cannot be
read or holds no pilot to analyse, 3 usage error.

Tier-0 tests: tests/python/test_vban_sine_continuity_1460.py.
"""
from __future__ import annotations

import argparse
import json
import math
import subprocess
import sys

import numpy as np

FS = 48000
CHUNK = 16
SILENCE_DB = -20.0
MIN_SILENCE_CHUNKS = 4
SUSTAIN_CHUNKS = 8
BLOCK = 480
STEP_WINDOW = 4
SLOPE_WINDOW = 25
MIN_JUMP_SAMPLES = 0.25
STEP_SHARPNESS = 0.4
GUARD_SAMPLES = 4 * BLOCK
# the codec's onset / end transient: blocks this close to either end of the active span are not used
EDGE_SAMPLES = 5 * BLOCK
AMP_LOW, AMP_HIGH = 0.5, 2.0
SETTLE_S = 60.0
MIN_RATE_BLOCKS = 100
# grading defaults
PPM_BOUND = 5.0
LEN_TOL = 64
# The receiver holds its first second (it plays from ~frame 187, issue 1439 ROZHODNUTÉ 6345255813),
# so the pilot onset sits up to ~1 s of sender time after the schedule's start: search 1.25 s.
ALIGN_SEARCH = 60000
RECEIVER_HOLD_S = 1.0   # the same first-second hold: never played, never a short pilot
ALIGN_TOL = 960
MIN_COVERAGE = 0.9
DIRTY_DB = -30.0
LEAD_SILENT_DB = -60.0
FRAMES_PER_PACKET = 256
# The issue-1439 receiver CONCEALS a run of at most this many lost slots between two received
# packets (the previous packet repeated, crossfaded) instead of playing silence. Pinned to
# `#define VBAN_RECV_CONCEAL_MAX_SLOTS` in vendor/obs-vban/src/vban-receive-timeline.h by a test.
CONCEAL_MAX_SLOTS = 3
# The receiver's first-second hold in sender frames (its `receive emitting: ... frame=N` line);
# the bench measured 187 on every run, the default when the line is absent.
DEFAULT_HOLD_FRAMES = 187
# Each concealable loss's window is widened by this much on both sides: its mapping onto the
# recording (the pilot onset at 16-sample resolution, the codec's smear of the crossfades) is not
# sample-exact.
CONCEAL_PAD = 2 * BLOCK


class AnalysisError(ValueError):
    """The input holds nothing this module can analyse."""


def read_channel(path, stream=0, channel=0, ffmpeg="ffmpeg"):
    """One channel of the K-th audio stream of PATH as float64 at 48 kHz (ffmpeg -> f32le).
    Raises AnalysisError when ffmpeg fails or returns nothing."""
    cmd = [ffmpeg, "-v", "error", "-nostdin", "-i", path, "-map", "0:a:%d" % stream,
           "-af", "pan=mono|c0=c%d" % channel, "-f", "f32le", "-ar", str(FS), "-"]
    try:
        out = subprocess.run(cmd, check=True, capture_output=True).stdout
    except (OSError, subprocess.CalledProcessError) as e:
        err = getattr(e, "stderr", b"") or b""
        raise AnalysisError("ffmpeg could not read %s: %s %s" % (path, e, err.decode(errors="replace")[-300:]))
    x = np.frombuffer(out, dtype="<f4").astype(np.float64)
    if x.size == 0:
        raise AnalysisError("%s holds no audio in stream %d" % (path, stream))
    return x


def _wrap(a):
    return (a + np.pi) % (2.0 * np.pi) - np.pi


def _chunk_rms(x):
    n = len(x) // CHUNK
    return np.sqrt(np.mean(x[: n * CHUNK].reshape(n, CHUNK) ** 2, axis=1))


def _runs(mask):
    """[(start, length)] of the True runs of a 1-D bool array."""
    if mask.size == 0:
        return []
    d = np.diff(np.concatenate(([0], mask.astype(np.int8), [0])))
    starts = np.flatnonzero(d == 1)
    ends = np.flatnonzero(d == -1)
    return [(int(s), int(e - s)) for s, e in zip(starts, ends)]


def _active_span(silent):
    """First chunk of the first, and end chunk of the last, run of >= SUSTAIN_CHUNKS loud chunks."""
    loud = [r for r in _runs(~silent) if r[1] >= SUSTAIN_CHUNKS]
    if not loud:
        return None
    return loud[0][0], loud[-1][0] + loud[-1][1]


def _block_fit(x, starts, omega, block):
    """Least squares cos / sin / DC per block -> (local phase, amplitude, residual rms)."""
    m = np.arange(block, dtype=np.float64)
    basis = np.stack([np.cos(omega * m), np.sin(omega * m), np.ones(block)], axis=1)
    pinv = np.linalg.pinv(basis)
    seg = np.stack([x[s:s + block] for s in starts]) if len(starts) else np.zeros((0, block))
    coef = seg @ pinv.T
    fit = coef @ basis.T
    resid = np.sqrt(np.mean((seg - fit) ** 2, axis=1)) if len(starts) else np.zeros(0)
    a, b = coef[:, 0], coef[:, 1]
    return np.arctan2(-b, a), np.hypot(a, b), resid


def _running_median(d, half):
    """Median of d over [i - half, i + half], truncated at the ends (NaN padding: an edge-value pad
    would hand the end value half the window and make it the median)."""
    if d.size == 0:
        return d
    padded = np.pad(d.astype(np.float64), half, mode="constant", constant_values=np.nan)
    win = np.lib.stride_tricks.sliding_window_view(padded, 2 * half + 1)
    return np.nanmedian(win, axis=1)


def _run_events(psi, d_slope, starts, omega, min_jump_rad, window):
    """Step/slew events inside ONE run of usable blocks. psi = unwrapped residual phase."""
    n = len(psi)
    if n < 2:
        return []
    d = np.diff(psi)
    e = psi - np.concatenate(([0.0], np.cumsum(d_slope)))
    c = np.concatenate(([0.0], np.cumsum(e)))
    stat = np.zeros(n - 1)
    for j in range(n - 1):                       # boundary between block j and j + 1
        w_left = min(window, j + 1)
        w_right = min(window, n - 1 - j)
        left = (c[j + 1] - c[j + 1 - w_left]) / w_left
        right = (c[j + 1 + w_right] - c[j + 1]) / w_right
        stat[j] = right - left
    events = []
    for j in np.flatnonzero(np.abs(stat) >= min_jump_rad):
        lo, hi = max(0, j - window), min(n - 1, j + window + 1)
        if np.abs(stat[j]) < np.max(np.abs(stat[lo:hi])) or (j > lo and np.abs(stat[lo:j]).max() == np.abs(stat[j])):
            continue
        k0, k1 = max(0, j - 1), min(n - 1, j + 2)
        sharp = float(np.max(np.abs(d[k0:k1] - d_slope[k0:k1])) / abs(stat[j]))
        events.append({"sample": int(starts[j + 1]), "size_samples": float(stat[j] / omega),
                       "kind": "step" if sharp >= STEP_SHARPNESS else "slew", "where": "run"})
    return events


def analyze(x, fs=FS, pilot_hz=997.0, block=BLOCK, min_jump=MIN_JUMP_SAMPLES, silence_db=SILENCE_DB,
            settle_s=SETTLE_S, lead_s=0.0, exclude=()):
    """The measurement dict of one recorded pilot track (module doc). Raises AnalysisError when
    there is no pilot to analyse. exclude: (lo, hi[, tag]) recording-sample windows whose blocks are
    not used (a concealed loss), so the bridge across each measures the timeline there; a bridge
    lists the tags (default: the window's index) of the windows it spans in `conceal_windows`."""
    if fs != FS:
        raise AnalysisError("only %d Hz is supported" % FS)
    x = np.asarray(x, dtype=np.float64)
    if x.size < 2 * fs:
        raise AnalysisError("only %.2f s of audio" % (x.size / fs))
    omega = 2.0 * np.pi * pilot_hz / fs
    rms = _chunk_rms(x)
    top = np.percentile(rms, 99)
    if top <= 1e-6:
        raise AnalysisError("the track is digital silence")
    ref = float(np.median(rms[rms > 0.1 * top]))
    silent = rms < ref * 10.0 ** (silence_db / 20.0)
    span = _active_span(silent)
    if span is None:
        raise AnalysisError("no sustained pilot in the track")
    c0, c1 = span
    active_start, active_end = c0 * CHUNK, c1 * CHUNK
    slots = [(active_start + s * CHUNK, n * CHUNK) for s, n in _runs(silent[c0:c1]) if n >= MIN_SILENCE_CHUNKS]
    # the blocks
    starts = np.arange(active_start, active_end - block + 1, block)
    local_phase, amp, resid = _block_fit(x, starts, omega, block)
    cycles = pilot_hz * starts / fs
    psi_raw = _wrap(local_phase - 2.0 * np.pi * (cycles - np.floor(cycles)))
    amp_med = float(np.median(amp)) if amp.size else 0.0
    usable = (amp >= AMP_LOW * amp_med) & (amp <= AMP_HIGH * amp_med)
    usable &= (starts >= active_start + EDGE_SAMPLES) & (starts + block <= active_end - EDGE_SAMPLES)
    for s, n in slots:
        usable &= ~((starts + block > s - GUARD_SAMPLES) & (starts < s + n + GUARD_SAMPLES))
    windows = [(int(w[0]), int(w[1]), w[2] if len(w) > 2 else k) for k, w in enumerate(exclude)]
    for lo, hi, _tag in windows:
        usable &= ~((starts + block > lo) & (starts < hi))
    good = np.flatnonzero(usable)
    if good.size < 2:
        raise AnalysisError("no usable pilot blocks (pilot frequency %.3f Hz?)" % pilot_hz)
    # runs of consecutive usable blocks
    breaks = np.flatnonzero(np.diff(good) > 1)
    run_idx = np.split(good, breaks + 1)
    min_jump_rad = omega * min_jump
    events, bridges, pieces = [], [], []
    prev = None
    for idx in run_idx:
        psi = np.unwrap(psi_raw[idx])
        d = np.diff(psi)
        d_slope = _running_median(d, SLOPE_WINDOW)
        evs = _run_events(psi, d_slope, starts[idx], omega, min_jump_rad, STEP_WINDOW)
        events.extend(evs)
        # remove the run's steps for the rate line
        corr = psi.copy()
        for ev in evs:
            corr[starts[idx] >= ev["sample"]] -= ev["size_samples"] * omega
        slope = float(np.median(d_slope)) if d_slope.size else 0.0
        # the run's own phase with its events removed, referred to its LAST level (each event added
        # back), so an event near either edge never leaks into the bridge's averaging window
        own_end = corr + sum(ev["size_samples"] for ev in evs) * omega
        if prev is not None:
            p_idx, p_corr, p_slope, p_end = prev
            gap_blocks_mid = (idx[0] + p_idx[-1]) / 2.0
            s_mid = (p_slope + slope) / 2.0 if p_idx.size > 1 and idx.size > 1 else (slope if idx.size > 1 else p_slope)
            w = min(STEP_WINDOW, p_idx.size)
            w2 = min(STEP_WINDOW, idx.size)

            def across(before, after):
                # the phase after the gap minus the phase before it, both projected to the gap's middle
                left = np.mean(before[-w:] + s_mid * (gap_blocks_mid - p_idx[-w:]))
                return np.mean(after[:w2] - s_mid * (idx[:w2] - gap_blocks_mid)) - left

            # issue 1460: the bridge reads the two runs' OWN phases. The chained phase already carries
            # every earlier step (a bridge, an in-run event), so measured on it each earlier move came
            # back at every later gap (live p4: the 81 s placement read -13.5 at all four losses).
            # The previous run is read at its last level, this one at its first level.
            dev = float(_wrap(across(p_end, corr)))
            gap_lo, gap_hi = int(starts[p_idx[-1]] + block), int(starts[idx[0]])
            inside = [s for s, n in slots if s >= gap_lo - GUARD_SAMPLES and s < gap_hi + GUARD_SAMPLES]
            concealed = [tag for lo, hi, tag in windows if lo < gap_hi and hi > gap_lo]
            bridges.append({"from_sample": gap_lo, "to_sample": gap_hi, "size_samples": dev / omega,
                            "silence_slots": inside, "conceal_windows": concealed})
            if abs(dev) >= min_jump_rad:
                events.append({"sample": gap_hi, "size_samples": dev / omega, "kind": "step",
                               "where": "bridge", "silence_slots": inside, "conceal_windows": concealed})
            # chain the run onto the previous one with the mismatch (and any 2 pi of the separate
            # unwrap) removed, so the rate line sees one continuous phase
            corr = corr - across(p_corr, corr)
        pieces.append((starts[idx], corr))
        prev = (idx, corr, slope, own_end)
    # the rate line over the continuous phase after the settle
    all_s = np.concatenate([p[0] for p in pieces]).astype(np.float64)
    all_psi = np.concatenate([p[1] for p in pieces])
    keep = all_s >= active_start + settle_s * fs
    rate_ppm = rate_se = None
    if np.count_nonzero(keep) >= MIN_RATE_BLOCKS:
        sx, sy = all_s[keep], all_psi[keep]
        sx0 = sx - sx.mean()
        beta = float(np.dot(sx0, sy - sy.mean()) / np.dot(sx0, sx0))
        res = sy - sy.mean() - beta * sx0
        se = float(np.sqrt(np.dot(res, res) / max(1, sx.size - 2) / np.dot(sx0, sx0)))
        rate_ppm, rate_se = beta / omega * 1e6, se / omega * 1e6
    resid_rel = resid[good] / np.maximum(amp[good] / np.sqrt(2.0), 1e-12)
    lead_db = None
    if lead_s > 0:
        # the first lead_s of the file: the caller starts the sender later than that, so a loud lead
        # is another source on the track (never the pilot)
        n_lead = int(lead_s * fs)
        lead_db = 20.0 * math.log10(max(float(np.sqrt(np.mean(x[:n_lead] ** 2))), 1e-12))
    events.sort(key=lambda ev: ev["sample"])
    for ev in events:
        ev["time_s"] = ev["sample"] / fs
    return {
        "sample_rate": fs, "pilot_hz": pilot_hz, "block": block, "min_jump_samples": min_jump,
        "samples": int(x.size),
        "active_start": active_start, "active_end": active_end,
        "active_start_s": active_start / fs, "active_s": (active_end - active_start) / fs,
        "pilot_dbfs": 20.0 * math.log10(ref * math.sqrt(2.0)),
        "blocks": int(starts.size), "usable_blocks": int(good.size),
        # excluded windows count as unused: a few concealed losses cost < 0.1 % of a profile
        "coverage": float(good.size * block / max(1, active_end - active_start)),
        "silence_slots": [{"sample": s, "length": n, "time_s": s / fs} for s, n in slots],
        "events": events,
        "steps": [ev for ev in events if ev["kind"] == "step"],
        "slews": [ev for ev in events if ev["kind"] == "slew"],
        "bridges": bridges,
        "excluded": [{"from_sample": lo, "to_sample": hi, "tag": tag} for lo, hi, tag in windows],
        "rate_ppm": rate_ppm, "rate_stderr_ppm": rate_se,
        "rate_blocks": int(np.count_nonzero(keep)),
        "residual_db_p99": float(20.0 * np.log10(max(np.percentile(resid_rel, 99), 1e-12))),
        "lead_dbfs": lead_db,
    }


def _r0_start(analysis, schedule, ratio, hold_frames):
    """The recording sample of sender sample 0: the pilot onset minus the frames the receiver held
    (never played). hold_frames None = the onset itself (the slot fit then has to find the hold)."""
    fpp = schedule.get("frames_per_packet", FRAMES_PER_PACKET)
    return float(analysis["active_start"]) - (hold_frames or 0) * fpp / ratio


def _match_losses(analysis, schedule, ratio, align_search, align_tol, r0=None):
    """Pair each scheduled loss with a silence slot -> ({loss_i: slot_i}, the fitted r0). r0 is
    where the fit starts (default: the pilot onset)."""
    slots = analysis["silence_slots"]
    losses = schedule.get("losses") or []
    r0 = float(analysis["active_start"]) if r0 is None else float(r0)
    offsets = []
    for lo in losses:
        exp = r0 + lo["sample"] / ratio
        near = [s["sample"] - exp for s in slots if abs(s["sample"] - exp) <= align_search]
        if near:
            offsets.append(min(near, key=abs))
    if offsets:
        r0 += float(np.median(offsets))
    pairs, used = {}, set()
    for i, lo in enumerate(losses):
        exp = r0 + lo["sample"] / ratio
        cands = [(abs(s["sample"] - exp), j) for j, s in enumerate(slots)
                 if j not in used and abs(s["sample"] - exp) <= align_tol]
        if cands:
            j = min(cands)[1]
            pairs[i] = j
            used.add(j)
    return pairs, r0


def conceal_windows(analysis, schedule, hold_frames=DEFAULT_HOLD_FRAMES, pad=CONCEAL_PAD,
                    align_search=ALIGN_SEARCH, align_tol=ALIGN_TOL, expect_ppm=None):
    """The recording-sample windows of every scheduled loss the receiver may conceal (count <=
    CONCEAL_MAX_SLOTS), as (lo, hi, loss index) for `analyze(exclude=...)`. The losses are mapped
    with r0 = the pilot onset - the receiver's held frames, refined by the silence slots the
    analysis found (the same fit `grade` uses)."""
    schedule = schedule or {}
    ratio = _ratio(analysis, schedule, expect_ppm)
    fpp = schedule.get("frames_per_packet", FRAMES_PER_PACKET)
    _pairs, r0 = _match_losses(analysis, schedule, ratio, align_search, align_tol,
                               _r0_start(analysis, schedule, ratio, hold_frames))
    out = []
    for i, lo in enumerate(schedule.get("losses") or []):
        if lo["count"] > CONCEAL_MAX_SLOTS:
            continue
        a = r0 + lo["sample"] / ratio
        out.append((int(math.floor(a - pad)), int(math.ceil(a + lo["count"] * fpp / ratio + pad)), i))
    return out


def analyze_scheduled(x, schedule=None, hold_frames=DEFAULT_HOLD_FRAMES, expect_ppm=None, **kw):
    """Two passes: analyze, map the concealable losses onto the recording, analyze again with
    those windows excluded so each is measured by the bridge across it. No such loss = one pass.
    The result carries `hold_frames`, which `grade` reads."""
    analysis = analyze(x, **kw)
    windows = conceal_windows(analysis, schedule, hold_frames, expect_ppm=expect_ppm) if schedule else []
    if windows:
        analysis = analyze(x, exclude=windows, **kw)
    analysis["hold_frames"] = hold_frames
    return analysis


def _ratio(analysis, schedule, expect_ppm=None):
    """1 + the rate offset the losses are mapped with: the measured one, else the injected one."""
    inj = schedule.get("ppm", expect_ppm)
    meas = analysis["rate_ppm"]
    return 1.0 + (meas if meas is not None else (inj or 0.0)) * 1e-6


def grade(analysis, schedule=None, ppm_bound=PPM_BOUND, expect_ppm=None, len_tol=LEN_TOL,
          align_search=ALIGN_SEARCH, align_tol=ALIGN_TOL, min_coverage=MIN_COVERAGE,
          dirty_db=DIRTY_DB, lead_silent_db=LEAD_SILENT_DB, hold_frames=None):
    """The PASS/FAIL verdict of one analysis (module doc). Returns {verdict, reasons, notes,
    losses: [...], ...}."""
    schedule = schedule or {}
    reasons, notes = [], []
    inj = schedule.get("ppm", expect_ppm)
    meas = analysis["rate_ppm"]
    ratio = _ratio(analysis, schedule, expect_ppm)
    fpp = schedule.get("frames_per_packet", FRAMES_PER_PACKET)
    if analysis["coverage"] < min_coverage:
        reasons.append("the pilot is usable in only %.1f %% of the active span (< %.0f %%)"
                       % (100 * analysis["coverage"], 100 * min_coverage))
    dur = schedule.get("duration_s")
    if dur:
        want = dur / ratio
        if analysis["active_s"] < want - 1.0 - RECEIVER_HOLD_S:
            reasons.append("the pilot plays %.1f s of the %.1f s the emulator sent" % (analysis["active_s"], want))
    if analysis["residual_db_p99"] > dirty_db:
        reasons.append("the track is not clean: fit residual p99 %.1f dB relative to the pilot (> %.0f dB) "
                       "-- another source feeds the test track" % (analysis["residual_db_p99"], dirty_db))
    lead = analysis.get("lead_dbfs")
    if lead is not None and lead > lead_silent_db:
        reasons.append("the track carried %.1f dBFS before the emulator started (> %.0f dBFS) -- another "
                       "source feeds the test track" % (lead, lead_silent_db))
    if hold_frames is None:
        hold_frames = analysis.get("hold_frames")
    pairs, r0 = _match_losses(analysis, schedule, ratio, align_search, align_tol,
                              _r0_start(analysis, schedule, ratio, hold_frames))
    slots = analysis["silence_slots"]
    loss_rows = []
    for i, lo in enumerate(schedule.get("losses") or []):
        row = {"packet": lo["packet"], "count": lo["count"], "time_s": lo.get("time_s")}
        want_len = lo["count"] * fpp / ratio
        j = pairs.get(i)
        across = [b for b in analysis["bridges"] if i in (b.get("conceal_windows") or ())]
        if j is None and lo["count"] <= CONCEAL_MAX_SLOTS and across:
            # concealed (the previous packet repeated): no silence, and the timeline must hold
            moved = [b for b in across if abs(b["size_samples"]) >= analysis["min_jump_samples"]]
            row["bridge_samples"] = (moved or across)[0]["size_samples"]
            if moved:
                row["result"] = "timeline moved"
                reasons.append("across the concealed loss of %d packet(s) at %.3f s the timeline moved by "
                               "%+.2f samples" % (lo["count"], lo.get("time_s") or 0.0, moved[0]["size_samples"]))
            else:
                row["result"] = "concealed"
        elif j is None:
            row["result"] = "no silence slot"
            reasons.append("the loss of %d packet(s) at %.3f s has no silence slot at its place (a dropped "
                           "or padded slot)" % (lo["count"], lo.get("time_s") or 0.0))
        else:
            s = slots[j]
            row.update({"slot_sample": s["sample"], "slot_length": s["length"]})
            if abs(s["length"] - want_len) > len_tol:
                row["result"] = "wrong length"
                reasons.append("the loss of %d packet(s) at %.3f s is %d samples of silence, not %.0f +- %d"
                               % (lo["count"], lo.get("time_s") or 0.0, s["length"], want_len, len_tol))
            else:
                bad = [b for b in analysis["bridges"] if s["sample"] in b["silence_slots"]
                       and abs(b["size_samples"]) >= analysis["min_jump_samples"]]
                if bad:
                    row["result"] = "timeline moved"
                    reasons.append("across the loss at %.3f s the timeline moved by %+.2f samples"
                                   % (lo.get("time_s") or 0.0, bad[0]["size_samples"]))
                else:
                    row["result"] = "ok"
        loss_rows.append(row)
    matched_slots = {slots[j]["sample"] for j in pairs.values()}
    for s in slots:
        if s["sample"] not in matched_slots:
            reasons.append("unscheduled silence at %.3f s (%d samples)" % (s["time_s"], s["length"]))
    for ev in analysis["steps"]:
        if set(ev.get("silence_slots") or ()) & matched_slots or ev.get("conceal_windows"):
            continue   # already a "timeline moved" reason of its loss
        reasons.append("phase step of %+.2f samples at %.3f s" % (ev["size_samples"], ev["time_s"]))
    for ev in analysis["slews"]:
        notes.append("slew of %+.2f samples at %.3f s" % (ev["size_samples"], ev["time_s"]))
    if inj is not None:
        if meas is None:
            reasons.append("the rate could not be measured (%d blocks after the settle)" % analysis["rate_blocks"])
        elif abs(meas - inj) > ppm_bound:
            reasons.append("measured rate %+.3f ppm vs injected %+.3f ppm (bound %.1f)" % (meas, inj, ppm_bound))
    return {"verdict": "FAIL" if reasons else "PASS", "reasons": reasons, "notes": notes,
            "losses": loss_rows, "rate_ppm": meas, "injected_ppm": inj, "recording_offset_samples": r0}


def _args(argv):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("file")
    ap.add_argument("--schedule")
    ap.add_argument("--stream", type=int, default=0)
    ap.add_argument("--channel", type=int, default=0)
    ap.add_argument("--pilot-hz", type=float, default=None, help="default: the schedule's, else 997")
    ap.add_argument("--ppm-bound", type=float, default=PPM_BOUND)
    ap.add_argument("--expect-ppm", type=float, default=None)
    ap.add_argument("--min-jump", type=float, default=MIN_JUMP_SAMPLES)
    ap.add_argument("--lead-s", type=float, default=0.0)
    ap.add_argument("--settle-s", type=float, default=SETTLE_S)
    ap.add_argument("--hold-frames", type=int, default=DEFAULT_HOLD_FRAMES,
                    help="the frames the receiver held before it played (its `receive emitting: ... "
                         "frame=N` line), default %d" % DEFAULT_HOLD_FRAMES)
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--ffmpeg", default="ffmpeg")
    return ap.parse_args(argv)


def main(argv=None):
    a = _args(argv)
    schedule = None
    try:
        if a.schedule:
            with open(a.schedule) as fh:
                schedule = json.load(fh)
        pilot = a.pilot_hz or (schedule or {}).get("pilot_hz") or 997.0
        if a.stream < 0 or a.channel < 0 or a.min_jump <= 0 or a.hold_frames < 0:
            raise ValueError("--stream / --channel / --hold-frames must be >= 0 and --min-jump > 0")
    except (OSError, ValueError) as e:
        print("vban-sine-continuity: %s" % e, file=sys.stderr)
        return 3
    try:
        x = read_channel(a.file, a.stream, a.channel, a.ffmpeg)
        analysis = analyze_scheduled(x, schedule, a.hold_frames, a.expect_ppm, pilot_hz=pilot,
                                     min_jump=a.min_jump, settle_s=a.settle_s, lead_s=a.lead_s)
    except AnalysisError as e:
        doc = {"verdict": "ERROR", "reasons": [str(e)]}
        print(json.dumps(doc) if a.json else "verdict=ERROR %s" % e)
        return 2
    verdict = grade(analysis, schedule, a.ppm_bound, a.expect_ppm)
    if a.json:
        print(json.dumps({"verdict": verdict["verdict"], "grade": verdict, "analysis": analysis,
                          "hold_frames": a.hold_frames}, sort_keys=True))
    else:
        print("verdict=%s rate_ppm=%s steps=%d slews=%d silence_slots=%d coverage=%.3f residual_p99_db=%.1f"
              % (verdict["verdict"], "n/a" if analysis["rate_ppm"] is None else "%+.3f" % analysis["rate_ppm"],
                 len(analysis["steps"]), len(analysis["slews"]), len(analysis["silence_slots"]),
                 analysis["coverage"], analysis["residual_db_p99"]))
        for row in verdict["losses"]:
            if row["result"] == "concealed":
                print("  note: the loss of %d packet(s) at %.3f s was concealed, the timeline held (%+.2f samples)"
                      % (row["count"], row.get("time_s") or 0.0, row["bridge_samples"]))
        for r in verdict["reasons"]:
            print("  FAIL: " + r)
        for n in verdict["notes"]:
            print("  note: " + n)
    return 0 if verdict["verdict"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
