"""issue 1439 (ROZHODNUTE 6359382787, owner 23.10.2026) -- the PROGRAM-AUDIO CONTINUITY decision of
the full-path E2E (scripts/program_audio_continuity.py): audio has the same zero-frame-loss bar as
video, a concealed dropout is a LOSS, an unprovable window is UNKNOWN and fails the run.

The fixture is a REAL excerpt of the stream-snv OBS log (`2026-10-23 02-35-32.txt` from line 41400,
read read-only on 23.10.2026): `mbc` lost 1 concealed slot at 09:01:10, 4 silent + 2 concealed at
09:02:00-01 and 2 concealed at 09:03:53, clean from 09:04 on. The window marks are LINE COUNTS of the
log, the same numbers the E2E takes with `wc -l` on the box. Lines the excerpt does not carry
(a buffering-guard re-anchor, another build's status line) are rendered from the SHIPPED C format
strings, so a format change in the C fails here first.
"""
import json
import pathlib
import re
import subprocess
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import program_audio_continuity as pac  # noqa: E402

FIXTURE = _ROOT / "tests/fixtures/program_audio_continuity_1439/stream-snv-2026-10-23-from-41400.txt"
FIRST = 41400
VBAN_C = _ROOT / "vendor/obs-vban/src/vban-source.c"
OBS_AUDIO = _ROOT / "vendor/obs-studio/libobs/obs-audio.c"
OBS_SOURCE = _ROOT / "vendor/obs-studio/libobs/obs-source.c"
SCRIPT = _SCRIPTS / "program_audio_continuity.py"


def _lines():
    return FIXTURE.read_text().splitlines()


def _decide(start_count, end_count, lines=None, name="mbc", vban=True):
    """The decision with the window marks given as the log's LINE COUNTS at start / stop."""
    lines = _lines() if lines is None else lines
    return pac.decide(lines, name, start_count - FIRST + 1, end_count - FIRST + 1, vban)


def _c_format(path, head):
    """The C format string starting at `head`, its literals joined, PRIu64 / %ld / %.*s made python."""
    src = re.sub(r'"\s*PRI[ud]64\s*"', "d", path.read_text())
    src = re.sub(r'"\s*PRI[ud]64\s*,', 'd",', src)  # a format that ENDS in a PRIu64
    start = src.index(head)
    end = src.index('",', start) + 1
    fmt = "".join(re.findall(r'"((?:[^"\\]|\\.)*)"', src[start:end]))
    return fmt.replace("%ld", "%d").replace("%.*s", "%s")


def _status_line(ts, received, lost, late=0, re_anchors=0, stale=0, reordered=0, concealed=0,
                 silence_ms=0.0, missed=0, name="mbc"):
    fmt = _c_format(VBAN_C, '"obs-vban receive: received=%')
    body = fmt % (received, lost, late, re_anchors, stale, reordered, concealed, silence_ms, -50.0,
                  0.5, 5.5, "locked", -40.0, missed, 80, 48000, 2, 6980, "stream", name)
    return f"{ts}: [obs-vban] {body}"


def _guard_line(ts, name="mbc"):
    fmt = _c_format(OBS_AUDIO, '"buffering-guard: \'%s\' %s: its audio ran')
    return f"{ts}: " + fmt % (name, "fell behind the mix (frozen at 85 ms)", 33.4, 33.4, "", 64, 85, 1, 1, 33.4)


def _asrc_line(ts, steps=0, jumps=0, name="mbc"):
    fmt = _c_format(OBS_SOURCE, '"asrc: source \'%s\' estimated=')
    return f"{ts}: " + fmt % (name, 0.0, 0.7, 0.0, 0.04, 60.0, 0, 0.0, 0.0, 0.7, steps, 0.0, 0, 0.0,
                              0.0, 1, jumps, 0.0, "stamp")


def _tele_line(ts, mixed_late=0, name="mbc"):
    """The per-source `audio-telemetry #800 '<src>': ... mixed_late=N` line (obs-audio.c, every 60 s)."""
    fmt = _c_format(OBS_AUDIO, '"audio-telemetry #800 \'%s\': ts_lag_ms=')
    return f"{ts}: " + fmt % (name, 21, 126, 0, 0, mixed_late)


