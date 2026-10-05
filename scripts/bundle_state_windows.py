#!/usr/bin/env python3
"""The Windows-only OBS-box identity readers of the :8899 bundle-state server, with their
process-lifetime caches: who owns TCP :4455 (a native netstat PID probe + a PID-keyed CIM resolve),
the ONE native tasklist read and its OBS-process / VB-Matrix consumers, the VB-Matrix start time
(a PID-keyed CIM read), the NL_STARTUP.ahk text, the Start-Menu shortcut + NDI runtime version
(file-stat-keyed PowerShell reads), and the OBS handle count (issue 1406: one ctypes
NtQuerySystemInformation process snapshot, uncached -- the count is the reading).

Issue 1386 slice D: moved verbatim out of scripts/bundle-state-server.py. The server keeps the
orchestration: its `_windows_*_facets` helpers call these readers through the SERVER's own module
globals, behind its one IS_WINDOWS gate, and it re-exports every name its tests reach (the caches
are the same dict objects, reset in place). Server-side, not a gather facet: the server imports it
directly, and it stays out of the pure `bundle_state_gather` facade because it spawns subprocesses
and holds caches. Stdlib only, so the provisioners' post-install `python3 -I` smoke import loads it.
Ships in the server tree declared in `scripts/lib/bundle-state-files.txt`.

Every reader degrades to "" on any failure, never a guessed value. The cache discipline (never
cache an empty resolve, always clear on a failure) is written up in
`.claude/rules/bundle-state-gather-latency.md`.
"""
from __future__ import annotations

import csv
import ctypes
import io
import os
import subprocess
import threading

import bundle_state_gather as bsg
from bundle_state_serverlog import log


# #1222 — port4455_owner()'s PID-keyed cache (see that function's own doc comment). Guarded by a
# lock because ThreadingHTTPServer dispatches each request on its own thread — same pattern as
# the server's _State class for the record-directory cache.
PORT4455_PID_PROBE_TIMEOUT_S = 5  # #1222b: netstat, no interpreter cold-start —
                                   # should never need anywhere near this long
PORT4455_FULL_RESOLVE_TIMEOUT_S = 15  # unchanged; now rare (only on an actual PID change)
_PORT4455_CACHE_LOCK = threading.Lock()
_port4455_cache = {"pid": None, "path": "", "version": ""}

# #1227 — vb_matrix_start_time()'s PID-keyed cache (same #1222 pattern as _port4455_cache above).
# The process PID (free from the native tasklist parse, read every request) is the cheap key; the
# expensive CIM CreationDate resolve is paid ONLY when that PID changes — VB-Matrix's PID is stable
# for days, so the PowerShell round-trip is essentially never on the hot path.
VB_MATRIX_START_RESOLVE_TIMEOUT_S = 15
_VB_MATRIX_START_CACHE_LOCK = threading.Lock()
_vb_matrix_start_cache = {"pid": None, "start": ""}


# #1222c — ndi_runtime_version()'s process-lifetime cache, keyed by the runtime DLL's own
# (mtime_ns, size). The NDI runtime version is static per install and only changes when the DLL
# itself is replaced (an NDI SDK upgrade) — live evidence: ~8.3s under OBS render load to read
# what is otherwise a static value on every single request.
_NDI_RUNTIME_CACHE_LOCK = threading.Lock()
_ndi_runtime_cache = {"path": None, "stat_key": None, "version": ""}


