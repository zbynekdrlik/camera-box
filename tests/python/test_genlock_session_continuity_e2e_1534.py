"""issue 1534 (owner ROZHODNUTE 18.11.2026) -- the E2E side of the BLOCKING whole-session video gate:
scripts/lib/genlock-session-continuity.sh (the marks on each box's newest OBS log, the session read
through the strih log reader's `session` op, the decision, the verdict-JSON merge), its wiring in
scripts/recording-e2e.sh, and the report (scripts/e2e_report_session_continuity.py through
scripts/e2e_discord_report.py).

The boxes are faked by GENLOCK_SESSION_CONTINUITY_READ_CMD, which RUNS the lib's real remote text in a
fake home per host (strih 10.77.9.202, stream 10.77.9.204: both linux-genlock fleet rows), so the
`stat`, the grep and the awk the box would run are the ones tested. The lib runs under the caller's
`set -euo pipefail` (recording-e2e.sh's mode); values reach bash as environment / arguments.
"""
import copy
import json
import pathlib
import subprocess
import sys

_HERE = pathlib.Path(__file__).resolve().parent
REPO = _HERE.parents[1]
for p in (REPO / "scripts", _HERE):
    if str(p) not in sys.path:
        sys.path.insert(0, str(p))

import e2e_discord_report as edr  # noqa: E402
import genlock_session_continuity as gsc  # noqa: E402
from test_genlock_session_continuity_1534 import session  # noqa: E402

LIB = REPO / "scripts/lib/genlock-session-continuity.sh"
E2E = REPO / "scripts/recording-e2e.sh"
CAMSET = REPO / "scripts/camera-set.sh"
STRIH, STREAM = "10.77.9.202", "10.77.9.204"
LOG = "2026-11-18 18-32-39.txt"
CAMS = ("JXS cam2 src", "JXS cam3 src", "JXS cam5 src")
PGM = "JXS PGM src"
FIX = _HERE / "fixtures" / "e2e_discord_report"
META = {"run_id": "42", "run_url": "https://example.invalid/run/42"}


def _boxes(tmp_path):
    homes = {}
    for role in ("strih", "stream"):
        h = tmp_path / f"home-{role}"
        (h / ".config/obs-studio/logs").mkdir(parents=True)
        homes[role] = h
    cmd = tmp_path / "read.sh"
    cmd.write_text(f"""#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  {STRIH}) export HOME='{homes['strih']}' ;;
  {STREAM}) export HOME='{homes['stream']}' ;;
  *) exit 255 ;;
esac
[ ! -e "$HOME/down" ] || exit 255
exec bash -c "$2"
""")
    cmd.chmod(0o755)
    return homes, cmd


def _split(text, at):
    lines = text.split("\n")
    return "\n".join(lines[:at]) + "\n", "\n".join(lines[at:])


def _logs(tmp_path, homes, strih_text, stream_text):
    """Each box's session in three parts: up to the start mark, the recording, after the stop mark."""
    for role, text in (("strih", strih_text), ("stream", stream_text)):
        n = len(text.split("\n"))
        a, rest = _split(text, int(n * 0.6))
        b, c = _split(rest, int(n * 0.2))
        for i, part in enumerate((a, b, c)):
            (tmp_path / f"{role}-part{i}.txt").write_text(part)
        (homes[role] / ".config/obs-studio/logs" / LOG).write_text("")


def _run(tmp_path, cmd, body, **env):
    script = tmp_path / "harness.sh"
    script.write_text("set -euo pipefail\n" + f'. "{CAMSET}"\n' + f'. "{LIB}"\n' + body + "\necho HARNESS-END\n")
    e = {"PATH": "/usr/local/bin:/usr/bin:/bin", "HOME": str(tmp_path),
         "GENLOCK_SESSION_CONTINUITY_READ_CMD": str(cmd), "GENLOCK_SESSION_CONTINUITY_STEP_S": "0",
         "GENLOCK_SESSION_CONTINUITY_WAIT_S": "0", "DIR": str(tmp_path), "HERE": str(REPO / "scripts"),
         "STRIH": STRIH, "STREAM": STREAM, "ALL_CAMBOX": "1", "CAMERA_ACTIVE_SET": "cam2 cam3 cam5",
         "CAMERA_STRIH_JXS_SET": "cam2 cam3 cam5", "STREAM_PROG_SOURCE": PGM, "STRIH_PROG_SOURCE": "JXS cam5 src"}
    e.update(env)
    return subprocess.run(["bash", str(script)], capture_output=True, text=True, env=e, timeout=120)


