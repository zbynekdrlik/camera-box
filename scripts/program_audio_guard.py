#!/usr/bin/env python3
"""issue 1404 -- the stream program-audio GUARD: may the stream program go to YouTube right now?

Reads `http://dev1:8890/program-audio.json` (written by scripts/program_audio_sampler.py, served by
scripts/rig-lease-server.py with `age_s` recomputed per request) and answers with an exit code.
Both YouTube gates call it before the broadcast starts and every ~10 s while it is live (camera-box
`scripts/lib/youtube-leg.sh`, restreamer issue 357) and stop the broadcast on anything but 0.

  exit 0  MEASUREMENT or SILENT, and fresh (-1 s <= age_s <= --max-age), and no FOREIGN window
          within --max-age
  exit 1  FOREIGN (non-measurement audio on the program) -- also when stale: an old FOREIGN
          reading is never downgraded to "maybe" -- and also when the CURRENT window is clean but
          a FOREIGN window ended within --max-age (`last_foreign_age_s`, the sampler's latch), so a
          gate polling every --max-age seconds never misses a FOREIGN window between two polls
  exit 2  UNKNOWN, stale (age_s > --max-age, missing, or more than 1 s in the future -- a small
          negative age is a dantesync date step), unreachable, an HTTP or protocol error, an
          unreadable payload, an unexpected verdict -- fail CLOSED

Output, always exactly one line on stdout:
  program-audio verdict=<V> rms=<x> outside_band=<y>% age=<s>[ reason=<why>]
<V> is the EFFECTIVE verdict (UNKNOWN when the reading cannot be trusted), `-` for a missing
number, and `reason=` explains every non-trivial outcome (stale, unreachable, the sampler's own
UNKNOWN reason).

Usage:
  program_audio_guard.py [--url http://dev1:8890/program-audio.json] [--max-age 10] [--timeout 5]
"""
from __future__ import annotations

import argparse
import http.client
import json
import math
import sys
import urllib.error
import urllib.request

DEFAULT_URL = "http://dev1:8890/program-audio.json"
DEFAULT_MAX_AGE_S = 10.0
DEFAULT_TIMEOUT_S = 5.0
NEGATIVE_AGE_TOLERANCE_S = 1.0  # a dantesync date step moves dev1's clock by tens of ms

EXIT_OK = 0
EXIT_FOREIGN = 1
EXIT_UNKNOWN = 2


def _num(v):
    return v if isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v) else None


def _fmt(v) -> str:
    v = _num(v)
    return "-" if v is None else f"{v:.1f}"


def _line(verdict, rms, outside, age, reason=None) -> str:
    line = (f"program-audio verdict={verdict} rms={_fmt(rms)} outside_band={_fmt(outside)}% "
            f"age={_fmt(age)}")
    if reason:
        line += " reason=" + " ".join(str(reason).split())
    return line


def decide(payload: dict, max_age_s: float) -> tuple[int, str]:
    """(exit code, the one output line) for a fetched payload."""
    verdict = payload.get("verdict")
    rms, outside, age = payload.get("rms_dbfs"), payload.get("outside_band_pct"), _num(payload.get("age_s"))
    stale = age is None or age > max_age_s or age < -NEGATIVE_AGE_TOLERANCE_S
    if age is None:
        stale_why = "stale (no age)"
    elif age < -NEGATIVE_AGE_TOLERANCE_S:
        stale_why = f"stale (age {age:.1f} s is in the future)"
    else:
        stale_why = f"stale (age {age:.1f} s > max {max_age_s:g} s)"
    if verdict == "FOREIGN":
        return EXIT_FOREIGN, _line("FOREIGN", rms, outside, age, stale_why if stale else None)
    last_foreign = _num(payload.get("last_foreign_age_s"))
    if last_foreign is not None and last_foreign <= max_age_s:
        return EXIT_FOREIGN, _line("FOREIGN", rms, outside, age,
                                   f"FOREIGN window {last_foreign:.1f} s ago (latched; current {verdict})")
    if verdict in ("MEASUREMENT", "SILENT"):
        if stale:
            return EXIT_UNKNOWN, _line("UNKNOWN", rms, outside, age, f"{stale_why}, last verdict {verdict}")
        return EXIT_OK, _line(verdict, rms, outside, age)
    if verdict == "UNKNOWN":
        return EXIT_UNKNOWN, _line("UNKNOWN", rms, outside, age, payload.get("reason") or "sampler UNKNOWN")
    return EXIT_UNKNOWN, _line("UNKNOWN", rms, outside, age, f"unexpected verdict {verdict!r}")


def fetch(url: str, timeout_s: float) -> dict:
    """GET + parse. Raises ValueError with a one-line reason on any failure."""
    try:
        with urllib.request.urlopen(url, timeout=timeout_s) as resp:
            body = resp.read()
    except urllib.error.HTTPError as exc:
        raise ValueError(f"HTTP {exc.code} from {url}") from exc
    except (urllib.error.URLError, OSError) as exc:
        reason = getattr(exc, "reason", exc)
        raise ValueError(f"unreachable: {url}: {reason}") from exc
    except http.client.HTTPException as exc:  # a garbled status line, a truncated body
        raise ValueError(f"broken HTTP response from {url}: {exc!r}") from exc
    try:
        payload = json.loads(body)
    except ValueError as exc:
        raise ValueError(f"unreadable payload from {url}: {exc}") from exc
    if not isinstance(payload, dict):
        raise ValueError(f"unreadable payload from {url}: not a JSON object")
    return payload


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="issue 1404 -- stream program-audio guard (exit 0/1/2)")
    ap.add_argument("--url", default=DEFAULT_URL, help=f"default {DEFAULT_URL}")
    ap.add_argument("--max-age", type=float, default=DEFAULT_MAX_AGE_S,
                    help=f"seconds a reading stays fresh (default {DEFAULT_MAX_AGE_S:g})")
    ap.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT_S,
                    help=f"HTTP timeout in seconds (default {DEFAULT_TIMEOUT_S:g})")
    args = ap.parse_args(argv)
    try:
        payload = fetch(args.url, args.timeout)
        code, line = decide(payload, args.max_age)
    except Exception as exc:  # fail CLOSED with the contract line on anything unforeseen
        reason = str(exc) if isinstance(exc, ValueError) else f"guard error: {exc!r}"
        print(_line("UNKNOWN", None, None, None, reason), flush=True)
        return EXIT_UNKNOWN
    print(line, flush=True)
    return code


if __name__ == "__main__":
    sys.exit(main())
