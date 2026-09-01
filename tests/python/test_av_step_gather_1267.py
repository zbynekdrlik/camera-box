"""#1267 — unit tests for the box-side av-sync dock measured-offset parser
(scripts/bundle_state_gather.av_offset_series_from_log).

It reads the dock's `LOCK-CORRECT SUGGESTED genlock_latency_ms_src <pin> -> <new>ms (measured
offset=<X>ms)` line (verified LIVE on the stream box 2026-09-02) from the SAME #1222 bounded tail and
summarizes the trend into scalars for the dev1 upstream-step watchdog: recent/base median offset, the
current pin, a pin-stability flag, the in-log freshness age, and per-window sample counts.

Committed `.txt` fixtures (not `.log`, which .gitignore excludes) carry the realistic full-shape
cases; inline strings carry the edges. Pure fixture tests, no clock injection (in-log relative
recency, the #1231 precedent). Same "source the PURE parser" split as test_audio_lag_gather_1231.py.
"""
import pathlib
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402

_FIX = _ROOT / "tests" / "fixtures" / "av-step-1267"


def _suggest(ts, pin, off, verb="SUGGESTED"):
    return (f"{ts}: [obs-audio-video-sync-dock] av-sync-dock: LOCK-CORRECT {verb} "
            f"genlock_latency_ms_src {pin} -> {pin - int(off)}ms (measured offset={off:.1f}ms) "
            f"[monitor-only -- the E2E gate is the only continuous writer]")


# ------------------------------------------------------------------ committed real-shape fixtures
def test_constant_pin_step_fixture():
    txt = (_FIX / "stream-step-constant-pin.txt").read_text(encoding="utf-8")
    recent, base, pin, ps, age, nr, nb = bsg.av_offset_series_from_log(txt)
    assert recent == "8.0" and base == "68.0"        # a -60 ms upstream step
    assert pin == "926" and ps == "1"                 # constant pin across the span
    assert int(age) < 300                             # fresh (dock still emitting)
    assert int(nr) >= 6 and int(nb) >= 6


def test_repin_window_fixture():
    txt = (_FIX / "stream-repin-window.txt").read_text(encoding="utf-8")
    recent, base, pin, ps, age, nr, nb = bsg.av_offset_series_from_log(txt)
    # a large raw offset diff (8 vs 54) BUT the pin moved 976->1024 across the span -> pin_stable "0"
    # (the covariate: never subtract the pin; the dev1 decision reads this as REPIN, not a false STEP).
    assert recent == "8.0" and base == "54.0"
    assert ps == "0" and pin == "1024"


# ------------------------------------------------------------------ absence / edges
def test_empty_text_all_blank():
    assert bsg.av_offset_series_from_log("") == ("", "", "", "", "", "", "")


def test_no_dock_lines_all_blank():
    txt = ("10:00:00.000: [obs] render tick\n"
           "10:00:30.000: audio-telemetry #800 'mbc': ts_lag_ms=120 buffered_ms=0 pending=0 timing_adjust_ms=0\n")
    assert bsg.av_offset_series_from_log(txt) == ("", "", "", "", "", "", "")


def test_requested_actuation_line_also_matches():
    # a future actuation build logs "requested" instead of "SUGGESTED"; the parser matches both.
    lines = [_suggest(f"18:0{m}:00.000", 926, 68.0, verb="requested") for m in range(0, 7)]
    lines.append("18:10:00.000: [obs] head")
    recent, base, pin, ps, age, nr, nb = bsg.av_offset_series_from_log("\n".join(lines) + "\n")
    assert recent == "68.0" and pin == "926" and int(nr) >= 6


def test_other_lock_correct_variants_are_ignored():
    # apply-skipped / read-back / pinned / unavailable lines have no "-> Nms (measured offset=" shape.
    txt = (
        "18:00:00.000: [obs-audio-video-sync-dock] av-sync-dock: LOCK-CORRECT apply skipped (UI thread) -- source 'mbc' not found\n"
        "18:00:30.000: [obs-audio-video-sync-dock] av-sync-dock: LOCK-CORRECT pinned at the hardware floor (3ms) with audio still EARLY by 40.0ms -- cannot correct further\n"
        "18:01:00.000: [obs-audio-video-sync-dock] av-sync-dock: LOCK-CORRECT unavailable -- source 'mbc' not found on this box\n"
        "18:10:00.000: [obs] head\n"
    )
    assert bsg.av_offset_series_from_log(txt) == ("", "", "", "", "", "", "")


def test_locked_updated_line_is_not_the_step_signal():
    # the plain LOCKED/UPDATED offset= line is a DIFFERENT (raw per-tick) line without the pin; the
    # step parser deliberately keys only on the LOCK-CORRECT SUGGESTED/requested line (pin inline).
    txt = ("18:00:00.000: [obs-audio-video-sync-dock] av-sync-dock: LOCKED offset=79.0ms source=cluster matched=8 mad=13.9ms\n"
           "18:00:10.000: [obs-audio-video-sync-dock] av-sync-dock: UPDATED offset=76.9ms source=cluster matched=10 mad=11.6ms\n"
           "18:10:00.000: [obs] head\n")
    assert bsg.av_offset_series_from_log(txt) == ("", "", "", "", "", "", "")


