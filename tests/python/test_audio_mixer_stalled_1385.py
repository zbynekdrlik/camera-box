"""issue 1385 -- the audio-mixer pager pages a STOPPED OBS audio thread (STALLED).

Issue 1381 shipped the audio-mixer pager with STALE (the newest `audio-stall #1367` dump sits more
than 180 s behind the log head) as log-only. A box with no obs-vban output (stream, strih-lx) then
had nothing that paged a dead audio thread -- silence on air. STALLED is the paging verdict for it:

  * the newest dump is older than the stale window (in-log age, behind the log head), AND
  * the log head itself is LIVE: `obs_log_head_age_s` (the box's own local wall clock minus the
    newest timestamped line of the tail) is at most 60 s -- positive proof OBS is still logging
    NOW, AND
  * the box is reachable.

Without that proof (OBS down, the log frozen, the facet absent on an older gather) the dump age
still reads STALE, log-only: obs-liveness / bundle-state own a dead OBS. A normal OBS start (fewer
than two dumps) stays UNKNOWN. When the last dumps leave the 5 MB tail of a long session, the mixer
facet still reports an age (the newest dump is older than the whole tail), so a thread that stays
dead keeps paging instead of decaying to UNKNOWN.

The replays cut the REAL 27.9 resolume log (tests/fixtures/audio_mixer_1381/) read-only: the
control window with every `audio-stall` line after 03:30:00 removed is a thread that stopped while
the obs-vban pacer thread kept logging. Tier-0 pytest (no cargo).
"""
import calendar
import gzip
import importlib.util
import json
import os
import pathlib
import stat
import subprocess
import sys
import time

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_FX = _ROOT / "tests" / "fixtures" / "audio_mixer_1381"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import audio_mixer_decision as amd  # noqa: E402
import bundle_state_gather as bsg  # noqa: E402

SEP = bsg.LOG_BOUNDED_READ_SEPARATOR
PASS_S = 300
PAGING = ("BEHIND", "OVERLOADED", "STALLED")


def _mixer(age_s, head_age_s, ticks=2813, over=0, tick_ms="21.3", reachable=1, **kw):
    return amd.classify_mixer(ticks, over, 60011, tick_ms, age_s, reachable,
                              log_head_age_s=head_age_s, **kw)["verdict"]


# ---------------------------------------------------------------------------------------------
# decision: STALLED
# ---------------------------------------------------------------------------------------------
def test_stale_dump_with_a_live_log_head_is_stalled():
    assert _mixer(400, 3) == "STALLED"
    assert _mixer(181, 0) == "STALLED"


def test_stale_dump_without_proof_the_log_is_live_stays_stale():
    assert _mixer(400, None) == "STALE"      # facet absent (an older gather) -> no proof
    assert _mixer(400, 61) == "STALE"        # the log head stopped (OBS down / frozen)
    assert _mixer(400, 7200) == "STALE"


def test_log_live_boundary_is_sixty_seconds_and_overridable():
    assert _mixer(400, 60) == "STALLED"
    assert _mixer(400, 61) == "STALE"
    assert _mixer(400, 90, log_live_s=120) == "STALLED"


def test_stalled_needs_the_dump_past_the_stale_window():
    assert _mixer(180, 0) == "HEALTHY"
    assert _mixer(181, 0) == "STALLED"
    assert _mixer(400, 0, stale_after_s=600) == "HEALTHY"


def test_stalled_is_decided_before_grading_the_old_dump():
    # A stale dump's counts describe a minute long gone; the stop is the current fault.
    assert _mixer(400, 2, ticks=2760, over=797) == "STALLED"


def test_an_age_only_facet_grades_staleness():
    # The last dumps scrolled out of the tail of a long session: no counts, only an age.
    assert _mixer(2400, 4, ticks=None, over=None, tick_ms=None) == "STALLED"
    assert _mixer(2400, 900, ticks=None, over=None, tick_ms=None) == "STALE"
    assert _mixer(30, 4, ticks=None, over=None, tick_ms=None) == "UNKNOWN"
    assert _mixer(None, 4, ticks=None, over=None, tick_ms=None) == "UNKNOWN"


def test_a_normal_obs_start_never_stalls():
    # First minute after a start: fewer than two dumps -> the whole mixer facet is absent.
    assert _mixer(None, 1, ticks=None, over=None, tick_ms=None) == "UNKNOWN"


def test_a_frozen_log_never_grades_its_old_counts():
    # OBS died with a BEHIND dump as its last word: the dump is fresh behind the frozen head, but
    # the head itself stopped. Grading those counts would page a dead OBS every hour.
    assert _mixer(30, 400, ticks=2760, over=797) == "STALE"
    assert _mixer(30, 181, ticks=2813, over=0) == "STALE"
    assert _mixer(30, 180, ticks=2760, over=797) == "BEHIND"   # a quiet but live log still grades
    assert _mixer(30, None, ticks=2760, over=797) == "BEHIND"  # an older gather: unchanged


