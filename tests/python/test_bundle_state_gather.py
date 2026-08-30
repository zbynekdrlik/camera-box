"""#650 — unit tests for scripts/bundle_state_gather.py, the PURE parsers/builders behind the
standing :8899 bundle-state HTTP service (version-integrity-gate.sh + recording-e2e.sh's
unattended CI fetch). No live OBS / no live box needed — same "source parsers, verify live
separately" split as tests/drift_guard.rs (drift-guard.sh) and test_obs_burn_filter.py
(obs_burn_filter.py's compute_burn_on).
"""
import hashlib
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


# ── #756: genlock_build_sha — the cross-box parity gate's per-box deployed build SHA ─────────────

def test_build_bundle_state_includes_genlock_build_sha_when_present():
    state = bsg.build_bundle_state(
        obs_version="32.1.2",
        genlock_build_sha="26de1c3c23980488a110dbf02e5e472f15cb001d",
    )
    assert state["genlock_build_sha"] == "26de1c3c23980488a110dbf02e5e472f15cb001d"


def test_build_bundle_state_omits_empty_genlock_build_sha():
    # An unread SHA (a build predating the marker, or an unreadable file) is OMITTED — the parity
    # gate then sees the box as unread (UNKNOWN), never a fabricated SHA.
    assert "genlock_build_sha" not in bsg.build_bundle_state(obs_version="32.1.2")


def test_genlock_build_sha_from_file_reads_first_token(tmp_path):
    f = tmp_path / "GENLOCK_BUILD_SHA.txt"
    f.write_text("26de1c3c23980488a110dbf02e5e472f15cb001d\n")
    assert (
        bsg.genlock_build_sha_from_file(str(f))
        == "26de1c3c23980488a110dbf02e5e472f15cb001d"
    )


def test_genlock_build_sha_from_file_strips_trailing_content(tmp_path):
    # Only the leading token of the first non-blank line — a stray trailing comment/whitespace can
    # never leak into the compared SHA.
    f = tmp_path / "GENLOCK_BUILD_SHA.txt"
    f.write_text("\n  26de1c3c2  built 2026-07-14\nextra\n")
    assert bsg.genlock_build_sha_from_file(str(f)) == "26de1c3c2"


def test_genlock_build_sha_from_file_missing_or_empty_is_blank(tmp_path):
    assert bsg.genlock_build_sha_from_file("") == ""
    assert bsg.genlock_build_sha_from_file(str(tmp_path / "nope.txt")) == ""
    empty = tmp_path / "empty.txt"
    empty.write_text("\n  \n")
    assert bsg.genlock_build_sha_from_file(str(empty)) == ""


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


# ── #826: strih OBS-identity machine-check facet — the 2026-07-27 incident (a hand-launched
# stale `1ME` OBS 31.1.2 squatted :4455 while the parity marker still described the pinned genlock
# 32.1.2) proved the box's OBS identity was never actually verified anywhere. These PURE gather
# functions feed the new version-integrity-gate.sh verdict functions (tested in
# tests/version_integrity_gate.rs) — same gather/gate split as every other facet in this module.

def test_obs_installs_under_finds_every_launchable_obs_exe(tmp_path):
    pinned = tmp_path / "program_files" / "obs-studio" / "bin" / "64bit"
    pinned.mkdir(parents=True)
    (pinned / "obs64.exe").write_bytes(b"x")
    retired = tmp_path / "apps" / "_RETIRED_1ME-obs_2026-07-27" / "bin" / "64bit"
    retired.mkdir(parents=True)
    (retired / "obs64.exe").write_bytes(b"x")  # renamed aside is NOT gone — must still be found
    (retired / "not-obs.dll").write_bytes(b"x")

    result = bsg.obs_installs_under([str(tmp_path / "program_files"), str(tmp_path / "apps")])
    paths = result.split(",")
    assert len(paths) == 2
    assert any(p.endswith(str(pathlib.Path("bin") / "64bit" / "obs64.exe")) and "program_files" in p for p in paths)
    assert any("_RETIRED_1ME-obs_2026-07-27" in p for p in paths)


