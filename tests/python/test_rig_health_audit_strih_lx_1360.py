"""issue 1360 part 2 -- rig-health-audit.py (the issue-787 status-page feeder) reads the strih box the
platform-resolved way.

Since the M4 cut-over the strih role is the Linux notebook strih-lx (10.77.9.202): its OBS log is the
newest `~/.config/obs-studio/logs/*.txt` (a SPACED filename) and its OBS process is `obs`, not
`obs64`. The audit only knew the Windows `-EncodedCommand` PowerShell reads, so against strih-lx the
process count came back None -> the strih status-page row read `FAIL unreachable over ssh` on a
healthy box, and the CG-chain row skipped with "strih OBS log unreadable".

Python cannot source scripts/lib/strih-log-read.sh / strih-platform.sh, so the audit carries a TWIN
of the two decisions it needs, pinned here byte-for-byte to the bash originals by running them:
  * strih_platform(host)        == scripts/lib/strih-platform.sh `strih_platform`
  * _obs_log_tail_cmd(ip, n)    == `strih_log_remote_cmd <platform> headtail <n>` (both platforms)
and the Linux command is exercised for real against a fixture HOME (newest spaced log, head + tail).
"""
import importlib.util
import os
import subprocess
from pathlib import Path

import pytest

HERE = Path(__file__).parent
REPO = HERE.parent.parent
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib"


