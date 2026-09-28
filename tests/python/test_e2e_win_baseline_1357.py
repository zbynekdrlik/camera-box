"""issue 1357 -- the full-path E2E `[0/8]` gathers the Windows OBS-box baseline, REPORT-ONLY.

scripts/lib/e2e-win-baseline.sh `e2e_win_baseline_gather OUT_DIR` runs scripts/win-baseline-check.sh
once under a bounded `timeout` and fills WIN_BASELINE_GATE_ARGS with `--win-baseline <box>=<file>`
for the version-integrity gate's report-only rows. Pinned here:

  * every outcome of the check (OK 0, DRIFT 20, UNKNOWN 11, a hung gather hitting the timeout, a
    crashed / missing check) is ONE run-log line and never aborts a `set -euo pipefail` caller;
  * the array names exactly the gather files that exist (a traveling box that is away -- resolume --
    is the check's own SKIPPED and gets no arg; none -> an empty array);
  * the timeout is sized from the check's own per-box bounds;
  * recording-e2e.sh calls the helper once in `[0/8]` right before the gate, and BOTH gate
    invocations (imag acked offline / normal) carry the array through a `set -u`-safe expansion,
    proven by running the real invocation text against a stub gate;
  * the gate fed by the helper keeps its exit code and roll-up whatever the baseline grades.

Offline: the check's WIN_BASELINE_FETCH_CMD seam + obs-fleet's OBS_FLEET_HOME (no box, no network).
"""
import os
import pathlib
import re
import subprocess
import time

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "e2e-win-baseline.sh"
E2E = SCRIPTS / "recording-e2e.sh"
FIX = pathlib.Path(__file__).resolve().parent / "fixtures" / "win_baseline_1357"
EXPANSION = '${WIN_BASELINE_GATE_ARGS[@]+"${WIN_BASELINE_GATE_ARGS[@]}"}'
CALL = 'e2e_win_baseline_gather "$OUTDIR/win-baseline"'


def _seam(tmp_path, mapping):
    """A WIN_BASELINE_FETCH_CMD stand-in: per box, copy a fixture, fail, or hang."""
    seam = tmp_path / "fetch.sh"
    cases = []
    for box, action in mapping.items():
        if action == "fail":
            cases.append(f"  {box}) exit 1 ;;")
        elif action == "hang":
            cases.append(f'  {box}) echo $$ >"{tmp_path}/hang.pid"; exec sleep 60 ;;')
        else:
            cases.append(f'  {box}) cp "{action}" "$3" ;;')
    seam.write_text('#!/usr/bin/env bash\ncase "$1" in\n' + "\n".join(cases) + "\n  *) exit 1 ;;\nesac\n")
    seam.chmod(0o755)
    return str(seam)


def _gather(tmp_path, env, out_dir=None, timeout=60):
    """Source the lib under the caller's `set -euo pipefail` and run the gather, as recording-e2e.sh
    does. The AFTER / ARGC lines prove the caller kept running and the expansion is `set -u` safe."""
    out = str(out_dir if out_dir is not None else tmp_path / "out")
    script = (
        'set -euo pipefail\n'
        f'. "{LIB}"\n'
        f'e2e_win_baseline_gather "{out}"\n'
        'echo "AFTER rc=$?"\n'
        f'set -- {EXPANSION}\n'
        'echo "ARGC=$#"\n'
        'for a in "$@"; do echo "ARG=$a"; done\n'
    )
    full_env = {**os.environ, **env}
    t0 = time.monotonic()
    r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, env=full_env,
                       timeout=timeout)
    return r, time.monotonic() - t0


def _args(r):
    return [ln[len("ARG="):] for ln in r.stdout.splitlines() if ln.startswith("ARG=")]


def _verdict(r):
    lines = [ln for ln in r.stdout.splitlines() if "Windows OBS-box baseline:" in ln]
    assert len(lines) == 1, r.stdout + r.stderr
    return lines[0]


