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
import os
import sys

# The re-ping cadence + key builder live in the ONE shared scripts/watchdog_reping.py twin (#1308) --
# this module delegates to it so there is a SINGLE implementation across every production-critical
# watchdog, never a second copy. The path insert makes `import watchdog_reping` resolve whether this
# file is run as a script (sys.path[0] = scripts/) OR exec'd via importlib in pytest (scripts/ absent
# from sys.path) -- __file__ is the real path in both cases.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import watchdog_reping as _reping  # noqa: E402

# --- verdicts ---------------------------------------------------------------------------------
V_SKIP = "SKIP"
V_UNKNOWN = "UNKNOWN"
V_OK = "OK"
V_NO_CLOCK = "NO_CLOCK"
# #1308: box is UP (a cheap TCP probe answered) but :8898 (dantesync HTTP) is dead -- the daemon is
# down/wedged on a live box, a production-critical page. Distinct from SKIP (box genuinely down ->
# defer #1001) and from NO_CLOCK (daemon answering but not locked).
V_NO_DANTESYNC = "NO_DANTESYNC"
# #1309: the box's :8898 answered (dantesync HTTP alive) but its ssh MANAGEMENT banner is dead (a
# kex reset / read timeout: anything that needs a fork -- an sshd session, the remoteos MCP, a
# gphoto2 spawn -- fails while already-running processes keep answering). This is the 2026-09-13 P0
# wedge: the box is UNMANAGEABLE while it still looks alive. Orthogonal to the clock axis, so it can
# co-occur with OK/NO_CLOCK -- and it takes PRECEDENCE (a box you cannot ssh into or repair is the
# P0; the clock reading, even OK, is moot until the box is reachable again).
V_MGMT_DEAD = "MGMT_DEAD"

# --- reason classes ---------------------------------------------------------------------------
R_NONE = "none"
R_NOT_LOCKED = "not_locked"
R_WRONG_GM = "wrong_gm"
R_STORM = "storm"
R_STALE = "stale"
R_CLOCK_ALARM = "clock_alarm"
R_NO_HTTP = "no_dantesync_http"  # #1308: box up, :8898 unreachable
R_SSH_DEAD = "ssh_banner_dead"  # #1309: :8898 up, ssh management banner dead (the 13.9. wedge)

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
# mis-set interval can never become a per-pass phone flood. Re-exported from the shared twin (#1308)
# so this module keeps the names while there is ONE source of truth.
REPING_INTERVAL_DEFAULT_S = _reping.REPING_INTERVAL_DEFAULT_S
REPING_INTERVAL_FLOOR_S = _reping.REPING_INTERVAL_FLOOR_S


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
            clock_alarm_field=CLOCK_ALARM_FIELD, box_up=None, version_pin=None, mgmt_ssh_ok=None):
    """One node's verdict, with the #1309 MGMT_DEAD axis folded over the clock verdict.

    Delegates the clock/HTTP grading to `_analyze_clock` (unchanged), then applies ONE orthogonal
    override: `mgmt_ssh_ok` (the dev1 ssh-banner probe result -- 1 banner read, 0 reset/timeout,
    None not probed) is consulted ONLY when the box's :8898 answered this pass (box_reachable == 1).
    A :8898-reachable box whose ssh MANAGEMENT banner is dead (mgmt_ssh_ok == 0) is MGMT_DEAD -- the
    2026-09-13 P0 wedge, an unmanageable-but-alive box -- and this takes PRECEDENCE over the clock
    verdict (carried on as `clock_verdict` for the alert body). mgmt_ssh_ok == None (the default, and
    every pre-#1309 caller) never overrides, so all prior behaviour + tests are byte-for-byte intact.
    """
    res = _analyze_clock(status_json_text, box_reachable, grandmaster_ip, now=now,
                         freshness_s=freshness_s, clock_alarm_field=clock_alarm_field,
                         box_up=box_up, version_pin=version_pin)
    if box_reachable == 1 and mgmt_ssh_ok == 0:
        return {**res, "verdict": V_MGMT_DEAD, "reason": R_SSH_DEAD,
                "clock_verdict": res.get("verdict")}
    return res


def analyze_local(status_json_text, box_reachable, grandmaster_ip, now=None, freshness_s=None,
                  clock_alarm_field=CLOCK_ALARM_FIELD, version_pin=None):
    """#1313 -- the dev1 CONTROL box's verdict. dev1 hosts this watchdog and is itself a dantesync
    node whose clock feeds every dev1-hosted gate, yet it is NOT a probed cam/obs node -- so on
    14.9.2026 it sat NTP-only for ~a day unpaged (its gm_allowlist on the retired literal + a fleet
    roll that skipped it). A `local` node is probed at 127.0.0.1:8898 with NO ssh/TCP reach probe.
    Two policy differences from a remote node, both asserted HERE (ONE tested source of truth,
    never re-encoded in bash):

      * box is UP by definition -- the watchdog runs ON it -- so a dead :8898 is NO_DANTESYNC (the
        daemon crashed/wedged on a live box), never SKIP (there is no "box down, defer #1001" case
        for the box we are running on). Forced via box_up=1.
      * no ssh MANAGEMENT axis (we ARE the box) -- mgmt_ssh_ok is never probed, so MGMT_DEAD can
        never fire for the local node. Forced via mgmt_ssh_ok=None.

    Everything else (OK / NO_CLOCK / UNKNOWN / gm / storm / stale / version reporting) is the SAME
    generic grading, so a local node can never disagree with a remote node about what a lost clock
    is."""
    return analyze(status_json_text, box_reachable, grandmaster_ip, now=now, freshness_s=freshness_s,
                   clock_alarm_field=clock_alarm_field, box_up=1, version_pin=version_pin,
                   mgmt_ssh_ok=None)


