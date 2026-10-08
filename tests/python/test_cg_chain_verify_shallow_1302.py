"""issue 1302 (design 6050999445, Approach 1) -- cg-chain-verify grades a shallow-latched input's head
age against its latched depth.

A shallow N==1 input (the strih-lx `CG-obs` at a 3 ms pin) is held at a latched depth D by design
(issue 1367: audit `shallow_depth=`), so its head age `ts_head_skew_ms` is about D x interval
(100 ms at D=3 on a 30 fps canvas). The verdict used to grade that absolute head age against the
20 ms bar, so every window of a correctly latched shallow input FAILed. Now:

* a SAMPLED tick of a line carrying `shallow_depth=` > 0 and a known `@ <fps>` canvas rate is graded
  by its EXCURSION `|ts_head_skew_ms - shallow_depth x 1000 / fps|` against the same bar;
* every other sample keeps the absolute `|ts_head_skew_ms|` (a line without the token is graded
  byte-identically to before; an unknown canvas rate stays absolute = fail-closed; an unsampled tick,
  `ts_present=0`, keeps its zero term);
* the `shallow_latches=` delta is a new `d_shallow_latches` column (appended to the CSV, `dLTCH`
  before VERDICT in the table) and a delta > 0 FAILs with a named reason (a re-latch = a lock event).
"""
import importlib.util
import os
import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_TOOL = _ROOT / "scripts" / "cg-chain-verify.sh"
_LIB = _ROOT / "scripts" / "lib" / "cg-chain-verify.sh"
_AUDIT = _ROOT / "scripts" / "rig-health-audit.py"

# Two real lines read off strih-lx on 8.10.2026 (the newest OBS log, `CG-obs`), verbatim incl. the
# U+2248 glyph the strip removes. Both: shallow_depth=3, ts_head_skew_ms=100, flat counters.
_LIVE = (
    "02:39:55.900: genlock-fifo audit 'CG-obs': received=4549098 consumed=4543103 underruns=218718 "
    "holds=0 overruns=4 backward_steps=0 dropped_due=5873 relocks=571 late_holds=0 locked=1 depth=3 "
    "peak=30 latency_ms=3 (≈0 frames @ 30.000fps) src_latency_ms=3 global_latency_ms=3 preload=1 "
    "(=33 ms) reserve_ms=3 cap=30 empty_run=0 (re-arm@30) ts_present=1791427195866666666 ts_due=3 "
    "ts_head_skew_ms=100 backward_regime_ticks=0 converge_sheds=2863 wall_qpc_drift_ms=3148 "
    "stamp_dup=180 stamp_gap=30675 n1_grows=2232 n2_early=0 shallow_depth=3 shallow_capped=0 "
    "shallow_latches=89 audio_slew_ms=0 audio_slews=0 audio_steps=0 audio_withheld=0 "
    "audio_place_err_ms=0 audio_hold=off video_delay_ms=100 audio_health=0 audio_enabled=0 "
    "audio_delay_ms=0 audio_pairing_offset_ms=-99 (#70/#97/#126/#147/#148/#184/#235/#245/#401/#1049/"
    "#800/#1303/#1355)\n"
    "02:40:00.933: genlock-fifo audit 'CG-obs': received=4549249 consumed=4543254 underruns=218718 "
    "holds=0 overruns=4 backward_steps=0 dropped_due=5873 relocks=571 late_holds=0 locked=1 depth=3 "
    "peak=30 latency_ms=3 (≈0 frames @ 30.000fps) src_latency_ms=3 global_latency_ms=3 preload=1 "
    "(=33 ms) reserve_ms=3 cap=30 empty_run=0 (re-arm@30) ts_present=1791427200900000000 ts_due=3 "
    "ts_head_skew_ms=100 backward_regime_ticks=0 converge_sheds=2863 wall_qpc_drift_ms=3148 "
    "stamp_dup=180 stamp_gap=30675 n1_grows=2232 n2_early=0 shallow_depth=3 shallow_capped=0 "
    "shallow_latches=89 audio_slew_ms=0 audio_slews=0 audio_steps=0 audio_withheld=0 "
    "audio_place_err_ms=0 audio_hold=off video_delay_ms=100 audio_health=0 audio_enabled=0 "
    "audio_delay_ms=0 audio_pairing_offset_ms=-99 (#70/#97/#126/#147/#148/#184/#235/#245/#401/#1049/"
    "#800/#1303/#1355)\n"
)