def _assert_caller_survived(r):
    assert r.returncode == 0, r.stdout + r.stderr
    assert "AFTER rc=0" in r.stdout, r.stdout + r.stderr
    assert "unbound variable" not in r.stderr, r.stderr


# --- the gather helper: every outcome is one line, never fatal ------------------------------------

def test_ok_both_boxes_read_fill_the_array_in_facet_order(tmp_path):
    seam = _seam(tmp_path, {"stream": FIX / "dup_high_performance_all_ok.txt",
                            "resolume": FIX / "ultimate_all_ok.txt"})
    r, _ = _gather(tmp_path, {"WIN_BASELINE_FETCH_CMD": seam, "OBS_FLEET_HOME": "stream resolume"})
    _assert_caller_survived(r)
    out = tmp_path / "out"
    assert _args(r) == ["--win-baseline", f"stream={out}/stream.txt",
                        "--win-baseline", f"resolume={out}/resolume.txt"], r.stdout
    assert "ARGC=4" in r.stdout
    assert (out / "stream.txt").read_bytes() == (FIX / "dup_high_performance_all_ok.txt").read_bytes()
    v = _verdict(r)
    assert v.split("Windows OBS-box baseline: ", 1)[1].startswith("OK"), v
    assert "report-only" in v and "does NOT block" in v and "2 box gather(s)" in v, v
    assert re.search(r"^\s+box=stream win_baseline=OK$", r.stdout, re.M), r.stdout
    assert re.search(r"^\s+box=resolume win_baseline=OK$", r.stdout, re.M), r.stdout
    assert (out / "win-baseline-check.log").is_file()


def test_drift_rc20_is_one_line_and_never_fatal(tmp_path):
    seam = _seam(tmp_path, {"stream": FIX / "stream_live_2026-09-27.txt",
                            "resolume": FIX / "resolume_balanced_all_drift.txt"})
    r, _ = _gather(tmp_path, {"WIN_BASELINE_FETCH_CMD": seam, "OBS_FLEET_HOME": "stream resolume"})
    _assert_caller_survived(r)
    assert _verdict(r).split("baseline: ", 1)[1].startswith("DRIFT"), r.stdout
    assert re.search(r"^\s+box=resolume win_baseline=DRIFT drift=power_scheme,", r.stdout, re.M), r.stdout
    assert re.search(r"^\s+box=stream win_baseline=DRIFT drift=wer_dontshowui$", r.stdout, re.M), r.stdout
    assert "ARGC=4" in r.stdout


def test_unread_box_rc11_is_unknown_and_its_empty_gather_still_goes_to_the_gate(tmp_path):
    seam = _seam(tmp_path, {"stream": "fail"})
    r, _ = _gather(tmp_path, {"WIN_BASELINE_FETCH_CMD": seam, "OBS_FLEET_HOME": "stream"})
    _assert_caller_survived(r)
    assert _verdict(r).split("baseline: ", 1)[1].startswith("UNKNOWN"), r.stdout
    out = tmp_path / "out"
    # the check leaves an empty file for a box it tried to read; the gate grades it UNKNOWN per item
    assert _args(r) == ["--win-baseline", f"stream={out}/stream.txt"], r.stdout
    assert (out / "stream.txt").read_bytes() == b""


def test_traveling_box_away_is_skipped_and_gets_no_arg(tmp_path):
    seam = _seam(tmp_path, {"stream": FIX / "dup_high_performance_all_ok.txt"})
    r, _ = _gather(tmp_path, {"WIN_BASELINE_FETCH_CMD": seam, "OBS_FLEET_HOME": "stream"})
    _assert_caller_survived(r)
    assert re.search(r"^\s+box=resolume win_baseline=SKIPPED", r.stdout, re.M), r.stdout
    assert _verdict(r).split("baseline: ", 1)[1].startswith("OK"), r.stdout
    assert _args(r) == ["--win-baseline", f"{'stream'}={tmp_path / 'out'}/stream.txt"], r.stdout
    assert "1 box gather(s)" in _verdict(r)


