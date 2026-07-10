"""#650 — unit tests for scripts/bundle_state_gather.py, the PURE parsers/builders behind the
standing :8899 bundle-state HTTP service (version-integrity-gate.sh + recording-e2e.sh's
unattended CI fetch). No live OBS / no live box needed — same "source parsers, verify live
separately" split as tests/drift_guard.rs (drift-guard.sh) and test_obs_burn_filter.py
(obs_burn_filter.py's compute_burn_on).
"""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402


# A trimmed real-shaped OBS log excerpt (the fields drift-guard.md step 1 scans for).
SAMPLE_LOG = """\
14:57:21.476: Portable mode: false
14:57:22.001: OBS 32.1.2 (64-bit, windows)
14:57:22.500: [obs-websocket] Server started successfully on 0.0.0.0:4455
14:57:23.010: DistroAV (Version 6.2.1)
14:57:23.200: video settings reset:
14:57:23.201: 	base resolution:   1920x1080
14:57:23.202: 	output resolution: 1920x1080
14:57:23.203: 	downscale filter:  Bicubic
14:57:23.204: 	fps:               30/1
14:57:23.205: 	format:            NV12
14:57:24.000: genlock: wall-clock-slaved render tick ENABLED
14:57:24.001: genlock: sub-frame jitter reserve engaged
"""


def test_obs_version_from_log_found():
    assert bsg.obs_version_from_log(SAMPLE_LOG) == "32.1.2"


def test_obs_version_from_log_absent():
    assert bsg.obs_version_from_log("nothing relevant here") == ""
    assert bsg.obs_version_from_log("") == ""
    assert bsg.obs_version_from_log(None) == ""


def test_distroav_version_from_log_found():
    assert bsg.distroav_version_from_log(SAMPLE_LOG) == "6.2.1"


def test_distroav_version_from_log_absent():
    assert bsg.distroav_version_from_log("no distroav line") == ""


def test_output_fps_from_log_found():
    assert bsg.output_fps_from_log(SAMPLE_LOG) == "30"


def test_output_fps_from_log_no_reset_block():
    assert bsg.output_fps_from_log("OBS 32.1.2\nno reset block here\n") == ""


def test_output_fps_from_log_reset_block_but_no_fps_line():
    # A reset block whose lines never carry an "fps: N/1" line -> UNKNOWN, not a stale guess.
    text = "video settings reset:\nbase resolution: 1920x1080\n"
    assert bsg.output_fps_from_log(text) == ""


def test_output_fps_from_log_uses_first_reset_block_only():
    # Two reset blocks -> the first one's fps line wins (mirrors the PowerShell block-then-break).
    text = (
        "video settings reset:\nfps:               60/1\n"
        "video settings reset:\nfps:               30/1\n"
    )
    assert bsg.output_fps_from_log(text) == "60"


def test_genlock_wall_clock_enabled():
    assert bsg.genlock_wall_clock_from_log(SAMPLE_LOG) == "1"


def test_genlock_wall_clock_disabled():
    text = "genlock: wall-clock-slaved render tick DISABLED\n"
    assert bsg.genlock_wall_clock_from_log(text) == "0"


def test_genlock_wall_clock_absent_is_unknown():
    assert bsg.genlock_wall_clock_from_log("no genlock marker at all") == ""


def test_genlock_capability_from_log_joins_every_marker_line():
    cap = bsg.genlock_capability_from_log(SAMPLE_LOG)
    assert "render tick ENABLED" in cap
    assert "sub-frame jitter reserve" in cap
    assert cap.count("\n") == 1  # exactly two matching lines in the sample


def test_genlock_capability_from_log_absent_is_empty():
    assert bsg.genlock_capability_from_log("stock OBS, no genlock lines") == ""


def test_distroav_dll_paths_finds_every_match(tmp_path):
    root_a = tmp_path / "program_files" / "obs-plugins" / "64bit"
    root_a.mkdir(parents=True)
    (root_a / "distroav.dll").write_bytes(b"x")
    root_b = tmp_path / "programdata" / "plugins" / "distroav" / "bin" / "64bit"
    root_b.mkdir(parents=True)
    (root_b / "DistroAV.dll").write_bytes(b"x")  # case-insensitive match
    (root_b / "not-it.dll").write_bytes(b"x")

    result = bsg.distroav_dll_paths([str(root_a.parent), str(root_b.parent.parent.parent)])
    paths = result.split(",")
    assert len(paths) == 2
    assert any(p.endswith("distroav.dll") for p in paths)
    assert any(p.endswith("DistroAV.dll") for p in paths)


def test_distroav_dll_paths_missing_root_is_empty():
    assert bsg.distroav_dll_paths(["/no/such/path", ""]) == ""


def test_distroav_dll_paths_none_found(tmp_path):
    empty = tmp_path / "empty"
    empty.mkdir()
    assert bsg.distroav_dll_paths([str(empty)]) == ""


NDI_INPUTS_STRIH = {
    "NDI 2ME PVW": {"kind": "ndi_source", "settings": {"latency": 0}},  # no genlock_fifo -> excluded
    "NDI cam1": {"kind": "ndi_source", "settings": {"genlock_fifo": True, "latency": 0}},
    "NDI cam3": {"kind": "ndi_source", "settings": {"genlock_fifo": True, "latency": 0}},
    "NDI cam5": {"kind": "ndi_source", "settings": {"genlock_fifo": True, "latency": 0}},
    "phase2-probe-src": {"kind": "ndi_source", "settings": {"genlock_fifo": False, "latency": 0}},
}


