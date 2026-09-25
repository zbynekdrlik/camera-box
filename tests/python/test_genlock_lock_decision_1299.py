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


# ================================================================================================
# #1299 REOPEN — an absent-sender input (no live NDI receiver connection) must NEVER grade the box
# DEGRADED. The Python `decide()` mirror gains an `n_absent` param; the input decisions judge only
# CONNECTED inputs (n_connected = n_inputs - n_absent). Mirrors the src/genlock_lock_state.rs tests
# and the C parity gate's new n_absent axis. These FAIL on the pre-#1299 decide() (no n_absent kwarg
# -> TypeError) and pass once the connected-term lands.
# ================================================================================================
def test_absent_sender_only_unlocked_is_still_locked():
    # The reopen scenario (stream 'NDIA cg stream'): 4 inputs, 3 connected+locked, 1 senderless.
    # n_connected=3 == n_locked -> nothing CONNECTED is unlocked -> LOCKED, never a false DEGRADED.
    f = healthy()
    f["n_inputs"] = 4
    f["n_locked"] = 3
    assert d.decide(n_absent=1, **f) == (d.ST_LOCKED, d.R_NONE)


def test_all_senders_absent_is_healthy_idle_locked():
    # Every input present but senderless -> HEALTHY-idle (nothing to lock onto), NOT UNLOCKED.
    f = healthy()
    f["n_inputs"] = 4
    f["n_locked"] = 0
    assert d.decide(n_absent=4, **f) == (d.ST_LOCKED, d.R_NONE)


def test_absent_plus_a_connected_unlocked_still_degrades():
    # 4 inputs: 1 absent, 3 connected of which only 2 locked -> a CONNECTED input is genuinely
    # unlocked -> DEGRADED. A real fault still pages; the absent one is merely excluded.
    f = healthy()
    f["n_inputs"] = 4
    f["n_locked"] = 2
    assert d.decide(n_absent=1, **f) == (d.ST_DEGRADED, d.R_INPUT_UNLOCKED)


def test_connected_senders_none_locking_is_unlocked_no_input():
    # Live senders present (n_connected>0) but none locking -> a genuine fault, UNLOCKED.
    f = healthy()
    f["n_inputs"] = 3
    f["n_locked"] = 0
    assert d.decide(n_absent=1, **f) == (d.ST_UNLOCKED, d.R_NO_INPUT_LOCKED)


def test_no_genlock_inputs_at_all_stays_unlocked_no_genlock():
    # n_inputs==0 (no genlock configured) is a real misconfiguration -> UNLOCKED, distinct from the
    # inputs-present-but-all-senderless HEALTHY-idle case above.
    f = healthy()
    f["n_inputs"] = 0
    f["n_locked"] = 0
    assert d.decide(n_absent=0, **f) == (d.ST_UNLOCKED, d.R_NO_GENLOCK)


def test_absent_default_zero_reproduces_pre_1299_verdict():
    # n_absent defaults 0 (a pre-#1299 caller) -> the old behaviour: 5 of 7 -> DEGRADED.
    f = healthy()
    f["n_locked"] = 5
    assert d.decide(**f) == (d.ST_DEGRADED, d.R_INPUT_UNLOCKED)


def test_analyze_carries_n_absent_from_a_v2_facet():
    # analyze() surfaces n_absent for the watchdog's observability log (the widget already decided
    # `state`, so this never changes the verdict).
    body = json.dumps({"genlock_lock": {"state": "LOCKED", "reason": "none",
                                        "n_inputs": 4, "n_locked": 3, "n_absent": 1}})
    res = d.analyze(body, box_reachable=1)
    assert res["verdict"] == "HEALTHY"
    assert res["n_absent"] == 1


def test_analyze_n_absent_none_for_a_v1_facet():
    # A v1 facet (no n_absent) -> analyze surfaces None (the pre-#1299 all-connected reading).
    body = json.dumps({"genlock_lock": {"state": "LOCKED", "reason": "none",
                                        "n_inputs": 7, "n_locked": 7}})
    res = d.analyze(body, box_reachable=1)
    assert res["n_absent"] is None


# ================================================================================================
# #1341 — a CONNECTED-but-IDLE input (a keep-alive-only SongPlayer playlist input) must NEVER grade
# the box DEGRADED. The Python `decide()` mirror gains an `n_idle` param; the input decisions judge
# only CONNECTED-non-idle inputs (n_connected = n_inputs - n_absent - n_idle). Mirrors the
# src/genlock_lock_state.rs tests + the C parity gate's new n_idle axis. These FAIL on the pre-#1341
# decide() (no n_idle kwarg -> TypeError) and pass once the idle term lands.
# ================================================================================================
def test_idle_sender_only_unlocked_is_still_locked():
    # The cg-OBS scenario: 12 inputs, 2 live+locked, 10 idle SongPlayer keep-alive inputs. The idle
    # ones are excluded from n_connected AND n_locked -> 2/2 live-locked -> LOCKED, never the chronic
    # DEGRADED/recent_event the idle relock churn produced.
    f = healthy()
    f["n_inputs"] = 12
    f["n_locked"] = 2
    assert d.decide(n_idle=10, **f) == (d.ST_LOCKED, d.R_NONE)


