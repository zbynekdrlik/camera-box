"""#1067 / #1222 / #1222b -- regression tests for scripts/bundle-state-server.py's
port4455_owner() resolving the :4455 listener's exe PATH and caching it.

#1067 (2026-08-15): the deployed BundleStateServer scheduled task runs non-elevated + hidden, so
port4455_owner()'s `Get-Process -Id <elevated obs>.Path` is access-denied -> `.Path` is null ->
both port4455_owner_path/_version were OMITTED (omit-when-empty) on the WHOLE live fleet, forcing
port4455_identity to stay opt-in in version-integrity-gate.sh. The fix resolves the path via the
elevation-independent `Get-CimInstance Win32_Process`.ExecutablePath (with Get-Process.Path kept as
a fallback), which is readable for an elevated process from a non-elevated caller.

#1222 (2026-08-30): live evidence off strih's :8899 log showed port4455_owner()'s WMI/CIM
Win32_Process resolution regularly TIMING OUT at its 15s subprocess timeout ("WARNING: could not
read the :4455 port owner: Command [...] timed out after 15 seconds") on EVERY single
/bundle-state.json request -- a measured ~15s of the ~18.7s fresh-log gather baseline (issue-1222
comment). The fix caches the (path, version) result keyed by the CURRENT owning PID, read via a
separate, much CHEAPER PID-only probe -- the same process staying on the port (the overwhelmingly
common case: OBS runs for hours) now costs one cheap PID probe instead of the full WMI+VersionInfo
round-trip; only a PID CHANGE (an OBS restart / a different process taking the port) re-pays for
the expensive resolution once, caching the fresh result under the new PID. Never a guessed value:
an unresolvable current PID (no listener, or even the cheap probe itself failing) CLEARS the cache
and returns ("", "") rather than serving a stale identity for a port nobody currently proves to own.

#1222b (2026-08-30, same day, live redeploy follow-up): the cheap PID-only probe's FIRST
implementation still shelled out to PowerShell (`Get-NetTCPConnection`); live post-deploy timing on
strih showed that command alone costing ~4.1s plus PowerShell's own interpreter cold-start
(~5-10s under load), so the "cheap" probe still cost ~10-15s per request there and the cache never
got a chance to help (stream, a lighter-loaded box, dropped to 1.3-1.8s while strih stayed at
18.2-18.7s). Fixed by replacing the probe with a `netstat -ano -p tcp` subprocess call -- a native
Windows tool with no interpreter startup cost -- parsed by the new PURE `_parse_netstat_listening_pid()`
for the row whose local address ends with the target port and whose state is LISTENING. Same
`_port4455_owning_pid()` signature and "" on-failure/no-listener contract, so port4455_owner()'s
cache logic (RESOLVED entirely by #1222 above) is completely untouched.

The real "does this actually resolve the elevated obs64 path / run fast on the deployed task" is a
LIVE-Windows-box property (no powershell / no netstat / no live OBS here) -- that is the
supervisor's redeploy + `curl :8899/bundle-state.json` confirmation step. What these tests lock
LOCALLY is the WIRING (the full resolution still uses WMI/CIM ExecutablePath, the cheap probe now
uses netstat and never touches PowerShell/WMI at all, the netstat output parser, and the caching
decision) with subprocess.run faked so no real PowerShell or netstat runs.

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
    # #1222b: the cheap probe is now a plain `netstat` invocation (no PowerShell at all).
    return bool(cmd) and cmd[0] == "netstat"


def _netstat_line(pid, port=4455, local_ip="0.0.0.0", state="LISTENING"):
    # A realistic single `netstat -ano -p tcp` TCP row -- 5 whitespace-separated columns
    # (Proto, Local Address, Foreign Address, State, PID), matching the live strih format
    # confirmed 2026-08-30: "  TCP    0.0.0.0:8899           0.0.0.0:0              LISTENING       9648".
    return f"  TCP    {local_ip}:{port}           0.0.0.0:0              {state}       {pid}\n"


def test_port4455_owner_resolves_path_via_wmi_executablepath(monkeypatch):
    # THE #1067 fix: the PowerShell round-trip must resolve the owner's exe path via the
    # elevation-independent WMI/CIM Win32_Process.ExecutablePath, not (only) the access-denied
    # Get-Process.Path. RED before the fix (the old command references neither token).
    _reset_port4455_cache()
    captured = {}

    def fake_run(cmd, **_kwargs):
        if _is_pid_probe(cmd):
            return types.SimpleNamespace(stdout=_netstat_line("4321"))
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
            return types.SimpleNamespace(stdout=_netstat_line("4321"))
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
            return types.SimpleNamespace(stdout=_netstat_line("4321"))
        return types.SimpleNamespace(stdout=f"{PINNED_EXE}\n")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    assert bss.port4455_owner() == (PINNED_EXE, "")


def test_port4455_owner_empty_on_subprocess_error(monkeypatch):
    # A subprocess failure (incl. the cheap PID probe itself) must degrade to ("", ""), never
    # crash the /bundle-state.json response.
    _reset_port4455_cache()

    def boom(cmd, **_kw):
        raise bss.subprocess.SubprocessError("subprocess blew up")

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
            return types.SimpleNamespace(stdout=_netstat_line("4321"))
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
            return types.SimpleNamespace(stdout=_netstat_line(state["pid"]))
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
            return types.SimpleNamespace(stdout=_netstat_line("4321"))
        return types.SimpleNamespace(stdout=f"{PINNED_EXE}\n32.1.2\n")

    monkeypatch.setattr(bss.subprocess, "run", fake_run_present)
    assert bss.port4455_owner() == (PINNED_EXE, "32.1.2")

    # The listener disappears entirely (no PID at all) -> must NOT keep serving the old identity.
    monkeypatch.setattr(bss.subprocess, "run", lambda cmd, **_kw: types.SimpleNamespace(stdout=""))
    assert bss.port4455_owner() == ("", "")


def test_port4455_owning_pid_probe_never_queries_wmi(monkeypatch):
    # The cheap PID-only probe must stay cheap -- it must never touch WMI/CIM at all (that is
    # exactly the half of the old single command the live #1222 evidence blamed for the timeout).
    captured = {}

    def fake_run(cmd, **_kw):
        captured["cmd"] = cmd
        return types.SimpleNamespace(stdout=_netstat_line("4321"))

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    assert bss._port4455_owning_pid() == "4321"
    ps = " ".join(captured["cmd"])
    assert "Win32_Process" not in ps


def test_port4455_owning_pid_probe_empty_on_subprocess_error(monkeypatch):
    def boom(cmd, **_kw):
        raise bss.subprocess.SubprocessError("subprocess blew up")

    monkeypatch.setattr(bss.subprocess, "run", boom)
    assert bss._port4455_owning_pid() == ""


# ---------------------------------------------------------------------------------------------
# #1222 fable review findings -- two cache-lifecycle holes on degenerate paths that contradicted
# the docstring's own "never serves a stale identity / clears the cache" promise.
# ---------------------------------------------------------------------------------------------

def test_port4455_owner_does_not_cache_an_empty_path_result(monkeypatch):
    # A full resolve that SUCCEEDS (exit 0) with EMPTY stdout (the #1067 access-denied shape, or
    # a transient CIM flake) must not be cached -- every later request must keep retrying instead
    # of serving ("", "") for the rest of the OBS session with no chance of recovery.
    _reset_port4455_cache()
    full_calls = []

    def fake_run(cmd, **_kw):
        if _is_pid_probe(cmd):
            return types.SimpleNamespace(stdout=_netstat_line("4321"))
        full_calls.append(cmd)
        return types.SimpleNamespace(stdout="")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    first = bss.port4455_owner()
    second = bss.port4455_owner()
    assert first == ("", "")
    assert second == ("", "")
    assert len(full_calls) == 2, (
        "an empty successful resolution must NOT be cached -- every request must keep retrying"
    )


def test_port4455_owner_clears_cache_when_full_resolve_raises(monkeypatch):
    # A failed full resolve must clear the cache, so a LATER pid reuse (Windows recycles PIDs)
    # cannot serve a stale identity resolved before the failure.
    _reset_port4455_cache()
    state = {"pid": "4321", "mode": "ok", "path": PINNED_EXE, "version": "32.1.2"}

    def fake_run(cmd, **_kw):
        if _is_pid_probe(cmd):
            return types.SimpleNamespace(stdout=_netstat_line(state["pid"]))
        if state["mode"] == "boom":
            raise bss.subprocess.SubprocessError("subprocess blew up")
        return types.SimpleNamespace(stdout=f"{state['path']}\n{state['version']}\n")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    first = bss.port4455_owner()
    assert first == (PINNED_EXE, "32.1.2")

    # A different process (pid 9999) takes the port and its full resolve fails.
    state["pid"] = "9999"
    state["mode"] = "boom"
    second = bss.port4455_owner()
    assert second == ("", "")

    # PID 4321 is REUSED by a totally different process (a real Windows possibility). The cache
    # must NOT still say "pid 4321 -> the earlier PINNED_EXE/32.1.2" from before the failure.
    state["pid"] = "4321"
    state["mode"] = "ok"
    state["path"] = r"C:\Program Files\obs-studio\bin\64bit\obs64_DIFFERENT.exe"
    state["version"] = "34.0.0"
    third = bss.port4455_owner()
    assert third == (state["path"], state["version"]), (
        "a failed resolve must clear the cache so a later PID reuse re-resolves instead of "
        "serving a stale cached identity from before the failure"
    )


# ---------------------------------------------------------------------------------------------
# #1222b -- the cheap probe is now netstat, not PowerShell (live strih timing: the PowerShell
# probe itself cost ~10-15s under load, defeating its own purpose). RED against the pre-fix
# probe: it invokes powershell and returns raw stdout with no netstat-shaped parsing at all.
# ---------------------------------------------------------------------------------------------

def test_port4455_owning_pid_probe_uses_netstat_not_powershell(monkeypatch):
    captured = {}

    def fake_run(cmd, **_kw):
        captured["cmd"] = cmd
        return types.SimpleNamespace(stdout=_netstat_line("9648"))

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    assert bss._port4455_owning_pid() == "9648"
    cmd = captured["cmd"]
    assert "netstat" in cmd, f"the cheap probe must use netstat, not PowerShell: {cmd}"
    assert "powershell" not in cmd, f"the cheap probe must never spawn PowerShell: {cmd}"


NETSTAT_SAMPLE = """
Active Connections

  Proto  Local Address          Foreign Address        State           PID
  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1234
  TCP    0.0.0.0:445            0.0.0.0:0              LISTENING       4
  TCP    0.0.0.0:8899           0.0.0.0:0              LISTENING       9648
  TCP    10.77.9.202:4455       0.0.0.0:0              LISTENING       5678
  TCP    10.77.9.202:54321      10.77.9.10:4455        ESTABLISHED     8888
  UDP    0.0.0.0:5353           *:*                                    2222
