"""issue 1487: dantesync -> dsync. The pin, the swap programs RUN on a fake box, and the scope of the roll."""

import os
import re
import shutil
import stat
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / "scripts"


_SEAMS = ("DSYNC_LEGACY_LINUX_UNIT", "DSYNC_LEGACY_LINUX_BIN", "DSYNC_LEGACY_LINUX_STATE")


def bash(body, env=None):
    full = {k: v for k, v in os.environ.items() if k not in _SEAMS}
    full.update(env or {})
    return subprocess.run(["bash", "-c", body], capture_output=True, text=True, env=full, cwd=ROOT)


def src(expr):
    """Source the upgrader (its guard stops before the flow) and evaluate expr."""
    return bash(f'set -euo pipefail; . scripts/dantesync-fleet-upgrade.sh; {expr}')


def test_the_pinned_shas_are_of_the_pinned_version():
    pin = re.search(r'DANTESYNC_VERSION_PIN:-([0-9.]+)', (SCRIPTS / "dantesync-version-gate.sh").read_text()).group(1)
    rel = (SCRIPTS / "lib/dsync-release.sh").read_text()
    assert pin == "1.35.1"
    assert re.search(r'DSYNC_PINNED_SHA_VERSION="\$\{DSYNC_PINNED_SHA_VERSION-([0-9.]+)\}"', rel).group(1) == pin
    assert len(re.findall(r'DSYNC_SHA256_[A-Z_]+="[0-9a-f]{64}"', rel)) == 4


def test_a_binary_that_is_not_the_pinned_one_is_refused():
    r = bash('. scripts/lib/dsync-release.sh; dsync_release_sha_ok dsync-linux-amd64 1.35.1 deadbeef; echo rc=$?')
    assert "rc=1" in r.stdout and "the pin says" in r.stderr
    r = bash('. scripts/lib/dsync-release.sh; dsync_release_sha_ok dsync-linux-amd64 9.9.9 deadbeef; echo rc=$?')
    assert "rc=0" in r.stdout


def _old_unit(text):
    """A unit as the pre-rename renderers wrote it."""
    return (text.replace("Description=dsync (PTP", "Description=Dante Time Sync (PTP")
            .replace("/usr/local/bin/dsync", "/usr/local/bin/dantesync"))


@pytest.mark.parametrize("args", ["server ''", "client '--ntp-server strih.lan'",
                                  "server '' enp56s0"])
def test_the_derived_unit_equals_what_setup_strih_renders(args):
    """Equal bytes, or the next deploy restarts the date master for a unit that only moved."""
    r = bash(f'set -euo pipefail; . scripts/lib/strih-provision.sh; strih_dantesync_unit_text {args}')
    assert r.returncode == 0, r.stderr
    new = r.stdout
    assert "ExecStart=/usr/local/bin/dsync" in new
    r = subprocess.run(
        ["bash", "-c", '. scripts/lib/dsync-swap.sh; eval "sed $(dsync_unit_from_legacy_sed)"'],
        input=_old_unit(new), capture_output=True, text=True, cwd=ROOT)
    assert r.stdout == new


