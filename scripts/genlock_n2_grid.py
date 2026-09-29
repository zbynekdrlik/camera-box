#!/usr/bin/env python3
"""Issue 1367 slice D1b -- the dev1 Python twin of the grid-exact N>=2 present age.

WHAT: since D1 (src/genlock_n2_grid.rs, the C `genlock_release_tick_n2_grid` in obs-source.c) an
N>=2 genlock input (a 60 fps camera into the 30 fps strih canvas) presents, at render tick T, the
stamp

    S*(T) = grid_floor(T - GENLOCK_N2_AGE_BASE_NS - pin, canvas_interval / N)

on the per-second SOURCE grid. Its on-air present age is therefore a pure function of the pin:
`ceil((50 ms + pin) / source interval)` source frames -- 66.7 ms at pins 1..16 ms (the production
pin 3), one more source frame per source interval of pin. The dev1 consumers that estimated the
present age as `latency_ms + mean_head_skew_ms` from the `genlock-fifo audit` line
(scripts/qr_align_pins.py, scripts/prerecord_phase_calibrate.py, scripts/arrival_floor_decompose.py,
fed by scripts/lib/qr-align.sh) read this twin instead for a CONFIRMED grid input. On the grid the
audit head is one source frame older than the target, so the old estimate read about 86 ms at pin 3;
`mean_head_skew_ms` stays only as the arrival-lag diagnostic.

THE CONSTANT is never retyped here: `load_age_base_ns()` reads `GENLOCK_N2_AGE_BASE_NS` from
src/genlock_n2_grid.rs at run time (the scripts/av_soak_decision.py pattern) and fails closed
(`N2GridConstantError`) when it cannot. The per-second grid helpers mirror src/genlock_grid.rs.
tests/fixtures/genlock_n2_present_age_1367.tsv is the ONE present-age table read by BOTH the Rust
test in src/genlock_n2_grid.rs and tests/python/test_genlock_n2_grid_twin_1367.py.

CONFIRMED grid input (`grid_inputs_from_audit`): every `genlock-fifo audit` line of the input carries
the `n2_early=` token (only a D1 build prints it) AND its rate multiple is within 0.25 of an integer
N >= 2 -- the design's `received` ~= N x `consumed` on the audit line, read as received frames per
render tick (consumed + holds + late holds + underruns) over the window (the last line's cumulative
ratio when the window holds one line with at least a second of ticks). A pre-D1
log, an N==1 input, or no log at all is never a grid input, so those consumers keep their arithmetic
byte-for-byte.

Pure: no I/O except the one read of the Rust source (cached per repo root). Tier-0 via pytest.
"""
from __future__ import annotations

import math
import os
import re
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

# The ONE audit-line tokenizer (the key=value scan genlock_audit_snapshot already uses).
from genlock_audit_snapshot import parse_audit_line  # noqa: E402

NS_PER_SECOND = 1_000_000_000
# (file under the repo root, constant name) -- where the age base lives. Never retyped.
AGE_BASE_SOURCE = ("src/genlock_n2_grid.rs", "GENLOCK_N2_AGE_BASE_NS")

_FPS_RE = re.compile(r"@\s*([0-9]+(?:\.[0-9]+)?)\s*fps")
# A rate ratio further than this from its nearest integer is inconclusive, never a guessed multiple
# (received frames per render tick: a clean 60-into-30 window reads 2.0).
RATE_RATIO_TOLERANCE = 0.25
# The cumulative ratio of ONE line is trusted only past a second of ticks (a just-restarted source
# reads received=3 consumed=2).
MIN_CUMULATIVE_TICKS = 30
# Every render tick of a genlock source is exactly one of these (obs-source.c: a present counts
# consumed=, a HOLD holds= or late_holds=, an empty queue underruns= in get_closest_frame).
_TICK_COUNTERS = ("consumed", "holds", "late_holds", "underruns")


