"""issue 1462 (owner ROZHODNUTÉ 6377910161) -- part 2: the stream recording decode `[8/8b]` runs on the
LINUX stream box stream-snv, so the fused verdict gets its stream leg (the QR + audio-marker A/V
measurement) and a missing stream leg FAILS the run by name, never a skip.

- scripts/recording-verdict-on-stream-snv.sh = the stream box's entry point onto the ONE Linux on-box
  decode (recording-verdict-on-strih-snv.sh: plain ssh/scp, the issue-1118 sha256 upload gate,
  --skip-if-exists, idle priority on the E-cores, the partial + pixel dir pulled back unchanged). It
  is RUN here against a fake box (tests/python/stream_box_fakes_1462.py: a fake sshpass that runs the
  remote bash text on a stub PATH and copies scp files on this host).
- scripts/lib/stream-lx-verdict.sh = the `[8/8b-pre]` pushes (marker log, switch schedule) onto the
  Linux box + the pull-back note.
- recording-e2e.sh: the Linux branches are cut out between count-1 anchors and RUN under the caller's
  `set -euo pipefail`; the Windows text stays byte-identical in the else.
Tier-0: no rig, no network, no cargo.
"""
import hashlib
import json
import os
import pathlib
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stream_box_fakes_1462 import calls, link_box_tools, make_rig  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
WRAPPER = SCRIPTS / "recording-verdict-on-stream-snv.sh"
LIB = SCRIPTS / "lib" / "stream-lx-verdict.sh"
E2E = SCRIPTS / "recording-e2e.sh"
WIN_STREAM_FLEET = "strih-snv|10.77.9.202|linux-genlock|always\nstream-snv|10.77.9.204|windows-genlock|always"

# The box's recording-verdict: writes the --out partial (+ a pixel dir when FAKE_VERDICT_PIXELS is set),
# logs its argv, exits FAKE_VERDICT_RC (no partial on a failure).
_VERDICT = r'''
import json, os, sys
a = sys.argv[1:]
with open(os.environ["FAKE_VERDICT_LOG"], "a") as f:
    f.write(json.dumps(a) + "\n")
rc = int(os.environ.get("FAKE_VERDICT_RC", "0"))
if rc:
    sys.exit(rc)
out = a[a.index("--out") + 1]
with open(out, "w") as f:
    json.dump({"box": "stream", "argv": a}, f)
if os.environ.get("FAKE_VERDICT_PIXELS"):
    d = out[:-len(".json")] + "-pixels"
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "frame-12.png"), "wb") as f:
        f.write(b"\x89PNG-fake")
'''

BOX_TOOLS = ("sha256sum", "cut", "mkdir", "chmod", "nice", "taskset", "cat", "env", "basename")


def _rig(tmp_path, verdict_body=_VERDICT):
    rig = make_rig(tmp_path)
    link_box_tools(rig, *BOX_TOOLS)
    verdict = tmp_path / "dev1-recording-verdict"
    verdict.write_text("#!%s\n%s" % (sys.executable, verdict_body))
    verdict.chmod(0o755)
    rig["verdict"] = verdict
    rig["verdict_log"] = tmp_path / "verdict.log"
    rig["env"]["FAKE_VERDICT_LOG"] = str(rig["verdict_log"])
    rig["local_out"] = tmp_path / "dev1-out"
    rig["remote_out"] = rig["home"] / "verdict-out"
    rig["remote_bin"] = rig["home"] / "recording-verdict"
    return rig


def _wrapper(rig, *extra_args, env=None, rec=None, out_name="stream-partial-7.json"):
    rec = rec or str(rig["home"] / "2026-10-24 02-58-45.mkv")
    e = dict(rig["env"])
    e.update({"STREAM_LX_BOX": "10.77.9.204"})
    e.update(env or {})
    args = ["bash", str(WRAPPER), "--verdict-bin", str(rig["verdict"]), "--remote-bin", str(rig["remote_bin"]),
            "--out-dir", str(rig["remote_out"]), "--local-out-dir", str(rig["local_out"]), "--stream-rec", rec,
            *extra_args, "--", "--extract-partial", "stream", "--stream", rec, "--capture-fps", "30",
            "--out", str(rig["remote_out"] / out_name)]
    return subprocess.run(args, capture_output=True, text=True, env=e, cwd=str(REPO), timeout=120)


