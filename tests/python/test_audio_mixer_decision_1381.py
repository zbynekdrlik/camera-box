"""issue 1381 -- the PURE dev1 decision core of the audio-mixer / VBAN-loss pager
(scripts/audio_mixer_decision.py).

MIXER arm (the `audio_mixer_*` facets, one `audio-stall #1367` dump window):
  BEHIND      -- ticks per minute more than 5 off real time (2812.5 at 48 kHz). Surplus pages
                 too: the 27.9 catch-up bursts read 3857 ticks/min.
  OVERLOADED  -- more than 30 ticks per minute arrived late (gap > 1.5 ticks).
  STALE       -- the newest dump sits > 180 s behind the log head (log-only, never a page).
  SKIP        -- :8899 not fetched (deferred to the reach / bundle-state watchdogs).
  UNKNOWN     -- facet absent (a normal OBS start has one partial dump only) or ungradable.
VBAN arm (the `vban_pacer_*` facets):
  VBAN_LOSS   -- a loss counter moved inside the gather's window (events > 0 or loss ms > 0).
  STALE / SKIP / UNKNOWN / HEALTHY as above.

No I/O -- Tier-0 pytest (no cargo).
"""
import json
import pathlib
import subprocess
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import audio_mixer_decision as amd  # noqa: E402


def _mixer(ticks=2813, over=0, window_ms=60011, tick_ms="21.3", age_s=0, reachable=1, **kw):
    return amd.classify_mixer(ticks, over, window_ms, tick_ms, age_s, reachable, **kw)["verdict"]


# ── expected rate ─────────────────────────────────────────────────────────────────────────────
def test_expected_rate_from_tick_ms():
    assert amd.expected_ticks_per_min("21.3") == 2812.5
    assert abs(amd.expected_ticks_per_min("23.2") - 60 * 44100 / 1024) < 1e-9
    assert amd.expected_ticks_per_min("17.0") is None
    assert amd.expected_ticks_per_min("") is None
    assert amd.expected_ticks_per_min(None) is None


# ── MIXER arm ─────────────────────────────────────────────────────────────────────────────────
def test_real_time_windows_are_healthy():
    # The live clean reads: 2813 / 2814 ticks in 60.01-60.03 s, ticks_over 0-2.
    assert _mixer(2813, 0, 60011) == "HEALTHY"
    assert _mixer(2814, 0, 60033) == "HEALTHY"
    assert _mixer(2813, 2, 60012) == "HEALTHY"


def test_behind_real_time_pages():
    # 06:09 on 27.9: 2760 ticks in 60.014 s -> 52.8 ticks/min short.
    assert _mixer(2760, 797, 60014) == "BEHIND"
    # the later deep deficit 1753-2201 ticks/min
    assert _mixer(1753, 900, 60020) == "BEHIND"


def test_surplus_ticks_page_too():
    # 08:00 on 27.9: the catch-up burst, 3857 ticks in 60.024 s.
    assert _mixer(3857, 222, 60024) == "BEHIND"
    assert _mixer(2868, 646, 60017) == "BEHIND"


def test_deviation_threshold_is_five_ticks_per_minute():
    assert _mixer(2817, 0, 60000) == "HEALTHY"     # +4.5
    assert _mixer(2818, 0, 60000) == "BEHIND"      # +5.5
    assert _mixer(2807, 0, 60000) == "BEHIND"      # -5.5
    assert _mixer(2818, 0, 60000, tolerance=6.0) == "HEALTHY"


def test_overloaded_pages_above_thirty_late_ticks_per_minute():
    # 06:00 read 22 (below), 06:01 read 36 (the onset) at a real-time tick count.
    assert _mixer(2813, 22, 60010) == "HEALTHY"
    assert _mixer(2813, 36, 60025) == "OVERLOADED"
    assert _mixer(2813, 30, 60000) == "HEALTHY"    # exactly 30/min is not above
    assert _mixer(2813, 31, 60000) == "OVERLOADED"


def test_behind_wins_over_overloaded():
    assert _mixer(2760, 797, 60014) == "BEHIND"


def test_the_count_is_graded_as_dumped_never_rescaled_by_the_log_interval():
    # The dump window is 60 s on the audio thread's own disciplined clock (dumped by the first
    # callback past 60 s), so its tick count already IS the per-minute rate. The log timestamps are
    # the wall clock, which dantesync steps: the real 27.9 log shows 03:59:13.844 -> 04:00:14.037
    # (60.193 s) around the 02:00 UTC nightly date step with ticks=2813. Rescaling by that interval
    # read 2804/min -> a false BEHIND; the count itself is real time.
    assert _mixer(2813, 0, 60193) == "HEALTHY"
    assert _mixer(2813, 0, 57000) == "HEALTHY"
    # A genuine stall still shows in the count (fewer ticks in the window, a surplus in the next).
    assert _mixer(2789, 0, 60400) == "BEHIND"
    assert _mixer(2836, 0, 60000) == "BEHIND"


def test_stale_dump_is_not_a_page():
    assert _mixer(2813, 0, 60011, age_s=181) == "STALE"
    assert _mixer(2760, 797, 60014, age_s=400) == "STALE"   # stale is decided first
    assert _mixer(2813, 0, 60011, age_s=180) == "HEALTHY"