def test_all_idle_is_healthy_idle_locked():
    # Every input present but idle (keep-alive only) -> HEALTHY-idle LOCKED, never UNLOCKED.
    f = healthy()
    f["n_inputs"] = 4
    f["n_locked"] = 0
    assert d.decide(n_idle=4, **f) == (d.ST_LOCKED, d.R_NONE)


def test_idle_plus_a_live_unlocked_still_degrades():
    # 12 inputs: 10 idle, 2 live of which only 1 locked -> a LIVE input is genuinely unlocked ->
    # DEGRADED. Idle inputs excluded, but a real fault on a live input still pages.
    f = healthy()
    f["n_inputs"] = 12
    f["n_locked"] = 1
    assert d.decide(n_idle=10, **f) == (d.ST_DEGRADED, d.R_INPUT_UNLOCKED)


def test_n_idle_and_n_absent_together_saturate_n_connected():
    # n_absent + n_idle > n_inputs (a transient over-count) -> n_connected saturates to 0 -> the
    # decision stays total and reads HEALTHY-idle, never a wrapped huge denominator.
    f = healthy()
    f["n_inputs"] = 3
    f["n_locked"] = 0
    assert d.decide(n_absent=2, n_idle=3, **f) == (d.ST_LOCKED, d.R_NONE)


def test_idle_default_zero_reproduces_pre_1341_verdict():
    # n_idle defaults 0 (a pre-#1341 caller) -> the old behaviour: 5 of 7 -> DEGRADED.
    f = healthy()
    f["n_locked"] = 5
    assert d.decide(**f) == (d.ST_DEGRADED, d.R_INPUT_UNLOCKED)


def test_analyze_carries_n_idle_from_a_v6_facet():
    # analyze() surfaces n_idle for the watchdog's observability log / card text (the widget already
    # decided `state`, so this never changes the verdict).
    body = json.dumps({"genlock_lock": {"state": "LOCKED", "reason": "none",
                                        "n_inputs": 12, "n_locked": 2, "n_absent": 0, "n_idle": 10}})
    res = d.analyze(body, box_reachable=1)
    assert res["verdict"] == "HEALTHY"
    assert res["n_idle"] == 10


def test_analyze_n_idle_none_for_a_pre_v6_facet():
    # A pre-v6 facet (no n_idle) -> analyze surfaces None (the pre-#1341 no-idle reading).
    body = json.dumps({"genlock_lock": {"state": "LOCKED", "reason": "none",
                                        "n_inputs": 7, "n_locked": 7}})
    res = d.analyze(body, box_reachable=1)
    assert res["n_idle"] is None


# ------------------------------------------------------------------------------------------------
# issue 1372 part D -- the media-clock (audio clock) term: the same precedence + verdict cases the
# Rust authority pins (src/genlock_lock_state.rs), so the python mirror decides it identically.
# ------------------------------------------------------------------------------------------------
def test_media_clock_drift_or_undisciplined_is_degraded_media_clock():
    for mc in (d.MC_DRIFT, d.MC_UNDISCIPLINED):
        f = healthy()
        f["media_clock"] = mc
        assert d.decide(**f) == (d.ST_DEGRADED, d.R_MEDIA_CLOCK)


def test_media_clock_never_unlocks_and_never_masks_an_unlock():
    for mc in (d.MC_DRIFT, d.MC_UNDISCIPLINED):
        f = healthy()
        f.update(media_clock=mc, clock_present=False)
        assert d.decide(**f) == (d.ST_UNLOCKED, d.R_CLOCK)
        f = healthy()
        f.update(media_clock=mc, n_locked=0)
        assert d.decide(**f) == (d.ST_UNLOCKED, d.R_NO_INPUT_LOCKED)


def test_qpc_step_beats_media_clock_beats_audio():
    f = healthy()
    f.update(media_clock=d.MC_DRIFT, qpc_drift_beyond_bound=True)
    assert d.decide(**f) == (d.ST_DEGRADED, d.R_QPC_DRIFT)
    f = healthy()
    f.update(media_clock=d.MC_DRIFT, audio_unpaired=True, audio_unexpected=True)
    assert d.decide(**f) == (d.ST_DEGRADED, d.R_MEDIA_CLOCK)


def _at_1hz(n, f):
    return [(i * 1000, f(i)) for i in range(n + 1)]


def _growth(samples):
    return d.media_clock_window_drift_us(samples, d.GENLOCK_MEDIA_CLOCK_MAX_RATE_PPM,
                                         d.GENLOCK_MEDIA_CLOCK_STEP_FLOOR_US,
                                         d.GENLOCK_MEDIA_CLOCK_MAX_GAP_MS)