def _verdict_calls(rig):
    p = rig["verdict_log"]
    return [json.loads(x) for x in p.read_text().splitlines()] if p.exists() else []


def _uploads(rig):
    return [c for c in calls(rig) if c["tool"] == "scp" and "@" in c["dst"].split(":", 1)[0] and ":" in c["dst"]]


# --- recording-verdict-on-stream-snv.sh ------------------------------------------------------------

def test_the_stream_box_decodes_its_recording_in_place_and_the_partial_comes_back(tmp_path):
    rig = _rig(tmp_path)
    p = _wrapper(rig, env={"FAKE_VERDICT_PIXELS": "1"})
    assert p.returncode == 0, p.stdout + p.stderr
    # the verdict binary went up once, behind the sha256 gate (absent -> upload)
    assert len(_uploads(rig)) == 1 and rig["remote_bin"].read_bytes() == rig["verdict"].read_bytes()
    # the decode ran ON the box against the recording where it lives (a space in the name, one arg)
    (argv,) = _verdict_calls(rig)
    assert argv[argv.index("--extract-partial") + 1] == "stream"
    assert argv[argv.index("--stream") + 1].endswith("2026-10-24 02-58-45.mkv")
    # ... at idle priority (the E-core pin is added when the box has cpu_atom)
    (decode,) = [c for c in calls(rig) if c["tool"] == "ssh" and "--extract-partial" in c["remote"]]
    assert 'LP="nice -n 19"' in decode["remote"] and "$LP env RUST_LOG=info" in decode["remote"], decode["remote"]
    # the partial + its pixel dir are back on dev1, byte-identical
    local = rig["local_out"] / "stream-partial-7.json"
    assert local.read_bytes() == (rig["remote_out"] / "stream-partial-7.json").read_bytes()
    px = rig["local_out"] / "stream-partial-7-pixels" / "frame-12.png"
    assert px.read_bytes() == b"\x89PNG-fake"
    assert "stream-snv" in p.stdout and "strih-snv" not in p.stdout, p.stdout


def test_an_identical_binary_on_the_box_is_never_uploaded_again(tmp_path):
    rig = _rig(tmp_path)
    assert _wrapper(rig).returncode == 0
    n = len(_uploads(rig))
    assert _wrapper(rig).returncode == 0
    assert len(_uploads(rig)) == n, "identical sha256 -> skip (issue 1118)"


def test_a_stale_binary_on_the_box_is_replaced(tmp_path):
    rig = _rig(tmp_path)
    rig["remote_bin"].write_text("#!/bin/sh\nexit 0\n")
    rig["remote_bin"].chmod(0o755)
    p = _wrapper(rig)
    assert p.returncode == 0, p.stdout + p.stderr
    assert len(_uploads(rig)) == 1
    assert hashlib.sha256(rig["remote_bin"].read_bytes()).hexdigest() == hashlib.sha256(rig["verdict"].read_bytes()).hexdigest()


def test_a_failed_decode_fails_the_stream_leg_and_brings_nothing_back(tmp_path):
    rig = _rig(tmp_path)
    p = _wrapper(rig, env={"FAKE_VERDICT_RC": "3"})
    assert p.returncode != 0, p.stdout + p.stderr
    assert not (rig["local_out"] / "stream-partial-7.json").exists()


def test_an_unreachable_stream_box_fails_the_stream_leg(tmp_path):
    rig = _rig(tmp_path)
    p = _wrapper(rig, env={"FAKE_SSH_DOWN": "1"})
    assert p.returncode != 0, p.stdout + p.stderr
    assert not (rig["local_out"] / "stream-partial-7.json").exists()


def test_an_existing_partial_skips_the_decode(tmp_path):
    rig = _rig(tmp_path)
    have = tmp_path / "have.json"
    have.write_text("{}")
    p = _wrapper(rig, "--skip-if-exists", str(have))
    assert p.returncode == 0 and "SKIP" in p.stdout, p.stdout + p.stderr
    assert calls(rig) == []


