#!/usr/bin/env python3
"""#1310 — PURE decision core for the dev1 measurement-audio presence watchdog.

WHY: the mbc measurement-audio chain (cam2 HDMI monitor speaker plays the QPSK marker → measurement
mic → mbc Ableton on 10.77.7.232 → Dante Virtual Soundcard → stream OBS ASIO input `mbc`) is the
instrument the whole A/V-sync leg reads. After a production it can read DIGITAL SILENCE — the mic
switched off, the Ableton mbc channel muted, the Dante route dropped — and NOTHING pages it: the only
code that judges mbc silence is `scripts/lib/audio-presence-preflight.sh::audio_preflight_is_silent`
(the -60 dB bar, #748), invoked ONLY inside a full ~300 s E2E cycle (`recording-e2e.sh [4b2/8]`).
So a silent chain is invisible until the next full E2E burns a cycle discovering it — the release
E2E 34764817477 abort (`max_volume -91.0 dB`, `n=120` samples flowing but all zeros) and the
2026-07-12 week-long-muted-mic incident. This module is the pure kernel of the dev1 watchdog that
samples the mbc peak level out-of-band and decides when to page.

No I/O, no WS, no OBS — exhaustively unit-testable (pytest, Tier-0, #557 kills local cargo), the
strih-nic-selfheal #1199 / ndi-halving #1203 / dantesync-clock #1307 python-mirror precedent. The
orchestrator `scripts/measurement-audio-alert-watchdog.sh` runs a WS-meter probe
(`scripts/measurement_audio_meter_probe.py`, the I/O half), calls `analyze` here, and drives
`obs-watchdog-decision.sh`'s 2-pass confirm + `airuleset notify` with a production-critical
time-bucketed `--dedup-key` (`watchdog_notify_key`, #1308).

THRESHOLD REUSE: the SILENT/PRESENT bar is the SAME -60 dB the #748 preflight uses. It is passed IN
(the orchestrator sources `audio-presence-preflight.sh` and passes
`audio_preflight_default_threshold_db`) — `classify`/`analyze` take it as a REQUIRED argument, so the
-60 literal is NEVER retyped here. SILENT uses strict `<` — byte-identical to `audio_preflight_is_silent`
(exactly at the threshold is PRESENT, not SILENT).

Verdicts (classify):
  SKIP    — stream OBS not reachable this pass (the probe exited non-zero / WS connect failed). That
            page is #1001 (network-reach) / #732 (bundle-state) territory, never this watchdog's, so
            paging requires a successfully-fetched positive reading and a dev1-side outage can only
            produce SKIP (never a false silent page).
  UNKNOWN — reachable but the `mbc` input never appeared in the meter stream this window (renamed /
            removed input, or InputVolumeMeters unavailable on this build), or a present meter with
            no numeric level → no reading to judge, held, never a fabricated page.
  SILENT  — reachable, `mbc` meter present, peak_db < threshold → page after a 2-pass confirm.
  PRESENT — reachable, `mbc` meter present, peak_db >= threshold → healthy.
"""
import argparse
import sys


def _peak_from_lines(text):
    """`peak_db` as a float from the probe's `peak_db=<x>` key=value line, or None (absent / empty /
    non-numeric). Never a fabricated 0 — an unparseable/empty peak is None (UNKNOWN), matching the
    "unreadable is never a silent pass" convention of audio_preflight_parse_max_db."""
    for ln in text.splitlines():
        if ln.startswith("peak_db="):
            raw = ln[len("peak_db="):].strip()
            if raw == "":
                return None
            try:
                return float(raw)
            except (ValueError, TypeError):
                return None
    return None


def _meter_present_from_lines(text):
    """`meter_present` as an int (0/1) from the probe's `meter_present=<n>` line. A missing/garbled
    line defaults to 0 (ABSENT → UNKNOWN), never a fabricated 1 — a truncated probe must never read
    as a present-and-healthy meter."""
    for ln in text.splitlines():
        if ln.startswith("meter_present="):
            raw = ln[len("meter_present="):].strip()
            return 1 if raw == "1" else 0
    return 0


def extract_probe(probe_text):
    """Parse the meter probe's key=value stdout → `(peak_db_float_or_None, meter_present_int)`
    (see `_peak_from_lines` / `_meter_present_from_lines`)."""
    if not probe_text:
        return (None, 0)
    return (_peak_from_lines(probe_text), _meter_present_from_lines(probe_text))


def classify(peak_db, box_reachable, meter_present, threshold_db):
    """One pass's verdict.

      box_reachable != 1     -> SKIP     (defer #1001/#732; never our page)
      meter_present != 1     -> UNKNOWN  (mbc input absent from the meter stream; no reading)
      peak_db is None        -> UNKNOWN  (present meter but no numeric level — defensive, never a
                                          false HEALTHY whose confirm-reset would be the wrong dir)
      peak_db < threshold_db -> SILENT   (strict '<' — exactly at the bar is PRESENT, matching
                                          audio_preflight_is_silent)
      otherwise              -> PRESENT
    """
    if box_reachable != 1:
        return "SKIP"
    if meter_present != 1:
        return "UNKNOWN"
    if peak_db is None:
        return "UNKNOWN"
    if peak_db < threshold_db:
        return "SILENT"
    return "PRESENT"


def analyze(probe_text, box_reachable, threshold_db):
    """Fetch-result -> `{"verdict", "peak_db", "meter_present"}`. When the box was not reachable,
    returns SKIP WITHOUT parsing the (empty) body, mirroring the caller's no-double-page guard."""
    if box_reachable != 1:
        return {"verdict": "SKIP", "peak_db": None, "meter_present": 0}
    peak, present = extract_probe(probe_text)
    verdict = classify(peak, box_reachable, present, threshold_db)
    return {"verdict": verdict, "peak_db": peak, "meter_present": present}


def _fmt(v):
    return "" if v is None else str(v)


def _main(argv):
    ap = argparse.ArgumentParser(description="pure measurement-audio presence watchdog decision (#1310)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    a = sub.add_parser("analyze",
                       help="read the meter probe's key=value stdin -> verdict + peak_db + meter_present")
    a.add_argument("--box-reachable", type=int, required=True)
    # REQUIRED — no hardcoded -60 default. Production sources audio-presence-preflight.sh and passes
    # audio_preflight_default_threshold_db so the #748 bar is single-source (never retyped here).
    a.add_argument("--threshold-db", type=float, required=True)

    ns = ap.parse_args(argv)

    if ns.cmd == "analyze":
        # The probe emits well-formed ASCII key=value lines, but tolerant-decode anyway (the
        # ndi_halving #1203 precedent: a strict read that raised was swallowed by 2>/dev/null and read
        # as SKIP forever). box_reachable=0 needs no stdin.
        text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
        res = analyze(text, ns.box_reachable, ns.threshold_db)
        for k in ("verdict", "peak_db", "meter_present"):
            print(f"{k}={_fmt(res[k])}")
        return 0

    return 2


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