# --- the real stream-snv log -------------------------------------------------------------------

def test_a_window_with_silent_and_concealed_losses_fails_naming_every_lost_slot():
    # start mark after line 41620 (09:01:1x), stop mark after line 41710 (09:02:0x): the opening
    # status line is 09:01:07 (lost 231), the closing one at least 10 s after the stop, 09:02:17
    res = _decide(41620, 41710)
    assert res["verdict"] == "FAIL", res
    assert res["counters"]["lost"]["delta"] == 7
    assert res["lost_concealed"] == 3 and res["lost_silence"] == 4
    assert "lost +7" in res["failures"]
    assert res["interval"]["from"].startswith("09:01:07") and res["interval"]["to"].startswith("09:02:17")
    assert len(res["loss_runs"]) == 3, res["loss_runs"]
    assert any("missing 4 packet(s)" in r and "silence" in r for r in res["loss_runs"])
    assert not res["pass"]


def test_a_concealed_only_loss_is_still_a_loss():
    # 09:03:53: 2 slots concealed (the previous packet repeated) -- the owner's rule: LOST, a FAIL
    res = _decide(41900, 41930)
    assert res["verdict"] == "FAIL", res
    assert res["lost_slots"] == 2 and res["lost_concealed"] == 2 and res["lost_silence"] == 0
    assert any("concealed" in r for r in res["loss_runs"])
    text = "\n".join(pac.summary_lines(res))
    assert "2 slot(s) LOST" in text and "concealed (still LOST)" in text


def test_a_clean_real_window_passes_with_every_check_named():
    res = _decide(42000, 42100)
    assert res["verdict"] == "PASS", res
    assert res["pass"] is True and res["gates_overall_pass"] is True
    assert res["checks"] == {"obs_vban": "ran", "asrc": "ran", "buffering_guard": "ran"}
    assert all(c["delta"] == 0 for k, c in res["counters"].items() if k not in ("received", "reordered"))
    assert res["counters"]["received"]["delta"] > 0
    assert res["asrc"] == {"place_jumps": {"before": 0, "after": 0, "delta": 0},
                           "steps": {"before": 0, "after": 0, "delta": 0}}
    assert res["mix_mixed_late_delta"] == 0


def test_the_closing_status_line_is_one_status_period_after_the_stop():
    # stop mark after 41712 (09:02:0x): the line at 09:02:07 is under 10 s after the stop, so the
    # closing line is 09:02:17 -- a loss decided right after the stop is never cut off
    res = _decide(41600, 41712)
    assert res["interval"]["to"].startswith("09:02:17"), res["interval"]


def test_a_window_with_no_closing_line_yet_is_unknown_and_retryable():
    lines = _lines()[: 42285 - FIRST + 1]
    res = _decide(42200, 42280, lines=lines)
    assert res["verdict"] == "UNKNOWN" and res["retryable"] is True, res
    assert any("one status period after the stop" in u for u in res["unknown_reasons"])


def test_no_status_line_before_the_window_is_unknown_never_a_pass():
    res = _decide(41401, 41500)  # the excerpt starts at 41400: nothing at/before the start
    assert res["verdict"] == "UNKNOWN", res
    assert any("at or before the window start" in u for u in res["unknown_reasons"])
    assert not res["pass"]


def test_an_input_with_no_status_line_at_all_is_unknown():
    res = _decide(42000, 42100, name="Zvuk z mixu")
    assert res["verdict"] == "UNKNOWN", res


# --- rendered lines: OBS restarts, older builds, the other counters -------------------------------

def _window(before, inside, after, asrc=True, tele=None):
    """[before..] START [inside..] STOP [after..] -> (lines, start_count, end_count) with FIRST = 1."""
    pre = ([_asrc_line("10:00:00.000"), _tele_line("10:00:00.100")] if asrc else []) + before
    post = after + ([_asrc_line("10:02:00.000"), _tele_line("10:02:00.100", **(tele or {}))] if asrc else [])
    lines = pre + inside + post
    return lines, len(pre), len(pre) + len(inside)