def test_a_windows_stream_box_is_refused_before_any_contact(tmp_path):
    rig = _rig(tmp_path)
    p = _wrapper(rig, env={"OBS_FLEET": WIN_STREAM_FLEET})
    assert p.returncode == 2, p.stdout + p.stderr
    assert "recording-verdict-on-stream.sh" in p.stderr and "windows" in p.stderr.lower(), p.stderr
    assert calls(rig) == []


def test_a_missing_recording_path_is_refused_by_name(tmp_path):
    rig = _rig(tmp_path)
    e = dict(rig["env"], STREAM_LX_BOX="10.77.9.204")
    args = ["bash", str(WRAPPER), "--verdict-bin", str(rig["verdict"]), "--stream-rec", "",
            "--", "--extract-partial", "stream", "--out", "/x.json"]
    p = subprocess.run(args, capture_output=True, text=True, env=e, cwd=str(REPO), timeout=60)
    assert p.returncode == 2, p.stdout + p.stderr
    # the wrapper's OWN refusal (the decode it would exec refuses an empty path too, differently)
    assert "StopRecord returned no recording path" in p.stderr and calls(rig) == []


def test_the_wrapper_reuses_the_one_linux_decode_never_a_copy():
    w = WRAPPER.read_text()
    assert "recording-verdict-on-strih-snv.sh" in w
    for copy in ("LOWPRIO_SNIPPET", "verdict_upload_decision", "sha256sum", "scp "):
        assert copy not in w, copy
    for banned in ("FileUpload", "FileDownload", "MCP Shell:", "win_ssh_"):
        assert banned not in w, banned
    assert os.access(WRAPPER, os.X_OK)


# --- scripts/lib/stream-lx-verdict.sh: the [8/8b-pre] pushes --------------------------------------

def _lib(tmp_path, rig, body, env=None):
    e = dict(rig["env"])
    e.update(env or {})
    driver = tmp_path / "lib-driver.sh"
    driver.write_text('set -euo pipefail\n. "%s/lib/stream-box.sh"\n. "%s"\n%s\n' % (SCRIPTS, LIB, body))
    return subprocess.run(["bash", str(driver)], capture_output=True, text=True, env=e, cwd=str(REPO), timeout=60)


def test_a_push_lands_in_the_remote_dir_and_prints_its_path(tmp_path):
    rig = _rig(tmp_path)
    csv = tmp_path / "markers.csv"
    csv.write_text("tick,ts\n1,2\n")
    remote = rig["home"] / "verdict-out"
    p = _lib(tmp_path, rig, 'p="$(stream_lx_push_file newlevel newlevel 10.77.9.204 "%s" "%s" av-markers-7.csv)"\necho "GOT=$p"' % (csv, remote))
    assert p.returncode == 0, p.stderr
    assert "GOT=%s/av-markers-7.csv" % remote in p.stdout
    assert (remote / "av-markers-7.csv").read_text() == csv.read_text()


def test_a_push_to_a_box_that_does_not_answer_fails_by_name(tmp_path):
    rig = _rig(tmp_path)
    csv = tmp_path / "markers.csv"
    csv.write_text("x\n")
    p = _lib(tmp_path, rig, 'stream_lx_push_file newlevel newlevel 10.77.9.204 "%s" /nowhere x.csv\necho AFTER' % csv,
             env={"FAKE_SSH_DOWN": "1"})
    assert p.returncode != 0 and "AFTER" not in p.stdout
    assert "10.77.9.204" in p.stderr and "x.csv" in p.stderr, p.stderr


def test_a_push_of_a_missing_local_file_fails_before_any_contact(tmp_path):
    rig = _rig(tmp_path)
    p = _lib(tmp_path, rig, 'stream_lx_push_file newlevel newlevel 10.77.9.204 /no/such.csv /r x.csv\necho AFTER')
    assert p.returncode != 0 and "AFTER" not in p.stdout
    assert "/no/such.csv" in p.stderr and calls(rig) == []


# --- recording-e2e.sh wiring --------------------------------------------------------------------

