#!/usr/bin/env python3
"""The host / process / filesystem facets of the bundle-state gather: the DistroAV + OBS install
scans, the genlocked NDI input latency CSV, the tasklist parsers (OBS process count, VB-Matrix
presence), the NL_STARTUP.ahk readers, the record-dir stats + free-space verdict, the deployed
genlock build SHA, the component byte sha256, and the OBS handle count (issue 1406: the pure
SystemProcessInformation parser the Windows reader feeds, and the Linux /proc fd read).

Part of the bundle-state gather split (issue 1386): a PURE facet family re-exported by
`bundle_state_gather` -- import it through that module, never directly (it resolves its flat
siblings). Ships in the :8899 server tree declared in `scripts/lib/bundle-state-files.txt`.
"""
from __future__ import annotations

import csv
import hashlib
import io
import json
import os
import re
import shutil
import struct
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


# #1295 — the minimum RAM (KB) an OBS process must report to count as a LIVE instance. A real OBS
# sits in the hundreds of MB; a DEAD/mid-exit Get-Process/zombie handle reads ~0-45 KB (the live
# 2026-09-12 RESOLUME-SNV pid-58560: WorkingSet64 ~45 KB, 0 threads). tasklist has NO HasExited /
# thread column, so the honest liveness proxy available from a tasklist row is its Mem Usage; a row
# at/below this floor is a zombie, never a live obs64. 1 MB is a wide, safe separator (a live OBS
# never sits below ~45 MB; the zombie was 45 KB), and this limitation is documented because tasklist
# cannot distinguish a truly-exited process from a live one any other way.
OBS_LIVE_MIN_MEM_KB = 1024


def tasklist_mem_kb(field):
    """#1295 — parse a `tasklist /FO CSV /NH` Mem-Usage field ("512,000 K", "45 K", "N/A", "") to
    an int of KB, or None when it carries no usable number (N/A / blank / unparseable). tasklist
    prints memory in KB with a thousands separator and a trailing " K"."""
    if not isinstance(field, str):
        return None
    s = field.strip().replace(" ", " ").rstrip("Kk").replace(",", "").replace(" ", "")
    if not s or not s.lstrip("-").isdigit():
        return None
    return int(s)


def tasklist_row_is_live_obs(mem_field, min_kb=OBS_LIVE_MIN_MEM_KB):
    """#1295 — True iff a tasklist obs-row's Mem-Usage field proves a LIVE instance (>= min_kb KB).
    An unparseable/absent Mem (None) reads NOT-live: a live OBS always reports a real Mem value, so
    excluding an ambiguous row is the fail-safe that keeps a zombie handle from inflating the
    'exactly one obs64' health signal (#1296). tasklist's limitation (no HasExited column, Mem is
    the only liveness proxy) is documented at OBS_LIVE_MIN_MEM_KB."""
    kb = tasklist_mem_kb(mem_field)
    return kb is not None and kb >= min_kb


# #1227 — VB-Audio Matrix presence, for the `vb_matrix_running` facet the dev1 VB-Matrix alert
# watchdog reads. The process image name after its `.exe` is stripped (tasklist prints e.g.
# `VBAudioMatrix_x64.exe`); the pattern enumerates the actual HOSTS — the stream build
# `VBAudioMatrix_x64` and strih's `VBAudioMatrixCoconut_x64` (+ their non-x64 variants), NOT a
# left-open `VBAudioMatrix_Setup` installer that shares the same folder (case-insensitive, anchored
# at both ends so `NotVBAudioMatrix…` / `…_Setup` never match). The exe pattern is derived from the
# SAME base so the two can never drift apart (a #1222c-style DRY finding).
_VB_MATRIX_NAME_BASE = r"(?i)^VBAudioMatrix(Coconut)?(_x64)?"
VB_MATRIX_PROCESS_NAME_RE = re.compile(_VB_MATRIX_NAME_BASE + r"$")
VB_MATRIX_EXE_RE = re.compile(_VB_MATRIX_NAME_BASE + r"\.exe$")


