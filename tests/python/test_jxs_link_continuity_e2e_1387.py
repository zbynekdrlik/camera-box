"""issue 1387 slice 2 (design 6558387590) -- the full-path E2E's BLOCKING JPEG XS link continuity gate:
scripts/lib/jxs-link-continuity.sh (the byte marks on each box's newest OBS log, the bounded read, the
decision, the verdict-JSON merge) and its wiring in scripts/recording-e2e.sh.

The boxes are faked by JXS_LINK_CONTINUITY_READ_CMD, which RUNS the lib's real remote text in a fake
home per host (strih 10.77.9.202, stream 10.77.9.204: both linux-genlock fleet rows), so the `stat`,
`tail -c`, `head -c` and `grep -b` the box would run are the ones tested. The lib runs under the
caller's `set -euo pipefail` (recording-e2e.sh's mode); values reach bash as ARGUMENTS.
"""
import json
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
REPO = HERE.parent.parent
LIB = REPO / "scripts/lib/jxs-link-continuity.sh"
E2E = REPO / "scripts/recording-e2e.sh"
CAMSET = REPO / "scripts/camera-set.sh"

from jxs_link_fakes_1387 import LOG_NAME, OUTPUT, noise, source_line, ts, window_set  # noqa: E402

STRIH, STREAM = "10.77.9.202", "10.77.9.204"
CAMS = ["JXS cam2 src", "JXS cam3 src", "JXS cam5 src"]
PGM = "JXS PGM src"
T0 = 14 * 3600


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
printf '%s\\n' "$2" >> '{tmp_path}/remote-texts.log'
exec bash -c "$2"
""")
    cmd.chmod(0o755)
    return homes, cmd


def _phases(tmp_path, homes, strih_phases, stream_phases, name=LOG_NAME):
    """Each box's log in three phases: before the start mark, the recording, after the stop mark."""
    for role, phases in (("strih", strih_phases), ("stream", stream_phases)):
        for i, lines in enumerate(phases):
            (tmp_path / f"{role}-phase{i}.txt").write_text("".join(l + "\n" for l in lines))
        (homes[role] / ".config/obs-studio/logs" / name).write_text("")


def _windows(t0, n, names, output, src=None, out=None):
    lines, t = [], t0
    for k in range(n):
        lines += window_set(t, names, output, src=(src or {}).get(k), out=(out or {}).get(k))
        t += 5.0
    return lines, t


def _script(tmp_path, body):
    s = tmp_path / "harness.sh"
    s.write_text("set -euo pipefail\n" + f'. "{CAMSET}"\n' + f'. "{LIB}"\n' + body + "\necho HARNESS-END\n")
    return s


def _run(tmp_path, cmd, body, **env):
    e = {"PATH": "/usr/local/bin:/usr/bin:/bin", "HOME": str(tmp_path), "JXS_LINK_CONTINUITY_READ_CMD": str(cmd),
         "JXS_LINK_CONTINUITY_STEP_S": "0", "JXS_LINK_CONTINUITY_WAIT_S": "0", "DIR": str(tmp_path),
         "HERE": str(REPO / "scripts"), "STRIH": STRIH, "STREAM": STREAM, "STRIH_PW": "pw", "STREAM_PW": "pw",
         "E2E_PROGRAM_PATH": "jxs", "ALL_CAMBOX": "1", "CAMERA_ACTIVE_SET": "cam2 cam3 cam5",
         "CAMERA_STRIH_JXS_SET": "cam2 cam3 cam5", "STREAM_PROG_SOURCE": PGM, "STRIH_PROG_SOURCE": "JXS cam5 src"}
    e.update(env)
    return subprocess.run(["bash", str(_script(tmp_path, body))], capture_output=True, text=True, env=e,
                          timeout=120)


