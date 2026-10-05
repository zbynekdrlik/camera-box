#!/usr/bin/env python3
"""#650 — PURE parsers + builders for the standing :8899 bundle-state HTTP service.

This module holds every piece of `scripts/bundle-state-server.py` that can be exercised
WITHOUT a live Windows box / live OBS — the same PURE-function-vs-flow split this repo already
uses in `scripts/drift-guard.sh` (parsers unit-tested by sourcing the script; the executed flow
is verified live on the rig) and `scripts/obs_burn_filter.py` (`compute_burn_on` pure, `cmd_check`/
`cmd_add` driven through a fake `_rpc` in tests/python/test_obs_burn_filter.py).

Why this exists (#650): `scripts/version-integrity-gate.sh --win-state` (#123/#119) and
`scripts/recording-e2e.sh`'s `fetch_box_state()` expect a live `http://<box>:8899/bundle-state.json`
serving the drift-guard `--compare` OBSERVED values (see `version-integrity-gate.sh`'s own doc
comment for the exact flat-JSON schema). Historically those values were gathered BY HAND, per
`.claude/commands/drift-guard.md` step 1/1b/1c, by an operator/agent holding the win-* MCP — fine
for an interactive `/drift-guard` run, but the automatic `pull_request`-triggered full-path-e2e CI
run (#406/#312 item5) has neither a human nor MCP access, so the gate always saw both boxes UNKNOWN
(exit 11) and refused. This module + `bundle-state-server.py` gather the SAME values, on-box,
so a standing service can answer the gate unattended.

Only the drift-guard `--compare` keys `version-integrity-gate.sh` MANDATORILY checks (i.e. the ones
that are NOT opt-in behind a `manifest=`/`burn_env=`/`genlock_source_latency=` key — see
`compare_observed()` in `scripts/drift-guard.sh`) are gathered here: `obs_version`,
`distroav_version`, `ndi_runtime`, `output_fps`, `genlock_wall_clock`, `ndi_input_latency`,
`distroav_dll_paths`. `genlock_capability` is ALSO gathered (harmless — it is only ever consulted
by the engine when a `manifest=` is supplied, which the current CI invocation never does) so the
bundle-state payload stays forward-compatible with the opt-in build-SHA facet without any schema
change later.

Issue 1386: this module is the ONE import name every consumer uses (bundle-state-server.py,
scripts/lib/av-soak.sh, recording-e2e.sh's free-space check, the tests). It holds the payload
builder -- `build_bundle_state` over the served key order `BUNDLE_STATE_KEYS` -- and re-exports
(`__all__`) every facet parser from its flat sibling modules:

  bundle_state_log        the #1222 bounded log read, the in-log time helpers, the timestamped
                          tail, the box clock + log-head age
  bundle_state_genlock    OBS / DistroAV banner, fps, genlock markers, the genlock LOCK facet,
                          the PROGRAM-render freeze and relock-burst facets
  bundle_state_audio      audio telemetry / ts_lag, the reference band, buffered_ms, audio mixer
  bundle_state_vban       the obs-vban pacer loss facet
  bundle_state_av_offset  the av-sync dock offset trend, live age, quality + quality age
  bundle_state_host       install scans, the NDI latency CSV, tasklist / VB-Matrix, the AHK
                          readers, record-dir stats + free-space verdict, build sha, byte sha256,
                          the OBS handle count (SystemProcessInformation parser, Linux /proc)

Every one of them ships with the server: the deployed tree is declared ONCE in
scripts/lib/bundle-state-files.txt (bash twin scripts/lib/bundle-state-files.sh, iterated by
setup-strih.sh / setup-imag.sh; the Windows runbook reads the .txt), and
tests/python/test_bundle_state_files_1386.py pins that list against the server's real imports.
"""
from __future__ import annotations

import os
import sys

# The facet modules are flat siblings of this file (the deployed tree is flat: /opt/camera-box on
# the Linux boxes, C:\ProgramData\camera-box on Windows). A caller that loads this file by path
# (importlib, several tests) without its directory on sys.path still resolves them.
_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.append(_HERE)

