"""#1222c -- regression tests for three more strih-side PowerShell round-trips in
scripts/bundle-state-server.py that a live per-facet timing breakdown pinned as the remaining
cost after the #1222/#1222b log-bound-read + port4455-netstat-cache fixes:

  gather timing: ... port4455_owner=11.077s ... shortcut=6.644s ... ndi_runtime=8.295s
                  obs_process_count=15.278s total=41.739s

(port4455_owner's 11s here is the ALREADY-FIXED once-per-OBS-PID cold resolve from #1222/#1222b --
not touched by this ticket.) Under sustained OBS render load, strih's per-PowerShell-invocation
cold-start (module loading etc.) costs 5-15s on top of a ~2.5-3s bare-interpreter baseline -- three
separate facets were each paying this tax on EVERY single /bundle-state.json request:

- obs_process_count (15.278s, actually TIMED OUT at its own 15s subprocess ceiling this run):
  swapped `Get-Process -Name 'obs*'` for a native `tasklist /FO CSV /NH` subprocess (no
  interpreter startup cost at all -- the SAME netstat-for-port4455 pattern from #1222b), parsed by
  a new PURE `_parse_tasklist_obs_process_names()` that reproduces the EXACT newline-of-bare-names
  contract `bsg.obs_process_count_from_listing` already consumes (no downstream parser changes).
- shortcut (6.644s) and ndi_runtime (8.295s): both resolve a value that is effectively STATIC
  between OBS box changes (a Start-Menu .lnk's target only changes when an operator re-points it;
  an NDI runtime DLL's version only changes on an SDK upgrade that replaces the file itself) -- so
  both get a process-lifetime cache keyed by the target file's own (mtime_ns, size), re-resolving
  only when that stat changes. Neither cache EVER stores a failed/empty resolve (the #1222 review's
  own lesson from the port4455 cache: a degenerate result must keep retrying next request, not
  freeze the facet blind for the rest of the process lifetime).

Same "source parsers, verify live separately" split as the sibling test_bundle_state_server_*.py
files -- bundle-state-server.py is hyphenated (not a normal import name) and __main__-guarded, so
it is loaded by file path via importlib without starting the HTTP server.
"""
import importlib.util
import pathlib
import sys
import types

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

_SPEC = importlib.util.spec_from_file_location(
    "bundle_state_server", _SCRIPTS / "bundle-state-server.py"
)
bss = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(bss)  # __name__ != "__main__" -> main()/serve_forever() does NOT run


# ---------------------------------------------------------------------------------------------
# obs_process_count: tasklist replaces PowerShell Get-Process
# ---------------------------------------------------------------------------------------------

TASKLIST_CSV_SAMPLE = (
    '"Image Name","PID","Session Name","Session#","Mem Usage"\r\n'
    '"System Idle Process","0","Services","0","8 K"\r\n'
    '"svchost.exe","1044","Services","0","12,345 K"\r\n'
    '"obs64.exe","4321","Console","1","512,000 K"\r\n'
    '"OBS32.exe","5555","Console","1","256,000 K"\r\n'
    '"notepad.exe","9999","Console","1","10,000 K"\r\n'
)


def test_parse_tasklist_obs_process_names_finds_obs_shaped_names():
    result = bss._parse_tasklist_obs_process_names(TASKLIST_CSV_SAMPLE)
    lines = result.splitlines()
    assert lines == ["obs64", "OBS32"], lines


def test_parse_tasklist_obs_process_names_matches_bsg_count_parser():
    # The output must feed bsg.obs_process_count_from_listing (UNCHANGED by this ticket) and
    # produce the same count it always did for a Get-Process-shaped list.
    import bundle_state_gather as bsg

    result = bss._parse_tasklist_obs_process_names(TASKLIST_CSV_SAMPLE)
    assert bsg.obs_process_count_from_listing(result) == "2"


def test_parse_tasklist_obs_process_names_empty_on_no_obs_processes():
    text = (
        '"Image Name","PID","Session Name","Session#","Mem Usage"\r\n'
        '"notepad.exe","9999","Console","1","10,000 K"\r\n'
    )
    assert bss._parse_tasklist_obs_process_names(text) == ""


def test_parse_tasklist_obs_process_names_empty_on_empty_text():
    assert bss._parse_tasklist_obs_process_names("") == ""
    assert bss._parse_tasklist_obs_process_names(None) == ""