def test_unreachable_box_is_skip_even_with_stale_facets():
    assert _mixer(400, 3, reachable=0) == "SKIP"


def test_analyze_reads_the_log_head_age_facet():
    body = json.dumps({"audio_mixer_ticks": "2813", "audio_mixer_ticks_over": "0",
                       "audio_mixer_window_ms": "60011", "audio_mixer_tick_ms": "21.3",
                       "audio_mixer_age_s": "420", "obs_log_head_age_s": "4"})
    r = amd.analyze(body, 1)
    assert r["mixer_verdict"] == "STALLED"
    assert r["log_head_age_s"] == 4
    assert amd.analyze(json.dumps({"audio_mixer_age_s": "420"}), 1)["mixer_verdict"] == "STALE"


def test_cli_prints_the_log_head_age_and_takes_the_live_window():
    body = json.dumps({"audio_mixer_ticks": "2813", "audio_mixer_ticks_over": "0",
                       "audio_mixer_window_ms": "60011", "audio_mixer_tick_ms": "21.3",
                       "audio_mixer_age_s": "420", "obs_log_head_age_s": "90"})
    cli = [sys.executable, str(_SCRIPTS / "audio_mixer_decision.py"), "analyze",
           "--box-reachable", "1"]
    out = subprocess.run(cli, input=body, capture_output=True, text=True, check=True).stdout
    kv = dict(line.split("=", 1) for line in out.strip().splitlines())
    assert kv["mixer_verdict"] == "STALE"
    assert kv["log_head_age_s"] == "90"
    out = subprocess.run(cli + ["--log-live-s", "120"], input=body, capture_output=True,
                         text=True, check=True).stdout
    assert "mixer_verdict=STALLED" in out


# ---------------------------------------------------------------------------------------------
# gather: obs_log_head_age_s (the box's own clock minus the newest tail line)
# ---------------------------------------------------------------------------------------------
def _tod(h, m, s):
    return h * 3600 + m * 60 + s


def _text(lines, crlf=False):
    nl = "\r\n" if crlf else "\n"
    return nl.join(lines) + nl


def _stall(ts, ticks=2813, over=0):
    return (f"{ts}: audio-stall #1367: tick_gap_max_ms=25.0 callback_max_ms=18.0 "
            f"ticks={ticks} ticks_over={over} tick_ms=21.3")


def test_log_head_age_is_the_box_clock_minus_the_newest_line():
    text = _text(["10:00:00.000: a", "10:05:03.250: program-render-audit: lagged=0"])
    assert bsg.obs_log_head_age_s_from_log(text, _tod(10, 5, 10)) == "7"


def test_log_head_age_reads_the_tail_only():
    head = _text(["23:00:00.000: an old head line"])
    tail = _text(["10:00:00.000: tail line"])
    assert bsg.obs_log_head_age_s_from_log(head + SEP + tail, _tod(10, 0, 30)) == "30"


def test_log_head_age_skips_an_unstamped_last_line():
    text = _text(["10:00:00.000: a", "10:00:04.000: b", "  continuation without a timestamp"])
    assert bsg.obs_log_head_age_s_from_log(text, _tod(10, 0, 10)) == "6"


def test_log_head_age_tolerates_crlf():
    text = _text(["10:00:00.000: a", "10:00:04.000: b"], crlf=True)
    assert bsg.obs_log_head_age_s_from_log(text, _tod(10, 0, 10)) == "6"


def test_log_head_age_wraps_midnight():
    text = _text(["23:59:50.000: before midnight"])
    assert bsg.obs_log_head_age_s_from_log(text, _tod(0, 0, 20)) == "30"


def test_log_head_a_little_ahead_of_the_clock_reads_zero():
    # "now" is read right after the log, so a head ahead of it is a wall-clock step back.
    text = _text(["10:00:00.500: a"])
    assert bsg.obs_log_head_age_s_from_log(text, _tod(10, 0, 0)) == "0"


def test_log_head_far_ahead_of_the_clock_is_a_previous_day():
    # A date-less log: a head an hour "ahead" of now is yesterday's line -> about 23 h old.
    text = _text(["10:00:00.000: a"])
    assert bsg.obs_log_head_age_s_from_log(text, _tod(9, 0, 0)) == str(23 * 3600)