from bundle_state_audio import (  # noqa: E402 -- after the sibling path setup above
    AUDIO_REF_BAND_DEFAULT_SRC,
    AUDIO_REF_BAND_DUTY_MARGIN_MS,
    AUDIO_REF_BAND_WINDOW_CAP,
    AUDIO_TS_LAG_STALE_AFTER_S,
    BUFFERED_MS_DEFAULT_SRC,
    audio_mixer_from_log,
    audio_ref_band_from_log,
    audio_telemetry_from_log,
    audio_ts_lag_ms_from_log,
    buffered_ms_series_from_log,
)
from bundle_state_av_offset import (  # noqa: E402 -- after the sibling path setup above
    AV_OFFSET_BASELINE_WINDOW_S,
    AV_OFFSET_RECENT_WINDOW_S,
    av_offset_dock_live_age_from_log,
    av_offset_quality_age_from_log,
    av_offset_quality_from_log,
    av_offset_series_from_log,
)
from bundle_state_genlock import (  # noqa: E402 -- after the sibling path setup above
    RELOCK_BURSTS_MIN_DEFAULT,
    distroav_version_from_log,
    genlock_capability_from_log,
    genlock_lock_facet_from_log,
    genlock_wall_clock_from_log,
    obs_version_from_log,
    output_fps_from_log,
    _parse_relock_event,
    program_render_lagged_from_log,
    relock_bursts_from_log,
    _summarize_relock_bursts,
)
from bundle_state_host import (  # noqa: E402 -- after the sibling path setup above
    DISTROAV_SCAN_ROOTS,
    OBS_LIVE_MIN_MEM_KB,
    OBS_PROCESS_NAME_RE,
    SPI_OFFSETS,
    VB_MATRIX_EXE_RE,
    VB_MATRIX_PROCESS_NAME_RE,
    ahk_app1_run,
    ahk_app1_shortcut_path,
    ahk_dead_config_present,
    component_sha256,
    distroav_dll_paths,
    filetime_to_epoch,
    genlock_build_sha_from_file,
    linux_obs_handles,
    ndi_input_latency_csv,
    obs_handles_facet,
    obs_installs_under,
    obs_process_count_from_listing,
    proc_btime,
    proc_nofile_soft_limit,
    proc_stat_start_ticks,
    record_dir_stats,
    recordings_free_line,
    recordings_free_verdict,
    system_processes_from_spi,
    tasklist_mem_kb,
    tasklist_row_is_live_obs,
    vb_matrix_install_present_under,
    vb_matrix_process_from_listing,
    vb_matrix_running_facet,
)
from bundle_state_log import (  # noqa: E402 -- after the sibling path setup above
    LOG_BOUNDED_READ_SEPARATOR,
    LOG_HEAD_BYTES,
    LOG_HEAD_CLOCK_SLACK_S,
    LOG_TAIL_BYTES,
    local_seconds_of_day,
    obs_log_head_age_s_from_log,
    read_bounded_log_text,
    timestamped_tail_lines,
)
from bundle_state_vban import (  # noqa: E402 -- after the sibling path setup above
    VBAN_BASELINE_S,
    VBAN_LEGACY_LOSS_EVENTS,
    VBAN_LOSS_EVENTS,
    VBAN_LOSS_MS,
    VBAN_LOSS_WINDOW_S,
    VBAN_MULTI_SENDER_GAP_S,
    VBAN_PREDECESSOR_SCAN,
    vban_pacer_loss_from_log,
)

