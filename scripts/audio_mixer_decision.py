#!/usr/bin/env python3
"""issue 1381 -- PURE decision core of the dev1 audio-mixer / VBAN-loss alert watchdog.

WHY: on 27.9.2026 the resolume cg OBS audio mixer left real time from 06:00 local (ticks_over 22,
36, then 215+ per minute; 2760 / 2868 ticks per minute by 06:09, 1753-2201 and 3857 later) and both
obs-vban outputs to FOH lost audio (2291 underflows / 703 overflows per output by 07:17). Nothing
paged; FOH heard it at 06:56. Both signals were in the OBS log. bundle_state_gather exposes them on
each box's :8899 (`audio_mixer_*`, `vban_pacer_*`); this module grades them; the orchestrator
scripts/audio-mixer-alert-watchdog.sh drives obs-watchdog-decision.sh's 2-pass confirm + throttle
and airuleset notify (time-bucketed --dedup-key, the production-critical class of issue 1308).

MIXER arm (classify_mixer) -- one complete `audio-stall #1367` dump window. The dump is written by
the first audio callback past 60 s on the audio thread's own disciplined clock, so its tick count
IS the per-minute rate. It is graded as dumped and never rescaled by the interval between two log
lines: those timestamps are the wall clock, which dantesync steps (the real 27.9 log reads 60.193 s
around the 02:00 UTC nightly date step with a real-time ticks=2813).
  SKIP        box not fetched (:8899 / box down) -- deferred to the bundle-state / reach watchdogs.
  STALLED     (issue 1385) the newest dump is more than STALE_AFTER_S behind the log head AND the
              log head itself is at most LOG_LIVE_S old on the box's own clock (the
              `obs_log_head_age_s` facet): OBS is still logging NOW while its audio thread stopped
              dumping -- silence on air on a box without VBAN outputs. PAGES. Decided first, from
              the age alone: a dump that left the tail of a long session reports only its age.
  STALE       the same old dump WITHOUT that proof (OBS down or hung, the log frozen, an older
              gather without the facet): log-only -- obs-liveness / bundle-state own a dead OBS.
              Also a FROZEN log: the log head more than LOG_FROZEN_S (180 s, fixed) old, so its
              last counts are never graded (a dead OBS whose last dump was BEHIND would otherwise
              page every hour). The watchdog resets the mixer confirm on STALE.
  UNKNOWN     facet absent (a normal OBS start: one partial dump only) or a tick length that is no
              known sample rate.
  BEHIND      ticks in the minute more than TOLERANCE off real time (2812.5 at 48 kHz). A SURPLUS
              pages too: that is the mixer catching up in bursts.
  OVERLOADED  more than OVER_MAX ticks in the minute came late (gap > 1.5 ticks).
  HEALTHY     otherwise.
LOG CLOCK (classify_log_clock, issue 1385) -- every mixer verdict above rests on the log head age,
which assumes OBS's log stamps and the gather's clock share one time zone. A log whose head age
barely changes between two passes is ADVANCING; if its head still reads older than the frozen
bound, the two clocks disagree and the whole mixer arm reads STALE, blind. MISMATCH pages once
(the orchestrator's stable-key CLOCK page); OK = consistent; UNKNOWN = not judgeable this pass.
VBAN arm (classify_vban) -- the per-destination loss-counter increase inside the gather window:
  SKIP / UNKNOWN (no VBAN output on this box) as above; VBAN_LOSS when any loss counter moved
  (events > 0 or loss ms > 0, decided before staleness: loss in the window is real even if the
  output then stopped); STALE when the newest status line is old (log-only); HEALTHY otherwise.

No I/O -- pytest Tier-0 (the render_freeze_decision.py / issue 1199 python-mirror precedent).
"""
import argparse
import json
import sys