class Box:
    """A fake root: every absolute path of the program is mapped under it; tools log `<tool> <args> root=<mode>`."""

    def __init__(self, tmp, legacy=True, ro=False, verify_breaks=False):
        self.root = tmp / "box"
        self.bin = tmp / "stub"
        self.log = tmp / "log"
        self.state = tmp / "rootmode"
        self.state.write_text("ro" if ro else "rw")
        self.bin.mkdir()
        (self.root / "etc/systemd/system").mkdir(parents=True)
        (self.root / "usr/local/bin").mkdir(parents=True)
        (self.root / "tmp").mkdir()
        self.ro_start = ro
        if legacy:
            (self.root / "etc/dantesync").mkdir()
            (self.root / "etc/dantesync/config.json").write_text("{}\n")
            (self.root / "etc/dantesync/date-offset.json").write_text('{"seq":7}\n')
            (self.root / "etc/systemd/system/dantesync.service").write_text(
                "[Unit]\nDescription=Dante Time Sync (PTP/NTP Synchronization)\n[Service]\n"
                "ExecStart=/usr/local/bin/dantesync --ntp-server strih.lan\n")
            (self.root / "etc/systemd/system/dantesync.service.d").mkdir()
            (self.root / "etc/systemd/system/dantesync.service.d/10-x.conf").write_text("[Service]\nNice=-5\n")
            (self.root / "usr/local/bin/dantesync").write_text("old")
        else:
            (self.root / "etc/dsync").mkdir()
            (self.root / "etc/systemd/system/dsync.service").write_text("[Service]\nExecStart=/usr/local/bin/dsync\n")
            (self.root / "usr/local/bin/dsync").write_text("old-dsync")
        for tool in ("systemctl", "mount", "sync"):
            self._stub(tool)
        self._findmnt()
        for name in ("dsync", "dantesync"):
            (self.bin / name).write_text(f"#!/bin/sh\necho {name} 1.35.1\n")
            (self.bin / name).chmod(0o755)
        staged = self.root / "tmp/dsync-staged"
        staged.write_text("#!/bin/sh\necho dsync 1.35.1\n")
        staged.chmod(0o755)
        sha = subprocess.check_output(["sha256sum", str(staged)], text=True).split()[0]
        (self.root / "tmp/dsync-staged.sha256").write_text(f"{sha}  x\n")
        self.fail_on = "restart dsync" if verify_breaks else ""

    def _stub(self, tool):
        p = self.bin / tool
        extra = ""
        if tool == "mount":
            extra = f'case "$*" in *remount,rw*) echo rw > {self.state} ;; *remount,ro*) echo ro > {self.state} ;; esac\n'
        p.write_text(
            "#!/bin/bash\n"
            f'echo "{tool} $* root=$(cat {self.state})" >> {self.log}\n{extra}'
            f'[ -n "${{FAKE_FAIL:-}}" ] && [ "{tool} $*" = "systemctl $FAKE_FAIL" ] && exit 1\nexit 0\n')
        p.chmod(p.stat().st_mode | stat.S_IEXEC)

    def _findmnt(self):
        p = self.bin / "findmnt"
        p.write_text(f'#!/bin/bash\necho "$(cat {self.state}),relatime"\n')
        p.chmod(0o755)

    def run(self, program, fail=""):
        m = re.sub(r'(?<![\w/.$-])/(etc|usr/local/bin|tmp/dsync)', lambda x: f"{self.root}/{x.group(1)}", program)
        return subprocess.run(["bash", "-c", m], capture_output=True, text=True,
                              env={**os.environ, "PATH": f"{self.bin}:{os.environ['PATH']}", "FAKE_FAIL": fail}, cwd=self.root)

    def logged(self):
        return self.log.read_text().splitlines() if self.log.exists() else []


def _upgrade_program(role="slave"):
    r = src(f'dantesync_linux_upgrade_cmd 1.35.1 {role}')
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_the_swap_moves_the_state_and_keeps_the_way_back_until_the_finalize(tmp_path):
    box = Box(tmp_path)
    r = box.run(_upgrade_program("ntp-master"))
    assert r.returncode == 0, r.stderr
    assert (box.root / "etc/dsync/date-offset.json").read_text() == '{"seq":7}\n'
    assert not (box.root / "etc/dantesync").exists()
    unit = (box.root / "etc/systemd/system/dsync.service").read_text()
    assert "ExecStart=/usr/local/bin/dsync --ntp-server strih.lan" in unit
    assert (box.root / "etc/systemd/system/dsync.service.d/10-x.conf").exists()
    assert (box.root / "usr/local/bin/dantesync").exists(), "the way back stays until verified"
    log = box.logged()
    order = [next(i for i, l in enumerate(log) if l.startswith(p))
             for p in ("systemctl stop dantesync", "systemctl disable dantesync", "systemctl enable dsync", "systemctl restart dsync")]
    assert order == sorted(order)
    box.run(src("dsync_linux_finalize_cmd").stdout)
    assert not (box.root / "usr/local/bin/dantesync").exists()
    assert not (box.root / "etc/systemd/system/dantesync.service").exists()
    assert not (box.root / "etc/systemd/system/dantesync.service.d").exists()


def test_nothing_starts_on_a_writable_root(tmp_path):
    box = Box(tmp_path, ro=True)
    r = box.run(_upgrade_program())
    assert r.returncode == 0, r.stderr
    log = box.logged()
    starts = [l for l in log if re.match(r"systemctl (restart|start) ", l)]
    assert starts and all(l.endswith("root=ro") for l in starts), log
    for l in log:
        if re.match(r"systemctl (stop|disable|enable) ", l):
            assert l.endswith("root=rw"), l


