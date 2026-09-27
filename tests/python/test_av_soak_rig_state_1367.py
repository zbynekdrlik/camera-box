"""issue 1367 -- the soak's pure RIG-STATE decisions (scripts/av_soak_rig_state.py).

Two decisions the soak's shell must never make by hand-parsing the rig-busy read:

  - `broadcast_state`: is a broadcast live right now? Cleanup restores the strih program only when
    the answer is a proven "idle" -- a cut on the strih program while the stream box streams is a
    cut on air (strih feeds the stream box's program).
  - `leftovers_plan`: which of the soak's flagged recordings may `--stop-leftovers` (the unit's
    ExecStopPost) stop? Only a box whose recording provably STARTED when the soak set its flag, and
    only while no box streams and both boxes read cleanly. Strih never streams, so "recording and
    not streaming" on strih alone is strih's normal broadcast state, never proof of a leftover.

Tier-0: pure python, no rig.
"""
import importlib.util
import json
import os
import subprocess
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
MODULE = os.path.join(REPO, "scripts", "av_soak_rig_state.py")

_spec = importlib.util.spec_from_file_location("av_soak_rig_state", MODULE)
ars = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ars)

NOW = 1_790_000_000.0


def _busy(strih=(False, False, None), stream=(False, False, None), busy=None, drop=None,
          exit3=False):
    """A rig-busy-check JSON line: per box (streaming, recording, recordTimecode)."""
    diags = []
    for name, (streaming, recording, tc) in (("strih", strih), ("stream", stream)):
        if name == drop:
            continue
        diags.append({"host": name, "streaming": streaming, "recording": recording,
                      "recordTimecode": tc})
    if busy is None:
        busy = any(d["streaming"] or d["recording"] for d in diags)
    out = {"busy": None if exit3 else busy, "reasons": [], "diagnostics": diags}
    return json.dumps(out)


def _state(strih=0, stream=0, strih_since=None, stream_since=None, lease="av-soak-X-1",
           window=None):
    lines = [f"strih={strih}", f"strih_since={'' if strih_since is None else int(strih_since)}",
             f"stream={stream}", f"stream_since={'' if stream_since is None else int(stream_since)}",
             f"lease={lease}"]
    if window is not None:
        lines.append(f"start_window_s={window}")
    return "\n".join(lines) + "\n"


def _plan(state, busy, now=NOW, window=60):
    return {box: (action, reason) for box, action, reason in
            ars.leftovers_plan(state, busy, now, window)}


# --- the recording age --------------------------------------------------------------------------


def test_parse_timecode():
    assert ars.parse_timecode("00:01:02.500") == pytest.approx(62.5)
    assert ars.parse_timecode("01:00:00") == pytest.approx(3600.0)
    assert ars.parse_timecode("10:00:00.000") == pytest.approx(36000.0)
    for bad in (None, "", "abc", "1:2", "00:61:00", "00:00:61"):
        assert ars.parse_timecode(bad) is None, bad


# --- is a broadcast live? -----------------------------------------------------------------------


def test_a_streaming_box_is_a_live_broadcast():
    assert ars.broadcast_state(_busy(stream=(True, True, "00:10:00"))) == ars.LIVE
    assert ars.broadcast_state(_busy(strih=(True, False, None))) == ars.LIVE


def test_a_live_broadcast_on_a_readable_box_wins_over_an_unreadable_one():
    assert ars.broadcast_state(_busy(stream=(True, True, "00:10:00"), drop="strih",
                                     exit3=True)) == ars.LIVE


def test_an_unreadable_rig_is_unknown_never_idle():
    assert ars.broadcast_state(_busy(drop="stream", exit3=True)) == ars.UNKNOWN
    assert ars.broadcast_state(_busy(drop="stream")) == ars.UNKNOWN, "a missing box is unread"
    assert ars.broadcast_state("") == ars.UNKNOWN
    assert ars.broadcast_state("not json") == ars.UNKNOWN
    assert ars.broadcast_state(json.dumps({"busy": False})) == ars.UNKNOWN


def test_recordings_without_a_stream_are_idle_for_the_broadcast_question():
    assert ars.broadcast_state(_busy()) == ars.IDLE
    assert ars.broadcast_state(_busy(strih=(False, True, "00:00:05"))) == ars.IDLE


# --- the recording.state file -------------------------------------------------------------------


def test_parse_state():
    st = ars.parse_state(_state(strih=1, strih_since=NOW, lease="av-soak-20260927T205047Z-42"))
    assert st["flags"] == {"strih": True, "stream": False}
    assert st["since"]["strih"] == NOW and st["since"]["stream"] is None
    assert st["lease"] == "av-soak-20260927T205047Z-42"
    # the pre-round-3 two-line shape still parses (no start time, no lease)
    old = ars.parse_state("strih=0\nstream=1\n")
    assert old["flags"] == {"strih": False, "stream": True}
    assert old["since"] == {"strih": None, "stream": None} and old["lease"] is None


# --- which leftovers may be stopped --------------------------------------------------------------


def test_the_soaks_own_fresh_recording_is_stopped():
    plan = _plan(_state(stream=1, stream_since=NOW - 100),
                 _busy(stream=(False, True, "00:01:35.000")))
    assert plan == {"stream": (ars.STOP, plan["stream"][1])}


