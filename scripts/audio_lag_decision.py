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
  UNKNOWN  -- box fetched OK but the audio_ts_lag_ms facet is absent (audio telemetry not present:
              a stock OBS, or no #800 line in the tail yet). No reading -> no page.
  HEALTHY  -- lag <= threshold_ms.
  LAGGING  -- lag > threshold_ms. The watchdog pages after a 2-pass confirm.
"""
import argparse
import json
import sys

DEFAULT_THRESHOLD_MS = 5000


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


def classify(lag_ms, box_reachable, threshold_ms=DEFAULT_THRESHOLD_MS):
    """One box's verdict. `box_reachable` is 1 iff the JSON was fetched this pass.

      box_reachable != 1 -> SKIP     (defer to #732/#1001; never our page)
      lag_ms is None     -> UNKNOWN  (facet absent; no reading to judge)
      lag_ms > threshold -> LAGGING
      otherwise          -> HEALTHY
    """
    if box_reachable != 1:
        return "SKIP"
    if lag_ms is None:
        return "UNKNOWN"
    if lag_ms > threshold_ms:
        return "LAGGING"
    return "HEALTHY"


def analyze(bundle_json_text, box_reachable, threshold_ms=DEFAULT_THRESHOLD_MS):
    """Fetch-result -> `{"verdict", "lag_ms", "src"}`. When the box was not reachable, returns SKIP
    WITHOUT parsing the (empty) body, mirroring the caller's no-double-page guard."""
    if box_reachable != 1:
        return {"verdict": "SKIP", "lag_ms": None, "src": None}
    lag, src = extract_audio_lag(bundle_json_text)
    return {"verdict": classify(lag, box_reachable, threshold_ms), "lag_ms": lag, "src": src}


def _fmt(v):
    return "" if v is None else str(v)


def _main(argv):
    ap = argparse.ArgumentParser(description="pure audio-lag watchdog decisions (#1226)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    a = sub.add_parser("analyze", help="read /bundle-state.json on stdin -> verdict + lag_ms + src")
    a.add_argument("--box-reachable", type=int, required=True)
    a.add_argument("--threshold-ms", type=int, default=DEFAULT_THRESHOLD_MS)

    ns = ap.parse_args(argv)

    if ns.cmd == "analyze":
        # The bundle-state body is well-formed UTF-8 JSON, but read bytes + tolerant-decode anyway
        # (the ndi_halving #1203 hotfix precedent: a strict read that raised was swallowed by the
        # caller's 2>/dev/null and read as SKIP forever). box_reachable=0 needs no stdin.
        text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
        res = analyze(text, ns.box_reachable, ns.threshold_ms)
        for k in ("verdict", "lag_ms", "src"):
            print(f"{k}={_fmt(res[k])}")
        return 0

    return 2


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
