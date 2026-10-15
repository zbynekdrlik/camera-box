"""issue 1381 -- the audio-mixer pager pages a RAISED audio mix buffering (the ratchet).

On 15.10.2026 a 432 ms stall of the stream-snv audio callback made libobs add 85 ms of mix
buffering (85 -> 170 ms, `ABOVE the floor ... (source: pre-fotky video)`). The whole stream output
then stayed 85 ms later until the OBS restart at 21:28, and the watchdog read HEALTHY throughout:
its MIXER arm grades the newest `audio-stall` dump, and a stall that catches up leaves the tick
count near nominal (comments 6201721226 + 6201735573, design 6201780920).

The durable consequence is logged every minute by `audio-telemetry #800: total_buffering=<N> ms`.
This slice reads it:

  * gather (`bundle_state_gather.audio_buffering_from_log`): the newest #800 total, the session's
    start value (the genlock floor line, else the first non-zero #800 total in the tail), the
    source that raised it (the newest `ABOVE the floor ... (source: <name>)` line) and the in-log
    age of the newest #800 line;
  * decision (`audio_mixer_decision.classify_buffering`): RAISED when the buffering sits at least
    one mixer tick above its start value (decided in whole ticks: libobs truncates the totals to
    whole ms, so 4 -> 5 ticks reads 85 -> 106 = 21 ms, under the 21.3 ms tick);
  * watchdog: a third arm, 2-pass confirm, PRODUCTION-CRITICAL time-bucketed key
    `audio-buffering-<box>`, recovery a machine-channel log line only.

Two live facts the parser follows (read-only, stream-snv logs of 13.10 and 15.10):
  * the FIRST #800 line of a session can read `total_buffering=0 ms (ticks=0/45)` (stream-snv: in
    the same millisecond as the floor line, before the first tick; resolume 14.10 read 85), so it
    is never the start value;
  * `buffering_source=` on the #800 line reads `-` on every live line, also after a raise.

Tier-0 pytest (no cargo).
"""
import importlib.util
import json
import os
import pathlib
import re
import stat
import subprocess
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import audio_mixer_decision as amd  # noqa: E402
import bundle_state_gather as bsg  # noqa: E402

SEP = bsg.LOG_BOUNDED_READ_SEPARATOR
OBS_AUDIO = _ROOT / "vendor/obs-studio/libobs/obs-audio.c"

# The real lines, verbatim from the stream-snv log `2026-10-15 15-57-52.txt` (read-only).
FLOOR = ("15:57:53.164: genlock audio buffering floor (issue 1367): total audio buffering is now 85 "
         "milliseconds from the first audio tick, dynamically increasing above")
ABOVE = ("17:43:35.159: genlock audio buffering ABOVE the floor (issue 1367): adding 85 milliseconds "
         "of audio buffering, total audio buffering is now 170 milliseconds (source: pre-fotky "
         "video); ASRC level band BROKEN: buffering + 9 ms base lands -79.7 ms off the 100 ms level "
         "target (reach +/-35 ms) -- a mixed source on an absolute ASRC level target (#1335/#1355, "
         "e.g. the stream mbc) cannot reach it at this buffering")


def _c_format_800():
    """The REAL #800 summary format string from obs-audio.c (never a hand-typed copy)."""
    src = OBS_AUDIO.read_text()
    start = src.index('"audio-telemetry #800: total_buffering=')
    end = src.index('",', start) + 1
    return "".join(re.findall(r'"((?:[^"\\]|\\.)*)"', src[start:end]))


def _t800(ts, total_ms, ticks, source="-", max_ticks=45):
    return f"{ts}: " + _c_format_800() % (total_ms, ticks, max_ticks, source)


def _text(lines, crlf=False):
    nl = "\r\n" if crlf else "\n"
    return nl.join(lines) + nl


def _session_15_10():
    """The 15.10 stream-snv session around the raise, as the log has it."""
    return [
        _t800("15:57:53.164", 0, 0),
        FLOOR,
        _t800("15:58:53.173", 85, 4),
        _t800("17:42:54.283", 85, 4),
        "17:43:35.140: audio-stall #1367: tick_gap_max_ms=431.8 callback_max_ms=431.8 ticks=2813 "
        "ticks_over=1 tick_ms=21.3",
        ABOVE,
        _t800("17:43:54.294", 170, 8),
        _t800("17:44:54.304", 170, 8),
        "17:45:00.100: program-render-audit: lagged=0",
    ]


