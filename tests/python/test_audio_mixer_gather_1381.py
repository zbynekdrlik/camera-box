"""issue 1381 -- the box-side bundle-state facets for the audio-mixer / VBAN-loss pager.

On 27.9.2026 the resolume cg OBS audio mixer left real time from 06:00 local and both obs-vban
outputs lost audio for over an hour before a human heard it at FOH. Both signals were already in
the OBS log; these facets expose them on :8899 from the SAME #1222 bounded read (tail only, no
second scan):

  * `audio_mixer_from_log` -- the newest `audio-stall #1367:` line (one per >= 60 s dump window,
    emitted by vendor/obs-studio/libobs/obs-audio.c) -> ticks / ticks_over / tick_ms, the window
    length (interval to the previous stall line) and the in-log age. The first dump after an OBS
    start is partial (ticks=1), so the facet needs TWO lines -- a normal start reads absent.
  * `vban_pacer_loss_from_log` -- the obs-vban `obs-vban pacing:` 10 s status lines, BOTH formats:
    the shipped 1372 line (underflows / overflows / trims, no destination -- two outputs print
    identical-looking lines) and the fixed-timeline pacer line (discontinuities / repays / resyncs /
    silence_ms / discarded_ms + dest=ip:port). The counters are cumulative, so the facet is the
    per-destination INCREASE inside the last window of the log: a counter tuple never seen before
    that dominates one seen earlier. `late_sends` is not a loss.

Pure parsers, Tier-0 pytest (no cargo).
"""
import importlib.util
import json
import pathlib
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402

SEP = bsg.LOG_BOUNDED_READ_SEPARATOR


def _stall(ts, ticks, over, gap="25.0", cb="18.0", tick_ms="21.3"):
    return (f"{ts}: audio-stall #1367: tick_gap_max_ms={gap} callback_max_ms={cb} "
            f"ticks={ticks} ticks_over={over} tick_ms={tick_ms}")


def _legacy(ts, u, o, t, late="3.000", depth="65.5", stream="cg"):
    return (f"{ts}: [obs-vban] obs-vban pacing: depth_ms={depth} underflows={u} overflows={o} "
            f"late_max_ms={late} trims={t} target_ms=64 stream='{stream}'")


def _new(ts, dest, disc=0, repays=0, resyncs=0, silence="0.0", discarded="0.0", late_sends=0):
    return (f"{ts}: [obs-vban] obs-vban pacing: depth_ms=64.0 late_sends={late_sends} "
            f"discontinuities={disc} repays={repays} silence_ms={silence} discarded_ms={discarded} "
            f"resyncs={resyncs} late_max_ms=3.000 target_ms=64 dest={dest} stream='cg'")


def _cfg(ts):
    return (f"{ts}: [obs-vban] obs-vban pacing-config: target_ms=64 packet_samples=239 rate=48000 "
            f"counters=reset stream='cg'")


def _text(lines, crlf=False):
    nl = "\r\n" if crlf else "\n"
    return nl.join(lines) + nl


# ---------------------------------------------------------------------------------------------
# audio_mixer_from_log
# ---------------------------------------------------------------------------------------------
def test_mixer_reads_the_newest_stall_line_with_its_window_and_age():
    text = _text([
        _stall("06:08:15.416", 2812, 641),
        _stall("06:09:15.430", 2760, 797),
        "06:09:20.100: some other line",
    ])
    ticks, over, window_ms, tick_ms, age = bsg.audio_mixer_from_log(text)
    assert (ticks, over, tick_ms) == ("2760", "797", "21.3")
    assert window_ms == "60014"
    assert age == "5"


def test_mixer_first_line_after_an_obs_start_is_absent_not_behind():
    # The first dump after start covers one tick (ticks=1) -- a partial window. One line alone
    # cannot be graded, so the facet is omitted (UNKNOWN downstream), never a false BEHIND.
    text = _text(["19:49:08.001: OBS 32.1.2 (64-bit, windows)",
                  _stall("19:49:10.146", 1, 0, gap="0.0", cb="0.0"),
                  "19:49:13.662: [obs-vban] obs-vban pacing-config: target_ms=64"])
    assert bsg.audio_mixer_from_log(text) == ("", "", "", "", "")


