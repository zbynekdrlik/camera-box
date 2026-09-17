#!/usr/bin/env python3
"""#813 -- the measurement A/V-sync LINE's GO/NO-GO as ONE pure, single-source-of-truth decider.

WHY THIS FILE EXISTS: two silent-failure incidents on the measurement line.
  (1) 2026-07-22 -- the measurement watchdog was dead the whole event and nobody noticed
      ("neprisla ani jedna hlaska"); silence was indistinguishable from "content can't be measured".
  (2) 2026-08-17 -- the measurement audio chain went digitally silent (~-91 dB) while the watchdog
      PROCESS stayed alive (heartbeat FRESH), caught only ~7h later at the #748 E2E preflight.

The existing dev1-side scripts/avsync-heartbeat-alert-watchdog.sh alarms on heartbeat STALENESS
only, and UNCONDITIONALLY -- so it (a) would NOT have paged today (the heartbeat epoch stayed fresh
every ~90s) and (b) can't tell a legitimately-off box from a dead watchdog during a live event.

WHAT THE CONTENT-LIVENESS SIGNAL ACTUALLY IS (the load-bearing correction): the on-box heartbeat is
written by scripts/avsync-watchdog.ps1 as `measured: db=<X> <last line of av_sync_measure.py>`.
av_sync_measure.py's OWN verdict text CANNOT distinguish silent audio from a normal band/graphics
segment -- it prints `[stamp] UNMEASURABLE window (... band/graphics segments are expected to skip)`
for BOTH (there is no usable face/lips in either case). The ONLY signal that distinguishes the
2026-08-17 incident (silent audio) from an ordinary no-face segment is the AUDIO LEVEL in dB
(digital silence ~-91 dB vs a live QPSK marker ~-5 dB -- scripts/lib/audio-presence-preflight.sh).
So avsync-watchdog.ps1 now prefixes the heartbeat with `db=<max_volume>` (ffmpeg volumedetect on the
SAME clip it already grabs every ~90s -- NO fourth measurement path), and this decider classifies
content-liveness on `db >= -60` (audio present) rather than on the SyncNet verdict text. A band
segment with audio present is VALID (the instrument is alive); silent audio is INVALID (dead line).

Architecture: this module is PURE (no ssh, no OBS-WS, no subprocess, no I/O) -- every function takes
already-gathered facts and returns a verdict, the "pure decision library" shape of
scripts/avsync_freshness.py / scripts/event_assert.py / scripts/lib/obs-watchdog-decision.sh.
Gathering happens in the thin caller scripts/avsync-lineup-alert-watchdog.sh. Everything fails
CLOSED: a missing/corrupt/ambiguous fact yields the NOT-fresh / NOT-present / NO-GO answer.

Heartbeat vocabulary this decider reads (avsync-watchdog.ps1 + av_sync_measure.py, verified):
  "no-signal: <reason>"                              -> dead relay / stale-clip (grab failed) (#814)
  "measured: db=<X> [stamp] AV offset ... A/V sync OK"   -> live, in-sync, audio present  -> VALID
  "measured: db=<X> [stamp] AV offset ... ZNIZ/ZVYS"     -> live, misaligned, audio present -> VALID
  "measured: db=<X> [stamp] UNMEASURABLE window (...)"   -> band segment: VALID if db>=-60, else the
                                                           silent-audio DEAD LINE (the 2026-08-17 case)
  "measured: db=<X> TIMEOUT: ..."                        -> wedged watchdog                -> INVALID

CLI:
  avsync_lineup.py preflight --facts <json>  -> "GO" / "NO-GO: <reasons>", exit 0 / 1
  avsync_lineup.py liveness  --facts <json>  -> "action=<OK|ALARM|SUPPRESSED> reason=<...> sig=<...>",
                                                exit 0 (OK/SUPPRESSED) / 20 (ALARM)
  avsync_lineup.py offset    --facts <json>  -> same shape; #1331 A/V-offset alarm (page when a
                                                CONFIDENT SyncNet offset is out of band during a
                                                LIVE stream), exit 0 (OK/SUPPRESSED) / 20 (ALARM)
"""

import argparse
import json
import re
import sys

# Default staleness windows.
#   RUN-TIME (liveness alarm): 20 min -- the ticket's operator-tolerable event window.
#   PRE-EVENT (preflight assert): 5 min -- right before going live the heartbeat should be very fresh.
STALE_S_DEFAULT = 1200
PREFLIGHT_STALE_S_DEFAULT = 300

