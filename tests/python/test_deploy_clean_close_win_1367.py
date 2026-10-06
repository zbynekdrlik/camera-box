"""issue 1367 -- the Windows genlock deploy closes OBS CLEANLY so runtime A/V settings persist.

Live cause (6.10.2026, comment 6013239473): the E2E gate's A/V correction and the latency pins are
written over obs-websocket and live in OBS runtime until OBS next saves its scene collection. The
deploy program (scripts/deploy-genlock-fleet.sh, build_windows_deploy_program) stopped OBS with
`Stop-Process -Force`, which skips the save on exit, so each deploy reloaded older saved values
(runtime `mbc` sync 37 ms vs saved 29 ms). Design: comment 6014590298 (Approach 1).

This suite pins the emitted program for both Windows boxes (stream, resolume), FULL and FAST:

  * step (0a): the obs-websocket stream/record read and the live-broadcast refusal run BEFORE any
    change on the box (before the power plan, the AutoHotkey64 stop and the keep-alive disable);
  * step (2): a re-read, StopRecord when recording, CloseMainWindow with a 45 s bound and the
    `clean close OK in N ms` line; the old `Stop-Process -Force` only as the named fallback;
  * step (2b): the report-only read-back of the saved pins and audio sync, after the stop;
  * ONE shared fragment (scripts/lib/obs-clean-close-win.sh), byte-identical for both boxes;
  * the whole program parses as PowerShell, and the fragment RUNS in pwsh against a fake
    obs-websocket (tests/python/obs_ws_fake_1367.py) and a fake obs64 process.

pwsh: ubuntu-latest ships it; dev1 has a portable one at ~/.local/pwsh74/pwsh. A missing pwsh
FAILS, never skips. pwsh 7 on Linux is not Windows PowerShell 5.1 and has no window to close, so
CloseMainWindow takes its "no main window -- forcing" branch here; the clean-exit and timeout
branches are pinned as text and were proven live (6.10.2026, a clean close exited in 6.6 s).
"""
import json
import os
import pathlib
import shutil
import subprocess
import sys

import pytest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from obs_ws_fake_1367 import FakeObsWs  # noqa: E402

REPO = HERE.parents[1]
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "obs-clean-close-win.sh"
BOXES = ["stream", "resolume"]
MODES = ["full", "fast"]
WS_URI = "ws://127.0.0.1:4455"
FORCE_LINE = ("Get-Process obs64,obs-browser-page -ErrorAction SilentlyContinue | "
              "Stop-Process -Force -ErrorAction SilentlyContinue")
BLOCKS = ["obs_clean_close_preflight_ps", "obs_clean_close_stop_ps", "obs_saved_settings_readback_ps"]


def _bash(script):
    # sourced under the caller's strict mode, as deploy-genlock-fleet.sh runs it
    return subprocess.run(["bash", "-c", "set -euo pipefail\n" + script], capture_output=True,
                          text=True, timeout=60)


def _program(box, mode="full"):
    r = _bash(
        f'. "{SCRIPTS}/deploy-genlock-fleet.sh"; '
        f'build_windows_deploy_program {box} {mode} "C:\\stage" "C:\\Program Files\\obs-studio" '
        f'"$(fleet_box_ahk_mode {box})" "C:\\obs-backup" 3 abc123 def456'
    )
    assert r.returncode == 0, r.stderr
    return r.stdout


def _block(fn):
    r = _bash(f'. "{LIB}"; {fn}')
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip(), f"{fn} printed nothing"
    return r.stdout


def _pwsh():
    pwsh = os.environ.get("PWSH") or shutil.which("pwsh")
    home_pwsh = os.path.expanduser("~/.local/pwsh74/pwsh")
    if not pwsh and os.access(home_pwsh, os.X_OK):
        pwsh = home_pwsh
    if not pwsh:
        pytest.fail("no pwsh: install PowerShell 7 or set PWSH=/path/to/pwsh (the program must parse and run)")
    return pwsh


# --- the emitted deploy program ------------------------------------------------------------------

