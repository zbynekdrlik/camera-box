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
  GET /healthz          -> 200 "ok" liveness probe.
  HEAD /rig-lease.json, HEAD /healthz -> same routing/status as the GET form, headers only, no body
                          (a cheap liveness probe for an external checker).
  any other PATH        -> 404.
  any other METHOD (POST/PUT/DELETE/OPTIONS/...) -> the stdlib default 501 Not Implemented (this
                          server implements no do_POST/do_PUT/etc. handler at all -- never a write
                          surface, never a 5xx from application code). This server accepts GET/HEAD
                          only; restreamer's own "streaming in progress" state is ITS lease signal
                          toward camera-box (see rig-busy-gate.sh), so this endpoint only ever needs
                          to be READ.

No authentication: the payload is a boolean + holder metadata (repo/run_id/job/timestamps) + a TTL
number -- nothing secret, matching the issue's own explicit call. The default bind (0.0.0.0) is
safe here ONLY because dev1 has no public IP exposure -- it is reachable exclusively via the two
private interfaces (LAN 10.77.9.103, tailscale 100.104.8.125), and its firewall is already LAN-open
(verified in the issue before this was designed), so this widens reachable SURFACE on an already-
open box, never actual internet access. NEVER deploy this on a box that DOES have a public IP
without narrowing --bind to a private interface explicitly.

Usage:
  python3 rig-lease-server.py [--bind 0.0.0.0] [--port 8890] [--lease-dir DIR] [--stale-secs N]

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
import time
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rig_lease_state as rls  # noqa: E402

DEFAULT_BIND = "0.0.0.0"
DEFAULT_PORT = 8890


def _default_lease_dir() -> str:
    return os.environ.get("RIG_LEASE_DIR", "/var/tmp/rig-lease")


def _default_stale_secs() -> int:
    raw = os.environ.get("RIG_LEASE_STALE_SECS", "")
    try:
        return int(raw) if raw else rls.DEFAULT_STALE_SECS
    except ValueError:
        return rls.DEFAULT_STALE_SECS


def log(msg: str) -> None:
    # A hidden/headless service context can hand this a dead stdout pipe (the same class
    # bundle-state-server.py's log() guards against, #829) -- logging must never take the server
    # down. The swallow is intentional and cannot itself log (stdout is the broken resource).
    # airuleset:script-ok the dead-stdout OSError is exactly what must be swallowed; logging it is impossible (stdout is the broken resource)
    try:
        print(f"{time.strftime('%Y-%m-%d %H:%M:%S')} {msg}", flush=True)
    except OSError:
        pass


class RigLeaseHandler(BaseHTTPRequestHandler):
    # Overridden per-instance by make_server() via a bound subclass -- see make_server() below.
    lease_dir = "/var/tmp/rig-lease"
    stale_secs = rls.DEFAULT_STALE_SECS

    server_version = "rig-lease-server/1277"
    # Suppress the interpreter version from the Server: response header (BaseHTTPRequestHandler's
    # version_string() concatenates server_version + " " + sys_version) -- no reason to advertise
    # the exact Python patch version to an unauthenticated caller.
    sys_version = ""

    def log_message(self, fmt, *args):
        log(f"{self.address_string()} {fmt % args}")

    def _request_path(self) -> str:
        # Strip a query string before matching -- `GET /rig-lease.json?t=1` (a common client-side
        # cache-buster) must still hit the real route, not fall through to 404 (which would make
        # restreamer's consumer contract fail-open and silently drop the lease check).
        return self.path.split("?", 1)[0]

    def _send(self, status: int, content_type: str, body: bytes, *, no_store: bool = False) -> None:
        # The WHOLE response (status line + headers + body) is wrapped in ONE try/except -- a
        # client that disconnects between send_response() and end_headers() would otherwise raise
        # an unguarded BrokenPipeError/ConnectionResetError (only the body write used to be
        # guarded), which socketserver logs as a traceback even though it is not a real fault.
        try:
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            if no_store:
                self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass  # a client that disconnected mid-response is not this server's problem

    def _handle(self):
        path = self._request_path()

        if path == "/rig-lease.json":
            try:
                state = rls.lease_state(self.lease_dir, datetime.now(timezone.utc), self.stale_secs)
            except Exception as exc:  # pragma: no cover -- lease_state() is designed never to raise
                log(f"lease_state() raised {exc!r} -- serving fail-closed held=true")
                state = rls.fail_closed_state(datetime.now(timezone.utc))
            self._send(200, "application/json", json.dumps(state).encode("utf-8"), no_store=True)
            return

        if path == "/healthz":
            self._send(200, "text/plain", b"ok")
            return

        self._send(404, "text/plain", b"")

    def do_GET(self):
        self._handle()

    def do_HEAD(self):
        # A cheap liveness probe an external checker can use without paying for a JSON body --
        # routes through the SAME path matching as do_GET (_handle() suppresses the body write
        # via self.command == "HEAD" inside _send()), so the two can never drift on which paths
        # are recognized.
        self._handle()


def make_server(bind: str, port: int, lease_dir: str, stale_secs: int) -> ThreadingHTTPServer:
    """Build a ThreadingHTTPServer bound to a handler CLASS carrying (lease_dir, stale_secs) --
    BaseHTTPRequestHandler subclasses are instantiated per-request by the server, so the config is
    threaded via class attributes on a small bound subclass rather than instance state."""
    bound_handler = type(
        "BoundRigLeaseHandler",
        (RigLeaseHandler,),
        {"lease_dir": lease_dir, "stale_secs": stale_secs},
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
    args = parser.parse_args(argv)

    server = make_server(args.bind, args.port, args.lease_dir, args.stale_secs)
    log(
        f"rig-lease-server listening on {args.bind}:{args.port} "
        f"(lease_dir={args.lease_dir}, stale_secs={args.stale_secs})"
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