def test_a_failed_start_swaps_back_to_dantesync(tmp_path):
    box = Box(tmp_path, ro=True)
    r = box.run(_upgrade_program(), fail="restart dsync")
    assert r.returncode != 0
    assert "SELF-HEAL: restored previous dantesync" in r.stderr
    assert (box.root / "etc/dantesync/date-offset.json").exists()
    assert not (box.root / "etc/systemd/system/dsync.service").exists()
    assert not (box.root / "usr/local/bin/dsync").exists()
    assert any(l.startswith("systemctl restart dantesync") and l.endswith("root=ro") for l in box.logged())


def test_a_verify_failure_rollback_is_the_reverse_swap(tmp_path):
    box = Box(tmp_path)
    assert box.run(_upgrade_program()).returncode == 0
    r = box.run(src('dantesync_linux_rollback_cmd slave 1.20.0').stdout)
    assert r.returncode != 127, r.stderr
    assert (box.root / "etc/dantesync/date-offset.json").exists()
    assert not (box.root / "etc/dsync").exists()
    assert any(l.startswith("systemctl restart dantesync") for l in box.logged())


def test_a_dsync_node_upgrades_in_place_and_never_enters_the_swap(tmp_path):
    box = Box(tmp_path, legacy=False)
    r = box.run(_upgrade_program())
    assert r.returncode == 0, r.stderr
    assert (box.root / "usr/local/bin/dsync.bak").read_text() == "old-dsync"
    assert not any("dantesync" in l for l in box.logged())


def test_a_finalize_on_a_clean_node_opens_no_window(tmp_path):
    box = Box(tmp_path, legacy=False, ro=True)
    assert box.run(src("dsync_linux_finalize_cmd").stdout).returncode == 0
    assert not any("remount" in l for l in box.logged())


def test_the_canary_is_never_the_master_and_the_master_goes_last():
    assert src('dantesync_resolve_canary "strih-snv cam2 cam3" "resolume-pp" "" "strih-snv strih-pp"').stdout.split() == ["cam2", "resolume-pp"]
    assert src('dantesync_masters_last "strih-snv cam3 strih-pp dev1" "strih-snv strih-pp"').stdout.split() == ["cam3", "dev1", "strih-snv", "strih-pp"]


@pytest.mark.parametrize("node", ["mbc", "fohabl"])
def test_a_node_dsync_upgrades_itself_stays_a_clock_row_outside_the_roll(node):
    r = bash(f'. scripts/lib/dantesync-fleet.sh; dantesync_fleet_self_upgraded {node}; echo rc=$?; '
             f'dantesync_fleet_field {node} role')
    assert "rc=0" in r.stdout and r.stdout.split()[-1] in ("video", "audio")


def test_a_rolled_node_is_not_a_self_upgraded_one():
    r = bash('. scripts/lib/dantesync-fleet.sh; dantesync_fleet_self_upgraded resolume-snv; echo rc=$?; '
             'dantesync_fleet_self_upgraded ""; echo rc=$?; dantesync_fleet_self_upgraded alex-nb; echo rc=$?')
    # issue 1487 (12.11.2026): dsync cannot log in to alex-nb, so camera-box rolls it again
    assert r.stdout.split() == ["rc=1", "rc=1", "rc=1"]


def test_a_windows_node_already_moved_reads_the_new_location():
    """resolume-snv, moved by hand on 2.11.2026: the gate and the upgrader read C:\\Program Files\\dsync."""
    gate = (SCRIPTS / "dantesync-version-gate.sh").read_text()
    assert r"C:\\Program Files\\dsync\\dsync.exe" in gate
    assert r"C:\\Program Files\\dsync\\dsync-tray.exe" in gate
    assert src('printf %s "$DANTESYNC_WIN_EXE"').stdout == r"C:\Program Files\dsync\dsync.exe"
    drift = bash('. scripts/lib/dantesync-fleet.sh; printf %s "$DANTESYNC_FLEET_CONFIG_WINDOWS"')
    assert drift.stdout == r"C:\ProgramData\dsync\config.json"


def test_the_windows_finalize_touches_nothing_on_a_node_with_no_old_install():
    """The already-moved node: no LEGACY line, the state dir is never named."""
    ps = src("dsync_windows_finalize_ps").stdout
    assert "if ($hadLegacy) {" in ps
    assert "ProgramData\\dsync" not in ps and "ProgramData\\DanteSync" not in ps
    assert ps.index("$hadLegacy = ") < ps.index("sc.exe delete dantesync")


