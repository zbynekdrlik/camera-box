"""issue 1357 -- the ONE Windows OBS-box baseline (scripts/lib/obs-box-baseline-win.sh).

Live cause (26.9.2026): RESOLUME-SNV ran the Windows `Balanced` power plan while every other
audio/video Windows box ran a max-performance plan; the host stalls made the FOH VB-Matrix underrun
on both resolume VBAN senders, and nothing graded the difference. This suite pins:

  * the pure grader over captured `powercfg` / `reg query` text in the gather's section format
    (Balanced -> DRIFT, Bitsum / High performance / Ultimate -> OK, garbage / truncated /
    unreadable -> UNKNOWN, never OK);
  * the read-only gather program (a `.ps1` run by `powershell -File`) mutates nothing;
  * the deploy program's ONE mutation: `powercfg /setactive` a High performance scheme only when the
    active one is not max-performance class, read back, before any OBS stop;
  * the report-only `--win-baseline` facet of version-integrity-gate.sh (never changes its exit);
  * the dev1 reader scripts/win-baseline-check.sh (fetch seam, traveling box SKIPPED);
  * the rig-health-audit NOTE row and the obs-fleet `win-baseline` facet.

Fixtures: tests/python/fixtures/win_baseline_1357/ -- the two `*_live_2026-09-27.txt` files are the
27.9.2026 read-only win-* MCP captures of stream and resolume, reassembled into the section format.
"""
import importlib.util
import os
import pathlib
import re
import subprocess

import pytest

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "obs-box-baseline-win.sh"
FIX = pathlib.Path(__file__).resolve().parent / "fixtures" / "win_baseline_1357"
ITEMS = ["power_scheme", "sleep_ac", "hibernate_ac", "usb_selective_suspend", "wer_dontshowui"]

# powercfg / reg verbs that WRITE state. Only `/setactive` is allowed, and only in the deploy block.
MUTATING = re.compile(
    r"(?i)powercfg(\.exe)?\s+[-/](change|x\b|setacvalueindex|setdcvalueindex|duplicatescheme|delete|d\b|"
    r"import|hibernate|h\b|changename|setsecuritydescriptor|attributes|setactive|s\b)"
    r"|reg(\.exe)?\s+(add|delete|import|load|restore|copy)\b"
    r"|Set-ItemProperty|New-ItemProperty|Remove-ItemProperty"
)


def _bash(script, env=None):
    full_env = dict(os.environ)
    if env:
        full_env.update(env)
    return subprocess.run(["bash", "-c", script], capture_output=True, text=True, env=full_env,
                          timeout=60)


def _grade(path):
    # Graded exactly as its consumers run it: under `set -euo pipefail` (win-baseline-check.sh and
    # version-integrity-gate.sh both set it). Every item must print a real verdict.
    r = _bash(f'set -euo pipefail; . "{LIB}"; win_baseline_grade "{path}"')
    rows = {}
    for ln in r.stdout.splitlines():
        parts = ln.split(None, 2)
        if parts and parts[0] in ITEMS:
            rows[parts[0]] = (parts[1] if len(parts) > 1 else "", parts[2] if len(parts) > 2 else "")
    assert list(rows) == ITEMS, r.stdout + r.stderr
    for item, (verdict, _) in rows.items():
        assert verdict in ("OK", "DRIFT", "UNKNOWN"), (item, r.stdout, r.stderr)
    assert "unbound variable" not in r.stderr, r.stderr
    return r.returncode, rows, r.stdout


# --- the grader ------------------------------------------------------------------------------

def test_grader_prints_every_item_in_list_order():
    rc, rows, out = _grade(FIX / "dup_high_performance_all_ok.txt")
    printed = [ln.split()[0] for ln in out.splitlines() if ln.split() and ln.split()[0] in ITEMS]
    assert printed == ITEMS, out


