#!/usr/bin/env python3
"""#1307 -- PURE decision core for the dev1 dante-clock alert watchdog.

WHY: the camboxes cam1-7 are HEADLESS (no desktop, no operator) -- their only path to a human is
dev1 -> Discord. On 2026-09-13 the Yamaha AIC128-D PTP grandmaster's DHCP lease moved off
10.77.9.184, every node's gm_allowlist stopped matching, the WHOLE fleet silently ran NTP-only for
hours (mode=ACQ is_locked=false; strih as NTP master stepping 165x/h) and nobody noticed until the
release E2E gate failed (#1307 / #1297). The dev1 alert-watchdog family (#732/#1001/#1226/#1299)
exists to close exactly this silent-background-degradation class, but had no member for the dante
clock. This module is the pure kernel of that missing watchdog: it reads each node's :8898/status
and decides when to page. No I/O, no ssh, no curl -- exhaustively unit-testable (pytest, Tier-0,
#557 kills local cargo), the strih-nic-selfheal #1199 / ndi-halving #1203 / genlock-lock #1299
python-mirror precedent. The orchestrator scripts/dantesync-clock-alert-watchdog.sh curls the JSON,
calls analyze() here, and drives obs-watchdog-decision.sh's 2-pass confirm + airuleset notify with a
TIME-BUCKETED --dedup-key computed here.

GRADING MIRROR: the field semantics are a faithful mirror of scripts/clock-offset-guard.sh's
ptp_locked_from_pipe_json (is_locked + mode in {NANO,LOCK}), gm_matches_expected (gm_source_ip ==
the rig grandmaster), and ntp_master_step_storm_verdict (the ntp_step_storm boolean) -- the SAME
:8898/status fields the E2E gate grades, so this watchdog can never disagree with the gate about
whether a node has the clock. This is NOT a diverging copy of the grading: the decision MUST be pure
Python to be Tier-0 testable (the bash extractors cannot be pytest-driven), the :8898/status schema
is a stable dantesync contract, and the field names here are identical so a schema drift is caught in
review.

PRODUCTION-CRITICAL RE-PING (owner ruling ROZHODNUTÉ #1307, 2026-09-13, verbatim: „...nech kazdu
minutu chodia notifikacie ze nemaju dante clock ... byt o tom dokolecka notifikovany"): while a
NO_CLOCK / DNS_UNRESOLVABLE / GM_CHANGED / storm condition PERSISTS, Discord must be re-pinged
REPEATEDLY, not once per incident. Mechanism (no new notify channel): keep airuleset's
`notify --dedup-key`, but bucket the key by time -- dante-clock-<box>-<floor(now/interval)>. Within
one bucket an identical state EDITS the card (no ping); every new bucket is a fresh ping. Recovery
stays ONE machine-channel log line, never a phone ping. dedup_key() lives here (deterministic `now`
injected) so the cadence is unit-tested.

Per-node verdicts (analyze):
  SKIP     -- box_reachable != 1 (status fetch failed / box down). Defers to the #1001 network-reach
              watchdog; an OFF box reads UNREACHABLE and must NEVER page.
  UNKNOWN  -- reachable but the body is unparseable, or carries NONE of is_locked/mode/ntp_step_storm/
              clock_alarm -- no reading to judge, NEVER a fabricated NO_CLOCK (never a false page).
  OK       -- is_locked true AND mode in {NANO,LOCK} AND (gm_source_ip == the grandmaster, or gm
              absent while otherwise locked) AND no ntp_step_storm AND clock_alarm not active.
  NO_CLOCK -- reachable + readable but any of: not locked (not_locked) / gm_source_ip present and
              != the grandmaster (wrong_gm) / ntp_step_storm true (storm) / clock_alarm.active true.
"""
import argparse
import json
import sys

# --- verdicts ---------------------------------------------------------------------------------
V_SKIP = "SKIP"
V_UNKNOWN = "UNKNOWN"
V_OK = "OK"
V_NO_CLOCK = "NO_CLOCK"

# --- reason classes ---------------------------------------------------------------------------
R_NONE = "none"
R_NOT_LOCKED = "not_locked"
R_WRONG_GM = "wrong_gm"
R_STORM = "storm"
R_STALE = "stale"
R_CLOCK_ALARM = "clock_alarm"

# updated_ts freshness (mirror clock-offset-guard.sh pipe_json_freshness_verdict, #550/#591/#595): a
# reachable-but-STALE :8898/status (HTTP thread alive, servo/updated_ts frozen) is a silent clock loss
# the E2E gate already fails on, so this watchdog pages it too. Default 300s (the gate's
# DANTESYNC_OFFSET_FRESHNESS_S default) -- generous over the ~30s updated_ts cadence, so only a genuine
# freeze trips it.
FRESHNESS_DEFAULT_S = 300