def ndi_runtime_version(dll_path):
    """Get-Item's VersionInfo.FileVersion, shelled to PowerShell (there is no stdlib way to read a
    Windows PE VERSIONINFO resource) — the exact one-liner drift-guard.md step 1 documents. ""
    on any failure (missing file, powershell error) — never a guessed value.

    #1222c: CACHED, keyed by *dll_path*'s own `(mtime_ns, size)` (see `_ndi_runtime_cache` above).
    A changed stat (an NDI SDK upgrade replacing the DLL) re-resolves and re-caches; a missing DLL
    is never cached (the existing `os.path.isfile` guard already short-circuits before any
    subprocess call at all). Same "never cache a failed or empty resolve" discipline as the #1222
    port4455 cache — a transient PowerShell hiccup must keep retrying, never freeze this facet
    blind for the rest of the process lifetime."""
    if not os.path.isfile(dll_path):
        log(f"WARNING: NDI runtime DLL not found at {dll_path}")
        return ""

    try:
        st = os.stat(dll_path)
        stat_key = (st.st_mtime_ns, st.st_size)
    except OSError:
        stat_key = None

    if stat_key is not None:
        with _NDI_RUNTIME_CACHE_LOCK:
            if _ndi_runtime_cache["path"] == dll_path and _ndi_runtime_cache["stat_key"] == stat_key:
                return _ndi_runtime_cache["version"]

    try:
        out = subprocess.run(
            [
                "powershell", "-NoProfile", "-NonInteractive", "-Command",
                f"(Get-Item -LiteralPath '{dll_path}').VersionInfo.FileVersion",
            ],
            capture_output=True, text=True, timeout=15, check=True,
        )
        version = out.stdout.strip()
    except (subprocess.SubprocessError, OSError) as e:
        log(f"WARNING: could not read NDI runtime version: {e}")
        return ""

    if version and stat_key is not None:
        with _NDI_RUNTIME_CACHE_LOCK:
            _ndi_runtime_cache["path"] = dll_path
            _ndi_runtime_cache["stat_key"] = stat_key
            _ndi_runtime_cache["version"] = version
    return version


def _parse_netstat_listening_pid(text, port=4455):
    """#1222b — parse `netstat -ano -p tcp` output *text* and return the PID (as a string) of the
    FIRST row whose local address ends with `:<port>` and whose state is LISTENING. "" if no such
    row exists, or *text* is empty/malformed (never a guessed value — same never-a-false-clean
    discipline as every other facet in this file).

    Defensive parsing (PURE — no subprocess, no live box needed, testable with a canned fixture):
    every genuine TCP row has exactly 5 whitespace-separated columns (Proto, Local Address,
    Foreign Address, State, PID); the "Active Connections" banner and the column-header row are
    naturally skipped because they do not have exactly 5 columns (7 tokens for the header row,
    fewer for the banner), and a UDP row (which itself has only 4 columns — no State — since the
    caller intentionally passes NO `-p` filter, see `_port4455_owning_pid`'s own doc comment on
    why) is also skipped defensively by both the column-count check and the explicit proto check.
    The `:<port>` check is an exact suffix match on the LOCAL address column only — a `:4455`
    mention in the FOREIGN address column of an unrelated ESTABLISHED connection, or a longer port
    like `:44551`, can never match. Works unchanged for an IPv6 row (`[::]:4455` still ends with
    the plain `:4455` suffix)."""
    suffix = f":{port}"
    for line in (text or "").splitlines():
        cols = line.split()
        if len(cols) != 5:
            continue
        proto, local_addr, _foreign_addr, state, pid = cols
        if proto.upper() != "TCP":
            continue
        if state.upper() != "LISTENING":
            continue
        if not local_addr.endswith(suffix):
            continue
        return pid
    return ""


def _port4455_owning_pid():
    """#1222 / #1222b — a CHEAP round-trip that reads ONLY the PID of whatever process is
    LISTENING on TCP :4455 right now — no WMI/CIM query, no VersionInfo read. Live strih evidence
    showed the FULL port4455_owner() resolution below (which folds this same listener lookup
    together with a Get-CimInstance Win32_Process query) regularly hitting its 15s subprocess
    timeout on EVERY /bundle-state.json request; this cheap probe lets port4455_owner() skip that
    expensive WMI round-trip entirely whenever the owning PID has not changed since last time.

    #1222b: this probe was FIRST implemented as its own PowerShell one-liner
    (`Get-NetTCPConnection`), but a live post-deploy timing on strih showed that command alone
    costing ~4.1s plus PowerShell's own interpreter cold-start (~5-10s under load) — the "cheap"
    probe still cost ~10-15s per request there, defeating its own purpose (the cache in
    port4455_owner() never got a chance to help). Replaced with `netstat -ano -p tcp` — a native
    Windows tool with no interpreter startup cost — parsed by the PURE `_parse_netstat_listening_pid`
    above. Same signature, same "" on-failure/no-listener contract, so port4455_owner()'s cache
    logic (unchanged by this swap) never needed to know which probe implementation feeds it.

    Returns the numeric PID as a string, or "" if there is no listener / the probe itself fails
    (never a guessed value — the caller then must not trust any cached identity either)."""
    try:
        # #1222b review finding: Windows `-p tcp` and `-p tcpv6` are DISTINCT address-family
        # filters -- `-p tcp` silently returns IPv4 rows ONLY, even though both families display
        # literally "TCP" in the Proto column when unfiltered. Passing it here would make this
        # probe permanently blind to a :4455 listener bound on IPv6 (a silent regression to ""
        # forever, not just a performance issue) -- so no `-p` filter is passed at all; the pure
        # parser's own proto/state check already does the real filtering correctly for BOTH
        # families (and skips UDP rows, which have only 4 columns with no filter applied).
        out = subprocess.run(
            ["netstat", "-ano"],
            capture_output=True, text=True, timeout=PORT4455_PID_PROBE_TIMEOUT_S, check=True,
        )
        return _parse_netstat_listening_pid(out.stdout, port=4455)
    except (subprocess.SubprocessError, OSError) as e:
        log(f"WARNING: could not read the :4455 listener PID: {e}")
        return ""


