#!/usr/bin/env python3
"""#1308 -- the ONE shared re-ping key builder for the production-critical dev1 watchdog class.

WHY (owner ruling ROZHODNUTÉ 2026-09-13, verbatim: „aj ntp aj ostatne veci bez ktorych nevie
produkcia bezat spravne musi notifikovat ... byt o tom dokolecka notifikovany"): a background fault
that production cannot run without -- a lost dante clock, a wedged OBS, a dead bundle-state server,
a missing VB-Matrix, an unreachable box, a lost NDI sender port -- must be RE-pinged repeatedly
while it PERSISTS, not paged once and then silently card-edited forever. #1307 built the first member
of this class (the dantesync-clock watchdog) with a private time-bucketed --dedup-key inside
scripts/dantesync_clock_decision.py. This module GENERALISES that ONE mechanism so every
production-critical watchdog buckets its key the SAME way and nobody hand-rolls a second copy:

  * this pure Python twin -- imported by the python-decision watchdogs (dantesync_clock_decision
    delegates to it, no second implementation);
  * the byte-for-byte bash twin `watchdog_notify_key` in scripts/lib/obs-watchdog-decision.sh --
    sourced by every bash alert-watchdog (a parity pytest diffs the two).

Contract: notify_key(base, now, interval) -> "<base>-<floor(now/interval)>".
  - interval default 600 s (10 min); a NON-numeric interval falls back to 600 (never a crash);
  - interval floored at 60 s: a smaller value (incl. a negative) is CLAMPED to 60, never a per-pass
    phone flood;
  - within one interval an identical state yields the SAME key (airuleset edits the card, no ping);
    the next interval yields a FRESH key (a new ping while the fault persists -- „dokolecka").

Recovery is NOT this helper's concern: a ✅ recovery stays ONE machine-channel log line per
watchdog (.claude/rules/watchdog-notify-dedup.md rule 2), never a phone ping. This is a DELIVERY-
layer helper only -- it builds a key; it never decides whether a fault exists.

Pure -> pytest Tier-0 (#557 kills local cargo). No I/O.
"""
import argparse
import sys

# Time-bucketed re-ping cadence (owner ruling #1307/#1308). Default 600s (10 min); floored at 60s so
# a mis-set interval can never become a per-pass phone flood.
REPING_INTERVAL_DEFAULT_S = 600
REPING_INTERVAL_FLOOR_S = 60


def reping_interval(interval_s):
    """The effective re-ping bucket size in seconds: the given value, clamped to >= the 60s floor;
    a non-numeric value falls back to the 600s default (never a crash, never a per-pass flood). A
    negative int is a valid int -> clamped to the floor (matches int() semantics; the bash twin
    strips a leading '-' before its digit test so the two agree)."""
    try:
        iv = int(interval_s)
    except (ValueError, TypeError):
        return REPING_INTERVAL_DEFAULT_S
    return iv if iv >= REPING_INTERVAL_FLOOR_S else REPING_INTERVAL_FLOOR_S


def notify_key(base, now, interval_s):
    """The TIME-BUCKETED airuleset --dedup-key: base-<floor(now/interval)>.

    Same state within one interval -> same key (airuleset edits the card, no re-ping); the next
    interval -> a new key (a fresh ping while the fault persists). `now` is injected so the cadence
    is deterministic + unit-tested."""
    iv = reping_interval(interval_s)
    try:
        bucket = int(now) // iv
    except (ValueError, TypeError):
        bucket = 0
    return f"{base}-{bucket}"


def _main(argv):
    ap = argparse.ArgumentParser(description="shared production-critical re-ping key (#1308)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    d = sub.add_parser("dedup-key", help="time-bucketed --dedup-key for a base + now + interval")
    d.add_argument("--base", required=True)
    d.add_argument("--now", type=int, required=True)
    d.add_argument("--interval", default=str(REPING_INTERVAL_DEFAULT_S))

    ns = ap.parse_args(argv)
    if ns.cmd == "dedup-key":
        print(notify_key(ns.base, ns.now, ns.interval))
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