def test_log_head_age_absent_without_a_timestamped_line():
    assert bsg.obs_log_head_age_s_from_log("", _tod(10, 0, 0)) == ""
    assert bsg.obs_log_head_age_s_from_log(None, _tod(10, 0, 0)) == ""
    assert bsg.obs_log_head_age_s_from_log("OBS 32.2.0\n", _tod(10, 0, 0)) == ""


def test_local_seconds_of_day_reads_the_local_clock(monkeypatch):
    # The rig's zone (CET/CEST): OBS stamps local time, so the box clock must be LOCAL too. In UTC
    # localtime == gmtime and a gmtime slip would stay green; here it reads 1-2 h off.
    monkeypatch.setenv("TZ", "CET-1CEST,M3.5.0,M10.5.0/3")
    time.tzset()
    try:
        winter = calendar.timegm((2026, 1, 15, 12, 0, 0)) + 0.25
        summer = calendar.timegm((2026, 7, 15, 12, 0, 0)) + 0.25
        assert bsg.local_seconds_of_day(winter) == pytest.approx(13 * 3600 + 0.25)
        assert bsg.local_seconds_of_day(summer) == pytest.approx(14 * 3600 + 0.25)
    finally:
        monkeypatch.undo()
        time.tzset()


def test_build_bundle_state_passes_the_head_age_and_omits_empty():
    assert bsg.build_bundle_state(obs_log_head_age_s="3")["obs_log_head_age_s"] == "3"
    assert bsg.build_bundle_state(obs_log_head_age_s="0")["obs_log_head_age_s"] == "0"
    assert "obs_log_head_age_s" not in bsg.build_bundle_state()


# ---------------------------------------------------------------------------------------------
# gather: the mixer age survives the last dumps leaving the tail of a long session
# ---------------------------------------------------------------------------------------------
def test_dumps_scrolled_out_of_the_tail_report_the_tail_span_as_the_age():
    head = _text(["16:35:05.000: OBS 32.2.0", _stall("16:36:05.904", ticks=1),
                  _stall("16:37:05.917")])
    tail = _text(["20:00:00.000: program-render-audit: lagged=0",
                  "20:40:00.000: program-render-audit: lagged=0"])
    assert bsg.audio_mixer_from_log(head + SEP + tail) == ("", "", "", "", "2400")


def test_one_dump_left_in_the_tail_reports_its_age():
    head = _text([_stall("16:36:05.904", ticks=1), _stall("16:37:05.917")])
    tail = _text(["20:00:00.000: x", _stall("20:01:00.000"), "20:10:00.000: y"])
    assert bsg.audio_mixer_from_log(head + SEP + tail) == ("", "", "", "", "540")


def test_no_dump_anywhere_stays_absent():
    # A build without the probe (a bisect to an old OBS) never reads as a stopped thread.
    head = _text(["16:35:05.000: OBS 32.1.2"])
    tail = _text(["20:00:00.000: x", "20:40:00.000: y"])
    assert bsg.audio_mixer_from_log(head + SEP + tail) == ("", "", "", "", "")


def test_a_small_whole_log_with_one_dump_is_still_a_normal_start():
    text = _text(["19:49:08.001: OBS 32.2.0", _stall("19:49:10.146", ticks=1),
                  "19:55:00.000: later line"])
    assert bsg.audio_mixer_from_log(text) == ("", "", "", "", "")


