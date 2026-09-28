"""Issue 1386 slice D -- the behaviour-neutral proof for moving the Windows-only identity readers out
of scripts/bundle-state-server.py.

The readers: the :4455 owner (netstat PID probe + the PID-keyed CIM resolve and its cache), the
shared tasklist read and its parsers, the VB-Matrix start time (CIM, PID-keyed cache), the AHK text,
the Start-Menu shortcut (file-stat-keyed cache) and the NDI runtime version (file-stat-keyed cache).

The slice-A golden (test_bundle_state_split_1386.py) STUBS these readers on the server module, so it
proves the server's orchestration but never runs the readers themselves. This golden runs them for
real, with only `subprocess.run` replaced by a scripted fake (the process-wide `subprocess` module
object, so the fake reaches the readers wherever they live). WINDOWS_GOLDEN was captured from the
PRE-MOVE code and committed before the move. For every step it holds, byte for byte:

  * the reader's return value;
  * every subprocess call it made (argv + keyword arguments, so the PowerShell programs, the
    timeouts and `check=True` are pinned);
  * every line it logged (timestamp normalized);
  * the four module-level caches after the step.

The steps cover cache miss / hit / invalidation / never-cache-empty / clear-on-failure for each
cache, and each subprocess failure kind (timeout, non-zero exit, missing binary). The golden also
holds the served JSON on BOTH gather paths THROUGH the real readers (two consecutive requests, so
the second one shows the cache hits), with the subprocess calls and log lines of each request, and
the timing key order.

The readers are reached as `bss.<name>` -- the names the server re-exports -- so the same file runs
unchanged on the pre-move and the post-move code. Refresh the golden ONLY for an intended output
change, never to absorb a refactor diff:
    python3 tests/python/test_bundle_state_windows_split_1386.py --write-golden
"""
from __future__ import annotations

import ast
import contextlib
import importlib.util
import io
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile
import types
from unittest import mock

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_FX = pathlib.Path(__file__).resolve().parent / "fixtures" / "bundle_state_split_1386"
_GOLDEN = _FX / "windows_golden.json"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

_spec = importlib.util.spec_from_file_location("bundle_state_server_windows_split_1386",
                                               _SCRIPTS / "bundle-state-server.py")
bss = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bss)
import bundle_state_windows as bsw  # noqa: E402 -- the readers + caches (issue 1386 slice D)

# Every name a test or the server's own orchestration reaches on the server module. Each must still
# resolve there after the move (the re-export surface).
_SERVER_SURFACE = (
    "log", "newest_obs_log_text", "gather_bundle_state", "gather_ndi_inputs", "IS_WINDOWS",
    "bsg", "DEFAULT_VB_MATRIX_INSTALL_DIRS",
    "ndi_runtime_version", "port4455_owner",
    "_parse_tasklist_obs_process_names", "tasklist_csv",
    "gather_vb_matrix_facet", "vb_matrix_start_time",
    "read_ahk_text", "resolve_shortcut",
)
# Reached only on bundle_state_windows: the caches, the internal parsers and the legacy
# process-list readers. The server no longer re-exports them for tests (issue 1386 supervisor
# decision 5866591291), so a test resets / patches them on that module.
_WINDOWS_ONLY = (
    "_ndi_runtime_cache", "_parse_netstat_listening_pid", "_port4455_owning_pid", "_port4455_cache",
    "obs_process_list", "vb_matrix_process_list", "_vb_matrix_start_cache", "_shortcut_cache",
)
_CACHES = ("_port4455_cache", "_ndi_runtime_cache", "_shortcut_cache", "_vb_matrix_start_cache")
_CACHE_EMPTY = {
    "_port4455_cache": {"pid": None, "path": "", "version": ""},
    "_ndi_runtime_cache": {"path": None, "stat_key": None, "version": ""},
    "_shortcut_cache": {"path": None, "stat_key": None, "target": "", "workdir": ""},
    "_vb_matrix_start_cache": {"pid": None, "start": ""},
}
_MTIME_NS = 1_700_000_000_123_456_789
_TS_RE = re.compile(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} ", re.M)
_OBS_EXE = r"C:\Program Files\obs-studio\bin\64bit\obs64.exe"
_OBS_BIN = r"C:\Program Files\obs-studio\bin\64bit"
_TASKLIST = (
    '"System Idle Process","0","Services","0","8 K"\n'
    '"obs64.exe","4242","Console","1","512,000 K"\n'
    '"obs64.exe","58560","Console","1","45 K"\n'
    '"OBS.exe","60","Console","1","2,048 K"\n'
    '"VBAudioMatrix_Setup.exe","77","Console","1","1,000 K"\n'
    '"VBAudioMatrix_x64.exe","9001","Console","1","18,236 K"\n'
)
_AHK = (
    'app1_run  := 1\r\n'
    'app1_path := "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\OBS Studio.lnk"\r\n'
    'app1_binarypath := "D:\\_APPS\\1ME-obs\\1ME.lnk"\r\n'
    'app2_run  := 0\r\n'
)
_NDI_INPUTS = {
    "NDI cam1": {"kind": "ndi_source", "settings": {"genlock_fifo": True, "latency": 3}},
    "NDI 2ME PGM": {"kind": "ndi_source", "settings": {"genlock_fifo": True, "latency": 963}},
}


