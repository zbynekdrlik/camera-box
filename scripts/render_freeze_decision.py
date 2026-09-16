#!/usr/bin/env python3
"""#1320 — PURE decision core for the dev1 render-freeze / relock-storm alert watchdog.

WHY: on 15.9.2026 a scene-switch-coincident DistroAV reattach whose blocking `NDIlib_recv_destroy`
ran on strih's OBS graphics thread froze the PROGRAM render ~7.5 s (`program-render-audit lagged=228
avg_frame_ms=782`) -> the `2ME PGM` NDI output was starved -> the stream receive FIFO underran ->
a 462-relock overshoot STORM -> the presented on-air video sat +2/+3 frames late for ~40 min. The
owner only noticed ~90 min later via the av-sync dock offset (issue 1318). The cure (bundle
02b53180b) moves the teardown off the graphics thread; this watchdog is the GUARDRAIL that pages in
~10 min if the freeze — or its receiver-side storm — ever RECURS.

Two page-able bundle-state facets on each box's :8899 (bundle_state_gather):
  * program_render_lagged (+ _age_s)  — MAX `program-render-audit lagged` over the tail (a `lagged>0`
    window == a PROGRAM render-thread freeze) + the in-log age of the most recent such window.
  * relock_bursts (+ _age_s)          — issue 1318's summarize_relock_bursts ported to the gather: the
    MAX per-input burst count (>=8 relocks within 1 s == a FIFO overshoot storm) + the age of the
    newest relock event.

No I/O, no ssh, no OBS — exhaustively pytest-able under Tier-0 (which kills cargo). The
audio_lag_decision.py / ndi_halving_decision.py (#1199) python-mirror precedent; the orchestrator
scripts/render-freeze-alert-watchdog.sh curls the JSON, calls `analyze` here, and drives
obs-watchdog-decision.sh's confirm/throttle + airuleset notify (time-bucketed --dedup-key, the
production-critical class of issue 1308).

RENDER arm verdicts (classify_render):
  SKIP           -- box not fetched this pass (:8899 down / box down). Deferred to issue 732 /
                    issue 1001; never our page. Paging REQUIRES a fetched positive reading.
  UNKNOWN        -- box fetched OK but no program_render_lagged facet (a stock OBS / no
                    program-render-audit line in the tail yet). No reading -> no page.
  HEALTHY        -- lagged below the magnitude FLOOR (a relaunch startup-lag: observed band 1/2/11),
                    OR present but STALE (age > fresh bound — a freeze that scrolled deep into the
                    tail / an old relaunch lag; e.g. stream's live prl=2 age=4464), OR lagged=0.
  RENDER_FREEZE  -- lagged >= FLOOR (default 30, above the relaunch band, below the smallest genuine
                    freeze — the 17:04 partial was 61, the 18:27 severe was 228) AND FRESH (age <=
                    fresh bound). The watchdog pages after a 2-pass confirm.

RELOCK arm verdicts (classify_relock):
  SKIP / UNKNOWN -- as the render arm (no relock_bursts facet at all is the steady state).
  HEALTHY        -- bursts below min (0 == relock telemetry live, no storm) OR present but STALE.
  RELOCK_STORM   -- bursts >= min (default 1) AND FRESH. Pages after a 2-pass confirm.
"""
import argparse
import json
import sys

DEFAULT_LAGGED_FLOOR = 30       # relaunch band is prl 1/2/11; genuine freeze is 61 (partial) / 228.
DEFAULT_MIN_BURSTS = 1          # one >=8-in-1s burst on any input is already a storm (issue 1318).
# Fresh bound: a freeze/storm's evidence line ages every pass; ~2x the 5-min cadence + margin spans
# the 2-pass confirm so a real, recent event pages while a stale one (deep in the tail / an old
# relaunch lag) does not. Env/CLI-overridable per arm.
DEFAULT_RENDER_FRESH_AGE_S = 600
DEFAULT_RELOCK_FRESH_AGE_S = 600


def _loads_obj(bundle_json_text):
    """A /bundle-state.json body -> its dict, or None (empty/None, non-JSON, or a non-object top
    level). The ONE json parse — both arms route through it so a body is never parsed twice."""
    if not bundle_json_text:
        return None
    try:
        obj = json.loads(bundle_json_text)
    except (ValueError, TypeError):
        return None
    return obj if isinstance(obj, dict) else None


def _int_or_none(raw):
    """A facet value (str/int/None) -> int, or None for absent/empty/non-integer (UNKNOWN — never a
    fabricated reading, matching the gather's omit-when-empty / never-a-fake-0 contract)."""
    if raw is None or (isinstance(raw, str) and raw.strip() == ""):
        return None
    try:
        return int(str(raw).strip())
    except (ValueError, TypeError):
        return None


def _facets_from_obj(obj):
    """`(lagged, lagged_age_s, bursts, bursts_age_s)` ints/None from an already-parsed bundle dict."""
    if not isinstance(obj, dict):
        return (None, None, None, None)
    return (
        _int_or_none(obj.get("program_render_lagged")),
        _int_or_none(obj.get("program_render_lagged_age_s")),
        _int_or_none(obj.get("relock_bursts")),
        _int_or_none(obj.get("relock_bursts_age_s")),
    )


