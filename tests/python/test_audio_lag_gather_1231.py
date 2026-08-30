"""#1231 — unit tests for the FRESHNESS/recency dimension added to the audio-lag facet in
scripts/bundle_state_gather.py (follow-up to the #1226 review finding W1).

Two adjacent gaps the #1226 facet left open (both reproduced live on the ticket):
  (a) a source removed/renamed while LAGGING kept its stale-high last reading winning the MAX;
  (b) telemetry that STOPPED while the OBS log kept advancing looked healthy (no freshness signal).

The new pure `audio_telemetry_from_log(text)` scans the SAME #1222 bounded tail ONCE, returns
`(max_FRESH_lag_str, src, age_s_str)`: it excludes sources whose newest `#800` line sits > a few
emit periods behind the log's newest line of ANY kind (a), and reports the in-log age of the freshest
`#800` line behind the log head so the dev1 decision can surface a STALE state distinctly (b).
`audio_ts_lag_ms_from_log` stays a 2-tuple wrapper (existing call site + #1226 tests unchanged).

In-log RELATIVE recency (the ndi_halving `ts_to_seconds`+wrap precedent) — no clock injection, so
these are pure fixture tests with zero time-mocking. Same "source the PURE parser" split as
test_audio_lag_gather_1226.py / test_bundle_state_gather.py.
"""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402


# (a) mbc lagged huge then STOPPED emitting ~10 min ago; cam keeps emitting fresh; a non-#800 line
# proves the log kept advancing past mbc's last line.
STALE_SOURCE_LOG = (
    "10:00:00.000: audio-telemetry #800 'mbc': ts_lag_ms=999999 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
    "10:09:00.000: [distroav] unrelated line proving the log keeps advancing\n"
    "10:10:00.000: audio-telemetry #800 'cam': ts_lag_ms=150 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
)

# (b) ALL telemetry stopped ~10 min ago; the log still advances with a non-#800 line.
ALL_STALE_LOG = (
    "10:00:00.000: audio-telemetry #800 'mbc': ts_lag_ms=120 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
    "10:00:00.001: audio-telemetry #800 'cam': ts_lag_ms=118 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
    "10:10:00.000: [obs] render tick — the log is alive, telemetry is not\n"
)

# fresh: every source emitted within the last emit period; the newest line IS a #800 line.
FRESH_LOG = (
    "10:44:06.002: audio-telemetry #800 'ASIO Input Capture': ts_lag_ms=107 buffered_ms=0 pending=0 timing_adjust_ms=-5\n"
    "10:44:06.003: audio-telemetry #800 'mbc': ts_lag_ms=118 buffered_ms=0 pending=0 timing_adjust_ms=-5\n"
    "10:44:06.004: audio-telemetry #800 'post video': ts_lag_ms=101 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
)


# ---------------------------------------------------------------- (a) stale-source exclusion
def test_a_stale_removed_source_excluded_from_max():
    # The removed lagging mbc (999999, >10 min behind the log head) must NOT win the max; only the
    # still-emitting fresh cam counts. This is the #1231 (a) fix.
    lag, src = bsg.audio_ts_lag_ms_from_log(STALE_SOURCE_LOG)
    assert lag == "150"
    assert src == "cam"


def test_a_stale_source_excluded_in_full_tuple():
    lag, src, age = bsg.audio_telemetry_from_log(STALE_SOURCE_LOG)
    assert (lag, src) == ("150", "cam")
    # the freshest #800 (cam) IS the newest log line -> age 0 (fresh telemetry present)
    assert age == "0"


# ---------------------------------------------------------------- (b) age / stale-while-advancing
def test_b_all_stale_reports_empty_lag_with_large_age():
    # Every source stopped while the log advanced 10 min: no FRESH positive reading (lag empty), but
    # the age carries the staleness so the dev1 decision can surface STALE distinctly.
    lag, src, age = bsg.audio_telemetry_from_log(ALL_STALE_LOG)
    assert (lag, src) == ("", "")
    assert age == "600"


def test_b_fresh_reports_small_age():
    lag, src, age = bsg.audio_telemetry_from_log(FRESH_LOG)
    assert lag == "118"
    assert src == "mbc"
    assert age == "0"


def test_b_absent_telemetry_returns_empty_everything():
    # No #800 line at all -> ("","","") : absent (stock OBS / none yet) -> UNKNOWN downstream, never a
    # fabricated age or lag.
    assert bsg.audio_telemetry_from_log("nothing relevant here") == ("", "", "")
    assert bsg.audio_telemetry_from_log("") == ("", "", "")
    assert bsg.audio_telemetry_from_log(None) == ("", "", "")


def test_b_summary_only_line_is_absent():
    # The name-less summary line is not a source reading; with no real #800 line, telemetry is absent.
    log = "10:44:06.001: audio-telemetry #800: total_buffering=5000 ms (ticks=3/45) buffering_source=mbc\n"
    assert bsg.audio_telemetry_from_log(log) == ("", "", "")


# ---------------------------------------------------------------- partial-stale + robustness
def test_partial_stale_keeps_fresh_max_and_small_age():
    # One stale (idle removed at high lag) + one fresh: the fresh one wins, age stays small (a fresh
    # #800 exists), so the dev1 decision does the normal lag judgment, never a STALE/UNKNOWN.
    log = (
        "10:00:00.000: audio-telemetry #800 'gone': ts_lag_ms=500000 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
        "10:07:00.000: audio-telemetry #800 'live': ts_lag_ms=300 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
    )
    lag, src, age = bsg.audio_telemetry_from_log(log)
    assert (lag, src) == ("300", "live")
    assert age == "0"


