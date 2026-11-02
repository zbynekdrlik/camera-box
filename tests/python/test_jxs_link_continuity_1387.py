"""issue 1387 slice 2 (design 6558387590) -- the pure decision of the BLOCKING JPEG XS link continuity
gate (scripts/jxs_link_continuity.py).

Every window line is rendered from the SHIPPED C format strings (jxs_link_fakes_1387). A box read is
emulated exactly as the box's remote text prints it (`#newest\\t<name>\\t<size>`, then `grep -a -b` of
the bytes [FROM, FROM + LEN)); test_jxs_link_continuity_e2e_1387.py runs the real remote text.
"""
import json
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
REPO = HERE.parent.parent
sys.path.insert(0, str(REPO / "scripts"))

import jxs_link_continuity as jlc  # noqa: E402
from jxs_link_fakes_1387 import (CAMS, LOG_NAME, OUTPUT, OUTPUT_CPP, OUTPUT_FMT, PGM, SOURCE_FMT, T0,  # noqa: E402
                                 Log, box_slice, output_line, recording, run_box, source_line, ts,
                                 window_set)

def links(res):
    return {l["link"]: l for l in res["links"]}


# --- the fixtures are the shipped format --------------------------------------------------------------

def test_the_rendered_lines_carry_every_gated_token_of_the_shipped_formats():
    for k in jlc.INPUT_GATED + ("reopens", "frames") + jlc.INPUT_REPORT:
        assert f" {k}=" in SOURCE_FMT, k
    for k in jlc.OUTPUT_GATED + ("frames",) + jlc.OUTPUT_REPORT:
        assert f" {k}=" in OUTPUT_FMT, k
    line = source_line("10:00:00.000", "JXS cam5 src", concealed=2)
    p = jlc.parse_line(line)
    assert p["kind"] == "source" and p["name"] == "JXS cam5 src" and p["window"]
    assert p["fields"]["concealed"] == "2" and p["fields"]["frames"] == "150"
    o = jlc.parse_line(output_line("10:00:00.004", dropped_busy=1))
    assert o["kind"] == "output" and o["name"] == OUTPUT and o["fields"]["dropped_busy"] == "1"


def test_the_output_name_is_the_one_the_plugin_creates():
    assert jlc.OUTPUT_NAME == OUTPUT
    assert f'#define JXS_OUTPUT_NAME "{OUTPUT}"' in OUTPUT_CPP.read_text()


def test_the_window_period_is_the_sources_status_period():
    decide = (REPO / "vendor/distroav/src/jxs-source-decide.h").read_text()
    m = re.search(r"#define CB_JXS_STATUS_PERIOD_NS (\d+)ULL", decide)
    assert m and int(m.group(1)) == int(jlc.PERIOD_S * 1e9)


# --- the decision --------------------------------------------------------------------------------------

def test_a_clean_recording_passes_every_link_and_counts_the_straddling_windows():
    log, start, stop, st, et = recording(windows=8)
    res = run_box(log, start, stop, start_time=st, stop_time=et)
    ls = links(res)
    assert set(ls) == set(CAMS) | {OUTPUT}
    for l in ls.values():
        assert l["verdict"] == "PASS", l
        assert l["windows"] == 9  # the 8 inside + the closing one after the stop
        assert l["frames"] == 9 * 150
        assert l["max_gap_s"] is not None and l["max_gap_s"] <= jlc.GAP_MAX_S
    assert ls[OUTPUT]["kind"] == "output" and ls["JXS cam3 src"]["kind"] == "input"
    assert jlc.combine([res])["verdict"] == "PASS"


def test_one_concealed_slice_fails_its_link_and_is_named_lost():
    log, start, stop, st, et = recording(src={3: {"JXS cam3 src": {"concealed": 1}}})
    res = run_box(log, start, stop, start_time=st, stop_time=et)
    l = links(res)["JXS cam3 src"]
    assert l["verdict"] == "FAIL" and l["counters"]["concealed"] == 1
    assert any("concealed +1" in f and "LOST" in f for f in l["failures"]), l["failures"]
    assert links(res)["JXS cam5 src"]["verdict"] == "PASS"
    block = jlc.combine([res])
    assert block["verdict"] == "FAIL" and not block["pass"] and block["gates_overall_pass"] is True
    assert any(f.startswith("strih 'JXS cam3 src': concealed +1") for f in block["failures"])