# Audio-presence floor (dB). At or above this the measurement audio is PRESENT; strictly below it the
# chain is silent -- mirrors scripts/lib/audio-presence-preflight.sh (digital silence ~-91 dB, a live
# QPSK marker ~-5 dB; -60 sits with wide margin between the two).
AUDIO_PRESENT_DB = -60.0

# #1331 offset ALERT arm. Page when the SyncNet-measured A/V offset is out of band during a LIVE
# stream. OFFSET_ALARM_MS_DEFAULT mirrors av_sync_measure.py's operator threshold (its
# `--threshold-ms default=30`, owner ruling 17.9.2026 / issue 1333 -- one A/V tolerance 30 ms
# everywhere); that value lives there ONLY as an argparse default, not an
# importable named constant, and importing av_sync_measure.py pulls torch/obs_phase2 (absent on
# dev1), so the value is cross-referenced here, never imported. OFFSET_CONF_FLOOR is the QUALITY
# gate the production-critical re-ping doctrine requires (watchdog-notify-dedup.md, the 16.9 storm
# caveat): a verdict below it is unreliable and MUST NOT page. It mirrors av_sync_measure.py's
# CONF_MIN=4.0 -- SyncNet's own usability floor below which a window has no reliable face/lip lock
# (the 2026-07-26 conf-3.6 garbage era falls under it); the healthy pinned-asset baseline reads ~8.
OFFSET_ALARM_MS_DEFAULT = 30
OFFSET_CONF_FLOOR = 4.0

