"""issue 1439 (ROZHODNUTE 6359382787) -- the full-path E2E's BLOCKING program-audio continuity gate:
scripts/lib/program-audio-continuity.sh (the window marks on the stream box's newest OBS log, the
bounded read, the decision, the verdict-JSON merge) and its wiring in scripts/recording-e2e.sh.

The box is faked by PROGRAM_AUDIO_CONTINUITY_READ_CMD replaying the REAL stream-snv log excerpt
(tests/fixtures/program_audio_continuity_1439/, line numbers from 41400) the way the real remote
text prints it: a mark = `<name>\t<line count>`, a slice = `#newest\t<name>` + the lines from FROM.
The lib runs under the caller's `set -euo pipefail` (recording-e2e.sh's mode).
"""
import json
import pathlib
import re
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
LIB = _ROOT / "scripts/lib/program-audio-continuity.sh"
E2E = _ROOT / "scripts/recording-e2e.sh"
LOG = _ROOT / "tests/fixtures/program_audio_continuity_1439/stream-snv-2026-10-23-from-41400.txt"
NAME = "2026-10-23 02-35-32.txt"
FIRST = 41400


def _fake_box(tmp_path, upto=None, newest=NAME, fail=False):
    """A READ_CMD replaying the fixture as the box's log (cut at line `upto` when given)."""
    lines = LOG.read_text().splitlines()
    if upto is not None:
        lines = lines[: upto - FIRST + 1]
    box = tmp_path / "boxlog.txt"  # the box's whole log: the fixture starts at its line 41400
    box.write_text("filler before the excerpt\n" * (FIRST - 1) + "\n".join(lines) + "\n")
    cmd = tmp_path / "read.sh"
    cmd.write_text(f"""#!/usr/bin/env bash
set -euo pipefail
{"exit 1" if fail else ""}
mode="$1"; from="${{4:-1}}"
if [ "$mode" = mark ]; then printf '%s\\t%s\\n' '{newest}' "$(wc -l < '{box}')"; exit 0; fi
printf '#newest\\t%s\\n' '{newest}'
tail -n +"$from" '{box}'
""")
    cmd.chmod(0o755)
    return cmd


def _marks(tmp_path, start, stop, sname=NAME, ename=NAME):
    (tmp_path / "program-audio-continuity-start.mark").write_text(f"{sname}\t{start}\n")
    (tmp_path / "program-audio-continuity-stop.mark").write_text(f"{ename}\t{stop}\n")


def _run(tmp_path, body, read_cmd, host="10.77.9.204", **env):
    script = tmp_path / "harness.sh"
    script.write_text("set -euo pipefail\n" + f'. "{LIB}"\n' + body + "\necho HARNESS-END\n")
    e = {"PATH": "/usr/local/bin:/usr/bin:/bin", "HOME": str(tmp_path),
         "PROGRAM_AUDIO_CONTINUITY_READ_CMD": str(read_cmd), "PROGRAM_AUDIO_CONTINUITY_STEP_S": "0",
         "PROGRAM_AUDIO_CONTINUITY_WAIT_S": "0", "HOST": host, "DIR": str(tmp_path),
         "HERE": str(_ROOT / "scripts")}
    e.update(env)
    return subprocess.run(["bash", str(script)], capture_output=True, text=True, env=e, timeout=120)


def _verdict(tmp_path):
    v = tmp_path / "verdict.json"
    v.write_text(json.dumps({"overall_pass": True}))
    return v


def _gate_body():
    return ('if program_audio_continuity_gate "$HERE" "$HOST" "$DIR" "$DIR/verdict.json"; then echo GATE=0; '
            'else echo GATE=1; fi')


def _block(tmp_path):
    return json.loads((tmp_path / "verdict.json").read_text())["program_audio_continuity"]


# --- the gate -------------------------------------------------------------------------------------

def test_a_window_with_lost_slots_fails_the_gate_and_lands_in_the_verdict_json(tmp_path):
    _verdict(tmp_path)
    _marks(tmp_path, 41620, 41710)
    r = _run(tmp_path, _gate_body(), _fake_box(tmp_path))
    assert "GATE=1" in r.stdout and "HARNESS-END" in r.stdout, r.stdout + r.stderr
    b = _block(tmp_path)
    assert b["verdict"] == "FAIL" and b["input"] == "mbc" and b["lost_concealed"] == 3, b
    assert "this run is RED" in r.stderr and "7 slot(s) LOST" in r.stdout