@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize("box", BOXES)
def test_live_stream_is_refused_before_anything_changes(box, mode):
    p = _program(box, mode)
    pre = p.index("# (0a) issue 1367")
    assert p.index("# (0) preflight") < pre
    # before the power plan (0b), the AutoHotkey64 step (1) and the keep-alive step (1b)
    for later in ("# (0b)", "# (1) ", "# (1b)", FORCE_LINE):
        assert pre < p.index(later), later
    seg = p[pre:p.index("# (0b)")]
    assert f"$ccObsWsUri = '{WS_URI}'" in seg
    assert "'GetStreamStatus'" in seg and "'GetRecordStatus'" in seg
    refusal = seg.index("clean close REFUSED: obs64 is STREAMING")
    assert seg.index("exit 12", refusal) > refusal
    assert "Nothing on this box was changed" in seg
    # an unreadable :4455 is named and fails open, like the rig-busy guard
    assert "unreadable" in seg and "Write-Warning" in seg


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize("box", BOXES)
def test_clean_close_first_bounded_45s_force_only_as_named_fallback(box, mode):
    p = _program(box, mode)
    close = p.index("# (2) issue 1367")
    assert p.index("# (1b)") < close and p.index("# (1) ") < close
    seg = p[close:p.index("# (3) ")]
    assert "$ccCloseTimeoutMs = 45000" in seg
    # re-read right before the close; a stream that started since (0a) is refused too
    assert seg.index("Get-CcObsOutputState") < seg.index(".CloseMainWindow()")
    assert "STARTED STREAMING" in seg and "exit 12" in seg
    # StopRecord, then a bounded confirm of the record state, both before the close
    stop = seg.index("'StopRecord'")
    assert stop < seg.index("'GetRecordStatus'", stop) < seg.index(".CloseMainWindow()")
    assert "recording stopped in" in seg
    # the close on the ONE obs64 of this session, waited on up to the bound
    assert "$_.SessionId -eq $ccSession" in seg
    wait = seg.index("$ccSw.ElapsedMilliseconds -lt $ccCloseTimeoutMs")
    assert seg.index(".CloseMainWindow()") < wait
    ok = seg.index("clean close OK in $($ccSw.ElapsedMilliseconds) ms")
    timed_out = seg.index("clean close timed out -- forcing")
    assert wait < timed_out < ok
    # the old force-kill: exactly once in the whole program, inside `if ($ccForce)`, after the
    # named fallback lines
    assert p.count(FORCE_LINE) == 1
    branch = seg.index("if ($ccForce) {")
    assert timed_out < branch < seg.index(FORCE_LINE) < seg.index("} else {", branch)
    assert seg.count("$ccForce = $true") == 3  # wrong process count, no main window, timeout
    # the crash-sentinel clear and its settle stay after the stop
    assert seg.index(FORCE_LINE) < seg.index("Start-Sleep -Seconds 5") < seg.index(".sentinel")


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize("box", BOXES)
def test_saved_settings_read_back_after_the_stop_report_only(box, mode):
    p = _program(box, mode)
    rb = p.index("# (2b) issue 1367")
    assert p.index(FORCE_LINE) < p.index(".sentinel") < rb < p.index("# (3) ")
    seg = p[rb:p.index("# (3) ")]
    assert "SceneCollectionFile=" in seg and "'user.ini'" in seg and "'global.ini'" in seg
    assert "genlock_latency_ms_src" in seg and "ndi_source" in seg
    assert "$ccS.mixers" in seg and "$ccS.sync" in seg
    # report-only: no exit, and every failure ends in the named UNREAD line
    assert "exit " not in seg
    assert "saved-settings read-back UNREAD" in seg


@pytest.mark.parametrize("mode", MODES)
def test_one_shared_fragment_for_both_windows_boxes(mode):
    progs = {box: _program(box, mode) for box in BOXES}
    for fn in BLOCKS:
        block = _block(fn)
        for box, p in progs.items():
            assert p.count(block) == 1, (fn, box)


def test_the_fragment_carries_no_box_specific_or_secret_text():
    text = "".join(_block(fn) for fn in BLOCKS)
    # no box name, no per-box block's variable, no password literal, no Write-Error (it throws
    # under the program's $ErrorActionPreference = 'Stop', so a following exit would never run)
    for word in ("resolume", "STREAM-SNV", "AutoHotkey64", "avsync-keepalive", "$disabledKeepAlive",
                 "server_password =", "Write-Error", "powercfg"):
        assert word not in text, word
    # the password is read from the box's own obs-websocket config, only when the server asks
    assert "plugin_config\\obs-websocket\\config.json" in text
    assert text.index("if ($hello.d.authentication)") < text.index(".server_password")