def test_the_windows_program_keeps_the_old_service_until_the_finalize():
    ps = src("dantesync_windows_upgrade_ps 1.35.1 slave").stdout
    assert "sc.exe delete dantesync" not in ps
    assert ps.index("Set-Service -Name dantesync -StartupType Disabled") < ps.index("New-Service -Name dsync")
    assert ps.index("Rename-Item -LiteralPath 'C:\\ProgramData\\DanteSync'") < ps.index("Start-Service dsync")
    assert "[regex]::Escape('C:\\Program Files\\DanteSync\\dantesync.exe')" in ps
    assert "Invoke-DsyncSwapBack" in ps[ps.index("} catch {"):]
    assert "https://github.com/zbynekdrlik/dsync/releases/download/v1.35.1/dsync-windows-amd64.exe" in ps


def _pwsh():
    pwsh = os.environ.get("PWSH") or shutil.which("pwsh")
    home_pwsh = os.path.expanduser("~/.local/pwsh74/pwsh")
    if not pwsh and os.access(home_pwsh, os.X_OK):
        pwsh = home_pwsh
    if not pwsh:
        pytest.fail("no pwsh: install PowerShell 7 or set PWSH=/path/to/pwsh (the swap .ps1 must really run)")
    return pwsh


def _win_swap(tmp, fail_start=False):
    """RUN the emitted upgrade on a node with the dantesync service: service cmdlets stubbed, the state
    dir rename and the exe copy real, on tmp paths. Returns (stdout, the CALL list, exe, new, old state)."""
    new_dir, old_dir, old_state, new_state = tmp / "dsync", tmp / "DanteSync", tmp / "state" / "DanteSync", tmp / "state" / "dsync"
    new_dir.mkdir()
    old_dir.mkdir()
    old_state.mkdir(parents=True)
    (old_state / "date-offset.json").write_text('{"seq":7}')
    (old_dir / "dantesync.exe").write_text("the 1.20.0 exe\n")
    exe, rel, temp = new_dir / "dsync.exe", tmp / "release.exe", tmp / "temp"
    temp.mkdir()
    rel.write_text("the 1.35.1 exe\n")
    r = src(f"DANTESYNC_WIN_EXE='{exe}'; DANTESYNC_WIN_BAK='{exe}.bak'; "
            f"DANTESYNC_WIN_TRAY_EXE='{tmp / 'no-tray.exe'}'; DANTESYNC_WIN_LEGACY_TRAY_EXE='{tmp / 'no-tray2.exe'}'; "
            f"DSYNC_LEGACY_WIN_EXE='{old_dir / 'dantesync.exe'}'; DSYNC_LEGACY_WIN_STATE='{old_state}'; DSYNC_WIN_STATE='{new_state}'; "
            "dantesync_windows_upgrade_ps 1.35.1")
    assert r.returncode == 0, r.stderr
    program = tmp / "upgrade.ps1"
    program.write_text(r.stdout.replace("& $exe --version", "Write-Output 'version read'"))
    harness = tmp / "harness.ps1"
    harness.write_text(f"""
$script:calls = New-Object System.Collections.Generic.List[string]
$script:hasNew = $false
$script:failStart = ${'true' if fail_start else 'false'}
function Note($t) {{ $script:calls.Add($t) }}
function Invoke-WebRequest {{ [CmdletBinding()] param([switch]$UseBasicParsing, $Uri, $OutFile)
    if ($OutFile -like '*.sha256') {{ Set-Content -NoNewline -LiteralPath $OutFile -Value ((Get-FileHash -Algorithm SHA256 -LiteralPath '{rel}').Hash + '  x') }}
    else {{ Microsoft.PowerShell.Management\\Copy-Item -Force -LiteralPath '{rel}' -Destination $OutFile }} }}
function Get-Service {{ [CmdletBinding()] param($Name)
    if ($Name -eq 'dantesync' -or ($Name -eq 'dsync' -and $script:hasNew)) {{ [pscustomobject]@{{ Name = $Name }} }} }}
function Get-CimInstance {{ [CmdletBinding()] param([Parameter(Position = 0)]$ClassName, $Filter)
    [pscustomobject]@{{ PathName = '"{old_dir / 'dantesync.exe'}" --service' }} }}
function Stop-Service {{ [CmdletBinding()] param([Parameter(Position = 0)]$Name, [switch]$Force) Note ('Stop-Service ' + $Name) }}
function Start-Service {{ [CmdletBinding()] param([Parameter(Position = 0)]$Name)
    Note ('Start-Service ' + $Name); if ($Name -eq 'dsync' -and $script:failStart) {{ throw 'the service did not start' }} }}
function Set-Service {{ [CmdletBinding()] param($Name, $StartupType) Note ('Set-Service ' + $Name + ' ' + $StartupType) }}
function New-Service {{ [CmdletBinding()] param($Name, $BinaryPathName, $DisplayName, $StartupType, $Description)
    $script:hasNew = $true; Note ('New-Service ' + $BinaryPathName) }}
function sc.exe {{ Note ('sc ' + ($args -join ' ')) }}
function Wait-Process {{ [CmdletBinding()] param($Name, $Timeout, $Id) }}
function Get-Process {{ [CmdletBinding()] param([string[]]$Name) }}
function Stop-Process {{ [CmdletBinding()] param($Name, [switch]$Force) }}
function cmd {{ }}
try {{ . '{program}' }} catch {{ Write-Output ('PROGRAM THREW: ' + $_.Exception.Message) }}
foreach ($c in $script:calls) {{ Write-Output ('CALL ' + $c) }}
""")
    run = subprocess.run([_pwsh(), "-NoProfile", "-NonInteractive", "-File", str(harness)],
                         capture_output=True, text=True, timeout=180, env=dict(os.environ, TEMP=str(temp)))
    assert run.returncode == 0, run.stdout + run.stderr
    calls = [ln[5:] for ln in run.stdout.splitlines() if ln.startswith("CALL ")]
    return run.stdout, calls, exe, new_state, old_state