def _e2e():
    return E2E.read_text()


def _cut(s, start, end):
    assert s.count(start) == 1, start
    a = s.index(start)
    b = s.index(end, a)
    return s[a:b]


PUSH_START = '  AV_MARKER_WIN="${AV_MARKER_WIN:-$OUT_DIR_WIN\\\\av-markers-${RUN_ID}.csv}"\n'
PUSH_END = '  echo "    --- [8/8b] extract the STREAM partial'


def _run_push(tmp_path, rig, env=None, execute="1"):
    region = _cut(_e2e(), PUSH_START, PUSH_END)
    marker = tmp_path / "av-markers-local.csv"
    marker.write_text("m\n")
    sched = tmp_path / "switch-schedule-local.json"
    sched.write_text("{}")
    text = (
        "set -euo pipefail\n"
        '. "%s/lib/win-ssh-exec.sh"\n. "%s/lib/stream-box.sh"\n. "%s"\n' % (SCRIPTS, SCRIPTS, LIB)
        + 'win_ssh_upload() { echo "WIN_UPLOAD $*"; }\n'
        + 'OUT_DIR_WIN="C:\\\\camera-box\\\\verdict-out" RUN_ID=7 ALL_CAMBOX=1 STREAM=10.77.9.204\n'
        + 'STREAM_USER=newlevel STREAM_PW=newlevel E2E_EXECUTE_VERDICT=%s\n' % execute
        + 'MARKER_CSV="%s" SWITCH_SCHEDULE_JSON="%s"\n' % (marker, sched)
        + region
        + 'echo "AV=$_av_marker_args"\necho "SW=$_switch_schedule_args"\n'
    )
    e = dict(rig["env"], STREAM_LX_REMOTE_OUT_DIR=str(rig["remote_out"]))
    e.update(env or {})
    drv = tmp_path / "push-driver.sh"
    drv.write_text(text)
    return subprocess.run(["bash", str(drv)], capture_output=True, text=True, env=e, cwd=str(REPO), timeout=60)


def test_the_marker_log_and_schedule_reach_a_linux_stream_box_in_both_modes(tmp_path):
    for execute in ("1", "0"):
        (tmp_path / execute).mkdir()
        rig = _rig(tmp_path / execute)
        p = _run_push(tmp_path / execute, rig, execute=execute)
        assert p.returncode == 0, p.stdout + p.stderr
        assert "AV=--av-marker-log %s/av-markers-7.csv" % rig["remote_out"] in p.stdout, p.stdout
        assert "SW=--switch-schedule %s/switch-schedule-7.json" % rig["remote_out"] in p.stdout, p.stdout
        assert (rig["remote_out"] / "av-markers-7.csv").read_text() == "m\n"
        assert (rig["remote_out"] / "switch-schedule-7.json").read_text() == "{}"
        assert "WIN_UPLOAD" not in p.stdout


def test_a_failed_marker_push_to_the_linux_stream_box_ends_the_run(tmp_path):
    rig = _rig(tmp_path)
    p = _run_push(tmp_path, rig, env={"FAKE_SSH_DOWN": "1"})
    assert p.returncode != 0 and "AV=" not in p.stdout, p.stdout + p.stderr


def test_a_windows_stream_box_keeps_the_windows_upload(tmp_path):
    rig = _rig(tmp_path)
    p = _run_push(tmp_path, rig, env={"OBS_FLEET": WIN_STREAM_FLEET})
    assert p.returncode == 0, p.stdout + p.stderr
    assert "WIN_UPLOAD newlevel newlevel 10.77.9.204" in p.stdout
    assert "AV=--av-marker-log C:\\camera-box\\verdict-out\\av-markers-7.csv" in p.stdout, p.stdout
    assert calls(rig) == []


def _stream_extract_fn(s):
    return _cut(s, "  run_stream_extract() {\n", "\n  }\n") + "\n  }\n"


