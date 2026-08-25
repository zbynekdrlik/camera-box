"""#1199 -- PURE decision core for the strih on-box NIC-fail self-heal watcher.

This module is the SINGLE SOURCE OF TRUTH for the self-heal ladder's state machine.
It is deliberately Tier-0 / dev1-CI testable (plain python, no Windows, no network):
`scripts/strih-nic-selfheal.ps1` MIRRORS this exact logic in PowerShell on the box
(there is no pwsh runtime on dev1 CI, so the ps1 is validated STATICALLY while this
mirror carries the RED->GREEN behavioural tests) -- the same "pure core + thin
platform glue + static-anchor mirror" pattern this repo already uses for
`scripts/avsync_lineup.py` <-> `avsync-watchdog.ps1`.

OWNER RULING (2026-08-25, verbatim): "uz si nejaky restart eth karty riesil a
neuspesne a ja ked vo windows dam ze sa ta karta ma disablovat a enablovat tak sa to
sekne a aj tak musim robit shutdown ... hlavne sa nemotaj vo veciach ktore si uz
skusal!!!!" -- On strih a NIC disable/enable HANGS (the owner tried it by hand; a
past session's adapter-restart also failed). So there is NO Restart-NetAdapter rung:
the ONLY self-heal action is a graceful reboot. The ladder is a single step.

WHY reachability, not adapter status (the load-bearing design decision, issue 1199):
on 2026-08-24 the strih NIC dropped packets while the box stayed alive -- `Get-NetAdapter`
almost certainly still read `Up`. So a status=="Down" trigger would have MISSED the exact
incident this exists to catch. The pass is classified purely on whether MULTIPLE LAN
targets are reachable; adapter status is READ for the log only (never a trigger, never
touched).

Fail-safe direction (issue 1199): a pass is `dead` ONLY when EVERY probed target returns
a CLEAN negative (no probe threw). Any reachable target is `alive` (resets everything);
any probe error with nothing reachable, or nothing probed at all, is `unknown` -- and
UNKNOWN never advances the ladder and never resets it (fail toward inaction).

The ladder (constants below, ~2 min cadence):
  armed     --N dead (~10 min)--> graceful reboot (best-effort OBS StopStream/StopRecord
                                  then `shutdown /r`), reboots+=1, stays `armed`
  (reboot cap MAX_REBOOTS): once reboots==MAX_REBOOTS and the reboot point is reached
                            again, phase=exhausted -- stop rebooting, keep loud logging;
                            the real fix is the physical card replacement.
  alive at any point resets phase=armed, consecutive_dead=0, reboots=0.
"""

# --- ladder constants (the ps1 mirror MUST carry the identical values; the static test
# in tests/python/test_strih_nic_selfheal_1199.py asserts the ps1 lines match these) -----
DEAD_PASSES_BEFORE_REBOOT = 5    # ~10 min of confirmed all-targets-dead before a graceful reboot
MAX_REBOOTS = 2                  # hard cap on self-heal reboots before giving up (physical fix needed)
PROBE_INTERVAL_MIN = 2           # informational: schtasks repetition cadence

PHASES = ("armed", "exhausted")
ACTIONS = ("none", "reboot", "give_up")


def classify_pass(reachable, clean, threw):
    """Classify ONE probe pass -> 'alive' | 'dead' | 'unknown'.

    - reachable >= 1 (any target answered) -> 'alive' (the NIC is clearly working).
    - else any probe threw (threw > 0) -> 'unknown' (a broken probe can never PROVE a
      total outage -- fail toward inaction; #1199 review W1).
    - else clean >= 1 (every probed target returned a CLEAN negative) -> 'dead'.
    - else (nothing probed) -> 'unknown'.

    `reachable`/`clean`/`threw` are per-pass counts: reachable = targets that answered,
    clean = targets that returned a definite reachable/unreachable answer, threw = targets
    whose probe raised. reachable is a subset of clean.
    """
    try:
        reachable = int(reachable)
        clean = int(clean)
        threw = int(threw)
    except (TypeError, ValueError):
        return "unknown"
    if reachable >= 1:
        return "alive"
    if threw >= 1:
        return "unknown"
    if clean >= 1:
        return "dead"
    return "unknown"


def initial_state():
    """The state carried across passes in the JSON state file (decision-relevant keys only)."""
    return {"phase": "armed", "consecutive_dead": 0, "reboots": 0}


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
    `new_state` is a fresh dict (never mutates the input). Any invalid phase is normalized to
    `armed` (the least-aggressive state) by `_norm_state`.
    """
    s = _norm_state(state)
    phase = s["phase"]
    cd = s["consecutive_dead"]
    rb = s["reboots"]

    if pass_class == "unknown":
        # fail-safe / fail toward inaction: never advance, never reset on an unprovable pass.
        return "none", dict(s), "unknown pass (probe error or nothing probed) -> fail-safe inaction"

    if pass_class == "alive":
        return "none", {"phase": "armed", "consecutive_dead": 0, "reboots": 0}, "alive (a target answered) -> reset"

    # pass_class == "dead"
    cd += 1

    if phase == "exhausted":
        # reboot cap already spent -- never reboot again; just keep counting + logging loudly.
        return "none", {"phase": "exhausted", "consecutive_dead": cd, "reboots": rb}, \
            "dead %d (exhausted, awaiting physical card replacement)" % cd

    # phase == "armed" (also any normalized/unknown phase)
    if cd >= DEAD_PASSES_BEFORE_REBOOT:
        if rb < MAX_REBOOTS:
            return ("reboot",
                    {"phase": "armed", "consecutive_dead": 0, "reboots": rb + 1},
                    "%d confirmed dead passes -> graceful reboot (%d/%d)" % (cd, rb + 1, MAX_REBOOTS))
        return ("give_up",
                {"phase": "exhausted", "consecutive_dead": 0, "reboots": rb},
                "reboot cap %d reached -> stop rebooting, keep alerting (physical card fix needed)" % MAX_REBOOTS)
    return "none", {"phase": "armed", "consecutive_dead": cd, "reboots": rb}, \
        "dead %d/%d (arming window)" % (cd, DEAD_PASSES_BEFORE_REBOOT)
