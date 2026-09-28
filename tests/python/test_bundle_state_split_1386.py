"""Issue 1386 slice A -- the behaviour-neutral proof for splitting scripts/bundle_state_gather.py
into sibling facet modules and cutting bundle-state-server.py's gather_bundle_state into helpers.

The GOLDEN (fixtures/bundle_state_split_1386/golden.json) was captured from the PRE-SPLIT code and
committed before any refactor commit. It holds, byte-for-byte:

  * every `*_from_log` facet parser (+ `timestamped_tail_lines`, the bounded read) over the recorded
    fixture logs (the real 27.9 resolume logs, the av-step and reference-band captures, the
    arrival-floor strih logs), one synthetic log carrying every line family across a midnight
    (fixtures/bundle_state_split_1386/all_families.txt), their concatenation, and the head+tail
    BOUNDED read of each one large enough to be cut (so the separator paths run too);
  * the server's served JSON (`json.dumps(gather_bundle_state(...))`, key order included) for
    each of those logs on BOTH gather paths: the Linux path (IS_WINDOWS False) and the Windows path
    (IS_WINDOWS True, every Windows subprocess leaf stubbed, the file scans pointed at a tmp tree);
  * the opt-in BUNDLE_STATE_TIMING key order, the pure host/process facets over fixed inputs, and
    `build_bundle_state` with every keyword the original signature accepted;
  * the public names the original module exposed (every one must still import).

Refresh the golden ONLY for an intended output change, never to absorb a refactor diff:
    python3 tests/python/test_bundle_state_split_1386.py --write-golden
"""
from __future__ import annotations

import ast
import contextlib
import gzip
import hashlib
import importlib.util
import inspect
import json
import os
import pathlib
import re
import sys
import tempfile
from unittest import mock

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_FX = pathlib.Path(__file__).resolve().parent / "fixtures" / "bundle_state_split_1386"
_GOLDEN = _FX / "golden.json"
sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402

_spec = importlib.util.spec_from_file_location("bundle_state_server_split_1386",
                                               _SCRIPTS / "bundle-state-server.py")
bss = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bss)

# The recorded logs (gz = CRLF as on the box; decoded exactly as a bounded read decodes them).
_RECORDED = (
    "tests/fixtures/audio_mixer_1381/resolume-start-1949-1957.txt.gz",
    "tests/fixtures/audio_mixer_1381/resolume-clean-2000-2045.txt.gz",
    "tests/fixtures/audio_mixer_1381/resolume-control-0300-0415.txt.gz",
    "tests/fixtures/audio_mixer_1381/resolume-onset-0540-0630.txt.gz",
    "tests/fixtures/av-step-1267/stream-step-constant-pin.txt",
    "tests/fixtures/av-step-1267/stream-repin-window.txt",
    "tests/python/fixtures/audio_ref_band_mbc_1265.txt",
    "tests/fixtures/arrival_floor_1168/recording-e2e-659887078/qr-align-strih-659887078.log",
    "tests/fixtures/arrival_floor_1168/recording-e2e-1363366080/qr-align-strih-1363366080.log",
)
_BOUND_HEAD = 4096
_BOUND_TAIL = 32768

# The parsers that take only the log text.
_TEXT_PARSERS = (
    "obs_version_from_log",
    "distroav_version_from_log",
    "output_fps_from_log",
    "genlock_wall_clock_from_log",
    "genlock_capability_from_log",
    "genlock_lock_facet_from_log",
    "audio_telemetry_from_log",
    "audio_ts_lag_ms_from_log",
    "program_render_lagged_from_log",
    "relock_bursts_from_log",
    "audio_ref_band_from_log",
    "av_offset_series_from_log",
    "av_offset_dock_live_age_from_log",
    "av_offset_quality_from_log",
    "av_offset_quality_age_from_log",
    "buffered_ms_series_from_log",
    "audio_mixer_from_log",
    "vban_pacer_loss_from_log",
)

