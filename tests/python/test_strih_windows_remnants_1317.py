"""issue 1317 part 4 -- the last Windows-strih remnants after the M4 cut-over.

The strih role moved to the Linux notebook strih-lx (10.77.9.202) on 20.9.2026 and the Windows
strih PC is retired. Parts 1-3 retired it from the fleet list, the watchdogs and the planners; this
file pins the remaining seven remnants:

  1. mv-reverify escalation: the headless strih-OBS restart routes by platform (strih-lx =
     `systemctl --user restart strih-obs.service` over plain ssh; Windows = the unchanged
     PowerShell/AHK program).
  2. phase_sync/av_sync calibrate push plans map the host by its obs-fleet CLASS (covered in
     test_phase_sync_calibrate.py / test_av_sync_calibrate.py) -- here: the shared python reader
     of the fleet table they (and the rig-health-audit twin) use.
  3. rig-dev-handover-check.sh routes strih through the dantesync gate's --linux arm on strih-lx.
  4. recording-e2e.sh prints platform-correct [8/8a] / pull-back / cleanup text.
  5. the retired strih's AHK defaults (has_ahk=1 + D:\\_APPS\\NL_STARTUP.ahk) are gone.
  6. ONE authority for "is this the Linux strih": the obs-fleet list, alias-aware (case, a `.lan`
     suffix, a DNS name that resolves to the strih-lx address), and strih_platform delegates to it.
  7. the retired PC's NIC self-heal watcher is removed.

Tier-0: every check here runs bash functions directly or reads files -- no cargo, no rig.
"""
import os
import pathlib
import stat
import subprocess
import sys

import pytest

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib"
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))

# A resolver stub every bash run installs AFTER sourcing obs-fleet.sh, so no test ever depends on
# the box's DNS: only strih.lan (the retired box's own name, which really resolves to .202 on dev1)
# resolves. Every call is logged so a test can prove an IP literal is never sent to the resolver.
_RESOLVER_STUB = r'''
obs_fleet_resolve_host() {
  printf '%s\n' "$1" >>"${RESOLVE_LOG:-/dev/null}"
  case "$1" in strih.lan) printf '10.77.9.202' ;; esac
  return 0
}
'''


def _bash(body: str, env: dict | None = None, extra_path: str | None = None):
    full_env = {k: v for k, v in os.environ.items()
                if k not in ("STRIH_PLATFORM", "STRIH_LX_HOST", "OBS_FLEET", "OBS_FLEET_HOME")}
    if extra_path:
        full_env["PATH"] = extra_path + os.pathsep + full_env.get("PATH", "")
    full_env.update(env or {})
    return subprocess.run(["bash", "-c", body], capture_output=True, text=True, env=full_env,
                          timeout=60)


def _fleet(body: str, env: dict | None = None):
    return _bash(f'. "{LIB}/obs-fleet.sh"\n{_RESOLVER_STUB}\n{body}', env=env)


def _platform(body: str, env: dict | None = None):
    # strih-platform.sh sources obs-fleet.sh itself (the fleet list is the authority); the stub is
    # installed after both so it overrides the real getent seam.
    return _bash(f'. "{LIB}/strih-platform.sh"\n{_RESOLVER_STUB}\n{body}', env=env)


# --- item 6: one alias-aware authority --------------------------------------------------------------

@pytest.mark.parametrize("host", ["strih-lx.lan", "STRIH-LX", "Strih-Lx.lan", "strih.lan",
                                  "10.77.9.202", "strih-lx"])
def test_fleet_class_for_host_catches_every_alias_of_the_linux_strih(host):
    r = _fleet(f'obs_fleet_class_for_host "{host}"')
    assert r.returncode == 0 and r.stdout == "linux-genlock", (
        f"issue 1317: {host!r} names the Linux strih-lx and must read linux-genlock: {r!r}")
    r = _fleet(f'obs_fleet_refuse_linux_target "{host}" some-tool.sh')
    assert r.returncode == 1 and "some-tool.sh" in r.stderr, (
        f"issue 1317: a Windows tool aimed at {host!r} (the Linux strih) must be refused: {r!r}")


@pytest.mark.parametrize("host,want", [("10.77.9.204", "windows-genlock"),
                                       ("resolume.lan", "windows-genlock"),
                                       ("RESOLUME.lan", "windows-genlock")])
def test_fleet_class_for_host_keeps_the_windows_boxes(host, want):
    r = _fleet(f'obs_fleet_class_for_host "{host}"')
    assert r.stdout == want, f"{host!r}: {r!r}"