def test_tail_window_only_head_stale_ignored():
    # The #1222 bounded read: only the tail is scanned; a head-slice high value never appears.
    text = (
        "09:00:00.000: audio-telemetry #800 'mbc': ts_lag_ms=888888 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
        + bsg.LOG_BOUNDED_READ_SEPARATOR
        + "10:46:06.003: audio-telemetry #800 'mbc': ts_lag_ms=120 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
    )
    lag, src, age = bsg.audio_telemetry_from_log(text)
    assert (lag, src, age) == ("120", "mbc", "0")


def test_crlf_line_endings_parse_correctly():
    # Windows OBS log is CRLF and the bounded read does NOT translate \r\n -> \n; the timestamp
    # prefix + value must still parse (the \r sits at line end, outside both matches).
    log = (
        "10:00:00.000: audio-telemetry #800 'mbc': ts_lag_ms=999999 buffered_ms=0 pending=0 timing_adjust_ms=0\r\n"
        "10:09:00.000: [distroav] unrelated line, log advancing\r\n"
        "10:10:00.000: audio-telemetry #800 'cam': ts_lag_ms=150 buffered_ms=0 pending=0 timing_adjust_ms=0\r\n"
    )
    lag, src, age = bsg.audio_telemetry_from_log(log)
    assert (lag, src, age) == ("150", "cam", "0")


def test_midnight_wrap_gap_is_bounded_not_a_false_huge_age():
    # A tail straddling midnight: the pre-midnight line has the LARGEST seconds-of-day, so a naive max
    # would compute an implausible ~day-long gap. The wrap/implausible guard must keep the age sane
    # (never a fabricated ~86400 s stale age), the ndi_halving precedent's conservative direction.
    log = (
        "23:59:59.000: audio-telemetry #800 'mbc': ts_lag_ms=118 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
        "00:00:03.000: audio-telemetry #800 'mbc': ts_lag_ms=120 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
    )
    lag, src, age = bsg.audio_telemetry_from_log(log)
    assert lag == "120"
    assert src == "mbc"
    # a bounded small age (~4 s across the wrap), never a ~86400 s false-stale
    assert int(age) < 180


def test_stale_after_s_is_configurable():
    # The staleness bound is tunable; a very large bound keeps even an old source fresh.
    lag, src, age = bsg.audio_telemetry_from_log(STALE_SOURCE_LOG, stale_after_s=100000)
    assert lag == "999999"  # nothing is stale at this bound -> the huge mbc reading is kept
    assert src == "mbc"


# ---------------------------------------------------------------- build_bundle_state age facet
def test_build_bundle_state_includes_age_when_present():
    state = bsg.build_bundle_state(audio_ts_lag_ms="1672741", audio_ts_lag_src="mbc",
                                   audio_ts_lag_age_s="600")
    assert state["audio_ts_lag_age_s"] == "600"


def test_build_bundle_state_omits_age_when_empty():
    state = bsg.build_bundle_state(obs_version="32.1.2")
    assert "audio_ts_lag_age_s" not in state


# ---------------------------------------------------------------- review 🔴: >1h stale + midnight
# The freshness must hold for a stale span > 1h and across midnight — the two regimes the rig enters
# (a quiet log where an old line survives in the 5 MB tail; every midnight). The log is APPEND-ONLY,
# so file order IS time order: the log head is the LAST parseable line, never max-seconds-of-day.
def test_a_source_stale_2h_still_excluded():
    # 'gone' removed ~2h ago while lagging huge; 'live' fresh; the log advanced. gone must NOT
    # re-enter the max as a "wrap artifact" — that is the exact false page (a) exists to close.
    log = (
        "07:00:00.000: audio-telemetry #800 'gone': ts_lag_ms=999999 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
        "08:59:00.000: [obs] unrelated line, log advancing\n"
        "09:00:00.000: audio-telemetry #800 'live': ts_lag_ms=300 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
    )
    assert bsg.audio_telemetry_from_log(log) == ("300", "live", "0")


def test_b_all_stopped_2h_is_stale_not_fake_healthy():
    # All telemetry stopped ~2h ago at a HEALTHY reading; the log kept advancing. This must be STALE
    # (lag empty, large age), NEVER a fake-HEALTHY "120" — the hard-banned outcome.
    log = (
        "07:00:00.000: audio-telemetry #800 'mbc': ts_lag_ms=120 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
        "09:00:00.000: [obs] render tick — the log is alive, telemetry is not\n"
    )
    assert bsg.audio_telemetry_from_log(log) == ("", "", "7200")


def test_b_all_stopped_2h_at_huge_lag_is_stale_not_a_page():
    # Stopped ~2h ago at a HUGE lag: must be STALE (lag empty), never a LAGGING page off dead data.
    log = (
        "07:00:00.000: audio-telemetry #800 'mbc': ts_lag_ms=1672741 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
        "09:00:00.000: [obs] render tick — log alive, telemetry dead\n"
    )
    assert bsg.audio_telemetry_from_log(log) == ("", "", "7200")


def test_stale_across_midnight_is_detected_not_masked():
    # Last #800 at 23:57, log head at 00:30 (past midnight) = genuinely 33 min stale. File-order
    # anchoring + the +86400 wrap correction yield the TRUE gap (1980 s) -> STALE, never a
    # false-fresh "179" from a max-seconds-of-day anchor picking the pre-midnight line.
    log = (
        "23:57:00.000: audio-telemetry #800 'mbc': ts_lag_ms=999999 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
        "00:30:00.000: [obs] render tick, log advancing past midnight\n"
    )
    assert bsg.audio_telemetry_from_log(log) == ("", "", "1980")
