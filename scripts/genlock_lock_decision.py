#!/usr/bin/env python3
"""#1299 -- PURE decision core for the dev1 genlock-lock alert watchdog.

WHY: the in-OBS genlock LOCK indicator (#1298) is visible ONLY to an operator standing at each
box's statusbar. The fleet (dev1) had no way to SEE whether strih/stream/imag/resolume are
LIVE-LOCKED to the fleet clock, and no page fired when a box silently left LOCKED -- the exact
silent-degradation class the dev1 alert-watchdog family (#732/#1001/#1226) exists to close.

The producer (the #1298 statusbar widget) already scalarises the THREE genlock producers
(per-source FIFO counters, the NDI output's wall-stamping flag, the dantesync :8898 clock facet)
into ONE verdict at the one place they meet, and emits it as a versioned `genlock-lock-json:` line;
bundle_state_gather exposes that as the nested `genlock_lock` facet on `:8899/bundle-state.json`.
This module is the pure kernel of the dev1 watchdog that reads that facet from each box and decides
when to page. No I/O, no ssh, no OBS, no MCP -- exhaustively unit-testable (pytest), the
strih-nic-selfheal #1199 / ndi-halving #1203 python-mirror precedent, so the decision RED->GREENs
LOCALLY under Tier-0 (#557 kills cargo). The orchestrator scripts/genlock-lock-alert-watchdog.sh
curls the JSON, calls `analyze` here, and drives obs-watchdog-decision.sh's confirm/throttle +
airuleset notify (--dedup-key genlock-lock-$box, #1206).

`decide()` is a byte-faithful Python MIRROR of src/genlock_lock_state.rs's `decide` (the Rust
authority, itself C-vs-Rust parity-gated against GenlockLockState.hpp). The watchdog trusts the
`state` string the widget already decided and carries in the facet; `decide()` exists so the
test suite can feed the SAME counters the Rust/C parity gate uses and assert the three-state
precedence is understood identically here -- the facet can never disagree with the statusbar.

Verdicts (classify):
  SKIP     -- box could not be fetched (:8899 down / box down). That page is #732 (bundle-state) /
              #1001 (network-reach) territory, never this watchdog's -- so paging requires a
              successfully fetched facet, and a dev1-side outage can only produce SKIP.
  UNKNOWN  -- box fetched OK but the genlock_lock facet is absent (a stock OBS, or no
              genlock-lock-json: line in the tail yet) -- NEVER a false UNLOCKED.
  HEALTHY  -- state == LOCKED.
  DEGRADED -- state == DEGRADED. The watchdog pages after a 2-pass confirm.
  UNLOCKED -- state == UNLOCKED. The watchdog pages after a 2-pass confirm.
"""
import argparse
import json
import sys

# State strings -- match src/genlock_lock_state.rs LockState + the C genlock_state_name().
ST_LOCKED = "LOCKED"
ST_DEGRADED = "DEGRADED"
ST_UNLOCKED = "UNLOCKED"

# Reason tokens -- match the C genlock_reason_key() + src/genlock_lock_state.rs LockReason.
R_NONE = "none"
R_NO_GENLOCK = "no_genlock"
R_CLOCK = "clock"
R_OUTPUT = "output"
R_NO_INPUT_LOCKED = "no_input_locked"
R_INPUT_UNLOCKED = "input_unlocked"
R_RECENT_EVENT = "recent_event"
R_NTP_FAILED = "ntp_failed"
R_QPC_DRIFT = "qpc_drift"


def decide(n_inputs, n_locked, recent_event, qpc_drift_beyond_bound, clock_present,
           clock_locked, clock_ntp_failed, output_present, output_stamping):
    """Pure three-state decision -- a byte-faithful mirror of src/genlock_lock_state.rs `decide`.

    UNLOCKED precedence: clock (absent/unlocked) > output (present but not stamping) >
    no-input-locked. DEGRADED precedence (only when no UNLOCKED condition holds): some-input-
    unlocked > recent-event > ntp-failed > qpc-drift. Otherwise LOCKED. Returns (state, reason).
    """
    # --- UNLOCKED (red): clock > output > no-input-locked ---------------------------
    if not clock_present or not clock_locked:
        return (ST_UNLOCKED, R_CLOCK)
    if output_present and not output_stamping:
        return (ST_UNLOCKED, R_OUTPUT)
    if n_locked == 0:
        reason = R_NO_GENLOCK if n_inputs == 0 else R_NO_INPUT_LOCKED
        return (ST_UNLOCKED, reason)

    # --- DEGRADED (amber): some-unlocked > recent-event > ntp > qpc ------------------
    if n_locked < n_inputs:
        return (ST_DEGRADED, R_INPUT_UNLOCKED)
    if recent_event:
        return (ST_DEGRADED, R_RECENT_EVENT)
    if clock_ntp_failed:
        return (ST_DEGRADED, R_NTP_FAILED)
    if qpc_drift_beyond_bound:
        return (ST_DEGRADED, R_QPC_DRIFT)

    # --- LOCKED (green) -------------------------------------------------------------
    return (ST_LOCKED, R_NONE)


