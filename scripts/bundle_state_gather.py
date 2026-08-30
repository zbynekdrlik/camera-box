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

import hashlib
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


# #1222 — the strih bundle-state gather's latency grew LINEARLY with the live OBS log size: a
# ~13h session (75 MB log) made every *_from_log parser above re-scan the WHOLE file on EVERY
# /bundle-state.json request (~0.25 s/MB measured, +19 s at 75 MB), pushing gather past
# recording-e2e.sh's `curl --max-time 30` and refusing the [0/8] version-integrity gate. Every
# fact these parsers need lives at the EDGES of the log, never the middle: the startup banner
# (obs_version / distroav_version / the first "video settings reset:" fps line / the FIRST
# genlock capability markers) is written once at process start, and the "current state" a caller
# might care about (the newest genlock capability marker) is always in the most recent lines. So
# bound the read to a HEAD slice (startup banner) + a TAIL slice (newest state), independent of
# how large the file has grown.
LOG_HEAD_BYTES = 2 * 1024 * 1024  # ~2 MB — a wide margin over the startup banner (#1222 measured
                                   # it sitting in the first few KB of a real log in practice).
LOG_TAIL_BYTES = 5 * 1024 * 1024  # ~5 MB — the newest state a caller might need (e.g. the latest
                                   # genlock capability marker).
# A separator that can never fake a real log line: no digits (so it can never satisfy a
# `\d+\.\d+\.\d+` / `fps:\s+\d+/` style pattern above), no colon-prefixed keyword any parser
# scans for ("OBS ", "DistroAV (Version", "video settings reset:", "genlock:"), and newline-padded
# on both sides so a byte-cut mid-line on either side of the join can never merge into something a
# parser could mistake for a real one.
# #1222 review: verified digit-free by construction (a future unanchored `\d+`-style parser
# would violate the claim above otherwise) -- keep it that way if this text ever changes.
LOG_BOUNDED_READ_SEPARATOR = (
    "\n\n===== bounded log read: middle omitted (head+tail only) =====\n\n"
)


def read_bounded_log_text(path, head_bytes=LOG_HEAD_BYTES, tail_bytes=LOG_TAIL_BYTES):
    """#1222 — the raw text of *path*, bounded to at most `head_bytes + tail_bytes +
    len(LOG_BOUNDED_READ_SEPARATOR)` characters, regardless of the file's actual size. A file no
    larger than `head_bytes + tail_bytes` is returned WHOLE, byte-for-byte (no separator, no
    truncation) — the common case for a freshly-started OBS session and for every existing test
    fixture in this suite. A larger file returns its first `head_bytes` bytes joined to its last
    `tail_bytes` bytes via LOG_BOUNDED_READ_SEPARATOR (see that constant's own doc comment for why
    it can never be mistaken for a real log line by any parser above). Read in BINARY mode and
    decoded with `errors="replace"` — a byte-boundary cut mid multi-byte UTF-8 character degrades
    to a harmless U+FFFD, never a crash. Unlike the original whole-file text-mode read this
    replaces, a Windows CRLF line ending is NOT translated to a bare `\n` here; every parser above
    is CRLF-tolerant (`splitlines()` strips a trailing `\r`, and no regex here crosses a line
    boundary), so this has no observed behavioral effect, but it is a real difference worth
    knowing if a future parser is added.

    "" if *path* is missing/unreadable — the same UNKNOWN-downstream contract as the whole-file
    read this replaces (callers already treat an empty log text as every derived facet coming
    back empty)."""
    try:
        size = os.path.getsize(path)
        with open(path, "rb") as f:
            if size <= head_bytes + tail_bytes:
                return f.read().decode("utf-8", errors="replace")
            head = f.read(head_bytes)
            f.seek(size - tail_bytes)
            tail = f.read(tail_bytes)
    except OSError as e:
        print(f"WARNING: read_bounded_log_text: could not read {path!r}: {e}", file=sys.stderr)
        return ""
    return (
        head.decode("utf-8", errors="replace")
        + LOG_BOUNDED_READ_SEPARATOR
        + tail.decode("utf-8", errors="replace")
    )


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


