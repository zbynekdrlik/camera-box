"""#1067 / #1222 — regression tests for scripts/bundle-state-server.py's port4455_owner()
resolving the :4455 listener's exe PATH and caching it.

#1067 (2026-08-15): the deployed BundleStateServer scheduled task runs non-elevated + hidden, so
port4455_owner()'s `Get-Process -Id <elevated obs>.Path` is access-denied -> `.Path` is null ->
both port4455_owner_path/_version were OMITTED (omit-when-empty) on the WHOLE live fleet, forcing
port4455_identity to stay opt-in in version-integrity-gate.sh. The fix resolves the path via the
elevation-independent `Get-CimInstance Win32_Process`.ExecutablePath (with Get-Process.Path kept as
a fallback), which is readable for an elevated process from a non-elevated caller.

#1222 ADDENDUM (2026-08-30): live evidence off strih's :8899 log showed port4455_owner()'s WMI/CIM
Win32_Process resolution regularly TIMING OUT at its 15s subprocess timeout ("WARNING: could not
read the :4455 port owner: Command [...] timed out after 15 seconds") on EVERY single
/bundle-state.json request -- a measured ~15s of the ~18.7s fresh-log gather baseline (issue-1222
comment). The fix caches the (path, version) result keyed by the CURRENT owning PID, read via a
separate, much CHEAPER Get-NetTCPConnection-only probe (no WMI) -- the same process staying on the
port (the overwhelmingly common case: OBS runs for hours) now costs one cheap PID probe instead of
the full WMI+VersionInfo round-trip; only a PID CHANGE (an OBS restart / a different process taking
the port) re-pays for the expensive resolution once, caching the fresh result under the new PID.
Never a guessed value: an unresolvable current PID (no listener, or even the cheap probe itself
failing) CLEARS the cache and returns ("", "") rather than serving a stale identity for a port
nobody currently proves to own.

The real "does CIM ExecutablePath actually read the elevated obs64 path in the deployed task
context" is a LIVE-Windows-box property (no powershell / no live OBS here) -- that is the
supervisor's redeploy + `curl :8899/bundle-state.json` confirmation step. What these tests lock
LOCALLY is the WIRING (the PowerShell round-trip uses the WMI/CIM ExecutablePath resolution, the
cheap PID probe never touches WMI, and the caching decision) with subprocess.run faked so no real
PowerShell runs.

Same "source parsers, verify live separately" split as test_bundle_state_gather.py -- but
bundle-state-server.py is hyphenated (not a normal import name) and __main__-guarded, so it is
loaded by file path via importlib without starting the HTTP server (mirrors
test_bundle_state_server_log.py).
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


def _reset_port4455_cache():
    # The #1222 cache is module-level (mirrors _State's own lock-guarded pattern in this file) --
    # reset it before every test so tests don't leak state into each other.
    bss._port4455_cache["pid"] = None
    bss._port4455_cache["path"] = ""
    bss._port4455_cache["version"] = ""


def _is_pid_probe(cmd):
    ps = " ".join(cmd)
    return "OwningProcess" in ps and "Win32_Process" not in ps


def test_port4455_owner_resolves_path_via_wmi_executablepath(monkeypatch):
    # THE #1067 fix: the PowerShell round-trip must resolve the owner's exe path via the
    # elevation-independent WMI/CIM Win32_Process.ExecutablePath, not (only) the access-denied
    # Get-Process.Path. RED before the fix (the old command references neither token).
    _reset_port4455_cache()
    captured = {}

    def fake_run(cmd, **_kwargs):
        if _is_pid_probe(cmd):
            return types.SimpleNamespace(stdout="4321\n")
        captured["cmd"] = cmd
        return types.SimpleNamespace(stdout="")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    bss.port4455_owner()
    ps = " ".join(captured["cmd"])
    assert "Win32_Process" in ps, f"must resolve via WMI/CIM Win32_Process: {ps}"
    assert "ExecutablePath" in ps, f"must read ExecutablePath (elevation-independent): {ps}"


def test_port4455_owner_returns_path_and_version_from_output(monkeypatch):
    # The (path, version) parse contract is unchanged: two non-empty stdout lines -> (path, version).
    _reset_port4455_cache()

    def fake_run(cmd, **_kwargs):
        if _is_pid_probe(cmd):
            return types.SimpleNamespace(stdout="4321\n")
        return types.SimpleNamespace(stdout=f"{PINNED_EXE}\n32.1.2\n")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    assert bss.port4455_owner() == (PINNED_EXE, "32.1.2")


def test_port4455_owner_empty_on_no_listener(monkeypatch):
    # No listener on :4455 -> the cheap PID probe itself returns empty stdout -> ("", "")
    # immediately, WITHOUT ever paying for the expensive WMI resolution.
    _reset_port4455_cache()
    calls = []

    def fake_run(cmd, **_kw):
        calls.append(cmd)
        return types.SimpleNamespace(stdout="")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    assert bss.port4455_owner() == ("", "")
    assert len(calls) == 1, "no listener -> must not pay for the expensive WMI resolution"
    assert _is_pid_probe(calls[0])


def test_port4455_owner_path_only_yields_empty_version(monkeypatch):
    # Path resolved but no version line (Get-Item VersionInfo empty) -> (path, "") -- never a guess.
    _reset_port4455_cache()

    def fake_run(cmd, **_kw):
        if _is_pid_probe(cmd):
            return types.SimpleNamespace(stdout="4321\n")
        return types.SimpleNamespace(stdout=f"{PINNED_EXE}\n")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    assert bss.port4455_owner() == (PINNED_EXE, "")


def test_port4455_owner_empty_on_subprocess_error(monkeypatch):
    # A PowerShell failure (incl. the cheap PID probe itself) must degrade to ("", ""), never
    # crash the /bundle-state.json response.
    _reset_port4455_cache()

    def boom(cmd, **_kw):
        raise bss.subprocess.SubprocessError("powershell blew up")

    monkeypatch.setattr(bss.subprocess, "run", boom)
    assert bss.port4455_owner() == ("", "")


# ---------------------------------------------------------------------------------------------
# #1222 -- the caching behavior: an unchanged owning PID must NOT re-pay for the expensive
# WMI/VersionInfo resolution; a CHANGED PID must re-resolve and cache the fresh identity; a
# vanished listener must never keep serving a stale identity.
# ---------------------------------------------------------------------------------------------

def test_port4455_owner_reuses_cached_identity_when_pid_unchanged(monkeypatch):
    _reset_port4455_cache()
    full_calls = []

    def fake_run(cmd, **_kw):
        if _is_pid_probe(cmd):
            return types.SimpleNamespace(stdout="4321\n")
        full_calls.append(cmd)
        return types.SimpleNamespace(stdout=f"{PINNED_EXE}\n32.1.2\n")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    first = bss.port4455_owner()
    second = bss.port4455_owner()
    assert first == (PINNED_EXE, "32.1.2")
    assert second == first
    assert len(full_calls) == 1, (
        "the expensive WMI resolution must run only ONCE for an unchanged PID (#1222)"
    )


def test_port4455_owner_re_resolves_when_pid_changes(monkeypatch):
    _reset_port4455_cache()
    state = {"pid": "4321", "path": PINNED_EXE, "version": "32.1.2"}
    full_calls = []

    def fake_run(cmd, **_kw):
        if _is_pid_probe(cmd):
            return types.SimpleNamespace(stdout=state["pid"] + "\n")
        full_calls.append(cmd)
        return types.SimpleNamespace(stdout=f"{state['path']}\n{state['version']}\n")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    first = bss.port4455_owner()
    assert first == (PINNED_EXE, "32.1.2")

    # A different process now owns the port (e.g. OBS restarted into a new build).
    state["pid"] = "9999"
    state["path"] = r"C:\Program Files\obs-studio\bin\64bit\obs64_new.exe"
    state["version"] = "33.0.0"
    second = bss.port4455_owner()
    assert second == (state["path"], state["version"])
    assert len(full_calls) == 2, "a PID change must re-run the expensive WMI resolution"


def test_port4455_owner_never_serves_a_stale_identity_when_listener_disappears(monkeypatch):
    _reset_port4455_cache()

    def fake_run_present(cmd, **_kw):
        if _is_pid_probe(cmd):
            return types.SimpleNamespace(stdout="4321\n")
        return types.SimpleNamespace(stdout=f"{PINNED_EXE}\n32.1.2\n")

    monkeypatch.setattr(bss.subprocess, "run", fake_run_present)
    assert bss.port4455_owner() == (PINNED_EXE, "32.1.2")

    # The listener disappears entirely (no PID at all) -> must NOT keep serving the old identity.
    monkeypatch.setattr(bss.subprocess, "run", lambda cmd, **_kw: types.SimpleNamespace(stdout=""))
    assert bss.port4455_owner() == ("", "")


def test_port4455_owning_pid_probe_never_queries_wmi(monkeypatch):
    # The cheap PID-only probe must stay cheap -- it must never touch Win32_Process/CIM (that is
    # exactly the half of the old single command the live #1222 evidence blamed for the timeout).
    captured = {}

    def fake_run(cmd, **_kw):
        captured["cmd"] = cmd
        return types.SimpleNamespace(stdout="4321\n")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    assert bss._port4455_owning_pid() == "4321"
    ps = " ".join(captured["cmd"])
    assert "Win32_Process" not in ps
    assert "OwningProcess" in ps


def test_port4455_owning_pid_probe_empty_on_subprocess_error(monkeypatch):
    def boom(cmd, **_kw):
        raise bss.subprocess.SubprocessError("powershell blew up")

    monkeypatch.setattr(bss.subprocess, "run", boom)
    assert bss._port4455_owning_pid() == ""