def test_obs_process_list_uses_tasklist_not_powershell(monkeypatch):
    captured = {}

    def fake_run(cmd, **_kw):
        captured["cmd"] = cmd
        return types.SimpleNamespace(stdout=TASKLIST_CSV_SAMPLE)

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    result = bss.obs_process_list()
    assert result.splitlines() == ["obs64", "OBS32"]
    cmd = captured["cmd"]
    assert "tasklist" in cmd, f"must use tasklist, not PowerShell: {cmd}"
    assert "powershell" not in cmd, f"must never spawn PowerShell: {cmd}"


def test_obs_process_list_empty_on_subprocess_error(monkeypatch):
    def boom(cmd, **_kw):
        raise bss.subprocess.SubprocessError("tasklist blew up")

    monkeypatch.setattr(bss.subprocess, "run", boom)
    assert bss.obs_process_list() == ""


# ---------------------------------------------------------------------------------------------
# resolve_shortcut: process-lifetime cache keyed by the .lnk file's own (mtime_ns, size)
# ---------------------------------------------------------------------------------------------

def _reset_shortcut_cache():
    bss._shortcut_cache["path"] = None
    bss._shortcut_cache["stat_key"] = None
    bss._shortcut_cache["target"] = ""
    bss._shortcut_cache["workdir"] = ""


def test_resolve_shortcut_reuses_cache_when_file_unchanged(tmp_path, monkeypatch):
    _reset_shortcut_cache()
    lnk = tmp_path / "OBS Studio.lnk"
    lnk.write_bytes(b"fake shortcut bytes")
    calls = []

    def fake_run(cmd, **_kw):
        calls.append(cmd)
        return types.SimpleNamespace(stdout="C:\\obs\\obs64.exe\nC:\\obs\n")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    first = bss.resolve_shortcut(str(lnk))
    second = bss.resolve_shortcut(str(lnk))
    assert first == ("C:\\obs\\obs64.exe", "C:\\obs")
    assert second == first
    assert len(calls) == 1, "an unchanged shortcut file must not re-pay for the PowerShell resolve"


def test_resolve_shortcut_re_resolves_when_file_changes(tmp_path, monkeypatch):
    _reset_shortcut_cache()
    lnk = tmp_path / "OBS Studio.lnk"
    lnk.write_bytes(b"version one")
    responses = {"stdout": "C:\\obs\\obs64.exe\nC:\\obs\n"}

    def fake_run(cmd, **_kw):
        return types.SimpleNamespace(stdout=responses["stdout"])

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    first = bss.resolve_shortcut(str(lnk))
    assert first == ("C:\\obs\\obs64.exe", "C:\\obs")

    # The operator re-points the shortcut -- file content (and mtime/size) changes.
    lnk.write_bytes(b"a very different version two content, longer")
    responses["stdout"] = "C:\\obs-new\\obs64_new.exe\nC:\\obs-new\n"
    second = bss.resolve_shortcut(str(lnk))
    assert second == ("C:\\obs-new\\obs64_new.exe", "C:\\obs-new")


def test_resolve_shortcut_does_not_cache_a_failed_resolve(tmp_path, monkeypatch):
    _reset_shortcut_cache()
    lnk = tmp_path / "OBS Studio.lnk"
    lnk.write_bytes(b"fake shortcut bytes")
    calls = []

    def boom(cmd, **_kw):
        calls.append(cmd)
        raise bss.subprocess.SubprocessError("powershell blew up")

    monkeypatch.setattr(bss.subprocess, "run", boom)
    first = bss.resolve_shortcut(str(lnk))
    second = bss.resolve_shortcut(str(lnk))
    assert first == ("", "")
    assert second == ("", "")
    assert len(calls) == 2, "a failed resolve must NOT be cached -- every request must keep retrying"


def test_resolve_shortcut_does_not_cache_an_empty_result(tmp_path, monkeypatch):
    _reset_shortcut_cache()
    lnk = tmp_path / "OBS Studio.lnk"
    lnk.write_bytes(b"fake shortcut bytes")
    calls = []

    def fake_run(cmd, **_kw):
        calls.append(cmd)
        return types.SimpleNamespace(stdout="")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    first = bss.resolve_shortcut(str(lnk))
    second = bss.resolve_shortcut(str(lnk))
    assert first == ("", "")
    assert second == ("", "")
    assert len(calls) == 2, "an empty resolve must NOT be cached -- every request must keep retrying"