def test_stream_live_capture_bitsum_ok_wer_absent_is_drift():
    rc, rows, out = _grade(FIX / "stream_live_2026-09-27.txt")
    assert rows["power_scheme"][0] == "OK", out
    assert "Bitsum Highest Performance" in rows["power_scheme"][1]
    for item in ("sleep_ac", "hibernate_ac", "usb_selective_suspend"):
        assert rows[item][0] == "OK", out
    assert rows["wer_dontshowui"][0] == "DRIFT", out
    assert "not set" in rows["wer_dontshowui"][1]
    assert rc == 20


def test_resolume_live_capture_stock_high_performance_ok():
    rc, rows, out = _grade(FIX / "resolume_live_2026-09-27.txt")
    assert rows["power_scheme"][0] == "OK", out
    assert "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c" in rows["power_scheme"][1]


def test_balanced_and_every_drifted_item_is_drift_and_named():
    rc, rows, out = _grade(FIX / "resolume_balanced_all_drift.txt")
    assert {k: v[0] for k, v in rows.items()} == {i: "DRIFT" for i in ITEMS}, out
    assert "Balanced" in rows["power_scheme"][1]
    assert "1800" in rows["sleep_ac"][1]
    assert "10800" in rows["hibernate_ac"][1]
    assert "Enabled" in rows["usb_selective_suspend"][1]
    assert "0x0" in rows["wer_dontshowui"][1]
    assert "OBS" not in rows["wer_dontshowui"][1], rows["wer_dontshowui"]
    assert rc == 20


def test_duplicated_high_performance_matched_by_name_all_ok():
    # stream's High performance scheme carries a duplicated GUID (659aca3b-...), not the stock one.
    rc, rows, out = _grade(FIX / "dup_high_performance_all_ok.txt")
    assert {k: v[0] for k, v in rows.items()} == {i: "OK" for i in ITEMS}, out
    assert rc == 0


def test_ultimate_performance_ok_power_saver_drift():
    _, rows, out = _grade(FIX / "ultimate_all_ok.txt")
    assert rows["power_scheme"][0] == "OK", out
    rc, rows, out = _grade(FIX / "power_saver.txt")
    assert rows["power_scheme"][0] == "DRIFT", out
    assert "Power saver" in rows["power_scheme"][1]
    assert rc == 20


@pytest.mark.parametrize("name", ["garbage.txt", "sections_unreadable.txt"])
def test_unreadable_gather_is_unknown_never_ok(name):
    rc, rows, out = _grade(FIX / name)
    assert {k: v[0] for k, v in rows.items()} == {i: "UNKNOWN" for i in ITEMS}, out
    assert rc == 11


def test_missing_file_is_unknown():
    rc, rows, out = _grade(FIX / "no-such-file.txt")
    assert {k: v[0] for k, v in rows.items()} == {i: "UNKNOWN" for i in ITEMS}, out
    assert rc == 11


def test_truncated_gather_grades_what_it_has_rest_unknown():
    rc, rows, out = _grade(FIX / "truncated.txt")
    assert rows["power_scheme"][0] == "OK", out
    for item in ITEMS[1:]:
        assert rows[item][0] == "UNKNOWN", out
    assert rc == 11


def test_a_value_read_from_the_wrong_setting_is_unknown(tmp_path):
    # The sleep section must be the STANDBYIDLE setting; the hibernate text under the sleep id is
    # not a reading of sleep.
    text = (FIX / "dup_high_performance_all_ok.txt").read_text()
    hib = text.split("==WINBASELINE-SECTION== hibernate_ac\n")[1].split("==WINBASELINE-EXIT==")[0]
    sleep = text.split("==WINBASELINE-SECTION== sleep_ac\n")[1].split("==WINBASELINE-EXIT==")[0]
    bad = tmp_path / "wrong.txt"
    bad.write_text(text.replace(sleep, hib, 1))
    _, rows, out = _grade(bad)
    assert rows["sleep_ac"][0] == "UNKNOWN", out