def test_unknown_names_still_pass_and_ip_literals_are_never_resolved(tmp_path):
    log = tmp_path / "resolve.log"
    r = _fleet('obs_fleet_class_for_host foo.example; echo "rc=$?"', env={"RESOLVE_LOG": str(log)})
    assert r.stdout.strip() == "rc=1", f"an unknown name has no class: {r!r}"
    r = _fleet("obs_fleet_refuse_linux_target foo.example t", env={"RESOLVE_LOG": str(log)})
    assert r.returncode == 0 and r.stderr == "", "an unknown name stays an authoritative ops target"
    log.write_text("")
    _fleet("obs_fleet_class_for_host 10.1.2.3; obs_fleet_class_for_host 10.77.9.204",
           env={"RESOLVE_LOG": str(log)})
    assert log.read_text() == "", "an IPv4 literal must never be sent to the DNS resolver"


@pytest.mark.parametrize("host,want", [
    ("10.77.9.202", "linux"), ("strih-lx.lan", "linux"), ("STRIH-LX", "linux"),
    ("strih.lan", "linux"), ("10.77.9.204", "windows"), ("192.0.2.10", "windows"), ("", "windows"),
])
def test_strih_platform_delegates_to_the_fleet_list(host, want):
    r = _platform(f'strih_platform "{host}"')
    assert r.stdout == want, (
        f"issue 1317: strih_platform must agree with the obs-fleet class for {host!r}: {r!r}")


def test_strih_platform_keeps_its_env_overrides():
    assert _platform("strih_platform strih-lx.lan", {"STRIH_PLATFORM": "windows"}).stdout == "windows"
    assert _platform("strih_platform 192.0.2.10", {"STRIH_PLATFORM": "linux"}).stdout == "linux"
    assert _platform("strih_platform 10.9.9.9", {"STRIH_LX_HOST": "10.9.9.9"}).stdout == "linux"


def test_python_fleet_reader_matches_the_bash_table(monkeypatch):
    import obs_fleet_table as t
    monkeypatch.delenv("OBS_FLEET", raising=False)
    names = [row[0] for row in t.fleet_rows()]
    r = _fleet('for n in strih-lx stream imag resolume; do obs_fleet_host "$n"; echo; done')
    assert names[:1] == ["strih-lx"] and "stream" in names, names
    assert [t.fleet_host(n) for n in ("strih-lx", "stream", "imag", "resolume")] == \
        r.stdout.splitlines(), "the python reader must read the SAME table as obs-fleet.sh"

    def stub(host):
        return "10.77.9.202" if host == "strih.lan" else ""

    for host in ["strih-lx.lan", "STRIH-LX", "strih.lan", "10.77.9.202", "10.77.9.204",
                 "resolume.lan", "foo.example", "10.1.2.3", ""]:
        bash = _fleet(f'obs_fleet_class_for_host "{host}"').stdout or None
        assert t.fleet_class_for_host(host, resolve=stub) == bash, (
            f"python/bash disagree on {host!r}")


# --- item 1: the mv-reverify strih-OBS restart routes by platform -----------------------------------

def _escalate(body: str, env: dict | None = None, extra_path: str | None = None):
    return _bash(f'HERE="{SCRIPTS}"\n. "{LIB}/mv-reverify-escalate.sh"\n{_RESOLVER_STUB}\n{body}',
                 env=env, extra_path=extra_path)


def test_linux_restart_cmd_checks_the_unit_then_restarts_it():
    r = _escalate("mv_reverify_obs_restart_linux_cmd")
    cmd = r.stdout
    assert r.returncode == 0 and cmd, r
    assert "systemctl --user" in cmd and "strih-obs.service" in cmd
    assert "restart --no-block strih-obs.service" in cmd, (
        "the restart must not block on the launcher's own :4455 wait -- mv_reverify_wait_obs_ws owns it")
    assert cmd.index("list-unit-files") < cmd.index("restart --no-block"), (
        "the unit must be proven installed BEFORE anything restarts OBS")
    assert "MV_REVERIFY_NO_UNIT" in cmd
    for win in ("powershell", "Stop-Process", "AutoHotkey", "EncodedCommand"):
        assert win not in cmd, f"no Windows text on the Linux restart: {win}"


def _fake_sshpass(tmp_path: pathlib.Path) -> str:
    bindir = tmp_path / "bin"
    bindir.mkdir()
    fake = bindir / "sshpass"
    fake.write_text("#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" >>\"$FAKE_ARGV\"\n"
                    "printf '%s\\n' \"${FAKE_OUT:-}\"\n")
    fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
    return str(bindir)