# #826 — filename pattern for a launchable OBS-shaped executable: `obs<digits>.exe` (obs64.exe,
# obs32.exe, the pinned genlock build's own name) OR a legacy `<name>ME.exe`-style build (the
# pre-genlock era's own naming, e.g. a literal "2ME.exe"). Case-insensitive — Windows filenames.
_OBS_EXE_RE = re.compile(r"(?i)^(obs\d*\.exe|\S*me\.exe)$")


def obs_installs_under(scan_roots):
    """#826 — every launchable OBS-shaped executable found under *scan_roots* (each walked
    recursively), sorted (case-insensitively) and comma-joined. Mirrors `distroav_dll_paths`'s
    walk-and-collect shape exactly (same "PURE, fed real filesystem roots" pattern already
    established in this module).

    A folder renamed aside (e.g. `D:\\_APPS\\_RETIRED_1ME-obs_2026-07-27`) is STILL walked and its
    exe is STILL reported — this is the whole point of the #826 acceptance: renaming a dormant
    install out of the way is not the same as removing it, and it can still be launched by hand
    (the exact 2026-07-27 incident: an agent ran a dead-variable-referenced `.lnk` and woke a
    year-old OBS 31.1.2, which then squatted TCP :4455 before the pinned genlock build could).

    "" when no *scan_roots* entry exists or none contains a match (never guessed)."""
    found = []
    for root in scan_roots:
        if not root or not os.path.isdir(root):
            continue
        for dirpath, _dirnames, filenames in os.walk(root):
            for name in filenames:
                if _OBS_EXE_RE.match(name):
                    found.append(os.path.join(dirpath, name))
    return ",".join(sorted(found, key=str.lower))


# #826 / #1222c — the ONE canonical "is this an OBS-shaped process name" pattern (obs64, obs32,
# bare obs — case-insensitive), shared between obs_process_count_from_listing below and
# bundle-state-server.py's _parse_tasklist_obs_process_names (a #1222c review finding: the two
# used to carry independent copies of the identical regex, a DRY violation that could silently
# drift apart on a future rename).
OBS_PROCESS_NAME_RE = re.compile(r"(?i)^obs\d*$")


def obs_process_count_from_listing(text):
    """#826 — count of currently-running OBS-class processes, from a plain newline-separated list
    of process NAMES (no `.exe` suffix — the shape `Get-Process | Select-Object -ExpandProperty
    Name` produces on Windows). Matches `obs<digits>` case-insensitively (obs64, obs32, bare obs)
    via the shared OBS_PROCESS_NAME_RE above.

    "" (never "0") when *text* itself is empty/unread — an unreachable box must read UNKNOWN, not
    a false "zero processes confirmed running" (the same never-a-false-clean discipline every
    other facet in this module follows)."""
    if not (text or "").strip():
        return ""
    count = 0
    for line in text.splitlines():
        if OBS_PROCESS_NAME_RE.match(line.strip()):
            count += 1
    return str(count)


# #826 — NL_STARTUP.ahk's own variable syntax (confirmed live on strih, issue #826 comments):
#   app1_run  := 1
#   app1_path := "C:\ProgramData\...\OBS Studio.lnk"
#   app1_binarypath := "D:\_APPS\1ME-obs\1ME.lnk"     <- the dead leftover that caused the incident
#   app2_run  := 0
#   app2_path := "D:\_APPS\2ME-obs\2ME.lnk"
def ahk_app1_shortcut_path(text):
    """#826 — the `app1_path := "..."` shortcut NL_STARTUP.ahk launches. Only the FIRST match is
    used (AHK assigns each variable once). "" when absent — this box has no NL_STARTUP.ahk at all
    (only strih runs it; stream has none, per `.claude/skills/obs-ops`), or the text is unread."""
    m = re.search(r'app1_path\s*:=\s*"([^"]*)"', text or "")
    return m.group(1) if m else ""


def ahk_app1_run(text):
    """#826 — the `app1_run := N` flag: "1" enabled / "0" disabled / "" if the line is absent
    (no NL_STARTUP.ahk on this box, or unread)."""
    m = re.search(r"app1_run\s*:=\s*(\d+)", text or "")
    return m.group(1) if m else ""