def test_run_stream_extract_decodes_on_a_linux_stream_box_with_the_av_inputs():
    fn = _stream_extract_fn(_e2e())
    lx_at = fn.index('if [ "$(stream_box_platform "$STREAM")" = "linux" ]; then')
    win_at = fn.index('"$HERE/recording-verdict-on-stream.sh" \\')
    assert lx_at < win_at
    lx = fn[lx_at:win_at]
    for need in ('"$HERE/recording-verdict-on-stream-snv.sh" \\', "--extract-partial stream",
                 '--stream "${STREAM_HOST_PATH:-}"', "$_av_marker_args", "$_switch_schedule_args",
                 '--cam2-run-id "$RUN_ID"', '--burn-stream-run-id "$BURN_STREAM_RUN_ID"',
                 '--out "$STREAM_LX_REMOTE_OUT_DIR/stream-partial-${RUN_ID}.json"',
                 '--local-out-dir "$OUTDIR"', "return"):
        assert need in lx, need
    assert "|| true" not in lx and "|| echo" not in lx, lx


def test_the_windows_stream_extract_text_is_unchanged():
    old_win = (
        '    "$HERE/recording-verdict-on-stream.sh" \\\n'
        '      --verdict-exe "$VERDICT_EXE_WIN" --out-dir "$OUT_DIR_WIN" --stream-rec "$STREAM_REC_WIN" \\\n'
        '      "${EXEC_STREAM_ARGS[@]}" \\\n'
        '      -- --extract-partial stream --stream "$STREAM_REC_WIN" --capture-fps "$STREAM_CAPTURE_FPS" \\\n'
    )
    assert _e2e().count(old_win) == 1


def _run_wait(tmp_path, stream_rc, have_stream_partial):
    s = _e2e()
    region = _cut(s, '    echo "    #703: waiting for the backgrounded strih+stream decode-in-place to finish..."\n',
                  '    echo "    #703: both partials present')
    out = tmp_path / "out"
    out.mkdir()
    (out / "strih-partial-7.json").write_text("{}")
    if have_stream_partial:
        (out / "stream-partial-7.json").write_text("{}")
    text = (
        "set -euo pipefail\n"
        "( exit 0 ) & STRIH_EXTRACT_PID=$!\n"
        "( exit %d ) & STREAM_EXTRACT_PID=$!\n" % stream_rc
        + 'STRIH_EXTRACT_LOG="%s/s.log" STREAM_EXTRACT_LOG="%s/t.log"\n' % (out, out)
        + 'STRIH_PARTIAL="%s/strih-partial-7.json" STREAM_PARTIAL="%s/stream-partial-7.json"\n' % (out, out)
        + region + 'echo "MERGE-REACHED"\n'
    )
    drv = tmp_path / "wait-driver.sh"
    drv.write_text(text)
    return subprocess.run(["bash", str(drv)], capture_output=True, text=True, timeout=60)


def test_a_failed_stream_leg_fails_the_run_by_name(tmp_path):
    p = _run_wait(tmp_path, 3, False)
    assert p.returncode == 1 and "MERGE-REACHED" not in p.stdout, p.stdout + p.stderr
    assert "stream extract rc=3" in p.stderr, p.stderr


def test_a_missing_stream_partial_fails_the_run_by_name(tmp_path):
    p = _run_wait(tmp_path, 0, False)
    assert p.returncode == 1 and "MERGE-REACHED" not in p.stdout, p.stdout + p.stderr
    assert "stream=%s" % (tmp_path / "out" / "stream-partial-7.json") in p.stderr, p.stderr


def test_the_merge_always_carries_the_stream_leg():
    s = _e2e()
    assert s.count('MERGE_ARGS=(--merge-partials "strih=$STRIH_PARTIAL" --merge-partials "stream=$STREAM_PARTIAL" \\') == 1


def test_the_8_8b_banner_names_the_transport_the_box_really_takes(tmp_path):
    s = _e2e()
    line = [ln for ln in s.splitlines() if "--- [8/8b] extract the STREAM partial" in ln]
    assert len(line) == 1 and 'stream_access_label "$STREAM"' in line[0], line
    rig = _rig(tmp_path)
    p = _lib(tmp_path, rig, 'echo "LX=$(stream_access_label 10.77.9.204)"')
    assert "LX=stream-snv, plain ssh/scp" in p.stdout, p.stdout + p.stderr
    p = _lib(tmp_path, rig, 'echo "WIN=$(stream_access_label 10.77.9.204)"', env={"OBS_FLEET": WIN_STREAM_FLEET})
    assert "WIN=win-stream-snv" in p.stdout, p.stdout + p.stderr


