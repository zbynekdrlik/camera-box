#!/usr/bin/env python3
"""#813 -- the measurement A/V-sync LINE's GO/NO-GO as ONE pure, single-source-of-truth decider.

WHY THIS FILE EXISTS: two silent-failure incidents on the measurement line.
  (1) 2026-07-22 -- the measurement watchdog was dead the whole event and nobody noticed
      ("neprisla ani jedna hlaska"); silence was indistinguishable from "content can't be measured".
  (2) 2026-08-17 -- the measurement audio chain went digitally silent (~-91 dB) while the watchdog
      PROCESS stayed alive (heartbeat FRESH), caught only ~7h later at the #748 E2E preflight.

The existing dev1-side scripts/avsync-heartbeat-alert-watchdog.sh alarms on heartbeat STALENESS
only, and UNCONDITIONALLY (not bound to stream state) -- so it (a) would NOT have paged today (the
heartbeat epoch stayed fresh every ~90s) and (b) can't tell a legitimately-off box from a dead
watchdog during a live event. This module is the missing DECISION: is the measurement line producing
a fresh, VALID reading, and -- for the run-time alarm -- should we page GIVEN the stream's state?

Architecture: this module is PURE (no ssh, no OBS-WS, no subprocess, no I/O) -- every function takes
already-gathered facts and returns a verdict, exactly the "pure decision library" shape of
scripts/avsync_freshness.py / scripts/event_assert.py / scripts/lib/obs-watchdog-decision.sh.
Gathering happens in the thin caller scripts/avsync-lineup-alert-watchdog.sh (ssh heartbeat read +
obs_phase2.py stream-status). Keeping the decision layer pure makes it exhaustively Tier-0
unit-testable (tests/python/test_avsync_lineup.py) and keeps the "is the line GO" judgment in ONE
place, shared by the pre-event assert AND the run-time alarm, never re-derived.

REUSE, never a fourth measurement path: the "did the line produce a valid reading" signal is read
from the SAME on-box heartbeat scripts/avsync-watchdog.ps1 already writes every pass (parsed by
scripts/lib/avsync-heartbeat.sh). The status vocabulary this module classifies is that file's real
Write-Heartbeat output:
  "no-signal: <reason>"                    -> dead relay / stale-clip (#814)                -> NO-GO
  "measured: TIMEOUT: ..."                 -> wedged watchdog (av_sync_measure.py killed)   -> NO-GO
  "measured: ... unknown, candidates: 0"   -> silent/undecodable content on a good grab (TODAY) -> NO-GO
  "measured: A/V sync OK ..."              -> a live, in-sync reading                       -> GO
  "measured: ... ZNIZ/ZVYS ..."            -> a live, misaligned reading (still a real reading) -> GO

Everything fails CLOSED: a missing/corrupt/ambiguous fact yields the NOT-fresh / NOT-healthy /
NO-GO answer, never a silently-assumed "healthy" (this repo's standing fail-loud-not-guess
discipline -- cf. avsync_heartbeat_is_stale's "missing = stale" and avsync_freshness's "malformed =
NO-SIGNAL").

CLI (what the watchdog shell calls):
  avsync_lineup.py preflight --facts <json>   -> prints "GO" / "NO-GO: <reasons>", exit 0 / 1
  avsync_lineup.py liveness  --facts <json>   -> prints "action=<OK|ALARM|SUPPRESSED> reason=<...>",
                                                  exit 0 (OK/SUPPRESSED) / 20 (ALARM)
"""

import argparse
import json
import sys

# Default staleness windows.
#   RUN-TIME (liveness alarm): 20 min -- the ticket's operator-tolerable window during a live event
#   (a slightly-late measurement pass must not page; a genuinely dead line must).
#   PRE-EVENT (preflight assert): 5 min -- right before going live the heartbeat should be very fresh.
STALE_S_DEFAULT = 1200
PREFLIGHT_STALE_S_DEFAULT = 300

