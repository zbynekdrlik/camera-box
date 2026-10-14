#!/usr/bin/env python3
"""issue 1426 -- PURE decision core for the dev1 fleet build-parity watchdog.

WHY: on 13.10.2026 the owner found the OBS genlock builds and the dantesync versions split across
the two venues again ("... ze sa ti znova rozisli verzie dantesync a obs ... a ze to nic
nezachytilo"). The only build-parity check was the E2E `[0/8]` row over the SNV run set, so a PP
box, or a traveling box that came back on an old build, was never compared with anything.

The orchestrator scripts/fleet-parity-alert-watchdog.sh reads every OBS box of the obs-fleet
`build-parity` facet (both venues) and feeds one JSON list here:

    [{"box": "strih-snv", "state": "present", "sha": "fd25d1d30..."},
     {"box": "strih-pp", "state": "away"}, ...]

`state` is one of:
  present    -- :8899 answered with a `genlock_build_sha`
  nofacet    -- :8899 answered, but carries no `genlock_build_sha`
  unreadable -- the box is home (its OBS-WS answers) but :8899 did not answer
  away       -- a traveling box that is not home (PP powered off is NORMAL, never a fault)

`analyze` prints key=value lines:
  verdict=OK|DRIFT|INCOMPLETE|UNKNOWN
    OK         -- every present box runs the same build and no home box is unreadable
    DRIFT      -- two or more builds among the present boxes (pages after the confirm)
    INCOMPLETE -- one build, but a home box could not be read (logged, never a page: the
                  bundle-state / network-reach watchdogs own a dead :8899)
    UNKNOWN    -- fewer than one present box (nothing to compare)
  modal=<sha>       -- the build most present boxes run (ties: the lexically smallest, stated)
  outliers=<box>:<sha12>,...  -- present boxes on another build
  blind=<box>(<state>),...    -- home boxes whose build could not be read
  away=<box>,...
No I/O; pytest Tier-0 (tests/python/test_fleet_parity_1426.py).
"""
import json
import sys
from collections import Counter

STATES = ("present", "nofacet", "unreadable", "away")


def classify(entries):
    present = [e for e in entries if e.get("state") == "present" and e.get("sha")]
    blind = [e for e in entries if e.get("state") in ("nofacet", "unreadable")
             or (e.get("state") == "present" and not e.get("sha"))]
    away = [e["box"] for e in entries if e.get("state") == "away"]
    out = {"present": len(present), "away": ",".join(away),
           "blind": ",".join("%s(%s)" % (e["box"], e.get("state")) for e in blind)}
    if not present:
        out.update(verdict="UNKNOWN", modal="", outliers="", tie="0")
        return out
    counts = Counter(e["sha"] for e in present)
    top = max(counts.values())
    modal_candidates = sorted(s for s, n in counts.items() if n == top)
    modal = modal_candidates[0]
    outliers = [e for e in present if e["sha"] != modal]
    out["modal"] = modal
    out["tie"] = "1" if len(modal_candidates) > 1 else "0"
    out["outliers"] = ",".join("%s:%s" % (e["box"], e["sha"][:12]) for e in outliers)
    if outliers:
        out["verdict"] = "DRIFT"
    elif blind:
        out["verdict"] = "INCOMPLETE"
    else:
        out["verdict"] = "OK"
    return out


def main(argv):
    if argv[1:] != ["analyze"]:
        print("usage: fleet_build_parity_decision.py analyze < entries.json", file=sys.stderr)
        return 2
    entries = json.load(sys.stdin)
    if not isinstance(entries, list):
        print("fleet_build_parity_decision: the input must be a JSON list", file=sys.stderr)
        return 2
    for e in entries:
        if not isinstance(e, dict) or not e.get("box") or e.get("state") not in STATES:
            print("fleet_build_parity_decision: bad entry %r" % (e,), file=sys.stderr)
            return 2
    for k, v in classify(entries).items():
        print("%s=%s" % (k, v))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