def test_restart_run_on_strih_lx_uses_plain_ssh_systemctl(tmp_path):
    argv = tmp_path / "argv"
    r = _escalate('mv_reverify_obs_restart_run 10.77.9.202; echo "rc=$?"',
                  env={"FAKE_ARGV": str(argv), "FAKE_OUT": "MV_REVERIFY_OBS_RESTART: ok"},
                  extra_path=_fake_sshpass(tmp_path))
    sent = argv.read_text()
    assert "rc=0" in r.stdout, r
    assert "systemctl --user" in sent and "strih-obs.service" in sent, sent
    assert "EncodedCommand" not in sent and "powershell" not in sent, (
        "issue 1317: a PowerShell program must never be sent to the Linux strih")


def test_restart_run_reports_a_missing_unit_as_not_performed(tmp_path):
    argv = tmp_path / "argv"
    r = _escalate('mv_reverify_obs_restart_run 10.77.9.202; echo "rc=$?"',
                  env={"FAKE_ARGV": str(argv), "FAKE_OUT": "MV_REVERIFY_NO_UNIT: missing"},
                  extra_path=_fake_sshpass(tmp_path))
    assert "rc=2" in r.stdout, f"a missing strih-obs.service means the restart was NOT performed: {r!r}"


def test_restart_run_on_a_windows_strih_keeps_the_powershell_program(tmp_path):
    argv = tmp_path / "argv"
    r = _escalate('mv_reverify_obs_restart_run 192.0.2.10; echo "rc=$?"',
                  env={"FAKE_ARGV": str(argv), "FAKE_OUT": "MV_REVERIFY_OBS_RESTART: ok"},
                  extra_path=_fake_sshpass(tmp_path))
    sent = argv.read_text()
    assert "rc=0" in r.stdout, r
    assert "EncodedCommand" in sent and "systemctl" not in sent, sent


def test_orchestrator_names_the_linux_unit_when_the_restart_is_impossible():
    text = (LIB / "mv-reverify-escalate.sh").read_text()
    orch = text[text.index("mv_reverify_or_escalate() {"):text.index("mv_reverify_resolve_wait() {")]
    assert "strih-obs.service" in orch, (
        "the escalation's restart-impossible message must name the Linux unit on strih-lx")


# --- item 3: the handover check routes strih-lx through the dantesync --linux arm --------------------

def test_dantesync_nodes_route_the_linux_strih_to_the_linux_arm():
    r = _platform("strih_dantesync_nodes linux 10.77.9.202 newlevel 10.77.9.204; echo; "
                  "strih_dantesync_nodes win 10.77.9.202 newlevel 10.77.9.204")
    assert r.stdout.splitlines() == ["strih=newlevel@10.77.9.202", "stream=newlevel@10.77.9.204"], r
    r = _platform("strih_dantesync_nodes linux 10.77.9.202 newlevel 10.77.9.204; echo; "
                  "strih_dantesync_nodes win 10.77.9.202 newlevel 10.77.9.204",
                  env={"STRIH_PLATFORM": "windows"})
    assert r.stdout.splitlines() == ["", "strih=newlevel@10.77.9.202 stream=newlevel@10.77.9.204"], r


def test_handover_check_uses_the_platform_routed_nodes():
    s = (SCRIPTS / "rig-dev-handover-check.sh").read_text()
    assert '--win "strih=' not in s, "issue 1317: the handover check must not hand strih-lx to --win"
    assert "strih_dantesync_nodes win" in s and "strih_dantesync_nodes linux" in s
    assert "lib/strih-platform.sh" in s


# --- item 4: recording-e2e platform-correct plan text ----------------------------------------------

def test_access_label_is_platform_correct():
    assert _platform("strih_access_label 10.77.9.202").stdout == "strih-lx, plain ssh/scp"
    assert _platform("strih_access_label 192.0.2.10").stdout == "win-strih"


def test_pullback_note_linux_names_scp_and_windows_is_the_original_text():
    args = "'/o/p.json' '/o/p-pixels' 'C:\\o\\p.json' 'C:\\o\\p-pixels'"
    lx = _platform(f"strih_partial_pullback_note 10.77.9.202 {args}").stdout
    assert "win-strih" not in lx and "FileDownload" not in lx and "scp" in lx and "/o/p.json" in lx
    win = _platform(f"strih_partial_pullback_note 192.0.2.10 {args}").stdout
    assert win == ("    pull back to dev1: /o/p.json  AND the #186 pixel-proof dir /o/p-pixels\n"
                   "      (win-strih FileDownload C:\\o\\p.json -> /o/p.json;\n"
                   "       win-strih FileDownload C:\\o\\p-pixels -> /o/p-pixels  [absent on a clean run])\n"), win