def test_every_gated_input_counter_fails_on_its_own():
    # the spec's list, written out (never read back from the module under test)
    for k in ("lost_slices", "concealed", "undecodable", "dropped_unread", "foreign", "reopens"):
        log, start, stop, st, et = recording(src={2: {"JXS cam5 src": {k: 2}}})
        l = links(run_box(log, start, stop, start_time=st, stop_time=et))["JXS cam5 src"]
        assert l["verdict"] == "FAIL", (k, l)
        assert l["counters"][k] == 2


def test_a_window_with_no_frame_fails():
    log, start, stop, st, et = recording(src={4: {"JXS cam2 src": {"frames": 0, "fps": "0.00"}}})
    l = links(run_box(log, start, stop, start_time=st, stop_time=et))["JXS cam2 src"]
    assert l["verdict"] == "FAIL" and l["zero_frame_windows"] == 1
    assert "1 window(s) with frames=0" in l["failures"]


def test_a_link_missing_its_windows_is_unknown_and_fails_the_block():
    log, start, stop, st, et = recording(names=["JXS cam2 src", "JXS cam5 src"])
    res = run_box(log, start, stop, names=CAMS, start_time=st, stop_time=et)
    l = links(res)["JXS cam3 src"]
    assert l["verdict"] == "UNKNOWN" and not l["pass"]
    assert "no window line of the JPEG XS input 'JXS cam3 src'" in l["unknown_reasons"][0]
    block = jlc.combine([res])
    assert block["verdict"] == "UNKNOWN" and not block["pass"]
    assert not block["retryable"]  # the log went on past the stop: it is not coming


def test_an_output_busy_drop_fails():
    for k in ("dropped_busy", "not_taken", "refused", "send_errors", "encode_errors"):
        log, start, stop, st, et = recording(out={5: {k: 1}})
        l = links(run_box(log, start, stop, start_time=st, stop_time=et))[OUTPUT]
        assert l["verdict"] == "FAIL", (k, l)
        assert l["counters"][k] == 1


def test_one_frame_in_the_closing_window_of_the_output_is_the_stoprecord_skip():
    # owner 2.11.2026: recording stays NVENC, the one frame its teardown costs at StopRecord is accepted for now
    log, start, stop, st, et = recording(closing_out={"dropped_busy": 1, "not_taken": 1})
    l = links(run_box(log, start, stop, start_time=st, stop_time=et))[OUTPUT]
    assert l["verdict"] == "PASS", l
    assert l["counters"]["dropped_busy"] == 1, "the count stays visible"
    assert l["stop_transient"] == {"dropped_busy": 1, "not_taken": 1}


def test_two_frames_in_the_closing_window_fail():
    log, start, stop, st, et = recording(closing_out={"dropped_busy": 2, "not_taken": 2})
    assert links(run_box(log, start, stop, start_time=st, stop_time=et))[OUTPUT]["verdict"] == "FAIL"


def test_the_stoprecord_frame_may_land_in_the_window_before_the_closing_one():
    # E2E 41362630489: StopRecord 00:48:52.4, the frame's window line 00:48:53.7, the stop mark 00:48:54.3
    log, start, stop, st, et = recording(out={7: {"dropped_busy": 1, "not_taken": 1}})
    assert links(run_box(log, start, stop, start_time=st, stop_time=et))[OUTPUT]["verdict"] == "PASS"


def test_one_frame_in_each_of_the_last_two_windows_fails():
    log, start, stop, st, et = recording(out={7: {"dropped_busy": 1, "not_taken": 1}},
                                         closing_out={"dropped_busy": 1, "not_taken": 1})
    assert links(run_box(log, start, stop, start_time=st, stop_time=et))[OUTPUT]["verdict"] == "FAIL"


def test_a_frame_earlier_in_the_recording_still_fails():
    log, start, stop, st, et = recording(out={6: {"dropped_busy": 1, "not_taken": 1}})
    assert links(run_box(log, start, stop, start_time=st, stop_time=et))[OUTPUT]["verdict"] == "FAIL"


def test_the_closing_window_excuses_nothing_else():
    log, start, stop, st, et = recording(closing_out={"dropped_busy": 1, "not_taken": 1, "send_errors": 1})
    assert links(run_box(log, start, stop, start_time=st, stop_time=et))[OUTPUT]["verdict"] == "FAIL"
    log, start, stop, st, et = recording(closing_src={"JXS cam5 src": {"lost_slices": 1}})
    assert links(run_box(log, start, stop, start_time=st, stop_time=et))["JXS cam5 src"]["verdict"] == "FAIL"