def _ticks(counters):
    return sum(counters.get(k, 0) for k in _TICK_COUNTERS)


_AGE_BASE_CACHE: dict = {}


class N2GridConstantError(Exception):
    """GENLOCK_N2_AGE_BASE_NS could not be read from its Rust source (fail closed)."""


# --- the constant -------------------------------------------------------------------------------


def parse_rust_u64_const(text, name):
    """`pub const NAME: u64 = <digits>;` -> int, or None when absent. Exact-name match; `_`
    digit separators allowed."""
    m = re.search(r"\bpub\s+const\s+" + re.escape(name) + r"\s*:\s*u64\s*=\s*([0-9_]+)\s*;",
                  text or "")
    if not m:
        return None
    try:
        return int(m.group(1).replace("_", ""))
    except ValueError:
        return None


def default_repo_root():
    return os.path.normpath(os.path.join(_HERE, ".."))


def load_age_base_ns(repo_root=None):
    """GENLOCK_N2_AGE_BASE_NS read from src/genlock_n2_grid.rs. Raises N2GridConstantError when the
    file is unreadable or the constant is missing / not positive. Cached per repo root."""
    root = os.path.normpath(repo_root or default_repo_root())
    if root in _AGE_BASE_CACHE:
        return _AGE_BASE_CACHE[root]
    rel, name = AGE_BASE_SOURCE
    path = os.path.join(root, rel)
    try:
        with open(path, encoding="utf-8") as f:
            text = f.read()
    except OSError as e:
        raise N2GridConstantError(f"cannot read {path}: {e}") from e
    val = parse_rust_u64_const(text, name)
    if val is None or val <= 0:
        raise N2GridConstantError(f"{name} not found as a positive `pub const {name}: u64` in {path}")
    _AGE_BASE_CACHE[root] = val
    return val


def _age_base(age_base_ns):
    return load_age_base_ns() if age_base_ns is None else int(age_base_ns)


# --- the per-second grid (mirror of src/genlock_grid.rs) ----------------------------------------