# A "measured: " heartbeat carrying ANY of these markers is NOT a valid reading -- it is a wedged
# watchdog (TIMEOUT) or silent/undecodable content on an otherwise-successful grab (unknown /
# candidates: 0), the exact 2026-08-17 case. Matched case-insensitively. Kept in ONE list so the
# preflight and the run-time alarm classify a status IDENTICALLY. These mirror the documented silent-
# measurement signatures in scripts/avsync-watchdog.ps1 (TIMEOUT) and scripts/lib/
# audio-presence-preflight.sh's own header ('av_sync verdict: "unknown", candidates: 0').
UNHEALTHY_MEASURED_MARKERS = (
    "timeout",
    "unknown",
    "candidates: 0",
    "candidates:0",
    "no-signal",
    "no signal",
    "silent",
)

# CLI exit codes.
EXIT_GO = 0
EXIT_NO_GO = 1
EXIT_ALARM = 20


# ---------------------------------------------------------------------------
# Pure sub-deciders.
# ---------------------------------------------------------------------------


def heartbeat_fresh(epoch, now, stale_s):
    """PURE: is the heartbeat epoch within [now-stale_s, now]? Fail-CLOSED -- a missing/non-numeric
    epoch/now/stale, a negative age (future stamp -> clock skew/corrupt), or an age past the window
    all return False. Mirrors scripts/lib/avsync-heartbeat.sh's avsync_heartbeat_is_stale contract
    (inverted sense: fresh == not stale), validating each arg individually (never by concatenation).
    """
    try:
        e = int(epoch)
        n = int(now)
        s = int(stale_s)
    except (TypeError, ValueError):
        return False
    age = n - e
    return 0 <= age <= s


def status_is_healthy_measured(status):
    """PURE: does the heartbeat STATUS text prove the line produced a REAL, present, decodable
    reading? True iff it starts with the "measured: " prefix scripts/avsync-watchdog.ps1 writes for a
    completed measurement AND carries none of the UNHEALTHY_MEASURED_MARKERS. A "no-signal: ..." line
    (no "measured: " prefix), an empty/None status, and a "measured: TIMEOUT/unknown/candidates: 0"
    line are all NOT healthy. Fail-CLOSED on anything ambiguous.
    """
    if not status:
        return False
    s = str(status).strip()
    if not s.startswith("measured: "):
        return False
    low = s.lower()
    return not any(m in low for m in UNHEALTHY_MEASURED_MARKERS)


def stream_is_live(output_active):
    """PURE: normalize an outputActive fact to True / False / None(unknown). A bool passes through;
    common string encodings (true/1/yes/active vs false/0/no/inactive, case-insensitive) map; anything
    else (None, garbage, an unreadable OBS-WS probe) is None -- deliberately DISTINCT from False so
    the caller can treat "OBS unreachable" differently from "stream genuinely off".
    """
    if output_active is None:
        return None
    if isinstance(output_active, bool):
        return output_active
    v = str(output_active).strip().lower()
    if v in ("true", "1", "yes", "active", "on"):
        return True
    if v in ("false", "0", "no", "inactive", "off"):
        return False
    return None


# ---------------------------------------------------------------------------
# Decision surface 1 -- the pre-event GO/NO-GO of the measurement LINE.
# ---------------------------------------------------------------------------


def preflight_verdict(facts):
    """PURE: GO iff ALL hold -- the measurement watchdog writes a FRESH heartbeat, its last reading
    is a VALID measurement, the dev1 forwarder/alert timer is active (so alarms would actually reach
    the phone during the event), AND a REAL Discord test-ping was delivered (HTTP 200). Returns
    (go: bool, reasons: [str]) where reasons NAMES every failing check in plain Slovak, so a NO-GO is
    an operator-actionable alert BEFORE the event, never a bare exit code.
    """
    reasons = []
    stale_s = facts.get("preflight_stale_s", PREFLIGHT_STALE_S_DEFAULT)
    status = facts.get("heartbeat_status", "")
    fwd = facts.get("forwarder_present")
    http = facts.get("discord_ping_http")

    if not heartbeat_fresh(facts.get("heartbeat_epoch"), facts.get("now"), stale_s):
        reasons.append(
            "meraci watchdog na stream boxe nepise cerstvy heartbeat "
            "(proces mrtvy alebo starsi nez {}s)".format(stale_s)
        )
    if not status_is_healthy_measured(status):
        reasons.append(
            "posledne meranie nie je platne (stav: '{}') -- audio retazec je "
            "tichy/nedekodovatelny alebo watchdog zaseknuty".format(status or "<ziadny>")
        )
    if fwd is not True:
        reasons.append(
            "dev1 forwarder/alert timer nie je aktivny -- alarmy by sa pocas eventu nedorucili"
        )
    if str(http) != "200":
        reasons.append(
            "Discord test ping sa nedorucil (HTTP {}, ocakavam 200)".format(
                http if http is not None else "<ziadny>"
            )
        )
    return (len(reasons) == 0, reasons)


