"""#1299 -- tests for the PURE decision core of the dev1 genlock-lock alert watchdog
(`scripts/genlock_lock_decision.py`).

Layer 1 (this file, RED->GREEN, local + CI): the pure decision -- the three-state `decide()` mirror
of src/genlock_lock_state.rs (fed the SAME precedence table the Rust/C parity gate uses, so the
facet can never disagree with the statusbar), plus `analyze()` classifying SKIP/UNKNOWN/HEALTHY/
DEGRADED/UNLOCKED from the nested `genlock_lock` bundle-state facet. No I/O, no ssh, no OBS -- the
strih-nic-selfheal #1199 / ndi-halving #1203 python-mirror precedent, so it RED->GREENs LOCALLY
under Tier-0 #557 (cargo, even --no-run, cannot run; the family `tests/harness_*.rs` are CI-only).

The bash orchestrator's confirm/throttle/home-gate GLUE has its own CI-only harness
(`tests/harness_genlock_lock_watchdog_1299.rs`); this file owns the pure matrix + the parity table.
"""

import json
import pathlib
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import genlock_lock_decision as d


# A fully healthy box: clock locked, every input locked, output stamping. Mirrors the
# `healthy()` fixture in src/genlock_lock_state.rs's own tests.
def healthy():
    return dict(n_inputs=7, n_locked=7, recent_event=False, qpc_drift_beyond_bound=False,
                clock_present=True, clock_locked=True, clock_ntp_failed=False,
                output_present=True, output_stamping=True)


# ------------------------------------------------------------------------------------------------
# decide() -- the same precedence cases src/genlock_lock_state.rs pins, so this Python mirror and
# the Rust/C authority agree on every three-state verdict (the "facet must agree with the statusbar"
# parity the #1299 ticket requires -- both are fed the SAME counters here).
# ------------------------------------------------------------------------------------------------
def test_all_good_is_locked():
    assert d.decide(**healthy()) == (d.ST_LOCKED, d.R_NONE)


def test_receiver_box_with_no_output_is_still_locked():
    f = healthy()
    f["output_present"] = False
    f["output_stamping"] = False
    assert d.decide(**f) == (d.ST_LOCKED, d.R_NONE)


def test_clock_absent_is_unlocked_clock():
    f = healthy()
    f["clock_present"] = False
    f["clock_locked"] = False
    assert d.decide(**f) == (d.ST_UNLOCKED, d.R_CLOCK)


def test_clock_present_but_not_locked_is_unlocked_clock():
    f = healthy()
    f["clock_locked"] = False
    assert d.decide(**f) == (d.ST_UNLOCKED, d.R_CLOCK)


def test_output_present_not_stamping_is_unlocked_output():
    f = healthy()
    f["output_stamping"] = False
    assert d.decide(**f) == (d.ST_UNLOCKED, d.R_OUTPUT)


def test_inputs_exist_none_locked_is_unlocked_no_input():
    f = healthy()
    f["n_locked"] = 0
    assert d.decide(**f) == (d.ST_UNLOCKED, d.R_NO_INPUT_LOCKED)


def test_no_inputs_at_all_is_unlocked_no_genlock():
    f = healthy()
    f["n_inputs"] = 0
    f["n_locked"] = 0
    assert d.decide(**f) == (d.ST_UNLOCKED, d.R_NO_GENLOCK)


def test_some_input_unlocked_is_degraded_input():
    f = healthy()
    f["n_locked"] = 5
    assert d.decide(**f) == (d.ST_DEGRADED, d.R_INPUT_UNLOCKED)


def test_recent_event_is_degraded_recent():
    f = healthy()
    f["recent_event"] = True
    assert d.decide(**f) == (d.ST_DEGRADED, d.R_RECENT_EVENT)


def test_ntp_failed_is_degraded_ntp():
    f = healthy()
    f["clock_ntp_failed"] = True
    assert d.decide(**f) == (d.ST_DEGRADED, d.R_NTP_FAILED)


def test_qpc_drift_is_degraded_qpc():
    f = healthy()
    f["qpc_drift_beyond_bound"] = True
    assert d.decide(**f) == (d.ST_DEGRADED, d.R_QPC_DRIFT)


# ---- precedence (mirrors src/genlock_lock_state.rs's precedence tests) --------------------------
def test_clock_beats_output_and_input():
    f = healthy()
    f["clock_locked"] = False
    f["output_stamping"] = False
    f["n_locked"] = 0
    assert d.decide(**f) == (d.ST_UNLOCKED, d.R_CLOCK)