# ---------------------------------------------------------------------------------------------
# the #800 line rendered from the C format still looks like the live line
# ---------------------------------------------------------------------------------------------
def test_the_rendered_800_line_matches_the_live_shape():
    assert _t800("17:43:54.294", 170, 8) == (
        "17:43:54.294: audio-telemetry #800: total_buffering=170 ms (ticks=8/45) "
        "buffering_source=-")


# ---------------------------------------------------------------------------------------------
# gather: audio_buffering_from_log
# ---------------------------------------------------------------------------------------------
def test_the_real_raise_reads_now_start_source_and_age():
    now, start, source, age = bsg.audio_buffering_from_log(_text(_session_15_10()))
    assert (now, start, source) == ("170", "85", "pre-fotky video")
    assert age == "6"   # 17:44:54.304 behind the log head 17:45:00.100


def test_a_healthy_session_reads_the_floor_as_now_and_start():
    lines = [_t800("21:28:27.715", 0, 0),
             FLOOR.replace("15:57:53.164", "21:28:27.715"),
             _t800("21:29:27.725", 85, 4), _t800("21:30:27.736", 85, 4)]
    now, start, source, age = bsg.audio_buffering_from_log(_text(lines))
    assert (now, start, source, age) == ("85", "85", "", "0")


def test_the_zero_line_before_the_first_tick_is_never_the_start():
    # A build without the floor line: the session's first #800 total is 0 (before the first tick);
    # the start is the first NON-ZERO total of the tail.
    lines = [_t800("10:00:00.000", 0, 0), _t800("10:01:00.000", 85, 4),
             _t800("10:02:00.000", 106, 5)]
    now, start, _src, _age = bsg.audio_buffering_from_log(_text(lines))
    assert (now, start) == ("106", "85")


def test_a_long_session_takes_the_floor_from_the_head_slice():
    head = [_t800("15:57:53.164", 0, 0), FLOOR, _t800("15:58:53.173", 85, 4)]
    tail = [_t800("21:26:56.972", 170, 8), _t800("21:27:56.982", 170, 8),
            "21:28:00.000: program-render-audit: lagged=0"]
    text = _text(head) + SEP + _text(tail)
    now, start, source, age = bsg.audio_buffering_from_log(text)
    assert (now, start, age) == ("170", "85", "3")
    assert source == ""   # the ABOVE line left both slices: no source, never a guess


def test_the_newest_floor_line_wins_after_an_audio_reset():
    # An audio reset (e.g. a sample-rate change) re-raises the floor: its new value is the start.
    lines = [FLOOR, _t800("16:00:00.000", 85, 4),
             "16:00:30.000: genlock audio buffering floor (issue 1367): total audio buffering is "
             "now 92 milliseconds from the first audio tick, dynamically increasing above",
             _t800("16:01:00.000", 92, 4)]
    now, start, _src, _age = bsg.audio_buffering_from_log(_text(lines))
    assert (now, start) == ("92", "92")


def test_a_bounded_read_without_a_floor_line_is_unknown_not_a_guess():
    # Review round 1: without a floor line the first non-zero total of a TAIL is not the session
    # start -- a raise older than the tail would read 170/170 and grade HEALTHY. Only a whole-file
    # read may fall back to it.
    head = ["10:00:00.000: OBS 32.0.0 (64-bit, windows)"]
    tail = [_t800("12:00:00.000", 170, 8), _t800("12:01:00.000", 170, 8)]
    now, start, _src, _age = bsg.audio_buffering_from_log(_text(head) + SEP + _text(tail))
    assert (now, start) == ("170", "")


def test_an_800_line_older_than_a_newer_floor_line_is_not_current():
    # Review round 1: an audio reset prints a new floor line; a #800 total logged BEFORE it is the
    # previous buffering, never compared with the new start.
    lines = [FLOOR, _t800("16:00:00.000", 170, 8),
             FLOOR.replace("15:57:53.164", "16:00:30.000")]
    assert bsg.audio_buffering_from_log(_text(lines)) == ("", "", "", "")
    lines.append(_t800("16:01:00.000", 85, 4))
    assert bsg.audio_buffering_from_log(_text(lines))[:2] == ("85", "85")


def _floor(ts, ms):
    return (f"{ts}: genlock audio buffering floor (issue 1367): total audio buffering is now {ms} "
            "milliseconds from the first audio tick, dynamically increasing above")


def test_a_bounded_read_takes_the_tails_floor_over_the_heads():
    # Review round 2: an audio reset inside the tail (e.g. a sample-rate change) wins over the
    # session's floor line in the head slice.
    head = [_floor("09:00:00.000", 85), _t800("09:01:00.000", 85, 4)]
    tail = [_floor("12:00:00.000", 92), _t800("12:01:00.000", 92, 4)]
    now, start, _src, _age = bsg.audio_buffering_from_log(_text(head) + SEP + _text(tail))
    assert (now, start) == ("92", "92")