def test_resolve_shortcut_missing_file_is_empty_and_uncached(tmp_path, monkeypatch):
    _reset_shortcut_cache()
    missing = tmp_path / "nope.lnk"
    calls = []

    def fake_run(cmd, **_kw):
        calls.append(cmd)
        return types.SimpleNamespace(stdout="C:\\obs\\obs64.exe\nC:\\obs\n")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    assert bss.resolve_shortcut(str(missing)) == ("C:\\obs\\obs64.exe", "C:\\obs")
    assert bss.resolve_shortcut(str(missing)) == ("C:\\obs\\obs64.exe", "C:\\obs")
    assert len(calls) == 2, "a shortcut path that cannot be stat'd must never be cached"


# ---------------------------------------------------------------------------------------------
# ndi_runtime_version: process-lifetime cache keyed by the DLL's own (mtime_ns, size)
# ---------------------------------------------------------------------------------------------

def _reset_ndi_runtime_cache():
    bss._ndi_runtime_cache["path"] = None
    bss._ndi_runtime_cache["stat_key"] = None
    bss._ndi_runtime_cache["version"] = ""


def test_ndi_runtime_version_reuses_cache_when_file_unchanged(tmp_path, monkeypatch):
    _reset_ndi_runtime_cache()
    dll = tmp_path / "Processing.NDI.Lib.x64.dll"
    dll.write_bytes(b"fake dll bytes")
    calls = []

    def fake_run(cmd, **_kw):
        calls.append(cmd)
        return types.SimpleNamespace(stdout="6.2.1.0\n")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    first = bss.ndi_runtime_version(str(dll))
    second = bss.ndi_runtime_version(str(dll))
    assert first == "6.2.1.0"
    assert second == "6.2.1.0"
    assert len(calls) == 1, "an unchanged DLL must not re-pay for the PowerShell VersionInfo read"


def test_ndi_runtime_version_re_resolves_when_file_changes(tmp_path, monkeypatch):
    _reset_ndi_runtime_cache()
    dll = tmp_path / "Processing.NDI.Lib.x64.dll"
    dll.write_bytes(b"version one bytes")
    responses = {"stdout": "6.2.1.0\n"}

    def fake_run(cmd, **_kw):
        return types.SimpleNamespace(stdout=responses["stdout"])

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    first = bss.ndi_runtime_version(str(dll))
    assert first == "6.2.1.0"

    # An NDI SDK upgrade replaces the DLL bytes.
    dll.write_bytes(b"a very different version two, quite a lot longer than before")
    responses["stdout"] = "6.3.0.0\n"
    second = bss.ndi_runtime_version(str(dll))
    assert second == "6.3.0.0"


def test_ndi_runtime_version_does_not_cache_a_failed_resolve(tmp_path, monkeypatch):
    _reset_ndi_runtime_cache()
    dll = tmp_path / "Processing.NDI.Lib.x64.dll"
    dll.write_bytes(b"fake dll bytes")
    calls = []

    def boom(cmd, **_kw):
        calls.append(cmd)
        raise bss.subprocess.SubprocessError("powershell blew up")

    monkeypatch.setattr(bss.subprocess, "run", boom)
    first = bss.ndi_runtime_version(str(dll))
    second = bss.ndi_runtime_version(str(dll))
    assert first == ""
    assert second == ""
    assert len(calls) == 2, "a failed resolve must NOT be cached -- every request must keep retrying"


def test_ndi_runtime_version_does_not_cache_an_empty_result(tmp_path, monkeypatch):
    _reset_ndi_runtime_cache()
    dll = tmp_path / "Processing.NDI.Lib.x64.dll"
    dll.write_bytes(b"fake dll bytes")
    calls = []

    def fake_run(cmd, **_kw):
        calls.append(cmd)
        return types.SimpleNamespace(stdout="")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    first = bss.ndi_runtime_version(str(dll))
    second = bss.ndi_runtime_version(str(dll))
    assert first == ""
    assert second == ""
    assert len(calls) == 2, "an empty resolve must NOT be cached -- every request must keep retrying"


def test_ndi_runtime_version_missing_dll_is_empty(monkeypatch):
    calls = []

    def fake_run(cmd, **_kw):
        calls.append(cmd)
        return types.SimpleNamespace(stdout="6.2.1.0\n")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    assert bss.ndi_runtime_version("/nonexistent/path/to.dll") == ""
    assert len(calls) == 0, "a missing DLL must never even attempt the PowerShell resolve"
