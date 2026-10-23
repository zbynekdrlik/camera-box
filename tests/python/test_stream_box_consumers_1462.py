"""issue 1462 -- the Windows-only stream-box steps of rig-mode.sh and recording-e2e.sh, now chosen by
the box's fleet class (scripts/lib/stream-box.sh). Each test RUNS the real code: rig-mode.sh is
sourced (its #901 checks sit above the source guard), a recording-e2e.sh step is cut out between two
count-1 anchors and run under the caller's `set -euo pipefail`. Behind them a fake `sshpass` plays the
stream box (tests/python/stream_box_fakes_1462.py) and a PATH `python3` fakes only obs_phase2.py
(anything else runs the real interpreter), so nothing leaves this host. Linux = the default fleet
(stream-snv is linux-genlock); Windows = an OBS_FLEET override, which must keep the PowerShell text.
Tier-0: no rig, no network, no cargo.
"""
import json
import os
import pathlib
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stream_box_fakes_1462 import calls, ffmpeg_calls, make_rig  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
RIG = SCRIPTS / "rig-mode.sh"
E2E = SCRIPTS / "recording-e2e.sh"
WIN_STREAM_FLEET = "strih-snv|10.77.9.202|linux-genlock|always\nstream-snv|10.77.9.204|windows-genlock|always"
REC_LX = "/home/newlevel/2026-10-23 01-23-45.mkv"
REC_WIN = "C:\\Users\\newlevel\\Videos\\2026-10-23 01-23-45.mkv"

# PATH python3: fakes obs_phase2.py (record start / stop / status, mbc-input-check) and logs its
# argv; anything else is the real interpreter.
_PY = r'''
import json, os, sys
a = sys.argv[1:]
if a and a[0].endswith("obs_phase2.py"):
    with open(os.environ["FAKE_PY_LOG"], "a") as f:
        f.write(json.dumps(a[1:]) + "\n")
    if "record" in a and "stop" in a:
        print(os.environ.get("FAKE_REC_PATH", ""))
    elif "record" in a and "status" in a:
        sys.exit(int(os.environ.get("FAKE_WS_RC", "0")))
    elif "mbc-input-check" in a:
        print("PASS: fake mbc check")
        sys.exit(int(os.environ.get("FAKE_MBC_RC", "0")))
    sys.exit(0)
os.execv(sys.executable, [sys.executable] + a)
'''

# the [4b3/8] probe binary: logs its argv + the WAV it was handed, prints an OK verdict line
_PROBE = r'''
import json, os, sys
a = sys.argv[1:]
wav = a[a.index("--qpsk-probe") + 1]
with open(os.environ["FAKE_PROBE_LOG"], "a") as f:
    f.write(json.dumps({"argv": a, "wav": open(wav, "rb").read().decode()}) + "\n")
print('{"preamble_screens":188,"candidates":190,"cluster_samples":7,"crc_ok":107,"crc_fail":83,"peak_dbfs":-19.2,"verdict":"OK"}')
'''


def _rig(tmp_path):
    rig = make_rig(tmp_path)
    py = rig["dev1"] / "python3"
    py.write_text("#!%s\n%s" % (sys.executable, _PY))
    py.chmod(0o755)
    rig["py_log"] = tmp_path / "py.log"
    rig["env"]["FAKE_PY_LOG"] = str(rig["py_log"])
    return rig


def _py_calls(rig):
    p = rig["py_log"]
    return [json.loads(x) for x in p.read_text().splitlines()] if p.exists() else []


def _drive(tmp_path, rig, text, env=None):
    e = dict(rig["env"])
    e.update(env or {})
    driver = tmp_path / "driver.sh"
    driver.write_text(text)
    return subprocess.run(["bash", str(driver)], capture_output=True, text=True, env=e, cwd=str(REPO), timeout=120)


# --- rig-mode.sh #901 item 2: the mbc transport -----------------------------------------------------

def _rig_mode(tmp_path, rig, body, env=None):
    return _drive(tmp_path, rig, 'set -euo pipefail\n. "%s"\nsleep() { :; }\n%s\n' % (RIG, body), env)


def test_rig_mode_grades_the_linux_stream_box_mbc_as_the_vban_source(tmp_path):
    rig = _rig(tmp_path)
    p = _rig_mode(tmp_path, rig, "verify_mbc_dante_transport; echo AFTER")
    assert p.returncode == 0, p.stderr
    assert "AFTER" in p.stdout
    (argv,) = [c for c in _py_calls(rig) if c[:1] == ["mbc-input-check"]]
    assert argv[argv.index("--host") + 1] == "10.77.9.204" and argv[argv.index("--input") + 1] == "mbc"
    assert argv[argv.index("--vban-stream") + 1] == "stream" and argv[argv.index("--vban-port") + 1] == "6980"


