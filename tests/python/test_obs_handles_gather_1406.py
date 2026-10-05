"""issue 1406 -- the `obs_handles` bundle-state facet: the OBS process's handle count (Windows) or
open-fd count (Linux strih-lx), its pid + start time (a restart resets the dev1 baseline) and, on
Linux, its soft open-files limit.

WHY: on 5.10.2026 the stream OBS held 4,066,772 handles after ~22 h, growing one registry key per
audio tick (46.875/s) because the Audio Monitor plugin kept opening an absent Focusrite endpoint.
Nothing in camera-box read a handle count, so the leak was found by accident ~100 h before the
16.7M per-process cap. This facet makes the count visible to the dev1 obs-handles watchdog.

Pieces under test (Tier-0, stdlib only, no Windows box needed):
  * `bsg.system_processes_from_spi` -- the PURE x64 SYSTEM_PROCESS_INFORMATION buffer parser the
    Windows reader feeds; its field offsets are cross-checked against the documented C layout built
    with ctypes, so a wrong hard-coded offset fails here, not on the box;
  * `bsg.obs_handles_facet` -- picks the live OBS-shaped process with the most handles;
  * the `/proc` parsers + `bsg.linux_obs_handles(proc_root)` over a fake /proc tree;
  * `bundle_state_windows.windows_obs_handles` with its ctypes leaf faked;
  * the server wiring on both gather paths (facet served at the end of the payload, omitted when
    unreadable -- never a false 0).
"""
from __future__ import annotations

import ctypes
import importlib.util
import json
import os
import pathlib
import struct
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402
import bundle_state_windows as bsw  # noqa: E402

_spec = importlib.util.spec_from_file_location("bundle_state_server_obs_handles_1406",
                                               _SCRIPTS / "bundle-state-server.py")
bss = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bss)

_FACET_KEYS = ("obs_handles", "obs_handles_pid", "obs_handles_start", "obs_handles_limit")

# --- the x64 SYSTEM_PROCESS_INFORMATION layout ---------------------------------------------------

_ENTRY = 256       # bytes per synthetic entry (the real one is longer; NextEntryOffset walks it)
_NAME_AT = 160     # where a synthetic entry stores its UTF-16 image name
_BASE = 0x2_0000_0000  # a fake buffer address: name pointers are absolute, as in the real buffer


class _UnicodeString(ctypes.Structure):
    _fields_ = [("Length", ctypes.c_uint16), ("MaximumLength", ctypes.c_uint16),
                ("Buffer", ctypes.c_void_p)]


class _SystemProcessInformation(ctypes.Structure):
    # The documented x64 layout (winternl.h; CreateTime inside the documented Reserved1 block, as
    # NT has laid it out since Windows 7) with explicit-width types so it is the same on this LP64
    # host as on Windows x64.
    _fields_ = [
        ("NextEntryOffset", ctypes.c_uint32),
        ("NumberOfThreads", ctypes.c_uint32),
        ("WorkingSetPrivateSize", ctypes.c_int64),
        ("HardFaultCount", ctypes.c_uint32),
        ("NumberOfThreadsHighWatermark", ctypes.c_uint32),
        ("CycleTime", ctypes.c_uint64),
        ("CreateTime", ctypes.c_int64),
        ("UserTime", ctypes.c_int64),
        ("KernelTime", ctypes.c_int64),
        ("ImageName", _UnicodeString),
        ("BasePriority", ctypes.c_int32),
        ("UniqueProcessId", ctypes.c_void_p),
        ("InheritedFromUniqueProcessId", ctypes.c_void_p),
        ("HandleCount", ctypes.c_uint32),
        ("SessionId", ctypes.c_uint32),
    ]


def _filetime(epoch_s):
    return (epoch_s + 11_644_473_600) * 10_000_000


def _spi(entries, base=_BASE):
    """A synthetic SystemProcessInformation buffer: one _ENTRY-byte block per process, the last one
    with NextEntryOffset 0, image names stored inside each block behind absolute pointers."""
    raw = bytearray(_ENTRY * len(entries))
    for i, e in enumerate(entries):
        off = i * _ENTRY
        name = e.get("name", "").encode("utf-16-le")
        nxt = 0 if i == len(entries) - 1 else _ENTRY
        struct.pack_into("<I", raw, off + 0, nxt)
        struct.pack_into("<I", raw, off + 4, e.get("threads", 12))
        struct.pack_into("<q", raw, off + 32, e.get("create", 0))
        struct.pack_into("<HH", raw, off + 56, len(name), len(name) + 2)
        struct.pack_into("<Q", raw, off + 64, (base + off + _NAME_AT) if name else 0)
        struct.pack_into("<Q", raw, off + 80, e["pid"])
        struct.pack_into("<I", raw, off + 96, e["handles"])
        raw[off + _NAME_AT: off + _NAME_AT + len(name)] = name
    return bytes(raw)