def test_obs_installs_under_matches_legacy_me_named_exe(tmp_path):
    legacy = tmp_path / "apps" / "2ME-obs"
    legacy.mkdir(parents=True)
    (legacy / "2ME.exe").write_bytes(b"x")
    result = bsg.obs_installs_under([str(tmp_path / "apps")])
    assert result.endswith("2ME.exe")


def test_obs_installs_under_missing_root_is_empty():
    assert bsg.obs_installs_under(["/no/such/path", ""]) == ""


def test_obs_installs_under_none_found(tmp_path):
    empty = tmp_path / "empty"
    empty.mkdir()
    assert bsg.obs_installs_under([str(empty)]) == ""


def test_obs_process_count_from_listing_counts_obs_class_names():
    text = "obs64\nAutoHotkey64\nobs32\nResolume Arena\n"
    assert bsg.obs_process_count_from_listing(text) == "2"


def test_obs_process_count_from_listing_single_process():
    assert bsg.obs_process_count_from_listing("obs64\n") == "1"


def test_obs_process_count_from_listing_zero_when_none_running():
    assert bsg.obs_process_count_from_listing("AutoHotkey64\nResolume Arena\n") == "0"


def test_obs_process_count_from_listing_empty_text_is_unknown():
    # An unread process listing is UNKNOWN ("") — NEVER "0 confirmed running", which would let an
    # unreachable box read as a false-clean pass.
    assert bsg.obs_process_count_from_listing("") == ""
    assert bsg.obs_process_count_from_listing(None) == ""


AHK_SAMPLE_CLEAN = """\
app1_run  := 1
app1_path := "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\OBS Studio.lnk"
app1_name := "ahk_exe obs64.exe"
app2_run  := 0
"""

AHK_SAMPLE_WITH_DEAD_LEFTOVER = """\
app1_run  := 1
app1_path := "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\OBS Studio.lnk"
app1_name := "ahk_exe obs64.exe"
app1_binarypath := "D:\\_APPS\\1ME-obs\\1ME.lnk"
app2_run  := 0
app2_path := "D:\\_APPS\\2ME-obs\\2ME.lnk"
"""

AHK_SAMPLE_WITH_ENABLED_APP2 = """\
app1_run  := 1
app1_path := "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\OBS Studio.lnk"
app2_run  := 1
app2_path := "D:\\_APPS\\2ME-obs\\2ME.lnk"
"""


def test_ahk_app1_shortcut_path_found():
    assert (
        bsg.ahk_app1_shortcut_path(AHK_SAMPLE_CLEAN)
        == r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\OBS Studio.lnk"
    )


def test_ahk_app1_shortcut_path_absent_is_empty():
    assert bsg.ahk_app1_shortcut_path("") == ""
    assert bsg.ahk_app1_shortcut_path("no app1_path here") == ""
    assert bsg.ahk_app1_shortcut_path(None) == ""


def test_ahk_app1_run_enabled():
    assert bsg.ahk_app1_run(AHK_SAMPLE_CLEAN) == "1"


def test_ahk_app1_run_disabled():
    assert bsg.ahk_app1_run("app1_run := 0\n") == "0"


def test_ahk_app1_run_absent_is_empty():
    assert bsg.ahk_app1_run("") == ""
    assert bsg.ahk_app1_run(None) == ""


def test_ahk_dead_config_present_clean_config_is_zero():
    assert bsg.ahk_dead_config_present(AHK_SAMPLE_CLEAN) == "0"


def test_ahk_dead_config_present_detects_dead_binarypath():
    assert bsg.ahk_dead_config_present(AHK_SAMPLE_WITH_DEAD_LEFTOVER) == "1"


def test_ahk_dead_config_present_detects_enabled_app2():
    assert bsg.ahk_dead_config_present(AHK_SAMPLE_WITH_ENABLED_APP2) == "1"