def test_rig_mode_keeps_the_dante_check_on_a_windows_stream_box(tmp_path):
    rig = _rig(tmp_path)
    p = _rig_mode(tmp_path, rig, "verify_mbc_dante_transport; echo AFTER", env={"OBS_FLEET": WIN_STREAM_FLEET})
    assert p.returncode == 0, p.stderr
    (argv,) = [c for c in _py_calls(rig) if c[:1] == ["mbc-input-check"]]
    assert "--vban-stream" not in argv and "--vban-port" not in argv
    assert "mbc Dante-transport check (device bound + unmuted)" in p.stdout


def test_rig_mode_a_failed_mbc_check_still_ends_test(tmp_path):
    rig = _rig(tmp_path)
    p = _rig_mode(tmp_path, rig, "verify_mbc_dante_transport; echo AFTER", env={"FAKE_MBC_RC": "1"})
    assert p.returncode != 0 and "AFTER" not in p.stdout


def test_rig_mode_refuses_an_input_name_the_box_facts_do_not_declare(tmp_path):
    rig = _rig(tmp_path)
    p = _rig_mode(tmp_path, rig, "verify_mbc_dante_transport; echo AFTER", env={"MBC_INPUT_NAME": "Zvuk z mixu"})
    assert p.returncode != 0 and "AFTER" not in p.stdout
    assert "'Zvuk z mixu'" in p.stderr and "'mbc'" in p.stderr and "STRIH_PROGRAM_AUDIO_INPUT" in p.stderr
    assert not [c for c in _py_calls(rig) if c[:1] == ["mbc-input-check"]]


def test_rig_mode_takes_an_input_name_with_spaces_from_the_facts(tmp_path):
    # stream-pp's program-audio input is `Zvuk z mixu`: the name comes last in the expectation and
    # reaches the check whole
    rig = _rig(tmp_path)
    env = {"OBS_FLEET": "stream-pp|10.77.8.204|linux-genlock|always", "STREAM_IP": "10.77.8.204",
           "MBC_INPUT_NAME": "Zvuk z mixu"}
    p = _rig_mode(tmp_path, rig, "verify_mbc_dante_transport; echo AFTER", env=env)
    assert p.returncode == 0, p.stderr
    (argv,) = [c for c in _py_calls(rig) if c[:1] == ["mbc-input-check"]]
    assert argv[argv.index("--input") + 1] == "Zvuk z mixu"
    assert argv[argv.index("--vban-stream") + 1] == "stream" and argv[argv.index("--vban-port") + 1] == "6980"


def test_rig_mode_refuses_a_linux_stream_box_without_facts_before_any_read(tmp_path):
    rig = _rig(tmp_path)
    p = _rig_mode(tmp_path, rig, "verify_mbc_dante_transport; echo AFTER",
                  env={"OBS_FLEET": "stream-zz|192.0.2.10|linux-genlock|always", "STREAM_IP": "192.0.2.10"})
    assert p.returncode != 0 and "AFTER" not in p.stdout
    assert "stream-zz" in p.stderr
    assert not [c for c in _py_calls(rig) if c[:1] == ["mbc-input-check"]]


# --- rig-mode.sh #901 item 1: the measurement-audio arrival -----------------------------------------

_AUDIO_ENV = {"AUDIO_CHAIN_PROBE_SECS": "0", "AUDIO_CHAIN_PARSE_RETRIES": "1", "STREAM_PW": "s3cret"}


def test_rig_mode_reads_the_level_on_the_linux_stream_box_with_bash(tmp_path):
    rig = _rig(tmp_path)
    p = _rig_mode(tmp_path, rig, "verify_measurement_audio_arrives; echo AFTER",
                  env=dict(_AUDIO_ENV, FAKE_REC_PATH=REC_LX, FAKE_MAX_DB="-12.3"))
    assert p.returncode == 0, p.stderr
    assert "AUDIBLE (max_volume -12.3 dB" in p.stdout and "AFTER" in p.stdout
    (ff,) = ffmpeg_calls(rig)
    assert ff[ff.index("-i") + 1] == REC_LX
    c = calls(rig)
    assert all("ps" not in x for x in c), "no PowerShell to a Linux box"
    assert [x["target"] for x in c] == ["newlevel@10.77.9.204"] * 2
    assert c[1]["remote"] == "rm -f -- '%s'" % REC_LX