def _full_run_body(gate=True):
    """Phase 0, the start mark, phase 1, the stop mark, phase 2, then the gate."""
    logs = '"$1"/.config/obs-studio/logs'
    body = f"""add() {{ cat "$DIR/strih-phase$1.txt" >> "$DIR/home-strih/.config/obs-studio/logs/{LOG_NAME}"
  cat "$DIR/stream-phase$1.txt" >> "$DIR/home-stream/.config/obs-studio/logs/{LOG_NAME}"; }}
add 0
jxs_link_continuity_mark start "$DIR" "$STRIH" "$STREAM"
add 1
jxs_link_continuity_mark stop "$DIR" "$STRIH" "$STREAM"
add 2
"""
    del logs
    if gate:
        body += ('printf \'{"overall_pass": true}\' > "$DIR/verdict.json"\n'
                 'if jxs_link_continuity_gate "$HERE" "$STRIH" "$STREAM" "$DIR" "$DIR/verdict.json"; then echo GATE=0; '
                 'else echo GATE=1; fi')
    return body


def _clean(src=None, out=None, pgm=None, n=8):
    pre_s, t = _windows(T0, 3, CAMS, True)
    pre_p, _ = _windows(T0, 3, [PGM], False)
    pre_s.append(noise(ts(t - 4.9)))
    pre_p.append(noise(ts(t - 4.9)))
    rec_s, t2 = _windows(t, n, CAMS, True, src=src, out=out)
    rec_p, _ = _windows(t, n, [PGM], False, src=pgm)
    rec_s.append(noise(ts(t2 - 4.0)))
    rec_p.append(noise(ts(t2 - 4.0)))
    post_s, _ = _windows(t2, 3, CAMS, True)
    post_p, _ = _windows(t2, 3, [PGM], False)
    return [pre_s, rec_s, post_s], [pre_p, rec_p, post_p]


def _block(tmp_path):
    return json.loads((tmp_path / "verdict.json").read_text())["jxs_link_continuity"]


# --- the gate ----------------------------------------------------------------------------------------

def test_a_clean_window_passes_every_link_on_both_boxes(tmp_path):
    homes, cmd = _boxes(tmp_path)
    _phases(tmp_path, homes, *_clean())
    r = _run(tmp_path, cmd, _full_run_body())
    assert "GATE=0" in r.stdout and "HARNESS-END" in r.stdout, r.stdout + r.stderr
    b = _block(tmp_path)
    assert b["verdict"] == "PASS" and b["pass"] and b["gates_overall_pass"] is True
    got = {(l["box"], l["link"]) for l in b["links"]}
    assert got == {("strih", c) for c in CAMS} | {("strih", OUTPUT), ("stream", PGM)}
    assert all(l["windows"] == 9 for l in b["links"]), b["links"]
    assert "ok: every JPEG XS link lost nothing" in r.stdout
    marks = (tmp_path / "jxs-link-continuity-strih-start.mark").read_text().rstrip("\n").split("\t")
    assert marks[0] == LOG_NAME and marks[1].isdigit() and re.fullmatch(r"\d\d:\d\d:\d\d\.\d{3}", marks[2])


def test_a_concealed_slice_on_the_program_link_fails_the_run(tmp_path):
    homes, cmd = _boxes(tmp_path)
    _phases(tmp_path, homes, *_clean(pgm={4: {PGM: {"concealed": 2, "lost_slices": 2}}}))
    r = _run(tmp_path, cmd, _full_run_body())
    assert "GATE=1" in r.stdout and "HARNESS-END" in r.stdout, r.stdout + r.stderr
    b = _block(tmp_path)
    assert b["verdict"] == "FAIL"
    assert "stream 'JXS PGM src': concealed +2 (a concealed slice is a LOST slice)" in b["failures"]
    assert "this run is RED" in r.stderr


def test_an_output_busy_drop_fails_the_run(tmp_path):
    homes, cmd = _boxes(tmp_path)
    _phases(tmp_path, homes, *_clean(out={2: {"dropped_busy": 1}}))
    r = _run(tmp_path, cmd, _full_run_body())
    assert "GATE=1" in r.stdout
    assert any(f.startswith(f"strih '{OUTPUT}': dropped_busy +1") for f in _block(tmp_path)["failures"])