def test_mixer_absent_or_ungradable_is_unknown():
    assert _mixer(None, None, None, None, None) == "UNKNOWN"
    assert _mixer(None, 0, 60011) == "UNKNOWN"
    assert _mixer(2813, None, 60011) == "UNKNOWN"
    assert _mixer(2813, 0, 60011, tick_ms="17.0") == "UNKNOWN"


def test_mixer_unreachable_is_skip():
    assert _mixer(2760, 797, 60014, reachable=0) == "SKIP"


def test_mixer_reports_the_rates_it_graded():
    r = amd.classify_mixer(2760, 797, 60014, "21.3", 5, 1)
    assert r["rate_per_min"] == 2760
    assert r["deviation_per_min"] == -52.5
    assert r["over_per_min"] == 797
    assert r["expected_per_min"] == 2812.5


# ── VBAN arm ─────────────────────────────────────────────────────────────────────────────────
def test_vban_loss_pages_on_any_moved_counter():
    assert amd.classify_vban(3, None, 0, 1) == "VBAN_LOSS"
    assert amd.classify_vban(0, 254.0, 0, 1) == "VBAN_LOSS"
    assert amd.classify_vban(1, 0.0, 0, 1) == "VBAN_LOSS"


def test_vban_clean_is_healthy():
    assert amd.classify_vban(0, None, 0, 1) == "HEALTHY"
    assert amd.classify_vban(0, 0.0, 3, 1) == "HEALTHY"


def test_vban_loss_is_decided_before_stale():
    # Loss inside the window is real even if the output then stopped logging.
    assert amd.classify_vban(4, None, 400, 1) == "VBAN_LOSS"
    assert amd.classify_vban(0, None, 400, 1) == "STALE"


def test_vban_absent_is_unknown_and_unreachable_is_skip():
    assert amd.classify_vban(None, None, None, 1) == "UNKNOWN"
    assert amd.classify_vban(5, None, 0, 0) == "SKIP"


# ── analyze + CLI ────────────────────────────────────────────────────────────────────────────
def _body(**facets):
    return json.dumps(facets)


def test_analyze_reads_both_arms():
    body = _body(audio_mixer_ticks="2760", audio_mixer_ticks_over="797",
                 audio_mixer_window_ms="60014", audio_mixer_tick_ms="21.3", audio_mixer_age_s="5",
                 vban_pacer_loss_events="44", vban_pacer_loss_dest="stream=cg",
                 vban_pacer_outputs="2", vban_pacer_age_s="0")
    r = amd.analyze(body, 1)
    assert r["mixer_verdict"] == "BEHIND"
    assert r["vban_verdict"] == "VBAN_LOSS"
    assert r["vban_events"] == 44
    assert r["vban_loss_ms"] is None
    assert r["vban_dest"] == "stream=cg"


def test_analyze_absent_facets_are_unknown():
    r = amd.analyze(_body(obs_version="32.1.2"), 1)
    assert r["mixer_verdict"] == "UNKNOWN"
    assert r["vban_verdict"] == "UNKNOWN"


def test_analyze_unreachable_skips_without_parsing():
    r = amd.analyze("not json", 0)
    assert r["mixer_verdict"] == "SKIP"
    assert r["vban_verdict"] == "SKIP"


def test_analyze_tolerates_a_non_json_body():
    r = amd.analyze("<html>", 1)
    assert r["mixer_verdict"] == "UNKNOWN"
    assert r["vban_verdict"] == "UNKNOWN"


def test_cli_prints_key_value_lines():
    body = _body(audio_mixer_ticks="3857", audio_mixer_ticks_over="222",
                 audio_mixer_window_ms="60024", audio_mixer_tick_ms="21.3", audio_mixer_age_s="0",
                 vban_pacer_loss_events="0", vban_pacer_loss_ms="0.0",
                 vban_pacer_loss_dest="10.77.7.106:6980", vban_pacer_outputs="2",
                 vban_pacer_age_s="0")
    out = subprocess.run([sys.executable, str(_SCRIPTS / "audio_mixer_decision.py"), "analyze",
                          "--box-reachable", "1"], input=body, capture_output=True, text=True,
                         check=True).stdout
    kv = dict(line.split("=", 1) for line in out.strip().splitlines())
    assert kv["mixer_verdict"] == "BEHIND"
    assert kv["rate_per_min"] == "3857.0"
    assert kv["deviation_per_min"] == "+1044.5"
    assert kv["vban_verdict"] == "HEALTHY"
    assert kv["vban_dest"] == "10.77.7.106:6980"


def test_cli_thresholds_are_overridable():
    body = _body(audio_mixer_ticks="2820", audio_mixer_ticks_over="40",
                 audio_mixer_window_ms="60000", audio_mixer_tick_ms="21.3", audio_mixer_age_s="0")
    out = subprocess.run([sys.executable, str(_SCRIPTS / "audio_mixer_decision.py"), "analyze",
                          "--box-reachable", "1", "--tolerance", "10", "--over-max", "50"],
                         input=body, capture_output=True, text=True, check=True).stdout
    assert "mixer_verdict=HEALTHY" in out


def test_cli_unreachable_needs_no_stdin():
    out = subprocess.run([sys.executable, str(_SCRIPTS / "audio_mixer_decision.py"), "analyze",
                          "--box-reachable", "0"], input="", capture_output=True, text=True,
                         check=True).stdout
    assert "mixer_verdict=SKIP" in out
    assert "vban_verdict=SKIP" in out