__all__ = [
    # this module
    "BUNDLE_STATE_KEYS",
    "build_bundle_state",
    # bundle_state_audio
    "AUDIO_REF_BAND_DEFAULT_SRC",
    "AUDIO_REF_BAND_DUTY_MARGIN_MS",
    "AUDIO_REF_BAND_WINDOW_CAP",
    "AUDIO_TS_LAG_STALE_AFTER_S",
    "BUFFERED_MS_DEFAULT_SRC",
    "audio_mixer_from_log",
    "audio_ref_band_from_log",
    "audio_telemetry_from_log",
    "audio_ts_lag_ms_from_log",
    "buffered_ms_series_from_log",
    # bundle_state_av_offset
    "AV_OFFSET_BASELINE_WINDOW_S",
    "AV_OFFSET_RECENT_WINDOW_S",
    "av_offset_dock_live_age_from_log",
    "av_offset_quality_age_from_log",
    "av_offset_quality_from_log",
    "av_offset_series_from_log",
    # bundle_state_genlock
    "RELOCK_BURSTS_MIN_DEFAULT",
    "distroav_version_from_log",
    "genlock_capability_from_log",
    "genlock_lock_facet_from_log",
    "genlock_wall_clock_from_log",
    "obs_version_from_log",
    "output_fps_from_log",
    "_parse_relock_event",
    "program_render_lagged_from_log",
    "relock_bursts_from_log",
    "_summarize_relock_bursts",
    # bundle_state_host
    "DISTROAV_SCAN_ROOTS",
    "OBS_LIVE_MIN_MEM_KB",
    "OBS_PROCESS_NAME_RE",
    "SPI_OFFSETS",
    "VB_MATRIX_EXE_RE",
    "VB_MATRIX_PROCESS_NAME_RE",
    "ahk_app1_run",
    "ahk_app1_shortcut_path",
    "ahk_dead_config_present",
    "component_sha256",
    "distroav_dll_paths",
    "filetime_to_epoch",
    "genlock_build_sha_from_file",
    "linux_obs_handles",
    "ndi_input_latency_csv",
    "obs_handles_facet",
    "obs_installs_under",
    "obs_process_count_from_listing",
    "proc_btime",
    "proc_nofile_soft_limit",
    "proc_stat_start_ticks",
    "record_dir_stats",
    "recordings_free_line",
    "recordings_free_verdict",
    "system_processes_from_spi",
    "tasklist_mem_kb",
    "tasklist_row_is_live_obs",
    "vb_matrix_install_present_under",
    "vb_matrix_process_from_listing",
    "vb_matrix_running_facet",
    # bundle_state_log
    "LOG_BOUNDED_READ_SEPARATOR",
    "LOG_HEAD_BYTES",
    "LOG_HEAD_CLOCK_SLACK_S",
    "LOG_TAIL_BYTES",
    "local_seconds_of_day",
    "obs_log_head_age_s_from_log",
    "read_bounded_log_text",
    "timestamped_tail_lines",
    # bundle_state_vban
    "VBAN_BASELINE_S",
    "VBAN_LEGACY_LOSS_EVENTS",
    "VBAN_LOSS_EVENTS",
    "VBAN_LOSS_MS",
    "VBAN_LOSS_WINDOW_S",
    "VBAN_MULTI_SENDER_GAP_S",
    "VBAN_PREDECESSOR_SCAN",
    "vban_pacer_loss_from_log",
]