# The :8898/status modes that count as PTP-locked (mirror clock-offset-guard.sh ptp_locked_from_pipe_json).
MODES_LOCKED = ("NANO", "LOCK")

# dantesync#114 forward-compat: the per-box clock alarm object. A SINGLE constant (the ticket's
# "make the field name a single constant") so a rename is one edit here.
CLOCK_ALARM_FIELD = "clock_alarm"

# Time-bucketed re-ping cadence (owner ruling #1307). Default 600s (10 min); floored at 60s so a
# mis-set interval can never become a per-pass phone flood.
REPING_INTERVAL_DEFAULT_S = 600
REPING_INTERVAL_FLOOR_S = 60


def _loads_obj(text):
    """Parse *text* as a JSON object; None on any failure (the ndi_halving #1203 precedent: a strict
    parse that raised got swallowed by the caller's 2>/dev/null and read as SKIP forever)."""
    if not (text or "").strip():
        return None
    try:
        obj = json.loads(text)
    except (ValueError, TypeError):
        return None
    return obj if isinstance(obj, dict) else None


def _is_stale(updated_ts, now, freshness_s):
    """Mirror clock-offset-guard.sh pipe_json_freshness_verdict:
      None  -- cannot judge (updated_ts absent, or now/freshness not usable ints) -> never a stale
               page (false-page-safe; falls through to the lock/gm/storm checks)
      True  -- |now - updated_ts| exceeds freshness_s (a frozen HTTP payload)
      False -- within freshness_s (fresh)
    """
    if updated_ts is None or now is None:
        return None
    try:
        delta = abs(int(now) - int(updated_ts))
        fresh = int(freshness_s) if freshness_s is not None else FRESHNESS_DEFAULT_S
    except (ValueError, TypeError):
        return None
    if fresh < 0:
        return None
    return delta > fresh


def _ptp_locked(is_locked, mode):
    """Mirror clock-offset-guard.sh ptp_locked_from_pipe_json:
      None   -- neither is_locked nor mode readable (UNKNOWN; nothing to judge)
      True   -- is_locked is true AND mode in {NANO, LOCK}
      False  -- otherwise (DEGRADED / not locked)
    """
    if is_locked is None and mode is None:
        return None
    return is_locked is True and mode in MODES_LOCKED


def analyze(status_json_text, box_reachable, grandmaster_ip, now=None, freshness_s=None,
            clock_alarm_field=CLOCK_ALARM_FIELD):
    """One node's verdict from its :8898/status body.

    `box_reachable` is 1 iff the orchestrator fetched a 200 JSON body this pass. `grandmaster_ip` is
    the rig grandmaster resolved from video-clock.lan (empty => the gm comparison is skipped, so gm
    never triggers a page -- the DNS_UNRESOLVABLE page is fired by the orchestrator instead). `now`
    (epoch s) + `freshness_s` grade the payload's `updated_ts` age (mirror the E2E gate): a STALE
    reading is NO_CLOCK. `now` omitted (None) => freshness is not graded (the 3-arg call)."""
    base = {"verdict": V_SKIP, "reason": None, "is_locked": None, "mode": None,
            "gm_source_ip": None, "ntp_step_storm": None, "ntp_steps_last_hour": None}
    if box_reachable != 1:
        return base

    obj = _loads_obj(status_json_text)
    if obj is None:
        return {**base, "verdict": V_UNKNOWN}

    is_locked = obj.get("is_locked")
    mode = obj.get("mode")
    gm = obj.get("gm_source_ip")
    storm = obj.get("ntp_step_storm")
    steps = obj.get("ntp_steps_last_hour")
    alarm = obj.get(clock_alarm_field)
    stale = _is_stale(obj.get("updated_ts"), now, freshness_s)

    alarm_active = None
    alarm_reason = None
    if isinstance(alarm, dict):
        alarm_active = alarm.get("active")
        alarm_reason = alarm.get("reason")

    out = {
        "verdict": V_OK, "reason": R_NONE,
        "is_locked": None if is_locked is None else ("true" if is_locked else "false"),
        "mode": mode,
        "gm_source_ip": gm or None,
        "ntp_step_storm": None if storm is None else ("true" if storm else "false"),
        "ntp_steps_last_hour": None if steps is None else str(steps),
    }

    ptp = _ptp_locked(is_locked, mode)

    # Nothing readable at all -> UNKNOWN, never a fabricated NO_CLOCK (a stock/partial payload must
    # never false-page). A STALE reading, storm, or alarm each count as a judgeable signal, so a
    # frozen daemon serving a skeleton payload still pages rather than reading UNKNOWN.
    if ptp is None and storm is None and alarm_active is None and stale is not True:
        return {**out, "verdict": V_UNKNOWN, "reason": None}

    # --- derived cross-check (the E2E gate's field semantics) --------------------------------
    reasons = []
    if stale is True:
        # A frozen payload is untrustworthy for every other field -- stale leads the reason.
        reasons.append(R_STALE)
    if storm is True:
        reasons.append(R_STORM)
    if ptp is False:
        reasons.append(R_NOT_LOCKED)
    # gm identity: page ONLY on a PRESENT-and-different gm (the #834 foreign-master case). A gm that
    # is simply absent while the node is otherwise locked is report-first OK (mirrors the gate's
    # DANTESYNC_GATE_GM_ENFORCE=0 default) -- the false-page-safe direction, and a genuinely lost
    # clock is is_locked=false, never gm-absent-but-locked.
    if grandmaster_ip and gm and gm != grandmaster_ip:
        reasons.append(R_WRONG_GM)

    # --- clock_alarm (dantesync#114) is AUTHORITATIVE for NO_CLOCK, derived check folds in as the
    # cross-check: page if EITHER the box says alarm active OR the derived check found a fault. --
    if alarm_active is True:
        return {**out, "verdict": V_NO_CLOCK, "reason": R_CLOCK_ALARM,
                "alarm_reason": alarm_reason}
    if reasons:
        return {**out, "verdict": V_NO_CLOCK, "reason": ",".join(reasons)}

    return out