"""


def test_parse_netstat_listening_pid_finds_the_correct_row():
    assert bss._parse_netstat_listening_pid(NETSTAT_SAMPLE, port=4455) == "5678"


def test_parse_netstat_listening_pid_ignores_foreign_address_match():
    # :4455 appears in the FOREIGN address column of an ESTABLISHED row on a totally different
    # local port -- must never be mistaken for the local :4455 LISTENING row.
    text = "  TCP    10.77.9.202:54321      10.77.9.10:4455        ESTABLISHED     8888\n"
    assert bss._parse_netstat_listening_pid(text, port=4455) == ""


def test_parse_netstat_listening_pid_ignores_a_port_suffix_collision():
    # :44551 must never satisfy an endswith(":4455") style check.
    text = "  TCP    0.0.0.0:44551          0.0.0.0:0              LISTENING       7777\n"
    assert bss._parse_netstat_listening_pid(text, port=4455) == ""


def test_parse_netstat_listening_pid_ignores_non_listening_state():
    text = "  TCP    0.0.0.0:4455           0.0.0.0:0              CLOSE_WAIT      7777\n"
    assert bss._parse_netstat_listening_pid(text, port=4455) == ""


def test_parse_netstat_listening_pid_ignores_udp_rows():
    text = "  UDP    0.0.0.0:4455           *:*                                    7777\n"
    assert bss._parse_netstat_listening_pid(text, port=4455) == ""


def test_parse_netstat_listening_pid_no_listener_is_empty():
    assert bss._parse_netstat_listening_pid("", port=4455) == ""
    banner_only = "Active Connections\n\n  Proto  Local Address  Foreign Address  State  PID\n"
    assert bss._parse_netstat_listening_pid(banner_only, port=4455) == ""


def test_parse_netstat_listening_pid_picks_the_first_match(monkeypatch):
    text = (
        "  TCP    0.0.0.0:4455           0.0.0.0:0              LISTENING       111\n"
        "  TCP    10.77.9.202:4455       0.0.0.0:0              LISTENING       222\n"
    )
    assert bss._parse_netstat_listening_pid(text, port=4455) == "111"