def test_a_camera_whose_link_is_missing_is_unknown_and_fails(tmp_path):
    homes, cmd = _boxes(tmp_path)
    s, p = _clean()
    s = [[l for l in ph if "'JXS cam3 src'" not in l] for ph in s]
    _phases(tmp_path, homes, s, p)
    r = _run(tmp_path, cmd, _full_run_body())
    assert "GATE=1" in r.stdout
    b = _block(tmp_path)
    assert b["verdict"] == "UNKNOWN"
    assert any("strih 'JXS cam3 src': no window line" in u for u in b["unknown_reasons"])


def test_the_ndi_path_skips_without_reading_a_box(tmp_path):
    homes, cmd = _boxes(tmp_path)
    _phases(tmp_path, homes, *_clean())
    r = _run(tmp_path, cmd, _full_run_body(), E2E_PROGRAM_PATH="ndi")
    assert "GATE=0" in r.stdout and "SKIPPED (the NDI path)" in r.stdout, r.stdout + r.stderr
    assert not (tmp_path / "remote-texts.log").exists()
    assert not list(tmp_path.glob("jxs-link-continuity-*.mark"))
    assert "jxs_link_continuity" not in json.loads((tmp_path / "verdict.json").read_text())


def test_a_missing_mark_is_unknown_and_never_aborts_the_caller(tmp_path):
    homes, cmd = _boxes(tmp_path)
    _phases(tmp_path, homes, *_clean())
    (homes["stream"] / "down").write_text("")
    r = _run(tmp_path, cmd, _full_run_body())
    assert "HARNESS-END" in r.stdout and "GATE=1" in r.stdout, r.stdout + r.stderr
    assert "no window start mark from the stream box" in r.stderr
    b = _block(tmp_path)
    assert any(u == "stream 'JXS PGM src': no window START mark (the stream OBS log was not readable before StartRecord)"
               for u in b["unknown_reasons"]), b["unknown_reasons"]
    assert all(l["verdict"] == "PASS" for l in b["links"] if l["box"] == "strih")


def test_obs_restarting_inside_the_window_is_unknown(tmp_path):
    homes, cmd = _boxes(tmp_path)
    _phases(tmp_path, homes, *_clean())
    body = _full_run_body(gate=False) + (
        f'cp "$DIR/home-strih/.config/obs-studio/logs/{LOG_NAME}" "$DIR/home-strih/.config/obs-studio/logs/2026-11-01 09-00-00.txt"\n'
        f'touch -d "+1 minute" "$DIR/home-strih/.config/obs-studio/logs/2026-11-01 09-00-00.txt"\n'
        'printf \'{"overall_pass": true}\' > "$DIR/verdict.json"\n'
        'if jxs_link_continuity_gate "$HERE" "$STRIH" "$STREAM" "$DIR" "$DIR/verdict.json"; then echo GATE=0; else echo GATE=1; fi')
    r = _run(tmp_path, cmd, body)
    assert "GATE=1" in r.stdout
    assert any("OBS restarted" in u for u in _block(tmp_path)["unknown_reasons"])


def test_a_windows_class_box_is_unknown_before_any_contact(tmp_path):
    homes, cmd = _boxes(tmp_path)
    _phases(tmp_path, homes, *_clean())
    r = _run(tmp_path, cmd, _full_run_body(), STREAM="192.0.2.10")
    assert "GATE=1" in r.stdout and "HARNESS-END" in r.stdout
    texts = (tmp_path / "remote-texts.log").read_text()
    assert texts.count("stat -c %s") >= 2  # strih read; the stream box was never dialled
    assert any("is not a Linux OBS box" in u for u in _block(tmp_path)["unknown_reasons"])