def _line(t, skew, depth=None, latches=None, fps="30.000", ts_present=1791427195866666666,
          src="CG-obs", dropped=0, underruns=0):
    """One `genlock-fifo audit` line in the deployed token layout. depth/latches None = a pre-1367
    line that carries no shallow token at all."""
    shallow = ""
    if depth is not None:
        shallow = f"shallow_depth={depth} shallow_capped=0 shallow_latches={latches or 0} "
    return (f"02:40:{t:02d}.000: genlock-fifo audit '{src}': received={1000 + t} consumed={999 + t} "
            f"underruns={underruns} holds=0 overruns=0 backward_steps=0 dropped_due={dropped} relocks=0 "
            f"late_holds=0 locked=1 depth=3 peak=5 latency_ms=3 (≈0 frames @ {fps}fps) "
            f"src_latency_ms=3 global_latency_ms=3 ts_present={ts_present} ts_due=3 "
            f"ts_head_skew_ms={skew} backward_regime_ticks=0 converge_sheds=0 n1_grows=0 {shallow}"
            f"audio_enabled=0 audio_delay_ms=0 audio_pairing_offset_ms=-99 (#70/#1303)\n")


def _run(tmp_path, text, src="CG-obs", args=()):
    log = tmp_path / "strih.log"
    log.write_text(text, encoding="utf-8")
    env = {k: v for k, v in os.environ.items() if not k.startswith("CG_CHAIN_")}
    env.update({"CG_CHAIN_STRIH_LOG": str(log), "CG_CHAIN_STRIH_SRC": src, "CG_CHAIN_NOW": "T"})
    return subprocess.run(["bash", str(_TOOL), "--hops", "strih", *args],
                          capture_output=True, text=True, env=env)