def port4455_owner():
    """#826 — the exe PATH (never just a process name) + FileVersion of whatever process is
    LISTENING on TCP :4455 right now. Returns (path, version), each "" on any failure/absence (no
    listener, PowerShell error, process vanished between calls) — never a guessed value. Matching by
    PATH (never just the process name) is the exact hole the 2026-07-27 incident exposed: a
    same-NAMED `obs64.exe` process can be a totally different, stale install.

    #1067 — resolve the path via `Get-CimInstance Win32_Process`.ExecutablePath (the WMI/CIM
    provider), NOT (only) `Get-Process -Id <pid>`.Path. The deployed BundleStateServer scheduled
    task runs NON-elevated + hidden, and Get-Process.Path must OPEN the target process to read its
    main-module path -> access-denied on the ELEVATED obs64 -> `.Path` is null -> BOTH keys were
    OMITTED on the whole live fleet (2026-08-15), forcing port4455_identity to stay opt-in in
    version-integrity-gate.sh. Win32_Process.ExecutablePath is readable for an elevated process from
    a non-elevated caller where the OpenProcess-based Get-Process.Path is not; the version read
    (Get-Item .VersionInfo.FileVersion) only needs read access to the on-disk exe, so it works once
    the path resolves (which is why the version was ALSO missing before — downstream of the null
    path, not a separate failure). Get-Process.Path is kept as a fallback for any box where CIM is
    unavailable.

    #1222 — CACHED, keyed by the CURRENT owning PID (read via the cheap `_port4455_owning_pid()`
    probe above, no WMI). Live strih evidence: this function's single PowerShell round-trip
    (Get-NetTCPConnection + Get-CimInstance Win32_Process + Get-Item VersionInfo) was regularly
    hitting its 15s subprocess timeout on EVERY /bundle-state.json request — ~15s of the
    ~18.7s fresh-log gather baseline (issue-1222 comment). Since a PID never changes identity
    mid-life on Windows, an UNCHANGED pid means the same process is still there and the
    already-resolved (path, version) is not a guess — it is re-served instead of re-resolved. Only
    a CHANGED pid (a genuine OBS restart, or a different process taking the port — rare, not a
    per-request event) pays for the expensive WMI resolution again. An unresolvable current PID
    (no listener, or even the cheap probe failing) CLEARS the cache and returns ("", "") — never
    serves a stale identity for a port nothing currently proves to still be owned by that process."""
    pid = _port4455_owning_pid()
    if not pid:
        with _PORT4455_CACHE_LOCK:
            _port4455_cache["pid"] = None
            _port4455_cache["path"] = ""
            _port4455_cache["version"] = ""
        return "", ""

    with _PORT4455_CACHE_LOCK:
        if _port4455_cache["pid"] == pid:
            return _port4455_cache["path"], _port4455_cache["version"]

    try:
        out = subprocess.run(
            [
                "powershell", "-NoProfile", "-NonInteractive", "-Command",
                # Single-quoted Python literals so the PowerShell double-quoted WQL filter embeds
                # cleanly; $path = $null (not '') avoids a PS single quote colliding with Python's.
                '$c = Get-NetTCPConnection -LocalPort 4455 -State Listen '
                '-ErrorAction SilentlyContinue | Select-Object -First 1; '
                'if ($c) { $procId = $c.OwningProcess; $path = $null; '
                '$cim = Get-CimInstance Win32_Process -Filter "ProcessId=$procId" '
                '-ErrorAction SilentlyContinue; '
                'if ($cim -and $cim.ExecutablePath) { $path = $cim.ExecutablePath } '
                'if (-not $path) { $gp = Get-Process -Id $procId -ErrorAction SilentlyContinue; '
                'if ($gp -and $gp.Path) { $path = $gp.Path } } '
                'if ($path) { $path; '
                '(Get-Item -LiteralPath $path -ErrorAction SilentlyContinue).VersionInfo.FileVersion } }',
            ],
            capture_output=True, text=True, timeout=PORT4455_FULL_RESOLVE_TIMEOUT_S, check=True,
        )
        lines = [ln for ln in out.stdout.splitlines() if ln.strip()]
        path = lines[0].strip() if len(lines) >= 1 else ""
        version = lines[1].strip() if len(lines) >= 2 else ""
    except (subprocess.SubprocessError, OSError) as e:
        log(f"WARNING: could not read the :4455 port owner: {e}")
        # #1222 review: a failed resolve must CLEAR the cache, not leave a previous entry
        # standing -- a later PID reuse (Windows recycles PIDs) must never serve an identity
        # resolved before this failure under a pid that may since belong to a different process.
        with _PORT4455_CACHE_LOCK:
            _port4455_cache["pid"] = None
            _port4455_cache["path"] = ""
            _port4455_cache["version"] = ""
        return "", ""

    if path:
        # #1222 review: only cache a NON-EMPTY path. A resolve that succeeds (exit 0) but returns
        # nothing (the #1067 access-denied shape, or a transient CIM flake) must NOT be cached --
        # caching it would serve ("", "") for the rest of the OBS session with no chance to
        # recover, whereas the pre-fix uncached code retried on every single request.
        with _PORT4455_CACHE_LOCK:
            _port4455_cache["pid"] = pid
            _port4455_cache["path"] = path
            _port4455_cache["version"] = version
    return path, version