def test_a_stale_gather_of_an_earlier_run_is_never_graded(tmp_path):
    out = tmp_path / "out"
    out.mkdir()
    (out / "resolume.txt").write_bytes((FIX / "resolume_balanced_all_drift.txt").read_bytes())
    seam = _seam(tmp_path, {"stream": FIX / "dup_high_performance_all_ok.txt"})
    r, _ = _gather(tmp_path, {"WIN_BASELINE_FETCH_CMD": seam, "OBS_FLEET_HOME": "stream"})
    _assert_caller_survived(r)
    assert not (out / "resolume.txt").exists()
    assert all("resolume=" not in a for a in _args(r)), r.stdout


def test_hung_gather_hits_the_bound_is_one_timeout_line_and_leaves_no_process(tmp_path):
    seam = _seam(tmp_path, {"stream": "hang"})
    r, elapsed = _gather(tmp_path, {"WIN_BASELINE_FETCH_CMD": seam, "OBS_FLEET_HOME": "stream",
                                    "E2E_WIN_BASELINE_TIMEOUT": "2"})
    _assert_caller_survived(r)
    assert elapsed < 15, elapsed
    v = _verdict(r)
    assert v.split("baseline: ", 1)[1].startswith("TIMEOUT after 2 s"), v
    # the box it was reading when the bound hit still goes to the gate (empty -> UNKNOWN there)
    assert _args(r) == ["--win-baseline", f"stream={tmp_path / 'out'}/stream.txt"], r.stdout
    pid = int((tmp_path / "hang.pid").read_text().strip())
    time.sleep(0.5)
    try:
        os.kill(pid, 0)
        alive = True
    except ProcessLookupError:
        alive = False
    assert not alive, f"the hung fetch {pid} outlived the bound"


def test_crashed_check_is_failed_empty_array_and_never_fatal(tmp_path):
    fake = tmp_path / "fake-check.sh"
    fake.write_text("#!/usr/bin/env bash\necho boom >&2\nexit 3\n")
    r, _ = _gather(tmp_path, {"E2E_WIN_BASELINE_CHECK": str(fake), "OBS_FLEET_HOME": "stream"})
    _assert_caller_survived(r)
    assert _verdict(r).split("baseline: ", 1)[1].startswith("FAILED (rc 3)"), r.stdout
    assert "ARGC=0" in r.stdout and _args(r) == [], r.stdout
    assert "boom" in (tmp_path / "out" / "win-baseline-check.log").read_text()


def test_missing_check_is_failed_and_never_fatal(tmp_path):
    r, _ = _gather(tmp_path, {"E2E_WIN_BASELINE_CHECK": str(tmp_path / "absent.sh")})
    _assert_caller_survived(r)
    assert "FAILED (rc 127)" in _verdict(r), r.stdout
    assert "ARGC=0" in r.stdout


def test_no_output_dir_is_skipped_with_an_empty_array(tmp_path):
    r, _ = _gather(tmp_path, {}, out_dir="")
    _assert_caller_survived(r)
    assert "SKIPPED -- no output dir" in r.stdout, r.stdout
    assert "ARGC=0" in r.stdout


def test_uncreatable_output_dir_is_skipped_never_fatal(tmp_path):
    blocker = tmp_path / "file"
    blocker.write_text("x")
    r, _ = _gather(tmp_path, {}, out_dir=blocker / "sub")
    _assert_caller_survived(r)
    assert "SKIPPED -- cannot create" in r.stdout, r.stdout
    assert "ARGC=0" in r.stdout


def _timeout_s(env):
    r = subprocess.run(["bash", "-c", f'set -euo pipefail; . "{LIB}"; e2e_win_baseline_timeout_s'],
                       capture_output=True, text=True, env={**os.environ, **env}, timeout=30)
    assert r.returncode == 0, r.stderr
    return int(r.stdout.strip())