def test_rig_mode_a_dead_chain_on_the_linux_box_still_fails_test(tmp_path):
    rig = _rig(tmp_path)
    p = _rig_mode(tmp_path, rig, "verify_measurement_audio_arrives; echo AFTER",
                  env=dict(_AUDIO_ENV, FAKE_REC_PATH=REC_LX, FAKE_MAX_DB="-91.0"))
    assert p.returncode != 0 and "AFTER" not in p.stdout
    assert "-91.0" in p.stderr


def test_rig_mode_keeps_the_powershell_text_on_a_windows_stream_box(tmp_path):
    rig = _rig(tmp_path)
    p = _rig_mode(tmp_path, rig, "verify_measurement_audio_arrives; echo AFTER",
                  env=dict(_AUDIO_ENV, OBS_FLEET=WIN_STREAM_FLEET, FAKE_REC_PATH=REC_WIN,
                           FAKE_WIN_REPLY="[Parsed_volumedetect_0 @ 0x1] max_volume: -12.3 dB\r\n"))
    assert p.returncode == 0, p.stderr
    assert "AUDIBLE (max_volume -12.3 dB" in p.stdout
    ps = [x["ps"] for x in calls(rig)]
    assert ps == ['ffmpeg -hide_banner -nostats -i "%s" -af volumedetect -f null NUL 2>&1' % REC_WIN,
                  'Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath "%s"' % REC_WIN]
    assert ffmpeg_calls(rig) == []


# --- recording-e2e.sh steps, cut out and run --------------------------------------------------------

_E2E_HEAD = """set -euo pipefail
HERE="{scripts}"
. "$HERE/lib/win-ssh-exec.sh"
. "$HERE/lib/obs-session-visibility.sh"
. "$HERE/lib/strih-platform.sh"
if [ -f "$HERE/lib/stream-box.sh" ]; then . "$HERE/lib/stream-box.sh"; fi  # (absent before the port: the old step runs)
. "$HERE/lib/audio-presence-preflight.sh"
. "$HERE/lib/marker-decodability-preflight.sh"
STRIH=10.77.9.202
STREAM=10.77.9.204
STRIH_USER=newlevel
STRIH_PW=newlevel
STREAM_USER=newlevel
STREAM_PW=s3cret
SVG_SSH_TIMEOUT=20
sleep() {{ :; }}
"""


def _region(start, end):
    s = E2E.read_text()
    assert s.count(start) == 1 and s.count(end) == 1, (start, end)
    i = s.index(start)
    return s[i:s.index(end, i)]


def _e2e(tmp_path, rig, region, env=None, extra=""):
    text = _E2E_HEAD.format(scripts=SCRIPTS) + extra + region + "\necho REGION-END\n"
    return _drive(tmp_path, rig, text, env)


def test_e2e_audio_presence_reads_the_linux_stream_box_with_bash(tmp_path):
    rig = _rig(tmp_path)
    region = _region('echo "[4b2/8]', 'echo "[4b3/8]')
    p = _e2e(tmp_path, rig, region, env={"FAKE_REC_PATH": REC_LX, "FAKE_MAX_DB": "-12.3"})
    assert p.returncode == 0, p.stderr
    assert "ok: mbc measurement audio AUDIBLE (max_volume -12.3 dB" in p.stdout and "REGION-END" in p.stdout
    (ff,) = ffmpeg_calls(rig)
    assert ff[ff.index("-i") + 1] == REC_LX
    c = calls(rig)
    assert all("ps" not in x for x in c)
    assert c[-1]["remote"] == "rm -f -- '%s'" % REC_LX


def test_e2e_audio_presence_still_aborts_on_a_silent_linux_chain(tmp_path):
    rig = _rig(tmp_path)
    region = _region('echo "[4b2/8]', 'echo "[4b3/8]')
    p = _e2e(tmp_path, rig, region, env={"FAKE_REC_PATH": REC_LX, "FAKE_MAX_DB": "-91.0"})
    assert p.returncode == 1 and "REGION-END" not in p.stdout
    assert "-91.0" in p.stderr