def test_fec_repaired_and_the_other_report_only_counters_never_fail():
    log, start, stop, st, et = recording(
        src={1: {n: {"fec_repaired": 7, "decoder_reopens": 1, "report_errors": 0} for n in CAMS}},
        out={1: {"fec_repaired": 3, "stamp_dup": 1, "fec_sent": 999}})
    res = run_box(log, start, stop, start_time=st, stop_time=et)
    for l in res["links"]:
        assert l["verdict"] == "PASS", l
    assert links(res)["JXS cam3 src"]["report"]["fec_repaired"] == 7
    assert links(res)[OUTPUT]["report"]["stamp_dup"] == 1
    assert jlc.combine([res])["verdict"] == "PASS"


def test_windows_spanning_midnight_pass_and_keep_their_spacing():
    log, start, stop, st, et = recording(windows=6, t0=86400 - 20)
    res = run_box(log, start, stop, start_time=st, stop_time=et)
    for l in res["links"]:
        assert l["verdict"] == "PASS", l
        assert abs(l["max_gap_s"] - 5.0) < 0.05, l  # never a -86395 s step
    assert links(res)["JXS cam5 src"]["interval"]["from"].startswith("23:59")
    assert links(res)["JXS cam5 src"]["interval"]["to"].startswith("00:00")


def test_a_missing_window_line_is_a_hole_in_the_link():
    log, start, stop, st, et = recording(windows=8, gap_at=4)
    l = links(run_box(log, start, stop, start_time=st, stop_time=et))["JXS cam5 src"]
    assert l["verdict"] == "FAIL"
    assert any("10.0 s gap between two windows" in f for f in l["failures"]), l["failures"]


def test_a_hole_event_inside_the_window_fails_and_outside_it_does_not():
    lost = source_line(ts(T0 + 32.0), "JXS cam3 src").split(": [distroav] ")[0] + \
        ": [distroav] jxs-source 'JXS cam3 src': not connected -- no frame for 3 s (or none yet) (issue 1387)"
    log, start, stop, st, et = recording(extra={2: [lost]})
    l = links(run_box(log, start, stop, start_time=st, stop_time=et))["JXS cam3 src"]
    assert l["verdict"] == "FAIL"
    assert any("a hole in the link" in f and "not connected" in f for f in l["failures"])
    out_restart = f"{ts(T0 + 33.0)}: [distroav] jxs-output '{OUTPUT}': started -- the program (issue 1387)"
    log, start, stop, st, et = recording(extra={3: [out_restart]})
    assert links(run_box(log, start, stop, start_time=st, stop_time=et))[OUTPUT]["verdict"] == "FAIL"
    # the same event long before the start (an earlier reconnect): outside the window
    log = Log()
    log.add([lost])
    log2, start, stop, st, et = recording()
    log.add([b.decode() for b in log2.data.split(b"\n") if b])
    shift = len(lost) + 1
    l = links(run_box(log, start + shift, stop + shift, start_time=st, stop_time=et, back=100000))["JXS cam3 src"]
    assert l["verdict"] == "PASS", l


def test_obs_restarted_after_the_start_is_unknown_for_every_link():
    log, start, stop, st, et = recording()
    res = run_box(log, start, stop, newest="2026-11-01 09-00-00.txt", start_time=st, stop_time=et)
    assert all(l["verdict"] == "UNKNOWN" for l in res["links"])
    assert "OBS restarted" in res["box_reasons"][0]


def test_a_closing_window_not_in_the_log_yet_is_retryable():
    log, start, stop, st, et = recording(post=0)
    res = run_box(log, start, stop, start_time=st, stop_time=et)
    block = jlc.combine([res])
    assert block["verdict"] == "UNKNOWN" and block["retryable"] is True
    assert all("after the stop yet" in l["unknown_reasons"][0] for l in res["links"])


def test_a_link_that_stopped_before_the_stop_fails_once_the_log_went_on():
    log, start, stop, st, et = recording(windows=8, post=0)
    # the log goes on (another link keeps writing), cam3 wrote nothing after its 4th window
    log2 = Log()
    keep = [b.decode() for b in log.data.split(b"\n") if b]
    cut = [i for i, l in enumerate(keep) if "'JXS cam3 src': fps=" in l]
    drop = set(cut[3 + 4:])  # pre=3: the 4 first windows inside stay
    log2.add([l for i, l in enumerate(keep) if i not in drop])
    stop2 = len(log2.data) - (len(log.data) - stop)
    start2 = start
    log2.add(window_set(T0 + 60 * 5, ["JXS cam2 src", "JXS cam5 src"], True))
    log2.add(window_set(T0 + 61 * 5, ["JXS cam2 src", "JXS cam5 src"], True))
    l = links(run_box(log2, start2, stop2, start_time=st, stop_time=et))["JXS cam3 src"]
    assert l["verdict"] == "FAIL", l
    assert any("before the stop" in f for f in l["failures"]), l["failures"]


