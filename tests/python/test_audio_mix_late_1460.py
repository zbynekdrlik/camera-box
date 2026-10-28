"""issue 1460 -- the audio-mixer pager pages a MIXED_LATE member of the OBS audio mix.

libobs logs `audio-telemetry #800 mix: frozen=<0|1> mixed_late=<N>` once a minute
(vendor/obs-studio/libobs/obs-audio.c, the same 60 s block as the `#800` summary line). N is
cumulative since the audio reset: every rise is a mix member that fell behind the frozen mix window
and was re-anchored, an audible hole on air. Before this slice only the E2E read it (design comment
6480263478):

  * gather (`bundle_state_gather.audio_mix_late_from_log`): the newest `#800 mix:` line's count,
    frozen flag and in-log age, served as `audio_mix_mixed_late` / `audio_mix_frozen` /
    `audio_mix_age_s`;
  * decision (`audio_mixer_decision.classify_mix_late`): LATE when the count rose since the count
    the watchdog stored, BASELINE with none stored or a lower one (a new audio session);
  * watchdog: a fourth arm, 2-pass confirm (the stored count holds its pre-rise value until the
    page, so ONE rise pages), PRODUCTION-CRITICAL time-bucketed key `audio-mixer-mixlate-<box>`,
    recovery a machine-channel log line only.

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


def _c_format_mix():
    """The REAL `#800 mix:` format string from obs-audio.c (never a hand-typed copy). The C string
    ends in `%` and the `PRIu64` macro supplies the conversion (`llu`), a decimal: rendered `d`."""
    src = OBS_AUDIO.read_text()
    start = src.index('"audio-telemetry #800 mix: frozen=')
    end = src.index(",", src.index("PRIu64", start))
    text = "".join(re.findall(r'"((?:[^"\\]|\\.)*)"', src[start:end]))
    assert text.endswith("mixed_late=%"), text
    return text + "d"


def _mix(ts, late, frozen=1):
    return f"{ts}: " + _c_format_mix() % (frozen, late)


def _text(lines, crlf=False):
    nl = "\r\n" if crlf else "\n"
    return nl.join(lines) + nl


# ---------------------------------------------------------------------------------------------
# gather: audio_mix_late_from_log
# ---------------------------------------------------------------------------------------------
def test_the_rendered_line_matches_the_live_shape():
    assert _mix("12:00:00.000", 7) == "12:00:00.000: audio-telemetry #800 mix: frozen=1 mixed_late=7"


def test_no_mix_line_is_absent():
    assert bsg.audio_mix_late_from_log("") == ("", "", "")
    assert bsg.audio_mix_late_from_log(_text(["12:00:00.000: something else"])) == ("", "", "")


def test_one_line_reads_count_frozen_and_age():
    log = _text([_mix("12:00:00.000", 3, frozen=0), "12:00:07.500: program-render-audit: x"])
    assert bsg.audio_mix_late_from_log(log) == ("3", "0", "8")


def test_zero_is_a_reading_never_absent():
    assert bsg.audio_mix_late_from_log(_text([_mix("12:00:00.000", 0)])) == ("0", "1", "0")


def test_the_newest_line_wins_also_when_lower_after_an_audio_reset():
    log = _text([_mix("12:00:00.000", 9), _mix("12:01:00.000", 11),
                 _mix("12:02:00.000", 0), "12:02:04.000: x"])
    assert bsg.audio_mix_late_from_log(log) == ("0", "1", "4")


def test_crlf_is_tolerated():
    log = _text([_mix("12:00:00.000", 5), "12:00:02.000: x"], crlf=True)
    assert bsg.audio_mix_late_from_log(log) == ("5", "1", "2")


def test_a_bounded_read_takes_only_the_tail():
    head = _text([_mix("10:00:00.000", 4)])
    assert bsg.audio_mix_late_from_log(head + SEP + _text(["12:00:00.000: x"])) == ("", "", "")
    tail = _text([_mix("12:00:00.000", 6), "12:00:30.000: x"])
    assert bsg.audio_mix_late_from_log(head + SEP + tail) == ("6", "1", "30")


def test_the_shared_tail_gives_the_same_result():
    log = _text([_mix("12:00:00.000", 2), _mix("12:01:00.000", 3), "12:01:05.000: x"])
    assert bsg.audio_mix_late_from_log(log, tail=bsg.timestamped_tail_lines(log)) == \
        bsg.audio_mix_late_from_log(log) == ("3", "1", "5")


def test_the_per_source_800_line_is_never_read_as_the_mix_line():
    log = _text(["12:00:00.000: audio-telemetry #800 'mbc': ts_lag_ms=5 buffered_ms=9 mixed_late=4"])
    assert bsg.audio_mix_late_from_log(log) == ("", "", "")


def test_build_bundle_state_carries_the_keys_and_omits_empty():
    state = bsg.build_bundle_state(**dict(zip(bsg.AUDIO_MIX_LATE_KEYS, ("0", "1", "4"))))
    assert state == {"audio_mix_mixed_late": "0", "audio_mix_frozen": "1", "audio_mix_age_s": "4"}
    assert bsg.build_bundle_state(**dict(zip(bsg.AUDIO_MIX_LATE_KEYS, ("", "", "")))) == {}


def _load_server():
    spec = importlib.util.spec_from_file_location("bundle_state_server_1460",
                                                  _SCRIPTS / "bundle-state-server.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_the_server_serves_the_mix_late_facet():
    bss = _load_server()
    log = _text([_mix("12:00:00.000", 2), _mix("12:01:00.000", 5), "12:01:03.000: x"])
    facets = bss._audio_log_facets(log, 12 * 3600 + 65, "mbc")
    assert (facets["audio_mix_mixed_late"], facets["audio_mix_frozen"],
            facets["audio_mix_age_s"]) == ("5", "1", "3")


# ---------------------------------------------------------------------------------------------
# decision: classify_mix_late (the truth table)
# ---------------------------------------------------------------------------------------------
@pytest.mark.parametrize("now, prev, age, reachable, verdict", [
    (5, 5, 0, 0, "SKIP"),
    (None, 5, 0, 1, "UNKNOWN"),
    (None, None, None, 1, "UNKNOWN"),
    (6, 5, amd.DEFAULT_STALE_AFTER_S + 1, 1, "STALE"),
    (6, 5, amd.DEFAULT_STALE_AFTER_S, 1, "LATE"),
    (5, None, 0, 1, "BASELINE"),
    (0, 5, 0, 1, "BASELINE"),
    (6, 5, 0, 1, "LATE"),
    (6, 5, None, 1, "LATE"),
    (5, 5, 0, 1, "HEALTHY"),
    (0, 0, 0, 1, "HEALTHY"),
])
def test_the_truth_table(now, prev, age, reachable, verdict):
    assert amd.classify_mix_late(now, prev, age, reachable) == verdict


def test_the_stale_bound_is_the_mixer_arms():
    assert amd.classify_mix_late(6, 5, 50, 1, stale_s=40) == "STALE"


def test_analyze_reads_the_facets_and_the_stored_count():
    body = json.dumps({"audio_mix_mixed_late": "7", "audio_mix_frozen": "1",
                       "audio_mix_age_s": "4"})
    res = amd.analyze(body, 1, prev_mix_late=5)
    assert (res["mix_late_verdict"], res["mix_late"], res["mix_late_prev"], res["mix_frozen"],
            res["mix_late_age_s"]) == ("LATE", 7, 5, 1, 4)
    assert amd.analyze(body, 1)["mix_late_verdict"] == "BASELINE"
    assert amd.analyze("{}", 1, prev_mix_late=5)["mix_late_verdict"] == "UNKNOWN"
    assert amd.analyze("", 0, prev_mix_late=5)["mix_late_verdict"] == "SKIP"


def test_the_cli_prints_the_mix_late_keys():
    body = json.dumps({"audio_mix_mixed_late": "7", "audio_mix_frozen": "1",
                       "audio_mix_age_s": "4"})
    out = subprocess.run([sys.executable, str(_SCRIPTS / "audio_mixer_decision.py"), "analyze",
                          "--box-reachable", "1", "--prev-mix-late", "5"],
                         input=body, capture_output=True, text=True, check=True).stdout
    for line in ("mix_late_verdict=LATE", "mix_late=7", "mix_late_prev=5", "mix_frozen=1",
                 "mix_late_age_s=4"):
        assert line in out.splitlines(), out


# ---------------------------------------------------------------------------------------------
# the real bash watchdog through the AUDIO_MIXER_FETCH_CMD seam
# ---------------------------------------------------------------------------------------------
def _body(late, age=4, head=2):
    return json.dumps({"obs_log_head_age_s": str(head), "audio_mix_mixed_late": str(late),
                       "audio_mix_frozen": "1", "audio_mix_age_s": str(age)})


def _run_watchdog(tmp_path, body, state_file, dry_run=True, notify=None):
    body_file = tmp_path / "body.json"
    body_file.write_text(body, encoding="utf-8")
    fetch = tmp_path / "fetch.sh"
    fetch.write_text(f"#!/usr/bin/env bash\ncat '{body_file}'\n", encoding="utf-8")
    fetch.chmod(fetch.stat().st_mode | stat.S_IEXEC)
    env = dict(os.environ)
    env.update({
        "AUDIO_MIXER_FETCH_CMD": str(fetch),
        "AUDIO_MIXER_BOXES": "stream-snv|10.77.9.204",
        "AUDIO_MIXER_ALERT_STATE_FILE": str(state_file),
        "AIRULESET_NOTIFY": notify or "/nonexistent/airuleset.py",
    })
    args = ["bash", str(_SCRIPTS / "audio-mixer-alert-watchdog.sh")]
    if dry_run:
        args.append("--dry-run")
    r = subprocess.run(args, capture_output=True, text=True, env=env, timeout=60)
    assert r.returncode == 0, r.stderr
    return r.stderr


def _pages(log):
    return [ln for ln in log.splitlines() if "WOULD alert (LATE)" in ln]


def _stored(state_file):
    for ln in state_file.read_text(encoding="utf-8").splitlines():
        if ln.startswith("stream-snv_mix_late_prev="):
            return ln.split("=", 1)[1]
    return None


def test_watchdog_baseline_then_one_rise_pages_once_and_recovers_log_only(tmp_path):
    state = tmp_path / "state"
    first = _run_watchdog(tmp_path, _body(3), state)
    assert "mix_late=BASELINE" in first and not _pages(first), first
    assert _stored(state) == "3"
    # one real rise: LATE, held for the confirm; the stored count keeps its pre-rise value
    second = _run_watchdog(tmp_path, _body(5), state)
    assert "mix_late=LATE" in second and not _pages(second), second
    assert _stored(state) == "3"
    third = _run_watchdog(tmp_path, _body(5), state)
    page = _pages(third)
    assert len(page) == 1 and "alert_now=1" in page[0], third
    assert "z 3 na **5** (+2)" in page[0], page[0]
    assert _stored(state) == "5"
    # the count holds: HEALTHY, one machine-channel RECOVERY line, never a page
    healthy = _run_watchdog(tmp_path, _body(5), state)
    assert "mix_late=HEALTHY" in healthy and not _pages(healthy), healthy
    assert len([ln for ln in healthy.splitlines()
                if "RECOVERY:" in ln and "audio-mix-late" in ln]) == 1, healthy
    again = _run_watchdog(tmp_path, _body(5), state)
    assert not [ln for ln in again.splitlines() if "RECOVERY:" in ln], again


def test_watchdog_unchanged_count_never_pages(tmp_path):
    state = tmp_path / "state"
    for _ in range(4):
        log = _run_watchdog(tmp_path, _body(7), state)
        assert not _pages(log), log
    assert "mix_late=HEALTHY" in log


def test_watchdog_a_lower_count_rebaselines_without_a_page(tmp_path):
    state = tmp_path / "state"
    _run_watchdog(tmp_path, _body(9), state)
    log = _run_watchdog(tmp_path, _body(0), state)
    assert "mix_late=BASELINE" in log and not _pages(log), log
    assert _stored(state) == "0"
    log = _run_watchdog(tmp_path, _body(0), state)
    assert "mix_late=HEALTHY" in log and not _pages(log), log


def _alerts_now(log):
    return [ln for ln in _pages(log) if "alert_now=1" in ln]


def test_watchdog_a_new_session_with_holes_pages_them_from_zero(tmp_path):
    state = tmp_path / "state"
    _run_watchdog(tmp_path, _body(9), state)
    log = _run_watchdog(tmp_path, _body(2), state)
    assert "mix_late=BASELINE" in log and not _pages(log), log
    assert _stored(state) == "0"
    log = _run_watchdog(tmp_path, _body(2), state)
    assert "mix_late=LATE" in log and not _pages(log), log
    log = _run_watchdog(tmp_path, _body(2), state)
    assert len(_alerts_now(log)) == 1 and "z 0 na **2** (+2)" in log, log


def test_watchdog_a_restart_during_the_confirm_still_pages_the_new_sessions_holes(tmp_path):
    state = tmp_path / "state"
    _run_watchdog(tmp_path, _body(3), state)
    log = _run_watchdog(tmp_path, _body(5), state)       # LATE, confirm 1
    assert not _pages(log), log
    log = _run_watchdog(tmp_path, _body(2), state)       # OBS restarted: BASELINE, confirm kept
    assert "mix_late=BASELINE" in log and not _pages(log), log
    log = _run_watchdog(tmp_path, _body(2), state)       # LATE against 0, confirm 2: the page
    assert len(_alerts_now(log)) == 1, log


def test_watchdog_a_count_rising_every_pass_is_one_throttled_incident(tmp_path):
    state = tmp_path / "state"
    logs = [_run_watchdog(tmp_path, _body(late), state) for late in (3, 4, 5, 6, 7, 8)]
    assert sum(len(_alerts_now(log)) for log in logs) == 1, logs
    assert all("mix_late=LATE" in log for log in logs[1:]), logs
    assert _stored(state) == "8"


def test_watchdog_each_separate_episode_pages_again(tmp_path):
    state = tmp_path / "state"
    seq = (3, 4, 4, 4, 5, 5)     # baseline, rise (held), page, quiet (recovery), rise, page
    logs = [_run_watchdog(tmp_path, _body(late), state) for late in seq]
    assert [len(_alerts_now(log)) for log in logs] == [0, 0, 1, 0, 0, 1], logs
    assert [ln for ln in logs[3].splitlines() if "RECOVERY:" in ln and "audio-mix-late" in ln], logs[3]


def test_watchdog_stale_between_two_rises_resets_the_confirm_and_keeps_the_count(tmp_path):
    state = tmp_path / "state"
    _run_watchdog(tmp_path, _body(3), state)
    logs = [_run_watchdog(tmp_path, body, state)
            for body in (_body(5), _body(5, age=amd.DEFAULT_STALE_AFTER_S + 1), _body(5))]
    assert not any(_pages(log) for log in logs), logs
    assert "mix_late=STALE" in logs[1] and _stored(state) == "3"
    # the STALE pass reset the confirm: the third pass is LATE at confirm 1, not a page
    log = _run_watchdog(tmp_path, _body(5), state)
    assert len(_pages(log)) == 1, log


def test_watchdog_unknown_and_skip_hold_without_a_page(tmp_path):
    state = tmp_path / "state"
    _run_watchdog(tmp_path, _body(3), state)
    for body in (json.dumps({"obs_log_head_age_s": "2"}), "not json"):
        log = _run_watchdog(tmp_path, body, state)
        assert not _pages(log), log
    assert _stored(state) == "3"


def test_watchdog_sends_a_time_bucketed_key(tmp_path):
    calls = tmp_path / "notify-calls.txt"
    notify = tmp_path / "notify.py"
    notify.write_text(
        "import sys\n"
        f"open({str(calls)!r}, 'a').write(' '.join(sys.argv[1:]) + '\\n')\n",
        encoding="utf-8")
    state = tmp_path / "state"
    for late in (3, 4, 4):
        _run_watchdog(tmp_path, _body(late), state, dry_run=False, notify=str(notify))
    sent = calls.read_text(encoding="utf-8").splitlines()
    assert len(sent) == 1, sent
    assert re.search(r"--dedup-key audio-mixer-mixlate-stream-snv-\d+$", sent[0]), sent[0]
    assert "✅" not in sent[0]