def vb_matrix_process_from_listing(text):
    """#1227 — the running VB-Matrix process from `tasklist /FO CSV /NH` output *text* (each row
    `"Image Name","PID","Session Name","Session#","Mem Usage"`). A `csv.reader` is REQUIRED — the
    Mem Usage column carries a thousands separator INSIDE its quotes (`"18,236 K"`), so a naive
    comma split would mis-column the PID. The image name has its `.exe` stripped before matching
    `VB_MATRIX_PROCESS_NAME_RE`, so a returned `name` is e.g. `VBAudioMatrix_x64`.

    THREE-state return so the caller never reads a FAILED read as a measured absence (issue 1227
    review 🔴, the #833 / `obs_process_count_from_listing` class):
      None       -- the listing is UNREADABLE (empty/whitespace text = a tasklist subprocess
                    failure, since a live box always lists SOME processes; or a `csv.Error`). The
                    caller must treat this as UNKNOWN (facet omitted), NEVER a DOWN.
      ("", "")   -- a VALID listing with no VB-Matrix HOST row (genuinely absent -> the caller reads
                    DOWN when the install is present on disk).
      (name,pid) -- the first VB-Matrix host process found."""
    if not (text or "").strip():
        return None
    try:
        for row in csv.reader(io.StringIO(text)):
            if not row:
                continue
            image_name = row[0]
            base = image_name[:-4] if image_name.lower().endswith(".exe") else image_name
            if VB_MATRIX_PROCESS_NAME_RE.match(base):
                pid = row[1].strip() if len(row) > 1 else ""
                return (base, pid)
    except csv.Error:
        return None
    return ("", "")


def vb_matrix_install_present_under(scan_dirs):
    """#1227 — True iff any `VBAudioMatrix*.exe` exists (recursively) under any of *scan_dirs* — the
    disk-install gate that distinguishes a box that HAS VB-Matrix but its process is dead (stream
    after a reboot with no host -> the facet must read running="0", a real DOWN) from a box that
    never had VB-Matrix at all (imag -> the facet is omitted, never a false negative). Mirrors
    `obs_installs_under`'s walk-and-match shape. False for a missing/empty dir list (never guessed)."""
    for root in scan_dirs or []:
        if not root or not os.path.isdir(root):
            continue
        for _dirpath, _dirnames, filenames in os.walk(root):
            for name in filenames:
                if VB_MATRIX_EXE_RE.match(name):
                    return True
    return False


def vb_matrix_running_facet(install_present, proc):
    """#1227 — the 3-state `(running, name, pid)` facet composition from the disk-install gate + the
    `vb_matrix_process_from_listing` result *proc* (None | ("", "") | (name, pid)):

      install_present False        -> ("", "", "")     (no VB-Matrix box, e.g. imag: OMITTED
                                                        downstream -> UNKNOWN, never a page)
      proc is None                 -> ("", "", "")     (the tasklist read FAILED — UNKNOWN, NEVER a
                                                        false DOWN off a failed read; issue 1227 🔴)
      install_present, proc ("","")-> ("0", "", "")    (a good read, host genuinely absent -> present
                                                        in JSON as running="0" -> DOWN -> page)
      install_present, (name,pid)  -> ("1", name, pid) (RUNNING)

    `"0"` is a truthy string, so `build_bundle_state`'s omit-when-empty filter KEEPS it (DOWN must
    surface); only the not-installed / unread `""` is dropped."""
    if not install_present:
        return ("", "", "")
    if proc is None:
        return ("", "", "")
    proc_name, proc_pid = proc
    if proc_name:
        return ("1", proc_name, proc_pid or "")
    return ("0", "", "")


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
    # #1276: the volume's FREE space — the owner-ruled (14.9.2026) WARNING signal is "<= 50 GB of
    # FREE space left on the recordings volume", not the sum of recording files. Read via
    # shutil.disk_usage on the SAME local record dir already scanned above (no new transport;
    # works on Windows and imag-Linux). Degrades to None (UNKNOWN downstream — never a false
    # low-space WARN) on any read failure, mirroring the zero-degrade of the file scan above.
    free_bytes = None
    try:
        free_bytes = shutil.disk_usage(record_dir).free
    except OSError as e:
        print(
            f"WARNING: record_dir_stats: could not read free space of {record_dir!r}: {e}",
            file=sys.stderr,
        )
    return {
        "total_bytes": total_bytes,
        "file_count": file_count,
        "oldest_mtime": oldest_mtime,
        "free_bytes": free_bytes,
    }