# ---------------------------------------------------------------------------------------------
# the server wires the facet (now read right after the log)
# ---------------------------------------------------------------------------------------------
def _load_server():
    spec = importlib.util.spec_from_file_location("bundle_state_server_1385",
                                                  _SCRIPTS / "bundle-state-server.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _gather(monkeypatch, tmp_path, log_text, now_tod):
    bss = _load_server()
    monkeypatch.setattr(bss, "gather_ndi_inputs", lambda host, password: {})
    monkeypatch.setattr(bss.bsg, "local_seconds_of_day", lambda *a: now_tod)
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


def test_server_serves_the_log_head_age(monkeypatch, tmp_path):
    text = _text([_stall("06:00:00.000"), _stall("06:01:00.000"),
                  "06:05:30.000: program-render-audit: lagged=0"])
    state = _gather(monkeypatch, tmp_path, text, _tod(6, 5, 32))
    assert state["obs_log_head_age_s"] == "2"
    assert state["audio_mixer_age_s"] == "270"
    assert state["audio_mixer_ticks"] == "2813"
    assert amd.analyze(json.dumps(state), 1)["mixer_verdict"] == "STALLED"


def test_server_omits_the_head_age_without_a_timestamped_line(monkeypatch, tmp_path):
    state = _gather(monkeypatch, tmp_path, "OBS 32.2.0 (64-bit, windows)\n", _tod(6, 0, 0))
    assert "obs_log_head_age_s" not in state


# ---------------------------------------------------------------------------------------------
# replay: the real 27.9 resolume control window with the audio thread stopping at 03:30
# ---------------------------------------------------------------------------------------------
CUT = _tod(3, 30, 0)          # every audio-stall line after this is removed
LAST_DUMP = _tod(3, 29, 13)   # the newest dump before the cut (03:29:13.x)
HEAD_LAG_S = 20               # the pacer logs every ~5 s, so the log head trails a pass by < 20 s


def _lines(name):
    raw = gzip.decompress((_FX / f"resolume-{name}.txt.gz").read_bytes()).decode("ascii")
    return raw.splitlines(keepends=True)


def _sec(line):
    h, m, s = line[:12].split(":")
    return int(h) * 3600 + int(m) * 60 + float(s)


def _stopped_thread(lines, cut=CUT, obs_dies_at=None, keep_other_lines=True):
    out = []
    for ln in lines:
        t = _sec(ln)
        if obs_dies_at is not None and t > obs_dies_at:
            continue
        stall = "audio-stall #1367" in ln
        if stall and t > cut:
            continue
        if not stall and not keep_other_lines:
            continue
        out.append(ln)
    return out


def _bundle_at(lines, t):
    """The body the box would serve at local time t (its log as it stood, its clock at t)."""
    text = "".join(ln for ln in lines if _sec(ln) <= t)
    mixer = bsg.audio_mixer_from_log(text)
    vban = bsg.vban_pacer_loss_from_log(text)
    st = bsg.build_bundle_state(
        audio_mixer_ticks=mixer[0], audio_mixer_ticks_over=mixer[1],
        audio_mixer_window_ms=mixer[2], audio_mixer_tick_ms=mixer[3], audio_mixer_age_s=mixer[4],
        vban_pacer_loss_events=vban[0], vban_pacer_loss_ms=vban[1], vban_pacer_loss_dest=vban[2],
        vban_pacer_age_s=vban[3], obs_log_head_age_s=bsg.obs_log_head_age_s_from_log(text, t))
    return json.dumps(st)


def _replay(lines, phase_s, start, end):
    t = start + phase_s
    confirm = 0
    out = []
    while t <= end:
        v = amd.analyze(_bundle_at(lines, t), 1)["mixer_verdict"]
        act = 0
        confirm, act = _confirm_step(confirm, v)
        out.append((t, v, act))
        t += PASS_S
    return out


def _confirm_step(confirm, verdict):
    """The watchdog's mixer-arm confirm: a paging verdict counts, HEALTHY and STALE reset (STALE =
    the log is not live, and its once-a-day date-less false-live pass must never pair with the next
    day's), SKIP / UNKNOWN hold."""
    if verdict in PAGING:
        confirm += 1
        return confirm, 1 if confirm >= 2 else 0
    if verdict in ("HEALTHY", "STALE"):
        return 0, 0
    return confirm, 0


def _hms(sec):
    sec = int(sec) % 86400
    return f"{sec // 3600:02d}:{(sec % 3600) // 60:02d}:{sec % 60:02d}"


def test_replay_a_stopped_audio_thread_pages_stalled_at_every_pass_phase():
    full = _lines("control-0300-0415")
    lines = _stopped_thread(full)
    start, end = _sec(full[0]), _sec(full[-1])
    for phase in range(0, PASS_S, 20):
        res = _replay(lines, phase, start, end)
        before = [(_hms(t), v) for t, v, _a in res if t < CUT and v in PAGING]
        assert not before, f"phase {phase}s: paged before the stop: {before}"
        verdicts = {v for t, v, _a in res if t > LAST_DUMP + 181 + HEAD_LAG_S}
        assert verdicts == {"STALLED"}, f"phase {phase}s: after the stop read {verdicts}"
        first = next((t for t, _v, a in res if a), None)
        assert first is not None, f"phase {phase}s: never paged"
        assert first <= LAST_DUMP + 181 + HEAD_LAG_S + 2 * PASS_S, \
            f"phase {phase}s: paged at {_hms(first)}"


def test_replay_obs_dying_after_the_stop_ends_the_stalled_verdict():
    full = _lines("control-0300-0415")
    dies = _tod(3, 45, 0)
    lines = _stopped_thread(full, obs_dies_at=dies)
    start, end = _sec(full[0]), _sec(full[-1])
    for phase in range(0, PASS_S, 20):
        res = _replay(lines, phase, start, end)
        after = {v for t, v, _a in res if t > dies + 61}
        assert after == {"STALE"}, f"phase {phase}s: a dead OBS read {after}"


def test_replay_a_log_with_no_other_advancing_line_never_pages():
    # Only the audio-stall lines, cut at 03:30: after the cut nothing proves the log advances.
    full = _lines("control-0300-0415")
    lines = _stopped_thread(full, keep_other_lines=False)
    start, end = _sec(full[0]), _sec(full[-1])
    for phase in range(0, PASS_S, 20):
        res = _replay(lines, phase, start, end)
        fired = [(_hms(t), v) for t, v, _a in res if v in PAGING]
        assert not fired, f"phase {phase}s: paged without a live log: {fired}"


@pytest.mark.parametrize("name", ["control-0300-0415", "start-1949-1957", "clean-2000-2045"])
def test_replay_quiet_windows_never_stall(name):
    lines = _lines(name)
    start, end = _sec(lines[0]), _sec(lines[-1])
    for phase in range(0, PASS_S, 20):
        res = _replay(lines, phase, start, end)
        fired = [(_hms(t), v) for t, v, _a in res if v in PAGING]
        assert not fired, f"{name} phase {phase}s graded a paging verdict: {fired}"


def test_replay_the_real_onset_pages_behind_never_stalled():
    # 27.9 06:00: the mixer left real time but kept dumping every minute -> BEHIND / OVERLOADED.
    lines = _lines("onset-0540-0630")
    start, end = _sec(lines[0]), _sec(lines[-1])
    for phase in range(0, PASS_S, 20):
        res = _replay(lines, phase, start, end)
        stalled = [(_hms(t), v) for t, v, _a in res if v in ("STALLED", "STALE")]
        assert not stalled, f"phase {phase}s: the onset read {stalled}"


# ---------------------------------------------------------------------------------------------
# a log that stopped for DAYS: the date-less head reads live once a day, never twice in a row
# ---------------------------------------------------------------------------------------------
def _frozen_log_passes(lines, dies, days=3):
    """Pass bodies of a box whose OBS died at `dies` and whose :8899 keeps serving the frozen log,
    every 300 s for `days` days, phased so one pass a day lands 30 s after the head's time of day
    (inside the date-less false-live window)."""
    frozen = [ln for ln in lines if _sec(ln) <= dies]
    text = "".join(frozen)
    head = _sec(frozen[-1])
    mixer = bsg.audio_mixer_from_log(text)
    vban = bsg.vban_pacer_loss_from_log(text)
    out = []
    t = head + 30.0
    while t <= head + 30.0 + days * 86400:
        now = t % 86400
        st = bsg.build_bundle_state(
            audio_mixer_ticks=mixer[0], audio_mixer_ticks_over=mixer[1],
            audio_mixer_window_ms=mixer[2], audio_mixer_tick_ms=mixer[3],
            audio_mixer_age_s=mixer[4], vban_pacer_loss_events=vban[0],
            vban_pacer_loss_ms=vban[1], vban_pacer_loss_dest=vban[2], vban_pacer_age_s=vban[3],
            obs_log_head_age_s=bsg.obs_log_head_age_s_from_log(text, now))
        out.append((t, json.dumps(st)))
        t += PASS_S
    return out


def _verdicts(passes):
    return [(t, body, amd.analyze(body, 1)["mixer_verdict"]) for t, body in passes]


def _compress(seq, keep=2):
    """Runs of one verdict cut to `keep` passes: the watchdog state machine is idempotent after the
    second identical pass, so the real bash sees every transition without 864 invocations."""
    out = []
    run = 0
    for i, item in enumerate(seq):
        run = run + 1 if i and item[2] == seq[i - 1][2] else 1
        if run <= keep:
            out.append(item)
    return out


def test_a_log_frozen_for_days_after_a_stop_never_pages_stalled():
    full = _lines("control-0300-0415")
    seq = _verdicts(_frozen_log_passes(_stopped_thread(full), dies=_tod(3, 45, 0)))
    stalled = [t for t, _b, v in seq if v == "STALLED"]
    assert len(stalled) >= 3, "the once-a-day false-live window was never exercised"
    confirm = 0
    for t, _b, v in seq:
        confirm, act = _confirm_step(confirm, v)
        assert not act, f"a dead OBS paged {v} on day {int(t // 86400)}"


def test_a_log_frozen_for_days_on_a_behind_dump_never_pages():
    # OBS died at 06:20 on 27.9 while the mixer was off real time: its last dump grades BEHIND /
    # OVERLOADED. Graded on a frozen log that paged every hour for as long as OBS stayed dead.
    full = _lines("onset-0540-0630")
    dies = _tod(6, 20, 0)
    passes = _frozen_log_passes(full, dies=dies)
    shape = json.loads(passes[0][1])
    shape.pop("obs_log_head_age_s", None)
    assert amd.analyze(json.dumps(shape), 1)["mixer_verdict"] in PAGING, "fixture shape changed"
    confirm = 0
    for t, _b, v in _verdicts(passes):
        confirm, act = _confirm_step(confirm, v)
        assert not act, f"a dead OBS paged {v} at +{int(t - dies)} s"


def _stream_shaped(lines, cut=CUT):
    """The stream / strih-lx shape: no obs-vban output. The real audio-stall lines of the control
    window up to `cut`, and a program-render-audit line every 5 s (the render thread keeps logging),
    CRLF as on the box."""
    stalls = [ln for ln in lines if "audio-stall #1367" in ln and _sec(ln) <= cut]
    start, end = _sec(lines[0]), _sec(lines[-1])
    render = []
    t = start
    while t <= end:
        h, rem = divmod(t, 3600)
        m, s = divmod(rem, 60)
        render.append(f"{int(h):02d}:{int(m):02d}:{s:06.3f}: program-render-audit: "
                      "render_fps=30.0 target_fps=30.0 avg_frame_ms=17.9 lagged=0 total=150\r\n")
        t += 5.0
    return sorted(stalls + render, key=_sec)


def test_replay_a_stopped_thread_on_a_box_without_vban_pages_stalled():
    full = _lines("control-0300-0415")
    lines = _stream_shaped(full)
    assert not any("obs-vban" in ln for ln in lines)
    start, end = _sec(full[0]), _sec(full[-1])
    for phase in range(0, PASS_S, 20):
        res = _replay(lines, phase, start, end)
        before = [(_hms(t), v) for t, v, _a in res if t < CUT and v in PAGING]
        assert not before, f"phase {phase}s: paged before the stop: {before}"
        first = next((t for t, _v, a in res if a), None)
        assert first is not None, f"phase {phase}s: never paged"
        assert first <= LAST_DUMP + 181 + HEAD_LAG_S + 2 * PASS_S, \
            f"phase {phase}s: paged at {_hms(first)}"


# ---------------------------------------------------------------------------------------------
# the real bash orchestrator over the stopped-thread replay: one page, STALLED, time-bucketed arm
# ---------------------------------------------------------------------------------------------
def _run_watchdog(tmp_path, body, state_file, now=None):
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
        "AIRULESET_NOTIFY": "/nonexistent/airuleset.py",
    })
    if now is not None:
        env["AUDIO_MIXER_NOW_EPOCH"] = str(int(now))
    r = subprocess.run(["bash", str(_SCRIPTS / "audio-mixer-alert-watchdog.sh"), "--dry-run"],
                       capture_output=True, text=True, env=env, timeout=60)
    assert r.returncode == 0, r.stderr
    return r.stderr