AUDIO_OUTPUT_FRAMES = 1024          # libobs mixes 1024 frames per tick
KNOWN_SAMPLE_RATES = (48000, 44100)  # the rates OBS offers
DEFAULT_TOLERANCE = 5.0             # ticks/min off real time (the design threshold)
DEFAULT_OVER_MAX = 30.0             # late ticks/min
DEFAULT_STALE_AFTER_S = 180         # 3x the 60 s dump period (the audio-lag sibling's bound)
DEFAULT_VBAN_STALE_AFTER_S = 180    # 18 missed 10 s status lines
# issue 1385 -- the log head counts as live while it is at most this old on the box's own clock. A
# genlock OBS logs every ~5 s (program-render-audit). A date-less log dead for whole days reads live
# for one pass a day at most; the watchdog resets the mixer confirm on the STALE passes between, so
# two such days never pair into a page.
DEFAULT_LOG_LIVE_S = 60
# issue 1385 -- a log head older than this is FROZEN (OBS down / hung): its last counts are not
# graded. FIXED, not the stale override: slack (10 s) + this must stay below the 300 s dev1 pass, so
# a date-less log dead for days grades its last counts on one pass a day at most.
DEFAULT_LOG_FROZEN_S = 180
# The log-clock check needs two passes a sane distance apart (a timer gap resets the judgement). The
# upper bound is below one quarter hour on purpose: each :8899 fetch makes OBS log a WebSocket
# connect AFTER the log read, so a hung OBS whose WebSocket thread still runs reads at most one gap
# old -- never within a minute of 900 s (review round 3).
LOG_CLOCK_MIN_GAP_S = 60
LOG_CLOCK_MAX_GAP_S = 600
# Time-zone offsets are whole quarter hours, and 86400 is one too, so a LIVE log stamped in another
# zone reads (offset + a fresh few seconds) mod 900 on both sides of the date wrap.
LOG_CLOCK_ZONE_STEP_S = 900


def expected_ticks_per_min(tick_ms):
    """The real-time tick rate for the dump's `tick_ms` (rounded to 0.1 ms by the log line), or None
    for a tick length that matches no known sample rate (never guess a rate)."""
    try:
        tick = round(float(tick_ms), 1)
    except (TypeError, ValueError):
        return None
    for rate in KNOWN_SAMPLE_RATES:
        if round(AUDIO_OUTPUT_FRAMES * 1000.0 / rate, 1) == tick:
            return 60.0 * rate / AUDIO_OUTPUT_FRAMES
    return None


def classify_mixer(ticks, ticks_over, window_ms, tick_ms, age_s, box_reachable,
                   tolerance=DEFAULT_TOLERANCE, over_max=DEFAULT_OVER_MAX,
                   stale_after_s=DEFAULT_STALE_AFTER_S, log_head_age_s=None,
                   log_live_s=DEFAULT_LOG_LIVE_S):
    """One box's MIXER verdict + the per-minute rates it graded (None when not graded).
    `window_ms` (the log interval since the previous dump) is context only -- see the module doc for
    why the count is never rescaled by it. `log_head_age_s` (issue 1385) is the log head's age on the
    box's own clock; None = no proof the log is live."""
    del window_ms   # context for the caller's log line, never graded
    res = {"verdict": "SKIP", "rate_per_min": None, "expected_per_min": None,
           "deviation_per_min": None, "over_per_min": None}
    if box_reachable != 1:
        return res
    if age_s is not None and age_s > stale_after_s:
        # A stale dump's counts describe a minute long gone; the stop is the current fault.
        live = log_head_age_s is not None and log_head_age_s <= log_live_s
        res["verdict"] = "STALLED" if live else "STALE"
        return res
    expected = expected_ticks_per_min(tick_ms)
    if ticks is None or ticks_over is None or expected is None:
        res["verdict"] = "UNKNOWN"
        return res
    if log_head_age_s is not None and log_head_age_s > DEFAULT_LOG_FROZEN_S:
        # The log itself stopped (OBS down / hung): its last counts are frozen, not current. The
        # bound is wider than the live one, so a quiet but live log still grades.
        res["verdict"] = "STALE"
        return res
    res.update(rate_per_min=ticks, expected_per_min=expected, deviation_per_min=ticks - expected,
               over_per_min=ticks_over)
    if abs(ticks - expected) > tolerance:
        res["verdict"] = "BEHIND"
    elif ticks_over > over_max:
        res["verdict"] = "OVERLOADED"
    else:
        res["verdict"] = "HEALTHY"
    return res