def test_real_gather_program_output_grades():
    # The EMITTED gather program run read-only on resolume through the win-* MCP Shell (27.9.2026),
    # as printed (CRLF, and the reg query stderr line its 2>&1 captures). This is the program's real
    # output shape, not a reassembled one.
    rc, rows, out = _grade(FIX / "resolume_gather_program_live_2026-09-27.txt")
    assert {k: v[0] for k, v in rows.items()} == {
        "power_scheme": "OK", "sleep_ac": "OK", "hibernate_ac": "OK",
        "usb_selective_suspend": "OK", "wer_dontshowui": "DRIFT"}, out
    assert rc == 20


def test_maxperf_predicate():
    ok = [("8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c", "Vysoký výkon"),
          ("E9A42B02-D5DF-448D-AA00-03F14749EB61", "x"),
          ("ad4dffa4-5964-4004-ba10-20b167c34cc8", "Bitsum Highest Performance"),
          ("659aca3b-5113-4d5e-8a68-f14ebc0aa866", "high performance")]
    bad = [("381b4222-f694-41f0-9685-ff5bb260df2e", "Balanced"),
           ("a1841308-3541-4fab-bc81-f71556f20b4a", "Power saver"),
           ("", "")]
    for g, n in ok:
        assert _bash(f'. "{LIB}"; win_baseline_scheme_is_maxperf "{g}" "{n}"').returncode == 0, (g, n)
    for g, n in bad:
        assert _bash(f'. "{LIB}"; win_baseline_scheme_is_maxperf "{g}" "{n}"').returncode != 0, (g, n)


# --- the read-only gather program ----------------------------------------------------------------

def _gather_ps1():
    r = _bash(f'. "{LIB}"; win_baseline_gather_ps1')
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_gather_program_is_read_only_and_covers_every_item():
    ps = _gather_ps1()
    assert not MUTATING.search(ps), MUTATING.search(ps)
    assert "==WINBASELINE-BEGIN==" in ps and "==WINBASELINE-END==" in ps
    for item in ITEMS:
        assert f"'{item}'" in ps, item
    for token in ("powercfg /getactivescheme", "STANDBYIDLE", "HIBERNATEIDLE",
                  "2a737441-1930-4402-8d77-b2bebba308a3", "48e6b7a6-50f5-4782-a5d4-53bb8f07e226",
                  "Windows Error Reporting", "DontShowUI"):
        assert token in ps, token


# --- the deploy set-and-verify block --------------------------------------------------------------

def _ensure_ps():
    r = _bash(f'. "{LIB}"; win_baseline_power_plan_ensure_ps')
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_ensure_block_sets_only_the_power_plan_then_reads_it_back():
    ps = _ensure_ps()
    muts = [m.group(0) for m in MUTATING.finditer(ps)]
    assert muts == ["powercfg /setactive"], muts
    set_at = ps.index("powercfg /setactive")
    # read-back AFTER the set, verified against the target, fail loud otherwise
    after = ps[set_at:]
    assert "Get-WbActiveScheme" in after
    assert "exit 11" in after
    # only in the ELSE branch of the max-performance test, and the read-back is compared with the target
    guard_at = ps.index("if (Test-WbMaxPerf $wbBefore)")
    else_at = ps.index("} else {", guard_at)
    assert guard_at < else_at < ps.index("powercfg /setactive $wbTarget[0]")
    assert ps.index("($wbAfter[0] -ne $wbTarget[0])") > set_at
    # the deploy program runs under $ErrorActionPreference = 'Stop', where Write-Error THROWS and the
    # following `exit 11` never runs -- the block must fail with its own exit code
    assert "Write-Error" not in ps, ps
    assert ps.count("exit 11") == 3
    # the named output line
    assert "issue 1357 power plan" in ps


def test_ensure_block_takes_its_lists_from_the_one_lib_list():
    r = _bash(f'. "{LIB}"; printf "%s\\n" "$WIN_BASELINE_MAXPERF_GUIDS" "$WIN_BASELINE_MAXPERF_NAMES"')
    guids, names = r.stdout.splitlines()[:2]
    ps = _ensure_ps()
    for g in guids.split():
        assert f"'{g}'" in ps, g
    for n in names.split("|"):
        assert f"'{n}'" in ps, n