def test_ahk_dead_config_present_absent_text_is_unknown():
    # No AHK text at all (e.g. this box has no NL_STARTUP.ahk — stream) is UNKNOWN, distinct from
    # "read and clean".
    assert bsg.ahk_dead_config_present("") == ""
    assert bsg.ahk_dead_config_present(None) == ""


def test_build_bundle_state_includes_826_obs_identity_keys_when_present():
    state = bsg.build_bundle_state(
        obs_version="32.1.2",
        obs_installs=r"C:\Program Files\obs-studio\bin\64bit\obs64.exe",
        port4455_owner_path=r"C:\Program Files\obs-studio\bin\64bit\obs64.exe",
        port4455_owner_version="32.1.2",
        obs_process_count="1",
        ahk_app1_shortcut_path=r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\OBS Studio.lnk",
        ahk_app1_run="1",
        ahk_dead_config_present="0",
        shortcut_target_path=r"C:\Program Files\obs-studio\bin\64bit\obs64.exe",
        shortcut_workdir=r"C:\Program Files\obs-studio\bin\64bit",
    )
    assert state["obs_installs"] == r"C:\Program Files\obs-studio\bin\64bit\obs64.exe"
    assert state["port4455_owner_path"] == r"C:\Program Files\obs-studio\bin\64bit\obs64.exe"
    assert state["port4455_owner_version"] == "32.1.2"
    assert state["obs_process_count"] == "1"
    assert state["ahk_app1_shortcut_path"] == r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\OBS Studio.lnk"
    assert state["ahk_app1_run"] == "1"
    assert state["ahk_dead_config_present"] == "0"
    assert state["shortcut_target_path"] == r"C:\Program Files\obs-studio\bin\64bit\obs64.exe"
    assert state["shortcut_workdir"] == r"C:\Program Files\obs-studio\bin\64bit"


def test_build_bundle_state_omits_826_keys_when_empty():
    state = bsg.build_bundle_state(obs_version="32.1.2")
    for key in (
        "obs_installs",
        "port4455_owner_path",
        "port4455_owner_version",
        "obs_process_count",
        "ahk_app1_shortcut_path",
        "ahk_app1_run",
        "ahk_dead_config_present",
        "shortcut_target_path",
        "shortcut_workdir",
    ):
        assert key not in state


# ── #862 follow-up: dantesync_version was REVERTED from bundle-state ──
#
# The #862 gate originally read strih/stream's dantesync version through this bundle-state facet
# (dantesync_version_from_log + a dantesync_version kwarg on build_bundle_state). Live verification
# (2026-07-30) found it half-wired end to end: the servers actually deployed on strih/stream never
# picked up the new key, and even the log line the parser looked for is not something dantesync
# ever logs on this fleet -- so the facet could only ever read UNKNOWN. The gate now reads EVERY
# node (including strih/stream) via a uniform `dantesync --version` over SSH instead
# (scripts/dantesync-version-gate.sh's dantesync_version_from_version_output) -- no bundle-state
# involvement at all. This module intentionally carries no dantesync_version code any more; do not
# re-add it without a live, working consumer.


# ── #770: byte-derived DistroAV/libobs parity — sha256 of the DEPLOYED plugin/core bytes ─────────
#
# The [0/8] version-integrity gate + drift-guard --compare engine already know how to compare the
# deployed obs.dll/distroav.dll sha256 against the #120 BUNDLE_MANIFEST (obs_dll_sha256 /
# distroav_dll_sha256 / bundle_hashes keys), but the box state NEVER carried the bytes — only the
# GENLOCK_BUILD_SHA.txt MARKER. `component_sha256` hashes a deployed file so those keys can be
# gathered; `build_bundle_state` gains obs_dll_sha256 / distroav_dll_sha256 (omit-when-empty, same
# never-a-false-clean discipline as every other facet). This closes the wrong-direction #119/#767
# hole (marker advanced, bytes stale) that the marker-only parity facet cannot catch.