def _netstat(*rows):
    head = "\nActive Connections\n\n  Proto  Local Address          Foreign Address        State           PID\n"
    return head + "".join(f"  {r}\n" for r in rows)


_NETSTAT_4242 = _netstat("TCP    0.0.0.0:8899           0.0.0.0:0              LISTENING       9648",
                         "TCP    0.0.0.0:4455           0.0.0.0:0              LISTENING       4242",
                         "UDP    0.0.0.0:5353           *:*                                    1234")
_NETSTAT_5555 = _netstat("TCP    [::]:4455              [::]:0                 LISTENING       5555")
_NETSTAT_NONE = _netstat("TCP    10.0.0.2:51000         10.0.0.9:4455          ESTABLISHED     77",
                         "TCP    0.0.0.0:44551          0.0.0.0:0              LISTENING       88")


class _Fake:
    """A scripted `subprocess.run`: each call is recorded, then answered from `self.answers` by the
    call's kind (netstat / tasklist / the four PowerShell programs). An answer is stdout text, or an
    exception class the fake raises the way the real call would."""

    def __init__(self):
        self.calls = []
        self.answers = {}

    @staticmethod
    def kind(cmd):
        if cmd[0] in ("netstat", "tasklist"):
            return cmd[0]
        prog = cmd[-1]
        for token, name in (("Get-NetTCPConnection", "ps_port4455"), ("WScript.Shell", "ps_shortcut"),
                            ("CreationDate", "ps_vb_start"), ("VersionInfo", "ps_ndi")):
            if token in prog:
                return name
        return "unknown"

    def __call__(self, cmd, **kw):
        self.calls.append({"cmd": list(cmd), "kw": {k: kw[k] for k in sorted(kw)}})
        answer = self.answers.get(self.kind(cmd), "")
        if answer is subprocess.TimeoutExpired:
            raise subprocess.TimeoutExpired(cmd, kw.get("timeout"))
        if answer is subprocess.CalledProcessError:
            raise subprocess.CalledProcessError(1, cmd)
        if answer is FileNotFoundError:
            raise FileNotFoundError(2, "No such file or directory", cmd[0])
        return types.SimpleNamespace(stdout=answer, returncode=0)


def _norm(obj, tmp):
    text = json.dumps(obj)
    return json.loads(text.replace(json.dumps(str(tmp))[1:-1], "<TMP>"))


def _reset_caches():
    for name in _CACHES:
        getattr(bsw, name).update(_CACHE_EMPTY[name])


def _caches():
    return {name: dict(getattr(bsw, name)) for name in _CACHES}


def _touch(path, data):
    path.write_bytes(data)
    os.utime(path, ns=(_MTIME_NS, _MTIME_NS))


def _run_step(fake, tmp, label, fn, answers):
    fake.answers = dict(answers)
    first = len(fake.calls)
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        result = fn()
    return _norm({"step": label, "result": result, "calls": fake.calls[first:],
                  "log": _TS_RE.sub("<TS> ", out.getvalue()).splitlines(),
                  "caches": _caches()}, tmp)