def test_mixer_absent_without_any_stall_line():
    assert bsg.audio_mixer_from_log("") == ("", "", "", "", "")
    assert bsg.audio_mixer_from_log(None) == ("", "", "", "", "")
    assert bsg.audio_mixer_from_log(_text(["10:00:00.000: OBS 32.1.2"])) == ("", "", "", "", "")


def test_mixer_scans_the_tail_only():
    # A stall line that survives only in the HEAD slice is never reported (current state only).
    head = _text([_stall("16:35:05.904", 1, 0), _stall("16:36:05.917", 2000, 900)])
    tail = _text([_stall("20:00:10.300", 2813, 0), _stall("20:01:10.311", 2813, 1)])
    ticks, over, window_ms, _tick, _age = bsg.audio_mixer_from_log(head + SEP + tail)
    assert (ticks, over, window_ms) == ("2813", "1", "60011")


def test_mixer_window_crosses_midnight():
    text = _text([_stall("23:59:40.000", 2813, 0), _stall("00:00:40.020", 2813, 0)])
    _t, _o, window_ms, _tick, age = bsg.audio_mixer_from_log(text)
    assert window_ms == "60020"
    assert age == "0"


def test_mixer_age_is_anchored_to_the_last_line_in_file_order():
    # The log head is the LAST parseable line in file order, never the max seconds-of-day: across
    # midnight a max-anchor would read a pre-midnight line as the head.
    text = _text([
        _stall("23:58:00.000", 2813, 0),
        _stall("23:59:00.000", 2813, 0),
        "23:59:59.000: filler",
        "00:03:00.000: the log kept advancing after midnight",
    ])
    _t, _o, _w, _tick, age = bsg.audio_mixer_from_log(text)
    assert age == "240"


def test_mixer_tolerates_crlf_line_endings():
    text = _text([_stall("05:59:15.314", 2813, 0), _stall("06:00:15.324", 2813, 22)], crlf=True)
    ticks, over, window_ms, tick_ms, _age = bsg.audio_mixer_from_log(text)
    assert (ticks, over, window_ms, tick_ms) == ("2813", "22", "60010", "21.3")


# ---------------------------------------------------------------------------------------------
# vban_pacer_loss_from_log -- the shipped (legacy) format: two outputs, identical lines, no dest
# ---------------------------------------------------------------------------------------------
def _pairs(start_s, n, tuples_a, tuples_b, step=10.0, skew=0.04):
    """n pacing periods; each period logs output A then output B (skew seconds apart)."""
    out = []
    for i in range(n):
        s = start_s + i * step
        out.append(_legacy(_hms(s), *tuples_a(i)))
        out.append(_legacy(_hms(s + skew), *tuples_b(i)))
    return out