def test_the_parser_offsets_match_the_documented_c_layout():
    s = _SystemProcessInformation
    assert ctypes.sizeof(ctypes.c_void_p) == 8, "the layout check needs a 64-bit interpreter"
    assert bsg.SPI_OFFSETS == {
        "next": s.NextEntryOffset.offset,
        "threads": s.NumberOfThreads.offset,
        "create_time": s.CreateTime.offset,
        "name_length": s.ImageName.offset + _UnicodeString.Length.offset,
        "name_buffer": s.ImageName.offset + _UnicodeString.Buffer.offset,
        "pid": s.UniqueProcessId.offset,
        "handles": s.HandleCount.offset,
    }
    # The documented winternl.h public offsets of the three fields the facet needs.
    assert (s.ImageName.offset, s.UniqueProcessId.offset, s.HandleCount.offset) == (56, 80, 96)


def test_spi_parser_reads_every_process_in_the_buffer():
    raw = _spi([
        {"pid": 0, "name": "", "handles": 0, "threads": 8},
        {"pid": 4, "name": "System", "handles": 6000},
        {"pid": 9224, "name": "obs64.exe", "handles": 4_066_772,
         "create": _filetime(1_791_110_100)},
    ])
    procs = bsg.system_processes_from_spi(raw, _BASE)
    assert procs == [
        {"pid": 0, "name": "", "handles": 0, "threads": 8, "start": None},
        {"pid": 4, "name": "System", "handles": 6000, "threads": 12, "start": None},
        {"pid": 9224, "name": "obs64.exe", "handles": 4_066_772, "threads": 12,
         "start": 1_791_110_100},
    ]


@pytest.mark.parametrize("raw", [b"", b"\x00" * 40])
def test_spi_parser_reads_a_short_buffer_as_unreadable(raw):
    assert bsg.system_processes_from_spi(raw, _BASE) is None


def test_spi_parser_refuses_an_entry_offset_that_overlaps_the_header():
    raw = bytearray(_spi([{"pid": 4, "name": "System", "handles": 1},
                          {"pid": 8, "name": "obs64.exe", "handles": 2}]))
    struct.pack_into("<I", raw, 0, 16)   # NextEntryOffset inside the first header
    assert bsg.system_processes_from_spi(bytes(raw), _BASE) is None


def test_spi_parser_refuses_an_entry_that_runs_past_the_buffer():
    raw = _spi([{"pid": 4, "name": "System", "handles": 1},
                {"pid": 8, "name": "obs64.exe", "handles": 2}])
    assert bsg.system_processes_from_spi(raw[: _ENTRY + 50], _BASE) is None


def test_spi_parser_leaves_a_name_pointing_outside_the_buffer_empty():
    raw = bytearray(_spi([{"pid": 8, "name": "obs64.exe", "handles": 2}]))
    struct.pack_into("<Q", raw, 64, _BASE + 10 * _ENTRY)
    assert bsg.system_processes_from_spi(bytes(raw), _BASE)[0]["name"] == ""


def test_filetime_to_epoch():
    assert bsg.filetime_to_epoch(_filetime(1_791_197_100)) == 1_791_197_100
    assert bsg.filetime_to_epoch(0) is None
    assert bsg.filetime_to_epoch(-5) is None


# --- choosing the OBS process -------------------------------------------------------------------

def test_facet_takes_the_live_obs_process_with_the_most_handles():
    procs = [
        {"pid": 4, "name": "System", "handles": 9_000_000, "threads": 200, "start": None},
        {"pid": 58560, "name": "obs64.exe", "handles": 3, "threads": 0, "start": 1},
        {"pid": 9224, "name": "obs64.exe", "handles": 4_066_772, "threads": 90,
         "start": 1_791_110_100},
        {"pid": 77, "name": "obs-ffmpeg-mux.exe", "handles": 9_999_999, "threads": 3, "start": 5},
    ]
    assert bsg.obs_handles_facet(procs) == {
        "obs_handles": "4066772", "obs_handles_pid": "9224",
        "obs_handles_start": "1791110100", "obs_handles_limit": "",
    }