def _deploy_program(box):
    r = _bash(
        f'. "{SCRIPTS}/deploy-genlock-fleet.sh"; '
        f'build_windows_deploy_program {box} full "C:\\stage" "C:\\Program Files\\obs-studio" '
        f'"$(fleet_box_ahk_mode {box})" "C:\\obs-backup" 3 abc123 def456'
    )
    assert r.returncode == 0, r.stderr
    return r.stdout


@pytest.mark.parametrize("box", ["stream", "resolume"])
def test_deploy_program_sets_the_power_plan_before_any_obs_stop(box):
    p = _deploy_program(box)
    set_at = p.index("powercfg /setactive")
    assert set_at < p.index("Get-Process obs64,obs-browser-page"), "plan set must precede the OBS stop"
    assert set_at < p.index("# (1) "), "plan set must precede the AHK step"
    assert set_at > p.index("# (0) preflight")
    # the ONLY baseline mutation in the whole deploy program
    muts = [m.group(0) for m in MUTATING.finditer(p)]
    assert muts == ["powercfg /setactive"], muts


# --- version-integrity-gate report-only facet -------------------------------------------------------

def _gate(tmp_path, extra):
    state = tmp_path / "stream-state.json"
    state.write_text('{"obs_version":"32.2.0"}\n')
    return subprocess.run(
        ["bash", str(SCRIPTS / "version-integrity-gate.sh"), "--win-state", f"stream={state}", *extra],
        capture_output=True, text=True, cwd=REPO, timeout=120,
    )


def test_gate_win_baseline_facet_is_report_only(tmp_path):
    base = _gate(tmp_path, [])
    drift = _gate(tmp_path, ["--win-baseline", f"resolume={FIX / 'resolume_balanced_all_drift.txt'}"])
    ok = _gate(tmp_path, ["--win-baseline", f"stream={FIX / 'dup_high_performance_all_ok.txt'}"])
    unread = _gate(tmp_path, ["--win-baseline", f"stream={tmp_path / 'absent.txt'}"])
    live = _gate(tmp_path, ["--win-baseline", f"stream={FIX / 'stream_live_2026-09-27.txt'}",
                            "--win-baseline", "resolume"])
    assert drift.returncode == base.returncode == ok.returncode == unread.returncode == live.returncode, (
        base.returncode, drift.returncode, ok.returncode, unread.returncode, live.returncode)
    # the roll-up (the GATE FAILED / INCOMPLETE lines and their box counts) must be byte-identical:
    # a baseline row can never add a box to bad/unknown, whatever it grades
    for run in (drift, ok, unread, live):
        assert run.stderr == base.stderr, (base.stderr, run.stderr)
    assert re.search(r"stream\s+win_baseline wer_dontshowui\s+DRIFT", live.stdout), live.stdout
    assert re.search(r"resolume\s+win_baseline power_scheme\s+UNKNOWN", live.stdout), live.stdout
    assert re.search(r"resolume\s+win_baseline power_scheme\s+DRIFT .*Balanced", drift.stdout), drift.stdout
    assert "report-only" in drift.stdout
    assert re.search(r"stream\s+win_baseline power_scheme\s+OK", ok.stdout), ok.stdout
    assert re.search(r"stream\s+win_baseline power_scheme\s+UNKNOWN", unread.stdout), unread.stdout
    # (the tmp dir name carries the test name, so assert on the row shape, not the bare word)
    assert not re.search(r"win_baseline power_scheme", base.stdout), base.stdout


# --- the dev1 reader -----------------------------------------------------------------------------

def _seam(tmp_path, mapping):
    """A WIN_BASELINE_FETCH_CMD stand-in: copies the per-box fixture into the outfile."""
    seam = tmp_path / "fetch.sh"
    cases = "\n".join(f'  {b}) cp "{f}" "$3" ;;' for b, f in mapping.items())
    seam.write_text(f'#!/usr/bin/env bash\ncase "$1" in\n{cases}\n  *) exit 1 ;;\nesac\n')
    seam.chmod(0o755)
    return str(seam)