def recordings_free_verdict(free_bytes, min_free_gb):
    """#1276 — the E2E recordings-retention free-space WARNING verdict, the python mirror of the
    canonical Rust ``recordings_retention::free_space_verdict``. Owner ruling (14.9.2026, verbatim
    "B varovanie ma byt ked 50gb uz len ostava miesta!!!"): warn when the recordings VOLUME has at
    most ``min_free_gb`` of FREE space left, NOT when the sum of recording files exceeds a budget.

    ``free_bytes`` is the volume's free space (from ``record_dir_stats``'s ``free_bytes``), or
    ``None`` when it could not be read. Returns "WARN" iff the free space is STRICTLY below
    ``min_free_gb`` (so exactly ``min_free_gb`` free is still "OK" — the spec's "free >= threshold
    -> no warn"), "UNKNOWN" for ``None`` (never a false low-space WARN from an unreadable stat),
    else "OK". Threshold + comparison in decimal GB (1e9 bytes), the same unit the existing warning
    and the owner's "50gb" meant."""
    if free_bytes is None:
        return "UNKNOWN"
    free_gb = free_bytes / 1e9
    return "WARN" if free_gb < min_free_gb else "OK"


def recordings_free_line(stats_json_text, min_free_gb):
    """issue 1367 -- the ONE "<VERDICT> <free_gb>" line a bash caller prints from a box's
    `/record-dir-stats.json` body: `recordings_free_verdict` on its `free_bytes`, the free space in
    decimal GB with one decimal, or `-1` when unknown. An empty / non-JSON / non-object body is
    "UNKNOWN -1" (never a false WARN), and so is a non-numeric / boolean free_bytes. Its callers:
    recording-e2e.sh through scripts/lib/recordings-free-line.sh (issue 1386) and
    scripts/lib/av-soak.sh."""
    try:
        d = json.loads(stats_json_text or "")
    except ValueError:
        return "UNKNOWN -1"
    if not isinstance(d, dict):
        return "UNKNOWN -1"
    fb = d.get("free_bytes")
    if not isinstance(fb, (int, float)) or isinstance(fb, bool):
        fb = None
    verdict = recordings_free_verdict(fb, float(min_free_gb))
    return f"{verdict} {'-1' if fb is None else '%.1f' % (fb / 1e9)}"


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


# --- issue 1406: the OBS process handle count (`obs_handles*` facet) -----------------------------
# On 5.10.2026 the stream obs64 held 4,066,772 handles after ~22 h: the Audio Monitor plugin kept
# opening an absent audio endpoint and leaked one registry key per audio tick (46.875/s), ~100 h
# from the 16,777,216 per-process cap. Nothing read a handle count, so the dev1 obs-handles
# watchdog now reads these keys: the count, the pid + start epoch, and on Linux the run token (the
# process identity; a new one, i.e. a restart, resets the baseline) and the soft open-files limit
# (the Linux cap). Absent when unreadable, never a 0.