def test_facet_is_empty_without_a_live_obs_process():
    assert bsg.obs_handles_facet([]) == {}
    assert bsg.obs_handles_facet(None) == {}
    dead = [{"pid": 58560, "name": "obs64.exe", "handles": 3, "threads": 0, "start": 1}]
    assert bsg.obs_handles_facet(dead) == {}


def test_facet_carries_the_linux_limit_and_a_missing_start_as_empty():
    procs = [{"pid": 4242, "name": "obs", "handles": 7, "start": None, "limit": 1024}]
    assert bsg.obs_handles_facet(procs) == {
        "obs_handles": "7", "obs_handles_pid": "4242", "obs_handles_start": "",
        "obs_handles_limit": "1024",
    }


# --- the Linux /proc read -------------------------------------------------------------------------

_LIMITS = (
    "Limit                     Soft Limit           Hard Limit           Units     \n"
    "Max cpu time              unlimited            unlimited            seconds   \n"
    "Max open files            {soft:<20} 524288               files     \n"
    "Max locked memory         8388608              8388608              bytes     \n"
)


def _stat_line(pid, comm, start_ticks):
    rest = ["S"] + [str(i) for i in range(4, 22)] + [str(start_ticks)] + ["0"] * 30
    return f"{pid} ({comm}) " + " ".join(rest) + "\n"


def _fake_proc(root, procs, btime=1_791_190_000):
    (root).mkdir()
    (root / "stat").write_text(f"cpu  1 2 3 4\nintr 5\nbtime {btime}\nprocesses 99\n")
    (root / "self").mkdir()
    for p in procs:
        d = root / str(p["pid"])
        d.mkdir()
        (d / "comm").write_text(p["comm"] + "\n")
        (d / "stat").write_text(_stat_line(p["pid"], p["comm"], p.get("ticks", 360_000)))
        if "soft" in p:
            (d / "limits").write_text(_LIMITS.format(soft=p["soft"]))
        if "fds" in p:
            (d / "fd").mkdir()
            for i in range(p["fds"]):
                (d / "fd" / str(i)).write_text("")
    return root


def test_proc_parsers():
    assert bsg.proc_stat_start_ticks(_stat_line(4242, "obs", 360_000)) == 360_000
    assert bsg.proc_stat_start_ticks(_stat_line(7, "a) (b", 99)) == 99
    assert bsg.proc_stat_start_ticks("garbage") is None
    assert bsg.proc_stat_start_ticks("") is None
    assert bsg.proc_btime("cpu 1\nbtime 1791190000\n") == 1_791_190_000
    assert bsg.proc_btime("cpu 1\n") is None
    assert bsg.proc_nofile_soft_limit(_LIMITS.format(soft="1024")) == 1024
    assert bsg.proc_nofile_soft_limit(_LIMITS.format(soft="unlimited")) is None
    assert bsg.proc_nofile_soft_limit("") is None


def test_linux_reader_counts_the_obs_fds(tmp_path):
    root = _fake_proc(tmp_path / "proc", [
        {"pid": 4242, "comm": "obs", "fds": 7, "soft": "1024", "ticks": 360_000},
        {"pid": 77, "comm": "bash", "fds": 3, "soft": "1024"},
        {"pid": 88, "comm": "obs-ffmpeg-mux", "fds": 40, "soft": "1024"},
        {"pid": 99, "comm": "obs"},          # another user's / a vanished OBS: no readable fd dir
    ])
    assert bsg.linux_obs_handles(str(root), clk_tck=100) == {
        "obs_handles": "7", "obs_handles_pid": "4242",
        "obs_handles_start": str(1_791_190_000 + 3600), "obs_handles_limit": "1024",
    }


def test_linux_reader_takes_the_largest_of_two_obs_and_drops_an_unlimited_limit(tmp_path):
    root = _fake_proc(tmp_path / "proc", [
        {"pid": 10, "comm": "obs", "fds": 2, "soft": "1024"},
        {"pid": 11, "comm": "obs", "fds": 5, "soft": "unlimited"},
    ])
    facet = bsg.linux_obs_handles(str(root), clk_tck=100)
    assert facet["obs_handles"] == "5" and facet["obs_handles_pid"] == "11"
    assert facet["obs_handles_limit"] == ""


def test_linux_reader_without_obs_or_proc_is_empty(tmp_path):
    root = _fake_proc(tmp_path / "proc", [{"pid": 77, "comm": "bash", "fds": 3}])
    assert bsg.linux_obs_handles(str(root), clk_tck=100) == {}
    assert bsg.linux_obs_handles(str(tmp_path / "no-proc"), clk_tck=100) == {}