def _parse_tasklist_obs_process_names(text):
    """#1222c — parse `tasklist /FO CSV /NH` output *text* and return a newline-joined list of
    every OBS-shaped process NAME (matches `obs<digits>` case-insensitively via the shared
    `bsg.OBS_PROCESS_NAME_RE` — #1222c review: this used to carry its own duplicate copy of that
    regex, a DRY violation the shared constant now closes; `.exe` suffix stripped) — the EXACT
    same shape `Get-Process -Name 'obs*' | Select-Object -ExpandProperty Name` used to produce, so
    `bsg.obs_process_count_from_listing` (UNCHANGED by this ticket) keeps working on it verbatim.
    Each CSV row is `"Image Name","PID","Session Name","Session#","Mem Usage"` (tasklist's own
    quoted-CSV format); `/NH` already suppresses the header row, but this parser tolerates one
    anyway (it simply never matches the obs<digits> pattern). #1295: a matching row whose Mem Usage
    proves it is a DEAD/zombie handle (`bsg.tasklist_row_is_live_obs` False — tasklist has no
    HasExited column, so Mem Usage is the liveness proxy) is EXCLUDED, so a stale ~0-KB obs handle
    never inflates the downstream "exactly one obs64" count (#1296).

    "" if *text* is empty/malformed (never a guessed/zero count downstream — the same
    never-a-false-clean discipline every other facet in this file follows)."""
    if not (text or "").strip():
        return ""
    names = []
    try:
        for row in csv.reader(io.StringIO(text)):
            if not row:
                continue
            image_name = row[0]
            base = image_name[:-4] if image_name.lower().endswith(".exe") else image_name
            if bsg.OBS_PROCESS_NAME_RE.match(base):
                # #1295: exclude a DEAD/mid-exit zombie obs row by its Mem Usage (tasklist has no
                # HasExited column) — a ~0-KB handle must not inflate the "exactly one obs64" count
                # health signal (#1296). Mem Usage is row[4]; a missing column reads NOT-live.
                mem_field = row[4] if len(row) > 4 else ""
                if not bsg.tasklist_row_is_live_obs(mem_field):
                    continue
                names.append(base)
    except csv.Error as e:
        log(f"WARNING: could not parse tasklist CSV output: {e}")
        return ""
    return "\n".join(names)