def ahk_dead_config_present(text):
    """#826 — "1" when NL_STARTUP.ahk still carries the dead `app1_binarypath` leftover (the exact
    variable an agent mistook for the box's canonical launcher during the #826 incident) OR an
    ENABLED `app2_run := 1` block (the issue's "config states one truth" cleanup requirement).
    "0" when the text was read and neither leftover is present. "" (UNKNOWN, distinct from "read
    and clean") when there is no AHK text to read at all — e.g. this box has no NL_STARTUP.ahk."""
    t = text or ""
    if not t.strip():
        return ""
    has_dead_binarypath = "app1_binarypath" in t
    m = re.search(r"app2_run\s*:=\s*(\d+)", t)
    app2_enabled = bool(m and m.group(1) == "1")
    return "1" if (has_dead_binarypath or app2_enabled) else "0"


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


def component_sha256(path):
    """#770 — the lowercase 64-hex sha256 of the DEPLOYED file at *path* (a plugin/core binary such
    as the live `distroav.dll` / `obs.dll`), read in binary in bounded chunks. This is the BYTE
    identity the `[0/8]` version-integrity gate compares against the #120 BUNDLE_MANIFEST — the
    truth the hand-written `GENLOCK_BUILD_SHA.txt` MARKER only POINTS at. It closes the wrong
    direction of the #119/#767 stale-bytes hole: a marker advanced to build X while the DLL bytes
    are an older build passes the marker-only cross-box parity, but its real sha256 will not match
    build X's manifest.

    Returns "" when *path* is empty/None, is not a regular file (missing, or a directory), or
    cannot be read — UNKNOWN downstream, NEVER a fabricated/zero SHA that would let a missing plugin
    read as "clean" (the same never-a-false-clean discipline every other facet in this module
    follows). The read never raises: a transient I/O error degrades to "" with a WARNING, exactly
    like `genlock_build_sha_from_file` above."""
    if not path or not os.path.isfile(path):
        return ""
    try:
        h = hashlib.sha256()
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(1024 * 1024), b""):
                h.update(chunk)
        return h.hexdigest()
    except OSError as e:
        print(
            f"WARNING: component_sha256: could not read {path!r}: {e}",
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
    obs_dll_sha256="",
    distroav_dll_sha256="",
    genlock_build_sha="",
    obs_installs="",
    port4455_owner_path="",
    port4455_owner_version="",
    obs_process_count="",
    ahk_app1_shortcut_path="",
    ahk_app1_run="",
    ahk_dead_config_present="",
    shortcut_target_path="",
    shortcut_workdir="",
):
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

    Note: `dantesync_version` was added here for #862, then REVERTED in its own follow-up fix —
    the deployed strih/stream servers never picked up the new key (half-wired), and the gate now
    reads every node's dantesync version uniformly via `dantesync --version` over SSH instead
    (scripts/dantesync-version-gate.sh), with no bundle-state involvement at all."""
    values = {
        "obs_version": obs_version,
        "distroav_version": distroav_version,
        "ndi_runtime": ndi_runtime,
        "output_fps": output_fps,
        "genlock_wall_clock": genlock_wall_clock,
        "ndi_input_latency": ndi_input_latency,
        "distroav_dll_paths": distroav_dll_paths,
        "genlock_capability": genlock_capability,
        "obs_dll_sha256": obs_dll_sha256,
        "distroav_dll_sha256": distroav_dll_sha256,
        "obs_installs": obs_installs,
        "port4455_owner_path": port4455_owner_path,
        "port4455_owner_version": port4455_owner_version,
        "obs_process_count": obs_process_count,
        "ahk_app1_shortcut_path": ahk_app1_shortcut_path,
        "ahk_app1_run": ahk_app1_run,
        "ahk_dead_config_present": ahk_dead_config_present,
        "shortcut_target_path": shortcut_target_path,
        "shortcut_workdir": shortcut_workdir,
        "genlock_build_sha": genlock_build_sha,
    }
    return {k: v for k, v in values.items() if v}