def test_check_reads_each_box_and_names_the_drift(tmp_path):
    seam = _seam(tmp_path, {"stream": FIX / "dup_high_performance_all_ok.txt",
                            "resolume": FIX / "resolume_balanced_all_drift.txt"})
    r = subprocess.run(["bash", str(SCRIPTS / "win-baseline-check.sh")], capture_output=True, text=True,
                       env={**os.environ, "WIN_BASELINE_FETCH_CMD": seam, "OBS_FLEET_HOME": "stream resolume"},
                       timeout=60)
    assert r.returncode == 20, r.stdout + r.stderr
    assert re.search(r"^box=stream win_baseline=OK$", r.stdout, re.M), r.stdout
    assert re.search(r"^box=resolume win_baseline=DRIFT "
                     r"drift=power_scheme,sleep_ac,hibernate_ac,usb_selective_suspend,wer_dontshowui$",
                     r.stdout, re.M), r.stdout
    assert re.search(r"box=resolume item=power_scheme verdict=DRIFT detail=.*Balanced", r.stdout), r.stdout


def test_check_over_the_live_captures_names_the_wer_drift(tmp_path):
    seam = _seam(tmp_path, {"stream": FIX / "stream_live_2026-09-27.txt",
                            "resolume": FIX / "resolume_gather_program_live_2026-09-27.txt"})
    r = subprocess.run(["bash", str(SCRIPTS / "win-baseline-check.sh")], capture_output=True, text=True,
                       env={**os.environ, "WIN_BASELINE_FETCH_CMD": seam, "OBS_FLEET_HOME": "stream resolume"},
                       timeout=60)
    assert r.returncode == 20, r.stdout + r.stderr
    for box in ("stream", "resolume"):
        assert f"box={box} item=wer_dontshowui verdict=DRIFT" in r.stdout, r.stdout
        assert re.search(rf"^box={box} win_baseline=DRIFT drift=wer_dontshowui$", r.stdout, re.M), r.stdout
    assert "unbound variable" not in r.stderr, r.stderr


def test_check_skips_a_traveling_box_that_is_away(tmp_path):
    seam = _seam(tmp_path, {"stream": FIX / "dup_high_performance_all_ok.txt"})
    r = subprocess.run(["bash", str(SCRIPTS / "win-baseline-check.sh")], capture_output=True, text=True,
                       env={**os.environ, "WIN_BASELINE_FETCH_CMD": seam, "OBS_FLEET_HOME": "stream"},
                       timeout=60)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "box=resolume win_baseline=SKIPPED" in r.stdout


def test_check_failed_fetch_is_unknown(tmp_path):
    seam = _seam(tmp_path, {})
    r = subprocess.run(["bash", str(SCRIPTS / "win-baseline-check.sh"), "--box", "stream"],
                       capture_output=True, text=True,
                       env={**os.environ, "WIN_BASELINE_FETCH_CMD": seam, "OBS_FLEET_HOME": "stream"},
                       timeout=60)
    assert r.returncode == 11, r.stdout + r.stderr
    assert re.search(r"^box=stream win_baseline=UNKNOWN unknown=power_scheme,sleep_ac,hibernate_ac,"
                     r"usb_selective_suspend,wer_dontshowui$", r.stdout, re.M), r.stdout