_DB_RE = re.compile(r"\bdb=(-?\d+(?:\.\d+)?)\b")
# The exact verdict shape av_sync_measure.py prints (scripts/av_sync_measure.py:422):
#   f"[{stamp}] AV offset {offset_frames:+d} fr ({offset_ms:+d} ms) conf {conf:.1f} :: {verdict}"
# Capture the ms value (what the operator's 2ME PGM latency knob turns) and the SyncNet confidence.
_OFFSET_RE = re.compile(
    r"AV offset\s+[+-]?\d+\s+fr\s+\(\s*([+-]?\d+)\s*ms\s*\)\s+conf\s+(-?\d+(?:\.\d+)?)"
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
    all return False. Mirrors scripts/lib/avsync-heartbeat.sh's avsync_heartbeat_is_stale contract."""
    try:
        e = int(epoch)
        n = int(now)
        s = int(stale_s)
    except (TypeError, ValueError):
        return False
    age = n - e
    return 0 <= age <= s


def is_measured_heartbeat(status):
    """PURE: does the status start with the 'measured: ' prefix avsync-watchdog.ps1 writes when the
    grab SUCCEEDED and a measurement ran? A successful grab means the RTMP relay was serving, i.e.
    the stream is publishing -- so a fresh 'measured:' heartbeat is itself proof the stream is LIVE,
    independent of any OBS-WS read (the #3 robustness point)."""
    return bool(status) and str(status).strip().startswith("measured: ")


def is_no_signal_heartbeat(status):
    """PURE: does the status start with 'no-signal:' -- the grab FAILED (dead relay / stream down /
    stale clip, per the #814 freshness gate)? Distinct from a stale heartbeat (the process is still
    alive and writing, it just has nothing to measure)."""
    return bool(status) and str(status).strip().startswith("no-signal:")


def audio_db_from_status(status):
    """PURE: extract the `db=<float>` reading avsync-watchdog.ps1 prefixes onto a measured heartbeat.
    Returns the float, or None when absent/unreadable/malformed (an old heartbeat, a 'db=unreadable'
    volumedetect failure, or a no-signal line) -- None is treated as NOT PRESENT downstream
    (fail-CLOSED: never assume audio is present when we can't read its level)."""
    if not status:
        return None
    m = _DB_RE.search(str(status))
    if not m:
        return None
    try:
        return float(m.group(1))
    except (TypeError, ValueError):
        return None


def audio_present(status, floor_db=AUDIO_PRESENT_DB):
    """PURE: is the measurement audio present (db >= floor)? Fail-CLOSED: an unreadable/absent db is
    NOT present. Strict '<' for silence mirrors audio_preflight_is_silent (exactly at the floor =
    audible)."""
    db = audio_db_from_status(status)
    return db is not None and db >= floor_db


def status_is_wedged(status):
    """PURE: a 'measured: ... TIMEOUT: ...' status -- av_sync_measure.py was force-killed at 180s,
    the watchdog loop is wedged. INVALID reading."""
    return bool(status) and "timeout" in str(status).lower()


def status_is_healthy_measured(status):
    """PURE: does the heartbeat represent a VALID measurement reading -- the measurement line is
    producing a real, present reading? True iff it is a 'measured:' heartbeat (grab succeeded) AND
    not wedged (no TIMEOUT) AND the measurement AUDIO is present (db >= -60). A band-segment
    UNMEASURABLE with audio present is VALID (the instrument is alive; SyncNet just had no face to
    lock); silent audio (db < -60) is INVALID -- the dead-line case. Fail-CLOSED on an unreadable db.
    """
    if not is_measured_heartbeat(status):
        return False
    if status_is_wedged(status):
        return False
    return audio_present(status)


def stream_is_live(output_active):
    """PURE: normalize an outputActive fact to True / False / None(unknown). None (unreadable OBS-WS
    probe) is DISTINCT from False so the caller can treat 'OBS unreachable' differently from 'stream
    genuinely off'."""
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
    """PURE: GO iff ALL hold -- the measurement watchdog writes a FRESH heartbeat (process alive),
    the dev1 forwarder/alert timer is active, a REAL Discord test-ping was delivered (HTTP 200), the
    stream-state read WORKS (returns a definite True/False, not None -- so the run-time alarm's own
    stream gate can function; #3), AND, IF the stream is currently publishing (a 'measured:'
    heartbeat), the last reading is VALID (audio present). A 'no-signal:' heartbeat (stream not yet
    publishing at assert time) does NOT fail the audio check -- the audio chain can only be proven
    against a live stream, which is the run-time alarm's job. Returns (go, reasons[]) naming every
    failing check in plain Slovak."""
    reasons = []
    stale_s = facts.get("preflight_stale_s", PREFLIGHT_STALE_S_DEFAULT)
    status = facts.get("heartbeat_status", "")
    fwd = facts.get("forwarder_present")
    http = facts.get("discord_ping_http")
    live = stream_is_live(facts.get("stream_output_active"))

    if not heartbeat_fresh(facts.get("heartbeat_epoch"), facts.get("now"), stale_s):
        reasons.append(
            "meraci watchdog na stream boxe nepise cerstvy heartbeat "
            "(proces mrtvy alebo starsi nez {}s)".format(stale_s)
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
    if live is None:
        reasons.append(
            "stav streamu (outputActive) sa neda precitat cez OBS-WS -- run-time alarm by nevedel "
            "odlisit vypnuty stream od mrtvej linky; over host/heslo OBS WebSocket"
        )
    # audio-validity check only when the stream is actually publishing at assert time.
    if is_measured_heartbeat(status) and not status_is_healthy_measured(status):
        if status_is_wedged(status):
            reasons.append("merania su TIMEOUT -- watchdog na stream boxe je zaseknuty")
        else:
            reasons.append(
                "stream vysiela, ale meracia audio linka je TICHA (db {} < {} dB) -- oziv mbc "
                "retazec PRED eventom".format(
                    _fmt_db(audio_db_from_status(status)), int(AUDIO_PRESENT_DB)
                )
            )
    return (len(reasons) == 0, reasons)


# ---------------------------------------------------------------------------
# Decision surface 2 -- the run-time liveness alarm BOUND TO STREAM STATE.
# ---------------------------------------------------------------------------


def liveness_alarm(facts):
    """PURE: returns (action, reason, sig) with action in {"OK", "ALARM", "SUPPRESSED"} and `sig` a
    COARSE stamp-free signature the caller throttles on (never the volatile heartbeat text, #4).

      - fresh 'measured:' heartbeat (grab succeeded => stream is publishing, regardless of the WS
        read): audio present + not wedged -> OK; silent audio OR wedged -> ALARM. This is the bar --
        it catches the 2026-08-17 case (fresh heartbeat, silent audio) WITHOUT depending on the
        OBS-WS read, which #3 showed is easily mis-configured to fail.
      - otherwise (a 'no-signal:' heartbeat = grab failed, or a STALE heartbeat = process dead) the
        stream might be legitimately off, so gate on outputActive:
          * stream LIVE  -> ALARM (a dead watchdog / broken relay DURING a live event).
          * stream OFF   -> SUPPRESSED (box off / between events -- silence is expected, never page).
          * stream UNKNOWN (OBS-WS unreachable) -> SUPPRESSED (INCONCLUSIVE; OBS reachability is
            owned by the network-reach/obs-liveness watchdogs -- do not double-page).

    An ALARM reason never promises self-recovery -- it says intervention is needed (the honest half
    of the ticket's "either really restart, or write 'needs a hand'")."""
    fresh = heartbeat_fresh(facts.get("heartbeat_epoch"), facts.get("now"),
                            facts.get("stale_s", STALE_S_DEFAULT))
    status = facts.get("heartbeat_status", "")
    live = stream_is_live(facts.get("stream_output_active"))

    if fresh and is_measured_heartbeat(status):
        if status_is_wedged(status):
            return ("ALARM",
                    "stream VYSIELA (grab presiel), ale merania su TIMEOUT -- watchdog zaseknuty -- treba zasah",
                    "wedged")
        if not audio_present(status):
            return ("ALARM",
                    "stream VYSIELA (grab presiel), ale meracia audio linka je TICHA (db {} < {} dB) -- "
                    "mbc retazec je mrtvy -- treba zasah".format(
                        _fmt_db(audio_db_from_status(status)), int(AUDIO_PRESENT_DB)),
                    "no-audio")
        return ("OK", "stream vysiela a meracia linka dava platne meranie s pritomnym audiom", "ok")

    # not a fresh valid measured reading -> gate on stream state.
    if live is True:
        if not fresh:
            return ("ALARM",
                    "stream VYSIELA, ale meraci watchdog nepise cerstvy heartbeat > {}s "
                    "(proces mrtvy) -- treba zasah".format(facts.get("stale_s", STALE_S_DEFAULT)),
                    "stale")
        return ("ALARM",
                "stream VYSIELA, ale merania su NO-SIGNAL (grab/relay padol) -- treba zasah",
                "no-signal")
    if live is False:
        return ("SUPPRESSED", "stream nevysiela (outputActive=false) -- merat netreba, ticho je v poriadku", "off")
    return ("SUPPRESSED",
            "stav streamu sa neda precitat (OBS-WS nedostupny) -- INCONCLUSIVE; "
            "dosiahnutelnost/zivotnost OBS vlastnia network-reach/obs-liveness watchdogy",
            "unknown")


# ---------------------------------------------------------------------------
# Decision surface 3 -- the run-time OFFSET alarm (#1331): page when a CONFIDENT SyncNet A/V offset
# is out of band during a LIVE stream. Distinct from liveness_alarm (which asks "is the line ALIVE
# / audio present?"); this asks "is the live program actually IN SYNC?".
# ---------------------------------------------------------------------------


def parse_offset(status):
    """PURE: parse '(±M ms) conf X' out of a measured heartbeat -> (offset_ms:int, conf:float), or
    (None, None) when absent/garbled (an UNMEASURABLE band segment, a no-signal/TIMEOUT line, a
    corrupt status, empty/None). Reads the ms value av_sync_measure.py prints (offset_frames *
    FRAME_MS) directly, plus the SyncNet confidence. Fail-CLOSED: any parse failure yields
    (None, None) so the caller can never page on a garbled reading."""
    if not status:
        return (None, None)
    m = _OFFSET_RE.search(str(status))
    if not m:
        return (None, None)
    try:
        return (int(m.group(1)), float(m.group(2)))
    except (TypeError, ValueError):
        return (None, None)


def offset_advice_from_status(status):
    """PURE: the operator advice av_sync_measure.py appends after ' :: ' (e.g. "audio predbieha
    video o ~80 ms -> ZNIZ '2ME PGM' latency o 80"), or None when absent. Included verbatim in the
    ALARM reason so the phone alert carries the exact knob direction."""
    if not status:
        return None
    parts = str(status).split(" :: ", 1)
    if len(parts) != 2:
        return None
    advice = parts[1].strip()
    return advice or None


def offset_alarm(facts):
    """PURE: returns (action, reason, sig) with action in {"OK", "ALARM", "SUPPRESSED"} and `sig` a
    COARSE stamp-free signature the caller throttles on -- same contract as liveness_alarm.

    ALARM iff ALL hold: the stream is LIVE (outputActive True), the heartbeat is FRESH, it is a
    measured (grab succeeded) non-wedged reading carrying a PARSEABLE offset verdict, the SyncNet
    confidence is at/above the floor (the QUALITY gate), AND |offset| >= the threshold. Everything
    else is SUPPRESSED (never a false page), gated in this order so each SUPPRESSED reason is honest:
      - stream not LIVE (off, or unreadable=None) -> the offset arm evaluates only during a live
        stream; off/unknown is owned by the liveness arm's stream gate.
      - heartbeat STALE -> the reading is not current; staleness is the liveness arm's job.
      - not a measured/non-wedged heartbeat (no-signal / TIMEOUT) -> nothing to grade; liveness owns it.
      - no parseable offset (an UNMEASURABLE band/graphics window) -> nothing to rozladit.
      - LOW confidence (< floor) -> unreliable verdict (the 2026-07-26 conf-3.6 garbage) -> never page.
      - |offset| < threshold -> in band -> OK.
    Fail-CLOSED throughout: a missing/garbled fact yields SUPPRESSED, never ALARM."""
    threshold = facts.get("offset_alarm_ms", OFFSET_ALARM_MS_DEFAULT)
    try:
        threshold = abs(float(threshold))
    except (TypeError, ValueError):
        threshold = float(OFFSET_ALARM_MS_DEFAULT)
    conf_floor = facts.get("offset_conf_floor", OFFSET_CONF_FLOOR)
    try:
        conf_floor = float(conf_floor)
    except (TypeError, ValueError):
        conf_floor = OFFSET_CONF_FLOOR

    status = facts.get("heartbeat_status", "")
    live = stream_is_live(facts.get("stream_output_active"))
    fresh = heartbeat_fresh(facts.get("heartbeat_epoch"), facts.get("now"),
                            facts.get("stale_s", STALE_S_DEFAULT))

    if live is not True:
        return ("SUPPRESSED",
                "stream nevysiela alebo sa stav neda precitat -- offset alarm sa vyhodnocuje len "
                "pocas ZIVEHO streamu",
                "not-live")
    if not fresh:
        return ("SUPPRESSED",
                "heartbeat nie je cerstvy -- offset nie je aktualny (staleness riesi liveness arm)",
                "stale")
    if not is_measured_heartbeat(status) or status_is_wedged(status):
        return ("SUPPRESSED",
                "ziadne cerstve meranie (no-signal / TIMEOUT) -- offset arm nema co hodnotit "
                "(riesi liveness arm)",
                "not-measured")
    # A silent measured heartbeat is the liveness arm's no-audio case -- the offset arm DEFERS so the
    # two arms stay strictly mutually exclusive (a confident SyncNet offset against digital silence is
    # self-contradictory anyway: SyncNet emits UNMEASURABLE with no face/lip lock there, which
    # parse_offset already rejects below -- this gate closes the synthetic silent+verdict double-page).
    if not audio_present(status):
        return ("SUPPRESSED",
                "meracia audio linka je TICHA -- rozladenie neposudzujem, no-audio riesi liveness arm",
                "silent")
    offset_ms, conf = parse_offset(status)
    if offset_ms is None or conf is None:
        return ("SUPPRESSED",
                "okno bez offset verdiktu (band/graphics segment) -- nie je co rozladit",
                "no-verdict")
    if conf < conf_floor:
        return ("SUPPRESSED",
                "meranie ma nizku spolahlivost (conf {:g} < {:g}) -- verdikt nie je doveryhodny".format(
                    conf, conf_floor),
                "low-conf")
    if abs(offset_ms) < threshold:
        return ("OK",
                "A/V je v tolerancii (offset {:+d} ms < {:g} ms, conf {:g})".format(
                    offset_ms, threshold, conf),
                "ok")
    advice = offset_advice_from_status(status)
    reason = "A/V je ROZLADENE pocas ZIVEHO streamu: offset {:+d} ms (conf {:g}, prah {:g} ms)".format(
        offset_ms, conf, threshold)
    if advice:
        reason += " -- " + advice
    return ("ALARM", reason, "offset")


def _fmt_db(db):
    return "<necitatelne>" if db is None else "{:g}".format(db)


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

    p_off = sub.add_parser("offset", help="run-time A/V offset alarm bound to stream state (#1331)")
    p_off.add_argument("--facts", required=True, help="path to the gathered-facts JSON")

    a = ap.parse_args(argv)
    facts = _load_facts(a.facts)

    if a.mode == "preflight":
        go, reasons = preflight_verdict(facts)
        if go:
            print("GO")
            return EXIT_GO
        print("NO-GO: " + " | ".join(reasons))
        return EXIT_NO_GO

    if a.mode == "offset":
        action, reason, sig = offset_alarm(facts)
    else:
        action, reason, sig = liveness_alarm(facts)
    print("action={} reason={} sig={}".format(action, reason, sig))
    return EXIT_ALARM if action == "ALARM" else EXIT_GO


if __name__ == "__main__":
    sys.exit(main())
