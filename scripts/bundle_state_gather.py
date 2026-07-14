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
"""
from __future__ import annotations

import os
import re
import sys

# The three OBS module scan paths that can each shadow-load a `distroav.dll` (#124, EPIC #125) —
# mirrors `.claude/commands/drift-guard.md` step 1c EXACTLY (same three roots, same rationale: a
# second copy in any of these can silently shadow the intended genlock build, #119).
DISTROAV_SCAN_ROOTS = (
    r"C:\Program Files\obs-studio\obs-plugins\64bit",
    r"C:\ProgramData\obs-studio\plugins",
    # %APPDATA% is resolved by the caller (this module stays free of env lookups so it is
    # trivially testable against a tmp_path tree); see bundle-state-server.py's gather step.
)


def obs_version_from_log(text):
    """"OBS 32.1.2 (64-bit, windows)" -> "32.1.2". "" if the log never printed it (UNKNOWN, per
    drift-guard's never-a-false-clean contract — the caller simply omits the key)."""
    m = re.search(r"OBS (\d+\.\d+\.\d+)", text or "")
    return m.group(1) if m else ""


def distroav_version_from_log(text):
    """"DistroAV (Version 6.2.1)" -> "6.2.1". "" if absent."""
    m = re.search(r"DistroAV \(Version (\d+\.\d+\.\d+)\)", text or "")
    return m.group(1) if m else ""


def output_fps_from_log(text):
    """The `fps:` line INSIDE the first "video settings reset:" block -> "30". "" if either the
    reset block or its fps line is absent. Mirrors `.claude/commands/drift-guard.md` step 1's
    PowerShell block-scoped scan line-for-line (first reset block only, first fps line inside it)."""
    lines = (text or "").splitlines()
    for i, line in enumerate(lines):
        if "video settings reset:" in line:
            for later in lines[i:]:
                m = re.search(r"fps:\s+(\d+)/", later)
                if m:
                    return m.group(1)
            break  # found the reset block but no fps line followed it — UNKNOWN, don't scan past it
    return ""


def genlock_wall_clock_from_log(text):
    """"1" (render tick ENABLED), "0" (DISABLED), "" if the build never logged the marker at all
    (a stock OBS, or OBS never launched — UNKNOWN, never guessed)."""
    t = text or ""
    if re.search(r"genlock:.*render tick ENABLED", t):
        return "1"
    if re.search(r"genlock:.*render tick DISABLED", t):
        return "0"
    return ""


def genlock_capability_from_log(text):
    """Every `genlock:` capability-marker line (render tick ENABLED / sub-frame jitter reserve /
    timestamp-aligned release) joined with '\\n' — the #122 build-unique tell. "" if the build
    emits none (a stock OBS). Gathered for forward-compat with the opt-in manifest/capability
    facet in drift-guard.sh; harmless when no manifest= is supplied (the current CI invocation)."""
    pattern = re.compile(r"genlock:.*(render tick ENABLED|sub-frame jitter reserve|timestamp-aligned release)")
    matches = [line for line in (text or "").splitlines() if pattern.search(line)]
    return "\n".join(matches)


def distroav_dll_paths(scan_roots):
    """Every `distroav.dll` found (case-insensitive) under *scan_roots* (each walked recursively),
    comma-joined, in the order given. "" if none found anywhere (UNKNOWN — never a false clean;
    drift_check_plugin_paths in drift-guard.sh already treats an empty observed set this way)."""
    found = []
    for root in scan_roots:
        if not root or not os.path.isdir(root):
            continue
        for dirpath, _dirnames, filenames in os.walk(root):
            for name in filenames:
                if name.lower() == "distroav.dll":
                    found.append(os.path.join(dirpath, name))
    return ",".join(found)


def ndi_input_latency_csv(ndi_inputs):
    """*ndi_inputs* is `{name: {"settings": {...}, ...}}` (the exact shape
    `~/.cache/obsprobe/obs_inputs.py` / `bundle-state-server.py`'s WS gather produces). Returns a
    sorted `"name=latency,..."` CSV of every GENLOCKED BROADCAST-PATH input — i.e. every NDI input
    whose settings carry `genlock_fifo: true` (the live marker for "this is a genlock-managed
    program/camera-ingest input", proven on strih + stream 2026-07-10: it selects exactly the
    camera ingests + program feed and excludes preview/CG/lyrics inputs, matching
    `.claude/commands/drift-guard.md`'s documented "genlocked broadcast-path inputs only" scope
    WITHOUT hardcoding scene/input names that would go stale as scenes are edited).
    An input with `genlock_fifo=true` but no readable `latency` setting is skipped (never a
    fabricated value) — drift_check_inputs then simply sees one fewer entry, not a wrong one.
    "" if there are no genlocked inputs at all (UNKNOWN downstream, never a silent clean)."""
    pairs = []
    for name, info in (ndi_inputs or {}).items():
        settings = (info or {}).get("settings") or {}
        if settings.get("genlock_fifo") is not True:
            continue
        if "latency" not in settings:
            continue
        pairs.append((name, str(settings["latency"])))
    pairs.sort(key=lambda kv: kv[0])
    return ",".join(f"{name}={latency}" for name, latency in pairs)


def record_dir_stats(record_dir):
    """#652: PURE, testable filesystem stats over the top-level files of *record_dir* (the OBS
    record directory) — powers the `/record-dir-stats.json` endpoint (bundle-state-server.py),
    which recording-e2e.sh's preflight curls to WARN (never fail) when a box's accumulated E2E
    test recordings exceed a disk budget. The live incident this addresses: strih accumulated
    ~500 GB / stream ~139 GB of forgotten test recordings (back to 2026-06-17), invisible until
    the disk nearly filled (17 GB free).

    Only the TOP-LEVEL files count (OBS records flat into this directory; a subdirectory is not
    this harness's business). Never raises: an unreadable or missing directory (unmounted, wrong
    path after a profile switch, permission error) returns the same zero result a genuinely empty
    directory would — a bogus large number is worse than under-reporting, since the caller could
    otherwise fire a false "over budget" WARN from a stat() crash it half-caught. Every degrade
    path is logged (comprehensive-logging.md) rather than silently swallowed.
    """
    total_bytes = 0
    file_count = 0
    oldest_mtime = None
    try:
        with os.scandir(record_dir) as it:
            for entry in it:
                try:
                    if not entry.is_file(follow_symlinks=False):
                        continue
                    st = entry.stat(follow_symlinks=False)
                except OSError as e:
                    # A single entry vanishing mid-scan (deleted while we're iterating, e.g. an
                    # in-progress OBS write finishing) is expected and harmless — skip just that
                    # entry, never abort the whole stats gather over one transient race.
                    print(
                        f"WARNING: record_dir_stats: skipping unreadable entry in "
                        f"{record_dir!r}: {e}", file=sys.stderr,
                    )
                    continue
                total_bytes += st.st_size
                file_count += 1
                if oldest_mtime is None or st.st_mtime < oldest_mtime:
                    oldest_mtime = st.st_mtime
    except OSError as e:
        # Missing/unmounted/permission-denied directory (e.g. a stale path after a profile
        # switch) — degrade to the same zero result an empty directory would report. A bogus
        # large number from a half-caught crash would be worse than under-reporting here.
        print(
            f"WARNING: record_dir_stats: could not read directory {record_dir!r}: {e}",
            file=sys.stderr,
        )
    return {"total_bytes": total_bytes, "file_count": file_count, "oldest_mtime": oldest_mtime}


def genlock_build_sha_from_file(path):
    """#756 — the box's DEPLOYED genlock build commit SHA, read from its `GENLOCK_BUILD_SHA.txt`
    (imag: `/opt/obs-genlock/GENLOCK_BUILD_SHA.txt`; the Windows boxes: the SAME file in the
    deployed genlock bundle). This is the value the #756 CROSS-BOX parity gate compares across the
    fleet — a peer-parity assert (every box on ONE build) that catches the stale-imag skew the
    origin/main ref-compare misses during a long-lived dev train (#530/#756).

    Returns the stripped first non-empty line, or "" when the file is missing / unreadable / empty
    (UNKNOWN downstream — never a guessed or fabricated SHA; the parity engine treats an unread box
    as INCOMPLETE and refuses, per drift-guard's never-a-false-clean contract). Only the leading
    token of the first non-blank line is kept, so a stray trailing comment/newline in the marker
    file can never leak into the compared SHA."""
    if not path:
        return ""
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                line = line.strip()
                if line:
                    return line.split()[0]
    except OSError as e:
        print(
            f"WARNING: genlock_build_sha_from_file: could not read {path!r}: {e}",
            file=sys.stderr,
        )
    return ""


def build_bundle_state(
    *,
    obs_version="",
    distroav_version="",
    ndi_runtime="",
    output_fps="",
    genlock_wall_clock="",
    ndi_input_latency="",
    distroav_dll_paths="",
    genlock_capability="",
    genlock_build_sha="",
):
    """Assemble the flat bundle-state dict `version-integrity-gate.sh --win-state`'s
    `compare_args_from_state()` parses. Every value is a STRING (its regex requires a quoted JSON
    string — a bare number/bool would silently fail to match and read as UNKNOWN, the opposite of
    what a present-but-unread value should mean). A key whose gather came back empty is OMITTED
    entirely (cleaner payload; compare_args_from_state treats an absent key and an empty-string
    value identically — both UNKNOWN — so this is a presentation choice, not a behavior one).

    #756: `genlock_build_sha` is the box's deployed genlock build SHA — the version-integrity gate
    reads it out of every box's state and runs the CROSS-BOX parity assert (fleet must be on ONE
    build). Same omit-when-empty rule as every other facet."""
    values = {
        "obs_version": obs_version,
        "distroav_version": distroav_version,
        "ndi_runtime": ndi_runtime,
        "output_fps": output_fps,
        "genlock_wall_clock": genlock_wall_clock,
        "ndi_input_latency": ndi_input_latency,
        "distroav_dll_paths": distroav_dll_paths,
        "genlock_capability": genlock_capability,
        "genlock_build_sha": genlock_build_sha,
    }
    return {k: v for k, v in values.items() if v}