def reping_interval(interval_s):
    """The effective re-ping bucket size in seconds: the given value, clamped to >= the 60s floor;
    a non-numeric value falls back to the 600s default (never a crash, never a per-pass flood)."""
    try:
        iv = int(interval_s)
    except (ValueError, TypeError):
        return REPING_INTERVAL_DEFAULT_S
    return iv if iv >= REPING_INTERVAL_FLOOR_S else REPING_INTERVAL_FLOOR_S


def dedup_key(base, now, interval_s):
    """The TIME-BUCKETED airuleset --dedup-key (owner ruling #1307): base-<floor(now/interval)>.

    Same state within one interval -> same key (airuleset edits the card, no re-ping); the next
    interval -> a new key (a fresh ping while the fault persists). `now` is injected so the cadence
    is deterministic + unit-tested."""
    iv = reping_interval(interval_s)
    try:
        bucket = int(now) // iv
    except (ValueError, TypeError):
        bucket = 0
    return f"{base}-{bucket}"


def grandmaster_change(prev_ip, cur_ip):
    """True iff the resolved grandmaster IP MOVED between passes (both non-empty and different) --
    exactly today's „ip sa zmenila". A first-ever pass (no persisted prior) is never a change."""
    return bool(prev_ip) and bool(cur_ip) and prev_ip != cur_ip


def _fmt(v):
    return "" if v is None else str(v)


def _main(argv):
    ap = argparse.ArgumentParser(description="pure dante-clock watchdog decisions (#1307)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    a = sub.add_parser("analyze", help="read :8898/status on stdin -> verdict + reason + fields")
    a.add_argument("--box-reachable", type=int, required=True)
    a.add_argument("--grandmaster-ip", default="")
    a.add_argument("--now", type=int, default=None, help="epoch s for updated_ts freshness (omit = skip)")
    a.add_argument("--freshness-s", type=int, default=FRESHNESS_DEFAULT_S)

    d = sub.add_parser("dedup-key", help="time-bucketed --dedup-key for a base + now + interval")
    d.add_argument("--base", required=True)
    d.add_argument("--now", type=int, required=True)
    d.add_argument("--interval", default=str(REPING_INTERVAL_DEFAULT_S))

    g = sub.add_parser("gm-change", help="did the resolved grandmaster IP move between passes?")
    g.add_argument("--prev", default="")
    g.add_argument("--cur", default="")

    ns = ap.parse_args(argv)

    if ns.cmd == "analyze":
        text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
        res = analyze(text, ns.box_reachable, ns.grandmaster_ip, now=ns.now, freshness_s=ns.freshness_s)
        for k in ("verdict", "reason", "is_locked", "mode", "gm_source_ip",
                  "ntp_step_storm", "ntp_steps_last_hour"):
            print(f"{k}={_fmt(res.get(k))}")
        print(f"alarm_reason={_fmt(res.get('alarm_reason'))}")
        return 0

    if ns.cmd == "dedup-key":
        print(dedup_key(ns.base, ns.now, ns.interval))
        return 0

    if ns.cmd == "gm-change":
        print("1" if grandmaster_change(ns.prev, ns.cur) else "0")
        return 0

    return 2


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