def _pure_steps():
    netstats = {"4242": _NETSTAT_4242, "5555-v6": _NETSTAT_5555, "none": _NETSTAT_NONE,
                "empty": "", "none-arg": None}
    tasklists = {"sample": _TASKLIST, "header": '"Image Name","PID","Session Name","Session#","Mem'
                 ' Usage"\n' + _TASKLIST, "empty": "", "blank": "  \n", "short-row": '"obs64.exe","1"\n',
                 "too-long-field": "x" * 200_000}
    out = {}
    for name, text in netstats.items():
        out[f"netstat[{name}]"] = bsw._parse_netstat_listening_pid(text)
    out["netstat[4242,port=8899]"] = bsw._parse_netstat_listening_pid(_NETSTAT_4242, port=8899)
    for name, text in tasklists.items():
        log = io.StringIO()
        with contextlib.redirect_stdout(log):
            names = bss._parse_tasklist_obs_process_names(text)
        out[f"tasklist[{name}]"] = [names, _TS_RE.sub("<TS> ", log.getvalue()).splitlines()]
    return out


def _tasklist_steps(fake, tmp):
    steps = []
    for label, answer in (("ok", _TASKLIST), ("timeout", subprocess.TimeoutExpired),
                          ("exit1", subprocess.CalledProcessError), ("missing", FileNotFoundError)):
        steps.append(_run_step(fake, tmp, f"tasklist_csv[{label}]", bss.tasklist_csv,
                               {"tasklist": answer}))
        steps.append(_run_step(fake, tmp, f"obs_process_list[{label}]", bsw.obs_process_list,
                               {"tasklist": answer}))
        steps.append(_run_step(fake, tmp, f"vb_matrix_process_list[{label}]",
                               bsw.vb_matrix_process_list, {"tasklist": answer}))
    starts = []

    def start_fn(pid):
        starts.append(pid)
        return f"start-of-{pid}" if pid else ""

    for present in (True, False):
        for label, text in (("sample", _TASKLIST), ("failed-read", ""), ("no-vbm", '"x.exe","1"\n')):
            steps.append(_run_step(
                fake, tmp, f"gather_vb_matrix_facet[{present},{label}]",
                lambda p=present, t=text: list(bss.gather_vb_matrix_facet(p, t, start_fn)), {}))
    steps.append({"step": "gather_vb_matrix_facet start_fn pids", "result": starts})
    return steps


def _vb_start_steps(fake, tmp):
    ok = {"ps_vb_start": "2026-09-28T01:02:03\n"}
    seq = (("none", None, ok), ("empty-str", "", ok), ("non-numeric", "12a", ok),
           ("miss", "9001", ok), ("hit", "9001", ok), ("other-pid-empty", "9002", {"ps_vb_start": ""}),
           ("other-pid-empty-again", "9002", {"ps_vb_start": "  \n"}), ("first-still-hit", "9001", ok),
           ("timeout", "9003", {"ps_vb_start": subprocess.TimeoutExpired}),
           ("after-clear-miss", "9001", ok), ("exit1", "9004", {"ps_vb_start": subprocess.CalledProcessError}),
           ("missing-ps", "9005", {"ps_vb_start": FileNotFoundError}), ("int-pid", 9006, ok))
    return [_run_step(fake, tmp, f"vb_matrix_start_time[{label}]",
                      lambda p=pid: bss.vb_matrix_start_time(p), ans) for label, pid, ans in seq]


def _ndi_steps(fake, tmp):
    dll = tmp / "Processing.NDI.Lib.x64.dll"
    other = tmp / "other-ndi.dll"
    ok = {"ps_ndi": "6.2.1.0\r\n"}
    steps = [_run_step(fake, tmp, "ndi[missing-file]",
                       lambda: bss.ndi_runtime_version(str(dll)), ok)]
    _touch(dll, b"ndi-runtime-v1")
    _touch(other, b"other")
    for label, path, answers, before in (
            ("miss", dll, ok, None), ("hit", dll, ok, None),
            ("changed-stat", dll, {"ps_ndi": "6.3.0.0\n"}, lambda: _touch(dll, b"ndi-runtime-v22")),
            ("hit-after-change", dll, ok, None), ("other-path-empty", other, {"ps_ndi": "\n"}, None),
            ("other-path-timeout", other, {"ps_ndi": subprocess.TimeoutExpired}, None),
            ("other-path-exit1", other, {"ps_ndi": subprocess.CalledProcessError}, None),
            ("other-path-missing-ps", other, {"ps_ndi": FileNotFoundError}, None),
            ("first-path-after-failures", dll, ok, None)):
        if before:
            before()
        steps.append(_run_step(fake, tmp, f"ndi[{label}]",
                               lambda p=path: bss.ndi_runtime_version(str(p)), answers))
    return steps