def test_a_bounded_read_takes_the_newest_floor_of_the_head():
    head = [_floor("09:00:00.000", 85), _floor("09:30:00.000", 92)]
    tail = [_t800("12:01:00.000", 92, 4)]
    now, start, _src, _age = bsg.audio_buffering_from_log(_text(head) + SEP + _text(tail))
    assert (now, start) == ("92", "92")


def test_the_first_minute_of_a_session_is_absent_not_zero():
    # The session's first #800 line (0 ms, before the first tick) precedes the floor line.
    lines = [_t800("21:28:27.715", 0, 0), FLOOR.replace("15:57:53.164", "21:28:27.715")]
    assert bsg.audio_buffering_from_log(_text(lines)) == ("", "", "", "")


def test_a_named_800_source_is_used_without_an_above_line():
    lines = [FLOOR, _t800("16:00:00.000", 106, 5, source="ASIO Input Capture")]
    _now, _start, source, _age = bsg.audio_buffering_from_log(_text(lines))
    assert source == "ASIO Input Capture"


def test_no_800_line_is_absent():
    assert bsg.audio_buffering_from_log(_text([FLOOR, ABOVE])) == ("", "", "", "")
    assert bsg.audio_buffering_from_log("") == ("", "", "", "")


def test_the_800_line_only_in_the_head_slice_is_absent():
    # Telemetry that is no longer in the tail is not the current buffering.
    text = _text([FLOOR, _t800("15:58:53.173", 85, 4)]) + SEP + _text(
        ["21:28:00.000: program-render-audit: lagged=0"])
    assert bsg.audio_buffering_from_log(text)[0] == ""


def test_crlf_is_tolerated():
    now, start, source, age = bsg.audio_buffering_from_log(_text(_session_15_10(), crlf=True))
    assert (now, start, source, age) == ("170", "85", "pre-fotky video", "6")


def test_the_shared_tail_gives_the_same_result():
    text = _text(_session_15_10())
    assert (bsg.audio_buffering_from_log(text, tail=bsg.timestamped_tail_lines(text))
            == bsg.audio_buffering_from_log(text))


def test_build_bundle_state_carries_the_buffering_keys_and_omits_empty():
    state = bsg.build_bundle_state(audio_buffering_ms="170", audio_buffering_start_ms="85",
                                   audio_buffering_source="pre-fotky video",
                                   audio_buffering_age_s="0")
    assert state["audio_buffering_ms"] == "170"
    assert state["audio_buffering_start_ms"] == "85"
    assert state["audio_buffering_source"] == "pre-fotky video"
    assert state["audio_buffering_age_s"] == "0"
    empty = bsg.build_bundle_state(audio_buffering_ms="", audio_buffering_start_ms="",
                                   audio_buffering_source="", audio_buffering_age_s="")
    assert not any(k.startswith("audio_buffering") for k in empty)