_AHK = (
    'app1_run  := 1\n'
    'app1_path := "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\OBS Studio.lnk"\n'
    'app1_binarypath := "D:\\_APPS\\1ME-obs\\1ME.lnk"\n'
    'app2_run  := 0\n'
)
_TASKLIST = (
    '"System Idle Process","0","Services","0","8 K"\n'
    '"obs64.exe","4242","Console","1","512,000 K"\n'
    '"obs64.exe","58560","Console","1","45 K"\n'
    '"VBAudioMatrix_Setup.exe","77","Console","1","1,000 K"\n'
    '"VBAudioMatrix_x64.exe","9001","Console","1","18,236 K"\n'
)
_NDI_INPUTS = {
    "NDI cam1": {"kind": "ndi_source", "settings": {"genlock_fifo": True, "latency": 3}},
    "NDI 2ME PGM": {"kind": "ndi_source", "settings": {"genlock_fifo": True, "latency": 963}},
    "Lyrics": {"kind": "ndi_source", "settings": {"latency": 0}},
    "cg": {"kind": "ndi_source", "settings": {"genlock_fifo": True}},
}
_NOW_TOD = (0.0, 43200.0, 86399.5)


def _load(rel):
    raw = (_ROOT / rel).read_bytes()
    if rel.endswith(".gz"):
        raw = gzip.decompress(raw)
    return raw.decode("utf-8", errors="replace")


def _cases(tmp_dir):
    """name -> log text: every recorded log, the synthetic one, their concatenation, and the
    head+tail bounded read of each text large enough to be cut."""
    whole = {pathlib.Path(rel).name: _load(rel) for rel in _RECORDED}
    whole["all_families.txt"] = (_FX / "all_families.txt").read_text(encoding="utf-8")
    whole["concat"] = "\n".join(whole.values())
    cases = dict(whole)
    for name, text in whole.items():
        path = os.path.join(tmp_dir, "bounded-" + name)
        with open(path, "wb") as fh:
            fh.write(text.encode("utf-8"))
        bounded = bsg.read_bounded_log_text(path, _BOUND_HEAD, _BOUND_TAIL)
        if bounded != text:
            cases["bounded:" + name] = bounded
    return cases


def _sha(obj):
    return hashlib.sha256(json.dumps(obj).encode("utf-8")).hexdigest()


def _facets(text):
    out = {name: getattr(bsg, name)(text) for name in _TEXT_PARSERS}
    out["audio_ref_band_from_log[ASIO]"] = bsg.audio_ref_band_from_log(
        text, ref_src="ASIO Input Capture")
    out["buffered_ms_series_from_log[ASIO]"] = bsg.buffered_ms_series_from_log(
        text, ref_src="ASIO Input Capture")
    stamped, head = bsg.timestamped_tail_lines(text)
    out["timestamped_tail_lines"] = [len(stamped), head, _sha(stamped)]
    out["audio_mixer_from_log[tail]"] = bsg.audio_mixer_from_log(text, tail=(stamped, head))
    out["vban_pacer_loss_from_log[tail]"] = bsg.vban_pacer_loss_from_log(text, tail=(stamped, head))
    for now in _NOW_TOD:
        out[f"obs_log_head_age_s_from_log[{now}]"] = bsg.obs_log_head_age_s_from_log(text, now)
    out["text_sha256"] = hashlib.sha256(text.encode("utf-8")).hexdigest()
    return json.loads(json.dumps(out))


def _host_tree(tmp_dir):
    """A tmp tree for the Windows-path file scans (DistroAV dll, OBS installs, VB-Matrix, dlls)."""
    root = pathlib.Path(tmp_dir) / "host"
    (root / "plugins" / "obs-plugins" / "64bit").mkdir(parents=True, exist_ok=True)
    (root / "plugins" / "obs-plugins" / "64bit" / "distroav.dll").write_bytes(b"distroav-bytes")
    (root / "apps" / "_RETIRED_1ME-obs").mkdir(parents=True, exist_ok=True)
    (root / "apps" / "obs64.exe").write_bytes(b"x")
    (root / "apps" / "_RETIRED_1ME-obs" / "1ME.exe").write_bytes(b"x")
    (root / "apps" / "notes.txt").write_bytes(b"x")
    (root / "vbm").mkdir(exist_ok=True)
    (root / "vbm" / "VBAudioMatrix_x64.exe").write_bytes(b"x")
    (root / "obs.dll").write_bytes(b"obs-bytes")
    (root / "GENLOCK_BUILD_SHA.txt").write_text("\n  abc1234def trailing-comment\n", encoding="utf-8")
    return root