def test_watchdog_dry_run_pages_a_stopped_thread_once(tmp_path):
    full = _lines("control-0300-0415")
    lines = _stopped_thread(full)
    state = tmp_path / "state"
    start, end = _sec(full[0]), _sec(full[-1])
    t = start + 60
    would = []
    logs = []
    while t <= end:
        log = _run_watchdog(tmp_path, _bundle_at(lines, t), state)
        logs.append(log)
        would += [(_hms(t), ln) for ln in log.splitlines() if "WOULD alert" in ln]
        t += PASS_S
    assert would, "\n".join(logs[-3:])
    assert all("STALLED" in ln for _t, ln in would), would
    assert all(tt > "03:30:00" for tt, _ln in would), would
    fired = [ln for _t, ln in would if "alert_now=1" in ln]
    assert len(fired) == 1, would
    assert any("log_head_age=" in lg for lg in logs)


def _mixer_body(age_s, head_age_s):
    return json.dumps({"audio_mixer_ticks": "2813", "audio_mixer_ticks_over": "0",
                       "audio_mixer_window_ms": "60011", "audio_mixer_tick_ms": "21.3",
                       "audio_mixer_age_s": str(age_s), "obs_log_head_age_s": str(head_age_s)})


def test_watchdog_stale_between_two_stalled_passes_resets_the_confirm(tmp_path):
    state = tmp_path / "state"
    stalled, stale = _mixer_body(420, 4), _mixer_body(420, 900)
    for body in (stalled, stale, stalled):
        log = _run_watchdog(tmp_path, body, state)
        assert "WOULD alert" not in log, log
    log = _run_watchdog(tmp_path, stalled, state)   # two in a row: now it pages
    assert "WOULD alert (STALLED)" in log, log