def test_the_linux_pull_back_note_replaces_the_mcp_download_text():
    s = _e2e()
    a = s.index("run_stream_extract\n  fi\n")
    region = s[a:s.index("cg_chain_onbox_extract_launch", a)]
    assert 'if [ "$(stream_box_platform "$STREAM")" = "linux" ]; then' in region, region
    assert 'stream_lx_partial_pullback_note "$STREAM_PARTIAL" "$STREAM_PIXELS"' in region, region
    assert region.index("stream_lx_partial_pullback_note") < region.index("win-stream-snv FileDownload")


# --- review round 1: the transport lives in stream-box.sh, the dir is checked, the decode RUNS ----

def test_the_upload_transport_lives_in_the_stream_box_lib(tmp_path):
    rig = _rig(tmp_path)
    src = tmp_path / "f.txt"
    src.write_text("payload")
    dest = rig["home"] / "f.txt"
    p = _lib(tmp_path, rig, 'stream_box_upload newlevel newlevel 10.77.9.204 "%s" "C:\\\\x\\\\f.txt" "%s"; echo RC=$?' % (src, dest))
    assert "RC=0" in p.stdout and dest.read_text() == "payload", p.stdout + p.stderr
    p = _lib(tmp_path, rig, 'win_ssh_upload() { echo "WIN_UPLOAD $*"; }\nstream_box_upload newlevel newlevel 10.77.9.204 "%s" "C:\\\\x\\\\f.txt" /nope' % src,
             env={"OBS_FLEET": WIN_STREAM_FLEET})
    assert "WIN_UPLOAD newlevel newlevel 10.77.9.204 %s C:\\x\\f.txt" % src in p.stdout, p.stdout + p.stderr
    lib = LIB.read_text()
    assert "sshpass" not in lib, "the push goes through stream-box.sh's transport, never its own ssh/scp"


def test_a_push_refuses_a_remote_dir_the_remote_shell_would_split(tmp_path):
    rig = _rig(tmp_path)
    csv = tmp_path / "m.csv"
    csv.write_text("x")
    for bad in ("/home/a b", "/home/it's", ""):
        p = _lib(tmp_path, rig, 'stream_lx_push_file newlevel newlevel 10.77.9.204 "%s" "%s" m.csv\necho AFTER' % (csv, bad))
        assert p.returncode != 0 and "AFTER" not in p.stdout, (bad, p.stdout)
    assert calls(rig) == []


def test_run_stream_extract_really_decodes_on_the_linux_stream_box(tmp_path):
    """The Linux branch cut out of recording-e2e.sh and RUN against the fake box: the recording path
    (with a space) and the A/V marker log reach the box decode as single arguments, the partial is
    back where the merge reads it."""
    rig = _rig(tmp_path)
    fn = _stream_extract_fn(_e2e())
    rec = str(rig["home"] / "2026-10-24 02-58-45.mkv")
    marker = str(rig["remote_out"] / "av-markers-7.csv")
    out = tmp_path / "outdir"
    out.mkdir()
    text = (
        "set -euo pipefail\n"
        '. "%s/lib/win-ssh-exec.sh"\n. "%s/lib/stream-box.sh"\n. "%s"\n' % (SCRIPTS, SCRIPTS, LIB)
        + 'HERE="%s" STREAM=10.77.9.204 STREAM_USER=newlevel STREAM_PW=newlevel\n' % SCRIPTS
        + 'VERDICT_BIN="%s" OUTDIR="%s" STREAM_HOST_PATH="%s" RUN_ID=7\n' % (rig["verdict"], out, rec)
        + 'STREAM_LX_REMOTE_OUT_DIR="%s" STREAM_LX_REMOTE_BIN="%s"\n' % (rig["remote_out"], rig["remote_bin"])
        + 'STREAM_CAPTURE_FPS=30 STRIH_CAPTURE_FPS=30 BURN_CAM1_RUN_ID=1 BURN_STRIH_RUN_ID=2 BURN_STREAM_RUN_ID=3\n'
        + '_av_marker_args="--av-marker-log %s" _switch_schedule_args="" CG_CHAIN_BURN_FLAG="" CG=""\n' % marker
        + fn + "\nrun_stream_extract\necho EXTRACT-DONE\n"
    )
    drv = tmp_path / "extract-driver.sh"
    drv.write_text(text)
    p = subprocess.run(["bash", str(drv)], capture_output=True, text=True, env=rig["env"], cwd=str(REPO), timeout=120)
    assert p.returncode == 0 and "EXTRACT-DONE" in p.stdout, p.stdout + p.stderr
    (argv,) = _verdict_calls(rig)
    assert argv[argv.index("--stream") + 1] == rec
    assert argv[argv.index("--av-marker-log") + 1] == marker
    assert argv[argv.index("--extract-partial") + 1] == "stream"
    assert (out / "stream-partial-7.json").is_file()
    assert rig["remote_bin"].is_file(), "the binary went to STREAM_LX_REMOTE_BIN"