def _decide_rendered(lines, start, end, **kw):
    return pac.decide(lines, kw.pop("name", "mbc"), start, end, kw.pop("vban", True))


def test_a_counter_that_went_backwards_is_unknown():
    lines, s, e = _window([_status_line("10:00:01.000", 1000, 5)], [],
                          [_status_line("10:00:30.000", 50, 0)])
    res = _decide_rendered(lines, s, e)
    assert res["verdict"] == "UNKNOWN" and any("went backwards" in u for u in res["unknown_reasons"]), res


def test_each_gated_counter_fails_on_its_own():
    for field, kw in (("late", {"late": 1}), ("re_anchors", {"re_anchors": 1}),
                      ("stale", {"stale": 2}), ("emit_missed", {"missed": 3})):
        lines, s, e = _window([_status_line("10:00:01.000", 1000, 0)], [],
                              [_status_line("10:00:30.000", 2000, 0, **kw)])
        res = _decide_rendered(lines, s, e)
        assert res["verdict"] == "FAIL", (field, res)
        assert any(f.startswith(field + " +") for f in res["failures"]), (field, res)


def test_an_older_build_is_graded_on_what_it_prints():
    old = re.compile(r" stale=\d+ reordered=\d+ concealed=\d+| emit_missed=\d+")
    b = old.sub("", _status_line("10:00:01.000", 1000, 2))
    a = old.sub("", _status_line("10:00:30.000", 2000, 2))
    lines, s, e = _window([b], [], [a])
    res = _decide_rendered(lines, s, e)
    assert res["verdict"] == "PASS", res
    assert set(res["not_graded"]) == {"stale", "emit_missed", "concealed"}
    lines, s, e = _window([b], [], [old.sub("", _status_line("10:00:30.000", 2000, 4))])
    res = _decide_rendered(lines, s, e)
    assert res["verdict"] == "FAIL" and res["lost_concealed"] is None, res


def test_a_format_change_inside_the_window_is_unknown():
    old = re.compile(r" stale=\d+ reordered=\d+ concealed=\d+")
    lines, s, e = _window([old.sub("", _status_line("10:00:01.000", 1000, 0))], [],
                          [_status_line("10:00:30.000", 2000, 0)])
    res = _decide_rendered(lines, s, e)
    assert res["verdict"] == "UNKNOWN" and any("one side of the window" in u for u in res["unknown_reasons"])


def test_a_buffering_guard_line_for_the_program_input_fails():
    lines, s, e = _window([_status_line("10:00:01.000", 1000, 0)], [_guard_line("10:00:10.000")],
                          [_status_line("10:00:30.000", 2000, 0)])
    res = _decide_rendered(lines, s, e)
    assert res["verdict"] == "FAIL" and res["failures"] == ["buffering-guard x1"], res
    assert res["guard_lines"][0].startswith("buffering-guard: 'mbc' fell behind the mix")


def test_a_buffering_guard_line_of_another_source_is_not_this_gate():
    lines, s, e = _window([_status_line("10:00:01.000", 1000, 0)],
                          [_guard_line("10:00:10.000", name="NDI test")],
                          [_status_line("10:00:30.000", 2000, 0)])
    assert _decide_rendered(lines, s, e)["verdict"] == "PASS"


def test_an_asrc_placement_jump_or_step_fails():
    for kw, name in (({"jumps": 1}, "place_jumps"), ({"steps": 2}, "steps")):
        lines = [_asrc_line("10:00:00.000"), _tele_line("10:00:00.100"), _status_line("10:00:01.000", 1000, 0),
                 _status_line("10:00:30.000", 2000, 0), _asrc_line("10:01:00.000", **kw),
                 _tele_line("10:01:00.100")]
        res = pac.decide(lines, "mbc", 3, 3, True)
        assert res["verdict"] == "FAIL" and any(f.startswith(f"asrc {name} +") for f in res["failures"]), res