def test_component_sha256_matches_hashlib(tmp_path):
    f = tmp_path / "distroav.dll"
    payload = b"\x00genlock-distroav-bytes\xff" * 37
    f.write_bytes(payload)
    assert bsg.component_sha256(str(f)) == hashlib.sha256(payload).hexdigest()


def test_component_sha256_of_empty_file(tmp_path):
    # An empty (but present) file has the well-known empty-content sha256 — a REAL value, never "".
    f = tmp_path / "obs.dll"
    f.write_bytes(b"")
    assert bsg.component_sha256(str(f)) == hashlib.sha256(b"").hexdigest()


def test_component_sha256_missing_file_returns_empty(tmp_path):
    # A file that is not there is UNKNOWN downstream (never a fabricated/zero SHA that would let a
    # missing plugin read as "clean") — same never-a-false-clean discipline as every other facet.
    assert bsg.component_sha256(str(tmp_path / "nope.dll")) == ""


def test_component_sha256_empty_path_returns_empty():
    assert bsg.component_sha256("") == ""
    assert bsg.component_sha256(None) == ""


def test_component_sha256_directory_returns_empty(tmp_path):
    # A directory is not a hashable regular file -> "" (UNKNOWN), never a crash.
    d = tmp_path / "bin"
    d.mkdir()
    assert bsg.component_sha256(str(d)) == ""


def test_build_bundle_state_includes_byte_shas_when_present():
    obs_sha = "a" * 64
    distroav_sha = "b" * 64
    state = bsg.build_bundle_state(
        obs_version="32.1.2",
        obs_dll_sha256=obs_sha,
        distroav_dll_sha256=distroav_sha,
    )
    assert state["obs_dll_sha256"] == obs_sha
    assert state["distroav_dll_sha256"] == distroav_sha


def test_build_bundle_state_omits_empty_byte_shas():
    # An unread deployed DLL (unreadable file, or a box whose server predates this facet) is
    # OMITTED — the gate then sees the box's bytes as unread, never a fabricated SHA. Opt-in
    # landing (#756-shape): a box not yet reporting the SHAs is silently skipped, not a false clean.
    state = bsg.build_bundle_state(obs_version="32.1.2")
    assert "obs_dll_sha256" not in state
    assert "distroav_dll_sha256" not in state


# ---------------------------------------------------------------------------------------------
# #1222 — bounded head+tail log read: gather latency must stay O(head+tail), never O(session
# length). Live incident: a ~13h OBS session (75 MB log) made every *_from_log parser above
# re-scan the WHOLE file on every request (~0.25 s/MB measured), pushing a /bundle-state.json
# fetch past recording-e2e.sh's `curl --max-time 30` and failing the [0/8] version-integrity gate.
# ---------------------------------------------------------------------------------------------

def test_log_head_and_tail_byte_constants_are_positive_and_bounded():
    # Sanity on the tuning constants (#1222 measured: the startup banner sits in the first few KB
    # of a real OBS log; 2 MB head / 5 MB tail is a wide, cheap margin against a 75 MB session).
    assert bsg.LOG_HEAD_BYTES > 0
    assert bsg.LOG_TAIL_BYTES > 0
    assert bsg.LOG_HEAD_BYTES + bsg.LOG_TAIL_BYTES < 20 * 1024 * 1024


def test_bounded_read_separator_matches_no_known_facet_pattern():
    # The separator spliced between the head and tail slices must never itself satisfy any of the
    # five *_from_log parsers above — otherwise a truncated log could fabricate a fact that was
    # never really in the log.
    sep = bsg.LOG_BOUNDED_READ_SEPARATOR
    assert bsg.obs_version_from_log(sep) == ""
    assert bsg.distroav_version_from_log(sep) == ""
    assert bsg.output_fps_from_log(sep) == ""
    assert bsg.genlock_wall_clock_from_log(sep) == ""
    assert bsg.genlock_capability_from_log(sep) == ""