def test_no_window_before_the_start_passes_within_one_window_of_slack():
    log, start, stop, st, et = recording(pre=0)
    res = run_box(log, start, stop, start_time=st, stop_time=et)
    assert all(l["verdict"] == "PASS" for l in res["links"]), res
    # a link that began one window too late fails
    log, start, stop, st, et = recording(pre=0)
    late = jlc._sod(f"{st}: ") - 7.0  # the start 7 s earlier: the first window ends 11.9 s after it
    res = run_box(log, start, stop, start_time=ts(late), stop_time=et)
    l = links(res)["JXS cam5 src"]
    assert l["verdict"] == "FAIL" and any("after the start" in f for f in l["failures"]), l


def test_a_start_line_cut_by_the_read_and_a_last_line_cut_by_the_cap_are_dropped():
    log, start, stop, st, et = recording()
    first_jxs = log.data.index(b"[distroav] jxs-source") - len("10:00:00.000: ")
    from_off = first_jxs + 3  # the read begins inside a window line's stamp
    data = box_slice(log, from_off, stop - from_off + 4096)
    header, entries = jlc.parse_slice(data, from_off, stop - from_off + 4096)
    assert header == {"newest": LOG_NAME, "size": len(log.data)}
    assert all(e[0] > from_off for e in entries)  # the offset-0 fragment is gone
    assert all(jlc._TS_RE.match(t) for _, t in entries)
    # a cap that cuts a window line in two: dropped, never parsed as a short window
    cut_at = log.data.index(b"[distroav] jxs-source 'JXS cam2 src'", stop)
    length = cut_at + 40 - from_off  # inside `fps=30.00 frames=150 ...`
    data = box_slice(log, from_off, length)
    assert b"'JXS cam2 src': fps=30.00 fr" in data  # the box did send the cut line
    _, entries = jlc.parse_slice(data, from_off, length)
    assert entries and all(e[0] + len(e[1].encode()) + 1 <= from_off + length for e in entries)
    assert all(e[0] < cut_at - 14 for e in entries)


def test_a_read_capped_before_the_closing_windows_grades_the_end_with_one_window_of_slack():
    log, start, stop, st, et = recording()
    res = run_box(log, start, stop, close=50, start_time=st, stop_time=et)  # the log goes on past the cap
    block = jlc.combine([res])
    assert block["verdict"] == "PASS" and not block["retryable"], block


def test_a_link_failing_and_unknown_at_once_is_a_fail():
    log, start, stop, st, et = recording(src={2: {"JXS cam5 src": {"lost_slices": 2}}})
    from_off = max(0, start - 4096)
    length = stop - from_off + 4096
    data = box_slice(log, from_off, length)
    data = data.replace(b"'JXS cam5 src': fps=30.00 frames=150 lost_slices=2 concealed=0 undecodable=0",
                        b"'JXS cam5 src': fps=30.00 frames=150 lost_slices=2 concealed=0")
    res = jlc.decide_box("strih", data, LOG_NAME, from_off, length, start, stop, st, et, CAMS, True)
    l = links(res)["JXS cam5 src"]
    assert l["unknown_reasons"] and l["failures"]
    assert l["verdict"] == "FAIL" and l["retryable"] is False


def test_a_late_link_after_midnight_is_placed_on_the_right_day():
    # every link but cam3 wrote windows up to 00:00:00; the start mark is at 00:00:00.100; cam3 begins
    # at 00:00:05, within one window of the start
    log, start, stop, st, et = recording(t0=86400 - 10)
    assert st == "00:00:00.100"
    lines = log.data.split(b"\n")
    kept, pos = [], 0
    for line in lines:
        if line and not (pos < start and b"'JXS cam3 src': fps=" in line):
            kept.append(line.decode())
        pos += len(line) + 1
    log2 = Log()
    log2.add(kept)
    shift = len(log.data) - len(log2.data)
    res = run_box(log2, start - shift, stop - shift, start_time=st, stop_time=et)
    l = links(res)["JXS cam3 src"]
    assert l["verdict"] == "PASS", l
    # the read spans midnight: the other links' windows before the start are on the previous day
    before = log2.data[max(0, start - shift - 4096):start - shift]
    assert b"23:59:55.000: [distroav] jxs-source" in before and b"00:00:00.000: [distroav] jxs-source" in before