def facet_from_obj(obj):
    """Return the nested `genlock_lock` facet dict from a parsed bundle-state object, or None when
    it is absent / not a dict (a stock OBS, or a box whose log has no genlock-lock-json: line yet).
    None means UNKNOWN downstream -- NEVER a fabricated UNLOCKED."""
    if not isinstance(obj, dict):
        return None
    facet = obj.get("genlock_lock")
    return facet if isinstance(facet, dict) else None


def _loads_obj(text):
    """Parse *text* as a JSON object; None on any failure (the ndi_halving #1203 precedent: a
    strict parse that raised got swallowed by the caller's 2>/dev/null and read as SKIP forever)."""
    if not (text or "").strip():
        return None
    try:
        obj = json.loads(text)
    except (ValueError, TypeError):
        return None
    return obj if isinstance(obj, dict) else None


def classify(state, box_reachable):
    """One box's verdict from the facet's carried `state` string.

      box_reachable != 1   -> SKIP     (defer to #732/#1001; never our page)
      state is None        -> UNKNOWN  (facet absent; no reading to judge -- never a false UNLOCKED)
      state == LOCKED      -> HEALTHY
      state == DEGRADED    -> DEGRADED
      state == UNLOCKED    -> UNLOCKED
      any other string     -> UNKNOWN  (fail-safe: an unrecognised state is never paged)
    """
    if box_reachable != 1:
        return "SKIP"
    if state is None:
        return "UNKNOWN"
    if state == ST_LOCKED:
        return "HEALTHY"
    if state == ST_DEGRADED:
        return "DEGRADED"
    if state == ST_UNLOCKED:
        return "UNLOCKED"
    return "UNKNOWN"


def analyze(bundle_json_text, box_reachable):
    """Fetch-result -> `{verdict, state, reason, n_inputs, n_locked}`. SKIP without parsing when the
    box was not reachable this pass; UNKNOWN (state None) when the facet is absent."""
    if box_reachable != 1:
        return {"verdict": "SKIP", "state": None, "reason": None,
                "n_inputs": None, "n_locked": None}
    facet = facet_from_obj(_loads_obj(bundle_json_text))
    if facet is None:
        return {"verdict": "UNKNOWN", "state": None, "reason": None,
                "n_inputs": None, "n_locked": None}
    state = facet.get("state")
    reason = facet.get("reason")
    n_inputs = facet.get("n_inputs")
    n_locked = facet.get("n_locked")
    return {"verdict": classify(state, box_reachable), "state": state, "reason": reason,
            "n_inputs": n_inputs, "n_locked": n_locked}


def _fmt(v):
    """key=value rendering for the shell: None -> empty string (the UNKNOWN/absent contract)."""
    return "" if v is None else str(v)


def _main(argv):
    ap = argparse.ArgumentParser(description="pure genlock-lock watchdog decisions (#1299)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    a = sub.add_parser("analyze",
                       help="read /bundle-state.json on stdin -> verdict + state + reason + inputs")
    a.add_argument("--box-reachable", type=int, required=True)

    ns = ap.parse_args(argv)

    if ns.cmd == "analyze":
        # Well-formed UTF-8 JSON, but read bytes + tolerant-decode anyway (the ndi_halving #1203
        # hotfix precedent: a strict read that raised got swallowed by 2>/dev/null -> SKIP forever).
        # box_reachable=0 needs no stdin.
        text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
        res = analyze(text, ns.box_reachable)
        for k, key in (("verdict", "verdict"), ("state", "state"), ("reason", "reason"),
                       ("n_inputs", "n_inputs"), ("n_locked", "n_locked")):
            print(f"{k}={_fmt(res[key])}")
        return 0

    return 2


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