@contextlib.contextmanager
def _server_env(host, windows, logs):
    """Pin every non-deterministic / Windows-only leaf of the server gather."""
    with contextlib.ExitStack() as st:
        st.enter_context(mock.patch.object(bss, "IS_WINDOWS", windows))
        st.enter_context(mock.patch.object(bss, "gather_ndi_inputs",
                                           lambda host_, pw: json.loads(json.dumps(_NDI_INPUTS))))
        st.enter_context(mock.patch.object(bss.bsg, "local_seconds_of_day", lambda *a: 43200.0))
        st.enter_context(mock.patch.object(
            bss, "port4455_owner",
            lambda: (r"C:\Program Files\obs-studio\bin\64bit\obs64.exe", "32.1.2")))
        st.enter_context(mock.patch.object(bss, "read_ahk_text", lambda p: _AHK))
        st.enter_context(mock.patch.object(
            bss, "resolve_shortcut",
            lambda p: (r"C:\Program Files\obs-studio\bin\64bit\obs64.exe",
                       r"C:\Program Files\obs-studio\bin\64bit")))
        st.enter_context(mock.patch.object(bss, "ndi_runtime_version", lambda p: "6.2.1.0"))
        st.enter_context(mock.patch.object(bss, "tasklist_csv", lambda: _TASKLIST))
        st.enter_context(mock.patch.object(bss, "vb_matrix_start_time",
                                           lambda pid: "2026-09-28T01:02:03" if pid else ""))
        st.enter_context(mock.patch.object(bss, "DEFAULT_VB_MATRIX_INSTALL_DIRS",
                                           (str(host / "vbm"),)))
        st.enter_context(mock.patch.object(bss, "log", logs.append))
        env = {k: v for k, v in os.environ.items() if k not in ("AUDIO_REF_BAND_SRC",)}
        env["BUNDLE_STATE_TIMING"] = "1"
        st.enter_context(mock.patch.dict(os.environ, env, clear=True))
        yield


def _server_payload(text, tmp_dir, host, windows):
    """The served JSON text + the BUNDLE_STATE_TIMING key order, tmp paths normalized."""
    log_dir = pathlib.Path(tmp_dir) / "logs"
    log_dir.mkdir(exist_ok=True)
    (log_dir / "obs.txt").write_bytes(text.encode("utf-8"))
    logs = []
    with _server_env(host, windows, logs):
        state = bss.gather_bundle_state(
            "127.0.0.1", "", str(log_dir), str(host / "ndi.dll"),
            [str(host / "plugins"), str(host / "missing-root")],
            genlock_build_sha_file=str(host / "GENLOCK_BUILD_SHA.txt"),
            obs_install_scan_roots=(str(host / "apps"),),
            startup_shortcut=str(host / "OBS Studio.lnk"),
            ahk_path=str(host / "NL_STARTUP.ahk"),
            obs_dll_path=str(host / "obs.dll"),
        )
    timing = [m for m in logs if m.startswith("gather timing:")]
    keys = re.findall(r"(\w+)=[\d.]+s", timing[-1]) if timing else []
    return json.dumps(state).replace(json.dumps(str(tmp_dir))[1:-1], "<TMP>"), keys


def _host_facets(tmp_dir, host):
    b = bsg
    out = {
        "ndi_input_latency_csv": b.ndi_input_latency_csv(_NDI_INPUTS),
        "ndi_input_latency_csv[none]": b.ndi_input_latency_csv(None),
        "obs_process_count_from_listing": [b.obs_process_count_from_listing(t) for t in
                                           ("obs64\nOBS\nexplorer\nobs32 \n", "", "  \n")],
        "tasklist_mem_kb": [b.tasklist_mem_kb(f) for f in
                            ("512,000 K", "45 K", "N/A", "", None, "1 024 K", "-5 K")],
        "tasklist_row_is_live_obs": [b.tasklist_row_is_live_obs(f) for f in
                                     ("512,000 K", "45 K", "1,024 K", "N/A")],
        "vb_matrix_process_from_listing": [b.vb_matrix_process_from_listing(t) for t in
                                           (_TASKLIST, "", '"x.exe","1"\n', '"a\n')],
        "vb_matrix_running_facet": [b.vb_matrix_running_facet(i, p) for i in (True, False)
                                    for p in (None, ("", ""), ("VBAudioMatrix_x64", "9001"))],
        "vb_matrix_install_present_under": [
            b.vb_matrix_install_present_under([str(host / "vbm")]),
            b.vb_matrix_install_present_under([str(host / "apps"), ""]),
            b.vb_matrix_install_present_under(None)],
        "ahk": [f(t) for f in (b.ahk_app1_shortcut_path, b.ahk_app1_run, b.ahk_dead_config_present)
                for t in (_AHK, "", "app2_run := 1\n", "app1_run := 0\n")],
        "recordings_free_verdict": [b.recordings_free_verdict(fb, 50) for fb in
                                    (None, 49_999_999_999, 50_000_000_000, 6e11)],
        "recordings_free_line": [b.recordings_free_line(t, g) for t, g in (
            ('{"free_bytes": 12345678901}', 50), ('{"free_bytes": 60000000000}', "50"),
            ('{"free_bytes": null}', 50), ('{"free_bytes": true}', 50), ("[]", 50),
            ("", 50), ("not json", 50))],
        "genlock_build_sha_from_file": [b.genlock_build_sha_from_file(p) for p in (
            str(host / "GENLOCK_BUILD_SHA.txt"), str(host / "absent.txt"), "", None)],
        "component_sha256": [b.component_sha256(p) for p in (
            str(host / "obs.dll"), str(host / "absent.dll"), str(host), "", None)],
        "distroav_dll_paths": b.distroav_dll_paths(
            [str(host / "plugins"), "", str(host / "absent")]),
        "obs_installs_under": b.obs_installs_under([str(host / "apps"), str(host / "absent")]),
    }
    return json.loads(json.dumps(out).replace(json.dumps(str(tmp_dir))[1:-1], "<TMP>"))