def test_the_gate_waits_for_a_closing_window_then_grades(tmp_path):
    homes, cmd = _boxes(tmp_path)
    s, p = _clean()
    _phases(tmp_path, homes, s, p)
    # phase 2 arrives only after the first read: a fake sleep appends it
    fake = tmp_path / "bin"
    fake.mkdir()
    (fake / "sleep").write_text(f"""#!/usr/bin/env bash
cat '{tmp_path}/strih-phase2.txt' >> '{homes['strih']}/.config/obs-studio/logs/{LOG_NAME}'
cat '{tmp_path}/stream-phase2.txt' >> '{homes['stream']}/.config/obs-studio/logs/{LOG_NAME}'
echo "slept $*" >> '{tmp_path}/slept.log'
""")
    (fake / "sleep").chmod(0o755)
    body = _full_run_body().replace("add 2\n", "")
    r = _run(tmp_path, cmd, body, PATH=f"{fake}:/usr/local/bin:/usr/bin:/bin", JXS_LINK_CONTINUITY_STEP_S="5",
             JXS_LINK_CONTINUITY_WAIT_S="10")
    assert "GATE=0" in r.stdout, r.stdout + r.stderr
    assert "a closing window is not in the log yet -- reading again in 5 s (1/3)" in r.stdout
    assert (tmp_path / "slept.log").read_text().count("slept") == 1


def test_a_block_that_could_not_be_merged_is_named_loudly(tmp_path):
    homes, cmd = _boxes(tmp_path)
    _phases(tmp_path, homes, *_clean())
    body = _full_run_body(gate=False) + (
        'printf \'not json\' > "$DIR/verdict.json"\n'
        'if jxs_link_continuity_gate "$HERE" "$STRIH" "$STREAM" "$DIR" "$DIR/verdict.json"; then echo GATE=0; else echo GATE=1; fi')
    r = _run(tmp_path, cmd, body)
    assert "GATE=0" in r.stdout  # the links passed; the report would not show it
    assert "could not be merged into" in r.stderr


def test_a_single_camera_run_grades_the_camera_on_strih_program(tmp_path):
    homes, cmd = _boxes(tmp_path)
    _phases(tmp_path, homes, *_clean())
    r = _run(tmp_path, cmd, _full_run_body(), ALL_CAMBOX="0")
    assert "GATE=0" in r.stdout
    got = {(l["box"], l["link"]) for l in _block(tmp_path)["links"]}
    assert got == {("strih", "JXS cam5 src"), ("strih", OUTPUT), ("stream", PGM)}


def test_an_acked_offline_camera_is_left_out_and_no_input_at_all_is_unknown(tmp_path):
    homes, cmd = _boxes(tmp_path)
    _phases(tmp_path, homes, *_clean())
    r = _run(tmp_path, cmd, _full_run_body(), PREFLIGHT_EXCLUDED_CAMS="cam3")
    assert "GATE=0" in r.stdout
    assert "JXS cam3 src" not in {l["link"] for l in _block(tmp_path)["links"]}
    r = _run(tmp_path, cmd, _full_run_body(), STREAM_PROG_SOURCE="")
    assert "GATE=1" in r.stdout
    assert any("no measured stream JPEG XS input resolved" in u for u in _block(tmp_path)["unknown_reasons"])


def test_the_read_is_bounded_never_a_whole_log_read(tmp_path):
    homes, cmd = _boxes(tmp_path)
    s, p = _clean()
    filler = [noise(ts(T0 - 600 + k * 0.01), k) for k in range(20000)]  # ~1.6 MB of older log
    s[0] = filler + s[0]
    _phases(tmp_path, homes, s, p)
    r = _run(tmp_path, cmd, _full_run_body(), JXS_LINK_CONTINUITY_LOOKBACK="8192")
    assert "GATE=0" in r.stdout, r.stdout + r.stderr
    texts = (tmp_path / "remote-texts.log").read_text()
    assert "wc -l" not in texts and "cat " not in texts and "tail -n +" not in texts
    assert "nice -n 19 tail -c +" in texts and "| head -c " in texts and "grep -a -b -E" in texts
    slice_bytes = (tmp_path / "jxs-link-continuity-strih-slice.txt").stat().st_size
    log_bytes = (homes["strih"] / ".config/obs-studio/logs" / LOG_NAME).stat().st_size
    assert slice_bytes < log_bytes / 20, (slice_bytes, log_bytes)  # only the window's JPEG XS lines