# The flat payload keys in their SERVED order (the JSON key order of /bundle-state.json), each
# with why it exists. `build_bundle_state` accepts exactly these keywords and omits an empty one.
BUNDLE_STATE_KEYS = (
    "obs_version",
    "distroav_version",
    "ndi_runtime",
    "output_fps",
    "genlock_wall_clock",
    "ndi_input_latency",
    "distroav_dll_paths",
    "genlock_capability",
    "obs_dll_sha256",
    "distroav_dll_sha256",
    "obs_installs",
    "port4455_owner_path",
    "port4455_owner_version",
    "obs_process_count",
    "ahk_app1_shortcut_path",
    "ahk_app1_run",
    "ahk_dead_config_present",
    "shortcut_target_path",
    "shortcut_workdir",
    "genlock_build_sha",
    # #1226 — the audio-timeline-lag facet the dev1 audio-lag watchdog reads; same
    # omit-when-empty rule (absent facet == UNKNOWN downstream, never a fake 0).
    "audio_ts_lag_ms",
    "audio_ts_lag_src",
    # #1231 — the freshness age (in-log seconds the freshest #800 line sits behind the log head);
    # present ("0" when fresh) whenever ANY #800 line exists, "" only when telemetry is absent.
    # A large value -> the dev1 decision surfaces a STALE (stopped-while-log-advancing) state.
    "audio_ts_lag_age_s",
    # #1265 — the per-REFERENCE-source (mbc on stream) ts_lag BAND SHAPE (base/high/low/duty/n),
    # from `audio_ref_band_from_log`. Same omit-when-empty rule; the dev1 audio-lag watchdog's
    # BAND arm reads these to catch a tens-of-ms bimodal/creeping drift the 5000 ms MAX-facet is
    # blind to, and recording-e2e.sh's #856 apply reads the derived verdict to HOLD when the run's
    # audio timeline was unstable.
    "audio_ref_lag_src",
    "audio_ref_lag_base_ms",
    "audio_ref_lag_high_ms",
    "audio_ref_lag_low_ms",
    "audio_ref_lag_duty_pct",
    "audio_ref_lag_n",
    # #1267 — the av-sync dock measured-offset trend the dev1 upstream-step watchdog reads: the
    # RECENT-vs-BASELINE median offset (a sustained step = a physical upstream A/V shift), the
    # CURRENT genlock pin + a pin-stability flag (a pin move -> the dev1 REPIN hold, never a
    # false step), the in-log freshness age (-> STALE when the dock stops), and the per-window
    # sample counts (too few -> UNKNOWN). Same omit-when-empty rule (absent == UNKNOWN, never 0).
    "av_offset_recent_med_ms",
    "av_offset_base_med_ms",
    "av_offset_pin",
    "av_offset_pin_stable",
    "av_offset_age_s",
    "av_offset_n_recent",
    "av_offset_n_base",
    # #1319 — the dock-LIVE freshness age (in-log seconds behind the log head of the freshest
    # `av-sync-dock: diag ... locked=yes` heartbeat). Lets the dev1 band decision read
    # IN_BAND_QUIET (dock LIVE, offset in the suggestion dead band) instead of a false STALE.
    # Same omit-when-empty rule (absent == UNKNOWN downstream, never a fake 0).
    "av_offset_dock_live_age_s",
    # #1319 Part 2 — the dock estimator's recent-window measurement QUALITY (median MAD +
    # min matched), from av_offset_quality_from_log. The dev1 band arm reads LOW_QUALITY
    # (log-only, never a page) unless recent_mad_ms <= 15 AND recent_matched_min >= 30, so a
    # noisy/biased dock reading no longer trips the +-30 ms band on its own scatter. Same
    # omit-when-empty rule (absent == quality unjudgeable -> the band proceeds, never a fake 0).
    "av_offset_recent_mad_ms",
    "av_offset_recent_matched_min",
    # #1325 — the in-log age (s) of the freshest dock QUALITY line. The dev1 band/step arms read
    # LOW_QUALITY (no page) when the quality facet is ABSENT (mad None) AND this age is stale (the
    # dock stopped decoding — the 3× false page of 16.9.2026), while an ABSENT age (older box, or
    # a dock actively measuring) keeps the #1319 "absent -> proceed, never swallow a real drift"
    # behaviour. Omit-when-empty (absent == no quality line at all -> UNKNOWN downstream).
    "av_offset_quality_age_s",
    # #1325 — the mbc buffered_ms DRIFT/STEP shape (slope ms/min + max positive refill step +
    # n + freshness age) the dev1 audio-lag watchdog's buffered arm reads (REPORT-ONLY). Same
    # omit-when-empty rule (absent == < 2 buffered readings for the ref source -> UNKNOWN).
    "buffered_ms_slope_ms_per_min",
    "buffered_ms_max_step_ms",
    "buffered_ms_n",
    "buffered_ms_age_s",
    # #1227 — the VB-Matrix presence facet the dev1 VB-Matrix alert watchdog reads. Same
    # omit-when-empty rule: running="0" (installed but the VBAudioMatrix* process is DEAD) is a
    # truthy string and is KEPT (surfaces as DOWN); running="" (a box with no VB-Matrix install,
    # e.g. imag) is dropped -> UNKNOWN downstream, never a false negative. name/pid/start are
    # context only (pid free from the tasklist parse; start best-effort, PID-keyed-cached CIM).
    "vb_matrix_running",
    "vb_matrix_name",
    "vb_matrix_pid",
    "vb_matrix_start",
    # #1320 — the strih PROGRAM-render freeze facet the dev1 render-freeze watchdog reads: the
    # MAX `program-render-audit lagged` over the tail + the in-log age (s) of the most recent
    # window achieving it. Same omit-when-empty rule: "0" (render telemetry live, no freeze) is
    # a truthy string and is KEPT; "" (no program-render-audit line at all) is dropped ->
    # UNKNOWN downstream, never a fabricated 0. From `program_render_lagged_from_log`.
    "program_render_lagged",
    "program_render_lagged_age_s",
    # #1320 — the RELOCK-BURST facet the dev1 render-freeze watchdog's relock arm reads: the MAX
    # per-input burst count (issue 1318 summarize_relock_bursts, >=8 relocks within 1 s) over the
    # #1222 bounded TAIL + the in-log age (s) of the newest relock event. Same omit-when-empty
    # rule: "0" (relock telemetry live, no storm) is truthy and KEPT; "" (NO relock line at all,
    # the steady state) is dropped -> UNKNOWN downstream, never a fabricated 0.
    "relock_bursts",
    "relock_bursts_age_s",
    # issue 1381 -- the audio MIXER real-time facet (the newest complete `audio-stall #1367`
    # dump + its window + in-log age, `audio_mixer_from_log`) and the obs-vban PACER loss facet
    # (per-destination loss-counter increase in the last window, `vban_pacer_loss_from_log`) the
    # dev1 audio-mixer watchdog reads. Same omit-when-empty rule: "0" readings are KEPT, a box
    # with no dump yet / no VBAN output omits them -> UNKNOWN downstream, never a fake 0.
    "audio_mixer_ticks",
    "audio_mixer_ticks_over",
    "audio_mixer_window_ms",
    "audio_mixer_tick_ms",
    "audio_mixer_age_s",
    "vban_pacer_loss_events",
    "vban_pacer_loss_ms",
    "vban_pacer_loss_dest",
    "vban_pacer_age_s",
    # issue 1385 -- the log head's age against the box's own clock (`obs_log_head_age_s_from_log`):
    # the positive "OBS is still logging now" proof the dev1 audio-mixer STALLED verdict needs.
    # "0" is a reading and is KEPT; no timestamped line omits it -> no proof downstream.
    "obs_log_head_age_s",
    # issue 1406 -- the OBS process's handle count (Windows, from NtQuerySystemInformation) or
    # open-fd count (Linux /proc), its pid + start epoch (a restart resets the dev1 obs-handles
    # watchdog's baseline) and the Linux soft open-files limit. Omitted when no OBS process is
    # readable -> UNKNOWN downstream, never a false 0.
    "obs_handles",
    "obs_handles_pid",
    "obs_handles_start",
    "obs_handles_limit",
)
_BUNDLE_STATE_KEY_SET = frozenset(BUNDLE_STATE_KEYS)