def test_ndi_input_latency_csv_filters_to_genlocked_inputs_only():
    csv = bsg.ndi_input_latency_csv(NDI_INPUTS_STRIH)
    assert csv == "NDI cam1=0,NDI cam3=0,NDI cam5=0"


def test_ndi_input_latency_csv_empty_when_none_genlocked():
    assert bsg.ndi_input_latency_csv({"x": {"settings": {"latency": 0}}}) == ""


def test_ndi_input_latency_csv_empty_input():
    assert bsg.ndi_input_latency_csv({}) == ""
    assert bsg.ndi_input_latency_csv(None) == ""


def test_ndi_input_latency_csv_skips_entry_missing_latency_key():
    inputs = {"NDI camX": {"settings": {"genlock_fifo": True}}}  # no "latency" key at all
    assert bsg.ndi_input_latency_csv(inputs) == ""


def test_ndi_input_latency_csv_nonbool_genlock_fifo_excluded():
    # DistroAV's genlock_fifo is always a real bool over the WS API; a stray truthy-but-not-True
    # value (e.g. the string "true") must NOT slip through — `is not True` is deliberate, not `not`.
    inputs = {"NDI camX": {"settings": {"genlock_fifo": "true", "latency": 0}}}
    assert bsg.ndi_input_latency_csv(inputs) == ""


def test_build_bundle_state_includes_only_nonempty_keys():
    state = bsg.build_bundle_state(
        obs_version="32.1.2",
        distroav_version="6.2.1",
        ndi_runtime="",
        output_fps="30",
        genlock_wall_clock="1",
        ndi_input_latency="NDI cam1=0",
        distroav_dll_paths="C:\\ProgramData\\obs-studio\\plugins\\distroav\\bin\\64bit\\distroav.dll",
        genlock_capability="",
    )
    assert state == {
        "obs_version": "32.1.2",
        "distroav_version": "6.2.1",
        "output_fps": "30",
        "genlock_wall_clock": "1",
        "ndi_input_latency": "NDI cam1=0",
        "distroav_dll_paths": "C:\\ProgramData\\obs-studio\\plugins\\distroav\\bin\\64bit\\distroav.dll",
    }
    assert "ndi_runtime" not in state
    assert "genlock_capability" not in state


def test_build_bundle_state_all_empty_yields_empty_dict():
    assert bsg.build_bundle_state() == {}


# ── #652: record_dir_stats — PURE filesystem stats behind /record-dir-stats.json ────────────────
# (the disk-budget preflight WARN in recording-e2e.sh; the harness's own E2E test recordings
# accumulated to ~500 GB on strih / 139 GB on stream, invisible until the disk nearly filled).

def test_record_dir_stats_empty_dir(tmp_path):
    stats = bsg.record_dir_stats(str(tmp_path))
    assert stats == {"total_bytes": 0, "file_count": 0, "oldest_mtime": None}


def test_record_dir_stats_sums_files_and_counts(tmp_path):
    (tmp_path / "a.mkv").write_bytes(b"x" * 100)
    (tmp_path / "b.mp4").write_bytes(b"y" * 250)
    stats = bsg.record_dir_stats(str(tmp_path))
    assert stats["total_bytes"] == 350
    assert stats["file_count"] == 2
    assert stats["oldest_mtime"] is not None


def test_record_dir_stats_ignores_subdirectories(tmp_path):
    (tmp_path / "a.mkv").write_bytes(b"x" * 10)
    sub = tmp_path / "subdir"
    sub.mkdir()
    (sub / "nested.mkv").write_bytes(b"z" * 999)
    stats = bsg.record_dir_stats(str(tmp_path))
    # Only the top-level file counts (OBS records flat; a subdir is none of this run's business).
    assert stats["total_bytes"] == 10
    assert stats["file_count"] == 1


def test_record_dir_stats_oldest_mtime_is_the_minimum(tmp_path):
    import os
    import time

    older = tmp_path / "older.mkv"
    newer = tmp_path / "newer.mkv"
    older.write_bytes(b"a")
    newer.write_bytes(b"b")
    now = time.time()
    os.utime(older, (now - 1000, now - 1000))
    os.utime(newer, (now - 10, now - 10))
    stats = bsg.record_dir_stats(str(tmp_path))
    assert stats["oldest_mtime"] == pytest_approx(now - 1000)


def pytest_approx(value, tol=1.0):
    """Tiny local approx helper (avoids adding a pytest.approx import just for this one check —
    mtime precision varies by filesystem)."""
    class _Approx:
        def __eq__(self, other):
            return abs(other - value) <= tol

    return _Approx()


def test_record_dir_stats_unreadable_dir_returns_zeros_never_raises():
    # A directory the caller cannot reach (unmounted, permissions, wrong path after a profile
    # switch) must degrade to a harmless zero result — never crash the /record-dir-stats.json
    # endpoint, and never a false "over budget" WARN from a bogus large number.
    stats = bsg.record_dir_stats("/this/path/does/not/exist/at/all")
    assert stats == {"total_bytes": 0, "file_count": 0, "oldest_mtime": None}
