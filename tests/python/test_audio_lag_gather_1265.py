"""#1265 — the per-REFERENCE-source `ts_lag_ms` BAND parser added to scripts/bundle_state_gather.py
(`audio_ref_band_from_log`), the box-side half of the tens-of-ms band watch.

The #1226/#1231 facet exposes only a single MAX-across-sources scalar at a 5000 ms page threshold,
which is structurally blind to the A/V-gate reference source (`mbc`) going BIMODAL (flat ~107 ms then
flapping 107↔180 ms, high mode creeping up) — a 23×-under-threshold drift that still shifts the A/V
residual past the ±90 gate. This parser reads the SAME #1222 bounded head+tail log ONCE and, for the
named reference source, computes the band SHAPE: `base_ms` (flat-start baseline = median of the HEAD
startup region), `high_ms`/`low_ms` (p90/p10 of the FRESH tail window), `duty_pct` (% of the tail
window above baseline+margin), `n` (tail window sample count). The dev1 decision (test_audio_lag_
decision_1265) thresholds these; both are pure, pytest Tier-0 (#557 kills cargo).

Fixture `fixtures/audio_ref_band_mbc_1265.log` is a small anonymized sample of the real 2026-09-01
old-instance shape: a flat ~107 ms head, the #1222 separator, then the bimodal 107↔180 ms tail flap.
"""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402

_FIX = pathlib.Path(__file__).resolve().parent / "fixtures" / "audio_ref_band_mbc_1265.txt"


def _fixture_text():
    return _FIX.read_text()


# ------------------------------------------------------------------ the real bimodal fixture
def test_band_from_real_bimodal_fixture():
    # (src, base_ms, high_ms, low_ms, duty_pct, n) — all strings, omit-when-empty contract.
    src, base, high, low, duty, n = bsg.audio_ref_band_from_log(_fixture_text(), ref_src="mbc")
    assert src == "mbc"
    assert base == "107"          # flat-start baseline = median of the HEAD region readings
    assert high == "181"          # p90 of the fresh tail window (the high mode)
    assert low == "107"           # p10 of the fresh tail window (the low mode persists)
    assert duty == "50"           # ~half the recent window sits above baseline+margin (bimodal)
    assert n == "14"              # fresh tail-window sample count


def test_other_sources_never_contaminate_the_ref_band():
    # The fixture interleaves 'ASIO Input Capture' / 'post video' readings; the band is mbc-only.
    _src, base, high, low, _duty, n = bsg.audio_ref_band_from_log(_fixture_text(), ref_src="mbc")
    # If ASIO's 133 or post-video's 170 leaked in, low/base/high would move; they must not.
    assert (base, high, low, n) == ("107", "181", "107", "14")


# ------------------------------------------------------------------ flat window = no drift
def test_flat_window_has_no_spread():
    head = "\n".join(f"05:{m:02d}:00.000: audio-telemetry #800 'mbc': ts_lag_ms=107 buffered_ms=85 pending=0 timing_adjust_ms=0" for m in range(16, 24))
    tail = "\n".join(f"21:{m:02d}:00.000: audio-telemetry #800 'mbc': ts_lag_ms={107 + (m % 2)} buffered_ms=85 pending=0 timing_adjust_ms=0" for m in range(0, 12))
    text = head + "\n" + bsg.LOG_BOUNDED_READ_SEPARATOR + tail + "\n"
    src, base, high, low, duty, n = bsg.audio_ref_band_from_log(text, ref_src="mbc")
    assert src == "mbc"
    assert int(high) - int(low) <= 5     # essentially no band
    assert duty == "0"                   # nothing above baseline+margin


