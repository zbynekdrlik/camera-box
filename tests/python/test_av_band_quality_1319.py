"""#1319 Part 2 — the A/V BAND alarm's measurement-QUALITY gate + the dock-native reference.

Owner report 15./16.9.2026: the band alarm paged 78 times overnight on a dock reading that (a)
scattered ±40 ms (MAD 9–31 ms) — as wide as the ±30 ms band — and (b) was judged against the
RECORDING-based residual (−35.3 ms) while the dock's own estimator carried a ≈ +85 ms bias, so
`|85 − (−35.3)| = 120 ms` tripped OUT_OF_BAND every pass. This lane adds:
  * bundle_state_gather.av_offset_quality_from_log — recent-window median MAD + min matched from the
    dock's own `UPDATED/LOCKED offset= … matched=M mad=D` lines (a SEPARATE facet; the offset SERIES
    stays byte-identical per the Part-1 decision).
  * av_step_decision.band_quality_ok + the LOW_QUALITY verdict (log-only, never a page) unless
    mad ≤ 15 ms AND matched ≥ 30 over the recent window.
  * av_step_decision.dock_reference — the quality-gated dock median the [4i/8] persist step records so
    the band compares DOCK-to-DOCK, cancelling the fixed frame bias.
"""
import importlib.util
import json
import os
import pathlib
import subprocess
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"


