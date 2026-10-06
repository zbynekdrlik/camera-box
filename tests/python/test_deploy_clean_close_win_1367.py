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
FAILS, never skips. pwsh 7 on Linux has no OBS window, so the runs shadow Get-Process with a
function (the repo's stub-function pattern) that returns the test's OWN fake obs64 with a scripted
MainWindowTitle and CloseMainWindow: a clean exit (SIGTERM), a hang (the bound shortened by the
test), no window, a projector in front, a refused close. A manual WM_CLOSE of stream OBS exited in
6.6 s live (6.10.2026, comment 6013239473); this fragment's own close first runs live at the next
deploy. pwsh 7 also accepts syntax Windows PowerShell 5.1 rejects, so a token scan bans that.

Only the test's own fake obs64 is visible to the fragment (the stub filters by pid), so two runs of
this file in parallel lanes cannot kill each other's fake.
"""
import json
import os
import pathlib
import re
import shutil
import socket
import signal
import subprocess
import sys
import threading

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
    # the whole line: a substring check would also accept 450000
    assert "\n$ccCloseTimeoutMs = 45000\n" in seg
    # re-read right before the close; a stream that started since (0a) is refused too
    assert seg.index("Get-CcObsOutputState") < seg.index(".CloseMainWindow()")
    assert "STARTED STREAMING" in seg and "exit 12" in seg
    # StopRecord, then a bounded confirm of the record state, both before the close
    stop = seg.index("'StopRecord'")
    assert stop < seg.index("'GetRecordStatus'", stop) < seg.index(".CloseMainWindow()")
    assert "recording stopped in" in seg
    # the close on the ONE LIVE obs64 of this session (a stale handle of an exited obs64 is not
    # counted), only when its front window is OBS's own, waited on up to the bound
    assert "$ccAll = @(Get-CcLiveObs)" in seg
    assert "$_.SessionId -eq $ccSession" in seg
    assert seg.index("$ccTitle -notlike 'OBS *'") < seg.index(".CloseMainWindow()")
    assert "Where-Object { -not $_.HasExited -and $_.Threads.Count -gt 0 }" in p
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
    # wrong process count, no main window, a projector in front, a refused close, the timeout
    assert seg.count("$ccForce = $true") == 5
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


@pytest.mark.parametrize("mode", MODES)
def test_a_late_stream_refusal_re_enables_the_keep_alive_tasks(mode):
    stream = _program("stream", mode)
    # stream (keep-alive tasks): the hook is defined right after the step-(1b) disable, before (2)
    hook = stream.index("# (1c) issue 1367")
    assert stream.index("# (1b)") < hook < stream.index("# (2) issue 1367")
    assert stream.count("function Invoke-CcRefusalRestore") == 1
    seg = stream[hook:stream.index("# (2) issue 1367")]
    assert "foreach ($t in $disabledKeepAlive)" in seg and "schtasks /Change /TN $t /ENABLE" in seg
    # the step-(2) refusal calls it, if defined, right before its exit 12
    close = stream[stream.index("# (2) issue 1367"):stream.index("# (3) ")]
    late = close.index("STARTED STREAMING")
    call = close.index("if (Get-Command Invoke-CcRefusalRestore -ErrorAction SilentlyContinue) { Invoke-CcRefusalRestore }", late)
    assert call < close.index("exit 12", late)
    # resolume has no keep-alive task, so no hook (the refusal only exits)
    assert "Invoke-CcRefusalRestore {" not in _program("resolume", mode)


def test_the_fragment_uses_no_powershell_7_only_syntax(tmp_path):
    # pwsh 7 parses && || ?? ?. and the ternary; Windows PowerShell 5.1 rejects them all
    src = tmp_path / "fragment.ps1"
    src.write_text("".join(_block(fn) for fn in BLOCKS + ["obs_clean_close_refusal_restore_ps"]))
    scan = tmp_path / "scan.ps1"
    scan.write_text(
        "$t = $null; $e = $null\n"
        "[void][System.Management.Automation.Language.Parser]::ParseFile($args[0], [ref]$t, [ref]$e)\n"
        "$bad = @($t | Where-Object { $_.Kind -in 'AndAnd','OrOr','QuestionQuestion','QuestionQuestionEquals',"
        "'QuestionDot','QuestionLBracket','QuestionMark' })\n"
        "foreach ($x in $bad) { Write-Output \"PS7-only $($x.Kind) at line $($x.Extent.StartLineNumber)\" }\n"
        "Write-Output \"tokens=$($t.Count) errors=$($e.Count) ps7only=$($bad.Count)\"\n"
        "exit ($bad.Count + $e.Count)\n")
    r = subprocess.run([_pwsh(), "-NoProfile", "-NonInteractive", "-File", str(scan), str(src)],
                       capture_output=True, text=True, timeout=120)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "errors=0 ps7only=0" in r.stdout


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

# Get-Process shadowed by a function: only the test's own fake obs64 is visible, with a scripted
# main window. Every other process comes from the real cmdlet.
STUB = r"""
$ccFakeObsPid = __PID__
$ccFakeMode = '__MODE__'
function Get-Process {
  [CmdletBinding()]
  param([Parameter(Position = 0)][string[]]$Name, [int[]]$Id)
  foreach ($q in @(Microsoft.PowerShell.Management\Get-Process @PSBoundParameters)) {
    if ($q.ProcessName -ne 'obs64') { $q; continue }
    if ($q.Id -ne $ccFakeObsPid) { continue }
    $title = 'OBS Studio 32.2.0 - newlevel.media build fake - Profile: Stream_Obs'
    if ($ccFakeMode -eq 'nowindow') { $title = '' }
    if ($ccFakeMode -eq 'projector') { $title = 'Fullscreen Projector (Program)' }
    $q | Add-Member -Force -MemberType NoteProperty -Name MainWindowTitle -Value $title
    $q | Add-Member -Force -MemberType ScriptMethod -Name CloseMainWindow -Value {
      Write-Host "HARNESS CloseMainWindow mode=$ccFakeMode pid=$($this.Id)"
      if ($ccFakeMode -eq 'clean') { $null = & /bin/kill -TERM $this.Id; return $true }
      if ($ccFakeMode -eq 'hang') { return $true }
      return $false
    }
    $q
  }
}
"""
REFUSAL_HOOK = "function Invoke-CcRefusalRestore { Write-Host 'HARNESS REFUSAL HOOK RAN' }\n"


class Rig:
    """A fake obs64 (a copy of `sleep` under that name) + APPDATA, and the fragment run in pwsh."""

    def __init__(self, tmp_path, obs_running=True):
        self.tmp = tmp_path
        self.appdata = tmp_path / "appdata"
        (self.appdata / "obs-studio" / "basic" / "scenes").mkdir(parents=True)
        self.obs = None
        self.reaper = None
        if obs_running:
            exe = tmp_path / "bin" / "obs64"
            exe.parent.mkdir()
            shutil.copy(shutil.which("sleep"), exe)
            self.obs = subprocess.Popen([str(exe), "600"])
            # reap it the moment it dies, so a closed fake leaves no zombie behind
            self.reaper = threading.Thread(target=self.obs.wait, daemon=True)
            self.reaper.start()

    def write(self, rel, text):
        path = self.appdata / "obs-studio" / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def run(self, uri, blocks=BLOCKS, mode="nowindow", close_ms=None, hook=False):
        body = "\n".join(_block(fn) for fn in blocks).replace(f"'{WS_URI}'", f"'{uri}'")
        if close_ms is not None:
            assert "\n$ccCloseTimeoutMs = 45000\n" in body
            body = body.replace("\n$ccCloseTimeoutMs = 45000\n", f"\n$ccCloseTimeoutMs = {close_ms}\n")
        pid = self.obs.pid if self.obs is not None else 0
        prelude = STUB.replace("__PID__", str(pid)).replace("__MODE__", mode) + (REFUSAL_HOOK if hook else "")
        prog = self.tmp / "program.ps1"
        prog.write_text("$ErrorActionPreference = 'Stop'\n" + prelude + body + "\nWrite-Host 'HARNESS DONE'\nexit 0\n")
        env = dict(os.environ, APPDATA=str(self.appdata))
        return subprocess.run([_pwsh(), "-NoProfile", "-NonInteractive", "-File", str(prog)],
                              capture_output=True, text=True, timeout=120, env=env)

    def obs_alive(self):
        if self.obs is None:
            return False
        self.reaper.join(timeout=2)
        return self.obs.returncode is None

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


def _dead_uri():
    # a port that was free a moment ago and that nothing listens on
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return f"ws://127.0.0.1:{port}"


def test_run_unreadable_websocket_fails_open_and_is_named(rig):
    r = rig()
    res = r.run(_dead_uri())
    out = res.stdout + res.stderr
    assert res.returncode == 0, out
    # step (0a) and step (2) each name it, once
    assert out.count("unreadable (") == 2, out
    assert out.count("-- the stream state is UNKNOWN; continuing") == 1, out
    assert out.count("-- closing without the stream/record read") == 1, out
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


# --- the close branch itself, through the Get-Process stub -------------------------------------

def test_run_clean_close_exits_by_itself_and_is_never_forced(rig, ws):
    r, server = rig(), ws()
    res = r.run(server.uri, mode="clean")
    out = res.stdout + res.stderr
    assert res.returncode == 0, out
    assert "HARNESS CloseMainWindow mode=clean" in out
    assert "clean close OK in" in out and "'OBS Studio 32.2.0" in out
    # the wait ends when OBS is gone, not at the 45 s bound
    waited_ms = int(re.search(r"clean close OK in (\d+) ms", out).group(1))
    assert waited_ms < 10000, out
    assert "-- forcing" not in out
    assert not r.obs_alive() and r.obs.returncode == -signal.SIGTERM  # closed, not killed


def test_run_close_that_hangs_is_forced_after_the_bound(rig, ws):
    r, server = rig(), ws()
    res = r.run(server.uri, mode="hang", close_ms=1500)
    out = res.stdout + res.stderr
    assert res.returncode == 0, out
    assert "HARNESS CloseMainWindow mode=hang" in out
    assert "clean close timed out -- forcing" in out and "after 1.5 s" in out
    assert "clean close OK" not in out
    assert not r.obs_alive() and r.obs.returncode == -signal.SIGKILL  # the named force did it


@pytest.mark.parametrize("mode,line", [
    ("projector", "shows 'Fullscreen Projector (Program)' in front, not the OBS main window -- forcing"),
    ("nowindow", "has no main window -- forcing"),
    ("disabled", "did not take the close (disabled behind a modal dialog) -- forcing"),
])
def test_run_no_closable_obs_window_is_forced_by_name(rig, ws, mode, line):
    r, server = rig(), ws()
    res = r.run(server.uri, mode=mode)
    out = res.stdout + res.stderr
    assert res.returncode == 0, out
    assert line in out
    # a projector in front is never sent the close (it would drop out of the saved projectors)
    assert ("HARNESS CloseMainWindow" in out) == (mode == "disabled")
    assert not r.obs_alive() and r.obs.returncode == -signal.SIGKILL


def test_run_stream_started_after_0a_is_refused_and_restores(rig, ws):
    r, server = rig(), ws(stream_starts_after=1)
    res = r.run(server.uri, mode="clean", hook=True)
    out = res.stdout + res.stderr
    assert res.returncode == 12, out
    assert "streaming=False" in out  # (0a) read it idle
    assert "STARTED STREAMING after the step-(0a) read" in out
    assert out.index("STARTED STREAMING") < out.index("HARNESS REFUSAL HOOK RAN")
    assert "HARNESS CloseMainWindow" not in out and "HARNESS DONE" not in out
    assert "StopRecord" not in server.requests
    assert r.obs_alive(), "a refused deploy must leave OBS running"