def test_tail_only_a_stale_head_value_is_never_reported():
    # a value surviving ONLY in the head (an old episode) must not be reported — only the tail counts.
    head = _suggest("10:00:00.000", 926, 500.0) + "\n"
    tail = "\n".join(_suggest(f"18:0{m}:00.000", 926, 8.0) for m in range(0, 7)) + "\n18:10:00.000: [obs] head\n"
    txt = head + bsg.LOG_BOUNDED_READ_SEPARATOR + tail
    recent, base, pin, ps, age, nr, nb = bsg.av_offset_series_from_log(txt)
    assert recent == "8.0"           # the 500 ms head value is gone
    assert "500" not in (recent + base)


def test_age_reports_a_stopped_dock_far_behind_the_head():
    # the freshest dock line is ~20 min behind the log's newest line of any kind -> a large age
    # (the dev1 STALE signal). The dock lines here are also all in the baseline window, none recent.
    lines = [_suggest(f"17:4{m}:00.000", 926, 68.0) for m in range(0, 7)]
    lines.append("18:10:00.000: [obs] render tick — head, dock silent for ~20 min")
    recent, base, pin, ps, age, nr, nb = bsg.av_offset_series_from_log("\n".join(lines) + "\n")
    assert int(age) > 300            # freshest dock line ~20+ min behind the head -> STALE downstream
    assert nr == "0"                 # nothing in the last 10 min


def test_median_even_and_odd():
    # odd count -> middle; even -> mean of the two middle values.
    odd = "\n".join(_suggest(f"18:0{i}:00.000", 926, float(v))
                    for i, v in enumerate([10, 20, 30, 40, 50, 60, 70])) + "\n18:10:00.000: [obs] head\n"
    recent, *_ = bsg.av_offset_series_from_log(odd)
    assert recent == "40.0"
    even = "\n".join(_suggest(f"18:0{i}:00.000", 926, float(v))
                     for i, v in enumerate([10, 20, 30, 40, 50, 60])) + "\n18:10:00.000: [obs] head\n"
    recent2, *_ = bsg.av_offset_series_from_log(even)
    assert recent2 == "35.0"


def test_negative_measured_offset_parses():
    lines = [_suggest(f"18:0{m}:00.000", 926, -31.0) for m in range(0, 7)]
    lines.append("18:10:00.000: [obs] head")
    recent, *_ = bsg.av_offset_series_from_log("\n".join(lines) + "\n")
    assert recent == "-31.0"


def test_single_midnight_wrap_across_the_analyzed_span():
    # #1267 review 🟡 (2a): the analyzed span straddles midnight (baseline 23:35-23:43, recent
    # 00:01-00:09, head 00:10). _recency_gap_s corrects a single wrap, so the pre-midnight baseline
    # is aged correctly into the 10-40 min window (file order IS time order, never max seconds-of-day).
    lines = []
    for m in range(35, 44):
        lines.append(_suggest(f"23:{m}:00.000", 926, 68.0))
    for i in range(9):
        lines.append(_suggest(f"00:0{i}:00.000", 926, 8.0))
    lines.append("00:10:00.000: [obs] head — new day")
    recent, base, pin, ps, age, nr, nb = bsg.av_offset_series_from_log("\n".join(lines) + "\n")
    assert recent == "8.0" and base == "68.0"      # the pre-midnight baseline is correctly aged in
    assert pin == "926" and ps == "1"
    assert int(age) < 300                            # freshest dock line ~10 min before the head


def test_pin_change_outside_the_40min_span_keeps_pin_stable():
    # #1267 review 🟡 (2b): a pin change 45 min back (OUTSIDE the 10-40 min baseline window) is
    # dropped from the span, so every WINDOWED sample shares the current pin -> pin_stable "1" (the
    # covariate is judged over the analyzed span only, per the docstring).
    lines = [_suggest(f"17:2{m}:00.000", 976, 55.0) for m in range(0, 5)]   # ~45 min back, old pin, dropped
    for m in range(35, 44):
        lines.append(_suggest(f"17:{m}:00.000", 926, 68.0))                 # baseline, current pin
    for i in range(9):
        lines.append(_suggest(f"18:0{i}:00.000", 926, 8.0))                 # recent, current pin
    lines.append("18:10:00.000: [obs] head")
    recent, base, pin, ps, age, nr, nb = bsg.av_offset_series_from_log("\n".join(lines) + "\n")
    assert ps == "1" and pin == "926"               # the out-of-span 976 samples never touch pin_stable
    assert recent == "8.0" and base == "68.0"


def test_constant_raw_offset_across_a_pin_jump_is_pin_unstable():
    # #1267 review 🔵 (7): the DOCUMENTED live shape — the pin jumps 976->1024 while the raw measured
    # offset stays ~constant (offset+pin ~ const), so `offset - pin` would read a phantom -48 step.
    # The parser instead reports pin_stable "0" (never subtract the pin) -> the dev1 decision REPINs.
    lines = [_suggest(f"17:{m}:00.000", 976, 50.0) for m in range(35, 44)]  # baseline pin 976, offset 50
    lines.append("18:00:00.000: [obs] E2E snapshot-and-set test latency")
    for i in range(9):
        lines.append(_suggest(f"18:0{i}:00.000", 1024, 50.0))              # recent pin 1024, SAME offset 50
    lines.append("18:10:00.000: [obs] head")
    recent, base, pin, ps, age, nr, nb = bsg.av_offset_series_from_log("\n".join(lines) + "\n")
    assert ps == "0" and pin == "1024"              # pin moved across the span -> unstable
    assert recent == "50.0" and base == "50.0"      # raw offset unchanged (offset - pin would be a phantom step)
