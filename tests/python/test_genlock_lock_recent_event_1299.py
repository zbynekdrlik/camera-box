"""#1299 Part 3 — the `recent_event` false-page fix + offender attribution, on the PYTHON consumer
side (the C++/Rust producer + pure-rule side is covered by the module unit tests + the parity gate).

Two consumer-side contracts this asserts, both RED against the pre-Part-3 tree:

  (a) `bundle_state_gather.genlock_lock_facet_from_log` must surface the NEW `recent_event_inputs`
      attribution list from a v3 `genlock-lock-json:` line (omit-when-absent: a v2 line, or an empty
      list, carries no key — never a fabricated attribution).

  (b) `genlock_lock_decision.analyze` must ENRICH the reason to `recent_event:<name>` when the facet
      carries `recent_event_inputs`, so the watchdog log line + Discord body name the offending input
      (`reason=recent_event:cg`) and a genuine page is actionable. Absent/empty -> the bare
      `recent_event` token (never `recent_event:` with an empty name).
"""
import pathlib
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402
import genlock_lock_decision as d  # noqa: E402


# ---- (a) gather parser surfaces recent_event_inputs (v3), omit-when-absent -----------------------

# A v3 DEGRADED line: cg is the top recent-event offender (relocks on a CONNECTED input); the
# machine-readable attribution rides the line so the fleet watchdog can name it.
_V3_WITH_OFFENDER = (
    '10:47:06.003: genlock-lock-json: {"v":3,"state":"DEGRADED","reason":"recent_event",'
    '"n_inputs":4,"n_locked":4,"n_absent":0,"latency_ms":3,"clock":"locked","output":"stamping",'
    '"recent_event":true,"recent_event_inputs":[{"name":"cg","events":25}],"qpc_drift_ms":0,'
    '"inputs":[{"name":"cg","locked":true,"connected":true,"latency_ms":3,"underruns":543,'
    '"relocks":25,"late_holds":0,"depth":2}]} (#1299)\n'
)

# A v3 LOCKED line with an EMPTY recent_event_inputs (no offender) — the parser must OMIT the key.
_V3_EMPTY = (
    '10:48:06.003: genlock-lock-json: {"v":3,"state":"LOCKED","reason":"none","n_inputs":4,'
    '"n_locked":4,"n_absent":0,"latency_ms":3,"clock":"locked","output":"stamping",'
    '"recent_event":false,"recent_event_inputs":[],"qpc_drift_ms":0,"inputs":[]} (#1299)\n'
)

# A v2 line from the currently-deployed build (no recent_event_inputs key at all).
_V2_NO_KEY = (
    '10:49:06.003: genlock-lock-json: {"v":2,"state":"DEGRADED","reason":"recent_event","n_inputs":4,'
    '"n_locked":4,"n_absent":0,"latency_ms":3,"clock":"locked","output":"stamping",'
    '"recent_event":true,"qpc_drift_ms":0,"inputs":[]} (#1299)\n'
)


def test_v3_line_carries_recent_event_inputs():
    f = bsg.genlock_lock_facet_from_log(_V3_WITH_OFFENDER)
    assert f is not None
    assert f["recent_event"] is True
    assert f["recent_event_inputs"] == [{"name": "cg", "events": 25}]


def test_v3_empty_offender_list_omits_the_key():
    f = bsg.genlock_lock_facet_from_log(_V3_EMPTY)
    assert f is not None
    assert "recent_event_inputs" not in f


def test_v2_line_without_key_omits_it():
    f = bsg.genlock_lock_facet_from_log(_V2_NO_KEY)
    assert f is not None
    assert "recent_event_inputs" not in f


# ---- (b) decision analyze enriches the reason with the offender name -----------------------------


def _bundle_facet(recent_event_inputs=None, reason="recent_event"):
    facet = {
        "state": "DEGRADED",
        "reason": reason,
        "n_inputs": 4,
        "n_locked": 4,
        "n_absent": 0,
    }
    if recent_event_inputs is not None:
        facet["recent_event_inputs"] = recent_event_inputs
    import json
    return json.dumps({"genlock_lock": facet})


def test_analyze_enriches_reason_with_offender():
    r = d.analyze(_bundle_facet([{"name": "cg", "events": 25}]), box_reachable=1)
    assert r["verdict"] == "DEGRADED"
    assert r["reason"] == "recent_event:cg"


def test_analyze_bare_recent_event_when_no_offender():
    # No recent_event_inputs at all (a v2 line) -> the bare token, never `recent_event:`.
    r = d.analyze(_bundle_facet(None), box_reachable=1)
    assert r["reason"] == "recent_event"


def test_analyze_bare_recent_event_when_offender_list_empty():
    r = d.analyze(_bundle_facet([]), box_reachable=1)
    assert r["reason"] == "recent_event"


def test_analyze_ignores_offender_for_non_recent_event_reason():
    # A malformed line that carried recent_event_inputs on a different reason must not corrupt it.
    r = d.analyze(_bundle_facet([{"name": "cg", "events": 1}], reason="input_unlocked"),
                  box_reachable=1)
    assert r["reason"] == "input_unlocked"