def _load_server():
    spec = importlib.util.spec_from_file_location("bundle_state_server_1381b",
                                                  _SCRIPTS / "bundle-state-server.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_the_server_serves_the_buffering_facet():
    bss = _load_server()
    facets = bss._audio_log_facets(_text(_session_15_10()), 17 * 3600 + 45 * 60 + 2, "mbc")
    assert facets["audio_buffering_ms"] == "170"
    assert facets["audio_buffering_start_ms"] == "85"
    assert facets["audio_buffering_source"] == "pre-fotky video"
    assert facets["audio_buffering_age_s"] == "6"


# ---------------------------------------------------------------------------------------------
# decision: classify_buffering (the truth table)
# ---------------------------------------------------------------------------------------------
def _b(now, start, age=0, reachable=1, tick_ms="21.3", head=None, **kw):
    return amd.classify_buffering(now, start, age, reachable, tick_ms=tick_ms,
                                  log_head_age_s=head, **kw)


@pytest.mark.parametrize("now,start,verdict,ticks", [
    (85, 85, "HEALTHY", 0),
    (106, 85, "RAISED", 1),     # one tick: libobs truncates 106.67 - 85.33 to 21 ms < 21.3
    (170, 85, "RAISED", 4),     # the 15.10 stream-snv raise
    (213, 85, "RAISED", 6),     # the 13.10 one (+128)
    (95, 85, "HEALTHY", 0),     # under half a tick (cannot happen: totals move by whole ticks)
    (85, 170, "HEALTHY", 0),    # lower than the start (a reset brought it down): never a page
])
def test_the_truth_table_at_48_khz(now, start, verdict, ticks):
    res = _b(now, start)
    assert res["verdict"] == verdict
    assert res["raised_ticks"] == ticks
    assert res["delta_ms"] == now - start


def test_one_tick_at_44_1_khz_is_raised():
    # 4 -> 5 ticks at 44.1 kHz: 92.88 -> 116.1, truncated 92 -> 116.
    res = _b(116, 92, tick_ms="23.2")
    assert (res["verdict"], res["raised_ticks"]) == ("RAISED", 1)


def test_the_tick_is_derived_never_a_literal():
    shortest = min(amd.AUDIO_OUTPUT_FRAMES * 1000.0 / r for r in amd.KNOWN_SAMPLE_RATES)
    assert amd.buffering_tick_ms("21.3") == pytest.approx(amd.AUDIO_OUTPUT_FRAMES * 1000.0 / 48000)
    assert amd.buffering_tick_ms("23.2") == pytest.approx(amd.AUDIO_OUTPUT_FRAMES * 1000.0 / 44100)
    # unknown or absent mixer tick: the shortest known tick still grades (half of it is under
    # any real tick step)
    assert amd.buffering_tick_ms(None) == pytest.approx(shortest)
    assert amd.buffering_tick_ms("99.9") == pytest.approx(shortest)
    assert _b(106, 85, tick_ms=None)["verdict"] == "RAISED"


def test_unreachable_is_skip():
    assert _b(170, 85, reachable=0)["verdict"] == "SKIP"


@pytest.mark.parametrize("now,start", [(None, 85), (170, None), (None, None)])
def test_a_missing_value_is_unknown(now, start):
    assert _b(now, start)["verdict"] == "UNKNOWN"


def test_an_old_800_line_is_stale():
    assert _b(170, 85, age=181)["verdict"] == "STALE"
    assert _b(170, 85, age=180)["verdict"] == "RAISED"
    assert _b(170, 85, age=300, stale_after_s=600)["verdict"] == "RAISED"


def test_a_frozen_log_is_stale():
    # OBS down or hung: the log head is old, its last buffering reading is not current.
    assert _b(170, 85, head=181)["verdict"] == "STALE"
    assert _b(170, 85, head=60)["verdict"] == "RAISED"


def test_analyze_reads_the_buffering_facets():
    body = json.dumps({"audio_buffering_ms": "170", "audio_buffering_start_ms": "85",
                       "audio_buffering_source": "pre-fotky video",
                       "audio_buffering_age_s": "6", "audio_mixer_tick_ms": "21.3",
                       "obs_log_head_age_s": "2"})
    res = amd.analyze(body, 1)
    assert res["buffering_verdict"] == "RAISED"
    assert (res["buffering_ms"], res["buffering_start_ms"]) == (170, 85)
    assert (res["buffering_delta_ms"], res["buffering_raised_ticks"]) == (85, 4)
    assert res["buffering_source"] == "pre-fotky video"
    assert res["buffering_age_s"] == 6
    assert amd.analyze(json.dumps({}), 1)["buffering_verdict"] == "UNKNOWN"
    assert amd.analyze("", 0)["buffering_verdict"] == "SKIP"


def test_the_cli_prints_the_buffering_keys():
    body = json.dumps({"audio_buffering_ms": "170", "audio_buffering_start_ms": "85",
                       "audio_buffering_source": "pre-fotky video",
                       "audio_buffering_age_s": "6"})
    out = subprocess.run([sys.executable, str(_SCRIPTS / "audio_mixer_decision.py"), "analyze",
                          "--box-reachable", "1"], input=body, capture_output=True, text=True,
                         check=True).stdout
    for line in ("buffering_verdict=RAISED", "buffering_ms=170", "buffering_start_ms=85",
                 "buffering_delta_ms=85", "buffering_raised_ticks=4",
                 "buffering_source=pre-fotky video", "buffering_age_s=6"):
        assert line in out.splitlines(), out


# ---------------------------------------------------------------------------------------------
# the real bash watchdog through the AUDIO_MIXER_FETCH_CMD seam
# ---------------------------------------------------------------------------------------------
def _body(now_ms, start_ms=85, source="pre-fotky video", age=6, head=2):
    return json.dumps({"audio_mixer_ticks": "2813", "audio_mixer_ticks_over": "0",
                       "audio_mixer_window_ms": "60011", "audio_mixer_tick_ms": "21.3",
                       "audio_mixer_age_s": "20", "obs_log_head_age_s": str(head),
                       "audio_buffering_ms": str(now_ms),
                       "audio_buffering_start_ms": str(start_ms),
                       "audio_buffering_source": source, "audio_buffering_age_s": str(age)})


def _run_watchdog(tmp_path, body, state_file, dry_run=True, notify=None):
    body_file = tmp_path / "body.json"
    body_file.write_text(body, encoding="utf-8")
    fetch = tmp_path / "fetch.sh"
    fetch.write_text(f"#!/usr/bin/env bash\ncat '{body_file}'\n", encoding="utf-8")
    fetch.chmod(fetch.stat().st_mode | stat.S_IEXEC)
    env = dict(os.environ)
    env.update({
        "AUDIO_MIXER_FETCH_CMD": str(fetch),
        "AUDIO_MIXER_BOXES": "stream|10.77.9.204",
        "AUDIO_MIXER_ALERT_STATE_FILE": str(state_file),
        "AIRULESET_NOTIFY": notify or "/nonexistent/airuleset.py",
    })
    args = ["bash", str(_SCRIPTS / "audio-mixer-alert-watchdog.sh")]
    if dry_run:
        args.append("--dry-run")
    r = subprocess.run(args, capture_output=True, text=True, env=env, timeout=60)
    assert r.returncode == 0, r.stderr
    return r.stderr


def test_watchdog_pages_a_raised_buffering_after_two_passes_and_recovers_log_only(tmp_path):
    state = tmp_path / "state"
    first = _run_watchdog(tmp_path, _body(170), state)
    assert "WOULD alert" not in first, first
    assert "buffering=RAISED" in first, first
    second = _run_watchdog(tmp_path, _body(170), state)
    page = [ln for ln in second.splitlines() if "WOULD alert (RAISED)" in ln]
    assert len(page) == 1 and "alert_now=1" in page[0], second
    assert "170 ms" in page[0] and "85 ms" in page[0], page[0]
    assert "pre-fotky video" in page[0] and "reštart" in page[0], page[0]
    # the MIXER arm is untouched by the buffering arm
    assert "WOULD alert (BEHIND)" not in second and "WOULD alert (OVERLOADED)" not in second
    healthy = _run_watchdog(tmp_path, _body(85), state)
    assert "WOULD alert" not in healthy, healthy
    recovery = [ln for ln in healthy.splitlines()
                if "RECOVERY:" in ln and "audio-buffering" in ln]
    assert len(recovery) == 1, healthy
    again = _run_watchdog(tmp_path, _body(85), state)
    assert not [ln for ln in again.splitlines() if "RECOVERY:" in ln], again


def test_watchdog_names_an_unknown_source_honestly(tmp_path):
    state = tmp_path / "state"
    _run_watchdog(tmp_path, _body(170, source=""), state)
    log = _run_watchdog(tmp_path, _body(170, source=""), state)
    page = [ln for ln in log.splitlines() if "WOULD alert (RAISED)" in ln]
    assert len(page) == 1 and "zdroj nie je v logu" in page[0], log


def test_watchdog_stale_between_two_raised_passes_resets_the_confirm(tmp_path):
    state = tmp_path / "state"
    for body in (_body(170), _body(170, head=900), _body(170)):
        log = _run_watchdog(tmp_path, body, state)
        assert "WOULD alert (RAISED)" not in log, log
    log = _run_watchdog(tmp_path, _body(170), state)
    assert "WOULD alert (RAISED)" in log, log


def test_watchdog_unknown_holds_without_a_page(tmp_path):
    state = tmp_path / "state"
    body = json.dumps({"obs_log_head_age_s": "2"})
    for _ in range(3):
        log = _run_watchdog(tmp_path, body, state)
        assert "WOULD alert" not in log, log
    assert "buffering=UNKNOWN" in log


def test_watchdog_sends_a_time_bucketed_key(tmp_path):
    calls = tmp_path / "notify-calls.txt"
    notify = tmp_path / "notify.py"
    notify.write_text(
        "import sys\n"
        f"open({str(calls)!r}, 'a').write(' '.join(sys.argv[1:]) + '\\n')\n",
        encoding="utf-8")
    state = tmp_path / "state"
    for _ in range(2):
        _run_watchdog(tmp_path, _body(170), state, dry_run=False, notify=str(notify))
    sent = calls.read_text(encoding="utf-8").splitlines()
    assert len(sent) == 1, sent
    assert re.search(r"--dedup-key audio-buffering-stream-\d+$", sent[0]), sent[0]
