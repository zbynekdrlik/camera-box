#!/usr/bin/env python3
"""#1226 — PURE decision core for the dev1 audio-lag alert watchdog.

WHY: on 2026-08-30 (nedeľná služba) stream OBS's audio pipeline fell ~24 s/min behind realtime
under stream load; `audio-telemetry #800 '<src>': ts_lag_ms=N` (obs-audio.c:698) grew to 1 672 741 ms
(27,9 min) and SCREAMED into the OBS log the whole hour, but nothing off the box read it, so the
YouTube stream's A/V desynced for a whole service before a viewer noticed. bundle_state_gather now
exposes the MAX per-source lag as the `audio_ts_lag_ms` facet on `:8899/bundle-state.json`; this
module is the pure kernel of the dev1 watchdog that reads that facet from strih+stream and decides
when to page.

No I/O, no ssh, no OBS, no MCP — exhaustively unit-testable (pytest), the strih-nic-selfheal #1199 /
ndi-halving #1203 python-mirror precedent, so the decision RED->GREENs LOCALLY under Tier-0 (#557
kills cargo). The orchestrator scripts/audio-lag-alert-watchdog.sh curls the JSON, calls `analyze`
here, and drives obs-watchdog-decision.sh's confirm/throttle + airuleset notify (--dedup-key #1206).

Verdicts (classify):
  SKIP     -- box could not be fetched (:8899 down / box down). That page is #732 (bundle-state) /
              #1001 (network-reach) territory, never this watchdog's -- so paging requires a
              successfully fetched POSITIVE lag reading, and a dev1-side outage can only produce
              SKIP (never a false audio page).
  STALE    -- (#1231) box fetched OK, telemetry PRESENT but the freshest #800 line sits > a few emit
              periods behind the OBS log's newest line (audio_ts_lag_age_s > stale_threshold_s): the
              audio tick stopped WHILE the log kept advancing. Surfaced DISTINCTLY (machine-channel
              log, NO phone page -- absence is never paged; a fully-down box is #732/#1001), so it is
              never a false HEALTHY and never a LAGGING page off a stale reading.
  UNKNOWN  -- box fetched OK but the audio_ts_lag_ms facet is absent (audio telemetry not present:
              a stock OBS, or no #800 line in the tail yet). No reading -> no page.
  HEALTHY  -- lag <= threshold_ms.
  LAGGING  -- lag > threshold_ms. The watchdog pages after a 2-pass confirm.
"""
import argparse
import json
import sys

DEFAULT_THRESHOLD_MS = 5000
# #1231 — telemetry older than this many seconds behind the OBS log head is STALE. ~3x the 60 s #800
# emit period, matching bundle_state_gather.AUDIO_TS_LAG_STALE_AFTER_S (the box's per-source filter),
# so a box that ships an empty lag (all sources stale) also ships an age over this threshold.
DEFAULT_STALE_THRESHOLD_S = 180


def extract_audio_lag(bundle_json_text):
    """Parse a /bundle-state.json body -> `(lag_ms_int_or_None, src_or_None)`.

    Returns `(None, None)` for: empty/None input, non-JSON, a non-object top level, a missing or
    empty `audio_ts_lag_ms`, or a value that is not an integer string (UNKNOWN — never a fabricated
    reading, matching the gather's omit-when-empty / never-a-fake-0 contract)."""
    if not bundle_json_text:
        return (None, None)
    try:
        obj = json.loads(bundle_json_text)
    except (ValueError, TypeError):
        return (None, None)
    if not isinstance(obj, dict):
        return (None, None)
    raw = obj.get("audio_ts_lag_ms")
    if raw is None or (isinstance(raw, str) and raw.strip() == ""):
        return (None, None)
    try:
        lag = int(str(raw).strip())
    except (ValueError, TypeError):
        return (None, None)
    src = obj.get("audio_ts_lag_src")
    src = str(src) if src is not None else None
    return (lag, src)


def extract_audio_age(bundle_json_text):
    """#1231 — parse a /bundle-state.json body -> the audio_ts_lag_age_s facet as an int (seconds),
    or None. None for: empty/None input, non-JSON, a non-object top level, a missing/empty
    audio_ts_lag_age_s, or a non-integer value (UNKNOWN — never a fabricated age, matching the
    gather's omit-when-empty contract). A "0" is a real value (fresh telemetry present), NOT None."""
    if not bundle_json_text:
        return None
    try:
        obj = json.loads(bundle_json_text)
    except (ValueError, TypeError):
        return None
    if not isinstance(obj, dict):
        return None
    raw = obj.get("audio_ts_lag_age_s")
    if raw is None or (isinstance(raw, str) and raw.strip() == ""):
        return None
    try:
        return int(str(raw).strip())
    except (ValueError, TypeError):
        return None


