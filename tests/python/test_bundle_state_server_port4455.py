"""#1067 — regression tests for scripts/bundle-state-server.py's port4455_owner() resolving the
:4455 listener's exe PATH from a NON-elevated context.

Live finding (2026-08-15): the deployed BundleStateServer scheduled task runs non-elevated + hidden,
so port4455_owner()'s `Get-Process -Id <elevated obs>.Path` is access-denied -> `.Path` is null ->
both port4455_owner_path/_version were OMITTED (omit-when-empty) on the WHOLE live fleet, forcing
port4455_identity to stay opt-in in version-integrity-gate.sh. The fix resolves the path via the
elevation-independent `Get-CimInstance Win32_Process`.ExecutablePath (with Get-Process.Path kept as
a fallback), which is readable for an elevated process from a non-elevated caller.

The real "does CIM ExecutablePath actually read the elevated obs64 path in the deployed task
context" is a LIVE-Windows-box property (no powershell / no live OBS here) — that is the supervisor's
redeploy + `curl :8899/bundle-state.json` confirmation step. What these tests lock LOCALLY is the
WIRING (the PowerShell round-trip uses the WMI/CIM ExecutablePath resolution) and the unchanged
(path, version) parse / degrade contract, with subprocess.run faked so no real PowerShell runs.

Same "source parsers, verify live separately" split as test_bundle_state_gather.py — but
bundle-state-server.py is hyphenated (not a normal import name) and __main__-guarded, so it is loaded
by file path via importlib without starting the HTTP server (mirrors test_bundle_state_server_log.py).
"""
import importlib.util
import pathlib
import sys
import types

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
# bundle-state-server.py does `import bundle_state_gather` at module scope, so scripts/ must be
# importable before we exec it.
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

_SPEC = importlib.util.spec_from_file_location(
    "bundle_state_server", _SCRIPTS / "bundle-state-server.py"
)
bss = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(bss)  # __name__ != "__main__" -> main()/serve_forever() does NOT run

PINNED_EXE = r"C:\Program Files\obs-studio\bin\64bit\obs64.exe"


def test_port4455_owner_resolves_path_via_wmi_executablepath(monkeypatch):
    # THE #1067 fix: the PowerShell round-trip must resolve the owner's exe path via the
    # elevation-independent WMI/CIM Win32_Process.ExecutablePath, not (only) the access-denied
    # Get-Process.Path. RED before the fix (the old command references neither token).
    captured = {}

    def fake_run(cmd, **_kwargs):
        captured["cmd"] = cmd
        return types.SimpleNamespace(stdout="")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    bss.port4455_owner()
    ps = " ".join(captured["cmd"])
    assert "Win32_Process" in ps, f"must resolve via WMI/CIM Win32_Process: {ps}"
    assert "ExecutablePath" in ps, f"must read ExecutablePath (elevation-independent): {ps}"


def test_port4455_owner_returns_path_and_version_from_output(monkeypatch):
    # The (path, version) parse contract is unchanged: two non-empty stdout lines -> (path, version).
    def fake_run(cmd, **_kwargs):
        return types.SimpleNamespace(stdout=f"{PINNED_EXE}\n32.1.2\n")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    assert bss.port4455_owner() == (PINNED_EXE, "32.1.2")


def test_port4455_owner_empty_on_no_listener(monkeypatch):
    # No listener on :4455 (empty stdout) -> ("", "") — omit-when-empty downstream, never a guess.
    monkeypatch.setattr(
        bss.subprocess, "run", lambda cmd, **_kw: types.SimpleNamespace(stdout="")
    )
    assert bss.port4455_owner() == ("", "")


def test_port4455_owner_path_only_yields_empty_version(monkeypatch):
    # Path resolved but no version line (Get-Item VersionInfo empty) -> (path, "") — never a guess.
    monkeypatch.setattr(
        bss.subprocess, "run", lambda cmd, **_kw: types.SimpleNamespace(stdout=f"{PINNED_EXE}\n")
    )
    assert bss.port4455_owner() == (PINNED_EXE, "")


def test_port4455_owner_empty_on_subprocess_error(monkeypatch):
    # A PowerShell failure must degrade to ("", ""), never crash the /bundle-state.json response.
    def boom(cmd, **_kw):
        raise bss.subprocess.SubprocessError("powershell blew up")

    monkeypatch.setattr(bss.subprocess, "run", boom)
    assert bss.port4455_owner() == ("", "")