def integer_fps(interval_ns):
    """The integer frame rate `interval_ns` belongs to, or None (mirror of genlock_grid::integer_fps)."""
    if not interval_ns:
        return None
    fps = (NS_PER_SECOND + interval_ns // 2) // interval_ns
    if fps == 0:
        return None
    return fps if abs(fps * interval_ns - NS_PER_SECOND) < fps else None


def _slot_in_second(offset, fps, units):
    slot = offset * fps // units
    if (slot + 1) * units // fps <= offset:
        slot += 1
    return slot


def per_second_floor(t, fps, units=NS_PER_SECOND):
    """The per-second grid boundary at or before `t` (mirror of genlock_grid::per_second_floor)."""
    if not fps or not units:
        return t
    sec = (t // units) * units
    return sec + _slot_in_second(t - sec, fps, units) * units // fps


def grid_floor_ns(t_ns, interval_ns):
    """The receiver grid point at or before `t_ns` (mirror of genlock_grid::grid_floor_ns)."""
    if not interval_ns:
        return t_ns
    fps = integer_fps(interval_ns)
    if fps:
        return per_second_floor(t_ns, fps, NS_PER_SECOND)
    return (t_ns // interval_ns) * interval_ns


# --- the D1 age function (mirror of src/genlock_n2_grid.rs) --------------------------------------


def source_interval_ns(canvas_interval_ns, n):
    """canvas_interval / n (mirror of n2_source_interval_ns); n == 0 -> 0."""
    if not n:
        return 0
    return canvas_interval_ns // n


def target_stamp_ns(tick_ns, pin_ns, canvas_interval_ns, n, age_base_ns=None):
    """The stamp an N>=2 source presents at tick `tick_ns` (mirror of n2_target_stamp_ns)."""
    age = _age_base(age_base_ns) + int(pin_ns)
    return grid_floor_ns(max(0, tick_ns - age), source_interval_ns(canvas_interval_ns, n))


def present_age_frames(pin_ms, source_interval_ns_, age_base_ns=None):
    """The whole source frames an N>=2 input at `pin_ms` presents behind an on-grid tick:
    ceil((GENLOCK_N2_AGE_BASE_NS + pin) / source interval), on the per-second grid for an integer
    source rate."""
    return _present_frames_ns(int(round(float(pin_ms) * 1e6)), source_interval_ns_, age_base_ns)


def _present_frames_ns(pin_ns, si, age_base_ns):
    age = _age_base(age_base_ns) + int(pin_ns)
    if not si:
        return None
    fps = integer_fps(si)
    if fps:
        return -(-(age * fps) // NS_PER_SECOND)
    return -(-age // si)


def present_age_ns_for_pin_ns(pin_ns, source_interval_ns_, age_base_ns=None):
    """The present age (ns) at an on-grid tick for a pin given in ns. The per-second grid alternates
    its slot lengths, so the Rust per-tick age is this value or one ns more; this returns the floor
    `frames x 1 s / fps` (exact for the 1970-grid fallback of a fractional rate). A zero interval
    (no video info) -> the unrounded age."""
    si = int(source_interval_ns_)
    frames = _present_frames_ns(pin_ns, si, age_base_ns)
    if frames is None:
        return _age_base(age_base_ns) + int(pin_ns)
    fps = integer_fps(si)
    if fps:
        return frames * NS_PER_SECOND // fps
    return frames * si


def present_age_ns(pin_ms, source_interval_ns_, age_base_ns=None):
    """The present age (ns) of an N>=2 grid input at `pin_ms` (see present_age_ns_for_pin_ns)."""
    return present_age_ns_for_pin_ns(int(round(float(pin_ms) * 1e6)), source_interval_ns_, age_base_ns)


def present_age_ms(pin_ms, source_interval_ns_, age_base_ns=None):
    """present_age_ns in ms (66.667 at pin 3 on a 60-into-30 input)."""
    return present_age_ns(pin_ms, source_interval_ns_, age_base_ns) / 1e6


def frames_for_hold(hold_ms, source_interval_ns_):
    """A present-age hold rounded to the nearest whole source frame (half up). The grid moves a
    source only in whole frames, so a measured hold is quantized before it becomes a pin."""
    si_ms = int(source_interval_ns_) / 1e6
    if si_ms <= 0 or hold_ms is None or hold_ms <= 0:
        return 0
    return int(math.floor(hold_ms / si_ms + 0.5))


def pin_for_frames(current_pin_ms, frames, source_interval_ns_, age_base_ns=None):
    """The integer ms pin that presents exactly `frames` more source frames than `current_pin_ms`:
    round(current + frames x source interval) -- a relative step of one source interval is one
    frame -- checked through the twin and nudged by 1 ms toward the band if the rounding crossed an
    edge. `frames <= 0` returns the current pin, rounded."""
    cur = int(round(float(current_pin_ms)))
    if not frames or frames <= 0:
        return cur
    want = present_age_frames(current_pin_ms, source_interval_ns_, age_base_ns) + int(frames)
    si_ms = int(source_interval_ns_) / 1e6
    pin = int(math.floor(float(current_pin_ms) + frames * si_ms + 0.5))
    for _ in range(int(math.ceil(si_ms)) + 2):
        got = present_age_frames(pin, source_interval_ns_, age_base_ns)
        if got == want:
            return pin
        pin += 1 if got < want else -1
    raise ValueError(f"no integer pin presents {want} frames near {current_pin_ms} ms "
                     f"(source interval {source_interval_ns_} ns)")


# --- which inputs run the grid (from the raw audit lines) ---------------------------------------


def canvas_interval_from_fps(fps):
    """The canvas frame interval (ns) the audit line's `@ F fps` names: 1e9 // F for an integer rate
    (the C `1e9 * fps_den / fps_num` with den 1), else the nearest ns of 1e9 / F (a fractional rate
    prints 3 decimals only). None for a missing / non-positive rate."""
    if fps is None or fps <= 0:
        return None
    r = round(fps)
    if r > 0 and abs(fps - r) < 0.0005:
        return NS_PER_SECOND // int(r)
    return int(round(NS_PER_SECOND / fps))


def classify_audit_inputs(log_text):
    """{source: {"lines", "n2_marker", "n", "canvas_interval_ns", "source_interval_ns", "grid",
    "n2_early_rate"}} from the `genlock-fifo audit` lines in `log_text`, first-seen order.

    - `n2_marker`: every line of the input carries `n2_early=`.
    - `n`: the nearest integer to received frames per render tick -- Δreceived / Δticks over the
      window's first..last line (ticks = consumed + holds + late holds + underruns, every tick counts
      once, so a camera with held or empty ticks still reads its true multiple), or the last line's
      cumulative received / ticks when the window has one line or no Δticks (and that line has at
      least MIN_CUMULATIVE_TICKS ticks). None when neither is measurable, or when the ratio is more
      than RATE_RATIO_TOLERANCE from an integer.
    - `grid`: n2_marker and n >= 2 and a known canvas rate.
    - `n2_early_rate`: Δn2_early / Δticks over the window (the design's 0.1 % budget base), None with
      one line or no ticks. An early tick presents one frame OLDER than the twin's age, so this is
      the share of ticks the twin over-states."""
    per = {}
    for line in (log_text or "").splitlines():
        parsed = parse_audit_line(line)
        if not parsed:
            continue
        name, counters = parsed
        if not counters:
            continue
        m = _FPS_RE.search(line)
        fps = float(m.group(1)) if m else None
        per.setdefault(name, []).append((counters, fps))
    out = {}
    for name, rows in per.items():
        first, last = rows[0][0], rows[-1][0]
        marker = all("n2_early" in c for c, _fps in rows)
        ratio = None
        d_rec = last.get("received", 0) - first.get("received", 0)
        d_ticks = _ticks(last) - _ticks(first)
        if len(rows) >= 2 and d_ticks > 0 and d_rec >= 0:
            ratio = d_rec / d_ticks
        elif _ticks(last) >= MIN_CUMULATIVE_TICKS:
            ratio = last.get("received", 0) / _ticks(last)
        n = None
        if ratio is not None:
            nearest = max(1, int(math.floor(ratio + 0.5)))
            if abs(ratio - nearest) <= RATE_RATIO_TOLERANCE:
                n = nearest
        canvas = canvas_interval_from_fps(rows[-1][1])
        grid = bool(marker and n is not None and n >= 2 and canvas)
        early_rate = None
        if len(rows) >= 2 and marker and d_ticks > 0:
            early_rate = max(0, last.get("n2_early", 0) - first.get("n2_early", 0)) / d_ticks
        out[name] = {
            "lines": len(rows),
            "n2_marker": marker,
            "n": n,
            "canvas_interval_ns": canvas,
            "source_interval_ns": source_interval_ns(canvas, n) if (canvas and n) else None,
            "grid": grid,
            "n2_early_rate": early_rate,
        }
    return out


def grid_inputs_from_audit(log_text):
    """{source: source_interval_ns} for every CONFIRMED D1 grid input in the audit lines (see
    classify_audit_inputs). {} for a pre-D1 log, an N==1-only log or no log."""
    return {name: c["source_interval_ns"] for name, c in classify_audit_inputs(log_text).items()
            if c["grid"]}


def read_grid_inputs(path):
    """grid_inputs_from_audit over a log FILE (bytes the OBS log mis-encodes are replaced, never
    fatal). Raises OSError when the file cannot be read -- the caller decides whether that degrades."""
    with open(path, encoding="utf-8", errors="replace") as f:
        return grid_inputs_from_audit(f.read())
