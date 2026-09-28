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
              Also a FROZEN log: the log head more than STALE_AFTER_S old on the box clock, so its
              last counts are never graded (a dead OBS whose last dump was BEHIND would otherwise
              page every hour). The watchdog resets the mixer confirm on STALE.
  UNKNOWN     facet absent (a normal OBS start: one partial dump only) or a tick length that is no
              known sample rate.
  BEHIND      ticks in the minute more than TOLERANCE off real time (2812.5 at 48 kHz). A SURPLUS
              pages too: that is the mixer catching up in bursts.
  OVERLOADED  more than OVER_MAX ticks in the minute came late (gap > 1.5 ticks).
  HEALTHY     otherwise.
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
    if log_head_age_s is not None and log_head_age_s > stale_after_s:
        # The log itself stopped (OBS down / hung): its last counts are frozen, not current. The
        # bound is the stale window, not the live one, so a quiet but live log still grades.
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
            vban_stale_after_s=DEFAULT_VBAN_STALE_AFTER_S, log_live_s=DEFAULT_LOG_LIVE_S):
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
    ns = ap.parse_args(argv)
    # Tolerant read (the ndi_halving precedent): a strict read that raised would be swallowed by the
    # caller's 2>/dev/null and read as SKIP forever. box_reachable=0 needs no stdin.
    text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
    res = analyze(text, ns.box_reachable, ns.tolerance, ns.over_max, ns.stale_after_s,
                  ns.vban_stale_after_s, ns.log_live_s)
    for k in ("mixer_verdict", "ticks", "ticks_over", "window_ms", "tick_ms", "age_s",
              "log_head_age_s",
              "rate_per_min", "expected_per_min", "deviation_per_min", "over_per_min",
              "vban_verdict", "vban_events", "vban_loss_ms", "vban_dest", "vban_age_s"):
        print(f"{k}={_fmt(k, res[k])}")
    return 0


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
