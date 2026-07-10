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


def log(msg):
    print(f"{time.strftime('%Y-%m-%d %H:%M:%S')} {msg}", flush=True)


def newest_obs_log_text(log_dir):
    """The raw text of the newest *.txt OBS log in *log_dir* ("" if none/unreadable — the callers
    already treat an unreadable log as every derived key coming back empty/UNKNOWN)."""
    try:
        candidates = glob.glob(os.path.join(log_dir, "*.txt"))
        if not candidates:
            log(f"WARNING: no OBS log files found under {log_dir}")
            return ""
        newest = max(candidates, key=os.path.getmtime)
        with open(newest, "r", encoding="utf-8", errors="replace") as f:
            return f.read()
    except OSError as e:
        log(f"WARNING: could not read OBS log dir {log_dir}: {e}")
        return ""


def ndi_runtime_version(dll_path):
    """Get-Item's VersionInfo.FileVersion, shelled to PowerShell (there is no stdlib way to read a
    Windows PE VERSIONINFO resource) — the exact one-liner drift-guard.md step 1 documents. ""
    on any failure (missing file, powershell error) — never a guessed value."""
    if not os.path.isfile(dll_path):
        log(f"WARNING: NDI runtime DLL not found at {dll_path}")
        return ""
    try:
        out = subprocess.run(
            [
                "powershell", "-NoProfile", "-NonInteractive", "-Command",
                f"(Get-Item -LiteralPath '{dll_path}').VersionInfo.FileVersion",
            ],
            capture_output=True, text=True, timeout=15, check=True,
        )
        return out.stdout.strip()
    except (subprocess.SubprocessError, OSError) as e:
        log(f"WARNING: could not read NDI runtime version: {e}")
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


def gather_bundle_state(obs_host, password, obs_log_dir, ndi_runtime_dll, distroav_scan_roots):
    """Build the fresh bundle-state dict for THIS request — every gather is attempted
    independently so one failing facet (e.g. OBS-WS momentarily unreachable) does not blank out
    the log-derived facets that still read fine; each key that could not be read is simply
    omitted (UNKNOWN downstream), never guessed."""
    log_text = newest_obs_log_text(obs_log_dir)
    ndi_inputs = {}
    try:
        ndi_inputs = gather_ndi_inputs(obs_host, password)
    except Exception as e:  # noqa: BLE001 - any WS/RPC failure must not crash the whole response
        log(f"WARNING: could not gather NDI input latency over obs-websocket: {e}")

    return bsg.build_bundle_state(
        obs_version=bsg.obs_version_from_log(log_text),
        distroav_version=bsg.distroav_version_from_log(log_text),
        ndi_runtime=ndi_runtime_version(ndi_runtime_dll),
        output_fps=bsg.output_fps_from_log(log_text),
        genlock_wall_clock=bsg.genlock_wall_clock_from_log(log_text),
        ndi_input_latency=bsg.ndi_input_latency_csv(ndi_inputs),
        distroav_dll_paths=bsg.distroav_dll_paths(distroav_scan_roots),
        genlock_capability=bsg.genlock_capability_from_log(log_text),
    )


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
    args = ap.parse_args(argv)

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
