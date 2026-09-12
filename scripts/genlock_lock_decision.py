#!/usr/bin/env python3
"""#1299 -- PURE decision core for the dev1 genlock-lock alert watchdog (STUB -- RED commit).

This is the deliberately-incomplete RED stub: the tests in
tests/python/test_genlock_lock_decision_1299.py are committed alongside it and FAIL against it,
proving they exercise real behaviour before the GREEN implementation lands in the next commit
(regression-test-first RED->GREEN, the ci-testing-gotchas "genuine pre-fix RED" pattern).
"""

ST_LOCKED = "LOCKED"
ST_DEGRADED = "DEGRADED"
ST_UNLOCKED = "UNLOCKED"

R_NONE = "none"
R_NO_GENLOCK = "no_genlock"
R_CLOCK = "clock"
R_OUTPUT = "output"
R_NO_INPUT_LOCKED = "no_input_locked"
R_INPUT_UNLOCKED = "input_unlocked"
R_RECENT_EVENT = "recent_event"
R_NTP_FAILED = "ntp_failed"
R_QPC_DRIFT = "qpc_drift"


def decide(*args, **kwargs):
    raise NotImplementedError("genlock-lock decide() -- GREEN commit implements this")


def classify(state, box_reachable):
    raise NotImplementedError("genlock-lock classify() -- GREEN commit implements this")


def analyze(bundle_json_text, box_reachable):
    raise NotImplementedError("genlock-lock analyze() -- GREEN commit implements this")