def _shortcut_steps(fake, tmp):
    lnk = tmp / "OBS Studio.lnk"
    other = tmp / "other.lnk"
    ok = {"ps_shortcut": f"{_OBS_EXE}\r\n{_OBS_BIN}\r\n"}
    steps = [_run_step(fake, tmp, "shortcut[missing-file]",
                       lambda: list(bss.resolve_shortcut(str(lnk))), ok),
             _run_step(fake, tmp, "shortcut[missing-file-again]",
                       lambda: list(bss.resolve_shortcut(str(lnk))), ok)]
    _touch(lnk, b"lnk-v1")
    _touch(other, b"other")
    for label, path, answers, before in (
            ("miss", lnk, ok, None), ("hit", lnk, ok, None),
            ("changed-stat", lnk, {"ps_shortcut": "D:\\_APPS\\obs\\obs64.exe\n"},
             lambda: _touch(lnk, b"lnk-v22")),
            ("hit-after-change", lnk, ok, None), ("other-empty-target", other,
                                                  {"ps_shortcut": "\nC:\\x\n"}, None),
            ("other-no-output", other, {"ps_shortcut": ""}, None),
            ("other-timeout", other, {"ps_shortcut": subprocess.TimeoutExpired}, None),
            ("other-exit1", other, {"ps_shortcut": subprocess.CalledProcessError}, None),
            ("other-missing-ps", other, {"ps_shortcut": FileNotFoundError}, None),
            ("first-after-failures", lnk, ok, None)):
        if before:
            before()
        steps.append(_run_step(fake, tmp, f"shortcut[{label}]",
                               lambda p=path: list(bss.resolve_shortcut(str(p))), answers))
    return steps


def _port4455_steps(fake, tmp):
    owner = {"netstat": _NETSTAT_4242, "ps_port4455": f"{_OBS_EXE}\r\n32.1.2\r\n"}
    seq = (
        ("no-listener", {"netstat": _NETSTAT_NONE}),
        ("miss", owner),
        ("hit", owner),
        ("pid-change-empty-resolve", {"netstat": _NETSTAT_5555, "ps_port4455": "\n  \n"}),
        ("back-to-cached-pid", owner),
        ("pid-change-timeout", {"netstat": _NETSTAT_5555, "ps_port4455": subprocess.TimeoutExpired}),
        ("path-only", {"netstat": _NETSTAT_5555, "ps_port4455": "\n  D:\\_APPS\\obs64.exe  \n"}),
        ("path-only-hit", {"netstat": _NETSTAT_5555, "ps_port4455": "unused\n"}),
        ("netstat-timeout", {"netstat": subprocess.TimeoutExpired}),
        ("after-probe-failure-miss", owner),
        ("netstat-exit1", {"netstat": subprocess.CalledProcessError}),
        ("netstat-missing", {"netstat": FileNotFoundError}),
        ("resolve-exit1", {"netstat": _NETSTAT_4242, "ps_port4455": subprocess.CalledProcessError}),
        ("resolve-missing-ps", {"netstat": _NETSTAT_4242, "ps_port4455": FileNotFoundError}),
        ("three-lines", {"netstat": _NETSTAT_4242, "ps_port4455": f"{_OBS_EXE}\n32.1.2\nextra\n"}),
    )
    steps = [_run_step(fake, tmp, f"port4455_owner[{label}]", lambda: list(bss.port4455_owner()), ans)
             for label, ans in seq]
    for label, ans in (("ok", {"netstat": _NETSTAT_4242}), ("v6", {"netstat": _NETSTAT_5555}),
                       ("none", {"netstat": _NETSTAT_NONE}), ("timeout", {"netstat": subprocess.TimeoutExpired})):
        steps.append(_run_step(fake, tmp, f"_port4455_owning_pid[{label}]", bsw._port4455_owning_pid, ans))
    return steps


def _ahk_steps(fake, tmp):
    ahk = tmp / "NL_STARTUP.ahk"
    steps = [_run_step(fake, tmp, "ahk[missing]", lambda: bss.read_ahk_text(str(ahk)), {})]
    ahk.write_bytes(_AHK.encode("utf-8") + b"; \xff\xfe bad bytes\n")
    steps.append(_run_step(fake, tmp, "ahk[present]", lambda: bss.read_ahk_text(str(ahk)), {}))
    steps.append(_run_step(fake, tmp, "ahk[a-directory]", lambda: bss.read_ahk_text(str(tmp)), {}))
    return steps