def test_e2e_audio_presence_keeps_the_powershell_text_on_windows(tmp_path):
    rig = _rig(tmp_path)
    region = _region('echo "[4b2/8]', 'echo "[4b3/8]')
    p = _e2e(tmp_path, rig, region, env={"OBS_FLEET": WIN_STREAM_FLEET, "FAKE_REC_PATH": REC_WIN,
                                         "FAKE_WIN_REPLY": "max_volume: -12.3 dB\r\n"})
    assert p.returncode == 0, p.stderr
    assert [x["ps"] for x in calls(rig)] == [
        'ffmpeg -hide_banner -nostats -i "%s" -af volumedetect -f null NUL 2>&1' % REC_WIN,
        'Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath "%s"' % REC_WIN]


def _probe(tmp_path, rig):
    pdir = tmp_path / "probe"
    pdir.mkdir()
    exe = pdir / "recording-verdict"
    exe.write_text("#!%s\n%s" % (sys.executable, _PROBE))
    exe.chmod(0o755)
    log = tmp_path / "probe.log"
    return pdir, log


def test_e2e_marker_decodability_extracts_and_pulls_the_wav_from_the_linux_box(tmp_path):
    rig = _rig(tmp_path)
    pdir, plog = _probe(tmp_path, rig)
    box_wav = rig["home"] / "camera-box-mbc-decodability.wav"  # the default: the ssh user's home, never /tmp
    out = tmp_path / "out"
    region = _region('echo "[4b3/8]', 'echo "[4c/8]')
    p = _e2e(tmp_path, rig, region, extra='PROBE_BIN_DIR="%s"\nOUTDIR="%s"\nRUN_ID=4242\n' % (pdir, out),
             env={"FAKE_REC_PATH": REC_LX, "FAKE_PROBE_LOG": str(plog)})
    assert p.returncode == 0, p.stderr
    assert "ok: mbc QPSK marker DECODABLE" in p.stdout and "REGION-END" in p.stdout
    (ff,) = ffmpeg_calls(rig)
    assert ff[ff.index("-i") + 1] == REC_LX and ff[-1] == "camera-box-mbc-decodability.wav"
    c = calls(rig)
    assert c[0]["remote"].startswith("rm -f -- 'camera-box-mbc-decodability.wav'; ffmpeg ")
    assert c[1]["tool"] == "scp" and c[1]["src"] == "newlevel@10.77.9.204:camera-box-mbc-decodability.wav"
    (probe,) = [json.loads(x) for x in plog.read_text().splitlines()]
    assert probe["wav"] == "RIFF-fake-wav"  # the pulled-back bytes reached the probe
    assert probe["argv"][probe["argv"].index("--qpsk-probe") + 1] == str(out / "mbc-decodability-4242.wav")
    assert not box_wav.exists(), "the throwaway WAV on the box is removed"
    assert all("ps" not in x for x in c)


def test_e2e_marker_decodability_never_pulls_back_a_stale_wav_after_a_failed_extract(tmp_path):
    # an aborted earlier run left its WAV on the box; this run's extract fails on every try: the stale
    # bytes must never reach the probe (a false DECODABLE), the step aborts on "never arrived"
    rig = _rig(tmp_path)
    pdir, plog = _probe(tmp_path, rig)
    stale = rig["home"] / "mbc decodability.wav"
    stale.write_bytes(b"RIFF-stale-wav-of-an-older-run")
    region = _region('echo "[4b3/8]', 'echo "[4c/8]')
    p = _e2e(tmp_path, rig, region, extra='PROBE_BIN_DIR="%s"\nOUTDIR="%s"\nRUN_ID=4242\n' % (pdir, tmp_path / "out"),
             env={"FAKE_REC_PATH": REC_LX, "FAKE_PROBE_LOG": str(plog), "FAKE_FFMPEG_RC": "1",
                  "AUDIO_DECODABILITY_READ_ATTEMPTS": "2", "AUDIO_DECODABILITY_WAV_LX": str(stale)})
    assert p.returncode == 1 and "never arrived" in p.stderr and "REGION-END" not in p.stdout
    assert not plog.exists(), "the probe never ran on the stale WAV"
    assert len(ffmpeg_calls(rig)) == 2