def build_bundle_state(**facets):
    """Assemble the flat bundle-state dict `version-integrity-gate.sh --win-state`'s
    `compare_args_from_state()` parses. Every value is a STRING (its regex requires a quoted JSON
    string — a bare number/bool would silently fail to match and read as UNKNOWN, the opposite of
    what a present-but-unread value should mean). A key whose gather came back empty is OMITTED
    entirely (cleaner payload; compare_args_from_state treats an absent key and an empty-string
    value identically — both UNKNOWN — so this is a presentation choice, not a behavior one).

    #756: `genlock_build_sha` is the box's deployed genlock build SHA — the version-integrity gate
    reads it out of every box's state and runs the CROSS-BOX parity assert (fleet must be on ONE
    build). Same omit-when-empty rule as every other facet.

    #770: `obs_dll_sha256`/`distroav_dll_sha256` are the sha256 of the DEPLOYED core/plugin BYTES
    (via `component_sha256`) — the byte identity the version-integrity gate compares against the
    #120 BUNDLE_MANIFEST (drift-guard `--compare` already consumes these keys). They make the
    GENLOCK_BUILD_SHA.txt marker just a POINTER: the truth is the bytes, closing the wrong-direction
    #119/#767 hole (marker advanced, bytes stale) the marker-only cross-box parity cannot catch.
    Same omit-when-empty rule; opt-in (#756-shape) — a box not yet reporting the SHAs is silently
    skipped, never a false clean.

    #826: the strih OBS-identity machine-check facet — `obs_installs` (every launchable OBS-shaped
    exe found), `port4455_owner_path`/`port4455_owner_version` (the process actually owning TCP
    :4455, matched by PATH not just process name — the exact hole the 2026-07-27 incident exposed),
    `obs_process_count` (must be exactly one running), and the startup-chain facts read off
    NL_STARTUP.ahk (`ahk_app1_shortcut_path`/`ahk_app1_run`/`ahk_dead_config_present`) + the
    Start-Menu shortcut's own resolution (`shortcut_target_path`/`shortcut_workdir`). Same
    omit-when-empty rule; `version-integrity-gate.sh` treats the whole group as opt-in per box
    (skipped entirely until a box's bundle-state-server reports at least one of them).

    #1226: `audio_ts_lag_ms`/`audio_ts_lag_src` = the MAX per-source audio-timeline lag (ms behind
    the OS clock) parsed from the newest `audio-telemetry #800` line per source (see
    `audio_ts_lag_ms_from_log`). Same omit-when-empty rule; the dev1 audio-lag alert watchdog
    (#1226) reads it to page when a box's audio pipeline falls sustained behind realtime.

    #1231: `audio_ts_lag_age_s` = the in-log freshness age (seconds the freshest #800 line sits
    behind the log's newest line of any kind, from `audio_telemetry_from_log`). Present ("0" when
    fresh) whenever ANY #800 line exists, "" only when telemetry is absent; a large value lets the
    dev1 decision surface a STALE (telemetry-stopped-while-log-advancing) state distinctly. The
    #1226 `audio_ts_lag_ms` now also EXCLUDES sources gone stale in the tail (concern a).

    Note: `dantesync_version` was added here for #862, then REVERTED in its own follow-up fix —
    the deployed strih/stream servers never picked up the new key (half-wired), and the gate now
    reads every node's dantesync version uniformly via `dantesync --version` over SSH instead
    (scripts/dantesync-version-gate.sh), with no bundle-state involvement at all."""
    unknown = [k for k in facets if k not in _BUNDLE_STATE_KEY_SET]
    if unknown:
        raise TypeError(f"build_bundle_state() got an unexpected keyword argument {unknown[0]!r}")
    return {k: facets[k] for k in BUNDLE_STATE_KEYS if facets.get(k)}