def _build_state_cases(keys):
    full = bsg.build_bundle_state(**{k: f"v{i}" for i, k in enumerate(keys)})
    half = bsg.build_bundle_state(**{k: ("" if i % 2 else "0") for i, k in enumerate(keys)})
    return {"full": json.dumps(full), "half": json.dumps(half),
            "empty": json.dumps(bsg.build_bundle_state())}


def _public_names():
    return sorted(n for n, v in vars(bsg).items()
                  if not n.startswith("_") and not inspect.ismodule(v) and n != "annotations")


def compute_golden(tmp_dir, keys=None):
    host = _host_tree(tmp_dir)
    cases = _cases(tmp_dir)
    golden = {"cases": {}, "server": {}, "timing_keys": {}}
    for name, text in cases.items():
        golden["cases"][name] = _facets(text)
        for windows in (False, True):
            payload, timing = _server_payload(text, tmp_dir, host, windows)
            golden["server"][f"{name}|windows={windows}"] = payload
            golden["timing_keys"][f"windows={windows}"] = timing
    golden["host"] = _host_facets(tmp_dir, host)
    if keys is None:
        # A first capture: the pre-split code's keyword-only parameters, in signature order.
        keys = [p for p in inspect.signature(bsg.build_bundle_state).parameters]
    golden["build_bundle_state_keys"] = keys
    golden["build_bundle_state"] = _build_state_cases(keys)
    golden["public_names"] = _public_names()
    return golden


def refresh_golden(tmp_dir):
    """What `--write-golden` writes: the outputs recomputed over the SAME inputs. The keyword list
    fed to build_bundle_state is an input (the historic signature order), so it is reused from the
    committed golden; only a first capture (no golden yet) reads it from the signature."""
    keys = None
    if _GOLDEN.is_file():
        keys = json.loads(_GOLDEN.read_text(encoding="utf-8"))["build_bundle_state_keys"]
    return compute_golden(tmp_dir, keys=keys)


# --- the proof -----------------------------------------------------------------------------------

@pytest.fixture(scope="module")
def golden():
    return json.loads(_GOLDEN.read_text(encoding="utf-8"))


@pytest.fixture(scope="module")
def actual(golden, tmp_path_factory):
    return compute_golden(str(tmp_path_factory.mktemp("split1386")),
                          keys=golden["build_bundle_state_keys"])


def test_every_facet_parser_is_byte_identical(golden, actual):
    assert sorted(actual["cases"]) == sorted(golden["cases"])
    for name in golden["cases"]:
        assert actual["cases"][name] == golden["cases"][name], name


def test_served_json_is_byte_identical_on_both_gather_paths(golden, actual):
    assert sorted(actual["server"]) == sorted(golden["server"])
    for name in golden["server"]:
        assert actual["server"][name] == golden["server"][name], name


def test_timing_breakdown_keeps_its_key_order(golden, actual):
    assert actual["timing_keys"] == golden["timing_keys"]


def test_host_facets_are_identical(golden, actual):
    assert actual["host"] == golden["host"]


def test_build_bundle_state_accepts_every_original_keyword_in_order(golden, actual):
    assert actual["build_bundle_state"] == golden["build_bundle_state"]


def test_every_original_public_name_still_imports(golden):
    missing = [n for n in golden["public_names"] if not hasattr(bsg, n)]
    assert missing == []