def test_e2e_marker_decodability_keeps_the_windows_path(tmp_path):
    rig = _rig(tmp_path)
    pdir, plog = _probe(tmp_path, rig)
    region = _region('echo "[4b3/8]', 'echo "[4c/8]')
    p = _e2e(tmp_path, rig, region, extra='PROBE_BIN_DIR="%s"\nOUTDIR="%s"\nRUN_ID=4242\n' % (pdir, tmp_path / "out"),
             env={"OBS_FLEET": WIN_STREAM_FLEET, "FAKE_REC_PATH": REC_WIN, "FAKE_PROBE_LOG": str(plog)})
    # the fake Windows box makes no WAV, so the step aborts on "never arrived" -- after the
    # unchanged PowerShell extract and the Windows scp source
    assert p.returncode == 1 and "never arrived" in p.stderr
    c = calls(rig)
    assert c[0]["ps"] == ('ffmpeg -hide_banner -nostats -y -i "%s" -map 0:a:0 -ar 48000 -c:a pcm_f32le '
                          '"C:\\camera-box\\mbc-decodability.wav" 2>&1' % REC_WIN)
    assert c[1]["tool"] == "scp" and c[1]["src"] == "newlevel@10.77.9.204:C:/camera-box/mbc-decodability.wav"


# --- recording-e2e.sh [0/8]: the stream session-visibility gate -------------------------------------

_SVG = ('echo "    ok: strih obs64/AHK visible', "# #1295: report-only")


def _svg_region():
    r = _region(*_SVG)
    return r[r.index("\n") + 1:]  # from the line after the strih ok line


def test_e2e_session_visibility_checks_the_linux_stream_unit_not_obs64(tmp_path):
    rig = _rig(tmp_path)
    p = _e2e(tmp_path, rig, _svg_region())
    assert p.returncode == 0, p.stderr
    assert "REGION-END" in p.stdout
    assert "ok: stream strih-obs.service active" in p.stdout and "obs64" not in p.stdout
    c = calls(rig)
    assert len(c) == 1 and "ps" not in c[0] and "strih-obs.service" in c[0]["remote"]
    assert c[0]["target"] == "newlevel@10.77.9.204"
    assert [x for x in _py_calls(rig) if "status" in x]


def test_e2e_session_visibility_names_the_linux_recovery_on_a_dead_stream_unit(tmp_path):
    rig = _rig(tmp_path)
    p = _e2e(tmp_path, rig, _svg_region(), env={"FAKE_SVC_STATE": "inactive"})
    assert p.returncode == 1 and "REGION-END" not in p.stdout
    assert "stream INVISIBLE" in p.stderr
    assert "systemctl --user restart strih-obs.service" in p.stderr and "launch-obs-genlock" not in p.stderr


def test_e2e_session_visibility_names_the_stream_box_when_it_does_not_answer(tmp_path):
    rig = _rig(tmp_path)
    p = _e2e(tmp_path, rig, _svg_region(), env={"FAKE_SSH_DOWN": "1"})
    assert p.returncode == 1 and "REGION-END" not in p.stdout
    assert "stream INVISIBLE" in p.stderr and "the stream box stream-snv unreachable" in p.stderr
    assert "strih-snv" not in p.stderr, "the stream box is never named as the strih"


def test_e2e_session_visibility_keeps_the_obs64_probe_on_windows(tmp_path):
    rig = _rig(tmp_path)
    reply = "ACTIVE_SESSION=1\r\nOWN_SESSION=0\r\nOBS_COUNT=1\r\nOBS_SESSION=1\r\nOBS_TITLE=\r\n"
    p = _e2e(tmp_path, rig, _svg_region(), env={"OBS_FLEET": WIN_STREAM_FLEET, "FAKE_WIN_REPLY": reply})
    assert p.returncode == 0, p.stderr
    (c,) = calls(rig)
    assert "Get-Process" in c["ps"]


# --- recording-e2e.sh [0/8]: the dantesync version-parity routing -----------------------------------

def _dv(tmp_path, rig, env=None):
    region = _region('DV_WIN_NODES="strih=', '${STRIH_LX_GATE_PREFIX:-} "$HERE/dantesync-version-gate.sh"')
    extra = 'DANTESYNC_VERSION_LINUX="cam5=root@10.77.9.65"\n'
    tail = 'printf "LINUX=[%s]\\nWIN=[%s]\\n" "$DANTESYNC_VERSION_LINUX" "$DV_WIN_NODES"\n'
    return _e2e(tmp_path, rig, region + tail, env=env, extra=extra)


def test_e2e_dantesync_version_reads_a_linux_stream_box_via_the_linux_arm(tmp_path):
    p = _dv(tmp_path, _rig(tmp_path))
    assert p.returncode == 0, p.stderr
    lines = dict(x.split("=", 1) for x in p.stdout.splitlines() if x.startswith(("LINUX=", "WIN=")))
    assert lines["LINUX"].strip("[]").split() == ["cam5=root@10.77.9.65", "strih=newlevel@10.77.9.202",
                                                  "stream=newlevel@10.77.9.204"]
    assert lines["WIN"].strip("[]").split() == [], "never one node in both arms"