def classify_log_clock(head_age_s, prev_head_age_s, gap_s, frozen_s=DEFAULT_LOG_FROZEN_S,
                       min_gap_s=LOG_CLOCK_MIN_GAP_S, max_gap_s=LOG_CLOCK_MAX_GAP_S):
    """issue 1385 -- do the OBS log clock and the gather clock agree? `head_age_s` now and
    `prev_head_age_s` on the previous pass `gap_s` seconds earlier (the orchestrator's state).
    A frozen log's head ages WITH the wall clock (by ~gap_s); a live log whose clocks agree reads
    young. A head that reads older than `frozen_s` on both passes yet aged less than half the gap is
    a log that advances with its stamps off the gather's clock -> MISMATCH. The time-zone offset
    cancels in the difference, so this holds whatever the offset. A frozen log's daily date wrap
    (a huge negative change) is OK, never a mismatch. Both heads must also sit within
    `DEFAULT_LOG_LIVE_S` of a whole quarter hour (a zone offset + a fresh line): a hung OBS whose
    only log writer is the pager's own WebSocket connect reads ~one gap old on every pass, which is
    silence on air, not a clock fault (review round 3)."""
    if head_age_s is None or prev_head_age_s is None or gap_s is None:
        return "UNKNOWN"
    if not min_gap_s <= gap_s <= max_gap_s:
        return "UNKNOWN"
    if head_age_s <= frozen_s or prev_head_age_s <= frozen_s:
        return "OK"
    if not (_near_zone_step(head_age_s) and _near_zone_step(prev_head_age_s)):
        return "OK"
    if abs(2 * (head_age_s - prev_head_age_s)) < gap_s:
        return "MISMATCH"
    return "OK"


def _near_zone_step(age_s, near_s=DEFAULT_LOG_LIVE_S, step_s=LOG_CLOCK_ZONE_STEP_S):
    """True when `age_s` is within `near_s` of a whole multiple of the zone step (either side)."""
    r = age_s % step_s
    return min(r, step_s - r) <= near_s


def classify_vban(events, loss_ms, age_s, box_reachable,
                  stale_after_s=DEFAULT_VBAN_STALE_AFTER_S):
    """One box's VBAN verdict (see the module doc)."""
    if box_reachable != 1:
        return "SKIP"
    if events is None:
        return "UNKNOWN"
    if events > 0 or (loss_ms is not None and loss_ms > 0):
        return "VBAN_LOSS"
    if age_s is not None and age_s > stale_after_s:
        return "STALE"
    return "HEALTHY"


def _loads_obj(text):
    if not text:
        return None
    try:
        obj = json.loads(text)
    except (ValueError, TypeError):
        return None
    return obj if isinstance(obj, dict) else None


def _num(obj, key, cast):
    """A facet value -> cast(value), or None for absent/empty/unparseable (UNKNOWN, never a fake 0)."""
    raw = obj.get(key) if isinstance(obj, dict) else None
    if raw is None or (isinstance(raw, str) and raw.strip() == ""):
        return None
    try:
        return cast(str(raw).strip())
    except (TypeError, ValueError):
        return None