BODY = f"""add() {{ cat "$DIR/strih-part$1.txt" >> "$DIR/home-strih/.config/obs-studio/logs/{LOG}"
  cat "$DIR/stream-part$1.txt" >> "$DIR/home-stream/.config/obs-studio/logs/{LOG}"; }}
add 0
genlock_session_continuity_mark start "$DIR" "$STRIH" "$STREAM"
add 1
genlock_session_continuity_mark stop "$DIR" "$STRIH" "$STREAM"
add 2
printf '{{"overall_pass": true}}' > "$DIR/verdict.json"
if genlock_session_continuity_gate "$HERE" "$STRIH" "$STREAM" "$DIR" "$DIR/verdict.json"; then echo GATE=0; else echo GATE=1; fi
"""


def _block(tmp_path):
    return json.loads((tmp_path / "verdict.json").read_text())["genlock_session_continuity"]


def _gate(tmp_path, strih_edits=None, stream_edits=None, body=BODY, **env):
    homes, cmd = _boxes(tmp_path)
    _logs(tmp_path, homes, session(minutes=20, names=CAMS, edits=strih_edits),
          session(minutes=20, names=(PGM,), edits=stream_edits))
    r = _run(tmp_path, cmd, body, **env)
    assert "HARNESS-END" in r.stdout, r.stdout + r.stderr
    return r


def test_a_clean_session_on_both_boxes_passes_and_merges(tmp_path):
    r = _gate(tmp_path)
    assert "GATE=0" in r.stdout, r.stdout + r.stderr
    blk = _block(tmp_path)
    assert blk["verdict"] == "PASS" and blk["gates_overall_pass"] is True
    assert set(blk["boxes"]) == {"strih", "stream"}
    assert {i["input"] for i in blk["boxes"]["strih"]["inputs"]} == set(CAMS)
    assert blk["boxes"]["strih"]["declared"] == list(CAMS)


def test_an_underrun_before_the_window_fails_the_run(tmp_path):
    # 5 min into a 20 min session: in the first part, long before the start mark
    r = _gate(tmp_path, strih_edits={("audit", "JXS cam3 src", 300): {"u": 2}})
    assert "GATE=1" in r.stdout, r.stdout + r.stderr
    blk = _block(tmp_path)
    assert blk["verdict"] == "FAIL"
    assert any("strih 'JXS cam3 src' underruns +2" in f for f in blk["failures"]), blk["failures"]


def test_a_lossy_program_window_on_the_stream_fails_the_run(tmp_path):
    r = _gate(tmp_path, stream_edits={("jxs", PGM, 200): {"lost": 4, "conc": 1}})
    assert "GATE=1" in r.stdout
    assert any(f.startswith("stream 'JXS PGM src' JPEG XS 1 lossy") for f in _block(tmp_path)["failures"])


def test_no_marks_is_unknown_and_red(tmp_path):
    body = BODY.replace('genlock_session_continuity_mark start "$DIR" "$STRIH" "$STREAM"\n', "")
    r = _gate(tmp_path, body=body)
    assert "GATE=1" in r.stdout
    blk = _block(tmp_path)
    assert blk["verdict"] == "UNKNOWN"
    assert any("no window START mark" in u for u in blk["unknown_reasons"])


def test_a_new_log_after_the_window_is_unknown(tmp_path):
    body = BODY.replace("add 2\n", f'add 2\ntouch -d "+1 minute" "$DIR/home-stream/.config/obs-studio/logs/'
                                   f'2026-11-18 20-00-00.txt"\n')
    r = _gate(tmp_path, body=body)
    assert "GATE=1" in r.stdout
    assert any("OBS restarted" in u for u in _block(tmp_path)["unknown_reasons"])


def test_an_unreachable_box_is_unknown_and_red(tmp_path):
    body = BODY.replace("add 2\n", 'add 2\ntouch "$DIR/home-strih/down"\n')
    r = _gate(tmp_path, body=body)
    assert "GATE=1" in r.stdout
    assert _block(tmp_path)["boxes"]["strih"]["verdict"] == "UNKNOWN"


def test_a_camera_acked_offline_is_graded_report_only(tmp_path):
    r = _gate(tmp_path, strih_edits={("audit", "JXS cam3 src", 300): {"u": 2}}, PREFLIGHT_EXCLUDED_CAMS="cam3",
              CAMERA_ACTIVE_SET="cam2 cam3 cam5")
    assert "GATE=0" in r.stdout, r.stdout + r.stderr
    cam3 = next(i for i in _block(tmp_path)["boxes"]["strih"]["inputs"] if i["input"] == "JXS cam3 src")
    assert cam3["excluded"] is True and cam3["verdict"] == "FAIL"