def test_no_asrc_line_after_the_stop_is_unknown_and_retryable():
    lines, s, e = _window([_status_line("10:00:01.000", 1000, 0)], [],
                          [_status_line("10:00:30.000", 2000, 0)], asrc=False)
    lines = [_asrc_line("09:59:59.000"), _tele_line("09:59:59.100")] + lines + [_tele_line("10:01:00.000")]
    res = _decide_rendered(lines, s + 2, e + 2)
    assert res["verdict"] == "UNKNOWN" and res["retryable"] is True, res


def test_a_windows_stream_box_grades_the_guard_and_asrc_only_and_says_so():
    lines, s, e = _window([], [], [])
    res = _decide_rendered(lines, s, e, vban=False)
    assert res["verdict"] == "PASS", res
    assert res["checks"]["obs_vban"].startswith("n/a: no obs-vban source")
    assert res["checks"]["asrc"] == "ran" and res["checks"]["buffering_guard"] == "ran"
    lines, s, e = _window([], [_guard_line("10:00:10.000")], [])
    assert _decide_rendered(lines, s, e, vban=False)["verdict"] == "FAIL"


def test_an_input_name_with_spaces_is_read():
    name = "Zvuk z mixu"
    lines = [_asrc_line("10:00:00.000", name=name), _tele_line("10:00:00.100", name=name),
             _status_line("10:00:01.000", 1000, 0, name=name),
             _status_line("10:00:30.000", 2000, 1, concealed=1, name=name), _asrc_line("10:01:00.000", name=name),
             _tele_line("10:01:00.100", name=name)]
    res = pac.decide(lines, name, 3, 3, True)
    assert res["verdict"] == "FAIL" and res["lost_concealed"] == 1, res


def test_the_status_format_of_the_shipped_c_is_parsed_whole():
    st = pac.parse_status(_status_line("10:00:01.000", 7, 6, 5, 4, 3, 2, 1, 9.5, 8))
    f = st["fields"]
    assert st["source"] == "mbc" and st["stream"] == "stream"
    assert (f["received"], f["lost"], f["late"], f["re_anchors"], f["stale"], f["reordered"],
            f["concealed"], f["emit_missed"]) == ("7", "6", "5", "4", "3", "2", "1", "8")


# --- review round 1 (fresh-context review of the lane) ------------------------------------------

def test_a_mixed_late_event_the_guard_did_not_log_in_the_window_fails():
    # the guard logs a source's later MIXED_LATE events once a minute: a storm that began just before
    # the window logged its line THERE, and its in-window events show only in the per-source counter
    lines, s, e = _window([_guard_line("10:00:00.500"), _status_line("10:00:01.000", 1000, 0)], [],
                          [_status_line("10:00:30.000", 2000, 0)], tele={"mixed_late": 3})
    res = _decide_rendered(lines, s, e)
    assert res["verdict"] == "FAIL" and "mixed_late +3" in res["failures"], res
    assert res["guard_lines"] == [], res  # the guard line sits before the counter interval


def test_no_per_source_telemetry_line_after_the_stop_is_unknown():
    lines, s, e = _window([_status_line("10:00:01.000", 1000, 0)], [], [_status_line("10:00:30.000", 2000, 0)])
    lines = [l for l in lines if "#800 'mbc'" not in l or l.startswith("10:00:00")]
    res = _decide_rendered(lines, s, e)
    assert res["verdict"] == "UNKNOWN" and any("audio-telemetry #800 'mbc'" in u for u in res["unknown_reasons"]), res
    assert res["retryable"] is True


def test_a_telemetry_line_without_mixed_late_is_named_not_graded():
    lines, s, e = _window([_status_line("10:00:01.000", 1000, 0)], [], [_status_line("10:00:30.000", 2000, 0)])
    lines = [l.replace(" mixed_late=0", "") for l in lines]
    res = _decide_rendered(lines, s, e)
    assert res["verdict"] == "PASS" and "mixed_late" in res["not_graded"], res
    assert any("not graded" in l and "mixed_late" in l for l in pac.summary_lines(res))


