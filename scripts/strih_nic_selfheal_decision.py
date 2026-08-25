"""#1199 -- PURE decision core for the strih on-box NIC-fail self-heal watcher.

This module is the SINGLE SOURCE OF TRUTH for the self-heal ladder's state machine.
It is deliberately Tier-0 / dev1-CI testable (plain python, no Windows, no network):
`scripts/strih-nic-selfheal.ps1` MIRRORS this exact logic in PowerShell on the box
(there is no pwsh runtime on dev1 CI, so the ps1 is validated STATICALLY while this
mirror carries the RED->GREEN behavioural tests) -- the same "pure core + thin
platform glue + static-anchor mirror" pattern this repo already uses for
`scripts/avsync_lineup.py` <-> `avsync-watchdog.ps1`.

WHY reachability, not adapter status (the load-bearing design decision, issue 1199):
on 2026-08-24 the strih NIC dropped packets while the box stayed alive -- `Get-NetAdapter`
almost certainly still read `Up`. So a status=="Down" trigger would have MISSED the exact
incident this exists to catch. The pass is classified purely on whether MULTIPLE LAN
targets are reachable; adapter status is advisory/log only (and used to pick which adapter
to restart), never the trigger.

Fail-safe direction (issue 1199): any probe error, or nothing probed at all, is UNKNOWN --
NEVER `dead`. UNKNOWN never advances the ladder and never resets it (fail toward inaction).
Any single reachable target is `alive` and resets every counter.

The ladder (constants below, ~2 min cadence):
  normal    --5 dead (~10 min)--> Restart-NetAdapter, phase=restarted
  restarted --5 dead (~10 min)--> graceful reboot (best-effort OBS StopStream/StopRecord
                                  then `shutdown /r`), phase=rebooted, reboots+=1
  rebooted  --5 dead----------->  re-arm with a cheap adapter restart, phase=restarted
  (reboot cap MAX_REBOOTS): once reboots==MAX_REBOOTS and the reboot point is reached again,
                            phase=exhausted -- stop rebooting, keep cheap adapter restarts +
                            loud logging; the real fix is the physical card replacement.
  alive at any point resets phase=normal, counters=0, reboots=0.
"""

# --- ladder constants (the ps1 mirror MUST carry the identical values; the static test
# in tests/python/test_strih_nic_selfheal_1199.py asserts the ps1 lines match these) -----
DEAD_PASSES_BEFORE_RESTART = 5   # ~10 min of confirmed all-targets-dead before Restart-NetAdapter
DEAD_PASSES_BEFORE_REBOOT = 5    # ~10 min more, still dead after the restart, before a reboot
MAX_REBOOTS = 2                  # hard cap on self-heal reboots before giving up (physical fix needed)
PROBE_INTERVAL_MIN = 2           # informational: schtasks repetition cadence

PHASES = ("normal", "restarted", "rebooted", "exhausted")
ACTIONS = ("none", "restart_adapter", "reboot", "give_up")


def classify_pass(reachable, total, probe_error):
    """Classify ONE probe pass -> 'alive' | 'dead' | 'unknown'.

    - probe_error (the probe mechanism itself threw / could not run) -> 'unknown'.
    - total <= 0 (no targets were even attempted) -> 'unknown' -- cannot conclude.
    - reachable >= 1 (any target answered) -> 'alive'.
    - reachable == 0 with total > 0 (clean negatives from every target) -> 'dead'.

    Fail-safe: everything that is not a CLEAN all-dead result is 'unknown', never 'dead'.
    """
    if probe_error:
        return "unknown"
    try:
        reachable = int(reachable)
        total = int(total)
    except (TypeError, ValueError):
        return "unknown"
    if total <= 0:
        return "unknown"
    if reachable >= 1:
        return "alive"
    if reachable <= 0:
        return "dead"
    return "unknown"


def initial_state():
    """The state carried across passes in the JSON state file (decision-relevant keys only)."""
    return {"phase": "normal", "consecutive_dead": 0, "reboots": 0}


def _norm_state(state):
    s = dict(initial_state())
    if isinstance(state, dict):
        phase = state.get("phase")
        if phase in PHASES:
            s["phase"] = phase
        try:
            s["consecutive_dead"] = max(0, int(state.get("consecutive_dead", 0)))
        except (TypeError, ValueError):
            s["consecutive_dead"] = 0
        try:
            s["reboots"] = max(0, int(state.get("reboots", 0)))
        except (TypeError, ValueError):
            s["reboots"] = 0
    return s


def decide(state, pass_class):
    """Pure transition. Returns (action, new_state, reason).

    `state` is the prior JSON state (decision keys); `pass_class` is classify_pass()'s output.
    `new_state` is a fresh dict (never mutates the input).
    """
    s = _norm_state(state)
    phase = s["phase"]
    cd = s["consecutive_dead"]
    rb = s["reboots"]

    if pass_class == "unknown":
        # fail-safe / fail toward inaction: never advance, never reset on an unprovable pass.
        return "none", dict(s), "unknown pass (probe error or nothing probed) -> fail-safe inaction"

    if pass_class == "alive":
        return "none", {"phase": "normal", "consecutive_dead": 0, "reboots": 0}, "alive (a target answered) -> reset"

    # pass_class == "dead"
    cd += 1

    if phase == "normal":
        if cd >= DEAD_PASSES_BEFORE_RESTART:
            return ("restart_adapter",
                    {"phase": "restarted", "consecutive_dead": 0, "reboots": rb},
                    "%d confirmed dead passes -> Restart-NetAdapter" % cd)
        return "none", {"phase": "normal", "consecutive_dead": cd, "reboots": rb}, \
            "dead %d/%d (normal window)" % (cd, DEAD_PASSES_BEFORE_RESTART)

    if phase == "restarted":
        if cd >= DEAD_PASSES_BEFORE_REBOOT:
            if rb < MAX_REBOOTS:
                return ("reboot",
                        {"phase": "rebooted", "consecutive_dead": 0, "reboots": rb + 1},
                        "still dead after Restart-NetAdapter -> graceful reboot (%d/%d)" % (rb + 1, MAX_REBOOTS))
            return ("give_up",
                    {"phase": "exhausted", "consecutive_dead": 0, "reboots": rb},
                    "reboot cap %d reached -> stop rebooting, keep alerting (physical card fix needed)" % MAX_REBOOTS)
        return "none", {"phase": "restarted", "consecutive_dead": cd, "reboots": rb}, \
            "dead %d/%d (confirming after restart)" % (cd, DEAD_PASSES_BEFORE_REBOOT)

    if phase == "rebooted":
        if cd >= DEAD_PASSES_BEFORE_RESTART:
            return ("restart_adapter",
                    {"phase": "restarted", "consecutive_dead": 0, "reboots": rb},
                    "still dead after reboot -> re-arm with a cheap adapter restart")
        return "none", {"phase": "rebooted", "consecutive_dead": cd, "reboots": rb}, \
            "dead %d/%d (post-reboot window)" % (cd, DEAD_PASSES_BEFORE_RESTART)

    # phase == "exhausted": cheap adapter restarts only, never reboot again.
    if cd >= DEAD_PASSES_BEFORE_RESTART:
        return ("restart_adapter",
                {"phase": "exhausted", "consecutive_dead": 0, "reboots": rb},
                "exhausted -> cheap adapter restart only, never reboot again")
    return "none", {"phase": "exhausted", "consecutive_dead": cd, "reboots": rb}, \
        "dead %d/%d (exhausted, awaiting physical card replacement)" % (cd, DEAD_PASSES_BEFORE_RESTART)