def tasklist_csv():
    """#1222c / #1227 — the RAW `tasklist /FO CSV /NH` output (a native subprocess, no PowerShell
    interpreter cold-start — the #1222 tax the obs_process swap eliminated). ONE call feeds BOTH the
    obs process-count facet (via `_parse_tasklist_obs_process_names`) AND the VB-Matrix presence
    facet (via `bsg.vb_matrix_process_from_listing`, which needs the raw PID column) — gather it once
    per request (the server's `_windows_process_facets`) and pass the text to both, never two
    native tasklist spawns (issue 1227 review 🟡). "" on any failure — a live box always lists SOME
    processes, so an empty result means the subprocess failed (both consumers treat "" as UNKNOWN,
    never a guessed count)."""
    try:
        out = subprocess.run(
            ["tasklist", "/FO", "CSV", "/NH"],
            capture_output=True, text=True, timeout=15, check=True,
        )
        return out.stdout
    except (subprocess.SubprocessError, OSError) as e:
        log(f"WARNING: could not list processes (tasklist): {e}")
        return ""


def obs_process_list():
    """#826 / #1222c — every running process NAME matching an OBS-shaped filter, newline-joined —
    feeds bsg.obs_process_count_from_listing. "" on any failure (never a guessed count; the gate
    then reads this box's process count as UNKNOWN, not "zero confirmed"). Uses the shared native
    `tasklist_csv()` (issue 1227 review 🟡 folds the once-separate VB-Matrix tasklist into it);
    behaviour is unchanged — still a native `tasklist` parsed by `_parse_tasklist_obs_process_names`,
    so `bsg.obs_process_count_from_listing` and the pinned tests need zero changes."""
    return _parse_tasklist_obs_process_names(tasklist_csv())


def vb_matrix_process_list():
    """#1227 — the RAW `tasklist /FO CSV /NH` output, for `bsg.vb_matrix_process_from_listing` (which
    needs the raw PID column). Thin alias over the shared `tasklist_csv()` so a single native tasklist
    per request feeds both this and the obs process-count facet (review 🟡)."""
    return tasklist_csv()


def gather_vb_matrix_facet(install_present, tasklist_text, start_fn):
    """#1227 — compose the `(running, name, pid, start)` VB-Matrix facet from the disk-install gate,
    the shared tasklist output, and a start-time resolver. Module-level (not a gather closure) so the
    "a failed tasklist read must NOT become a false DOWN" path is directly testable.

    `start_fn(pid)` is called UNCONDITIONALLY (review 🔵): a falsy pid clears the PID-keyed cache and
    returns "" WITHOUT spawning PowerShell, so imag / a DOWN box still never pays a CIM query, while a
    DOWN->UP restart that reuses the same Windows pid no longer serves a stale cached start time."""
    running, name, pid = bsg.vb_matrix_running_facet(
        install_present, bsg.vb_matrix_process_from_listing(tasklist_text)
    )
    return running, name, pid, start_fn(pid)


