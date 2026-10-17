#!/usr/bin/env python3
"""#1277 -- read-only HTTP exposure of the #830 cross-repo rig lease on dev1 (port 8890).

WHY: `scripts/lib/rig-lease.sh`'s lockdir contract (`/var/tmp/rig-lease/`) assumes both lease
participants run ON dev1's local filesystem. That is true for camera-box's own
full-path-e2e.yml runner, but FALSE for restreamer's OBS-driving E2E jobs, which run on the
Windows STREAM box (10.77.9.204) as a SYSTEM-level self-hosted runner -- a completely different
host/filesystem that can never see dev1's local lockdir. This server is the read-only window onto
that SAME lockdir restreamer's runner needs, reached over plain LAN/tailscale HTTP instead of a new
SSH credential (see the issue's own design comment for the two rejected alternatives). Consumer
contract for restreamer#349: `.claude/rules/rig-lease-http.md`.

  GET /rig-lease.json  -> the lease state, computed FRESH from RIG_LEASE_DIR at THIS request --
                          never a cached/timer-refreshed snapshot (a stale snapshot is exactly the
                          race window a coordination lock must never introduce). Schema + staleness
                          rules: see scripts/rig_lease_state.py's own module doc (the pure decision
                          this handler is a thin transport wrapper around). A trailing query string
                          (e.g. a client cache-buster `?t=1`) is stripped before matching.
                          issue 1450: the v1 keys stay first (schema 1), then `lease_model` 2, a
                          per-resource `resources` map and the `groups` (scripts/rig-lease-resources.txt).
  GET /rig-lease.json?resources=<csv> -> the same document for those resources (atoms / groups) only:
                          held iff the holder covers any of them, + `scope`. An unknown or empty
                          scope answers the WHOLE rig + `scope_error` (fail-closed, never a 400).
  GET /healthz          -> 200 "ok" liveness probe.
  HEAD /rig-lease.json, HEAD /healthz -> same routing/status as the GET form, headers only, no body
                          (a cheap liveness probe for an external checker).
  GET /rig-qpsk-markers.csv -> issue 1404: cam2's QPSK marker log as mirrored into the SERVE dir by
                          the rig-marker-mirror service (complete rows, text/csv), with
                          `X-Mirror-Age-S` = whole seconds since the mirror last received new rows;
                          404 while absent, unreadable or owned by another user. HEAD -> the same
                          status + headers, no body. Read from `--serve-dir` (default
                          $RIG_LEASE_SERVE_DIR or $XDG_RUNTIME_DIR/rig-lease-serve,
                          scripts/rig_serve_files.py) -- NEVER the lease dir, whose mere existence
                          means held=true. Without a serve dir (the old make_server() call shape)
                          the route is a plain 404.
  any other PATH        -> 404. That includes /program-audio.json: the program-audio verdict is
                          served only by the sampler's own endpoint on strih-snv
                          (scripts/program_audio_http.py, :8891); the dev1 route was retired on
                          8.10.2026 (issue 1404).
  POST /rig-lease/acquire, /rig-lease/heartbeat, /rig-lease/release -> issue 1432: the
                          AUTHENTICATED write routes a holder on another host (restreamer's stream-box
                          runner) uses to TAKE the lease; logic + contract in scripts/rig_lease_write.py,
                          the lock itself stays scripts/lib/rig-lease.sh. Any other POST path -> 404.
  any other METHOD (PUT/DELETE/OPTIONS/...) -> the stdlib default 501 Not Implemented.

No authentication on the GET routes: the payload is a boolean + holder metadata (repo/run_id/job/timestamps) + a TTL
number -- nothing secret, matching the issue's own explicit call. The default bind (0.0.0.0) is
safe here ONLY because dev1 has no public IP exposure -- it is reachable exclusively via the two
private interfaces (LAN, reached by the name dev1 since the DHCP address drifts; tailscale 100.104.8.125), and its firewall is already LAN-open
(verified in the issue before this was designed), so this widens reachable SURFACE on an already-
open box, never actual internet access. NEVER deploy this on a box that DOES have a public IP
without narrowing --bind to a private interface explicitly.

Usage:
  python3 rig-lease-server.py [--bind 0.0.0.0] [--port 8890] [--lease-dir DIR] [--stale-secs N]
                              [--serve-dir DIR]

`--lease-dir` defaults to `$RIG_LEASE_DIR` (matching scripts/lib/rig-lease.sh's own env override,
so both halves of the #830 contract read the exact same env-overridable path) or
`/var/tmp/rig-lease`. `--stale-secs` defaults to `$RIG_LEASE_STALE_SECS` (matching
scripts/rig-busy-gate.sh's own override) or 5400 (scripts/rig_lease_state.DEFAULT_STALE_SECS).
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import threading
import time
import urllib.parse
from datetime import datetime, timezone
from http.server import ThreadingHTTPServer

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rig_lease_state as rls  # noqa: E402
import rig_serve_files as rsf  # noqa: E402
import rig_lease_write as rlw  # noqa: E402

DEFAULT_BIND = "0.0.0.0"
DEFAULT_PORT = 8890


def _default_lease_dir() -> str:
    # One default for this server and the issue-1404 writers: an EMPTY $RIG_LEASE_DIR means the
    # default, exactly like scripts/lib/rig-lease.sh's ${RIG_LEASE_DIR:-/var/tmp/rig-lease}.
    return rsf.default_lease_dir()


def _default_stale_secs() -> int:
    raw = os.environ.get("RIG_LEASE_STALE_SECS", "")
    try:
        return int(raw) if raw else rls.DEFAULT_STALE_SECS
    except ValueError:
        return rls.DEFAULT_STALE_SECS


def _resources_scope(raw_path: str):
    """issue 1450: the `resources=` query value(s) of a request path, comma-joined, or None when the
    query does not name it (the whole-rig view)."""
    query = urllib.parse.urlsplit(raw_path).query
    values = urllib.parse.parse_qs(query, keep_blank_values=True).get("resources")
    return None if values is None else ",".join(values)


def log(msg: str) -> None:
    # A hidden/headless service context can hand this a dead stdout pipe (the same class
    # bundle-state-server.py's log() guards against, #829) -- logging must never take the server
    # down. The swallow is intentional and cannot itself log (stdout is the broken resource).
    # airuleset:script-ok the dead-stdout OSError is exactly what must be swallowed; logging it is impossible (stdout is the broken resource)
    try:
        print(f"{time.strftime('%Y-%m-%d %H:%M:%S')} {msg}", flush=True)
    except OSError:
        pass


class RigLeaseHandler(rsf.ReadOnlyHandler):
    # The response framing (GET/HEAD through _handle(), the query-string strip, _send(), no Python
    # version in the Server header) is rig_serve_files.ReadOnlyHandler, shared with the program-audio
    # sampler's own endpoint (issue 1404). Overridden per-instance by make_server() via a bound
    # subclass -- see make_server() below.
    lease_dir = "/var/tmp/rig-lease"
    stale_secs = rls.DEFAULT_STALE_SECS
    # issue 1404: the dir the mirrored marker file is served from; None = that route is a 404.
    serve_dir = None
    # issue 1432: the bearer-token file the three POST /rig-lease/* write routes check
    # (scripts/rig_lease_write.py); an absent/unusable file refuses every write.
    token_file = None
    # One write at a time through this server (the bash lib's mkdir lock is the real guard).
    _write_lock = threading.Lock()

    server_version = "rig-lease-server/1277"

    def log_message(self, fmt, *args):
        log(f"{self.address_string()} {fmt % args}")

    def _handle(self):
        path = self._request_path()

        if path == "/rig-lease.json":
            # issue 1450: `?resources=<csv>` asks for those rig resources only (repeated = joined);
            # absent = the whole rig. Every other query parameter (a cache-buster) is ignored.
            scope = _resources_scope(self.path)
            try:
                state = rls.lease_view(self.lease_dir, datetime.now(timezone.utc), self.stale_secs, scope=scope)
            except Exception as exc:  # an unreadable resource table; lease_state() itself never raises
                log(f"lease_view() raised {exc!r} -- serving fail-closed held=true")
                state = rls.fail_closed_view(datetime.now(timezone.utc), scope)
            self._send(200, "application/json", json.dumps(state).encode("utf-8"), no_store=True)
            return

        if path == "/healthz":
            self._send(200, "text/plain", b"ok")
            return

        if path == "/rig-qpsk-markers.csv" and self.serve_dir:
            try:
                mirror = rsf.read_mirror(os.path.join(self.serve_dir, rsf.MARKERS_NAME), time.time())
            except OSError as exc:
                log(f"{rsf.MARKERS_NAME} unreadable: {exc!r} -- serving 404")
                mirror = None
            if mirror is not None:
                data, age_s = mirror
                self._send(200, "text/csv", data, no_store=True,
                           extra_headers=(("X-Mirror-Age-S", str(age_s)),))
                return

        self._send(404, "text/plain", b"")

    def do_POST(self):
        # issue 1432: the authenticated write routes. Every other POST path is a 404 and never
        # reads the body; the other verbs keep the stdlib 501.
        path = self._request_path()
        raw = None
        if path in rlw.WRITE_PATHS:
            try:
                length = int(self.headers.get("Content-Length", ""))
            except ValueError:
                length = -1
            if 0 <= length <= rlw.MAX_BODY:
                raw = self.rfile.read(length)
            else:
                self.close_connection = True
        with self._write_lock:
            reply = rlw.handle(path, self.headers.get("Authorization"), raw, self.lease_dir,
                               self.stale_secs, self.token_file or "")
        holder = reply.payload.get("holder") or {}
        log(f"POST {path} -> {reply.status} {reply.payload.get('error', '')} "
            f"holder={holder.get('repo')}#{holder.get('run_id')}")
        self._send(reply.status, "application/json", json.dumps(reply.payload).encode("utf-8"),
                   no_store=True)


def make_server(bind: str, port: int, lease_dir: str, stale_secs: int,
                serve_dir: str | None = None, token_file: str | None = None) -> ThreadingHTTPServer:
    """Build a ThreadingHTTPServer bound to a handler CLASS carrying (lease_dir, stale_secs,
    serve_dir) -- BaseHTTPRequestHandler subclasses are instantiated per-request by the server, so
    the config is threaded via class attributes on a small bound subclass rather than instance
    state. serve_dir None (the default) keeps the issue-1404 marker route a plain 404."""
    bound_handler = type(
        "BoundRigLeaseHandler",
        (RigLeaseHandler,),
        {"lease_dir": lease_dir, "stale_secs": stale_secs, "serve_dir": serve_dir,
         "token_file": token_file if token_file is not None else rlw.default_token_file()},
    )
    return ThreadingHTTPServer((bind, port), bound_handler)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(
        description="#1277 -- read-only HTTP exposure of the #830 rig lease (GET /rig-lease.json)"
    )
    parser.add_argument("--bind", default=DEFAULT_BIND, help=f"bind address (default {DEFAULT_BIND})")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT, help=f"listen port (default {DEFAULT_PORT})")
    parser.add_argument(
        "--lease-dir", default=_default_lease_dir(),
        help="lockdir to read (default $RIG_LEASE_DIR or /var/tmp/rig-lease)",
    )
    parser.add_argument(
        "--stale-secs", type=int, default=_default_stale_secs(),
        help="heartbeat-staleness threshold in seconds (default $RIG_LEASE_STALE_SECS or 5400)",
    )
    parser.add_argument(
        "--serve-dir", default=rsf.default_serve_dir(),
        help="dir of the issue-1404 served marker file (default $RIG_LEASE_SERVE_DIR or "
             "$XDG_RUNTIME_DIR/rig-lease-serve); never the lease dir",
    )
    parser.add_argument(
        "--token-file", default=rlw.default_token_file(),
        help="issue 1432: the 0600 bearer-token file of the POST /rig-lease/* write routes "
             "(default $RIG_LEASE_TOKEN_FILE or ~/.config/camera-box/rig-lease-token)",
    )
    args = parser.parse_args(argv)
    # Only the lease-dir case stops the server: ownership/permissions are checked per served file,
    # so a bad serve dir never takes the lease endpoint itself down.
    inside = rsf.serve_dir_inside_lease(args.serve_dir, args.lease_dir)
    if inside:
        parser.error(inside)

    server = make_server(args.bind, args.port, args.lease_dir, args.stale_secs, serve_dir=args.serve_dir,
                         token_file=args.token_file)
    log(
        f"rig-lease-server listening on {args.bind}:{args.port} "
        f"(lease_dir={args.lease_dir}, stale_secs={args.stale_secs}, serve_dir={args.serve_dir}, "
        f"writes={'enabled' if rlw.read_token(args.token_file) else 'refused (no usable token file)'})"
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