def test_check_emit_ps1_is_the_lib_gather_program():
    r = subprocess.run(["bash", str(SCRIPTS / "win-baseline-check.sh"), "--emit-ps1"],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0
    assert r.stdout == _gather_ps1()


def test_check_out_dir_keeps_the_raw_gather_for_the_gate(tmp_path):
    seam = _seam(tmp_path, {"stream": FIX / "stream_live_2026-09-27.txt"})
    out = tmp_path / "raw"
    subprocess.run(["bash", str(SCRIPTS / "win-baseline-check.sh"), "--box", "stream", "--out-dir", str(out)],
                   capture_output=True, text=True,
                   env={**os.environ, "WIN_BASELINE_FETCH_CMD": seam, "OBS_FLEET_HOME": "stream"}, timeout=60)
    assert (out / "stream.txt").read_bytes() == (FIX / "stream_live_2026-09-27.txt").read_bytes()


# --- the fleet roster + rig-health row ---------------------------------------------------------------

def test_obs_fleet_win_baseline_facet_is_the_windows_obs_boxes():
    r = _bash(f'. "{SCRIPTS}/lib/obs-fleet.sh"; obs_fleet_facet_members win-baseline')
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["stream", "resolume"]


def _audit():
    spec = importlib.util.spec_from_file_location("rig_health_audit_1357", SCRIPTS / "rig-health-audit.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_rig_health_win_baseline_row_is_note_and_names_drift():
    mod = _audit()
    assert mod.WIN_BASELINE_REPORT_VERDICT == "NOTE"
    out = ("box=resolume item=power_scheme verdict=DRIFT detail=Balanced (381b4222-f694-41f0-9685-ff5bb260df2e) is not max-performance class\n"
           "box=resolume item=sleep_ac verdict=OK detail=never\n"
           "box=resolume win_baseline=DRIFT drift=power_scheme\n"
           "box=stream win_baseline=OK\n"
           "box=imag win_baseline=SKIPPED (away)\n")
    rows = mod.win_baseline_rows_from_output(out)
    assert rows["resolume"].startswith("win_baseline=DRIFT")
    assert "power_scheme=DRIFT" in rows["resolume"] and "Balanced" in rows["resolume"]
    assert rows["stream"].startswith("win_baseline=OK")
    assert "report-only" in rows["stream"]
    assert "imag" not in rows  # a skipped (away) box is omitted, never a stale row


def test_rig_health_empty_verdict_is_unknown_and_named():
    rows = _audit().win_baseline_rows_from_output(
        "box=stream item=wer_dontshowui verdict= detail=\nbox=stream win_baseline=UNKNOWN\n")
    assert "wer_dontshowui=UNKNOWN" in rows["stream"], rows


def _fake_reader(tmp_path, body):
    reader = tmp_path / "reader.py"
    reader.write_text("import sys\nsys.stdout.buffer.write(" + repr(body) + ")\nsys.exit(" +
                      ("3" if b"CRASH" in body else "20") + ")\n")
    wrap = tmp_path / "reader.sh"
    wrap.write_text(f"#!/usr/bin/env bash\nexec python3 {reader}\n")
    return str(wrap)


def test_rig_health_survives_a_non_utf8_reader_byte(tmp_path, capsys):
    # scheme names / reg text arrive in the Windows OEM codepage; one non-UTF-8 byte must never
    # take the whole audit down
    mod = _audit()
    mod.WIN_BASELINE_SCRIPT = _fake_reader(
        tmp_path, b"box=stream item=power_scheme verdict=DRIFT detail=Vysok\xec v\xfdkon (x) is not\n"
                  b"box=stream win_baseline=DRIFT drift=power_scheme\n")
    mod.check_win_baseline()
    out = capsys.readouterr().out
    assert "[NOTE] stream-win win_baseline=DRIFT power_scheme=DRIFT[" in out, out


def test_rig_health_reader_crash_after_some_rows_is_still_a_note_row(tmp_path, capsys):
    mod = _audit()
    mod.WIN_BASELINE_SCRIPT = _fake_reader(tmp_path, b"box=stream win_baseline=OK\nCRASH\n")
    mod.check_win_baseline()
    out = capsys.readouterr().out
    assert "[NOTE] stream-win win_baseline=OK" in out, out
    assert "[NOTE] win-baseline" in out and "rc=3" in out, out


def test_rig_health_crashed_tool_is_a_note_row(tmp_path, capsys):
    mod = _audit()
    crash = tmp_path / "crash.sh"
    crash.write_text("#!/usr/bin/env bash\necho boom >&2\nexit 3\n")
    mod.WIN_BASELINE_SCRIPT = str(crash)
    mod.check_win_baseline()
    out = capsys.readouterr().out
    assert "[NOTE] win-baseline" in out and "rc=3" in out, out
    assert mod.results == ["NOTE"]