def test_a_clean_window_passes_and_reads_the_input_from_the_box_facts(tmp_path):
    _verdict(tmp_path)
    _marks(tmp_path, 42000, 42100)
    r = _run(tmp_path, _gate_body(), _fake_box(tmp_path))
    assert "GATE=0" in r.stdout, r.stdout + r.stderr
    b = _block(tmp_path)
    assert b["verdict"] == "PASS" and b["input"] == "mbc" and b["checks"]["obs_vban"] == "ran"


def test_a_missing_mark_is_unknown_and_fails(tmp_path):
    _verdict(tmp_path)
    (tmp_path / "program-audio-continuity-stop.mark").write_text(f"{NAME}\t41710\n")
    r = _run(tmp_path, _gate_body(), _fake_box(tmp_path))
    assert "GATE=1" in r.stdout
    b = _block(tmp_path)
    assert b["verdict"] == "UNKNOWN" and "START mark" in b["unknown_reasons"][0], b


def test_obs_restarting_inside_the_window_is_unknown(tmp_path):
    _verdict(tmp_path)
    _marks(tmp_path, 41620, 120, ename="2026-10-23 09-10-00.txt")
    r = _run(tmp_path, _gate_body(), _fake_box(tmp_path))
    assert "GATE=1" in r.stdout and "OBS restarted" in _block(tmp_path)["unknown_reasons"][0]


def test_a_new_log_after_the_stop_mark_is_unknown(tmp_path):
    _verdict(tmp_path)
    _marks(tmp_path, 42000, 42100)
    r = _run(tmp_path, _gate_body(), _fake_box(tmp_path, newest="2026-10-23 09-30-00.txt"))
    assert "GATE=1" in r.stdout and "new log" in _block(tmp_path)["unknown_reasons"][0]


def test_an_unreadable_box_is_unknown_never_a_pass(tmp_path):
    _verdict(tmp_path)
    _marks(tmp_path, 42000, 42100)
    r = _run(tmp_path, _gate_body(), _fake_box(tmp_path, fail=True))
    assert "GATE=1" in r.stdout and "HARNESS-END" in r.stdout
    assert "could not be read" in _block(tmp_path)["unknown_reasons"][0]


def test_the_gate_waits_for_the_closing_lines_then_grades(tmp_path):
    # the first reads see the log cut at 42110 (no closing status line yet), a later one the rest
    _verdict(tmp_path)
    _marks(tmp_path, 42000, 42100)
    short = _fake_box(tmp_path, upto=42110)
    full_box = tmp_path / "full"
    full_box.mkdir()
    full = _fake_box(full_box)
    flip = tmp_path / "flip.sh"
    flip.write_text(f"""#!/usr/bin/env bash
set -euo pipefail
n=$(cat '{tmp_path}/count' 2>/dev/null || echo 0); echo $((n + 1)) > '{tmp_path}/count'
if [ "$n" -lt 2 ]; then exec '{short}' "$@"; else exec '{full}' "$@"; fi
""")
    flip.chmod(0o755)
    r = _run(tmp_path, _gate_body(), flip, PROGRAM_AUDIO_CONTINUITY_WAIT_S="5", PROGRAM_AUDIO_CONTINUITY_STEP_S="1")
    assert "GATE=0" in r.stdout, r.stdout + r.stderr
    assert "not in the log yet" in r.stdout
    assert int((tmp_path / "count").read_text()) == 3


def test_a_windows_stream_box_grades_guard_and_asrc_and_names_vban_na(tmp_path):
    _verdict(tmp_path)
    _marks(tmp_path, 42000, 42100)
    r = _run(tmp_path, _gate_body(), _fake_box(tmp_path), PROGRAM_AUDIO_CONTINUITY_PLATFORM="windows")
    assert "GATE=0" in r.stdout, r.stdout + r.stderr
    b = _block(tmp_path)
    assert b["vban"] is False and b["checks"]["obs_vban"].startswith("n/a") and b["input"] == "mbc"


