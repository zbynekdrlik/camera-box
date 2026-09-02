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
import csv
import glob
import io
import json
import os
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import unquote

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import bundle_state_gather as bsg  # noqa: E402

try:
    from obs_phase2 import _conn, _rpc  # noqa: E402
except ImportError:
    sys.exit("missing dep: pip install websocket-client (obs_phase2.py needs it)")

DEFAULT_PORT = 8899
DEFAULT_OBS_HOST = "127.0.0.1"
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

# #1222 — port4455_owner()'s PID-keyed cache (see that function's own doc comment). Guarded by a
# lock because ThreadingHTTPServer dispatches each request on its own thread — same pattern as
# the _State class below for the record-directory cache.
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


def log(msg):
    # A hidden Scheduled-Task context can hand this process a DEAD stdout pipe (the #650 supervisor's
    # `python | Out-File` reader dying, or a console-less Start-Process without -RedirectStandardOutput):
    # print(flush=True) then raises OSError [Errno 22] INSIDE the request handler, killing every
    # request before it serves ("connection closed unexpectedly" with zero log lines -- live stream-box
    # incident 2026-08-15). Logging must never take the server down: swallow a broken-stdout write and
    # keep serving. The swallow is intentional and cannot itself log (stdout is what is broken). (#829)
    # airuleset:script-ok the dead-stdout OSError is exactly what must be swallowed; logging it is impossible (stdout is the broken resource)
    try:
        print(f"{time.strftime('%Y-%m-%d %H:%M:%S')} {msg}", flush=True)
    except OSError:
        pass


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
    anyway (it simply never matches the obs<digits> pattern).

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
    per request in `gather_bundle_state` and pass the text to both, never two native tasklist spawns
    (issue 1227 review 🟡). "" on any failure — a live box always lists SOME processes, so an empty
    result means the subprocess failed (both consumers treat "" as UNKNOWN, never a guessed count)."""
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
    read already fixes) instead of guessing which facet is slow."""
    timings = {}
    t_total0 = time.perf_counter()

    log_text = _timed(timings, "obs_log_read", newest_obs_log_text, obs_log_dir)

    # #1265 — the reference source whose ts_lag BAND is watched (mbc on stream; env-overridable so a
    # future box with a different A/V reference can set it). A box that has no such source (strih)
    # simply reports the band facets empty -> omitted, never a false band.
    ref_band_src = os.environ.get("AUDIO_REF_BAND_SRC", bsg.AUDIO_REF_BAND_DEFAULT_SRC)

    def _parse_log_facets():
        return (
            bsg.obs_version_from_log(log_text),
            bsg.distroav_version_from_log(log_text),
            bsg.output_fps_from_log(log_text),
            bsg.genlock_wall_clock_from_log(log_text),
            bsg.genlock_capability_from_log(log_text),
            # #1226/#1231 — MAX per-source audio-timeline lag + freshness age
            # (max_fresh_lag_str, src, age_s) from the SAME bounded log_text (no second read); the
            # dev1 audio-lag watchdog reads these facets.
            bsg.audio_telemetry_from_log(log_text),
            # #1265 — the per-REFERENCE-source ts_lag BAND SHAPE (src, base, high, low, duty, n) from
            # the SAME bounded log_text (still ONE obs_log_parse timing, no second read).
            bsg.audio_ref_band_from_log(log_text, ref_src=ref_band_src),
            # #1267 — the av-sync dock measured-offset trend (recent/base median + pin + pin_stable +
            # age + per-window counts) from the SAME bounded log_text (no second read); the dev1
            # upstream-step watchdog reads these facets.
            bsg.av_offset_series_from_log(log_text),
        )

    (obs_version, distroav_version, output_fps, genlock_wall_clock, genlock_capability,
     audio_ts_lag, audio_ref_band, av_offset) = _timed(timings, "obs_log_parse", _parse_log_facets)
    audio_ts_lag_ms_val, audio_ts_lag_src_val, audio_ts_lag_age_s_val = audio_ts_lag
    (audio_ref_lag_src_val, audio_ref_lag_base_ms_val, audio_ref_lag_high_ms_val,
     audio_ref_lag_low_ms_val, audio_ref_lag_duty_pct_val, audio_ref_lag_n_val) = audio_ref_band
    (av_offset_recent_med_val, av_offset_base_med_val, av_offset_pin_val, av_offset_pin_stable_val,
     av_offset_age_s_val, av_offset_n_recent_val, av_offset_n_base_val) = av_offset

    def _gather_ndi():
        try:
            return gather_ndi_inputs(obs_host, password)
        except Exception as e:  # noqa: BLE001 - any WS/RPC failure must not crash the whole response
            log(f"WARNING: could not gather NDI input latency over obs-websocket: {e}")
            return {}

    ndi_inputs = _timed(timings, "ndi_inputs", _gather_ndi)

    # #826 — the strih OBS-identity machine-check facet (each gather independent, same
    # never-let-one-failure-blank-the-rest discipline as every other facet here).
    port_owner_path, port_owner_version = _timed(timings, "port4455_owner", port4455_owner)
    ahk_text = _timed(timings, "ahk_text", read_ahk_text, ahk_path)
    shortcut_target, shortcut_workdir = _timed(
        timings, "shortcut", resolve_shortcut, startup_shortcut
    )

    # #770 — the DEPLOYED plugin/core byte identity the [0/8] version-integrity gate compares
    # against the #120 BUNDLE_MANIFEST. distroav.dll: hash the FIRST located copy (scan order:
    # Program Files, then ProgramData, then %APPDATA% — the primary genlock plugin), so a single
    # observed distroav_dll_sha256 pairs with the manifest's by-basename distroav.dll sha. A
    # shadowing duplicate is a SEPARATE #124 concern (distroav_dll_paths reports the whole set).
    # #1115: under Option A the deploy (deploy-genlock-fleet.sh FULL) ships the canonical
    # genlock distroav.dll TO this ProgramData load path, so the FIRST-located copy IS the
    # deployed canonical build (Program Files stays /XF-excluded => no shadow ahead of it) —
    # hashing it here is exactly the byte the version-integrity gate compares by basename.
    # Each hash degrades to "" (UNKNOWN downstream, never a guessed/zero SHA) when the file is
    # missing/unreadable — the opt-in landing (#756-shape): a box with no genlock DLL is skipped.
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
    genlock_build_sha_val = _timed(
        timings, "genlock_build_sha", bsg.genlock_build_sha_from_file, genlock_build_sha_file
    )
    ndi_runtime_val = _timed(timings, "ndi_runtime", ndi_runtime_version, ndi_runtime_dll)
    # #1227 review 🟡 — ONE native tasklist per request feeds BOTH the obs process-count facet and
    # the VB-Matrix presence facet (never two spawns).
    tasklist_text = _timed(timings, "tasklist", tasklist_csv)
    obs_process_count_val = _timed(
        timings,
        "obs_process_count",
        lambda: bsg.obs_process_count_from_listing(_parse_tasklist_obs_process_names(tasklist_text)),
    )

    # #1227 — the VB-Matrix presence facet (the dev1 VB-Matrix alert watchdog reads it). The disk
    # install gate + the shared tasklist parse compose the 3-state running facet; the start time is a
    # best-effort PID-keyed-cached CIM read gathered via start_fn (a falsy pid = no subprocess, so
    # imag / a DOWN box never pays a CIM query). A FAILED tasklist read is UNKNOWN, never a false DOWN.
    (vb_matrix_running_val, vb_matrix_name_val, vb_matrix_pid_val, vb_matrix_start_val) = _timed(
        timings, "vb_matrix",
        lambda: gather_vb_matrix_facet(
            bsg.vb_matrix_install_present_under(DEFAULT_VB_MATRIX_INSTALL_DIRS),
            tasklist_text, vb_matrix_start_time,
        ),
    )

    result = bsg.build_bundle_state(
        obs_version=obs_version,
        distroav_version=distroav_version,
        ndi_runtime=ndi_runtime_val,
        output_fps=output_fps,
        genlock_wall_clock=genlock_wall_clock,
        ndi_input_latency=bsg.ndi_input_latency_csv(ndi_inputs),
        distroav_dll_paths=distroav_paths_csv,
        genlock_capability=genlock_capability,
        # #770 — deployed core/plugin byte sha256 (the truth the marker only POINTS at).
        obs_dll_sha256=obs_dll_sha256_val,
        distroav_dll_sha256=distroav_dll_sha256_val,
        # #756 — the deployed genlock build SHA for the cross-box parity gate.
        genlock_build_sha=genlock_build_sha_val,
        # #826 — the strih OBS-identity machine-check facet.
        obs_installs=obs_installs_val,
        port4455_owner_path=port_owner_path,
        port4455_owner_version=port_owner_version,
        obs_process_count=obs_process_count_val,
        ahk_app1_shortcut_path=bsg.ahk_app1_shortcut_path(ahk_text),
        ahk_app1_run=bsg.ahk_app1_run(ahk_text),
        ahk_dead_config_present=bsg.ahk_dead_config_present(ahk_text),
        shortcut_target_path=shortcut_target,
        shortcut_workdir=shortcut_workdir,
        # #1226 — the audio-timeline-lag facet (omit-when-empty; absent == UNKNOWN downstream).
        audio_ts_lag_ms=audio_ts_lag_ms_val,
        audio_ts_lag_src=audio_ts_lag_src_val,
        # #1231 — the freshness age of that facet (in-log seconds behind the log head); a large value
        # -> the dev1 decision surfaces STALE (telemetry stopped while the log advanced).
        audio_ts_lag_age_s=audio_ts_lag_age_s_val,
        # #1265 — the per-REFERENCE-source ts_lag BAND SHAPE (omit-when-empty; a box with no such
        # source reports them empty). The dev1 audio-lag watchdog's BAND arm + recording-e2e.sh's
        # #856 apply-guard read these.
        audio_ref_lag_src=audio_ref_lag_src_val,
        audio_ref_lag_base_ms=audio_ref_lag_base_ms_val,
        audio_ref_lag_high_ms=audio_ref_lag_high_ms_val,
        audio_ref_lag_low_ms=audio_ref_lag_low_ms_val,
        audio_ref_lag_duty_pct=audio_ref_lag_duty_pct_val,
        audio_ref_lag_n=audio_ref_lag_n_val,
        # #1267 — the av-sync dock measured-offset trend the dev1 upstream-step watchdog reads
        # (omit-when-empty; absent == UNKNOWN downstream).
        av_offset_recent_med_ms=av_offset_recent_med_val,
        av_offset_base_med_ms=av_offset_base_med_val,
        av_offset_pin=av_offset_pin_val,
        av_offset_pin_stable=av_offset_pin_stable_val,
        av_offset_age_s=av_offset_age_s_val,
        av_offset_n_recent=av_offset_n_recent_val,
        av_offset_n_base=av_offset_n_base_val,
        # #1227 — VB-Matrix presence (omit-when-empty; running="0" installed-but-dead surfaces as
        # DOWN, running="" not-installed is dropped -> UNKNOWN downstream, never a false negative).
        vb_matrix_running=vb_matrix_running_val,
        vb_matrix_name=vb_matrix_name_val,
        vb_matrix_pid=vb_matrix_pid_val,
        vb_matrix_start=vb_matrix_start_val,
    )

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
            """#652: read-only disk-usage stats over the box's OWN OBS record directory (total
            bytes + file count + oldest mtime of its top-level files) — powers
            recording-e2e.sh's disk-budget preflight WARN (the harness's own E2E test recordings
            had silently accumulated to ~500 GB on strih / 139 GB on stream). Same resolve-live
            + last-known-good fallback as the static-file GET path below — never a stale/wrong
            directory after a profile switch."""
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
