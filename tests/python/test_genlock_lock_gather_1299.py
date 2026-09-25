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
        # #1299 v2: `connected` defaults True for this v1 fixture (no `connected` key in the line).
        # #1341 v6: `idle` defaults False for this pre-v6 fixture (no `idle` key in the line).
        "locked": True, "connected": True, "idle": False, "latency_ms": 3, "underruns": 0,
        "relocks": 1, "late_holds": 0, "depth": 2
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


# ---- #1299 v2: n_absent (senderless input count) + per-input connected --------------------------

# A v2 LOCKED line: 4 genlock inputs, 3 connected+locked, the 4th ('NDIA cg stream') has NO sender
# (connected:false). This is the reopen scenario — the widget already decided LOCKED (not DEGRADED)
# because n_connected=3 == n_locked; the facet must carry n_absent + per-input connected through.
V2_ABSENT_LINE = (
    '10:47:06.003: genlock-lock-json: {"v":2,"state":"LOCKED","reason":"none","n_inputs":4,'
    '"n_locked":3,"n_absent":1,"latency_ms":3,"clock":"locked","output":"absent","recent_event":false,'
    '"qpc_drift_ms":0,"inputs":['
    '{"name":"NDI cam1","locked":true,"connected":true,"latency_ms":3,"underruns":0,"relocks":0,"late_holds":0,"depth":2},'
    '{"name":"NDIA cg stream","locked":false,"connected":false,"latency_ms":3,"underruns":0,"relocks":0,"late_holds":0,"depth":0}'
    ']} (#1299)\n'
)


def test_v2_line_carries_n_absent_and_per_input_connected():
    f = bsg.genlock_lock_facet_from_log(V2_ABSENT_LINE)
    assert f is not None
    assert f["state"] == "LOCKED" and f["reason"] == "none"
    assert f["n_inputs"] == 4 and f["n_locked"] == 3 and f["n_absent"] == 1
    assert f["inputs"]["NDI cam1"]["connected"] is True
    # the senderless input is visible (report-only) with connected=false — never dropped
    assert f["inputs"]["NDIA cg stream"]["connected"] is False
    assert f["inputs"]["NDIA cg stream"]["locked"] is False


def test_v1_line_defaults_n_absent_none_and_connected_true():
    # A v1 line from an older build (no n_absent / no per-input connected) must degrade gracefully:
    # n_absent -> None (decision treats absent as 0, the pre-#1299 all-connected reading) and every
    # input -> connected True. LOCKED_LINE is the existing v1 fixture.
    f = bsg.genlock_lock_facet_from_log(LOCKED_LINE)
    assert f["n_absent"] is None
    assert f["inputs"]["NDI cam1"]["connected"] is True
    assert f["inputs"]["NDI cam2"]["connected"] is True


# ---- #1341 v6: n_idle (connected-but-idle input count) + per-input idle --------------------------

# A v6 LOCKED line: the cg-OBS scenario — 12 genlock inputs, 2 live+locked, 10 idle SongPlayer
# keep-alive inputs (connected:true, idle:true). The widget already decided LOCKED (not DEGRADED)
# because n_connected = 12 - 0 - 10 = 2 == n_locked; the facet must carry n_idle + per-input idle.
V6_IDLE_LINE = (
    '10:48:06.003: genlock-lock-json: {"v":6,"state":"LOCKED","reason":"none","n_inputs":12,'
    '"n_locked":2,"n_absent":0,"n_idle":10,"latency_ms":3,"clock":"locked","output":"stamping",'
    '"recent_event":false,"qpc_drift_ms":0,"inputs":['
    '{"name":"NDI 2ME PGM","locked":true,"connected":true,"idle":false,"latency_ms":3,"underruns":0,"relocks":0,"late_holds":0,"depth":2},'
    '{"name":"sp-slow_video","locked":false,"connected":true,"idle":true,"latency_ms":3,"underruns":0,"relocks":60,"late_holds":5,"depth":0}'
    ']} (#1299)\n'
)


def test_v6_line_carries_n_idle_and_per_input_idle():
    f = bsg.genlock_lock_facet_from_log(V6_IDLE_LINE)
    assert f is not None
    assert f["state"] == "LOCKED" and f["reason"] == "none"
    assert f["n_inputs"] == 12 and f["n_locked"] == 2 and f["n_idle"] == 10
    assert f["inputs"]["NDI 2ME PGM"]["idle"] is False
    # the idle keep-alive input is visible (report-only) with idle=true — never dropped or blamed
    assert f["inputs"]["sp-slow_video"]["idle"] is True
    assert f["inputs"]["sp-slow_video"]["connected"] is True


def test_pre_v6_line_defaults_n_idle_none_and_idle_false():
    # A pre-v6 line (no n_idle / no per-input idle) must degrade gracefully: n_idle -> None (decision
    # treats idle as 0, the pre-#1341 reading) and every input -> idle False. V2_ABSENT_LINE has no
    # idle keys.
    f = bsg.genlock_lock_facet_from_log(V2_ABSENT_LINE)
    assert f["n_idle"] is None
    assert f["inputs"]["NDI cam1"]["idle"] is False
    assert f["inputs"]["NDIA cg stream"]["idle"] is False


# --- issue 1372 part D (schema v7): the media (audio) clock facet ---------------------------------

V7_MEDIA_DRIFT_LINE = (
    '12:00:00.000: genlock-lock-json: {"v":7,"state":"DEGRADED","reason":"media_clock","n_inputs":4,'
    '"n_locked":4,"n_absent":0,"n_idle":0,"latency_ms":3,"clock":"locked","output":"stamping",'
    '"recent_event":false,"recent_event_inputs":[],"audio_unexpected_inputs":[],"qpc_drift_ms":67,'
    '"inputs":[],"qpc_drift_ppm":13.500,"qpc_expected_ppm":13.000,"qpc_step":false,'
    '"media_clock":{"state":"drift","drift_ms":8,"window_s":600,"ready":true,"discipline":"active"}}'
    ' (#1299)\n'
)


def test_v7_line_carries_the_media_clock_facet():
    f = bsg.genlock_lock_facet_from_log(V7_MEDIA_DRIFT_LINE)
    assert f["state"] == "DEGRADED" and f["reason"] == "media_clock"
    assert f["media_clock"] == {"state": "drift", "drift_ms": 8, "window_s": 600, "ready": True,
                                "discipline": "active"}


def test_pre_v7_line_omits_the_media_clock_facet():
    # An older build has no media_clock object: the key is OMITTED, never a fabricated verdict.
    for line in (LOCKED_LINE, UNLOCKED_LINE, RECEIVER_LOCKED_LINE):
        assert "media_clock" not in bsg.genlock_lock_facet_from_log(line)


def test_malformed_media_clock_object_is_omitted():
    good = ('"media_clock":{"state":"drift","drift_ms":8,"window_s":600,"ready":true,'
            '"discipline":"active"}')
    for bad in ('"media_clock":"drift"', '"media_clock":{"drift_ms":8}', '"media_clock":{"state":1}'):
        line = V7_MEDIA_DRIFT_LINE.replace(good, bad)
        assert bad in line
        f = bsg.genlock_lock_facet_from_log(line)
        assert f is not None and "media_clock" not in f
