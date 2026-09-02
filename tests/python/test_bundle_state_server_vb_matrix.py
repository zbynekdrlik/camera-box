"""#1227 — server-side tests for the VB-Matrix facet plumbing in scripts/bundle-state-server.py:

- vb_matrix_process_list(): a native `tasklist /FO CSV /NH` (never PowerShell), returning the RAW
  CSV so bsg.vb_matrix_process_from_listing can read the PID column; "" on any failure.
- vb_matrix_start_time(pid): the best-effort CIM CreationDate read with the #1222 PID-keyed cache
  (resolve only on a pid change; never-cache-empty; clear-on-failure; "" for a falsy pid).

bundle-state-server.py is hyphenated + __main__-guarded, so it is loaded by file path via importlib
without starting the HTTP server (same pattern as the sibling test_bundle_state_server_*.py files).
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


# The live-observed stream-box tasklist row (VBAudioMatrix_x64.exe PID 8144).
TASKLIST_CSV = (
    '"Image Name","PID","Session Name","Session#","Mem Usage"\r\n'
    '"obs64.exe","4321","Console","1","512,000 K"\r\n'
    '"VBAudioMatrix_x64.exe","8144","Console","1","18,236 K"\r\n'
)


# ---------------------------------------------------------------- vb_matrix_process_list
def test_process_list_uses_tasklist_not_powershell(monkeypatch):
    captured = {}

    def fake_run(cmd, **_kw):
        captured["cmd"] = cmd
        return types.SimpleNamespace(stdout=TASKLIST_CSV)

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    out = bss.vb_matrix_process_list()
    # returns the RAW csv (so bsg can parse the PID column)
    assert "VBAudioMatrix_x64.exe" in out
    assert bss.bsg.vb_matrix_process_from_listing(out) == ("VBAudioMatrix_x64", "8144")
    cmd = captured["cmd"]
    assert "tasklist" in cmd, f"must use tasklist, not PowerShell: {cmd}"
    assert "powershell" not in cmd, f"must never spawn PowerShell for presence: {cmd}"


def test_process_list_empty_on_subprocess_error(monkeypatch):
    def boom(cmd, **_kw):
        raise bss.subprocess.SubprocessError("tasklist blew up")

    monkeypatch.setattr(bss.subprocess, "run", boom)
    assert bss.vb_matrix_process_list() == ""


# ---------------------------------------------------------------- vb_matrix_start_time
def _reset_start_cache():
    bss._vb_matrix_start_cache["pid"] = None
    bss._vb_matrix_start_cache["start"] = ""


def test_start_time_falsy_pid_is_empty_no_query(monkeypatch):
    _reset_start_cache()
    calls = []
    monkeypatch.setattr(bss.subprocess, "run",
                        lambda cmd, **_kw: calls.append(cmd) or types.SimpleNamespace(stdout="x"))
    assert bss.vb_matrix_start_time("") == ""
    assert bss.vb_matrix_start_time(None) == ""
    assert calls == [], "a falsy pid must never spawn the CIM query"


def test_start_time_resolves_and_caches_by_pid(monkeypatch):
    _reset_start_cache()
    calls = []

    def fake_run(cmd, **_kw):
        calls.append(cmd)
        return types.SimpleNamespace(stdout="2026-09-02T14:01:40\n")

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    first = bss.vb_matrix_start_time("8144")
    second = bss.vb_matrix_start_time("8144")
    assert first == "2026-09-02T14:01:40"
    assert second == first
    assert len(calls) == 1, "an unchanged pid must not re-pay for the CIM resolve"
    # the CIM query must scope by ProcessId and format the DateTime (locale-stable)
    joined = " ".join(calls[0])
    assert "Win32_Process" in joined and "ProcessId=8144" in joined
    assert "yyyy-MM-ddTHH:mm:ss" in joined


def test_start_time_re_resolves_when_pid_changes(monkeypatch):
    _reset_start_cache()
    responses = {"stdout": "2026-09-02T14:01:40\n"}
    monkeypatch.setattr(bss.subprocess, "run",
                        lambda cmd, **_kw: types.SimpleNamespace(stdout=responses["stdout"]))
    assert bss.vb_matrix_start_time("8144") == "2026-09-02T14:01:40"
    responses["stdout"] = "2026-09-02T16:20:00\n"   # VB-Matrix restarted -> new pid
    assert bss.vb_matrix_start_time("9001") == "2026-09-02T16:20:00"


def test_start_time_does_not_cache_empty(monkeypatch):
    _reset_start_cache()
    calls = []

    def fake_run(cmd, **_kw):
        calls.append(cmd)
        return types.SimpleNamespace(stdout="")  # access-denied / flaky -> empty

    monkeypatch.setattr(bss.subprocess, "run", fake_run)
    assert bss.vb_matrix_start_time("8144") == ""
    assert bss.vb_matrix_start_time("8144") == ""
    assert len(calls) == 2, "an empty resolve must NOT be cached -- keep retrying"


def test_start_time_clears_cache_on_failure(monkeypatch):
    _reset_start_cache()
    calls = []

    def boom(cmd, **_kw):
        calls.append(cmd)
        raise bss.subprocess.SubprocessError("powershell blew up")

    monkeypatch.setattr(bss.subprocess, "run", boom)
    assert bss.vb_matrix_start_time("8144") == ""
    assert bss.vb_matrix_start_time("8144") == ""
    assert len(calls) == 2, "a failed resolve must clear the cache -- keep retrying"
    assert bss._vb_matrix_start_cache["pid"] is None