def test_timeout_is_sized_from_the_checks_own_per_box_bounds():
    clean = {k: "" for k in ("E2E_WIN_BASELINE_TIMEOUT", "WIN_BASELINE_SSH_TIMEOUT",
                             "OBS_FLEET_RESOLVE_TIMEOUT", "OBS_FLEET_STATUS_TIMEOUT")}
    # two boxes (stream, resolume) x (2 x 20 s ssh/scp + 2 s resolve + 4 s OBS-WS probe) + 10 s
    assert _timeout_s(clean) == 2 * (2 * 20 + 2 + 4) + 10
    assert _timeout_s({**clean, "WIN_BASELINE_SSH_TIMEOUT": "09"}) == 2 * (2 * 9 + 2 + 4) + 10
    assert _timeout_s({**clean, "WIN_BASELINE_SSH_TIMEOUT": "abc"}) == 2 * (2 * 20 + 2 + 4) + 10
    assert _timeout_s({**clean, "E2E_WIN_BASELINE_TIMEOUT": "08"}) == 8
    assert _timeout_s({**clean, "E2E_WIN_BASELINE_TIMEOUT": "0"}) == 102
    assert _timeout_s({**clean, "E2E_WIN_BASELINE_TIMEOUT": "-5"}) == 102


def test_the_helper_feeds_the_gate_and_never_changes_its_exit(tmp_path):
    seam = _seam(tmp_path, {"stream": FIX / "stream_live_2026-09-27.txt",
                            "resolume": FIX / "resolume_balanced_all_drift.txt"})
    r, _ = _gather(tmp_path, {"WIN_BASELINE_FETCH_CMD": seam, "OBS_FLEET_HOME": "stream resolume"})
    _assert_caller_survived(r)
    state = tmp_path / "stream-state.json"
    state.write_text('{"obs_version":"32.2.0"}\n')

    def gate(extra):
        return subprocess.run(
            ["bash", str(SCRIPTS / "version-integrity-gate.sh"), "--win-state", f"stream={state}", *extra],
            capture_output=True, text=True, cwd=REPO, timeout=120)

    base, fed = gate([]), gate(_args(r))
    assert fed.returncode == base.returncode, (base.returncode, fed.returncode)
    assert fed.stderr == base.stderr, (base.stderr, fed.stderr)
    assert re.search(r"stream\s+win_baseline wer_dontshowui\s+DRIFT", fed.stdout), fed.stdout
    assert re.search(r"resolume\s+win_baseline power_scheme\s+DRIFT .*Balanced", fed.stdout), fed.stdout


# --- recording-e2e.sh wiring -------------------------------------------------------------------------

def _e2e():
    return E2E.read_text()


def _gate_block(s):
    """The `[0/8]` gate `if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then ... fi` block (both invocations)."""
    start = s.index('if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then\n  "$HERE/version-integrity-gate.sh" \\\n')
    end = s.index("\nfi\n", s.index('\nelse\n"$HERE/version-integrity-gate.sh" \\\n', start)) + len("\nfi\n")
    return start, s[start:end]


def _invocations(block):
    """Each gate invocation = its backslash-continued lines, from the gate line to the first line that
    does not end in a backslash (the command's last argument)."""
    lines = block.splitlines()
    out = []
    for i, ln in enumerate(lines):
        if ln.strip() == '"$HERE/version-integrity-gate.sh" \\':
            cmd = [ln]
            j = i + 1
            while cmd[-1].endswith("\\"):
                cmd.append(lines[j])
                j += 1
            out.append(cmd)
    return out