def extract(bundle_json_text):
    """Parse a /bundle-state.json body -> `(lagged, lagged_age_s, bursts, bursts_age_s)` (ints/None)."""
    return _facets_from_obj(_loads_obj(bundle_json_text))


def classify_render(lagged, age_s, box_reachable, lagged_floor=DEFAULT_LAGGED_FLOOR,
                    fresh_age_s=DEFAULT_RENDER_FRESH_AGE_S):
    """One box's RENDER-arm verdict. `box_reachable` is 1 iff the JSON was fetched this pass.

      box_reachable != 1                 -> SKIP     (defer to issue 732/1001; never our page)
      lagged is None                     -> UNKNOWN  (facet absent; no reading to judge)
      lagged < lagged_floor              -> HEALTHY  (a relaunch startup-lag / lagged=0; below the
                                                      magnitude floor is not a freeze)
      age_s is not None and age_s > bound-> HEALTHY  (present + severe but STALE: a freeze that
                                                      scrolled deep into the tail / an old relaunch
                                                      lag — not a RECENT freeze. age_s None (an old
                                                      gather with no age facet) skips this branch)
      otherwise                          -> RENDER_FREEZE
    """
    if box_reachable != 1:
        return "SKIP"
    if lagged is None:
        return "UNKNOWN"
    if lagged < lagged_floor:
        return "HEALTHY"
    if age_s is not None and age_s > fresh_age_s:
        return "HEALTHY"
    return "RENDER_FREEZE"


def classify_relock(bursts, age_s, box_reachable, min_bursts=DEFAULT_MIN_BURSTS,
                    fresh_age_s=DEFAULT_RELOCK_FRESH_AGE_S):
    """One box's RELOCK-arm verdict (same shape as classify_render).

      box_reachable != 1                 -> SKIP
      bursts is None                     -> UNKNOWN  (no relock_bursts facet — the steady state)
      bursts < min_bursts                -> HEALTHY  (0 == relocks live, no storm)
      age_s is not None and age_s > bound-> HEALTHY  (a storm that aged out of the fresh window)
      otherwise                          -> RELOCK_STORM
    """
    if box_reachable != 1:
        return "SKIP"
    if bursts is None:
        return "UNKNOWN"
    if bursts < min_bursts:
        return "HEALTHY"
    if age_s is not None and age_s > fresh_age_s:
        return "HEALTHY"
    return "RELOCK_STORM"


def analyze(bundle_json_text, box_reachable, lagged_floor=DEFAULT_LAGGED_FLOOR,
            render_fresh_age_s=DEFAULT_RENDER_FRESH_AGE_S, min_bursts=DEFAULT_MIN_BURSTS,
            relock_fresh_age_s=DEFAULT_RELOCK_FRESH_AGE_S):
    """Fetch-result -> both arms' verdicts + readings. When the box was not reachable, returns SKIP
    WITHOUT parsing the (empty) body, mirroring the caller's no-double-page guard."""
    if box_reachable != 1:
        return {"render_verdict": "SKIP", "lagged": None, "lagged_age_s": None,
                "relock_verdict": "SKIP", "bursts": None, "bursts_age_s": None}
    lagged, lagged_age, bursts, bursts_age = _facets_from_obj(_loads_obj(bundle_json_text))
    return {
        "render_verdict": classify_render(lagged, lagged_age, box_reachable, lagged_floor,
                                          render_fresh_age_s),
        "lagged": lagged, "lagged_age_s": lagged_age,
        "relock_verdict": classify_relock(bursts, bursts_age, box_reachable, min_bursts,
                                          relock_fresh_age_s),
        "bursts": bursts, "bursts_age_s": bursts_age,
    }


def _fmt(v):
    return "" if v is None else str(v)


def _main(argv):
    ap = argparse.ArgumentParser(description="pure render-freeze / relock-storm watchdog decisions (#1320)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("analyze",
                       help="read /bundle-state.json on stdin -> both arms' verdicts + readings")
    a.add_argument("--box-reachable", type=int, required=True)
    a.add_argument("--lagged-floor", type=int, default=DEFAULT_LAGGED_FLOOR)
    a.add_argument("--render-fresh-age-s", type=int, default=DEFAULT_RENDER_FRESH_AGE_S)
    a.add_argument("--min-bursts", type=int, default=DEFAULT_MIN_BURSTS)
    a.add_argument("--relock-fresh-age-s", type=int, default=DEFAULT_RELOCK_FRESH_AGE_S)
    ns = ap.parse_args(argv)

    if ns.cmd == "analyze":
        # Tolerant read (the ndi_halving #1203 precedent: a strict read that raised was swallowed by
        # the caller's 2>/dev/null and read as SKIP forever). box_reachable=0 needs no stdin.
        text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
        res = analyze(text, ns.box_reachable, ns.lagged_floor, ns.render_fresh_age_s,
                      ns.min_bursts, ns.relock_fresh_age_s)
        for k in ("render_verdict", "lagged", "lagged_age_s",
                  "relock_verdict", "bursts", "bursts_age_s"):
            print(f"{k}={_fmt(res[k])}")
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