def test_the_golden_refresh_reproduces_the_committed_golden(tmp_path):
    # `--write-golden` (refresh_golden) must reproduce every committed output, so the documented
    # refresh command keeps working on the split code. The public-name list may only grow (the
    # split adds BUNDLE_STATE_KEYS), never lose a name.
    fresh = refresh_golden(str(tmp_path))
    committed = json.loads(_GOLDEN.read_text(encoding="utf-8"))
    assert set(committed["public_names"]) <= set(fresh.pop("public_names"))
    committed.pop("public_names")
    assert fresh == committed


def test_build_bundle_state_rejects_an_unknown_keyword_and_positionals():
    with pytest.raises(TypeError):
        bsg.build_bundle_state(not_a_facet="x")
    with pytest.raises(TypeError):
        bsg.build_bundle_state("x")


# --- the facade contract (added with the split) --------------------------------------------------

_FAMILIES = ("bundle_state_log", "bundle_state_genlock", "bundle_state_audio", "bundle_state_vban",
             "bundle_state_av_offset", "bundle_state_host")
# private helpers the tests call through the facade (test_relock_bursts_gather_1320.py)
_PRIVATE_REEXPORTS = ("_parse_relock_event", "_summarize_relock_bursts")


def test_the_facade_reexports_through_an_explicit_list(golden):
    names = list(vars(bsg).get("__all__", ()))
    assert names, "bundle_state_gather must declare its re-exports in an explicit __all__"
    assert len(names) == len(set(names)), "a duplicate __all__ entry"
    assert set(golden["public_names"]) | set(_PRIVATE_REEXPORTS) <= set(names), (
        "every name the pre-split module exposed (+ the tested private helpers) must stay importable "
        "from bundle_state_gather")
    for n in names:
        assert hasattr(bsg, n), f"__all__ names {n!r} but the facade does not bind it"


def test_every_reexport_is_the_family_object_not_a_copy():
    for fam in _FAMILIES:
        mod = importlib.import_module(fam)
        for n in bsg.__all__:
            if n in vars(mod):
                assert getattr(bsg, n) is vars(mod)[n], f"{n} re-exported from {fam} is a copy"


_DEFS = (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)


def _span(node):
    return node.end_lineno - node.lineno + 1


def _own_lines(fn):
    """A function's own lines: its span minus its docstring and minus every def / class nested in
    it (each is measured on its own), so a handler-class factory is not charged for its methods."""
    n = _span(fn)
    first = fn.body[0]
    if isinstance(first, ast.Expr) and isinstance(first.value, ast.Constant) \
            and isinstance(first.value.value, str):
        n -= _span(first)
    stack = list(ast.iter_child_nodes(fn))
    while stack:
        node = stack.pop()
        if isinstance(node, _DEFS):
            n -= _span(node)
        else:
            stack.extend(ast.iter_child_nodes(node))
    return n


def _functions(path):
    tree = ast.parse(path.read_text(encoding="utf-8"))
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            yield node.name, _own_lines(node)


def test_the_split_files_and_functions_stay_inside_the_budget():
    files = [_SCRIPTS / "bundle_state_gather.py"] + [_SCRIPTS / f"{f}.py" for f in _FAMILIES]
    for path in files:
        n = path.read_text(encoding="utf-8").count("\n")
        assert n <= 800, f"{path.name} is {n} lines (budget 800): split it by responsibility"
    for path in files + [_SCRIPTS / "bundle-state-server.py"]:
        for name, n in _functions(path):
            assert n <= 100, f"{path.name}:{name} is {n} own lines (budget 100): cut it into helpers"
    # The server itself is still over the ~1000-line file budget (issue 1386 scoped its server work
    # to cutting gather_bundle_state). It must not grow further: a change that needs more room moves
    # the Windows-only identity readers (port4455 / tasklist / VB-Matrix start / shortcut / AHK /
    # NDI runtime + their caches) into a bundle_state_* module and lists it in bundle-state-files.
    server = (_SCRIPTS / "bundle-state-server.py").read_text(encoding="utf-8").count("\n")
    assert server <= 1110, f"bundle-state-server.py grew to {server} lines (ratchet 1110): split it"


if __name__ == "__main__" and sys.argv[1:] == ["--write-golden"]:
    with tempfile.TemporaryDirectory() as d:
        data = refresh_golden(d)
    _GOLDEN.write_text(json.dumps(data, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    print(f"wrote {_GOLDEN} ({len(data['cases'])} log cases)")