def test_read_bounded_log_text_missing_file_is_empty(tmp_path):
    assert bsg.read_bounded_log_text(str(tmp_path / "nope.txt")) == ""


def test_read_bounded_log_text_small_file_returned_whole_unmodified(tmp_path):
    p = tmp_path / "small.txt"
    p.write_text(SAMPLE_LOG, encoding="utf-8")
    # Well under the (tiny, test-supplied) bound -> returned verbatim, no separator inserted.
    assert bsg.read_bounded_log_text(str(p), head_bytes=1000, tail_bytes=1000) == SAMPLE_LOG


def test_read_bounded_log_text_large_file_is_bounded_and_excludes_the_middle(tmp_path):
    head_marker = "HEAD_MARKER_ONLY_AT_START"
    middle_sentinel = "MIDDLE_FILLER_LINE_THAT_MUST_NEVER_SURVIVE_" + ("x" * 200)
    tail_marker = "TAIL_MARKER_ONLY_AT_END"
    # >1 MB synthetic fixture, built programmatically (drift-guard-log-parsers SIGPIPE-test
    # pattern) -- never committed as a fixture file.
    lines = [head_marker] + [middle_sentinel] * 5000 + [tail_marker]
    full_text = "\n".join(lines) + "\n"
    assert len(full_text) > 1_000_000  # sanity: genuinely larger than the test bound below

    p = tmp_path / "big.txt"
    p.write_text(full_text, encoding="utf-8")

    bounded = bsg.read_bounded_log_text(str(p), head_bytes=500, tail_bytes=500)

    assert len(bounded) < len(full_text)
    assert head_marker in bounded
    assert tail_marker in bounded
    assert middle_sentinel not in bounded
    assert bsg.LOG_BOUNDED_READ_SEPARATOR in bounded
    assert (
        bounded.index(head_marker)
        < bounded.index(bsg.LOG_BOUNDED_READ_SEPARATOR)
        < bounded.index(tail_marker)
    )


def test_read_bounded_log_text_default_constants_bound_a_multi_mb_log():
    # Uses the REAL production constants end-to-end (no scaled-down test bound) against a
    # synthetic log deliberately larger than LOG_HEAD_BYTES + LOG_TAIL_BYTES, generated
    # programmatically (never committed as a fixture file) -- the startup-banner facets (head)
    # AND the newest-state facet (tail) must both still parse correctly out of the bounded read.
    import tempfile

    filler_line = "15:00:00.000: genlock-fifo audit received=1 sent=1 gaps=0\n"
    total_target = bsg.LOG_HEAD_BYTES + bsg.LOG_TAIL_BYTES + (1024 * 1024)
    repeats = total_target // len(filler_line) + 1
    tail_capability_line = "23:59:59.000: genlock: timestamp-aligned release engaged\n"
    text = SAMPLE_LOG + (filler_line * repeats) + tail_capability_line
    assert len(text) > bsg.LOG_HEAD_BYTES + bsg.LOG_TAIL_BYTES

    with tempfile.TemporaryDirectory() as td:
        p = pathlib.Path(td) / "huge.txt"
        p.write_text(text, encoding="utf-8")

        bounded = bsg.read_bounded_log_text(str(p))  # default head_bytes/tail_bytes

        assert len(bounded) == (
            bsg.LOG_HEAD_BYTES + len(bsg.LOG_BOUNDED_READ_SEPARATOR) + bsg.LOG_TAIL_BYTES
        )
        # startup-banner facets (head) still parse:
        assert bsg.obs_version_from_log(bounded) == "32.1.2"
        assert bsg.distroav_version_from_log(bounded) == "6.2.1"
        assert bsg.output_fps_from_log(bounded) == "30"
        assert bsg.genlock_wall_clock_from_log(bounded) == "1"
        # newest-state facet (tail) still parses:
        assert "timestamp-aligned release" in bsg.genlock_capability_from_log(bounded)