def _load_module():
    spec = importlib.util.spec_from_file_location(
        "rig_health_audit_1360", SCRIPTS / "rig-health-audit.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_mod = _load_module()


def _bash(script: str, *args: str, env: dict | None = None) -> str:
    """Run a bash program (file-less, argv-passed) and return its stdout."""
    full_env = {k: v for k, v in os.environ.items() if k not in ("STRIH_PLATFORM", "STRIH_LX_HOST")}
    full_env.update(env or {})
    out = subprocess.run(["bash", "-c", script, "bash", *args],
                         capture_output=True, text=True, env=full_env, timeout=20)
    assert out.returncode == 0, f"bash failed rc={out.returncode}: {out.stderr}"
    return out.stdout


def _bash_platform(host: str, env: dict | None = None) -> str:
    return _bash('. "$1"; strih_platform "$2"', str(LIB / "strih-platform.sh"), host, env=env)


def _bash_remote_cmd(platform: str, op: str, arg: str) -> str:
    return _bash('. "$1"; strih_log_remote_cmd "$2" "$3" "$4"',
                 str(LIB / "strih-log-read.sh"), platform, op, arg)


# --- platform resolution twin ---------------------------------------------------------------------

@pytest.mark.parametrize("host,env", [
    ("10.77.9.202", {}),
    ("10.77.9.204", {}),
    ("", {}),
    ("10.0.0.1", {"STRIH_PLATFORM": "linux"}),
    ("10.77.9.202", {"STRIH_PLATFORM": "windows"}),
    ("10.77.9.202", {"STRIH_PLATFORM": "bogus"}),
    ("10.9.9.9", {"STRIH_LX_HOST": "10.9.9.9"}),
])
def test_strih_platform_twin_matches_bash_1360(monkeypatch, host, env):
    monkeypatch.delenv("STRIH_PLATFORM", raising=False)
    monkeypatch.delenv("STRIH_LX_HOST", raising=False)
    for k, v in env.items():
        monkeypatch.setenv(k, v)
    assert _mod.strih_platform(host) == _bash_platform(host, env), (
        f"issue 1360: the python strih_platform twin must agree with strih-platform.sh for {host!r} {env}")


# --- log command twin, both platforms ------------------------------------------------------------

@pytest.mark.parametrize("tail", [500, 1, 1200])
def test_linux_log_tail_cmd_is_byte_identical_to_the_bash_reader_1360(monkeypatch, tail):
    monkeypatch.delenv("STRIH_PLATFORM", raising=False)
    monkeypatch.delenv("STRIH_LX_HOST", raising=False)
    py = _mod._obs_log_tail_cmd(_mod.STRIH, tail)
    sh = _bash_remote_cmd("linux", "headtail", str(tail))
    assert py == sh and sh, (
        f"issue 1360: the strih-lx head+tail read must be the shared reader's command verbatim\n"
        f"python: {py!r}\nbash:   {sh!r}")
    assert "powershell" not in py and "APPDATA" not in py


def test_windows_log_tail_cmd_is_byte_identical_to_the_bash_reader_1360(monkeypatch):
    monkeypatch.delenv("STRIH_PLATFORM", raising=False)
    py = _mod._obs_log_tail_cmd(_mod.STREAM, 500)
    assert py == _mod._windows_obs_log_tail_cmd(500), (
        "issue 1360: a Windows box (stream) keeps the audit's existing head-600 + tail read")
    assert py == _bash_remote_cmd("windows", "headtail", "500"), (
        "issue 1360: the bash reader's Windows headtail op must be the audit's PowerShell verbatim")


def test_log_tail_cmd_clamps_a_non_numeric_count_1360(monkeypatch):
    monkeypatch.delenv("STRIH_PLATFORM", raising=False)
    py = _mod._obs_log_tail_cmd(_mod.STRIH, "5; rm -rf /")
    assert "rm -rf" not in py and 'tail -n 500 "$F"' in py, py


def test_linux_obs_count_cmd_counts_live_obs_processes_1360(monkeypatch):
    monkeypatch.delenv("STRIH_PLATFORM", raising=False)
    cmd = _mod._obs_count_cmd(_mod.STRIH)
    assert "powershell" not in cmd and "obs64" not in cmd, cmd
    assert _mod._obs_count_cmd(_mod.STREAM) == _mod._windows_obs_count_cmd()


# --- the Linux commands run for real against a fixture HOME --------------------------------------

def _fixture_home(tmp_path: Path) -> Path:
    logs = tmp_path / "home" / ".config" / "obs-studio" / "logs"
    logs.mkdir(parents=True)
    old = logs / "2026-09-22 08-00-00.txt"
    old.write_text("OLD-LOG-LINE\n")
    new = logs / "2026-09-23 09-23-36.txt"
    new.write_text("".join(f"line-{i}\n" for i in range(1, 1001)))
    os.utime(old, (1_000_000, 1_000_000))
    os.utime(new, (2_000_000, 2_000_000))
    return tmp_path / "home"


def test_linux_log_tail_cmd_reads_head_600_and_tail_of_the_newest_log_1360(tmp_path, monkeypatch):
    monkeypatch.delenv("STRIH_PLATFORM", raising=False)
    home = _fixture_home(tmp_path)
    out = subprocess.run(["bash", "-c", _mod._obs_log_tail_cmd(_mod.STRIH, 5)],
                         capture_output=True, text=True, env={**os.environ, "HOME": str(home)},
                         timeout=20).stdout.splitlines()
    assert out[:2] == ["line-1", "line-2"], "the head (launch-time audio buffering) must be read"
    assert out[599] == "line-600" and out[600:] == [f"line-{i}" for i in range(996, 1001)], (
        f"head 600 then the last 5 lines of the NEWEST (spaced-name) log: {out[595:]}")
    assert "OLD-LOG-LINE" not in out


def _run_count_with_ps_stub(tmp_path: Path, ps_body: str) -> subprocess.CompletedProcess:
    """Run the Linux count command with a PATH-stubbed `ps` that logs its argv and prints the given
    `stat` column (one process per line)."""
    stub_dir = tmp_path / "bin"
    stub_dir.mkdir()
    (stub_dir / "ps").write_text(
        "#!/usr/bin/env bash\n"
        f'printf "%s\\n" "$*" > "{tmp_path}/ps.argv"\n'
        f"printf '{ps_body}'\n")
    (stub_dir / "ps").chmod(0o755)
    env = {**os.environ, "PATH": f"{stub_dir}:{os.environ['PATH']}"}
    return subprocess.run(["bash", "-c", _mod._obs_count_cmd(_mod.STRIH)],
                          capture_output=True, text=True, env=env, timeout=20)


def test_linux_obs_count_cmd_counts_live_obs_only_1360(tmp_path, monkeypatch):
    monkeypatch.delenv("STRIH_PLATFORM", raising=False)
    out = _run_count_with_ps_stub(tmp_path, "S\\nZ\\nSl\\n")
    assert out.returncode == 0 and out.stdout.strip() == "2", (
        f"two live obs processes + one zombie must count 2: {out!r}")
    argv = (tmp_path / "ps.argv").read_text().split()
    assert argv[:2] == ["-C", "obs"], (
        f"the count must match the exact `obs` comm (never obs-browser-pag): {argv}")


def test_linux_obs_count_cmd_prints_zero_when_obs_is_down_1360(tmp_path, monkeypatch):
    monkeypatch.delenv("STRIH_PLATFORM", raising=False)
    out = _run_count_with_ps_stub(tmp_path, "")
    assert out.returncode == 0 and out.stdout.strip() == "0", (
        f"OBS down must read 0 and exit 0 (never an ssh failure): {out!r}")


# --- the strih row + the CG-chain row use the platform-resolved commands --------------------------

def test_check_windows_box_strih_uses_the_linux_reads_on_strih_lx_1360(monkeypatch, capsys):
    monkeypatch.delenv("STRIH_PLATFORM", raising=False)
    calls: list[str] = []

    def fake_ssh(host, cmd, user="root", timeout=20, pw=_mod.SSH_PW):
        calls.append(cmd)
        if cmd == _mod._obs_count_cmd(_mod.STRIH):
            return "1\n"
        return "12:00:00.000: genlock-fifo audit 'NDI cam1': received=100\n"

    monkeypatch.setattr(_mod, "ssh", fake_ssh)
    monkeypatch.setattr(_mod, "obs_ws_stats", lambda *a, **k: None)
    monkeypatch.setattr(_mod, "http_get", lambda *a, **k: None)
    _mod.results.clear()
    _mod.check_windows_box("strih", _mod.STRIH, None, program_fps=30.0, expect_latency=False)
    assert calls and all("powershell" not in c for c in calls), (
        f"issue 1360: the strih-lx row must never be read the PowerShell way: {calls}")
    row = capsys.readouterr().out
    assert "unreachable over ssh" not in row and "obs64=1" in row and "arrivals[" in row, (
        f"issue 1360: a healthy strih-lx must render its real OBS count + log facets: {row!r}")


def test_cg_chain_reads_strih_through_the_platform_resolved_tail_1360(monkeypatch):
    monkeypatch.delenv("STRIH_PLATFORM", raising=False)
    seen: dict[str, str] = {}

    def fake_ssh(host, cmd, user="root", timeout=20, pw=_mod.SSH_PW):
        seen[host] = cmd
        return None  # unreadable -> the report-only NOTE row, no verdict tool run

    monkeypatch.setattr(_mod, "ssh", fake_ssh)
    _mod.results.clear()
    _mod.check_cg_chain()
    assert seen.get(_mod.STRIH) == _mod._obs_log_tail_cmd(_mod.STRIH, 500), seen
    assert "powershell" not in seen[_mod.STRIH]