def test_a_window_line_without_a_gated_token_is_unknown():
    log, start, stop, st, et = recording()
    data = box_slice(log, max(0, start - 4096), stop - max(0, start - 4096) + 4096)
    data = data.replace(b" dropped_unread=0", b"")
    from_off = max(0, start - 4096)
    res = jlc.decide_box("strih", data, LOG_NAME, from_off, stop - from_off + 4096, start, stop, st, et, CAMS, True)
    l = links(res)["JXS cam5 src"]
    assert l["verdict"] == "UNKNOWN" and "dropped_unread" in l["unknown_reasons"][0]


def test_a_fail_wins_over_an_unknown_and_the_block_is_not_retried():
    log, start, stop, st, et = recording(src={1: {"JXS cam5 src": {"lost_slices": 4}}})
    a = run_box(log, start, stop, start_time=st, stop_time=et)
    b = jlc.unknown_box("stream", "no window START mark", [PGM], False)
    block = jlc.combine([a, b])
    assert block["verdict"] == "FAIL" and block["retryable"] is False
    assert any("stream 'JXS PGM src': no window START mark" == u for u in block["unknown_reasons"])


def test_an_empty_block_is_unknown():
    assert jlc.combine([])["verdict"] == "UNKNOWN"


def test_the_marks_must_fit_the_read():
    log, start, stop, st, et = recording()
    res = jlc.decide_box("strih", box_slice(log, 0, 10), LOG_NAME, 0, 10, start, stop, st, et, CAMS, True)
    assert all(l["verdict"] == "UNKNOWN" for l in res["links"])


# --- the CLI ------------------------------------------------------------------------------------------

def _cli(*args):
    return subprocess.run([sys.executable, str(REPO / "scripts/jxs_link_continuity.py"), *map(str, args)],
                          capture_output=True, text=True, timeout=60)


def test_the_cli_decides_combines_and_merges(tmp_path):
    log, start, stop, st, et = recording(src={2: {"JXS cam2 src": {"concealed": 3, "lost_slices": 3}}})
    from_off = max(0, start - 4096)
    length = stop - from_off + 4096
    (tmp_path / "slice.txt").write_bytes(box_slice(log, from_off, length))
    r = _cli("--box", "strih", "--slice", tmp_path / "slice.txt", "--name", LOG_NAME, "--from", from_off,
             "--len", length, "--start", start, "--stop", stop, "--start-time", st, "--stop-time", et,
             *sum((["--input", n] for n in CAMS), []), "--program-output", "--box-out", tmp_path / "s.json")
    assert r.returncode == 0, r.stderr
    r = _cli("--box", "stream", "--unknown", "no window STOP mark", "--input", PGM, "--box-out", tmp_path / "p.json")
    assert r.returncode == 0, r.stderr
    (tmp_path / "v.json").write_text(json.dumps({"overall_pass": True}))
    r = _cli("--combine", tmp_path / "s.json", tmp_path / "p.json", "--out", tmp_path / "b.json",
             "--merge-into", tmp_path / "v.json")
    assert r.returncode == 1, r.stdout + r.stderr
    assert "JPEG XS link continuity: FAIL" in r.stdout
    assert "FAIL: strih 'JXS cam2 src': lost_slices +3" in r.stdout
    v = json.loads((tmp_path / "v.json").read_text())
    assert v["overall_pass"] is True and v["jxs_link_continuity"]["verdict"] == "FAIL"
    r = _cli("--from-block", tmp_path / "b.json")
    assert r.returncode == 1


def test_the_cli_exit_codes_pass_and_unknown(tmp_path):
    log, start, stop, st, et = recording(names=[PGM], output=False)
    from_off = max(0, start - 4096)
    length = stop - from_off + 4096
    (tmp_path / "slice.txt").write_bytes(box_slice(log, from_off, length))
    r = _cli("--box", "stream", "--slice", tmp_path / "slice.txt", "--name", LOG_NAME, "--from", from_off,
             "--len", length, "--start", start, "--stop", stop, "--start-time", st, "--stop-time", et,
             "--input", PGM, "--box-out", tmp_path / "p.json")
    assert r.returncode == 0
    assert _cli("--combine", tmp_path / "p.json").returncode == 0
    (tmp_path / "bad.json").write_text("not json")
    r = _cli("--combine", tmp_path / "bad.json")
    assert r.returncode == 2 and "UNKNOWN" in r.stdout