def test_e2e_sources_the_helper_and_calls_it_once_right_before_the_gate():
    s = _e2e()
    assert s.count('. "$HERE/lib/e2e-win-baseline.sh"') == 1
    assert s.count(CALL) == 1
    call_at = s.index(CALL)
    source_at = s.index('. "$HERE/lib/e2e-win-baseline.sh"')
    gate_at, _ = _gate_block(s)
    banner = s.index("[0/8] version-integrity gate")
    assert banner < source_at < call_at < gate_at
    # nothing but comments/blank lines between the call and the gate block
    between = s[s.index("\n", call_at) + 1:gate_at]
    assert all(not ln.strip() or ln.lstrip().startswith("#") for ln in between.splitlines()), between
    # a bare call: never wrapped in a condition that could skip it or let it abort the run
    call_line = s[s.rindex("\n", 0, call_at) + 1:s.index("\n", call_at)]
    assert call_line == CALL, call_line


def test_both_gate_invocations_carry_the_array_inside_the_continued_command():
    _, block = _gate_block(_e2e())
    cmds = _invocations(block)
    assert len(cmds) == 2, cmds
    for cmd in cmds:
        joined = "\n".join(cmd)
        assert joined.count(EXPANSION) == 1, joined
        # inside the command (a continuation line), never the dropped tail of a split command
        assert any(ln.strip() == EXPANSION + " \\" for ln in cmd), joined
        assert "${STRIH_LINUX_GATE_ARG:+--strih-linux}" in cmd[-1], joined


def _run_block(tmp_path, acked, args_decl):
    """Run the REAL gate block text from recording-e2e.sh against a stub gate that prints its args."""
    _, block = _gate_block(_e2e())
    here = tmp_path / "here"
    here.mkdir(exist_ok=True)
    stub = here / "version-integrity-gate.sh"
    stub.write_text('#!/usr/bin/env bash\nfor a in "$@"; do echo "GATEARG=$a"; done\n')
    stub.chmod(0o755)
    script = (
        "set -euo pipefail\n"
        f'HERE="{here}"\n'
        f'IMAG_OFFLINE_ACKED="{1 if acked else 0}"\n'
        'IMAG_OFFLINE_ACK_REASON="acked"\n'
        'AUTO_WIN_MANIFEST="" AUTO_WIN_ALT_MANIFEST="" AUTO_IMAG_MANIFEST="" IMAG_SO_CSV=""\n'
        'VERSION_STRIH_STATE=/s.json VERSION_STREAM_STATE=/t.json IMAG_GENLOCK_SHA=abc\n'
        'STRIH_LINUX_GATE_ARG="1"\n'
        f"{args_decl}\n"
        f"{block}"
        'echo "BLOCK-DONE"\n'
    )
    r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "BLOCK-DONE" in r.stdout and "unbound variable" not in r.stderr, r.stdout + r.stderr
    return [ln[len("GATEARG="):] for ln in r.stdout.splitlines() if ln.startswith("GATEARG=")]


def test_real_gate_block_passes_the_array_on_both_branches(tmp_path):
    decl = 'WIN_BASELINE_GATE_ARGS=(--win-baseline "stream=/o/stream.txt" --win-baseline "resolume=/o/r x.txt")'
    for acked in (True, False):
        got = _run_block(tmp_path, acked, decl)
        i = got.index("--win-baseline")
        assert got[i:i + 4] == ["--win-baseline", "stream=/o/stream.txt",
                                "--win-baseline", "resolume=/o/r x.txt"], got
        assert got[-1] == "--strih-linux", got
        assert got.count("--win-state") == 2, got


def test_real_gate_block_is_set_u_safe_with_an_unset_or_empty_array(tmp_path):
    for decl in ("unset WIN_BASELINE_GATE_ARGS", "WIN_BASELINE_GATE_ARGS=()"):
        for acked in (True, False):
            got = _run_block(tmp_path, acked, decl)
            assert "--win-baseline" not in got, got
            assert "" not in got, got   # no stray empty argument from the expansion
            assert got[-1] == "--strih-linux", got