def test_a_bad_wait_knob_never_aborts_the_run(tmp_path):
    r = _gate(tmp_path, GENLOCK_SESSION_CONTINUITY_WAIT_S="abc", GENLOCK_SESSION_CONTINUITY_STEP_S="x")
    assert "GATE=0" in r.stdout, r.stdout + r.stderr


def test_a_windows_class_box_is_unknown_before_any_contact(tmp_path):
    r = _gate(tmp_path, STREAM="10.77.9.205")
    assert "GATE=1" in r.stdout
    assert any("not a Linux OBS box" in u for u in _block(tmp_path)["unknown_reasons"])


def test_a_declared_camera_missing_from_the_log_is_unknown(tmp_path):
    r = _gate(tmp_path, CAMERA_ACTIVE_SET="cam2 cam3 cam5 cam1", CAMERA_STRIH_JXS_SET="cam1 cam2 cam3 cam5")
    assert "GATE=1" in r.stdout
    assert any("'JXS cam1 src'" in u for u in _block(tmp_path)["unknown_reasons"])


# --- recording-e2e.sh wiring ---------------------------------------------------------------------

def _e2e():
    return E2E.read_text()


def test_the_lib_is_sourced_once():
    assert _e2e().count('. "$HERE/lib/genlock-session-continuity.sh"') == 1


def test_the_marks_sit_right_after_the_jpeg_xs_marks():
    s = _e2e()
    for label in ("start", "stop"):
        jxs = s.index(f'jxs_link_continuity_mark {label} "$OUTDIR" "$STRIH" "$STREAM"')
        ours = s.index(f'genlock_session_continuity_mark {label} "$OUTDIR" "$STRIH" "$STREAM" || true')
        assert jxs < ours and s.count(f"genlock_session_continuity_mark {label} ") == 1
        assert "\n" not in s[s.index("\n", jxs) + 1:ours]
    assert s.index("genlock_session_continuity_mark start") < s.index("[5/8] StartRecord on strih")


def test_the_gate_folds_into_gate_in_both_verdict_tails():
    s = _e2e()
    call = ('genlock_session_continuity_gate "$HERE" "$STRIH" "$STREAM" "$OUTDIR" "$REPORT_JSON"; '
            'then [ "$GATE" -ne 0 ] || GATE=1; fi')
    assert s.count(call) == 2
    for pos in [i for i in range(len(s)) if s.startswith(call, i)]:
        before = s.rfind("jxs_link_continuity_gate", 0, pos)
        assert before > 0 and pos - before < 400
    assert s.rindex(call) < s.rindex('exit "$GATE"')


# --- the report --------------------------------------------------------------------------------

def _pass_verdict():
    return json.loads((FIX / "verdict_real_pass_reportonly_1104689227.json").read_text())


def _read(text):
    return "#newest\t" + LOG + "\t" + str(len(text.encode()) + 10) + "\n" + "\n".join(
        f"{i}:{line}" for i, line in enumerate(l for l in text.split("\n")
                                                if "genlock-fifo audit" in l or "jxs-source" in l)) + "\n"


def _blk(edits=None):
    strih = gsc.decide_box("strih", _read(session(names=CAMS, edits=edits)), list(CAMS), log_name=LOG,
                           start_byte=10**9, stop_byte=1)
    return gsc.combine([strih])


def _with(blk):
    v = _pass_verdict()
    v["genlock_session_continuity"] = blk
    return v


def test_a_fail_block_turns_a_passing_merge_red_and_names_the_input():
    text = edr.compose_summary(_with(_blk({("audit", "JXS cam3 src", 300): {"u": 3}})), META)
    assert text.startswith("❌"), text
    assert "Strata obrazu od štartu OBS" in text and "strih 'JXS cam3 src': underruns +3 po ustálení" in text
    assert "after its start settle" not in text  # the Slovak report never carries the English lines


def test_a_lossy_window_is_called_lost():
    text = edr.compose_summary(_with(_blk({("jxs", "JXS cam5 src", 300): {"lost": 2, "conc": 1}})), META)
    assert "zamaskované = STRATENÉ" in text


def test_an_unknown_block_is_red():
    blk = gsc.combine([gsc.unknown_box("stream", "no window START mark", [PGM])])
    text = edr.compose_summary(_with(blk), META)
    assert text.startswith("❌") and "NEOVERENÉ = ZLYHANIE" in text