def test_the_windows_swap_keeps_the_state_and_the_service_arguments(tmp_path):
    out, calls, exe, new_state, old_state = _win_swap(tmp_path)
    assert "PROGRAM THREW" not in out, out
    assert (new_state / "date-offset.json").read_text() == '{"seq":7}'
    assert not old_state.exists()
    assert exe.read_text() == "the 1.35.1 exe\n"
    assert calls[0] == "Stop-Service dantesync"
    assert calls[1] == "Set-Service dantesync Disabled"
    made = next(c for c in calls if c.startswith("New-Service "))
    assert str(exe) in made and made.endswith(" --service"), made
    assert calls[-1] == "Start-Service dsync"
    assert not any(c.startswith("sc ") for c in calls), "the old service stays until the finalize"


def test_a_failed_windows_start_swaps_back_to_the_running_dantesync(tmp_path):
    out, calls, _, new_state, old_state = _win_swap(tmp_path, fail_start=True)
    assert "PROGRAM THREW" in out, out
    assert (old_state / "date-offset.json").exists() and not new_state.exists()
    assert "sc delete dsync" in calls
    assert "Set-Service dantesync Automatic" in calls
    assert calls[-1] == "Start-Service dantesync", calls


def _retire_env(tmp):
    return {"DSYNC_INSTALL_LEGACY_UNIT": str(tmp / "dantesync.service"), "DSYNC_INSTALL_LEGACY_BIN": str(tmp / "dantesync"),
            "DSYNC_INSTALL_LEGACY_STATE": str(tmp / "etc-dantesync")}


def test_provisioning_retires_a_live_dantesync_and_says_it_ran(tmp_path):
    stub = tmp_path / "stub"
    stub.mkdir()
    (stub / "systemctl").write_text(f'#!/bin/bash\necho "$*" >> {tmp_path}/log\nexit 0\n')
    (stub / "systemctl").chmod(0o755)
    (tmp_path / "dantesync.service").write_text("x")
    (tmp_path / "dantesync").write_text("x")
    old, new = tmp_path / "old", tmp_path / "new"
    old.mkdir()
    (old / "date-offset.json").write_text("{}")
    body = ('set -euo pipefail; . scripts/lib/dantesync-install.sh; '
            f'DSYNC_LEGACY_LINUX_STATE={old}; DSYNC_LINUX_STATE={new}; '
            'dantesync_install_retire_legacy; echo active=$DSYNC_RETIRED_ACTIVE')
    r = bash(body, env={**_retire_env(tmp_path), "PATH": f"{stub}:{os.environ['PATH']}", "DSYNC_INSTALL_LEGACY_STATE": str(old)})
    assert r.returncode == 0, r.stderr
    assert "active=1" in r.stdout
    assert (new / "date-offset.json").exists() and not old.exists()
    assert not (tmp_path / "dantesync").exists() and not (tmp_path / "dantesync.service").exists()
    log = (tmp_path / "log").read_text()
    assert log.index("stop dantesync") < log.index("daemon-reload")


def test_setup_imag_installs_the_pin_never_latest():
    text = (SCRIPTS / "setup-imag.sh").read_text()
    assert "DANTESYNC_REPO" not in text
    assert 'DANTESYNC_URL="$(dantesync_install_release_url "$DANTESYNC_PIN")"' in text