def test_e2e_dantesync_version_keeps_a_windows_stream_box_under_win(tmp_path):
    p = _dv(tmp_path, _rig(tmp_path), env={"OBS_FLEET": WIN_STREAM_FLEET})
    assert p.returncode == 0, p.stderr
    lines = dict(x.split("=", 1) for x in p.stdout.splitlines() if x.startswith(("LINUX=", "WIN=")))
    assert "stream=" not in lines["LINUX"]
    assert lines["WIN"].strip("[]").split() == ["stream=newlevel@10.77.9.204"]


# --- the bundle-state self-heal class ---------------------------------------------------------------

def _selfheal_class(tmp_path, host, strih, env=None):
    tmp_path = tmp_path / ("run-%d" % len(list(tmp_path.glob("run-*"))))
    tmp_path.mkdir()
    rig = _rig(tmp_path)
    p = _drive(tmp_path, rig, 'set -euo pipefail\n. "%s"\nbundle_state_selfheal_class "%s" "%s"\n'
               % (SCRIPTS / "lib" / "bundle-state-selfheal.sh", host, strih), env)
    assert p.returncode == 0, p.stderr
    return p.stdout.strip()


def test_selfheal_restarts_the_linux_stream_box_as_linux(tmp_path):
    assert _selfheal_class(tmp_path, "10.77.9.204", "10.77.9.202") == "linux-genlock"


def test_selfheal_keeps_a_windows_stream_box_on_schtasks_even_with_a_forced_linux_strih(tmp_path):
    env = {"OBS_FLEET": WIN_STREAM_FLEET, "STRIH_PLATFORM": "linux"}
    assert _selfheal_class(tmp_path, "10.77.9.204", "10.77.9.202", env) == "windows-genlock"
    assert _selfheal_class(tmp_path, "192.0.2.9", "10.77.9.202") == "windows-genlock"


_CURL_DOWN = "#!/bin/sh\nexit 7\n"  # dev1's curl: :8899 never answers


def _ssh_opts(argv):
    return [argv[i + 1] for i, a in enumerate(argv[:-1]) if a == "-o"]


def test_selfheal_restart_ssh_ignores_the_stale_known_hosts_key(tmp_path):
    # the stream box's address held the Windows PC's host key until 22.10.2026; with
    # StrictHostKeyChecking=no a stale key disables password login, so the restart must not use it
    rig = _rig(tmp_path)
    (rig["dev1"] / "curl").write_text(_CURL_DOWN)
    (rig["dev1"] / "curl").chmod(0o755)
    body = ('set -euo pipefail\n. "%s"\nbundle_state_selfheal_fetch 10.77.9.204 "%s" 8899 newlevel pw linux-genlock'
            ' || echo "RC=$?"\n' % (SCRIPTS / "lib" / "bundle-state-selfheal.sh", tmp_path / "state.json"))
    p = _drive(tmp_path, rig, body, env={"BUNDLE_STATE_SELFHEAL_TRIES": "1", "BUNDLE_STATE_SELFHEAL_SLEEP_S": "0"})
    assert "RC=1" in p.stdout, p.stderr
    (c,) = calls(rig)
    assert "UserKnownHostsFile=/dev/null" in _ssh_opts(c["argv"]) and "StrictHostKeyChecking=no" in _ssh_opts(c["argv"])
    assert "systemctl --user restart" in c["remote"]


def test_watchdog_restart_ssh_ignores_the_stale_known_hosts_key(tmp_path):
    rig = _rig(tmp_path)
    body = ('set --\n. "%s"\nset +e\nrestart_bundle_state_task stream-snv 10.77.9.204\necho "RC=$?"\n'
            % (SCRIPTS / "bundle-state-alert-watchdog.sh"))
    p = _drive(tmp_path, rig, body)
    assert "RC=" in p.stdout, p.stderr
    (c,) = calls(rig)
    assert c["target"] == "newlevel@10.77.9.204"
    assert "UserKnownHostsFile=/dev/null" in _ssh_opts(c["argv"])
    assert "systemctl --user restart" in c["remote"], "stream-snv is a linux-genlock box"


# --- the two scripts source the lib -----------------------------------------------------------------

def test_both_scripts_source_the_stream_box_lib():
    assert RIG.read_text().count('. "$RIG_MODE_DIR/lib/stream-box.sh"') == 1
    assert E2E.read_text().count('. "$HERE/lib/stream-box.sh"') == 1