def test_linux_cleanup_note_is_an_exact_path_rm_never_a_sweep():
    r = _platform("strih_lx_recording_cleanup_note '  ' 10.77.9.202 \"/srv/_REC/2026-09-23 20-00-07.mkv\"")
    out = r.stdout
    assert out.startswith("  strih-lx ssh:"), out
    assert "rm -f -- '/srv/_REC/2026-09-23 20-00-07.mkv'" in out and "newlevel@10.77.9.202" in out
    assert "Remove-Item" not in out and "*" not in out and "-r" not in out.split("rm -f")[1]
    r = _platform("strih_lx_recording_cleanup_note '' 10.77.9.202 \"/srv/a'b.mkv\"")
    assert "rm -f -- '/srv/a'\\''b.mkv'" in r.stdout, r


def test_planner_holder_note_is_platform_correct():
    lx = _platform("strih_planner_holder_note 10.77.9.202").stdout
    assert "8/8a" in lx and "8/8b on stream" in lx and "strih-lx" in lx, lx
    win = _platform("strih_planner_holder_note 192.0.2.10").stdout
    assert win.startswith("    The win-* MCP holder runs 8/8a + 8/8b on strih+stream"), win


def test_recording_e2e_routes_every_strih_plan_line_through_the_platform():
    s = (SCRIPTS / "recording-e2e.sh").read_text()
    region = s[s.index("  run_strih_extract() {"):s.index("[8/8b-pre] PUSH")]
    assert "win-strih" not in region, (
        "issue 1317: the [8/8a] region must print no unconditional win-strih text")
    assert 'strih_partial_pullback_note "$STRIH"' in region
    assert 'strih_access_label "$STRIH"' in s
    assert 'strih_planner_holder_note "$STRIH"' in s
    assert "The win-* MCP holder runs 8/8a" not in s
    lines = s.splitlines()
    sites = [i for i, ln in enumerate(lines) if "win-strih Shell:" in ln]
    assert len(sites) == 3, sites
    for i in sites:
        window = "\n".join(lines[max(0, i - 4):i])
        assert "strih_lx_recording_cleanup_note" in window and "else" in window, (
            f"the win-strih cleanup line at {i + 1} must be the Windows branch of a platform split")


# --- item 5: the retired strih's AHK defaults are gone -----------------------------------------------

def test_no_retired_strih_ahk_default_remains():
    for rel in ("lib/ahk-watchdog.sh", "launch-obs-genlock.sh"):
        text = (SCRIPTS / rel).read_text()
        assert ":-D:\\\\_APPS" not in text, f"{rel} still defaults to the retired strih's AHK path"


def test_ahk_relaunch_without_a_script_fails_closed():
    r = _bash(f'. "{LIB}/ahk-watchdog.sh"; ahk_resolve_and_relaunch_ps')
    assert r.returncode != 0 and r.stdout == "" and "1317" in r.stderr, r


def test_launch_program_two_arg_default_carries_no_ahk():
    r = _bash(f'. "{SCRIPTS}/launch-obs-genlock.sh"; build_launch_program "C:\\\\obs" 0')
    assert r.returncode == 0 and "Stop-Process -Name AutoHotkey64" not in r.stdout, r.stderr
    r = _bash(f'. "{SCRIPTS}/launch-obs-genlock.sh"; build_launch_program "C:\\\\obs" 0 1')
    assert r.returncode != 0 and "1317" in r.stderr, r


# --- item 7: the retired PC's NIC self-heal watcher is removed ---------------------------------------

@pytest.mark.parametrize("rel", ["scripts/install-strih-nic-selfheal.ps1",
                                 "scripts/strih-nic-selfheal.ps1",
                                 "scripts/strih_nic_selfheal_decision.py",
                                 "tests/python/test_strih_nic_selfheal_1199.py",
                                 ".claude/rules/strih-nic-selfheal.md"])
def test_retired_nic_watcher_files_are_gone(rel):
    assert not (REPO / rel).exists(), f"issue 1317: {rel} belongs to the retired Windows strih PC"


def test_claude_md_no_longer_routes_to_the_nic_watcher():
    assert "strih-nic-selfheal.md" not in (REPO / "CLAUDE.md").read_text()