def vb_matrix_start_time(pid):
    """#1227 — the VB-Matrix process's start time as a locale-stable `yyyy-MM-ddTHH:mm:ss` string,
    resolved from the given *pid* via `Get-CimInstance Win32_Process`.CreationDate (CONTEXT for the
    alert body — the core running/pid facet never depends on it). "" for a falsy pid or any failure.

    #1222 PID-keyed cache: the pid (free from the tasklist parse) is the cheap key; the CIM resolve
    is paid ONLY when the pid changes. `wmic` (the native CreationDate source) is REMOVED on the
    stream box (Win11 24H2+, verified live 2026-09-02), so CIM is the only path — but it carries the
    PowerShell interpreter cold-start the latency rule fights, hence the cache. CIM CreationDate is
    readable non-elevated (the #1067 ExecutablePath precedent). Whether it reads in the deployed
    non-elevated task context is a LIVE-Windows property the supervisor verifies post-deploy; a ""
    degrade there leaves the running/pid facet fully intact.

    Same never-cache-empty / clear-on-failure discipline as port4455_owner() (#1222 review)."""
    # A falsy OR non-numeric pid clears the cache and returns "" with NO subprocess (review 🔵: a
    # non-numeric pid would otherwise resolve to "" every request -> a PowerShell cold start each
    # time; the pid comes from tasklist so it is normally numeric, this is cheap insurance).
    if not pid or not str(pid).isdigit():
        with _VB_MATRIX_START_CACHE_LOCK:
            _vb_matrix_start_cache["pid"] = None
            _vb_matrix_start_cache["start"] = ""
        return ""

    with _VB_MATRIX_START_CACHE_LOCK:
        if _vb_matrix_start_cache["pid"] == pid:
            return _vb_matrix_start_cache["start"]

    try:
        out = subprocess.run(
            [
                "powershell", "-NoProfile", "-NonInteractive", "-Command",
                # Single-quoted Python literals so the PowerShell double-quoted WQL filter embeds
                # cleanly; `.ToString("s")` is the .NET SORTABLE (ISO-8601, invariant-culture) format
                # = exactly `yyyy-MM-ddTHH:mm:ss`, and unlike a custom `HH:mm:ss` pattern its `:` is
                # NOT the locale time separator (review 🔵), so it is fully locale-stable. `pid` is
                # numeric-guarded above before it reaches this WQL filter.
                f'$p = Get-CimInstance Win32_Process -Filter "ProcessId={pid}" '
                '-ErrorAction SilentlyContinue | Select-Object -First 1; '
                'if ($p -and $p.CreationDate) { $p.CreationDate.ToString("s") }',
            ],
            capture_output=True, text=True, timeout=VB_MATRIX_START_RESOLVE_TIMEOUT_S, check=True,
        )
        start = out.stdout.strip()
    except (subprocess.SubprocessError, OSError) as e:
        log(f"WARNING: could not read the VB-Matrix start time (pid {pid}): {e}")
        # #1222 review: a failed resolve CLEARS the cache (never leaves a prior pid's start standing
        # under a pid that may since belong to a different process on a Windows PID recycle).
        with _VB_MATRIX_START_CACHE_LOCK:
            _vb_matrix_start_cache["pid"] = None
            _vb_matrix_start_cache["start"] = ""
        return ""

    if start:
        # #1222 review: only cache a NON-EMPTY resolve — an access-denied/flaky "" must keep retrying
        # next request, not freeze the facet blank for the rest of the process lifetime.
        with _VB_MATRIX_START_CACHE_LOCK:
            _vb_matrix_start_cache["pid"] = pid
            _vb_matrix_start_cache["start"] = start
    return start


def read_ahk_text(ahk_path):
    """#826 — the raw text of NL_STARTUP.ahk, a plain local file (no PowerShell needed — this
    process already runs ON the box). "" if the file is absent (stream, which runs no
    NL_STARTUP.ahk at all — the correct, non-failure UNKNOWN) or unreadable."""
    try:
        with open(ahk_path, "r", encoding="utf-8", errors="replace") as f:
            return f.read()
    except OSError as e:
        log(f"INFO: no NL_STARTUP.ahk at {ahk_path} ({e}) — this box likely runs none")
        return ""


# #1222c — resolve_shortcut()'s process-lifetime cache, keyed by the target .lnk file's own
# (mtime_ns, size). A Start-Menu shortcut is static until an operator re-points it, so paying for
# a fresh COM/PowerShell round-trip on EVERY request (live evidence: ~6.6s under OBS render load)
# is wasted once the file has not changed. Guarded by a lock for the same ThreadingHTTPServer
# per-request-thread reason as every other cache in this file.
_SHORTCUT_CACHE_LOCK = threading.Lock()
_shortcut_cache = {"path": None, "stat_key": None, "target": "", "workdir": ""}


def resolve_shortcut(lnk_path):
    """#826 — a Windows .lnk shortcut's own TargetPath + WorkingDirectory, via the same
    WScript.Shell COM technique scripts/launch-obs-genlock.sh already uses to launch OBS through
    its Start-Menu shortcut. Returns (target, workdir), each "" on any failure (missing shortcut,
    powershell error) — never a guessed value.

    #1222c: CACHED, keyed by *lnk_path*'s own `(mtime_ns, size)` (see `_shortcut_cache` above). A
    changed stat (the operator re-points the shortcut, or replaces the target file) re-resolves
    and re-caches; a file that cannot be stat'd (missing, unreadable) is never cached at all, so it
    keeps retrying every request rather than freezing on a stale/empty result. Same "never cache a
    failed or empty resolve" discipline as the #1222 port4455 cache — a transient PowerShell hiccup
    must keep retrying next request, not freeze this facet blind for the rest of the process
    lifetime."""
    try:
        st = os.stat(lnk_path)
        stat_key = (st.st_mtime_ns, st.st_size)
    except OSError:
        stat_key = None

    if stat_key is not None:
        with _SHORTCUT_CACHE_LOCK:
            if _shortcut_cache["path"] == lnk_path and _shortcut_cache["stat_key"] == stat_key:
                return _shortcut_cache["target"], _shortcut_cache["workdir"]

    try:
        out = subprocess.run(
            [
                "powershell", "-NoProfile", "-NonInteractive", "-Command",
                f"$s = New-Object -ComObject WScript.Shell; "
                f"$l = $s.CreateShortcut('{lnk_path}'); $l.TargetPath; $l.WorkingDirectory",
            ],
            capture_output=True, text=True, timeout=15, check=True,
        )
        lines = out.stdout.splitlines()
        target = lines[0].strip() if len(lines) >= 1 else ""
        workdir = lines[1].strip() if len(lines) >= 2 else ""
    except (subprocess.SubprocessError, OSError) as e:
        log(f"WARNING: could not resolve shortcut {lnk_path!r}: {e}")
        return "", ""

    if target and stat_key is not None:
        with _SHORTCUT_CACHE_LOCK:
            _shortcut_cache["path"] = lnk_path
            _shortcut_cache["stat_key"] = stat_key
            _shortcut_cache["target"] = target
            _shortcut_cache["workdir"] = workdir
    return target, workdir