def _lib(tmp_path, text, body, *args):
    """Source the real lib under the caller's `set -euo pipefail` (scripts/cg-chain-verify.sh), set
    `$LOG` = the fixture text, run `body`; return stdout (rc must be 0). Every value reaches bash as
    a positional ARGUMENT (`$1`.. inside `body`), never inside the script text."""
    log = tmp_path / "lib.log"
    log.write_text(text, encoding="utf-8")
    script = f'set -euo pipefail\n. "$1"\nLOG="$(cat "$2")"\nshift 2\n{body}\n'
    r = subprocess.run(["bash", "-c", script, "harness", str(_LIB), str(log), *args],
                       capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    # a lib function never writes stderr: an awk runtime error (gawk aborts on a division by zero
    # where mawk prints inf) would otherwise pass as an empty facet
    assert r.stderr == "", r.stderr
    return r.stdout


def _row(out, src="CG-obs"):
    rows = [ln for ln in out.splitlines() if ln.startswith("strih") and src in ln]
    assert len(rows) == 1, out
    return rows[0].split()


def _reasons(out):
    return [ln.split("reason:", 1)[1].strip() for ln in out.splitlines() if "reason:" in ln]


# ---------------------------------------------------------------------------------------------
# the shallow skew term
# ---------------------------------------------------------------------------------------------
def test_the_live_strih_lx_cg_obs_lines_pass_on_skew(tmp_path):
    # Depth 3 + head age 100 ms on a 30 fps canvas = exactly the latched target: excursion 0.
    r = _run(tmp_path, _LIVE)
    assert r.returncode == 0, r.stdout + r.stderr
    row = _row(r.stdout)
    assert row[-1] == "PASS", r.stdout
    assert row[4] == "0", r.stdout  # SKEWms = the graded term
    assert "OVERALL: PASS" in r.stdout


def test_depth_3_skew_100_passes_on_skew(tmp_path):
    text = "".join(_line(t, 100, depth=3, latches=89) for t in (0, 5, 10))
    r = _run(tmp_path, text)
    assert r.returncode == 0, r.stdout + r.stderr
    assert _row(r.stdout)[-1] == "PASS"


def test_depth_3_skew_167_fails_with_a_named_excursion(tmp_path):
    text = _line(0, 100, depth=3, latches=89) + _line(5, 167, depth=3, latches=89)
    r = _run(tmp_path, text)
    assert r.returncode == 3, r.stdout
    row = _row(r.stdout)
    assert row[-1] == "FAIL" and row[4] == "67", r.stdout
    skew = [x for x in _reasons(r.stdout) if "excursion" in x]
    assert len(skew) == 1, r.stdout
    assert "67 ms" in skew[0] and "depth 3" in skew[0] and "100 ms" in skew[0], skew[0]


def test_the_window_term_is_the_worst_sample(tmp_path):
    # 19 ms excursion stays within the 20 ms bar, 21 does not.
    ok = _line(0, 100, depth=3, latches=7) + _line(5, 119, depth=3, latches=7)
    assert _run(tmp_path, ok).returncode == 0
    bad = _line(0, 100, depth=3, latches=7) + _line(5, 121, depth=3, latches=7)
    r = _run(tmp_path, bad)
    assert r.returncode == 3 and _row(r.stdout)[4] == "21", r.stdout


def test_the_target_rounds_two_and_four_frames_on_a_30_fps_canvas(tmp_path):
    # D=2 -> 66.67 ms, D=4 -> 133.33 ms; the integer head age 67 / 133 is an excursion of 0.
    for depth, skew in ((2, 67), (4, 133)):
        text = _line(0, skew, depth=depth, latches=1) + _line(5, skew, depth=depth, latches=1)
        r = _run(tmp_path, text)
        assert r.returncode == 0, (depth, r.stdout)
        assert _row(r.stdout)[4] == "0", (depth, r.stdout)
    # D=2, head age 88 -> excursion 21.33 -> 21 > 20.
    text = _line(0, 88, depth=2, latches=1) + _line(5, 88, depth=2, latches=1)
    r = _run(tmp_path, text)
    assert r.returncode == 3 and _row(r.stdout)[4] == "21", r.stdout


def test_the_excursion_rounds_half_up_at_the_bar(tmp_path):
    # D=2 -> 66.67 ms; head age 46 -> excursion 20.67 -> 21 > 20 FAILs (a truncation would read 20
    # and pass it).
    text = _line(0, 46, depth=2, latches=1) + _line(5, 46, depth=2, latches=1)
    r = _run(tmp_path, text)
    assert r.returncode == 3, r.stdout
    assert _row(r.stdout)[4] == "21", r.stdout


def test_the_interval_comes_from_the_line_canvas_rate(tmp_path):
    # A 60 fps canvas: D=3 -> 50 ms.
    text = _line(0, 50, depth=3, latches=2, fps="60.000") + _line(5, 50, depth=3, latches=2, fps="60.000")
    assert _run(tmp_path, text).returncode == 0
    text = _line(0, 100, depth=3, latches=2, fps="60.000") + _line(5, 100, depth=3, latches=2, fps="60.000")
    r = _run(tmp_path, text)
    assert r.returncode == 3 and _row(r.stdout)[4] == "50", r.stdout


def test_an_unknown_canvas_rate_keeps_the_absolute_grading(tmp_path):
    # fps unknown -> the head age cannot be placed -> the absolute 100 ms FAILs (fail-closed).
    text = _line(0, 100, depth=3, latches=2, fps="0.000") + _line(5, 100, depth=3, latches=2, fps="0.000")
    r = _run(tmp_path, text)
    assert r.returncode == 3 and _row(r.stdout)[4] == "100", r.stdout
    # the facet is still read (no awk error): nothing graded, the latch delta kept
    out = _lib(tmp_path, text, "printf '%s\\n' \"$LOG\" | cg_chain_shallow_window 'CG-obs'")
    assert out.strip() == "0|100|3||0|", out


def test_the_reason_names_the_sample_that_produced_the_max(tmp_path):
    # The worst term comes from a sample with no canvas rate (graded absolutely), while another
    # sample of the window was graded against D: the reason must not blame the latched depth.
    text = _line(0, 100, depth=3, latches=4) + _line(5, 100, depth=3, latches=4, fps="0.000")
    r = _run(tmp_path, text)
    assert r.returncode == 3, r.stdout
    reasons = _reasons(r.stdout)
    skew = [x for x in reasons if "excursion" in x]
    assert len(skew) == 1 and skew[0].startswith("skew excursion 100 ms"), reasons
    assert "graded absolutely" in skew[0] and "latched shallow depth" not in skew[0], skew
    # the table's shallow: line follows the same provenance (never "= ms @ fps" with empty values)
    info = [ln for ln in r.stdout.splitlines() if "shallow:" in ln]
    assert len(info) == 1 and "carried no latched depth or canvas rate" in info[0], r.stdout
    out = _lib(tmp_path, text, "printf '%s\\n' \"$LOG\" | cg_chain_shallow_window 'CG-obs'")
    assert out.strip() == "1|100|3||0|", out


def test_a_source_name_with_an_fps_like_text_never_sets_the_rate(tmp_path):
    # The rate is read from the `(... frames @ F fps)` parenthetical, never from the source name.
    src = "cg @ 60fps"
    text = _line(0, 100, depth=3, latches=1, src=src) + _line(5, 100, depth=3, latches=1, src=src)
    r = _run(tmp_path, text, src=src)
    assert r.returncode == 0, r.stdout


def test_an_unsampled_tick_is_not_an_excursion(tmp_path):
    # genlock_clear_ts_sample zeroes ts_present/ts_head_skew_ms on a count-gate / empty tick.
    text = (_line(0, 100, depth=3, latches=5) + _line(5, 0, depth=3, latches=5, ts_present=0)
            + _line(10, 100, depth=3, latches=5))
    r = _run(tmp_path, text)
    assert r.returncode == 0, r.stdout
    assert _row(r.stdout)[4] == "0", r.stdout


def test_a_shallow_depth_of_zero_keeps_the_absolute_grading(tmp_path):
    text = _line(0, 100, depth=0, latches=0) + _line(5, 100, depth=0, latches=0)
    r = _run(tmp_path, text)
    assert r.returncode == 3, r.stdout
    assert "skew excursion 100 ms > bound 20 ms -- presentation not flat" in _reasons(r.stdout)


# ---------------------------------------------------------------------------------------------
# lines without shallow_depth: grading byte-identical
# ---------------------------------------------------------------------------------------------
def test_a_line_without_shallow_depth_is_graded_as_before(tmp_path):
    text = _line(0, 8) + _line(5, 25)
    r = _run(tmp_path, text)
    assert r.returncode == 3, r.stdout
    row = _row(r.stdout)
    assert row[4] == "25" and row[-1] == "FAIL", r.stdout
    assert _reasons(r.stdout) == ["skew excursion 25 ms > bound 20 ms -- presentation not flat"]
    assert "shallow:" not in r.stdout


def test_the_shallow_window_is_empty_on_a_pre_1367_log(tmp_path):
    out = _lib(tmp_path, _line(0, 8) + _line(5, 25),
               "printf '%s\\n' \"$LOG\" | cg_chain_shallow_window 'CG-obs'")
    assert out == ""


def test_the_verdict_with_an_empty_fourth_argument_is_byte_identical(tmp_path):
    # The 1-3 argument form is the resolume_playback::evaluate replica; an empty shallow argument
    # must not change one byte of it.
    cases = ["30|3|8|0|0|0|0|0|1", "30|3|25|0|0|0|0|0|1", "1|3|8|0|0|0|0|0|1",
             "30|3|50|2|0|1|0|0|1", "30|3|8|0|2|0|1|4|0", ""]
    for case in cases:
        out = _lib(tmp_path, "", 'A=$(cg_chain_verdict "$1" 20 2); B=$(cg_chain_verdict "$1" 20 2 "");'
                                 ' if [ "$A" = "$B" ]; then echo SAME; else echo DIFF; echo "$A"; echo "$B"; fi',
                   case)
        assert out.strip() == "SAME", (case, out)


def test_the_summary_line_is_unchanged_for_a_shallow_source(tmp_path):
    # cg_chain_summarize_window stays the jitter_audit::summarize replica (absolute max skew).
    out = _lib(tmp_path, _line(0, 100, depth=3, latches=89) + _line(5, 167, depth=3, latches=90),
               "printf '%s\\n' \"$LOG\" | cg_chain_summarize_window 'CG-obs'")
    assert out.strip() == "2|3|167|0|0|0|0|0|1"


def test_the_summary_max_skew_is_numeric_not_text(tmp_path):
    # The awk replica used to compare skews as STRINGS ("8" above "25", "99" above "100"), so it
    # diverged from jitter_audit::summarize, which takes a numeric max.
    for skews, want in (((8, 25), "25"), ((99, 100), "100"), ((8, 25, -100, 99), "100")):
        text = "".join(_line(5 * i, s) for i, s in enumerate(skews))
        out = _lib(tmp_path, text, "printf '%s\\n' \"$LOG\" | cg_chain_summarize_window 'CG-obs'")
        assert out.strip().split("|")[2] == want, (skews, out)


def test_the_shallow_window_fields(tmp_path):
    # graded|skew_term|depth|target_ms|d_shallow_latches|fps
    out = _lib(tmp_path, _line(0, 100, depth=3, latches=89) + _line(5, 167, depth=3, latches=90),
               "printf '%s\\n' \"$LOG\" | cg_chain_shallow_window 'CG-obs'")
    assert out.strip() == "2|67|3|100|1|30.000"


# ---------------------------------------------------------------------------------------------
# d_shallow_latches
# ---------------------------------------------------------------------------------------------
def test_a_re_latch_in_the_window_fails_with_its_reason(tmp_path):
    text = _line(0, 100, depth=3, latches=89) + _line(5, 100, depth=3, latches=91)
    r = _run(tmp_path, text)
    assert r.returncode == 3, r.stdout
    row = _row(r.stdout)
    assert row[-1] == "FAIL" and row[-2] == "2", r.stdout
    latch = [x for x in _reasons(r.stdout) if "re-latch" in x]
    assert len(latch) == 1 and latch[0].startswith("2 shallow re-latch"), r.stdout
    # every latch counts, a lock or a re-measure (obs-source.c increments on any latch)
    assert "re-measure" in latch[0], latch[0]


def test_the_table_carries_dltch_before_the_verdict(tmp_path):
    r = _run(tmp_path, _LIVE)
    header = r.stdout.splitlines()[0].split()
    assert header[-2:] == ["dLTCH", "VERDICT"], header
    assert _row(r.stdout)[-2] == "0"
    # a log without the latch token shows `-`
    r2 = _run(tmp_path, _line(0, 8) + _line(5, 9))
    assert _row(r2.stdout)[-2] == "-", r2.stdout


def test_the_csv_appends_d_shallow_latches(tmp_path):
    csv = tmp_path / "soak.csv"
    _run(tmp_path, _line(0, 100, depth=3, latches=89) + _line(5, 100, depth=3, latches=90),
         args=("--csv", str(csv), "--report-only"))
    _run(tmp_path, _line(0, 8) + _line(5, 9), args=("--csv", str(csv), "--report-only"))
    lines = csv.read_text().splitlines()
    assert lines[0].endswith(",audio_pairing_offset_ms,d_shallow_latches"), lines[0]
    ncol = len(lines[0].split(","))
    assert all(len(ln.split(",")) == ncol for ln in lines), lines
    shallow_row, plain_row = lines[1].split(","), lines[2].split(",")
    assert shallow_row[-1] == "1" and shallow_row[4] == "0", lines[1]  # graded skew term, latch delta
    assert plain_row[-1] == "" and plain_row[4] == "9", lines[2]


def test_a_csv_from_an_older_tool_version_is_refused_not_made_ragged(tmp_path):
    csv = tmp_path / "old.csv"
    old = ("ts_utc,hop,source,verdict,max_abs_skew_ms,d_dropped,d_underruns,d_relocks,d_late_holds,"
           "d_backward_regime,asrc_ppm,audio_enabled,audio_delay_ms,audio_pairing_offset_ms\n"
           "T,strih,cg,PASS,8,0,0,0,0,0,7.62,0,0,-99\n")
    csv.write_text(old)
    r = _run(tmp_path, _LIVE, args=("--csv", str(csv)))
    assert r.returncode == 2, r.stdout + r.stderr
    assert "different column header" in r.stderr
    assert csv.read_text() == old


def test_the_csv_row_helper_takes_the_appended_column(tmp_path):
    out = _lib(tmp_path, "", 'cg_chain_csv_header; cg_chain_csv_row "$@"',
               "T", "strih", "cg", "PASS", "0", "0", "0", "0", "0", "0", "7.62", "0", "0", "-99", "3")
    header, row = out.splitlines()
    assert len(header.split(",")) == len(row.split(",")) and row.endswith(",0,0,-99,3"), out


def test_help_prints_the_shallow_paragraph():
    r = subprocess.run(["bash", str(_TOOL), "--help"], capture_output=True, text=True)
    assert "SHALLOW-LATCHED INPUTS (issue 1302)" in r.stdout
    assert "the token is graded exactly as before." in r.stdout


# ---------------------------------------------------------------------------------------------
# the rig-health fold still reads the verdict as the last token
# ---------------------------------------------------------------------------------------------
def test_the_rig_health_fold_still_reads_the_verdict_as_the_last_token(tmp_path):
    # This pins the FOLD over the tool's table (dLTCH sits before VERDICT, the `shallow:` line ends
    # in no verdict word). It does not claim the rig-health row itself reads strih-lx `CG-obs`: that
    # row still runs the strih hop with the default source.
    spec = importlib.util.spec_from_file_location("rig_health_audit_1302", _AUDIT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    r = _run(tmp_path, _LIVE, args=("--report-only",))
    detail = mod.cg_chain_detail_from_output(r.stdout)
    assert "overall=PASS" in detail and "sources_pass=1" in detail and "sources_fail=0" in detail, detail
    info = [ln for ln in r.stdout.splitlines() if "shallow:" in ln]
    assert len(info) == 1 and info[0].split()[-1] not in ("PASS", "FAIL", "ABSENT"), r.stdout
