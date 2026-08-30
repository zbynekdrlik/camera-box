"""#1226 — unit tests for the audio-timeline-lag facet added to scripts/bundle_state_gather.py.

The incident (2026-08-30 nedeľná služba): stream OBS's audio pipeline fell ~24 s/min behind
realtime under stream load; `audio-telemetry #800 '<src>': ts_lag_ms=N` (obs-audio.c:698) screamed
the whole hour but nothing read it, so YouTube A/V desynced for a whole service. This facet exposes
the MAX per-source `ts_lag_ms` (from the newest line per source in the TAIL window) through the
existing :8899 bundle-state gather so the dev1 audio-lag watchdog (and rig-status) can see it.

Same "source the PURE parser, verify live separately" split as tests/drift_guard.rs and
test_bundle_state_gather.py — no live OBS / no live box needed.
"""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402


# A real-shaped OBS log tail: several audio sources, all lagging (the incident signature — every
# source lagging equally = a global audio-tick/mix pipeline behind realtime), mbc the highest.
LAGGING_LOG = """\
10:44:06.001: audio-telemetry #800: total_buffering=0 ms (ticks=0/45) buffering_source=-
10:44:06.002: audio-telemetry #800 'ASIO Input Capture': ts_lag_ms=1670120 buffered_ms=0 pending=0 timing_adjust_ms=-5
10:44:06.003: audio-telemetry #800 'mbc': ts_lag_ms=1672741 buffered_ms=0 pending=0 timing_adjust_ms=-5
10:44:06.004: audio-telemetry #800 'post video': ts_lag_ms=1671003 buffered_ms=0 pending=0 timing_adjust_ms=0
10:44:06.005: audio-telemetry #800 'test-audio': ts_lag_ms=1669900 buffered_ms=0 pending=0 timing_adjust_ms=0
"""

HEALTHY_LOG = """\
10:46:06.002: audio-telemetry #800 'ASIO Input Capture': ts_lag_ms=107 buffered_ms=0 pending=0 timing_adjust_ms=-5
10:46:06.003: audio-telemetry #800 'mbc': ts_lag_ms=118 buffered_ms=0 pending=0 timing_adjust_ms=-5
10:46:06.004: audio-telemetry #800 'post video': ts_lag_ms=101 buffered_ms=0 pending=0 timing_adjust_ms=0
"""


def test_reports_max_lag_and_source():
    lag, src = bsg.audio_ts_lag_ms_from_log(LAGGING_LOG)
    assert lag == "1672741"
    assert src == "mbc"


def test_healthy_reports_the_max_small_value():
    lag, src = bsg.audio_ts_lag_ms_from_log(HEALTHY_LOG)
    assert lag == "118"
    assert src == "mbc"


def test_last_value_per_source_wins():
    # A source that recovered: its LATER (most recent) line is the one that counts, even though an
    # earlier line for the same source carried a huge value.
    log = (
        "10:00:00.000: audio-telemetry #800 'mbc': ts_lag_ms=999999 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
        "10:00:00.001: audio-telemetry #800 'cam': ts_lag_ms=200 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
        "10:01:00.000: audio-telemetry #800 'mbc': ts_lag_ms=150 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
    )
    lag, src = bsg.audio_ts_lag_ms_from_log(log)
    assert lag == "200"
    assert src == "cam"


def test_summary_line_without_quoted_source_is_ignored():
    # The `audio-telemetry #800: total_buffering=...` summary line carries no quoted source name and
    # no ts_lag_ms — it must never be parsed as a source reading.
    log = "10:44:06.001: audio-telemetry #800: total_buffering=5000 ms (ticks=3/45) buffering_source=mbc\n"
    assert bsg.audio_ts_lag_ms_from_log(log) == ("", "")


def test_absent_returns_empty_never_fake_zero():
    assert bsg.audio_ts_lag_ms_from_log("nothing relevant here") == ("", "")
    assert bsg.audio_ts_lag_ms_from_log("") == ("", "")
    assert bsg.audio_ts_lag_ms_from_log(None) == ("", "")


def test_negative_one_no_timeline_is_excluded_from_max():
    # ts_lag_ms=-1 means audio_ts==0 (source present but no audio timeline yet) — not a lag. It must
    # never be reported as the max; a real positive reading elsewhere wins.
    log = (
        "10:00:00.000: audio-telemetry #800 'idle': ts_lag_ms=-1 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
        "10:00:00.001: audio-telemetry #800 'mbc': ts_lag_ms=250 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
    )
    lag, src = bsg.audio_ts_lag_ms_from_log(log)
    assert lag == "250"
    assert src == "mbc"


def test_only_negative_readings_report_empty():
    # If every source's newest reading is -1 (no audio timeline anywhere), there is no lag to report.
    log = (
        "10:00:00.000: audio-telemetry #800 'idle1': ts_lag_ms=-1 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
        "10:00:00.001: audio-telemetry #800 'idle2': ts_lag_ms=-1 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
    )
    assert bsg.audio_ts_lag_ms_from_log(log) == ("", "")


def test_tail_window_only_stale_head_high_value_ignored():
    # The #1222 bounded read returns head + LOG_BOUNDED_READ_SEPARATOR + tail. The facet must reflect
    # the CURRENT state (the tail), so a stale HIGH value that only exists in the head slice (an old,
    # recovered episode from the startup region) must NOT be reported.
    text = (
        "09:00:00.000: audio-telemetry #800 'mbc': ts_lag_ms=888888 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
        + bsg.LOG_BOUNDED_READ_SEPARATOR
        + "10:46:06.003: audio-telemetry #800 'mbc': ts_lag_ms=120 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
    )
    lag, src = bsg.audio_ts_lag_ms_from_log(text)
    assert lag == "120"
    assert src == "mbc"


def test_deterministic_tie_break_on_equal_max():
    # Two sources at the same max value -> the reported src is deterministic (alphabetically first),
    # so the facet is stable across requests and never flaps the watchdog's dedup key.
    log = (
        "10:00:00.000: audio-telemetry #800 'zeta': ts_lag_ms=5000 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
        "10:00:00.001: audio-telemetry #800 'alpha': ts_lag_ms=5000 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
    )
    lag, src = bsg.audio_ts_lag_ms_from_log(log)
    assert lag == "5000"
    assert src == "alpha"


def test_build_bundle_state_includes_audio_lag_when_present():
    state = bsg.build_bundle_state(audio_ts_lag_ms="1672741", audio_ts_lag_src="mbc")
    assert state["audio_ts_lag_ms"] == "1672741"
    assert state["audio_ts_lag_src"] == "mbc"


def test_build_bundle_state_omits_audio_lag_when_empty():
    state = bsg.build_bundle_state(obs_version="32.1.2")
    assert "audio_ts_lag_ms" not in state
    assert "audio_ts_lag_src" not in state