# Field offsets of the x64 SYSTEM_PROCESS_INFORMATION record NtQuerySystemInformation returns
# (class 5). `next`, `threads`, the ImageName UNICODE_STRING, `pid` and `handles` are the public
# winternl.h layout; `create_time` sits inside its documented Reserved1 block, where NT has kept it
# since Windows 7 (WorkingSetPrivateSize 8, HardFaultCount 16, NumberOfThreadsHighWatermark 20,
# CycleTime 24, CreateTime 32). tests/python/test_obs_handles_gather_1406.py rebuilds the C layout
# with ctypes and pins these numbers to it.
SPI_OFFSETS = {
    "next": 0,
    "threads": 4,
    "create_time": 32,
    "name_length": 56,
    "name_buffer": 64,
    "pid": 80,
    "handles": 96,
}
_SPI_HEADER_BYTES = 104          # through SessionId: every field above lies inside it
_FILETIME_EPOCH_OFFSET_S = 11_644_473_600   # 1601-01-01 -> 1970-01-01


def filetime_to_epoch(filetime):
    """A Windows FILETIME (100 ns ticks since 1601 UTC) -> whole epoch seconds, or None for a
    zero/negative value (the idle process, an unset field)."""
    if not isinstance(filetime, int) or filetime <= 0:
        return None
    return filetime // 10_000_000 - _FILETIME_EPOCH_OFFSET_S


def _spi_entry(raw, off, base_addr):
    """One SYSTEM_PROCESS_INFORMATION record at byte offset *off* -> (next, process dict)."""
    o = SPI_OFFSETS
    (nxt,) = struct.unpack_from("<I", raw, off + o["next"])
    (threads,) = struct.unpack_from("<I", raw, off + o["threads"])
    (create,) = struct.unpack_from("<q", raw, off + o["create_time"])
    (name_len,) = struct.unpack_from("<H", raw, off + o["name_length"])
    (name_ptr,) = struct.unpack_from("<Q", raw, off + o["name_buffer"])
    (pid,) = struct.unpack_from("<Q", raw, off + o["pid"])
    (handles,) = struct.unpack_from("<I", raw, off + o["handles"])
    name = ""
    name_at = name_ptr - base_addr if name_ptr else -1
    if name_len and 0 <= name_at and name_at + name_len <= len(raw):
        name = raw[name_at:name_at + name_len].decode("utf-16-le", errors="replace")
    return nxt, {"pid": pid, "name": name, "handles": handles, "threads": threads,
                 "start": filetime_to_epoch(create)}


def system_processes_from_spi(raw, base_addr):
    """PURE: the x64 SystemProcessInformation buffer *raw* (whose image-name pointers are absolute,
    the buffer starting at *base_addr*) -> one dict per process: pid, name, handles, threads,
    start (epoch s or None). None when the buffer is short or malformed (an entry offset inside its
    own header, or an entry running past the end): unreadable, never a partial list that could miss
    the OBS process and read as "no OBS"."""
    raw = raw or b""
    procs, off = [], 0
    while True:
        if off + _SPI_HEADER_BYTES > len(raw):
            return None
        nxt, proc = _spi_entry(raw, off, base_addr)
        procs.append(proc)
        if nxt == 0:
            return procs
        if nxt < _SPI_HEADER_BYTES:
            return None
        off += nxt


def obs_handles_facet(procs):
    """PURE: the live OBS-shaped process (`OBS_PROCESS_NAME_RE` on the name minus `.exe`) with the
    most handles -> the `obs_handles*` keys as strings ({} when there is none). A process with no
    threads has exited (a zombie the system still lists) and never counts. `limit` (Linux only) is
    the soft open-files limit; `start` may be None (the watchdog then keys on the pid alone)."""
    best = None
    for p in procs or ():
        name = str(p.get("name") or "")
        base = name[:-4] if name.lower().endswith(".exe") else name
        if not OBS_PROCESS_NAME_RE.match(base) or p.get("threads", 1) == 0:
            continue
        if best is None or p["handles"] > best["handles"]:
            best = p
    if best is None:
        return {}
    start, limit = best.get("start"), best.get("limit")
    return {
        "obs_handles": str(best["handles"]),
        "obs_handles_pid": str(best["pid"]),
        "obs_handles_start": "" if start is None else str(start),
        "obs_handles_run": best.get("run") or "",
        "obs_handles_limit": "" if limit is None else str(limit),
    }