def test_output_beats_no_input_locked():
    f = healthy()
    f["output_stamping"] = False
    f["n_locked"] = 0
    assert d.decide(**f) == (d.ST_UNLOCKED, d.R_OUTPUT)


def test_input_unlocked_beats_recent_event_ntp_and_qpc():
    f = healthy()
    f["n_locked"] = 6
    f["recent_event"] = True
    f["clock_ntp_failed"] = True
    f["qpc_drift_beyond_bound"] = True
    assert d.decide(**f) == (d.ST_DEGRADED, d.R_INPUT_UNLOCKED)


def test_recent_event_beats_ntp_and_qpc():
    f = healthy()
    f["recent_event"] = True
    f["clock_ntp_failed"] = True
    f["qpc_drift_beyond_bound"] = True
    assert d.decide(**f) == (d.ST_DEGRADED, d.R_RECENT_EVENT)


def test_ntp_beats_qpc():
    f = healthy()
    f["clock_ntp_failed"] = True
    f["qpc_drift_beyond_bound"] = True
    assert d.decide(**f) == (d.ST_DEGRADED, d.R_NTP_FAILED)


# ------------------------------------------------------------------------------------------------
# classify() / analyze() -- the watchdog verdict from the facet's carried `state`.
# ------------------------------------------------------------------------------------------------
def _bundle(state, reason="none", n_inputs=7, n_locked=7, extra=None):
    facet = {"state": state, "reason": reason, "n_inputs": n_inputs, "n_locked": n_locked,
             "source": "log"}
    if extra:
        facet.update(extra)
    return json.dumps({"obs_version": "32.1.2", "genlock_lock": facet})


def test_classify_skip_when_unreachable():
    assert d.classify(d.ST_LOCKED, box_reachable=0) == "SKIP"
    # unreachable short-circuits before any state read
    assert d.classify(None, box_reachable=0) == "SKIP"


def test_classify_unknown_when_state_none():
    assert d.classify(None, box_reachable=1) == "UNKNOWN"


def test_classify_unknown_on_unrecognised_state_failsafe():
    assert d.classify("WOBBLY", box_reachable=1) == "UNKNOWN"


def test_classify_maps_each_state():
    assert d.classify(d.ST_LOCKED, 1) == "HEALTHY"
    assert d.classify(d.ST_DEGRADED, 1) == "DEGRADED"
    assert d.classify(d.ST_UNLOCKED, 1) == "UNLOCKED"


def test_analyze_skip_needs_no_body():
    r = d.analyze("", box_reachable=0)
    assert r["verdict"] == "SKIP" and r["state"] is None


def test_analyze_locked_is_healthy():
    r = d.analyze(_bundle("LOCKED", "none"), box_reachable=1)
    assert r["verdict"] == "HEALTHY"
    assert r["state"] == "LOCKED" and r["reason"] == "none"
    assert r["n_inputs"] == 7 and r["n_locked"] == 7


def test_analyze_unlocked_clock_is_unlocked():
    r = d.analyze(_bundle("UNLOCKED", "clock", n_locked=0), box_reachable=1)
    assert r["verdict"] == "UNLOCKED"
    assert r["state"] == "UNLOCKED" and r["reason"] == "clock"


def test_analyze_degraded_is_degraded():
    r = d.analyze(_bundle("DEGRADED", "input_unlocked", n_locked=6), box_reachable=1)
    assert r["verdict"] == "DEGRADED" and r["reason"] == "input_unlocked"


def test_analyze_unknown_when_facet_absent_never_false_unlocked():
    # a stock OBS / no genlock-lock-json: line -> the facet key is absent -> UNKNOWN, NOT UNLOCKED.
    r = d.analyze(json.dumps({"obs_version": "32.1.2"}), box_reachable=1)
    assert r["verdict"] == "UNKNOWN" and r["state"] is None


def test_analyze_unknown_on_garbage_json():
    r = d.analyze("not json at all {{{", box_reachable=1)
    assert r["verdict"] == "UNKNOWN"


def test_analyze_unknown_when_facet_not_a_dict():
    # a malformed facet (string, list) must read UNKNOWN, never crash / never a false UNLOCKED.
    assert d.analyze(json.dumps({"genlock_lock": "oops"}), box_reachable=1)["verdict"] == "UNKNOWN"
    assert d.analyze(json.dumps({"genlock_lock": [1, 2]}), box_reachable=1)["verdict"] == "UNKNOWN"
