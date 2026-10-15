"""issue 1381 -- the `audio-stall #1367:` line gained the audio-thread attribution fields
(wake_late / sleep_over / lock_wait / out_pass / out_cb_*, media-io/audio-io.c, design
6191167734). The dev1 audio-mixer pager reads that same line through
`bundle_state_gather.audio_mixer_from_log`; the new fields are appended after `tick_ms=`, so the
existing facets must keep parsing.

The line is rendered from the REAL C format string in obs-audio.c (not a hand-typed copy), so a
reorder or a new field in front of `tick_ms=` fails here before it reaches a box.
"""
import pathlib
import re
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402

OBS_AUDIO = _ROOT / "vendor/obs-studio/libobs/obs-audio.c"


def _c_format():
    """The concatenated string literals of the blog() call that writes the stall line."""
    src = OBS_AUDIO.read_text()
    start = src.index('"audio-stall #1367: tick_gap_max_ms=')
    end = src.index('",', start) + 1
    return "".join(re.findall(r'"((?:[^"\\]|\\.)*)"', src[start:end]))


def _py_format(c_fmt):
    out = (c_fmt.replace("%llu", "%d").replace("%llx", "%x").replace("%u", "%d"))
    assert "%ll" not in out and "%z" not in out, out
    return out


def _render(ts, ticks, over, name="NDI PGM"):
    fmt = _py_format(_c_format())
    values = [101.4, 1.7, ticks, over, 21.3, 95.3, 3.1, 12.3, 88.9, 5.6, 1, 98.1, 1.0, 93.1, 12.4,
              4.0, 80.1, 1, "output", 0x7FF612345678, 0xDEADBEEF00, name]
    assert fmt.count("%") == len(values), fmt
    return f"{ts}: " + fmt % tuple(values)


def test_the_c_line_keeps_the_1367_fields_first_and_carries_the_attribution():
    line = _render("06:08:15.416", 2816, 441)
    assert ("audio-stall #1367: tick_gap_max_ms=101.4 callback_max_ms=1.7 ticks=2816 "
            "ticks_over=441 tick_ms=21.3 wake_late_max_ms=95.3") in line
    for field in ("sleep_over_max_ms=", "lock_wait_max_ms=", "out_pass_max_ms=", "tail_max_ms=",
                  "late_slept=1", "late_prev_busy_ms=", "late_prev_mix_ms=", "late_prev_pass_ms=",
                  "late_prev_lock_ms=", "late_prev_tail_ms=", "out_cb_max_ms=", "out_cb_mix=",
                  "out_cb_kind=", "out_cb_fn=0x", "out_cb_param=0x"):
        assert field in line, field
    # the free-text output name is the LAST field
    assert line.endswith(" out_cb_name='NDI PGM'"), line


def test_the_mixer_facet_still_parses_the_extended_line():
    text = "\n".join([
        _render("06:08:15.416", 2812, 641),
        _render("06:09:15.430", 2760, 797),
        "06:09:20.100: some other line",
    ]) + "\n"
    ticks, over, window_ms, tick_ms, age = bsg.audio_mixer_from_log(text)
    assert (ticks, over, tick_ms) == ("2760", "797", "21.3")
    assert window_ms == "60014"
    assert age == "5"


def test_an_output_name_that_looks_like_a_field_does_not_move_the_counts():
    # out_cb_name is free text (an output name) at the END of the line; the facet's fields all
    # sit before it, so even a name spelling `ticks=` cannot change the reading.
    text = "\n".join([
        _render("06:08:15.416", 2812, 3, name="x ticks=1 ticks_over=999 tick_ms=9.9"),
        _render("06:09:15.430", 2813, 4, name="x ticks=1 ticks_over=999 tick_ms=9.9"),
    ]) + "\n"
    ticks, over, _window, tick_ms, _age = bsg.audio_mixer_from_log(text)
    assert (ticks, over, tick_ms) == ("2813", "4", "21.3")