# --- the marks ------------------------------------------------------------------------------------

def test_a_mark_records_the_line_count_and_never_aborts_the_caller(tmp_path):
    r = _run(tmp_path, 'program_audio_continuity_mark start "$DIR" "$HOST"', _fake_box(tmp_path))
    assert "HARNESS-END" in r.stdout
    assert (tmp_path / "program-audio-continuity-start.mark").read_text() == f"{NAME}\t42292\n"
    r = _run(tmp_path, 'program_audio_continuity_mark stop "$DIR" "$HOST"', _fake_box(tmp_path, fail=True))
    assert "HARNESS-END" in r.stdout and "will read UNKNOWN" in r.stderr
    assert not (tmp_path / "program-audio-continuity-stop.mark").exists()


def test_a_mark_refuses_a_name_that_is_not_an_obs_log_name(tmp_path):
    r = _run(tmp_path, 'program_audio_continuity_mark start "$DIR" "$HOST"',
             _fake_box(tmp_path, newest="x'; rm -rf ~; '.txt"))
    assert "HARNESS-END" in r.stdout
    assert not (tmp_path / "program-audio-continuity-start.mark").exists()


def test_the_remote_texts_quote_the_log_name():
    r = subprocess.run(["bash", "-c", f'. "{LIB}"; _program_audio_continuity_sh slice "$1" 41400', "_", NAME],
                       capture_output=True, text=True)
    assert f"tail -n +41400 -- \"$d\"/'{NAME}'" in r.stdout, r.stdout


# --- the wiring in recording-e2e.sh ----------------------------------------------------------------

def _e2e():
    return E2E.read_text()


def test_the_lib_is_sourced_once():
    assert _e2e().count('. "$HERE/lib/program-audio-continuity.sh"') == 1


def test_the_window_start_mark_is_taken_right_before_start_record():
    s = _e2e()
    mark = s.index('program_audio_continuity_mark start "$OUTDIR" "$STREAM"')
    start_rec = s.index('record --host "$STREAM" --action start')
    assert mark < start_rec
    assert s.count("program_audio_continuity_mark start") == 1
    between = s[mark:start_rec]
    assert "python3" in between and "StartRecord" in between  # the banner + the strih start sit between


def test_the_window_stop_mark_is_taken_right_after_stop_record():
    s = _e2e()
    stop_rec = s.index('record --host "$STREAM" --action stop)')
    mark = s.index('program_audio_continuity_mark stop "$OUTDIR" "$STREAM"')
    assert stop_rec < mark < s.index("cg_chain_after_stoprecord ")
    assert s.count("program_audio_continuity_mark stop") == 1


def test_the_gate_folds_into_gate_before_the_report_and_the_exit():
    s = _e2e()
    merge = s.index('"$VERDICT_BIN" "${MERGE_ARGS[@]}" || GATE=$?')
    calls = [m.start() for m in re.finditer(re.escape('program_audio_continuity_gate "$HERE" "$STREAM" "$OUTDIR" "$REPORT_JSON"'), s)]
    assert len(calls) == 2, calls  # the execute branch and the legacy decode path
    report = s.index('e2e_discord_report_send "$REPORT_JSON"')
    first_exit = s.index('exit "$GATE"', merge)
    assert merge < calls[0] < report < first_exit
    fold = 'then [ "$GATE" -ne 0 ] || GATE=1; fi'
    assert s[calls[0]:calls[0] + 200].count(fold) == 1
    assert s[calls[1]:calls[1] + 200].count(fold) == 1
    assert calls[1] > first_exit and calls[1] < s.rindex('exit "$GATE"')


def test_the_fold_never_lowers_a_red_gate_under_set_e(tmp_path):
    body = ('GATE=3; f() { return 1; }; if ! f; then [ "$GATE" -ne 0 ] || GATE=1; fi; echo "G=$GATE"\n'
            'GATE=0; if ! f; then [ "$GATE" -ne 0 ] || GATE=1; fi; echo "G=$GATE"')
    r = _run(tmp_path, body, "/bin/false")
    assert "G=3\nG=1" in r.stdout and "HARNESS-END" in r.stdout