def test_a_pass_block_keeps_the_three_line_pass_and_no_block_is_byte_identical():
    base = edr.compose_summary(_pass_verdict(), META)
    assert edr.compose_summary(_with(_blk()), META) == base
    v = _pass_verdict()
    assert edr.compose_summary(copy.deepcopy(v), META) == edr.compose_summary(v, META)


def test_the_full_report_has_a_section_with_one_line_per_input():
    rep = edr.compose_report(_with(_blk({("audit", "JXS cam3 src", 300): {"u": 1}})), META)
    assert "Obraz od štartu OBS (celá session" in rep
    for cam in CAMS:
        assert f"strih '{cam}'" in rep
    assert "❌ FAIL" in rep


# --- the run's own camera handling, recorded durably on dev1 (E2E 44502999091, 19.11.2026) ------

TOUCH_BODY = BODY.replace('add 0\n', 'add 0\ngenlock_session_touch begin "$RUN_ID" "$STRIH"\n', 1)


def test_the_runs_own_camera_restart_inside_its_touch_passes(tmp_path):
    # second 800 lies in the recording part, after the run's touch begin
    r = _gate(tmp_path, strih_edits={("audit", "JXS cam5 src", 800): {"u": 52786}}, body=TOUCH_BODY,
              RUN_ID="42", GENLOCK_SESSION_TOUCH_FILE=str(tmp_path / "touch.tsv"))
    assert "GATE=0" in r.stdout, r.stdout + r.stderr
    line = (tmp_path / "touch.tsv").read_text().strip().split("\t")
    assert line[0] == LOG and line[2] == "-" and line[3] == "42", line
    assert _block(tmp_path)["boxes"]["strih"]["run_touch"][0][1] is None


def test_a_loss_before_the_runs_touch_still_fails(tmp_path):
    r = _gate(tmp_path, strih_edits={("audit", "JXS cam3 src", 300): {"u": 2}}, body=TOUCH_BODY,
              RUN_ID="42", GENLOCK_SESSION_TOUCH_FILE=str(tmp_path / "touch.tsv"))
    assert "GATE=1" in r.stdout, r.stdout + r.stderr


def test_end_closes_only_this_runs_open_touch(tmp_path):
    homes, cmd = _boxes(tmp_path)
    (homes["strih"] / ".config/obs-studio/logs" / LOG).write_text("x" * 100)
    touch = tmp_path / "touch.tsv"
    touch.write_text(f"{LOG}\t10\t-\t41\n")
    body = ('genlock_session_touch begin 42 "$STRIH"\n'
            f'printf "%0200d" 0 >> "$DIR/home-strih/.config/obs-studio/logs/{LOG}"\n'
            'genlock_session_touch end 42 "$STRIH"\n_gsc_touch_args "' + LOG + '" 43\n')
    r = _run(tmp_path, cmd, body, GENLOCK_SESSION_TOUCH_FILE=str(touch))
    assert "HARNESS-END" in r.stdout, r.stdout + r.stderr
    assert touch.read_text() == f"{LOG}\t10\t-\t41\n{LOG}\t100\t300\t42\n"
    assert "--touch\n10-~\n--touch\n100-300\n" in r.stdout, r.stdout


def test_the_touch_calls_sit_before_the_first_camera_mutation_and_end_cleanup():
    s = _e2e()
    begin = s.find('genlock_session_touch begin "$RUN_ID" "$STRIH"')
    assert begin > 0 and s.count('genlock_session_touch begin "$RUN_ID" "$STRIH"') == 1
    assert begin < s.find('stray_session_check_assert "$HERE" "$STRIH" "$STREAM" "the camera-box + painter parity auto-align"')
    end = s.find('genlock_session_touch end "$RUN_ID" "$STRIH"')
    assert s.count('genlock_session_touch end "$RUN_ID" "$STRIH"') == 1
    tail = ('  bkshading_e2e_pause_restore cam2 "$PAINTER_IP" "$CAM_PW" "${BKSH_PAUSE_PAINTER_WAS_ACTIVE:-0}"\n'
            '  genlock_session_touch end "$RUN_ID" "$STRIH" || true')
    assert s.count(tail) == 1, "the end call must be the last line of cleanup()"
    at = s.find(tail)
    assert at < end < s.find("\n}\n", at) < s.find("trap cleanup EXIT HUP INT TERM")
    assert s.find("\n}\n", at) == s.find("\n", end)  # nothing between the end call and cleanup's brace