def proc_stat_start_ticks(stat_text):
    """PURE: field 22 (starttime, clock ticks since boot) of a /proc/<pid>/stat line, or None.
    The command name in field 2 may hold spaces and parentheses, so the fields after it are read
    from the LAST ')'. The same parse as `proc_start_ticks` in scripts/strih_browser_keeper.py
    (a separately deployed tree), pinned to it by tests/python/test_obs_handles_gather_1406.py."""
    text = stat_text if isinstance(stat_text, str) else ""
    cut = text.rfind(")")
    if cut < 0:
        return None
    fields = text[cut + 1:].split()   # fields[0] is field 3 (state)
    if len(fields) < 20 or not fields[19].isdigit():
        return None
    return int(fields[19])


def proc_btime(proc_stat_text):
    """PURE: the boot time (epoch s) from the `btime` line of /proc/stat, or None."""
    for line in (proc_stat_text or "").splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[0] == "btime" and parts[1].isdigit():
            return int(parts[1])
    return None


def proc_nofile_soft_limit(limits_text):
    """PURE: the soft `Max open files` limit from /proc/<pid>/limits, or None (absent, unreadable
    or `unlimited`)."""
    for line in (limits_text or "").splitlines():
        if line.startswith("Max open files"):
            soft = line[len("Max open files"):].split()[:1]
            return int(soft[0]) if soft and soft[0].isdigit() else None
    return None


def _read_text(path):
    """A small /proc file as text, or None when it cannot be read (the process exited)."""
    try:
        with open(path, "rb") as fh:
            return fh.read().decode("utf-8", errors="replace")
    except OSError:
        return None


def _linux_obs_process(proc_root, pid, boot, btime, clk_tck):
    """One /proc/<pid> -> a process dict when it is an OBS-shaped process whose fd directory this
    user can list, else None (not OBS, another user's process, or one that exited mid-scan)."""
    base = os.path.join(proc_root, pid)
    comm = (_read_text(os.path.join(base, "comm")) or "").strip()
    if not OBS_PROCESS_NAME_RE.match(comm):
        return None
    try:
        handles = len(os.listdir(os.path.join(base, "fd")))
    except OSError:
        return None
    ticks = proc_stat_start_ticks(_read_text(os.path.join(base, "stat")))
    start = btime + ticks // clk_tck if btime is not None and ticks is not None else None
    return {"pid": int(pid), "name": comm, "handles": handles, "start": start,
            "run": None if ticks is None else f"{boot}:{ticks}",
            "limit": proc_nofile_soft_limit(_read_text(os.path.join(base, "limits")))}


def linux_obs_handles(proc_root="/proc", clk_tck=None):
    """The Linux `obs_handles*` facet: the open-fd count of the OBS process (comm `obs`, the
    strih-obs.service binary), its start epoch (btime + /proc/<pid>/stat start ticks, context), its
    RUN token `<boot id>:<start ticks>` and its soft open-files limit. The run token is the process
    identity: btime moves whenever the wall clock is stepped (strih-lx is the dantesync date
    master), the boot id and the start ticks never do -- the same OBS-run identity
    scripts/strih_browser_keeper.py uses. The server runs as the same user as OBS (on strih-lx both
    are --user units of the desktop user), so the fd directory is listable. {} when no OBS process
    is readable."""
    try:
        pids = [e for e in os.listdir(proc_root) if e.isdigit()]
    except OSError:
        return {}
    if clk_tck is None:
        clk_tck = os.sysconf("SC_CLK_TCK")
    boot = (_read_text(os.path.join(proc_root, "sys", "kernel", "random", "boot_id")) or "").strip()
    btime = proc_btime(_read_text(os.path.join(proc_root, "stat")))
    procs = [p for p in (_linux_obs_process(proc_root, pid, boot, btime, clk_tck) for pid in pids)
             if p]
    return obs_handles_facet(procs)