def test_linux_reader_without_btime_still_counts(tmp_path):
    root = _fake_proc(tmp_path / "proc", [{"pid": 12, "comm": "obs", "fds": 4}])
    (root / "stat").write_text("cpu 1\n")
    facet = bsg.linux_obs_handles(str(root), clk_tck=100)
    assert facet["obs_handles"] == "4" and facet["obs_handles_start"] == ""
    assert facet["obs_handles_limit"] == ""


# --- the Windows reader ---------------------------------------------------------------------------

def test_windows_reader_builds_the_facet_from_the_process_snapshot(monkeypatch):
    raw = _spi([{"pid": 4, "name": "System", "handles": 6000},
                {"pid": 5748, "name": "obs64.exe", "handles": 5790,
                 "create": _filetime(1_791_197_100)}])
    monkeypatch.setattr(bsw, "system_process_information", lambda: (raw, _BASE))
    assert bsw.windows_obs_handles() == {
        "obs_handles": "5790", "obs_handles_pid": "5748",
        "obs_handles_start": "1791197100", "obs_handles_limit": "",
    }


def test_windows_reader_is_empty_when_the_snapshot_fails_or_is_malformed(monkeypatch):
    lines = []
    monkeypatch.setattr(bsw, "log", lines.append)
    monkeypatch.setattr(bsw, "system_process_information", lambda: None)
    assert bsw.windows_obs_handles() == {}
    monkeypatch.setattr(bsw, "system_process_information", lambda: (b"\x00" * 10, _BASE))
    assert bsw.windows_obs_handles() == {}
    assert any("WARNING" in line for line in lines)


@pytest.mark.skipif(os.name == "nt", reason="the non-Windows degrade path")
def test_the_ctypes_snapshot_degrades_off_windows(monkeypatch):
    lines = []
    monkeypatch.setattr(bsw, "log", lines.append)
    assert bsw.system_process_information() is None
    assert lines and "WARNING" in lines[0]


# --- the server wiring ----------------------------------------------------------------------------

def test_the_payload_declares_the_facet_keys_last():
    assert bsg.BUNDLE_STATE_KEYS[-len(_FACET_KEYS):] == _FACET_KEYS


def _gather(monkeypatch, tmp_path, windows, facet):
    monkeypatch.setattr(bss, "IS_WINDOWS", windows)
    monkeypatch.setattr(bss, "gather_ndi_inputs", lambda host, password: {})
    monkeypatch.setattr(bss, "_windows_identity_facets", lambda timings, **kw: {})
    calls = []

    def reader(*a, **k):
        calls.append("called")
        return dict(facet)

    monkeypatch.setattr(bss, "windows_obs_handles", reader if windows else _fail)
    monkeypatch.setattr(bss.bsg, "linux_obs_handles", _fail if windows else reader)
    log_dir = tmp_path / "logs"
    log_dir.mkdir(exist_ok=True)
    (log_dir / "obs.txt").write_text("12:00:00.000: OBS 32.2.0 (64-bit, windows)\n")
    state = bss.gather_bundle_state("127.0.0.1", "", str(log_dir), str(tmp_path / "ndi.dll"), [],
                                    genlock_build_sha_file=str(tmp_path / "sha.txt"))
    assert calls == ["called"]
    return state


def _fail(*a, **k):
    raise AssertionError("the other platform's reader must not run")


@pytest.mark.parametrize("windows", [False, True])
def test_the_server_serves_the_facet_on_both_platforms(monkeypatch, tmp_path, windows):
    facet = {"obs_handles": "5790", "obs_handles_pid": "5748",
             "obs_handles_start": "1791197100", "obs_handles_limit": "" if windows else "1024"}
    state = _gather(monkeypatch, tmp_path, windows, facet)
    served = {k: state[k] for k in _FACET_KEYS if k in state}
    assert served == {k: v for k, v in facet.items() if v}
    keys = list(json.loads(json.dumps(state)))
    assert keys[-len(served):] == [k for k in _FACET_KEYS if facet[k]]


@pytest.mark.parametrize("windows", [False, True])
def test_the_server_omits_an_unreadable_facet(monkeypatch, tmp_path, windows):
    state = _gather(monkeypatch, tmp_path, windows, {})
    assert not any(k in state for k in _FACET_KEYS), "unreadable must be ABSENT, never a false 0"