def test_watchdog_dry_run_never_pages_a_log_frozen_for_days(tmp_path):
    full = _lines("control-0300-0415")
    seq = _compress(_verdicts(_frozen_log_passes(_stopped_thread(full), dies=_tod(3, 45, 0))))
    assert sum(1 for _t, _b, v in seq if v == "STALLED") >= 2
    state = tmp_path / "state"
    for t, body, v in seq:
        log = _run_watchdog(tmp_path, body, state, now=1_790_000_000 + t)
        assert "WOULD alert" not in log, f"day {int(t // 86400)} {v}: {log}"


def test_watchdog_dry_run_pages_a_stopped_thread_on_a_box_without_vban_once(tmp_path):
    full = _lines("control-0300-0415")
    lines = _stream_shaped(full)
    state = tmp_path / "state"
    start, end = _sec(full[0]), _sec(full[-1])
    t = start + 60
    fired = []
    while t <= end:
        log = _run_watchdog(tmp_path, _bundle_at(lines, t), state)
        fired += [ln for ln in log.splitlines() if "WOULD alert" in ln and "alert_now=1" in ln]
        t += PASS_S
    assert len(fired) == 1 and "STALLED" in fired[0] and "stream" in fired[0], fired


def test_watchdog_dry_run_logs_stale_without_a_page_when_the_log_stopped(tmp_path):
    body = json.dumps({"audio_mixer_ticks": "2813", "audio_mixer_ticks_over": "0",
                       "audio_mixer_window_ms": "60011", "audio_mixer_tick_ms": "21.3",
                       "audio_mixer_age_s": "420", "obs_log_head_age_s": "900"})
    state = tmp_path / "state"
    for _ in range(3):
        log = _run_watchdog(tmp_path, body, state)
        assert "WOULD alert" not in log
    assert "mixer=STALE" in log