def _host_tree(tmp):
    host = tmp / "host"
    (host / "plugins" / "obs-plugins" / "64bit").mkdir(parents=True)
    (host / "plugins" / "obs-plugins" / "64bit" / "distroav.dll").write_bytes(b"distroav-bytes")
    (host / "apps" / "_RETIRED_1ME-obs").mkdir(parents=True)
    (host / "apps" / "obs64.exe").write_bytes(b"x")
    (host / "apps" / "_RETIRED_1ME-obs" / "1ME.exe").write_bytes(b"x")
    (host / "vbm").mkdir()
    (host / "vbm" / "VBAudioMatrix_x64.exe").write_bytes(b"x")
    (host / "obs.dll").write_bytes(b"obs-bytes")
    (host / "GENLOCK_BUILD_SHA.txt").write_text("abc1234def\n", encoding="utf-8")
    (host / "NL_STARTUP.ahk").write_text(_AHK, encoding="utf-8", newline="")
    _touch(host / "OBS Studio.lnk", b"lnk")
    _touch(host / "ndi.dll", b"ndi")
    (host / "logs").mkdir()
    return host


def _served_steps(fake, tmp):
    """Two consecutive requests per (log, platform): the served JSON, the subprocess calls, the log
    lines (timing line reduced to its key order) of each."""
    host = _host_tree(tmp)
    answers = {"netstat": _NETSTAT_4242, "tasklist": _TASKLIST, "ps_ndi": "6.2.1.0\n",
               "ps_port4455": f"{_OBS_EXE}\n32.1.2\n", "ps_shortcut": f"{_OBS_EXE}\n{_OBS_BIN}\n",
               "ps_vb_start": "2026-09-28T01:02:03\n"}
    logs = {"all_families": (_FX / "all_families.txt").read_text(encoding="utf-8"), "empty": ""}
    out = {}
    for log_name, text in logs.items():
        (host / "logs" / "obs.txt").write_bytes(text.encode("utf-8"))
        for windows in (False, True):
            _reset_caches()
            env = {k: v for k, v in os.environ.items() if k != "AUDIO_REF_BAND_SRC"}
            env["BUNDLE_STATE_TIMING"] = "1"
            with contextlib.ExitStack() as st:
                st.enter_context(mock.patch.object(bss, "IS_WINDOWS", windows))
                st.enter_context(mock.patch.object(
                    bss, "gather_ndi_inputs", lambda h, p: json.loads(json.dumps(_NDI_INPUTS))))
                st.enter_context(mock.patch.object(bss.bsg, "local_seconds_of_day", lambda *a: 43200.0))
                st.enter_context(mock.patch.object(bss, "DEFAULT_VB_MATRIX_INSTALL_DIRS",
                                                   (str(host / "vbm"),)))
                st.enter_context(mock.patch.dict(os.environ, env, clear=True))
                for request in (1, 2):
                    step = _run_step(fake, tmp, f"gather[{log_name},windows={windows},#{request}]",
                                     lambda: json.dumps(bss.gather_bundle_state(
                                         "127.0.0.1", "", str(host / "logs"), str(host / "ndi.dll"),
                                         [str(host / "plugins"), str(host / "missing-root")],
                                         genlock_build_sha_file=str(host / "GENLOCK_BUILD_SHA.txt"),
                                         obs_install_scan_roots=(str(host / "apps"),),
                                         startup_shortcut=str(host / "OBS Studio.lnk"),
                                         ahk_path=str(host / "NL_STARTUP.ahk"),
                                         obs_dll_path=str(host / "obs.dll"))), answers)
                    step["log"] = [(" ".join(re.findall(r"(\w+)=[\d.]+s", line))
                                    if "gather timing:" in line else line) for line in step["log"]]
                    out[step.pop("step")] = step
    return out


def compute_golden(tmp_dir):
    tmp = pathlib.Path(tmp_dir)
    fake = _Fake()
    golden = {"pure": _pure_steps()}
    with mock.patch.object(subprocess, "run", fake):
        _reset_caches()
        golden["steps"] = []
        for family, fn in (("tasklist", _tasklist_steps), ("vb_start", _vb_start_steps),
                           ("ndi", _ndi_steps), ("shortcut", _shortcut_steps),
                           ("port4455", _port4455_steps), ("ahk", _ahk_steps)):
            sub = tmp / family
            sub.mkdir()
            golden["steps"].extend(fn(fake, sub))
        served = tmp / "served"
        served.mkdir()
        golden["served"] = _served_steps(fake, served)
    _reset_caches()
    return _norm(golden, tmp)