def test_the_whole_program_parses_as_powershell(tmp_path):
    pwsh = _pwsh()
    files = []
    for box in BOXES:
        for mode in MODES:
            f = tmp_path / f"{box}-{mode}.ps1"
            f.write_text(_program(box, mode))
            files.append(str(f))
    parser = tmp_path / "parse.ps1"
    parser.write_text(
        "$bad = 0\n"
        "foreach ($f in $args) {\n"
        "  $e = $null\n"
        "  [void][System.Management.Automation.Language.Parser]::ParseFile($f, [ref]$null, [ref]$e)\n"
        "  foreach ($x in $e) { Write-Output (\"$f :: line $($x.Extent.StartLineNumber): $($x.Message)\"); $bad++ }\n"
        "}\n"
        "Write-Output \"parsed $($args.Count) programs, $bad error(s)\"\n"
        "exit $bad\n")
    r = subprocess.run([pwsh, "-NoProfile", "-NonInteractive", "-File", str(parser), *files],
                       capture_output=True, text=True, timeout=120)
    assert r.returncode == 0, r.stdout + r.stderr
    assert f"parsed {len(files)} programs, 0 error(s)" in r.stdout


# --- the fragment RUN in pwsh against a fake obs-websocket and a fake obs64 ----------------------

class Rig:
    """A fake obs64 (a copy of `sleep` under that name) + APPDATA, and the fragment run in pwsh."""

    def __init__(self, tmp_path, obs_running=True):
        self.tmp = tmp_path
        self.appdata = tmp_path / "appdata"
        (self.appdata / "obs-studio" / "basic" / "scenes").mkdir(parents=True)
        self.obs = None
        if obs_running:
            exe = tmp_path / "bin" / "obs64"
            exe.parent.mkdir()
            shutil.copy(shutil.which("sleep"), exe)
            self.obs = subprocess.Popen([str(exe), "600"])

    def write(self, rel, text):
        path = self.appdata / "obs-studio" / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def run(self, uri, blocks=BLOCKS):
        body = "\n".join(_block(fn) for fn in blocks).replace(f"'{WS_URI}'", f"'{uri}'")
        prog = self.tmp / "program.ps1"
        prog.write_text("$ErrorActionPreference = 'Stop'\n" + body + "\nWrite-Host 'HARNESS DONE'\nexit 0\n")
        env = dict(os.environ, APPDATA=str(self.appdata))
        return subprocess.run([_pwsh(), "-NoProfile", "-NonInteractive", "-File", str(prog)],
                              capture_output=True, text=True, timeout=120, env=env)

    def obs_alive(self):
        return self.obs is not None and self.obs.poll() is None

    def close(self):
        if self.obs_alive():
            self.obs.kill()
        if self.obs is not None:
            self.obs.wait(timeout=10)


@pytest.fixture
def rig(tmp_path):
    made = []

    def make(obs_running=True):
        r = Rig(tmp_path, obs_running)
        made.append(r)
        return r

    yield make
    for r in made:
        r.close()


@pytest.fixture
def ws():
    made = []

    def make(**kw):
        server = FakeObsWs(**kw)
        made.append(server)
        return server

    yield make
    for server in made:
        server.close()
        assert server.errors == []


def test_run_live_stream_refused_and_obs_left_running(rig, ws):
    r, server = rig(), ws(streaming=True)
    res = r.run(server.uri)
    out = res.stdout + res.stderr
    assert res.returncode == 12, out
    assert "clean close REFUSED: obs64 is STREAMING" in out
    assert "HARNESS DONE" not in out
    assert r.obs_alive(), "a refused deploy must leave OBS running"
    assert server.requests == ["GetStreamStatus", "GetRecordStatus"]


def test_run_recording_is_stopped_and_confirmed_before_the_close(rig, ws):
    r, server = rig(), ws(recording=True, record_stops_after=2)
    res = r.run(server.uri)
    out = res.stdout + res.stderr
    assert res.returncode == 0, out
    assert "obs64 is RECORDING -- StopRecord before the close" in out
    assert "recording stopped in" in out
    stop = server.requests.index("StopRecord")
    # (0a) read, (2) re-read, StopRecord, then GetRecordStatus until it reads stopped (3 reads)
    assert server.requests[:stop] == ["GetStreamStatus", "GetRecordStatus"] * 2
    assert server.requests[stop + 1:] == ["GetRecordStatus"] * 3
    assert server.recording is False
    # no window to close under pwsh on Linux: the named fallback forces the fake obs64
    assert "-- forcing" in out
    assert not r.obs_alive()