def test_a_source_re_init_inside_the_window_is_unknown():
    # a port / stream / host change re-inits every counter: a young source then reads no backwards step
    reinit = ["10:00:10.000: [obs-vban] obs-vban receive worker: source 'mbc' stopped (update)",
              "10:00:11.000: [obs-vban] obs-vban receive anchored: source 'mbc' frame=1 rate=48000 ch=2 "
              "receive_buffer_ms=80 -- holding its first second"]
    lines, s, e = _window([_status_line("10:00:01.000", 500, 0)], reinit, [_status_line("10:00:30.000", 6000, 0)])
    res = _decide_rendered(lines, s, e)
    assert res["verdict"] == "UNKNOWN" and any("re-initialized" in u for u in res["unknown_reasons"]), res


def test_a_form_feed_in_a_log_line_never_moves_the_window_marks(tmp_path):
    # wc -l / tail -n +N count newlines only; str.splitlines() also splits on \f, \r, \x1c..: the marks
    # would land one line early and a quiet 10 s before the stop picked a too-early closing line
    rows = [_asrc_line("10:00:00.000"), _tele_line("10:00:00.100"),
            _status_line("10:00:01.000", 1000, 0), "10:00:02.000: odd\x0cline",
            _status_line("10:00:11.000", 1500, 0), _status_line("10:00:20.000", 2000, 0),
            "10:00:31.000: the stop", _status_line("10:00:32.000", 2500, 0),
            _status_line("10:00:42.000", 3000, 4), _asrc_line("10:01:00.000"), _tele_line("10:01:00.100")]
    log = tmp_path / "log.txt"
    log.write_bytes(("\n".join(rows) + "\n").encode())
    r = _cli("--log", str(log), "--input", "mbc", "--start-line", "4", "--end-line", "7")
    assert r.returncode == 1 and "lost +4" in r.stdout, r.stdout + r.stderr


def test_a_missing_opening_line_is_not_retryable_but_asks_for_a_longer_read():
    lines, s, e = _window([], [], [])
    lines = [l for l in lines if "obs-vban receive:" not in l]
    res = _decide_rendered(lines, s, e)
    assert res["verdict"] == "UNKNOWN" and res["retryable"] is False, res
    assert res["opening_missing"] is True


def test_file_order_times_cross_midnight():
    t = pac.file_order_times(["23:59:59.500: a", "no stamp", "00:00:01.000: b"])
    assert t == [86399.5, 86399.5, 86401.0]


# --- the CLI --------------------------------------------------------------------------------------

def _cli(*args, stdin=None):
    return subprocess.run([sys.executable, str(SCRIPT), *args], capture_output=True, text=True, input=stdin)


def test_the_cli_exit_codes_and_the_merge(tmp_path):
    verdict = tmp_path / "verdict.json"
    verdict.write_text(json.dumps({"overall_pass": True}))
    r = _cli("--log", str(FIXTURE), "--input", "mbc", "--first-line", str(FIRST),
             "--start-line", "41620", "--end-line", "41710", "--merge-into", str(verdict))
    assert r.returncode == 1, r.stdout + r.stderr
    assert "FAIL" in r.stdout and "LOST" in r.stdout
    block = json.loads(verdict.read_text())["program_audio_continuity"]
    assert block["verdict"] == "FAIL" and block["gates_overall_pass"] is True
    assert json.loads(verdict.read_text())["overall_pass"] is True  # the merge never rewrites it
    r = _cli("--log", "-", "--input", "mbc", "--first-line", str(FIRST),
             "--start-line", "42000", "--end-line", "42100", stdin=FIXTURE.read_text())
    assert r.returncode == 0, r.stdout + r.stderr
    out = tmp_path / "u.json"
    r = _cli("--unknown", "the window start mark is missing", "--input", "mbc", "--out", str(out))
    assert r.returncode == 2 and json.loads(out.read_text())["verdict"] == "UNKNOWN"
    assert "UNKNOWN fails the run" in r.stdout