def test_strih_recording_while_the_stream_box_broadcasts_is_never_stopped():
    # strih never streams: "recording, not streaming" is strih's normal broadcast state
    plan = _plan(_state(strih=1, strih_since=NOW - 100),
                 _busy(strih=(False, True, "00:01:35.000"), stream=(True, True, "00:01:35.000")))
    assert plan["strih"][0] == ars.KEEP
    assert "broadcast" in plan["strih"][1]


def test_nothing_is_stopped_when_a_box_is_unreadable():
    plan = _plan(_state(strih=1, strih_since=NOW - 100),
                 _busy(strih=(False, True, "00:01:35.000"), drop="stream", exit3=True))
    assert plan["strih"][0] == ars.KEEP
    assert "unreadable" in plan["strih"][1]


def test_a_recording_that_started_before_the_soak_set_its_flag_is_not_the_soaks():
    plan = _plan(_state(strih=1, strih_since=NOW - 100),
                 _busy(strih=(False, True, "01:00:00.000")))
    assert plan["strih"][0] == ars.KEEP
    # the age is OBS's frame-count duration (lagged frames undercount it): never a claim that the
    # recording is someone else's, only that it cannot be proven to be the soak's
    assert "cannot prove" in plan["strih"][1] and "not the soak" not in plan["strih"][1]


def test_a_recording_that_started_long_after_the_flag_is_not_the_soaks():
    plan = _plan(_state(strih=1, strih_since=NOW - 3600),
                 _busy(strih=(False, True, "00:00:30.000")))
    assert plan["strih"][0] == ars.KEEP


def test_the_ownership_window_edges():
    since = NOW - 1000
    # started 5 s before the flag (clock slack) .. start-window after it
    ok_early = _plan(_state(strih=1, strih_since=since),
                     _busy(strih=(False, True, "00:16:45.000")))  # started at since - 5
    ok_late = _plan(_state(strih=1, strih_since=since), _busy(strih=(False, True, "00:15:40.000")),
                    window=60)  # started at since + 60
    too_early = _plan(_state(strih=1, strih_since=since),
                      _busy(strih=(False, True, "00:16:46.000")))
    too_late = _plan(_state(strih=1, strih_since=since), _busy(strih=(False, True, "00:15:39.000")),
                     window=60)
    assert ok_early["strih"][0] == ars.STOP and ok_late["strih"][0] == ars.STOP
    assert too_early["strih"][0] == ars.KEEP and too_late["strih"][0] == ars.KEEP


def test_the_start_window_written_by_the_run_wins_over_the_callers():
    since = NOW - 1000
    busy = _busy(strih=(False, True, "00:16:10.000"))  # started at since + 30
    assert _plan(_state(strih=1, strih_since=since), busy, window=60)["strih"][0] == ars.STOP
    kept = _plan(_state(strih=1, strih_since=since, window=10), busy, window=60)
    assert kept["strih"][0] == ars.KEEP
    assert ars.parse_state(_state(window=10))["start_window_s"] == 10.0
    assert ars.parse_state(_state())["start_window_s"] is None


def test_no_start_time_means_no_proof_of_ownership():
    plan = _plan("strih=1\nstream=0\n", _busy(strih=(False, True, "00:00:30.000")))
    assert plan["strih"][0] == ars.KEEP
    assert "start time" in plan["strih"][1]


def test_an_unreadable_recording_age_is_kept():
    plan = _plan(_state(strih=1, strih_since=NOW - 100), _busy(strih=(False, True, None)))
    assert plan["strih"][0] == ars.KEEP


def test_a_flagged_box_that_is_not_recording_is_cleared():
    plan = _plan(_state(strih=1, strih_since=NOW - 100, stream=1, stream_since=NOW - 100),
                 _busy())
    assert plan == {"strih": (ars.CLEAR, plan["strih"][1]), "stream": (ars.CLEAR, plan["stream"][1])}


def test_an_unflagged_box_is_never_in_the_plan():
    assert _plan(_state(), _busy(strih=(False, True, "00:00:30.000"))) == {}


# --- the CLI the shell calls --------------------------------------------------------------------


def _cli(*args, stdin=""):
    return subprocess.run([sys.executable, MODULE, *args], input=stdin, capture_output=True,
                          text=True, timeout=30)


def test_cli_broadcast():
    p = _cli("broadcast", stdin=_busy(stream=(True, True, "00:10:00")))
    assert p.returncode == 0 and p.stdout.strip() == "live"
    assert _cli("broadcast", stdin="").stdout.strip() == "unknown"
    assert _cli("broadcast", stdin=_busy()).stdout.strip() == "idle"


def test_cli_leftovers(tmp_path):
    st = tmp_path / "recording.state"
    st.write_text(_state(strih=1, strih_since=NOW - 100, stream=1, stream_since=NOW - 100))
    p = _cli("leftovers", "--state", str(st), "--now", str(int(NOW)), "--start-window-s", "60",
             stdin=_busy(strih=(False, True, "00:01:35.000")))
    assert p.returncode == 0, p.stderr
    lines = [ln.split("\t") for ln in p.stdout.splitlines()]
    assert [(b, a) for b, a, _ in lines] == [("strih", "stop"), ("stream", "clear")]


def test_cli_leftovers_without_a_state_file_is_a_usage_error(tmp_path):
    p = _cli("leftovers", "--state", str(tmp_path / "absent"), "--now", "1",
             "--start-window-s", "60", stdin=_busy())
    assert p.returncode == 3


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-q"]))
