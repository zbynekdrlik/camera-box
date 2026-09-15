"""#1299 — unit tests for the genlock_lock facet parser added to scripts/bundle_state_gather.py.

The #1298 statusbar widget joins the three genlock producers into ONE decided verdict and emits it
as a versioned `genlock-lock-json: {…} (#1299)` line (heartbeat + on-change). This parser reads the
NEWEST such line from the SAME #1222-bounded OBS-log text every other facet uses and reshapes it
into the nested `genlock_lock` facet the dev1 watchdog + rig-status read. Absent line -> None
(facet OMITTED, never a false UNLOCKED).

Same "source the PURE parser, verify live separately" split as test_audio_lag_gather_1226.py —
no live OBS / no live box needed.
"""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402


# A real-shaped LOCKED line (the #1298 widget's genlock-lock-json: heartbeat), with an OBS log-time
# prefix and the trailing (#1299) tag the parser must both ignore.
LOCKED_LINE = (
    '10:44:06.003: genlock-lock-json: {"v":1,"state":"LOCKED","reason":"none","n_inputs":7,'
    '"n_locked":7,"latency_ms":3,"clock":"locked","output":"stamping","recent_event":false,'
    '"qpc_drift_ms":0,"inputs":['
    '{"name":"NDI cam1","locked":true,"latency_ms":3,"underruns":0,"relocks":1,"late_holds":0,"depth":2},'
    '{"name":"NDI cam2","locked":true,"latency_ms":3,"underruns":0,"relocks":0,"late_holds":0,"depth":2}'
    ']} (#1299)\n'
)

UNLOCKED_LINE = (
    '10:45:06.003: genlock-lock-json: {"v":1,"state":"UNLOCKED","reason":"clock","n_inputs":7,'
    '"n_locked":0,"latency_ms":0,"clock":"absent","output":"not-stamping","recent_event":false,'
    '"qpc_drift_ms":0,"inputs":[]} (#1299)\n'
)

RECEIVER_LOCKED_LINE = (  # imag: a pure receiver, output absent, still LOCKED
    '10:46:06.003: genlock-lock-json: {"v":1,"state":"LOCKED","reason":"none","n_inputs":7,'
    '"n_locked":7,"latency_ms":3,"clock":"locked","output":"absent","recent_event":false,'
    '"qpc_drift_ms":0,"inputs":[]} (#1299)\n'
)


def test_absent_line_is_none():
    assert bsg.genlock_lock_facet_from_log("") is None
    assert bsg.genlock_lock_facet_from_log("some unrelated OBS log\nlines here\n") is None


def test_locked_line_parses_to_facet():
    f = bsg.genlock_lock_facet_from_log(LOCKED_LINE)
    assert f is not None
    assert f["state"] == "LOCKED" and f["reason"] == "none"
    assert f["n_inputs"] == 7 and f["n_locked"] == 7 and f["latency_ms"] == 3
    assert f["recent_event"] is False and f["qpc_drift_ms"] == 0
    assert f["clock"] == {"state": "locked"}
    assert f["output"] == {"present": True, "stamping_wallclock": True}
    assert f["source"] == "log"
    # inputs are keyed by name
    assert set(f["inputs"]) == {"NDI cam1", "NDI cam2"}
    assert f["inputs"]["NDI cam1"] == {
        "locked": True, "latency_ms": 3, "underruns": 0, "relocks": 1, "late_holds": 0, "depth": 2
    }


def test_newest_line_wins():
    # an old LOCKED heartbeat then a newer UNLOCKED one -> the facet reflects the NEWEST.
    text = LOCKED_LINE + "10:44:30.000: some other line\n" + UNLOCKED_LINE
    f = bsg.genlock_lock_facet_from_log(text)
    assert f["state"] == "UNLOCKED" and f["reason"] == "clock"
    assert f["n_locked"] == 0
    assert f["clock"] == {"state": "absent"}
    # output "not-stamping" = a genlock output IS present but not stamping (present True, not False).
    assert f["output"] == {"present": True, "stamping_wallclock": False}
    assert f["inputs"] == {}


def test_receiver_box_output_absent_is_present_false():
    f = bsg.genlock_lock_facet_from_log(RECEIVER_LOCKED_LINE)
    assert f["state"] == "LOCKED"
    assert f["output"] == {"present": False, "stamping_wallclock": False}


def test_malformed_json_line_is_none_never_crashes():
    bad = "10:44:06.003: genlock-lock-json: {this is not valid json (#1299)\n"
    assert bsg.genlock_lock_facet_from_log(bad) is None


def test_marker_present_but_no_braces_is_none():
    assert bsg.genlock_lock_facet_from_log("10:44:06: genlock-lock-json: (no payload)\n") is None


def test_non_object_payload_is_none():
    # a JSON array / scalar after the marker must not be mistaken for a facet.
    assert bsg.genlock_lock_facet_from_log('x: genlock-lock-json: [1,2,3] (#1299)\n') is None