def _analyze_clock(status_json_text, box_reachable, grandmaster_ip, now=None, freshness_s=None,
                   clock_alarm_field=CLOCK_ALARM_FIELD, box_up=None, version_pin=None):
    """One node's CLOCK/HTTP verdict from its :8898/status body (the #1307/#1308 core; #1309 wraps
    this with the MGMT_DEAD override in `analyze`).

    `box_reachable` is 1 iff the orchestrator fetched a 200 JSON body this pass. `grandmaster_ip` is
    the rig grandmaster resolved from video-clock.lan (empty => the gm comparison is skipped, so gm
    never triggers a page -- the DNS_UNRESOLVABLE page is fired by the orchestrator instead). `now`
    (epoch s) + `freshness_s` grade the payload's `updated_ts` age (mirror the E2E gate): a STALE
    reading is NO_CLOCK. `now` omitted (None) => freshness is not graded (the 3-arg call).

    `box_up` (#1308) is consulted ONLY when :8898 is unreachable, to discriminate a live box whose
    dantesync HTTP is dead (box_up == 1 -> NO_DANTESYNC, production-critical page) from a box that is
    genuinely down (box_up 0/None -> SKIP, defer to the #1001 network-reach watchdog; None = up-ness
    not probed / probe errored, the false-page-safe direction). `version_pin` (#1308), when set,
    populates a REPORT-ONLY `version_note` if the daemon's `version` field differs from it -- a version
    mismatch is surfaced in the card/log text, never a page on its own (a stale-but-locked node still
    has the clock); an absent `version` field is silent."""
    base = {"verdict": V_SKIP, "reason": None, "is_locked": None, "mode": None,
            "gm_source_ip": None, "ntp_step_storm": None, "ntp_steps_last_hour": None,
            "version": None, "version_note": None}
    if box_reachable != 1:
        # :8898 not fetchable this pass. Discriminate a live box with dead dantesync HTTP from a box
        # that is simply down. Only a PROVEN-up box (box_up == 1) pages NO_DANTESYNC; a down box, or an
        # unprobed/errored up-ness (None), stays SKIP -- never a false page.
        if box_up == 1:
            return {**base, "verdict": V_NO_DANTESYNC, "reason": R_NO_HTTP}
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

    # Version reporting (#1308): report-only, never a page. Absent field or no pin => silent.
    version = obj.get("version")
    version_note = None
    if version_pin and version is not None and str(version) != str(version_pin):
        version_note = f"version={version} (pin {version_pin})"

    out = {
        "verdict": V_OK, "reason": R_NONE,
        "is_locked": None if is_locked is None else ("true" if is_locked else "false"),
        "mode": mode,
        "gm_source_ip": gm or None,
        "ntp_step_storm": None if storm is None else ("true" if storm else "false"),
        "ntp_steps_last_hour": None if steps is None else str(steps),
        "version": None if version is None else str(version),
        "version_note": version_note,
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


# reping_interval / dedup_key delegate to the shared scripts/watchdog_reping.py twin (#1308) -- NOT
# a second implementation. `reping_interval` IS the shared function object (so a `is` identity test
# pins the delegation); `dedup_key` is the shared `notify_key` under its historical #1307 name.
reping_interval = _reping.reping_interval
dedup_key = _reping.notify_key


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
    a.add_argument("--box-up", type=int, default=None,
                   help="#1308: 1/0 box up-ness when :8898 unreachable (omit = not probed -> SKIP)")
    a.add_argument("--version-pin", default=None,
                   help="#1308: expected dantesync version; a mismatch is reported, never a page")
    a.add_argument("--mgmt-ssh-ok", type=int, default=None,
                   help="#1309: 1/0 ssh management-banner probe result (omit = not probed). A "
                        "reachable :8898 + a dead banner (0) -> MGMT_DEAD (the 13.9. wedge)")
    a.add_argument("--local", type=int, default=0,
                   help="#1313: 1 = the dev1 CONTROL box (analyze_local). Forces box_up=1 (a dead "
                        ":8898 -> NO_DANTESYNC, never SKIP) + no ssh axis (never MGMT_DEAD), "
                        "ignoring --box-up / --mgmt-ssh-ok. Default 0 = a remote node, unchanged.")

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
        if ns.local == 1:
            # #1313: the dev1 CONTROL box -- box_up=1 (dead :8898 -> NO_DANTESYNC) + no ssh axis,
            # ignoring --box-up / --mgmt-ssh-ok (analyze_local is the ONE tested local policy point).
            res = analyze_local(text, ns.box_reachable, ns.grandmaster_ip, now=ns.now,
                                freshness_s=ns.freshness_s, version_pin=ns.version_pin)
        else:
            res = analyze(text, ns.box_reachable, ns.grandmaster_ip, now=ns.now, freshness_s=ns.freshness_s,
                          box_up=ns.box_up, version_pin=ns.version_pin, mgmt_ssh_ok=ns.mgmt_ssh_ok)
        # Stable key ORDER (existing keys first, new #1308 keys appended) so the orchestrator's
        # `sed -n 's/^KEY=//p'` reads keep working and a new key is purely additive.
        for k in ("verdict", "reason", "is_locked", "mode", "gm_source_ip",
                  "ntp_step_storm", "ntp_steps_last_hour"):
            print(f"{k}={_fmt(res.get(k))}")
        print(f"alarm_reason={_fmt(res.get('alarm_reason'))}")
        print(f"version={_fmt(res.get('version'))}")
        print(f"version_note={_fmt(res.get('version_note'))}")
        print(f"clock_verdict={_fmt(res.get('clock_verdict'))}")  # #1309: the underlying clock verdict when MGMT_DEAD overrode it
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