def classify(lag_ms, box_reachable, threshold_ms=DEFAULT_THRESHOLD_MS,
             age_s=None, stale_threshold_s=DEFAULT_STALE_THRESHOLD_S):
    """One box's verdict. `box_reachable` is 1 iff the JSON was fetched this pass.

      box_reachable != 1        -> SKIP     (defer to #732/#1001; never our page)
      age_s > stale_threshold_s -> STALE    (#1231: telemetry present but stopped while the log
                                             advanced; decided BEFORE the lag checks so a stale
                                             reading is never a false LAGGING page. age_s None — an
                                             old box with no freshness facet — skips this branch,
                                             so the pre-#1231 lag decision is preserved exactly)
      lag_ms is None            -> UNKNOWN  (facet absent; no reading to judge)
      lag_ms < 0                -> UNKNOWN  (a -1 == no audio timeline yet; the gather already
                                             excludes negatives, but treat one defensively as
                                             UNKNOWN — not HEALTHY, whose confirm-reset would be the
                                             wrong failure direction)
      lag_ms > threshold        -> LAGGING
      otherwise                 -> HEALTHY
    """
    if box_reachable != 1:
        return "SKIP"
    if age_s is not None and age_s > stale_threshold_s:
        return "STALE"
    if lag_ms is None:
        return "UNKNOWN"
    if lag_ms < 0:
        return "UNKNOWN"
    if lag_ms > threshold_ms:
        return "LAGGING"
    return "HEALTHY"


def analyze(bundle_json_text, box_reachable, threshold_ms=DEFAULT_THRESHOLD_MS,
            stale_threshold_s=DEFAULT_STALE_THRESHOLD_S):
    """Fetch-result -> `{"verdict", "lag_ms", "src"}`. When the box was not reachable, returns SKIP
    WITHOUT parsing the (empty) body, mirroring the caller's no-double-page guard. The #1231
    freshness age drives the STALE verdict internally but is NOT added to the returned dict (the
    shell reads it from the separate CLI `age_s=` line)."""
    if box_reachable != 1:
        return {"verdict": "SKIP", "lag_ms": None, "src": None}
    lag, src = extract_audio_lag(bundle_json_text)
    age_s = extract_audio_age(bundle_json_text)
    verdict = classify(lag, box_reachable, threshold_ms, age_s, stale_threshold_s)
    return {"verdict": verdict, "lag_ms": lag, "src": src}


def _fmt(v):
    return "" if v is None else str(v)


def _main(argv):
    ap = argparse.ArgumentParser(description="pure audio-lag watchdog decisions (#1226)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    a = sub.add_parser("analyze",
                       help="read /bundle-state.json on stdin -> verdict + lag_ms + src + age_s")
    a.add_argument("--box-reachable", type=int, required=True)
    a.add_argument("--threshold-ms", type=int, default=DEFAULT_THRESHOLD_MS)
    a.add_argument("--stale-threshold-s", type=int, default=DEFAULT_STALE_THRESHOLD_S)

    ns = ap.parse_args(argv)

    if ns.cmd == "analyze":
        # The bundle-state body is well-formed UTF-8 JSON, but read bytes + tolerant-decode anyway
        # (the ndi_halving #1203 hotfix precedent: a strict read that raised was swallowed by the
        # caller's 2>/dev/null and read as SKIP forever). box_reachable=0 needs no stdin.
        text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
        res = analyze(text, ns.box_reachable, ns.threshold_ms, ns.stale_threshold_s)
        for k in ("verdict", "lag_ms", "src"):
            print(f"{k}={_fmt(res[k])}")
        # #1231 — the freshness age is an ADDITIONAL line (not in the analyze dict) so the existing
        # verdict/lag_ms/src CLI contract is unchanged; the shell logs it and it corroborates STALE.
        age_s = None if ns.box_reachable != 1 else extract_audio_age(text)
        print(f"age_s={_fmt(age_s)}")
        return 0

    return 2


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