# ---------------------------------------------------------------------------------------------
# a clock mismatch (OBS and the gather disagree on the time zone) must not blind the pager silently
# ---------------------------------------------------------------------------------------------
def test_log_clock_mismatch_is_a_log_that_advances_yet_reads_old():
    # A live log whose stamps are 2 h off the gather's clock: the head age stays ~7200 between
    # passes. A frozen log ages by the pass gap; a live one with matching clocks reads young.
    assert amd.classify_log_clock(7203, 7201, 300) == "MISMATCH"
    assert amd.classify_log_clock(79199, 79203, 305) == "MISMATCH"   # OBS ahead of the gather
    assert amd.classify_log_clock(1300, 1000, 300) == "OK"           # frozen: ages with the wall
    assert amd.classify_log_clock(4, 6, 300) == "OK"                 # live, clocks agree
    assert amd.classify_log_clock(250, 86350, 300) == "OK"           # a frozen log's date wrap
    assert amd.classify_log_clock(7197, 7199, 300) == "MISMATCH"     # stamps a moment ahead
    assert amd.classify_log_clock(7203, 7201, 30) == "UNKNOWN"       # passes too close to judge
    assert amd.classify_log_clock(7203, 7201, 7200) == "UNKNOWN"     # a long timer gap
    assert amd.classify_log_clock(7203, 7201, 900) == "UNKNOWN"      # past the 600 s bound
    assert amd.classify_log_clock(7203, None, 300) == "UNKNOWN"
    assert amd.classify_log_clock(None, 7201, 300) == "UNKNOWN"
    assert amd.classify_log_clock(7203, None, None) == "UNKNOWN"


def test_log_clock_boundary_is_half_the_pass_gap():
    assert amd.classify_log_clock(7229, 7180, 100) == "MISMATCH"     # aged 49 s of 100
    assert amd.classify_log_clock(7230, 7180, 100) == "OK"           # aged half the gap


def test_a_log_head_that_is_not_a_zone_offset_is_never_a_mismatch():
    # Review round 3: a hung OBS whose WebSocket thread still logs each :8899 connect (written
    # AFTER the gather's log read) reads ~one pass gap old on every pass. That is on-air silence,
    # not a time-zone fault, so the CLOCK page must not claim it. A zone offset is a multiple of
    # 15 min, so a live log's head age sits within a minute of a quarter hour; 300 s never does.
    assert amd.classify_log_clock(301, 299, 300) == "OK"
    assert amd.classify_log_clock(302, 300, 305) == "OK"
    assert amd.classify_log_clock(5000, 5001, 300) == "OK"           # 5000 s = 5 min past 1 h 15
    assert amd.classify_log_clock(7261, 7259, 300) == "OK"           # 61 s past the quarter hour
    assert amd.classify_log_clock(7260, 7259, 300) == "MISMATCH"     # 60 s: still a fresh line


def test_watchdog_never_blames_the_clock_for_a_hung_obs_whose_websocket_still_logs(tmp_path):
    state = tmp_path / "state"
    for i, head in enumerate((301, 300, 302, 299, 301)):
        log = _run_watchdog(tmp_path, _mixer_body(900, head), state,
                            now=1_790_000_000 + i * 300)
        assert "clock MISMATCH" not in log, log
        assert "WOULD alert (CLOCK)" not in log, log