# ---------------------------------------------------------------------------
# Decision surface 2 -- the run-time liveness alarm BOUND TO STREAM STATE.
# ---------------------------------------------------------------------------


def liveness_alarm(facts):
    """PURE: returns (action, reason) with action in {"OK", "ALARM", "SUPPRESSED"}.

      - stream NOT live (outputActive=False) -> SUPPRESSED: the box is off / between events, so a
        silent measurement line is expected -- never page (this is the "bind to stream state"
        distinction the ticket asks for; a plain stale-log alarm can't make it).
      - stream state UNKNOWN (OBS-WS unreachable) -> SUPPRESSED: "is OBS reachable/alive" is owned by
        the network-reach + obs-liveness watchdogs, so this alarm does not double-page; it resumes
        the moment OBS answers again. The reason names it INCONCLUSIVE so the log stays honest.
      - stream LIVE and the line is fresh + healthy -> OK.
      - stream LIVE and the line is stale OR the last reading is invalid -> ALARM. This is the bar:
        a fresh heartbeat with a "measured: unknown/candidates: 0" (silent audio) status pages here,
        which the existing staleness-only watchdog misses entirely.

    The ALARM reason never promises to self-recover -- it states plainly that intervention is needed
    (the honest half of the ticket's "either really restart, or write 'needs a hand'"; a scheduled
    self-heal restart is out of this decider's scope by design).
    """
    live = stream_is_live(facts.get("stream_output_active"))
    if live is False:
        return ("SUPPRESSED", "stream nevysiela (outputActive=false) -- merat netreba, ticho je v poriadku")
    if live is None:
        return (
            "SUPPRESSED",
            "stav streamu sa neda precitat (OBS-WS nedostupny) -- INCONCLUSIVE; "
            "dosiahnutelnost/zivotnost OBS vlastnia network-reach/obs-liveness watchdogy",
        )

    stale_s = facts.get("stale_s", STALE_S_DEFAULT)
    fresh = heartbeat_fresh(facts.get("heartbeat_epoch"), facts.get("now"), stale_s)
    status = facts.get("heartbeat_status", "")
    healthy = status_is_healthy_measured(status)
    if fresh and healthy:
        return ("OK", "stream vysiela a meracia linka dava platne meranie")

    parts = []
    if not fresh:
        parts.append("ziadny cerstvy heartbeat > {}s (meraci watchdog mrtvy)".format(stale_s))
    if not healthy:
        parts.append(
            "posledne meranie neplatne (stav: '{}') -- audio retazec tichy/nedekodovatelny".format(
                status or "<ziadny>"
            )
        )
    return (
        "ALARM",
        "stream VYSIELA, ale " + " a ".join(parts) + " -- treba zasah (alarm sam neozivuje)",
    )


# ---------------------------------------------------------------------------
# CLI.
# ---------------------------------------------------------------------------


def _load_facts(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def main(argv=None):
    ap = argparse.ArgumentParser(description="#813 measurement-line GO/NO-GO + liveness decider")
    sub = ap.add_subparsers(dest="mode", required=True)

    p_pre = sub.add_parser("preflight", help="pre-event GO/NO-GO of the measurement line")
    p_pre.add_argument("--facts", required=True, help="path to the gathered-facts JSON")

    p_live = sub.add_parser("liveness", help="run-time liveness alarm bound to stream state")
    p_live.add_argument("--facts", required=True, help="path to the gathered-facts JSON")

    a = ap.parse_args(argv)
    facts = _load_facts(a.facts)

    if a.mode == "preflight":
        go, reasons = preflight_verdict(facts)
        if go:
            print("GO")
            return EXIT_GO
        print("NO-GO: " + " | ".join(reasons))
        return EXIT_NO_GO

    # liveness
    action, reason = liveness_alarm(facts)
    print("action={} reason={}".format(action, reason))
    return EXIT_ALARM if action == "ALARM" else EXIT_GO


if __name__ == "__main__":
    sys.exit(main())