def _load(name):
    spec = importlib.util.spec_from_file_location(name, _SCRIPTS / f"{name}.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


bsg = _load("bundle_state_gather")
asd = _load("av_step_decision")

_PFX = "[obs-audio-video-sync-dock] av-sync-dock:"


def _ts(h, m, s, ms=0):
    return f"{h:02d}:{m:02d}:{s:02d}.{ms:03d}"


def _upd(h, m, s, off, matched=36, mad=9.0):
    return (f"{_ts(h, m, s)}: {_PFX} UPDATED offset={off}ms source=cluster "
            f"matched={matched} mad={mad}ms")


def _locked(h, m, s, off, matched=36, mad=9.0):
    return (f"{_ts(h, m, s)}: {_PFX} LOCKED offset={off}ms source=cluster "
            f"matched={matched} mad={mad}ms")


# ---------------------------------------------------------------- av_offset_quality_from_log
def test_quality_parser_median_mad_and_min_matched_recent_window():
    # a burst of UPDATED/LOCKED lines in the freshest 10 min: median mad + min matched.
    lines = [_upd(19, 59, 0, 47.0, matched=36, mad=9.0),
             _upd(19, 59, 10, 48.0, matched=31, mad=15.0),
             _locked(19, 59, 20, 47.0, matched=34, mad=11.0),
             "19:59:30.000: something else"]
    mad, mmin = bsg.av_offset_quality_from_log("\n".join(lines) + "\n")
    assert mad == "11.0", mad          # median of [9,15,11]
    assert mmin == "31", mmin          # min matched


def test_quality_parser_absent_when_no_updated_locked_lines():
    lines = [f"{_ts(19, 59, s)}: {_PFX} LOCK-CORRECT SUGGESTED genlock_latency_ms_src 932 -> "
             f"885ms (measured offset=47.0ms) [monitor-only]" for s in range(0, 40, 2)]
    mad, mmin = bsg.av_offset_quality_from_log("\n".join(lines) + "\n")
    assert mad == "" and mmin == ""
    assert bsg.av_offset_quality_from_log("") == ("", "")


def test_quality_parser_excludes_lines_older_than_the_recent_window():
    # an UPDATED burst ~20 min before the head must NOT be counted in the recent window.
    old = [_upd(19, 40, s, 12.0, matched=8, mad=38.0) for s in range(0, 20, 2)]
    fresh = [_upd(20, 0, 0, 47.0, matched=33, mad=10.0)]
    tail = [f"{_ts(20, 0, 5)}: {_PFX} diag locked=yes state=LIVE"]
    mad, mmin = bsg.av_offset_quality_from_log("\n".join(old + fresh + tail) + "\n")
    assert mad == "10.0" and mmin == "33"    # only the fresh line, not the 20-min-old bad burst


# ---------------------------------------------------------------- band_quality_ok
def test_band_quality_ok_three_states():
    assert asd.band_quality_ok(9.0, 36) is True
    assert asd.band_quality_ok(15.0, 30) is True       # boundary: mad<=15, matched>=30
    assert asd.band_quality_ok(15.1, 36) is False      # mad too wide
    assert asd.band_quality_ok(9.0, 29) is False       # too few matched
    assert asd.band_quality_ok(None, 36) is None       # unjudgeable -> None (never LOW_QUALITY)
    assert asd.band_quality_ok(9.0, None) is None


# ---------------------------------------------------------------- classify_av_band + LOW_QUALITY
def _b(recent, ps, nr, dla, reachable=1, ref=0.0, band=30, mn=6, stale=300, mad=None, mmin=None):
    return asd.classify_av_band(recent, ps, nr, dla, reachable, band_reference_ms=ref, band_ms=band,
                                min_samples=mn, stale_threshold_s=stale, recent_mad_ms=mad,
                                recent_matched_min=mmin)


def test_low_quality_suppresses_a_would_be_out_of_band_page():
    # the overnight case: recent_med 85 vs ref -35, |120|>30 => WOULD be OUT_OF_BAND, but MAD 28 and
    # matched 27 fail the quality bar => LOW_QUALITY (log-only, never paged).
    assert _b(85.0, "1", 12, 0, ref=-35.0, band=30, mad=28.0, mmin=27) == "LOW_QUALITY"
    assert _b(85.0, "1", 12, 0, ref=-35.0, band=30, mad=9.0, mmin=27) == "LOW_QUALITY"   # matched<30
    assert _b(85.0, "1", 12, 0, ref=-35.0, band=30, mad=28.0, mmin=36) == "LOW_QUALITY"  # mad>15


def test_good_quality_still_pages_out_of_band():
    assert _b(85.0, "1", 12, 0, ref=-35.0, band=30, mad=9.0, mmin=36) == "OUT_OF_BAND"


def test_absent_quality_proceeds_backward_compatible():
    # no quality facet (older box / no UPDATED line in the window) -> band-judged, page-capable.
    assert _b(85.0, "1", 12, 0, ref=-35.0, band=30, mad=None, mmin=None) == "OUT_OF_BAND"
    assert _b(21.0, "1", 12, 0, ref=0.0, band=30, mad=None, mmin=None) == "IN_BAND"


def test_repin_beats_low_quality():
    # a moved pin is REPIN regardless of quality (the pin-settle hold precedes the quality gate).
    assert _b(85.0, "0", 12, 0, ref=-35.0, band=30, mad=28.0, mmin=27) == "REPIN"


# ---------------------------------------------------------------- analyze_band quality fields
_LOWQ_JSON = json.dumps({
    "av_offset_recent_med_ms": "85.0", "av_offset_pin": "939", "av_offset_pin_stable": "1",
    "av_offset_n_recent": "12", "av_offset_dock_live_age_s": "3",
    "av_offset_recent_mad_ms": "28.0", "av_offset_recent_matched_min": "27",
})


def test_analyze_band_reads_quality_and_returns_low_quality():
    d = asd.analyze_band(_LOWQ_JSON, 1, band_reference_ms=-35.0, band_ms=30)
    assert d["verdict"] == "LOW_QUALITY"
    assert d["recent_mad_ms"] == 28.0 and d["recent_matched_min"] == 27
    assert d["quality_ok"] == 0


def test_cli_analyze_band_low_quality():
    p = subprocess.run([sys.executable, str(_SCRIPTS / "av_step_decision.py"), "analyze-band",
                        "--box-reachable", "1", "--band-reference-ms", "-35", "--band-ms", "30"],
                       input=_LOWQ_JSON.encode(), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    assert p.returncode == 0, p.stderr.decode()
    out = dict(l.split("=", 1) for l in p.stdout.decode().splitlines() if "=" in l)
    assert out["verdict"] == "LOW_QUALITY"
    assert out["recent_mad_ms"] == "28.0" and out["recent_matched_min"] == "27"


# ---------------------------------------------------------------- dock_reference
def test_dock_reference_quality_gated():
    good = json.dumps({"av_offset_recent_med_ms": "40.0", "av_offset_n_recent": "12",
                       "av_offset_pin_stable": "1", "av_offset_recent_mad_ms": "9.0",
                       "av_offset_recent_matched_min": "34"})
    d = asd.dock_reference(good, 1)
    assert d["quality_ok"] == 1 and d["median_ms"] == 40.0 and d["n"] == 12 and d["mad_ms"] == 9.0
    # bad quality -> quality_ok=0 (the persist step records NO dock reference)
    bad = json.dumps({"av_offset_recent_med_ms": "40.0", "av_offset_n_recent": "12",
                      "av_offset_pin_stable": "1", "av_offset_recent_mad_ms": "28.0",
                      "av_offset_recent_matched_min": "27"})
    assert asd.dock_reference(bad, 1)["quality_ok"] == 0
    # unreachable -> quality_ok 0, no parse
    assert asd.dock_reference("garbage", 0)["quality_ok"] == 0


def test_cli_dock_reference():
    good = json.dumps({"av_offset_recent_med_ms": "40.0", "av_offset_n_recent": "12",
                       "av_offset_pin_stable": "1", "av_offset_recent_mad_ms": "9.0",
                       "av_offset_recent_matched_min": "34"})
    p = subprocess.run([sys.executable, str(_SCRIPTS / "av_step_decision.py"), "dock-reference",
                        "--box-reachable", "1"], input=good.encode(),
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    assert p.returncode == 0, p.stderr.decode()
    out = dict(l.split("=", 1) for l in p.stdout.decode().splitlines() if "=" in l)
    assert out["quality_ok"] == "1" and out["median_ms"] == "40.0" and out["n"] == "12"


# ---------------------------------------------------------------- resolve_band_reference dock pref
def test_resolve_band_reference_prefers_dock_native_median(tmp_path):
    wd = _SCRIPTS / "av-step-alert-watchdog.sh"
    f = tmp_path / "resid.json"
    env = dict(os.environ, AV_BAND_REFERENCE_FILE=str(f))
    env.pop("AV_BAND_REFERENCE_MS", None)

    def _resolve():
        p = subprocess.run(["bash", "-c", f'source "{wd}"; resolve_band_reference'],
                           env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        return p.stdout.decode().strip()

    # dock median present -> preferred over the recording residual
    f.write_text(json.dumps({"residual_median_ms": -35.3, "dock_offset_median_ms": 42.0}))
    out = _resolve()
    assert out.split()[0] == "42.0", out
    assert "dock_offset_median_ms" in out
    # dock median absent -> falls back to the recording residual, and SAYS so
    f.write_text(json.dumps({"residual_median_ms": -35.3}))
    out = _resolve()
    assert out.split()[0] == "-35.3" and "residual_median_ms" in out