def test_cli_grades_the_log_clock_from_the_previous_pass():
    body = json.dumps({"audio_mixer_ticks": "2813", "audio_mixer_ticks_over": "0",
                       "audio_mixer_window_ms": "60011", "audio_mixer_tick_ms": "21.3",
                       "audio_mixer_age_s": "30", "obs_log_head_age_s": "7203"})
    cli = [sys.executable, str(_SCRIPTS / "audio_mixer_decision.py"), "analyze",
           "--box-reachable", "1"]
    out = subprocess.run(cli + ["--prev-log-head-age-s", "7201", "--pass-gap-s", "300"],
                         input=body, capture_output=True, text=True, check=True).stdout
    assert "mixer_verdict=STALE" in out
    assert "log_clock=MISMATCH" in out
    out = subprocess.run(cli, input=body, capture_output=True, text=True, check=True).stdout
    assert "log_clock=UNKNOWN" in out


def test_watchdog_pages_once_when_the_log_clock_disagrees(tmp_path):
    state = tmp_path / "state"
    live_offset = _mixer_body(30, 7203)       # a fresh dump, a live log stamped 2 h off
    logs = [_run_watchdog(tmp_path, live_offset, state, now=1_790_000_000 + i * 300)
            for i in range(5)]
    fired = [ln for lg in logs for ln in lg.splitlines() if "WOULD alert" in ln]
    assert len(fired) == 1 and "CLOCK" in fired[0], fired
    assert "WOULD alert" not in logs[0] + logs[1]      # no previous pass, then one: confirming
    # the clocks agree again -> the guard clears on the machine channel only
    log = _run_watchdog(tmp_path, _mixer_body(30, 4), state, now=1_790_000_000 + 5 * 300)
    assert "WOULD alert" not in log
    assert "RECOVERY" in log and "clock" in log


def test_watchdog_never_flags_a_frozen_log_as_a_clock_mismatch(tmp_path):
    state = tmp_path / "state"
    for i in range(6):
        body = _mixer_body(420, 900 + i * 300)   # OBS dead: the head ages with the wall clock
        log = _run_watchdog(tmp_path, body, state, now=1_790_000_000 + i * 300)
        assert "WOULD alert" not in log, log
        assert "clock MISMATCH" not in log, log


def test_replay_a_live_log_on_an_offset_box_clock_pages_the_blind_pager_once(tmp_path):
    # The stream-shaped real replay, but the gather's clock runs 2 h ahead of the OBS stamps: every
    # mixer verdict reads STALE (blind), and the clock guard pages that exactly once.
    full = _lines("control-0300-0415")
    lines = _stream_shaped(full, cut=_tod(5, 0, 0))   # the audio thread never stops here
    state = tmp_path / "state"
    start, end = _sec(full[0]), _sec(full[-1])
    t = start + 60
    fired = []
    verdicts = set()
    while t <= end:
        text = "".join(ln for ln in lines if _sec(ln) <= t)
        body = json.loads(_bundle_at(lines, t))
        body["obs_log_head_age_s"] = bsg.obs_log_head_age_s_from_log(text, (t + 7200) % 86400)
        verdicts.add(amd.analyze(json.dumps(body), 1)["mixer_verdict"])
        log = _run_watchdog(tmp_path, json.dumps(body), state, now=1_790_000_000 + t)
        fired += [ln for ln in log.splitlines() if "WOULD alert" in ln]
        t += PASS_S
    assert "STALE" in verdicts and verdicts <= {"STALE", "UNKNOWN"}, verdicts
    assert len(fired) == 1 and "CLOCK" in fired[0], fired


def test_the_frozen_bound_does_not_follow_the_stale_override():
    # AUDIO_MIXER_STALE_AFTER_S widens the dump window only; the frozen-log bound stays fixed, so a
    # dead OBS's last counts are still graded on at most one pass a day.
    assert _mixer(30, 181, ticks=2760, over=797, stale_after_s=600) == "STALE"
    assert _mixer(30, 180, ticks=2760, over=797, stale_after_s=600) == "BEHIND"


def test_watchdog_source_pages_stalled_through_the_time_bucketed_mixer_arm():
    src = (_SCRIPTS / "audio-mixer-alert-watchdog.sh").read_text(encoding="utf-8")
    assert 'STALLED) handle_arm "$box" "STALLED" "audio-mixer" "audio-mixer"' in src
    assert '--log-live-s "$LOG_LIVE_S"' in src
    assert 'LOG_LIVE_S="${AUDIO_MIXER_LOG_LIVE_S:-60}"' in src
    # the dump age is a lower bound once the last dumps left the tail
    assert "aspoň **${age} s**" in src