# --- the wiring in recording-e2e.sh --------------------------------------------------------------------

def _e2e():
    return E2E.read_text()


def test_the_lib_is_sourced_once_after_the_program_input_resolver():
    s = _e2e()
    assert s.count('. "$HERE/lib/jxs-link-continuity.sh"') == 1
    assert s.index('. "$HERE/lib/program-inputs.sh"') < s.index('. "$HERE/lib/jxs-link-continuity.sh"')


def test_the_marks_sit_next_to_the_program_audio_marks():
    s = _e2e()
    start = s.index('jxs_link_continuity_mark start "$OUTDIR" "$STRIH" "$STREAM" || true')
    assert s.index('program_audio_continuity_mark start "$OUTDIR" "$STREAM"') < start
    assert start < s.index('record --host "$STREAM" --action start\nSTREAM_RECORDING_STARTED=1')
    stop = s.index('jxs_link_continuity_mark stop "$OUTDIR" "$STRIH" "$STREAM" || true')
    assert s.index('program_audio_continuity_mark stop "$OUTDIR" "$STREAM"') < stop < s.index("cg_chain_after_stoprecord ")
    assert s.count("jxs_link_continuity_mark start") == 1 and s.count("jxs_link_continuity_mark stop") == 1


def test_the_gate_folds_into_gate_in_both_tails_before_the_report_and_the_exit():
    s = _e2e()
    merge = s.index('"$VERDICT_BIN" "${MERGE_ARGS[@]}" || GATE=$?')
    calls = [m.start() for m in re.finditer(
        re.escape('if ! jxs_link_continuity_gate "$HERE" "$STRIH" "$STREAM" "$OUTDIR" "$REPORT_JSON"'), s)]
    assert len(calls) == 2, calls
    first_exit = s.index('exit "$GATE"', merge)
    assert merge < calls[0] < s.index('e2e_discord_report_send "$REPORT_JSON"') < first_exit
    assert first_exit < calls[1] < s.rindex('exit "$GATE"')
    for c in calls:
        assert s[c:s.index("\n", c)].endswith('then [ "$GATE" -ne 0 ] || GATE=1; fi')


def test_the_fold_never_lowers_a_red_gate_under_set_e(tmp_path):
    homes, cmd = _boxes(tmp_path)
    _phases(tmp_path, homes, *_clean(pgm={1: {PGM: {"undecodable": 1}}}))
    body = _full_run_body(gate=False) + (
        'printf \'{"overall_pass": true}\' > "$DIR/verdict.json"\n'
        'GATE=3; if ! jxs_link_continuity_gate "$HERE" "$STRIH" "$STREAM" "$DIR" "$DIR/verdict.json"; '
        'then [ "$GATE" -ne 0 ] || GATE=1; fi; echo "G=$GATE"\n'
        'GATE=0; if ! jxs_link_continuity_gate "$HERE" "$STRIH" "$STREAM" "$DIR" "$DIR/verdict.json"; '
        'then [ "$GATE" -ne 0 ] || GATE=1; fi; echo "G=$GATE"')
    r = _run(tmp_path, cmd, body)
    assert "G=3\n" in r.stdout and "G=1\n" in r.stdout and "HARNESS-END" in r.stdout, r.stdout + r.stderr


def test_the_window_line_marker_the_box_greps_is_the_shipped_prefix():
    line = source_line("14:00:00.000", "JXS cam5 src")
    assert re.match(r"^[0-9:.]+: \[distroav\] jxs-(source|output) ", line)
    assert '"^[0-9:.]+: \\[distroav\\] jxs-(source|output) "' in LIB.read_text()