# --- the proof -----------------------------------------------------------------------------------

@pytest.fixture(scope="module")
def golden():
    return json.loads(_GOLDEN.read_text(encoding="utf-8"))


@pytest.fixture(scope="module")
def actual(tmp_path_factory):
    return compute_golden(str(tmp_path_factory.mktemp("windows1386")))


def test_the_pure_parsers_are_byte_identical(golden, actual):
    assert actual["pure"] == golden["pure"]


def test_every_reader_step_is_byte_identical(golden, actual):
    assert [s["step"] for s in actual["steps"]] == [s["step"] for s in golden["steps"]]
    for got, want in zip(actual["steps"], golden["steps"]):
        assert got == want, want["step"]


def test_the_served_json_through_the_real_readers_is_byte_identical(golden, actual):
    assert sorted(actual["served"]) == sorted(golden["served"])
    for name, want in golden["served"].items():
        assert actual["served"][name] == want, name


def test_the_golden_exercises_every_reader_and_both_cache_outcomes(golden):
    # A guard on the golden itself: it must reach each subprocess kind, a failure log, and a cache
    # hit (a step with no subprocess call) -- so an empty or trivial capture can never pass.
    kinds = {_Fake.kind(c["cmd"]) for s in golden["steps"] for c in s.get("calls", [])}
    assert kinds == {"netstat", "tasklist", "ps_port4455", "ps_shortcut", "ps_vb_start", "ps_ndi"}
    assert any("WARNING" in line for s in golden["steps"] for line in s.get("log", []))
    hits = [s["step"] for s in golden["steps"] if "[hit" in s["step"] and not s["calls"]]
    assert {"ndi[hit]", "shortcut[hit]", "vb_matrix_start_time[hit]"} <= set(hits)
    windows = golden["served"]["gather[all_families,windows=True,#1]"]
    assert '"port4455_owner_path"' in windows["result"] and '"vb_matrix_start"' in windows["result"]
    assert golden["served"]["gather[all_families,windows=False,#1]"]["calls"] == []


def test_every_name_the_tests_and_the_orchestration_reach_still_resolves_on_the_server():
    missing = [n for n in _SERVER_SURFACE if not hasattr(bss, n)]
    assert missing == []


def test_the_reader_internals_live_only_on_the_windows_module():
    assert [n for n in _WINDOWS_ONLY if not hasattr(bsw, n)] == []
    assert [n for n in _WINDOWS_ONLY if hasattr(bss, n)] == [], "a test-only re-export came back"
    assert not hasattr(bss, "subprocess"), "the server imports subprocess only for tests again"


# --- the moved layout (added with the move; the proof above ran unchanged before and after it) ---

def test_the_server_reexports_the_moved_objects_not_copies():
    # The server's orchestration calls the readers through its own globals, so each server name
    # must be the very object the readers module holds (a patch of `bss.<reader>` reaches the
    # gather). The logger is ONE function for both.
    import bundle_state_serverlog
    import bundle_state_windows as bsw
    moved = [n for n in _SERVER_SURFACE if n in vars(bsw)]
    assert {"port4455_owner", "tasklist_csv", "vb_matrix_start_time",
            "resolve_shortcut", "ndi_runtime_version", "read_ahk_text", "log"} <= set(moved)
    for name in moved:
        assert getattr(bss, name) is vars(bsw)[name], f"bss.{name} is a copy, not the moved object"
    assert bss.log is bsw.log is bundle_state_serverlog.log
    for name in ("port4455_owner", "tasklist_csv", "vb_matrix_start_time", "resolve_shortcut",
                 "ndi_runtime_version", "read_ahk_text", "_port4455_cache", "_shortcut_cache"):
        assert name not in _server_own_definitions(), f"the server still defines {name} itself"


def _server_own_definitions():
    tree = ast.parse((_SCRIPTS / "bundle-state-server.py").read_text(encoding="utf-8"))
    names = set()
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.ClassDef)):
            names.add(node.name)
        elif isinstance(node, ast.Assign):
            names.update(t.id for t in node.targets if isinstance(t, ast.Name))
    return names


if __name__ == "__main__" and sys.argv[1:] == ["--write-golden"]:
    with tempfile.TemporaryDirectory() as d:
        data = compute_golden(d)
    _GOLDEN.write_text(json.dumps(data, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    print(f"wrote {_GOLDEN} ({len(data['steps'])} reader steps, {len(data['served'])} requests)")
