"""issue 1425 (E2 slice 3, design 6220191978) -- the audio-mixer BUFFERING arm's session start once
the genlock libobs has no buffering floor.

The arm pages RAISED when the mix buffering sits at least one mixer tick above its session start
(issue 1381). Its start value was the genlock floor line, printed at the first tick of every launch.
With the floor at 0 that line is never printed, so a bounded read of a long session had no start at
all (the arm read UNKNOWN for good) and a whole-file read fell back to its first non-zero total,
which on a launch that drew no buffering is the raise itself. Since slice 3 the audio reset block's
`buffering type:  dynamically increasing from N ms (genlock floor, issue 1425)` line marks a new
audio session, and the start is the first `#800` total logged at least
AUDIO_BUFFERING_SETTLE_S after it: by then the launch's own startup race (0-43 ms) is done.

The lines are rendered from the shipped C format strings (obs.c, obs-audio.c), never hand-typed.
Tier-0 pytest (no cargo).
"""
import pathlib
import re
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import audio_mixer_decision as amd  # noqa: E402
import bundle_state_gather as bsg  # noqa: E402

SEP = bsg.LOG_BOUNDED_READ_SEPARATOR
OBS_C = _ROOT / "vendor/obs-studio/libobs/obs.c"
OBS_AUDIO = _ROOT / "vendor/obs-studio/libobs/obs-audio.c"


def _c_string(path, head):
    """The C string literal(s) starting at `head` up to the end of the argument, joined."""
    src = path.read_text()
    start = src.index(head)
    end = src.index('",', start) + 1
    return "".join(re.findall(r'"((?:[^"\\]|\\.)*)"', src[start:end]))


def _reset_line(ts, floor_ms=0):
    fmt = _c_string(OBS_C, '"\\tbuffering type:  dynamically increasing from')
    return f"{ts}: " + fmt.replace("\\t", "\t") % floor_ms


def _above_line(ts, add_ms, total_ms, source):
    fmt = _c_string(OBS_AUDIO, '"genlock audio buffering ABOVE the floor (issue 1367)')
    return f"{ts}: " + fmt % (add_ms, total_ms, source)


def _t800(ts, total_ms, ticks):
    fmt = _c_string(OBS_AUDIO, '"audio-telemetry #800: total_buffering=')
    return f"{ts}: " + fmt % (total_ms, ticks, 45, "-")


def _text(lines):
    return "\n".join(lines) + "\n"


def _launch(draw_ms=42, draw_ticks=2):
    """A slice-3 launch: the reset block, the pre-tick #800 line, the startup race's growth by the
    mic, then the minute lines."""
    return [
        _reset_line("10:00:00.000"),
        _t800("10:00:00.001", 0, 0),
        _above_line("10:00:00.100", draw_ms, draw_ms, "Mic/Aux") if draw_ms else
        "10:00:00.100: program-render-audit: lagged=0",
        _t800("10:01:00.010", draw_ms, draw_ticks),
        _t800("10:02:00.020", draw_ms, draw_ticks),
    ]


def test_the_rendered_lines_are_the_shipped_shapes():
    assert _reset_line("10:00:00.000") == (
        "10:00:00.000: \tbuffering type:  dynamically increasing from 0 ms (genlock floor, issue 1425)")
    assert _above_line("10:00:00.100", 21, 42, "mbc") == (
        "10:00:00.100: genlock audio buffering ABOVE the floor (issue 1367): adding 21 milliseconds of "
        "audio buffering, total audio buffering is now 42 milliseconds (source: mbc)")


def test_the_settled_launch_draw_is_the_start():
    now, start, source, age = bsg.audio_buffering_from_log(_text(_launch()))
    assert (now, start, source, age) == ("42", "42", "Mic/Aux", "0")
    assert amd.classify_buffering(int(now), int(start), 0, True)["verdict"] == "HEALTHY"


def test_a_raise_after_the_launch_reads_raised_and_names_its_source():
    lines = _launch() + [
        _above_line("10:30:00.000", 21, 64, "pre-fotky video"),
        _t800("10:30:00.020", 64, 3),
        _t800("10:31:00.030", 64, 3),
    ]
    now, start, source, _age = bsg.audio_buffering_from_log(_text(lines))
    assert (now, start, source) == ("64", "42", "pre-fotky video")
    assert amd.classify_buffering(int(now), int(start), 0, True)["verdict"] == "RAISED"


def test_a_launch_that_drew_nothing_starts_at_zero_so_a_later_raise_still_reads_raised():
    # The old fallback (the first NON-ZERO total) would have taken the raise itself as the start.
    lines = _launch(draw_ms=0, draw_ticks=0) + [
        _above_line("10:30:00.000", 21, 21, "media clip"),
        _t800("10:30:00.020", 21, 1),
    ]
    now, start, source, _age = bsg.audio_buffering_from_log(_text(lines))
    assert (now, start, source) == ("21", "0", "media clip")
    assert amd.classify_buffering(int(now), int(start), 0, True)["verdict"] == "RAISED"


def test_a_session_younger_than_the_settle_window_has_no_start_yet():
    lines = [_reset_line("10:00:00.000"), _t800("10:00:00.001", 0, 0),
             _t800("10:00:20.000", 42, 2)]
    now, start, _src, _age = bsg.audio_buffering_from_log(_text(lines))
    assert (now, start) == ("42", "")


def test_a_long_session_takes_the_start_from_the_head_slice():
    head = _launch()
    tail = [_t800("14:00:00.000", 42, 2), _t800("14:01:00.000", 64, 3),
            "14:01:05.000: program-render-audit: lagged=0"]
    now, start, source, age = bsg.audio_buffering_from_log(_text(head) + SEP + _text(tail))
    assert (now, start, age) == ("64", "42", "5")
    assert source == "Mic/Aux"   # the newest ABOVE line in the bounded read (the head's launch draw)


def test_an_audio_reset_starts_a_new_session():
    lines = _launch() + [
        _t800("10:30:00.000", 106, 5),
        _reset_line("10:31:00.000"),
        _t800("10:31:00.001", 0, 0),
        _t800("10:32:00.000", 21, 1),
    ]
    now, start, _src, _age = bsg.audio_buffering_from_log(_text(lines))
    assert (now, start) == ("21", "21")


def test_a_reset_line_with_no_total_after_it_holds_no_current_value():
    lines = _launch() + [_reset_line("10:31:00.000")]
    assert bsg.audio_buffering_from_log(_text(lines)) == ("", "", "", "")


def test_the_settle_window_is_half_a_telemetry_minute():
    # The #800 summary prints once a minute: the first line after the pre-tick one is the settled
    # draw, a line inside the startup race never is.
    assert 0 < bsg.AUDIO_BUFFERING_SETTLE_S < 60