def analyze(bundle_json_text, box_reachable, tolerance=DEFAULT_TOLERANCE,
            over_max=DEFAULT_OVER_MAX, stale_after_s=DEFAULT_STALE_AFTER_S,
            vban_stale_after_s=DEFAULT_VBAN_STALE_AFTER_S, log_live_s=DEFAULT_LOG_LIVE_S,
            prev_log_head_age_s=None, pass_gap_s=None):
    """Fetch result -> both arms' verdicts + the readings. Unreachable -> SKIP without parsing."""
    obj = _loads_obj(bundle_json_text) if box_reachable == 1 else None
    ticks = _num(obj, "audio_mixer_ticks", int)
    over = _num(obj, "audio_mixer_ticks_over", int)
    window_ms = _num(obj, "audio_mixer_window_ms", int)
    tick_ms = obj.get("audio_mixer_tick_ms") if isinstance(obj, dict) else None
    age_s = _num(obj, "audio_mixer_age_s", int)
    head_age = _num(obj, "obs_log_head_age_s", int)
    mixer = classify_mixer(ticks, over, window_ms, tick_ms, age_s, box_reachable, tolerance,
                           over_max, stale_after_s, log_head_age_s=head_age,
                           log_live_s=log_live_s)
    events = _num(obj, "vban_pacer_loss_events", int)
    loss_ms = _num(obj, "vban_pacer_loss_ms", float)
    vban_age = _num(obj, "vban_pacer_age_s", int)
    return {
        "mixer_verdict": mixer["verdict"], "ticks": ticks, "ticks_over": over,
        "window_ms": window_ms, "tick_ms": tick_ms, "age_s": age_s, "log_head_age_s": head_age,
        "log_clock": (classify_log_clock(head_age, prev_log_head_age_s, pass_gap_s)
                      if box_reachable == 1 else "UNKNOWN"),
        "rate_per_min": mixer["rate_per_min"], "expected_per_min": mixer["expected_per_min"],
        "deviation_per_min": mixer["deviation_per_min"], "over_per_min": mixer["over_per_min"],
        "vban_verdict": classify_vban(events, loss_ms, vban_age, box_reachable,
                                      vban_stale_after_s),
        "vban_events": events, "vban_loss_ms": loss_ms,
        "vban_dest": obj.get("vban_pacer_loss_dest") if isinstance(obj, dict) else None,
        "vban_age_s": vban_age,
    }


_FORMATS = {"rate_per_min": "{:.1f}", "expected_per_min": "{:.1f}",
            "deviation_per_min": "{:+.1f}", "over_per_min": "{:.1f}", "vban_loss_ms": "{:.1f}"}


def _fmt(key, v):
    if v is None:
        return ""
    return _FORMATS.get(key, "{}").format(v)


def _main(argv):
    ap = argparse.ArgumentParser(description="pure audio-mixer / VBAN-loss decisions (issue 1381)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("analyze", help="read /bundle-state.json on stdin -> both arms' verdicts")
    a.add_argument("--box-reachable", type=int, required=True)
    a.add_argument("--tolerance", type=float, default=DEFAULT_TOLERANCE)
    a.add_argument("--over-max", type=float, default=DEFAULT_OVER_MAX)
    a.add_argument("--stale-after-s", type=int, default=DEFAULT_STALE_AFTER_S)
    a.add_argument("--vban-stale-after-s", type=int, default=DEFAULT_VBAN_STALE_AFTER_S)
    a.add_argument("--log-live-s", type=int, default=DEFAULT_LOG_LIVE_S)
    a.add_argument("--prev-log-head-age-s", type=int, default=None)
    a.add_argument("--pass-gap-s", type=int, default=None)
    ns = ap.parse_args(argv)
    # Tolerant read (the ndi_halving precedent): a strict read that raised would be swallowed by the
    # caller's 2>/dev/null and read as SKIP forever. box_reachable=0 needs no stdin.
    text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
    res = analyze(text, ns.box_reachable, ns.tolerance, ns.over_max, ns.stale_after_s,
                  ns.vban_stale_after_s, ns.log_live_s, ns.prev_log_head_age_s, ns.pass_gap_s)
    for k in ("mixer_verdict", "ticks", "ticks_over", "window_ms", "tick_ms", "age_s",
              "log_head_age_s", "log_clock",
              "rate_per_min", "expected_per_min", "deviation_per_min", "over_per_min",
              "vban_verdict", "vban_events", "vban_loss_ms", "vban_dest", "vban_age_s"):
        print(f"{k}={_fmt(k, res[k])}")
    return 0


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