def test_run_unreadable_websocket_fails_open_and_is_named(rig):
    r = rig()
    closed = FakeObsWs()
    uri = closed.uri
    closed.close()  # nothing listens on that port any more
    res = r.run(uri)
    out = res.stdout + res.stderr
    assert res.returncode == 0, out
    assert out.count("unreadable") == 2, out  # step (0a) and step (2), both named
    assert "the stream state is UNKNOWN; continuing" in out
    assert "HARNESS DONE" in out
    assert not r.obs_alive()


def test_run_authentication_uses_the_box_config_password(rig, ws):
    r, server = rig(), ws(password="rig-ws-pw")
    r.write("plugin_config/obs-websocket/config.json", json.dumps({"server_password": "rig-ws-pw"}))
    res = r.run(server.uri)
    out = res.stdout + res.stderr
    assert res.returncode == 0, out
    assert server.auth_failures == 0 and server.identified == 2
    assert "streaming=False recording=False" in out
    assert "unreadable" not in out


def test_run_no_obs_running_reads_nothing_and_closes_nothing(rig, ws):
    r, server = rig(obs_running=False), ws(streaming=True)
    res = r.run(server.uri)
    out = res.stdout + res.stderr
    assert res.returncode == 0, out
    assert "obs64 is not running -- no broadcast to protect" in out
    assert "obs64 is not running -- nothing to close" in out
    assert server.connections == 0


def _collection(**extra):
    sources = [
        {"name": "Scene", "id": "scene", "versioned_id": "scene", "mixers": 0, "sync": 0, "settings": {}},
        {"name": "mbc", "id": "asio_input_capture", "versioned_id": "asio_input_capture", "mixers": 255,
         "sync": 37000000, "settings": {}},
        {"name": "NDI 2ME PGM", "id": "ndi_source", "versioned_id": "ndi_source", "mixers": 255,
         "sync": 0, "settings": {"genlock_latency_ms_src": 1020}},
        {"name": "Zaloha kamera", "id": "ndi_source", "versioned_id": "ndi_source", "mixers": 0,
         "sync": 0, "settings": {}},
        {"name": "fallback repro", "id": "asio_input_capture", "versioned_id": "asio_input_capture",
         "mixers": 255, "sync": 1500000, "settings": {}},
    ]
    return json.dumps({"name": "Stream_Obs", "sources": sources, **extra})


@pytest.mark.parametrize("ini,line", [("user.ini", "SceneCollectionFile=Stream_Obs.json"),
                                      ("global.ini", "SceneCollectionFile=Stream_Obs")])
def test_run_read_back_prints_the_saved_pins_and_sync(rig, ini, line):
    r = rig(obs_running=False)
    r.write(ini, f"[Basic]\nProfile=Stream_Obs\n{line}\n")
    r.write("basic/scenes/Stream_Obs.json", _collection())
    res = r.run("ws://127.0.0.1:9", blocks=["obs_saved_settings_readback_ps"])
    out = res.stdout + res.stderr
    assert res.returncode == 0, out
    assert "issue 1367 SAVED scene collection" in out and "Stream_Obs.json" in out
    assert "NDI input 'NDI 2ME PGM': genlock_latency_ms_src = 1020 ms" in out
    assert "NDI input 'Zaloha kamera': genlock_latency_ms_src = absent (the build default)" in out
    assert "audio input 'mbc': sync = 37 ms" in out
    assert "audio input 'fallback repro': sync = 1.5 ms" in out
    assert "audio input 'NDI 2ME PGM': sync = 0 ms" in out
    # a source without audio (a scene, an NDI input saved with mixers 0) prints no sync
    assert "'Scene'" not in out and "audio input 'Zaloha kamera'" not in out


@pytest.mark.parametrize("damage", ["no_ini", "bad_json"])
def test_run_read_back_failure_is_named_and_never_fails_the_deploy(rig, damage):
    r = rig(obs_running=False)
    if damage == "bad_json":
        r.write("user.ini", "[Basic]\nSceneCollectionFile=Stream_Obs.json\n")
        r.write("basic/scenes/Stream_Obs.json", "{ not json")
    res = r.run("ws://127.0.0.1:9", blocks=["obs_saved_settings_readback_ps"])
    out = res.stdout + res.stderr
    assert res.returncode == 0, out
    assert "saved-settings read-back UNREAD" in out
    assert "HARNESS DONE" in out