def test_media_clock_window_drift_mirrors_the_rust_authority():
    # the pre-part-A stream: 13.5 us per 1 Hz sample -> 8.1 ms over the 600 s window
    assert _growth(_at_1hz(600, lambda i: i * 27 // 2)) == 8100
    # a dantesync locked master stepping 1460 us with phase_slew off, and a client stepping 600 us:
    # steps, never drift
    assert _growth(_at_1hz(600, lambda i: (i // 60) * 1460)) == 0
    assert _growth(_at_1hz(600, lambda i: (i // 30) * 600)) == 0
    # the same steps on top of the rate: only the rate counts (each step sample takes its ~14 us)
    assert _growth(_at_1hz(600, lambda i: i * 27 // 2 + (i // 60) * 1460)) == 7960
    assert _growth(_at_1hz(600, lambda i: (i % 3 - 1) * 40)) == 0     # noise that returns
    assert _growth([(0, 0), (1000, 400)]) == 400                       # the rate ceiling
    assert _growth([(0, 0), (1000, 401)]) == 0
    assert _growth([(0, 0), (4000, 1150)]) == 1150                     # a 4 s UI stall
    assert _growth([(0, 0), (6000, 100)]) == 0                         # a 6 s stall adds nothing
    assert _growth([(0, 7)]) == 0
    assert _growth([]) == 0


def test_media_clock_step_allowance_mirrors_the_rust_authority():
    r, f = d.GENLOCK_MEDIA_CLOCK_MAX_RATE_PPM, d.GENLOCK_MEDIA_CLOCK_STEP_FLOOR_US
    assert [d.media_clock_step_allowance_us(dt, r, f) for dt in (1000, 1003, 1004, 4000, 0, -5)] == \
        [400, 400, 401, 1150, 150, 150]
    assert d.media_clock_step_allowance_us(1000, 0, f) == 150


def test_media_clock_verdict_mirrors_the_rust_authority():
    b = d.GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US
    assert d.media_clock_verdict(True, 8100, b, "active", True) == d.MC_DRIFT
    for drift in (-2000, -1, 0, 1, 2000):
        assert d.media_clock_verdict(True, drift, b, "active", True) == d.MC_OK
    assert d.media_clock_verdict(False, 99000, b, "active", True) == d.MC_OK  # window not ready
    assert d.media_clock_verdict(True, 5000, b, "n/a", True) == d.MC_DRIFT    # Linux: drift only
    for disc in ("disabled", "read_failed", "api_missing"):
        assert d.media_clock_verdict(False, 0, b, disc, True) == d.MC_UNDISCIPLINED
        assert d.media_clock_verdict(False, 0, b, disc, False) == d.MC_OK  # no dantesync
    for disc in ("unknown", "active", "n/a"):
        assert d.media_clock_verdict(False, 0, b, disc, True) == d.MC_OK


def test_analyze_names_the_media_clock_sub_kind():
    mc = {"state": "drift", "drift_us": 8100, "window_s": 600, "ready": True, "discipline": "active"}
    r = d.analyze(_bundle("DEGRADED", "media_clock", extra={"media_clock": mc}), box_reachable=1)
    assert r["verdict"] == "DEGRADED"
    assert r["reason"] == "media_clock:drift"
    assert r["media_clock"] == "drift" and r["media_clock_drift_us"] == 8100
    assert r["media_clock_discipline"] == "active"
    mc2 = dict(mc, state="undisciplined", drift_us=0, discipline="disabled")
    r = d.analyze(_bundle("DEGRADED", "media_clock", extra={"media_clock": mc2}), box_reachable=1)
    assert r["reason"] == "media_clock:undisciplined"


def test_analyze_pre_v7_facet_has_no_media_clock_and_keeps_the_bare_reason():
    r = d.analyze(_bundle("LOCKED", "none"), box_reachable=1)
    assert r["media_clock"] is None and r["media_clock_drift_us"] is None
    # a media_clock reason without the object (malformed) stays the bare token, never `media_clock:`
    r = d.analyze(_bundle("DEGRADED", "media_clock"), box_reachable=1)
    assert r["reason"] == "media_clock"
    # an unrelated reason is never rewritten by a stray media_clock object
    mc = {"state": "drift", "drift_us": 8100, "window_s": 600, "ready": True, "discipline": "active"}
    r = d.analyze(_bundle("DEGRADED", "ntp_failed", extra={"media_clock": mc}), box_reachable=1)
    assert r["reason"] == "ntp_failed"


def test_cli_prints_the_media_clock_fields(capsys):
    mc = {"state": "drift", "drift_us": 8100, "window_s": 600, "ready": True, "discipline": "active"}
    body = _bundle("DEGRADED", "media_clock", extra={"media_clock": mc})

    class _Stdin:
        class buffer:  # noqa: N801 -- mimics sys.stdin.buffer
            @staticmethod
            def read():
                return body.encode()

    old = sys.stdin
    sys.stdin = _Stdin
    try:
        assert d._main(["analyze", "--box-reachable", "1"]) == 0
    finally:
        sys.stdin = old
    out = capsys.readouterr().out.splitlines()
    assert "reason=media_clock:drift" in out
    assert "media_clock=drift" in out
    assert "media_clock_drift_us=8100" in out
    assert "media_clock_discipline=active" in out