# Issue 1406 -- the OBS handle count. ONE NtQuerySystemInformation(SystemProcessInformation) call
# returns every process's HandleCount, pid and create time WITHOUT opening any process and without a
# pid lookup first. GetProcessHandleCount would need an open of the elevated obs64 from the
# non-elevated BundleStateServer task: its PROCESS_QUERY_LIMITED_INFORMATION open is normally
# granted, but that is not verified live, and issue 1067 showed an open of obs64 denied in this
# task's context (Get-Process .Path). A CIM read costs a PowerShell cold start per request while the
# count cannot be cached. No subprocess, a few ms.
_SYSTEM_PROCESS_INFORMATION = 5
_STATUS_INFO_LENGTH_MISMATCH = 0xC0000004
_SPI_FIRST_BYTES = 1 << 20      # a rig box lists a few hundred processes (~0.5 MB)
_SPI_MAX_BYTES = 64 << 20


def system_process_information():
    """The raw SystemProcessInformation snapshot as `(bytes, base address)` -- the image-name
    pointers inside are absolute, so the parser needs the address the buffer lived at. None (with a
    WARNING) when ntdll is unavailable (any non-Windows host), the interpreter is not 64-bit (the
    parser reads the x64 layout), or the call fails. Grows the buffer while the kernel answers
    STATUS_INFO_LENGTH_MISMATCH (a process list that grew between the size guess and the call)."""
    try:
        if ctypes.sizeof(ctypes.c_void_p) != 8:
            log("WARNING: obs_handles needs a 64-bit Python (the x64 SystemProcessInformation layout)")
            return None
        query = ctypes.WinDLL("ntdll").NtQuerySystemInformation
    except (AttributeError, OSError) as e:
        log(f"WARNING: could not load NtQuerySystemInformation for obs_handles: {e}")
        return None
    query.restype = ctypes.c_uint32
    query.argtypes = [ctypes.c_uint32, ctypes.c_void_p, ctypes.c_uint32,
                      ctypes.POINTER(ctypes.c_uint32)]
    size = _SPI_FIRST_BYTES
    while size <= _SPI_MAX_BYTES:
        buf = ctypes.create_string_buffer(size)
        needed = ctypes.c_uint32(0)
        status = query(_SYSTEM_PROCESS_INFORMATION, buf, size, ctypes.byref(needed))
        if status == 0:
            return buf.raw, ctypes.addressof(buf)
        if status != _STATUS_INFO_LENGTH_MISMATCH:
            log(f"WARNING: NtQuerySystemInformation(SystemProcessInformation) failed: 0x{status:08X}")
            return None
        size = max(size * 2, needed.value + (64 << 10))
    log(f"WARNING: the SystemProcessInformation snapshot exceeds {_SPI_MAX_BYTES} bytes")
    return None


def windows_obs_handles():
    """The Windows `obs_handles*` facet (bsg.obs_handles_facet over the process snapshot): the
    obs64 handle count, pid and start epoch. {} when the snapshot cannot be taken or parsed, or no
    live OBS process exists -- the facet is then omitted (UNKNOWN downstream), never a 0."""
    snap = system_process_information()
    if snap is None:
        return {}
    procs = bsg.system_processes_from_spi(*snap)
    if procs is None:
        log("WARNING: could not parse the SystemProcessInformation snapshot; obs_handles omitted")
        return {}
    return bsg.obs_handles_facet(procs)