def _hms(sec):
    sec = sec % 86400
    h = int(sec // 3600)
    m = int((sec % 3600) // 60)
    s = sec - h * 3600 - m * 60
    return f"{h:02d}:{m:02d}:{s:06.3f}"


def test_legacy_two_clean_outputs_read_zero_loss():
    lines = _pairs(3 * 3600, 90, lambda i: (1, 0, 1), lambda i: (1, 0, 1))
    events, loss_ms, dest, age = bsg.vban_pacer_loss_from_log(_text(lines))
    assert events == "0"
    assert loss_ms == ""          # the legacy line carries no ms counters
    assert dest == "stream=cg"
    assert age == "0"


def test_legacy_counters_that_differ_between_outputs_but_never_move_read_zero():
    # A: 44/13/9, B: 45/13/9 -- both stay put. Interleaving must never fake a loss.
    lines = _pairs(3 * 3600, 90, lambda i: (44, 13, 9), lambda i: (45, 13, 9))
    events, _ms, _dest, _age = bsg.vban_pacer_loss_from_log(_text(lines))
    assert events == "0"


@pytest.mark.parametrize("skew", [0.04, 6.0, 9.0, 9.9])
@pytest.mark.parametrize("first,second", [((1, 0, 1), (45, 13, 9)), ((45, 13, 9), (1, 0, 1))])
def test_legacy_phase_offset_outputs_never_read_loss(skew, first, second):
    # Review round 1: two clean outputs whose lines are several seconds apart (an output restart
    # moves its logging phase), from the very start of the tail. The second output's first line
    # is that output's own value, never a loss of the first output. 40 periods (400 s) keep the
    # tail's first lines inside the 660 s loss window.
    lines = _pairs(6 * 3600, 40, lambda i: first, lambda i: second, skew=skew)
    events, _ms, _dest, _age = bsg.vban_pacer_loss_from_log(_text(lines))
    assert events == "0"


@pytest.mark.parametrize("skew", [0.04, 6.0, 9.0])
def test_legacy_common_logging_pause_never_reads_loss(skew):
    # Review round 1: both outputs stop logging for 40 s (OBS frozen), then resume unchanged.
    before = _pairs(6 * 3600, 20, lambda i: (1, 0, 1), lambda i: (45, 13, 9), skew=skew)
    after = _pairs(6 * 3600 + 240, 20, lambda i: (1, 0, 1), lambda i: (45, 13, 9), skew=skew)
    events, _ms, _dest, _age = bsg.vban_pacer_loss_from_log(_text(before + after))
    assert events == "0"


def test_legacy_loss_during_a_common_pause_is_counted():
    # The same pause, but output A lost audio meanwhile (2 underflows): counted once it resumes.
    before = _pairs(6 * 3600, 20, lambda i: (1, 0, 1), lambda i: (45, 13, 9), skew=6.0)
    after = _pairs(6 * 3600 + 240, 20, lambda i: (3, 0, 1), lambda i: (45, 13, 9), skew=6.0)
    events, _ms, _dest, _age = bsg.vban_pacer_loss_from_log(_text(before + after))
    assert events == "2"


def test_legacy_loss_inside_the_window_is_summed_over_both_outputs():
    # 60 periods (600 s) clean at 1/0/1, then A gains 1 underflow + 1 trim, B 1 underflow.
    def a(i):
        return (1, 0, 1) if i < 60 else (2, 0, 2)

    def b(i):
        return (1, 0, 1) if i < 60 else (2, 0, 1)
    lines = _pairs(6 * 3600, 70, a, b)
    events, _ms, _dest, _age = bsg.vban_pacer_loss_from_log(_text(lines))
    assert events == "3"


def test_legacy_loss_older_than_the_window_is_not_reported():
    # The 18:05 underflow on 26.9: a single step, then 1/0/1 for hours -> no loss in the window.
    def a(i):
        return (0, 0, 0) if i < 5 else (1, 0, 1)
    lines = _pairs(18 * 3600, 200, a, a)   # 2000 s of lines, the step at +50 s
    events, _ms, _dest, _age = bsg.vban_pacer_loss_from_log(_text(lines))
    assert events == "0"


def test_legacy_loss_on_one_of_two_outputs_is_counted():
    # A = 44 then +1 underflow per period for 10 periods, B = 90 fixed: 10 underflows. The loss
    # starts after the tail's first logging period, which only seeds the known counters.
    def a(i):
        return (44 + min(max(i - 1, 0), 10), 13, 9)
    lines = _pairs(6 * 3600, 30, a, lambda i: (90, 13, 9))
    events, _ms, _dest, _age = bsg.vban_pacer_loss_from_log(_text(lines))
    assert events == "10"


def test_legacy_output_restart_resets_counters_without_a_false_loss():
    # An output restart (pacing-config counters=reset) drops its counters back to 0.
    lines = _pairs(6 * 3600, 10, lambda i: (7, 2, 3), lambda i: (7, 2, 3))
    lines.append(_cfg("06:01:41.000"))
    lines.append(_cfg("06:01:41.004"))
    lines += _pairs(6 * 3600 + 111, 10, lambda i: (0, 0, 0), lambda i: (0, 0, 0))
    events, _ms, _dest, _age = bsg.vban_pacer_loss_from_log(_text(lines))
    assert events == "0"


def test_pacing_config_line_is_not_a_status_line():
    text = _text([_cfg("19:49:13.662"), _cfg("19:49:13.664")])
    assert bsg.vban_pacer_loss_from_log(text) == ("", "", "", "")


def test_vban_absent_without_pacing_lines():
    assert bsg.vban_pacer_loss_from_log("") == ("", "", "", "")
    assert bsg.vban_pacer_loss_from_log(_text(["10:00:00.000: OBS 32.1.2"])) == ("", "", "", "")


def test_vban_scans_the_tail_only():
    head = _text(_pairs(3600, 30, lambda i: (i, 0, 0), lambda i: (i, 0, 0)))
    tail = _text(_pairs(7200, 30, lambda i: (99, 0, 0), lambda i: (99, 0, 0)))
    events, _ms, _dest, _age = bsg.vban_pacer_loss_from_log(head + SEP + tail)
    assert events == "0"


def test_vban_age_is_the_newest_pacing_line_behind_the_log_head():
    lines = _pairs(6 * 3600, 5, lambda i: (1, 0, 1), lambda i: (1, 0, 1))
    lines.append("06:05:00.000: the log kept advancing")
    _e, _ms, _dest, age = bsg.vban_pacer_loss_from_log(_text(lines))
    assert age == "260"


def test_vban_lines_older_than_the_window_read_no_loss():
    lines = _pairs(6 * 3600, 5, lambda i: (1, 0, 1), lambda i: (1, 0, 1))
    lines.append("07:00:00.000: much later, the outputs were stopped")
    events, _ms, _dest, age = bsg.vban_pacer_loss_from_log(_text(lines))
    assert events == "0"
    assert age == "3560"


# ---------------------------------------------------------------------------------------------
# vban_pacer_loss_from_log -- the fixed-timeline pacer format (dest=, ms counters)
# ---------------------------------------------------------------------------------------------
def test_new_format_splits_by_destination_and_reports_the_worst():
    lines = []
    for i in range(30):
        s = 6 * 3600 + i * 10
        disc = 0 if i < 20 else 1
        silence = "0.0" if i < 20 else "254.0"
        lines.append(_new(_hms(s), "10.77.7.106:6980", disc=disc, silence=silence))
        lines.append(_new(_hms(s + 0.01), "10.77.8.20:6980"))
    events, loss_ms, dest, _age = bsg.vban_pacer_loss_from_log(_text(lines))
    assert dest == "10.77.7.106:6980"
    assert events == "1"
    assert loss_ms == "254.0"


def test_new_format_late_sends_are_not_a_loss():
    # After an in-grace buffering hole late_sends climbs ~200/s with silence_ms flat: late but
    # complete audio -- the pacer's own known consequence, never a loss.
    lines = [_new(_hms(6 * 3600 + i * 10), "10.77.7.106:6980", late_sends=2000 * i)
             for i in range(30)]
    events, loss_ms, _dest, _age = bsg.vban_pacer_loss_from_log(_text(lines))
    assert events == "0"
    assert loss_ms == "0.0"


def test_new_format_discarded_and_resyncs_count():
    lines = []
    for i in range(30):
        s = 6 * 3600 + i * 10
        rep, rs, disc_ms = (0, 0, "0.0") if i < 25 else (1, 1, "120.5")
        lines.append(_new(_hms(s), "10.77.7.106:6980", repays=rep, resyncs=rs, discarded=disc_ms))
    events, loss_ms, _dest, _age = bsg.vban_pacer_loss_from_log(_text(lines))
    assert events == "2"
    assert loss_ms == "120.5"


# ---------------------------------------------------------------------------------------------
# build_bundle_state + the server gather wiring
# ---------------------------------------------------------------------------------------------
_NEW_KEYS = ("audio_mixer_ticks", "audio_mixer_ticks_over", "audio_mixer_window_ms",
             "audio_mixer_tick_ms", "audio_mixer_age_s", "vban_pacer_loss_events",
             "vban_pacer_loss_ms", "vban_pacer_loss_dest", "vban_pacer_age_s")


def test_build_bundle_state_passes_the_new_facets_and_omits_empty():
    st = bsg.build_bundle_state(audio_mixer_ticks="2813", audio_mixer_ticks_over="0",
                                audio_mixer_window_ms="60011", audio_mixer_tick_ms="21.3",
                                audio_mixer_age_s="0", vban_pacer_loss_events="0",
                                vban_pacer_loss_ms="", vban_pacer_loss_dest="stream=cg",
                                vban_pacer_age_s="0")
    assert st["audio_mixer_ticks"] == "2813"
    assert st["audio_mixer_ticks_over"] == "0"      # "0" is a reading, kept
    assert st["vban_pacer_loss_events"] == "0"
    assert "vban_pacer_loss_ms" not in st           # "" omitted
    empty = bsg.build_bundle_state()
    for k in _NEW_KEYS:
        assert k not in empty


def _load_server():
    spec = importlib.util.spec_from_file_location("bundle_state_server_1381",
                                                  _SCRIPTS / "bundle-state-server.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _gather_with_log(monkeypatch, tmp_path, log_text):
    bss = _load_server()
    monkeypatch.setattr(bss, "gather_ndi_inputs", lambda host, password: {})
    log_dir = tmp_path / "logs"
    log_dir.mkdir()
    (log_dir / "obs.txt").write_text(log_text, encoding="utf-8")
    return bss.gather_bundle_state(
        "127.0.0.1", "", str(log_dir), str(tmp_path / "missing-ndi.dll"), [],
        genlock_build_sha_file=str(tmp_path / "missing-sha.txt"),
        obs_install_scan_roots=(),
        startup_shortcut=str(tmp_path / "missing.lnk"),
        ahk_path=str(tmp_path / "missing.ahk"),
        obs_dll_path=str(tmp_path / "missing-obs.dll"),
    )


def test_server_gather_flows_the_mixer_and_vban_facets(monkeypatch, tmp_path):
    lines = ["OBS 32.1.2 (64-bit, windows)", _stall("06:08:15.416", 2812, 641),
             "06:08:15.420: audio-telemetry #800 'mbc': ts_lag_ms=107 buffered_ms=85 pending=0 "
             "timing_adjust_ms=0"]
    lines += _pairs(6 * 3600 + 8 * 60 + 16, 6, lambda i: (1, 0, 1) if i < 3 else (5, 1, 1),
                    lambda i: (1, 0, 1))
    lines.append(_stall("06:09:15.430", 2760, 797))
    state = _gather_with_log(monkeypatch, tmp_path, _text(lines))
    assert state["audio_mixer_ticks"] == "2760"
    assert state["audio_mixer_ticks_over"] == "797"
    assert state["audio_mixer_window_ms"] == "60014"
    assert state["audio_mixer_tick_ms"] == "21.3"
    assert state["vban_pacer_loss_events"] == "5"
    assert state["vban_pacer_loss_dest"] == "stream=cg"
    # A facet parsed BEFORE the new ones in the order-sensitive unpack still lands on its own key.
    assert state["audio_ts_lag_ms"] == "107"
    assert state["audio_ts_lag_src"] == "mbc"
    json.dumps(state)


def test_server_gather_omits_the_facets_on_a_box_without_them(monkeypatch, tmp_path):
    state = _gather_with_log(monkeypatch, tmp_path, "OBS 32.1.2 (64-bit, windows)\n")
    for k in _NEW_KEYS:
        assert k not in state