# ------------------------------------------------------------------ a single spike is not a band
def test_single_spike_does_not_widen_the_band():
    # 13 flat 107s + one 300 spike: p90 stays flat, duty stays tiny -> not a drift.
    vals = [107] * 13 + [300]
    tail = "\n".join(f"21:{i:02d}:00.000: audio-telemetry #800 'mbc': ts_lag_ms={v} buffered_ms=85 pending=0 timing_adjust_ms=0" for i, v in enumerate(vals))
    text = bsg.LOG_BOUNDED_READ_SEPARATOR + tail + "\n"
    _src, base, high, low, duty, n = bsg.audio_ref_band_from_log(text, ref_src="mbc")
    assert base == ""                    # no head region (separator at very start)
    assert high == "107"                 # p90 robust to the single top outlier
    assert low == "107"
    assert int(duty) < 10                # one sample is not a duty cycle
    assert n == "14"


# ------------------------------------------------------------------ small whole log (no separator)
def test_small_whole_log_has_no_head_baseline_but_still_windows():
    tail = "\n".join(f"21:{i:02d}:00.000: audio-telemetry #800 'mbc': ts_lag_ms={107 if i % 2 else 180} buffered_ms=85 pending=0 timing_adjust_ms=0" for i in range(10))
    src, base, high, low, duty, n = bsg.audio_ref_band_from_log(tail + "\n", ref_src="mbc")
    assert src == "mbc"
    assert base == ""                    # no separator -> no distinct startup region
    assert int(high) >= 180 and int(low) <= 108   # the bimodal window still shows the two modes
    assert n == "10"


# ------------------------------------------------------------------ absent / negatives / empty
def test_absent_ref_source_is_all_empty():
    text = "21:00:00.000: audio-telemetry #800 'ASIO Input Capture': ts_lag_ms=133 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
    assert bsg.audio_ref_band_from_log(text, ref_src="mbc") == ("", "", "", "", "", "")


def test_empty_and_none_are_all_empty():
    assert bsg.audio_ref_band_from_log("", ref_src="mbc") == ("", "", "", "", "", "")
    assert bsg.audio_ref_band_from_log(None, ref_src="mbc") == ("", "", "", "", "", "")


def test_negative_readings_excluded_from_the_band():
    # ts_lag_ms=-1 (no audio timeline yet) never contributes.
    tail = (
        "21:00:00.000: audio-telemetry #800 'mbc': ts_lag_ms=-1 buffered_ms=0 pending=0 timing_adjust_ms=0\n"
        + "\n".join(f"21:{i+1:02d}:00.000: audio-telemetry #800 'mbc': ts_lag_ms=107 buffered_ms=85 pending=0 timing_adjust_ms=0" for i in range(9))
    )
    _src, _base, high, low, _duty, n = bsg.audio_ref_band_from_log(tail + "\n", ref_src="mbc")
    assert n == "9"                      # the -1 dropped, 9 real readings remain
    assert high == "107" and low == "107"


# ------------------------------------------------------------------ build_bundle_state wiring
def test_build_bundle_state_emits_band_facets_when_present():
    st = bsg.build_bundle_state(
        audio_ref_lag_src="mbc", audio_ref_lag_base_ms="107", audio_ref_lag_high_ms="181",
        audio_ref_lag_low_ms="107", audio_ref_lag_duty_pct="50", audio_ref_lag_n="14",
    )
    assert st["audio_ref_lag_src"] == "mbc"
    assert st["audio_ref_lag_base_ms"] == "107"
    assert st["audio_ref_lag_high_ms"] == "181"
    assert st["audio_ref_lag_low_ms"] == "107"
    assert st["audio_ref_lag_duty_pct"] == "50"
    assert st["audio_ref_lag_n"] == "14"


def test_build_bundle_state_omits_band_facets_when_empty():
    st = bsg.build_bundle_state()
    for k in ("audio_ref_lag_src", "audio_ref_lag_base_ms", "audio_ref_lag_high_ms",
              "audio_ref_lag_low_ms", "audio_ref_lag_duty_pct", "audio_ref_lag_n"):
        assert k not in st, f"{k} must be omit-when-empty"