# --- review round 1: missing-slot pixel proofs of the stream recording on the Linux box ----------

MSP = SCRIPTS / "lib" / "missing-slot-pixels.sh"


def _msp(tmp_path, rig, env=None):
    rec = rig["home"] / "stream rec.mkv"
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=64x48:rate=30", "-t", "1",
                    "-pix_fmt", "yuv420p", str(rec)], check=True, timeout=60)
    # the box runs the REAL ffmpeg/ffprobe (the stub ffmpeg of the fake rig is replaced)
    (rig["box"] / "ffmpeg").unlink()
    link_box_tools(rig, "ffmpeg", "ffprobe", "head", "rmdir")
    out = tmp_path / "msp-out"
    out.mkdir()
    report = out / "verdict-77.json"
    report.write_text('{"overall_pass": true, "full_chain": {"loss": {"stream": {"classified": '
                      '[{"id": 1, "kind": "burn_unreadable", "frame_index": 12, "png": null}]}}}}')
    text = ('set -euo pipefail\n. "%s"\n'
            "missing_slot_pixels_run '%s' '%s' 77 strih.test windows '' 10.77.9.204 '%s' 'C:\\o' || true\n"
            "echo MSP-DONE\n") % (MSP, report, out, rec)
    drv = tmp_path / "msp-driver.sh"
    drv.write_text(text)
    e = dict(rig["env"], STREAM_LX_REMOTE_OUT_DIR=str(rig["home"] / "verdict-out"))
    e.update(env or {})
    p = subprocess.run(["bash", str(drv)], capture_output=True, text=True, env=e, cwd=str(REPO), timeout=180)
    return p, out, report


def test_the_stream_recording_pixel_proofs_are_exported_on_the_linux_stream_box(tmp_path):
    rig = _rig(tmp_path)
    p, out, report = _msp(tmp_path, rig)
    assert p.returncode == 0 and "MSP-DONE" in p.stdout, p.stdout + p.stderr
    for k in (11, 12, 13):
        assert (out / "stream-missing" / ("frame-%d.png" % k)).is_file(), p.stdout + p.stderr
    assert '"stream": "ok"' in report.read_text()
    sshs = [c for c in calls(rig) if c["tool"] == "ssh"]
    assert sshs and not any(c["remote"].startswith("powershell") for c in sshs)
    assert all("verdict-out/missing-slot-pixels-stream-77" in c["remote"] for c in sshs), sshs


def test_a_windows_stream_box_keeps_the_powershell_pixel_export(tmp_path):
    rig = _rig(tmp_path)
    p, _out, _report = _msp(tmp_path, rig, env={"OBS_FLEET": WIN_STREAM_FLEET})
    assert p.returncode == 0, p.stdout + p.stderr
    sshs = [c for c in calls(rig) if c["tool"] == "ssh"]
    assert sshs and sshs[0]["remote"].startswith("powershell"), sshs
