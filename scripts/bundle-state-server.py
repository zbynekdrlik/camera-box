#!/usr/bin/env python3
"""#650 — the standing :8899 bundle-state/recording HTTP service for strih + stream.

Runs ON each Windows OBS box (strih 10.77.9.202 / stream 10.77.9.204) as an auto-starting
background process (see scripts/run-bundle-state-server.ps1, the Scheduled-Task wrapper this
deploys under). It serves TWO things on ONE port so both `scripts/version-integrity-gate.sh`'s
`--win-state` fetch and `scripts/recording-fetch-windows.sh`'s recording download keep working
completely unattended, with no operator/agent win-* MCP round-trip:

  GET /bundle-state.json  -> a FRESH (regenerated on every request, never cached/stale) JSON of
                              the drift-guard `--compare` observed values this box can gather on
                              its own (see bundle_state_gather.py's module doc for exactly which
                              keys and why). This is what closed #650: the automatic
                              pull_request-triggered full-path-e2e gate previously always saw both
                              boxes UNKNOWN (nothing listened on :8899) and refused (exit 11).
  GET /record-dir-stats.json -> #652: read-only stats over this box's OWN OBS record directory
                              (total bytes + file count + oldest mtime of its top-level files) —
                              recording-e2e.sh's preflight curls this to WARN (never fail) when a
                              box's accumulated E2E test recordings exceed a disk budget.

  GET /<any other path>   -> served as a static file out of OBS's OWN current record directory
                              (read live via `GetRecordDirectory` over the local obs-websocket —
                              never a hardcoded/stale path, so a profile switch can never leave
                              this serving the wrong folder). This is the pre-#650 behavior that
                              used to run as an ad-hoc `python -m http.server 8899` in the record
                              folder; recording-fetch-windows.sh's URL-encoding contract (OBS
                              filenames contain a space -> %20) is unchanged — Python's
                              `http.server` already unquotes the request path.

Needs `pip install websocket-client` (already present on both boxes, confirmed 2026-07-10) for the
local obs-websocket v5 RPCs (`ndi_input_latency` + `GetRecordDirectory`); reuses scripts/obs_phase2.py's
proven `_conn`/`_rpc` helpers (auth handshake, request/response framing, the #328 stuck-op timeout)
rather than re-deriving a third OBS-WS client in this repo.

Usage (see scripts/run-bundle-state-server.ps1 for the deployed invocation):
  python bundle-state-server.py [--port 8899] [--obs-host 127.0.0.1] [--password ...]

The OBS WebSocket password is READ FROM THE ENVIRONMENT (`OBS_PASSWORD`, matching every other
script in this repo — obs_burn_filter.py, av_sync_calibrate.py, obs_phase2.py's rig-busy-check —
never a CLI literal, never committed; see scripts/run-bundle-state-server.ps1's own doc for where
that env var is set on-box).
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import unquote

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import bundle_state_gather as bsg  # noqa: E402

# Issue 1386 slice D: the Windows-only identity readers and their caches live in
# bundle_state_windows, and the timestamped logger they share with this server in
# bundle_state_serverlog. The orchestration below calls the readers through THIS module's globals,
# so a test that patches `bss.<reader>` reaches the gather. Such a patch reaches ONLY calls this
# module makes: a call made inside bundle_state_windows (a reader's own helpers, its own log,
# its caches, subprocess) resolves there, so patch or reset that module for it -- a cache in place,
# never rebound.
from bundle_state_serverlog import log  # noqa: E402
from bundle_state_windows import (  # noqa: E402
    _parse_tasklist_obs_process_names,
    gather_vb_matrix_facet,
    ndi_runtime_version,
    port4455_owner,
    read_ahk_text,
    resolve_shortcut,
    tasklist_csv,
    vb_matrix_start_time,
)


try:
    from obs_phase2 import _conn, _rpc  # noqa: E402
except ImportError:
    sys.exit("missing dep: pip install websocket-client (obs_phase2.py needs it)")

DEFAULT_PORT = 8899
DEFAULT_OBS_HOST = "127.0.0.1"

# #1299 — this server was authored for the Windows OBS boxes (strih/stream), but imag runs OBS on
# Linux and needs the SAME server to expose the fleet-visible genlock-lock facet. Every Windows-only
# identity gather (native tasklist/netstat/CIM process reads, ProgramData core/plugin DLL byte
# hashes, the .lnk/AHK Start-Menu paths + PowerShell NDI-runtime read) is gated behind this flag at
# the gather boundary so on Linux it is SKIPPED — no Windows-only subprocess is spawned, no
# per-request WARNING is logged, and each such facet is simply OMITTED (absent == UNKNOWN downstream,
# never a false 0/False). On Windows the gate is a no-op, so the served payload stays byte-identical.
IS_WINDOWS = os.name == "nt"
# The documented NDI runtime DLL (.claude/commands/drift-guard.md step 1) — read-only Get-Item.
DEFAULT_NDI_RUNTIME_DLL = r"C:\Program Files\NDI\NDI 6 Tools\Runtime\Processing.NDI.Lib.x64.dll"
# The two static OBS module scan paths (the third, %APPDATA%\obs-studio\plugins, is resolved from
# the environment below — mirrors bundle_state_gather.DISTROAV_SCAN_ROOTS + drift-guard.md 1c).
APPDATA_DISTROAV_ROOT = "obs-studio/plugins"
# #756 — the deployed genlock bundle's build-SHA marker on a Windows box (the SAME
# GENLOCK_BUILD_SHA.txt imag serves from /opt/obs-genlock/). Read-only; "" if absent (a stock/
# non-genlock install, or a build predating the marker) -> UNKNOWN, never a guessed SHA.
DEFAULT_GENLOCK_BUILD_SHA_FILE = r"C:\Program Files\obs-studio\GENLOCK_BUILD_SHA.txt"

# #770 — the deployed OBS core DLL whose sha256 the [0/8] version-integrity gate compares against
# the #120 BUNDLE_MANIFEST (drift-guard --compare's obs_dll_sha256 key). The genlock hot-swap
# replaces this exact file (sibling of DEFAULT_OBS_INSTALL_EXE), mirroring imag's libobs.so.30.
# Read-only Get-FileHash-equivalent (bsg.component_sha256); "" if absent -> UNKNOWN, never guessed.
DEFAULT_OBS_DLL = r"C:\Program Files\obs-studio\bin\64bit\obs.dll"

# #826 — the strih OBS-identity machine-check facet. The 2026-07-27 incident: a hand-launched
# stale `1ME` OBS 31.1.2 install squatted TCP :4455 while this box's own parity marker still
# described the pinned genlock 32.1.2 build. These defaults are read-only scan roots/paths; every
# gather below degrades to "" (UNKNOWN downstream) on any failure, never a guessed value.
DEFAULT_OBS_INSTALL_SCAN_ROOTS = (
    r"C:\Program Files\obs-studio",
    r"D:\_APPS",
)
DEFAULT_STARTUP_SHORTCUT = r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\OBS Studio.lnk"
# Only strih runs NL_STARTUP.ahk (stream has none, per .claude/skills/obs-ops) — a box without
# this file simply gathers "" for every ahk_* key, which the gate correctly reads as "this facet
# does not apply here" rather than a failure (see version-integrity-gate.sh's startup_chain scope).
DEFAULT_AHK_PATH = r"D:\_APPS\NL_STARTUP.ahk"

# #1227 — where to look for a VB-Audio Matrix install (the disk gate that distinguishes a box that
# HAS VB-Matrix but its process is dead — a real DOWN — from a box that never had it, e.g. imag).
# The stream build lives at C:\Program Files (x86)\VB\VBAudioMatrix\VBAudioMatrix_x64.exe (verified
# live 2026-09-02); strih runs VBAudioMatrixCoconut_x64 under the same VB\ tree. Read-only scan.
DEFAULT_VB_MATRIX_INSTALL_DIRS = (
    r"C:\Program Files (x86)\VB\VBAudioMatrix",
    r"C:\Program Files\VB\VBAudioMatrix",
)


def newest_obs_log_text(log_dir, head_bytes=bsg.LOG_HEAD_BYTES, tail_bytes=bsg.LOG_TAIL_BYTES):
    """The raw text of the newest *.txt OBS log in *log_dir*, BOUNDED to at most
    `head_bytes + tail_bytes` (#1222 — a growing multi-hour OBS session's log made every
    *_from_log parser re-scan the WHOLE file on every request, ~0.25 s/MB measured, pushing
    gather latency past recording-e2e.sh's `curl --max-time 30` and failing the [0/8]
    version-integrity gate). "" if none/unreadable — the callers already treat an unreadable log
    as every derived key coming back empty/UNKNOWN. See bsg.read_bounded_log_text for the actual
    bounded-read implementation (shared, PURE, testable without a live box)."""
    try:
        candidates = glob.glob(os.path.join(log_dir, "*.txt"))
        if not candidates:
            log(f"WARNING: no OBS log files found under {log_dir}")
            return ""
        newest = max(candidates, key=os.path.getmtime)
        return bsg.read_bounded_log_text(newest, head_bytes, tail_bytes)
    except OSError as e:
        log(f"WARNING: could not read OBS log dir {log_dir}: {e}")
        return ""


def gather_ndi_inputs(host, password):
    """{name: {"kind": ..., "settings": {...}}} for every ndi_source-kind input, over the local
    obs-websocket — mirrors ~/.cache/obsprobe/obs_inputs.py's shape (bundle_state_gather.
    ndi_input_latency_csv consumes exactly this). Raises on a connection/RPC failure — the caller
    decides what an unreadable OBS means for the payload (never silently fabricates a value)."""
    ws = _conn(host, password)
    try:
        inputs = _rpc(ws, "GetInputList")["inputs"]
        result = {}
        for i in inputs:
            name = i["inputName"]
            kind = i.get("inputKind", "")
            if "ndi" not in kind.lower():
                continue
            settings = _rpc(ws, "GetInputSettings", {"inputName": name})["inputSettings"]
            result[name] = {"kind": kind, "settings": settings}
        return result
    finally:
        ws.close()


def gather_record_directory(host, password):
    """The LIVE current OBS record directory (GetRecordDirectory) — never a cached/hardcoded
    path, so a profile switch can never leave the static-file server pointed at a stale folder.
    Raises on failure; the caller falls back to the last-known-good value (module-level cache)."""
    ws = _conn(host, password)
    try:
        return _rpc(ws, "GetRecordDirectory")["recordDirectory"]
    finally:
        ws.close()


def _timed(timings, key, fn, *args, **kwargs):
    """#1222 — run fn(*args, **kwargs), recording its wall-clock duration under *key* in the
    *timings* dict (the opt-in BUNDLE_STATE_TIMING=1 per-facet breakdown — see
    gather_bundle_state's own doc comment). Never swallows an exception; only measures."""
    t0 = time.perf_counter()
    try:
        return fn(*args, **kwargs)
    finally:
        timings[key] = time.perf_counter() - t0


def _genlock_log_facets(log_text):
    """The OBS / genlock facets of the bounded log: the startup banner (OBS + DistroAV version, the
    reset block's fps), the genlock wall-clock + capability markers, and the #1320 PROGRAM-render
    freeze + relock-burst facets. A dict of `build_bundle_state` keywords."""
    lagged, lagged_age_s = bsg.program_render_lagged_from_log(log_text)
    relock_bursts, relock_bursts_age_s = bsg.relock_bursts_from_log(log_text)
    return {
        "obs_version": bsg.obs_version_from_log(log_text),
        "distroav_version": bsg.distroav_version_from_log(log_text),
        "output_fps": bsg.output_fps_from_log(log_text),
        "genlock_wall_clock": bsg.genlock_wall_clock_from_log(log_text),
        "genlock_capability": bsg.genlock_capability_from_log(log_text),
        "program_render_lagged": lagged,
        "program_render_lagged_age_s": lagged_age_s,
        "relock_bursts": relock_bursts,
        "relock_bursts_age_s": relock_bursts_age_s,
    }


def _av_offset_log_facets(log_text):
    """The av-sync dock facets of the bounded log (#1267 / #1319 / #1325). A dict of
    `build_bundle_state` keywords; the dev1 av-step watchdog + the audio-lag band arm read them."""
    (recent_med, base_med, pin, pin_stable, age_s, n_recent,
     n_base) = bsg.av_offset_series_from_log(log_text)
    recent_mad_ms, recent_matched_min = bsg.av_offset_quality_from_log(log_text)
    return {
        "av_offset_recent_med_ms": recent_med,
        "av_offset_base_med_ms": base_med,
        "av_offset_pin": pin,
        "av_offset_pin_stable": pin_stable,
        "av_offset_age_s": age_s,
        "av_offset_n_recent": n_recent,
        "av_offset_n_base": n_base,
        "av_offset_dock_live_age_s": bsg.av_offset_dock_live_age_from_log(log_text),
        "av_offset_recent_mad_ms": recent_mad_ms,
        "av_offset_recent_matched_min": recent_matched_min,
        "av_offset_quality_age_s": bsg.av_offset_quality_age_from_log(log_text),
    }


def _audio_log_facets(log_text, log_read_tod, ref_band_src):
    """The OBS audio facets of the bounded log: the #1226/#1231 ts_lag max + freshness, the #1265
    reference band, the #1325 buffered_ms shape, the issue-1381 mixer + obs-vban pacer loss, and the
    issue-1385 log-head age. A dict of `build_bundle_state` keywords."""
    # issue 1381: the timestamped tail is parsed ONCE and shared by the mixer + pacer facets below.
    stamped_tail = bsg.timestamped_tail_lines(log_text)
    ts_lag_ms, ts_lag_src, ts_lag_age_s = bsg.audio_telemetry_from_log(log_text)
    (ref_src, ref_base, ref_high, ref_low, ref_duty,
     ref_n) = bsg.audio_ref_band_from_log(log_text, ref_src=ref_band_src)
    (buf_slope, buf_max_step, buf_n,
     buf_age_s) = bsg.buffered_ms_series_from_log(log_text, ref_src=ref_band_src)
    (mixer_ticks, mixer_ticks_over, mixer_window_ms, mixer_tick_ms,
     mixer_age_s) = bsg.audio_mixer_from_log(log_text, tail=stamped_tail)
    (vban_events, vban_ms, vban_dest,
     vban_age_s) = bsg.vban_pacer_loss_from_log(log_text, tail=stamped_tail)
    return {
        "audio_ts_lag_ms": ts_lag_ms,
        "audio_ts_lag_src": ts_lag_src,
        "audio_ts_lag_age_s": ts_lag_age_s,
        "audio_ref_lag_src": ref_src,
        "audio_ref_lag_base_ms": ref_base,
        "audio_ref_lag_high_ms": ref_high,
        "audio_ref_lag_low_ms": ref_low,
        "audio_ref_lag_duty_pct": ref_duty,
        "audio_ref_lag_n": ref_n,
        "buffered_ms_slope_ms_per_min": buf_slope,
        "buffered_ms_max_step_ms": buf_max_step,
        "buffered_ms_n": buf_n,
        "buffered_ms_age_s": buf_age_s,
        "audio_mixer_ticks": mixer_ticks,
        "audio_mixer_ticks_over": mixer_ticks_over,
        "audio_mixer_window_ms": mixer_window_ms,
        "audio_mixer_tick_ms": mixer_tick_ms,
        "audio_mixer_age_s": mixer_age_s,
        "vban_pacer_loss_events": vban_events,
        "vban_pacer_loss_ms": vban_ms,
        "vban_pacer_loss_dest": vban_dest,
        "vban_pacer_age_s": vban_age_s,
        "obs_log_head_age_s": bsg.obs_log_head_age_s_from_log(log_text, log_read_tod),
    }


def _parse_log_facets(log_text, log_read_tod, ref_band_src):
    """Every log-derived facet from the SAME bounded log_text (no second read — one #1222
    `obs_log_parse` timing): `(facets, genlock_lock)`, where `facets` is a dict of
    `build_bundle_state` keywords and `genlock_lock` is the nested #1299 LOCK facet (None when the
    log has no `genlock-lock-json:` line — a stock OBS / no line yet, never a false UNLOCKED).
    Each key's purpose (and which dev1 watchdog reads it) is documented once, at its entry in
    `bundle_state_gather.BUNDLE_STATE_KEYS`."""
    facets = {}
    facets.update(_genlock_log_facets(log_text))
    facets.update(_audio_log_facets(log_text, log_read_tod, ref_band_src))
    facets.update(_av_offset_log_facets(log_text))
    return facets, bsg.genlock_lock_facet_from_log(log_text)


def _gather_ndi_inputs_or_empty(obs_host, password):
    """The OBS-WS NDI inputs, or {} when OBS-WS is unreachable — any WS/RPC failure must not crash
    the whole response (the log-derived facets still read fine); logged, never guessed."""
    try:
        return gather_ndi_inputs(obs_host, password)
    except Exception as e:  # noqa: BLE001 - any WS/RPC failure must not crash the whole response
        log(f"WARNING: could not gather NDI input latency over obs-websocket: {e}")
        return {}


def _windows_startup_facets(timings, ahk_path, startup_shortcut):
    """#826 — the strih OBS-identity machine-check facet (each gather independent, same
    never-let-one-failure-blank-the-rest discipline as every other facet here): who owns :4455,
    the NL_STARTUP.ahk launcher facts, and the Start-Menu shortcut's own resolution."""
    port_owner_path, port_owner_version = _timed(timings, "port4455_owner", port4455_owner)
    ahk_text = _timed(timings, "ahk_text", read_ahk_text, ahk_path)
    shortcut_target, shortcut_workdir = _timed(
        timings, "shortcut", resolve_shortcut, startup_shortcut
    )
    return {
        "port4455_owner_path": port_owner_path,
        "port4455_owner_version": port_owner_version,
        "ahk_app1_shortcut_path": bsg.ahk_app1_shortcut_path(ahk_text),
        "ahk_app1_run": bsg.ahk_app1_run(ahk_text),
        "ahk_dead_config_present": bsg.ahk_dead_config_present(ahk_text),
        "shortcut_target_path": shortcut_target,
        "shortcut_workdir": shortcut_workdir,
    }


def _windows_install_facets(timings, distroav_scan_roots, obs_install_scan_roots, obs_dll_path,
                            ndi_runtime_dll):
    """#770 — the DEPLOYED plugin/core byte identity the [0/8] version-integrity gate compares
    against the #120 BUNDLE_MANIFEST, plus the #826 install scan and the NDI runtime version.
    distroav.dll: hash the FIRST located copy (scan order: Program Files, then ProgramData, then
    %APPDATA% — the primary genlock plugin), so a single observed distroav_dll_sha256 pairs with the
    manifest's by-basename distroav.dll sha. A shadowing duplicate is a SEPARATE #124 concern
    (distroav_dll_paths reports the whole set). #1115: under Option A the deploy
    (deploy-genlock-fleet.sh FULL) ships the canonical genlock distroav.dll TO this ProgramData load
    path, so the FIRST-located copy IS the deployed canonical build (Program Files stays
    /XF-excluded => no shadow ahead of it) — hashing it here is exactly the byte the
    version-integrity gate compares by basename. Each hash degrades to "" (UNKNOWN downstream, never
    a guessed/zero SHA) when the file is missing/unreadable — the opt-in landing (#756-shape): a box
    with no genlock DLL is skipped."""
    distroav_paths_csv = _timed(
        timings, "distroav_dll_paths", bsg.distroav_dll_paths, distroav_scan_roots
    )
    first_distroav = distroav_paths_csv.split(",")[0] if distroav_paths_csv else ""
    obs_installs_val = _timed(
        timings, "obs_installs", bsg.obs_installs_under, obs_install_scan_roots
    )
    obs_dll_sha256_val = _timed(timings, "obs_dll_sha256", bsg.component_sha256, obs_dll_path)
    distroav_dll_sha256_val = _timed(
        timings, "distroav_dll_sha256", bsg.component_sha256, first_distroav
    )
    ndi_runtime_val = _timed(timings, "ndi_runtime", ndi_runtime_version, ndi_runtime_dll)
    return {
        "distroav_dll_paths": distroav_paths_csv,
        "obs_installs": obs_installs_val,
        "obs_dll_sha256": obs_dll_sha256_val,
        "distroav_dll_sha256": distroav_dll_sha256_val,
        "ndi_runtime": ndi_runtime_val,
    }


def _windows_process_facets(timings):
    """The OBS process count and the #1227 VB-Matrix presence facet, from ONE native tasklist per
    request (#1227 review 🟡 — never two spawns)."""
    tasklist_text = _timed(timings, "tasklist", tasklist_csv)
    obs_process_count_val = _timed(
        timings,
        "obs_process_count",
        lambda: bsg.obs_process_count_from_listing(_parse_tasklist_obs_process_names(tasklist_text)),
    )
    # #1227 — the VB-Matrix presence facet (the dev1 VB-Matrix alert watchdog reads it). The disk
    # install gate + the shared tasklist parse compose the 3-state running facet; the start time is
    # a best-effort PID-keyed-cached CIM read gathered via start_fn (a falsy pid = no subprocess, so
    # imag / a DOWN box never pays a CIM query). A FAILED tasklist read is UNKNOWN, never a false DOWN.
    (vb_matrix_running_val, vb_matrix_name_val, vb_matrix_pid_val, vb_matrix_start_val) = _timed(
        timings, "vb_matrix",
        lambda: gather_vb_matrix_facet(
            bsg.vb_matrix_install_present_under(DEFAULT_VB_MATRIX_INSTALL_DIRS),
            tasklist_text, vb_matrix_start_time,
        ),
    )
    return {
        "obs_process_count": obs_process_count_val,
        "vb_matrix_running": vb_matrix_running_val,
        "vb_matrix_name": vb_matrix_name_val,
        "vb_matrix_pid": vb_matrix_pid_val,
        "vb_matrix_start": vb_matrix_start_val,
    }


def _windows_identity_facets(timings, *, ahk_path, startup_shortcut, distroav_scan_roots,
                             obs_install_scan_roots, obs_dll_path, ndi_runtime_dll):
    """#1299 — the OBS-box IDENTITY facets, ALL Windows-specific (native tasklist/netstat/CIM
    process reads, ProgramData core/plugin DLL byte hashes, the .lnk/AHK Start-Menu paths +
    PowerShell NDI-runtime read). On a Linux OBS box (imag) they do not apply, so the caller skips
    this whole gather at ONE os.name gate (not scattered per-facet try/excepts): every one of these
    keys stays absent, so build_bundle_state OMITS it (absent == UNKNOWN downstream, never a false
    0/False), with no Windows-only subprocess spawned and no per-request WARNING. The timing keys
    are recorded in the order the gathers run."""
    facets = _windows_startup_facets(timings, ahk_path, startup_shortcut)
    facets.update(_windows_install_facets(timings, distroav_scan_roots, obs_install_scan_roots,
                                          obs_dll_path, ndi_runtime_dll))
    facets.update(_windows_process_facets(timings))
    return facets


def gather_bundle_state(
    obs_host, password, obs_log_dir, ndi_runtime_dll, distroav_scan_roots,
    genlock_build_sha_file=DEFAULT_GENLOCK_BUILD_SHA_FILE,
    obs_install_scan_roots=DEFAULT_OBS_INSTALL_SCAN_ROOTS,
    startup_shortcut=DEFAULT_STARTUP_SHORTCUT,
    ahk_path=DEFAULT_AHK_PATH,
    obs_dll_path=DEFAULT_OBS_DLL,
):
    """Build the fresh bundle-state dict for THIS request — every gather is attempted
    independently so one failing facet (e.g. OBS-WS momentarily unreachable) does not blank out
    the log-derived facets that still read fine; each key that could not be read is simply
    omitted (UNKNOWN downstream), never guessed.

    #1222: every facet gather is timed into a per-request breakdown, logged as ONE line
    ("gather timing: key=Xs ...") when BUNDLE_STATE_TIMING=1 is set in the environment — opt-in
    so the normal request path pays only a cheap perf_counter() call per facet. This gives the
    NEXT session real per-facet data to attack the remaining ~18.7s cold-log baseline (measured
    2026-08-29 AFTER a fresh-log restart, so it is NOT the log-size problem this ticket's bounded
    read already fixes) instead of guessing which facet is slow.

    Issue 1386: the facet families are gathered by the named helpers above (log: genlock / audio /
    A/V offset; OBS-WS NDI inputs; the Windows identity group); this function only sequences them
    and assembles the payload, so the served JSON (key order included) is unchanged."""
    timings = {}
    t_total0 = time.perf_counter()

    log_text = _timed(timings, "obs_log_read", newest_obs_log_text, obs_log_dir)
    # issue 1385 -- the box's local clock read right AFTER the log, so the log head's age against it
    # (`obs_log_head_age_s`) proves the log is being written now; never re-read later in the gather.
    log_read_tod = bsg.local_seconds_of_day()

    # #1265 — the reference source whose ts_lag BAND is watched (mbc on stream; env-overridable so a
    # future box with a different A/V reference can set it). A box that has no such source (strih)
    # simply reports the band facets empty -> omitted, never a false band.
    ref_band_src = os.environ.get("AUDIO_REF_BAND_SRC", bsg.AUDIO_REF_BAND_DEFAULT_SRC)

    log_facets, genlock_lock = _timed(
        timings, "obs_log_parse", _parse_log_facets, log_text, log_read_tod, ref_band_src
    )
    ndi_inputs = _timed(timings, "ndi_inputs", _gather_ndi_inputs_or_empty, obs_host, password)

    # #1299 — the DEPLOYED genlock build SHA is a plain CROSS-PLATFORM file read (imag serves it from
    # /opt/obs-genlock/GENLOCK_BUILD_SHA.txt; the Windows boxes from the deployed bundle path), so it
    # is gathered UNCONDITIONALLY — never behind the Windows-identity gate below — alongside the
    # OBS-WS ndi_inputs and every log-derived facet. It is the #756 cross-box parity value the
    # version-integrity gate reads off each box's state.
    genlock_build_sha_val = _timed(
        timings, "genlock_build_sha", bsg.genlock_build_sha_from_file, genlock_build_sha_file
    )

    # #1299 — ONE os.name gate for the Windows-only identity group (see _windows_identity_facets).
    # On Windows the gate is a no-op, so the served payload stays byte-identical to before.
    identity = {}
    if IS_WINDOWS:
        identity = _windows_identity_facets(
            timings,
            ahk_path=ahk_path,
            startup_shortcut=startup_shortcut,
            distroav_scan_roots=distroav_scan_roots,
            obs_install_scan_roots=obs_install_scan_roots,
            obs_dll_path=obs_dll_path,
            ndi_runtime_dll=ndi_runtime_dll,
        )

    result = bsg.build_bundle_state(
        ndi_input_latency=bsg.ndi_input_latency_csv(ndi_inputs),
        # #756 — the deployed genlock build SHA for the cross-box parity gate.
        genlock_build_sha=genlock_build_sha_val,
        **log_facets,
        **identity,
    )

    # #1299 — the genlock_lock facet is a NESTED object, not a flat string, so it is attached here
    # rather than through build_bundle_state (whose every-value-is-a-quoted-string contract the
    # version-integrity gate's regex depends on; it simply ignores this extra key). Omit-when-absent:
    # a stock OBS / no genlock-lock-json: line yields None -> the facet never appears (UNKNOWN
    # downstream), never a false UNLOCKED.
    if genlock_lock is not None:
        result["genlock_lock"] = genlock_lock

    timings["total"] = time.perf_counter() - t_total0
    if os.environ.get("BUNDLE_STATE_TIMING") == "1":
        breakdown = " ".join(f"{k}={v:.3f}s" for k, v in timings.items())
        log(f"gather timing: {breakdown}")
    return result


class _State:
    """Shared, thread-safe last-known-good record directory (ThreadingHTTPServer dispatches each
    request on its own thread) — a transient OBS-WS hiccup while serving a FILE (as opposed to
    /bundle-state.json, which has no meaningful fallback) should not 404 a recording fetch that
    would otherwise succeed against the directory we already resolved a moment ago."""

    def __init__(self):
        self.lock = threading.Lock()
        self.last_record_dir = None


def make_handler(args, state):
    class Handler(BaseHTTPRequestHandler):
        server_version = "camera-box-bundle-state/1"

        def log_message(self, fmt, *fmt_args):  # route through our timestamped logger
            log(f"{self.address_string()} {fmt % fmt_args}")

        def do_GET(self):
            path = self.path.split("?", 1)[0]
            if path == "/bundle-state.json":
                self._serve_bundle_state()
            elif path == "/record-dir-stats.json":
                self._serve_record_dir_stats()
            else:
                self._serve_record_file(path)

        def _serve_bundle_state(self):
            try:
                payload = gather_bundle_state(
                    args.obs_host, args.password, args.obs_log_dir,
                    args.ndi_runtime_dll, self._distroav_scan_roots(),
                    args.genlock_build_sha_file,
                    obs_install_scan_roots=args.obs_install_scan_root,
                    startup_shortcut=args.startup_shortcut,
                    ahk_path=args.ahk_path,
                    obs_dll_path=args.obs_dll,
                )
            except Exception as e:  # noqa: BLE001 - never let a gather bug hang the gate forever
                log(f"ERROR: bundle-state gather failed: {e}")
                self.send_response(500)
                self.end_headers()
                return
            body = json.dumps(payload, indent=2).encode("utf-8")
            log(f"served /bundle-state.json ({len(payload)} key(s): {sorted(payload)})")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def _serve_record_dir_stats(self):
            """#652/#1276: read-only disk-usage stats over the box's OWN OBS record directory (total
            bytes + file count + oldest mtime of its top-level files, plus the volume's free_bytes
            since #1276) — powers recording-e2e.sh's recordings preflight WARN, which since #1276
            fires when the recordings VOLUME has <= RECORDINGS_FREE_MIN_GB of FREE space left (not
            when the file sum exceeds a budget). Same resolve-live + last-known-good fallback as the
            static-file GET path below — never a stale/wrong directory after a profile switch."""
            record_dir = self._resolve_record_dir()
            if record_dir is None:
                self.send_response(503)
                self.end_headers()
                return
            stats = bsg.record_dir_stats(record_dir)
            body = json.dumps(stats).encode("utf-8")
            log(f"served /record-dir-stats.json for {record_dir!r}: {stats}")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def _distroav_scan_roots(self):
            roots = list(bsg.DISTROAV_SCAN_ROOTS)
            appdata = os.environ.get("APPDATA")
            if appdata:
                roots.append(os.path.join(appdata, APPDATA_DISTROAV_ROOT))
            return roots

        def _serve_record_file(self, path):
            name = unquote(path.lstrip("/"))
            # Reject any path-traversal / absolute-drive attempt outright (this is a public LAN
            # port serving a directory tree — never let a crafted request escape the record dir).
            if not name or ".." in name.split("/") or ":" in name or name.startswith(("/", "\\")):
                log(f"REJECTED unsafe path: {path!r}")
                self.send_response(400)
                self.end_headers()
                return
            record_dir = self._resolve_record_dir()
            if record_dir is None:
                self.send_response(503)
                self.end_headers()
                return
            full_path = os.path.join(record_dir, name)
            if not os.path.isfile(full_path):
                log(f"404: {full_path!r} not found (record dir {record_dir!r})")
                self.send_response(404)
                self.end_headers()
                return
            size = os.path.getsize(full_path)
            log(f"serving {full_path!r} ({size} bytes)")
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(size))
            self.end_headers()
            with open(full_path, "rb") as f:
                while True:
                    chunk = f.read(1024 * 1024)
                    if not chunk:
                        break
                    self.wfile.write(chunk)

        def _resolve_record_dir(self):
            try:
                fresh = gather_record_directory(args.obs_host, args.password)
                with state.lock:
                    state.last_record_dir = fresh
                return fresh
            except Exception as e:  # noqa: BLE001 - fall back to the last-known-good directory
                with state.lock:
                    fallback = state.last_record_dir
                log(
                    f"WARNING: GetRecordDirectory failed ({e}); "
                    f"falling back to last-known-good {fallback!r}"
                )
                return fallback

    return Handler


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--port", type=int, default=DEFAULT_PORT)
    ap.add_argument("--obs-host", default=DEFAULT_OBS_HOST)
    ap.add_argument("--password", default=os.environ.get("OBS_PASSWORD", ""))
    ap.add_argument(
        "--obs-log-dir",
        default=os.path.join(os.environ.get("APPDATA", ""), "obs-studio", "logs"),
    )
    ap.add_argument("--ndi-runtime-dll", default=DEFAULT_NDI_RUNTIME_DLL)
    # #756 — the deployed genlock build-SHA marker file for the cross-box parity gate. Default is
    # the Windows bundle path; imag's service passes /opt/obs-genlock/GENLOCK_BUILD_SHA.txt.
    ap.add_argument("--genlock-build-sha-file", default=DEFAULT_GENLOCK_BUILD_SHA_FILE)
    ap.add_argument("--obs-dll", default=DEFAULT_OBS_DLL)
    # #826 — the strih OBS-identity machine-check facet. --obs-install-scan-root is repeatable;
    # --ahk-path defaults to strih's NL_STARTUP.ahk -- a box that has none (stream) just gathers ""
    # for every ahk_* key, which the gate correctly reads as "this facet does not apply here".
    ap.add_argument(
        "--obs-install-scan-root", action="append", default=None,
        help="a root to scan for launchable obs*.exe/*ME.exe installs (repeatable; "
             f"default: {', '.join(DEFAULT_OBS_INSTALL_SCAN_ROOTS)})",
    )
    ap.add_argument("--startup-shortcut", default=DEFAULT_STARTUP_SHORTCUT)
    ap.add_argument("--ahk-path", default=DEFAULT_AHK_PATH)
    args = ap.parse_args(argv)
    if args.obs_install_scan_root is None:
        args.obs_install_scan_root = list(DEFAULT_OBS_INSTALL_SCAN_ROOTS)

    state = _State()
    handler = make_handler(args, state)
    httpd = ThreadingHTTPServer(("0.0.0.0", args.port), handler)
    log(
        f"bundle-state-server listening on :{args.port} "
        f"(obs_host={args.obs_host}, obs_log_dir={args.obs_log_dir})"
    )
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        log("shutting down (KeyboardInterrupt)")
    finally:
        httpd.server_close()


if __name__ == "__main__":
    main()
