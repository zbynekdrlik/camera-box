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
                          this handler is a thin transport wrapper around).
  GET /healthz          -> 200 "ok" liveness probe.
  anything else         -> 404. This server accepts GET only -- no state-mutating verb, no write
                          surface at all; restreamer's own "streaming in progress" state is ITS
                          lease signal toward camera-box (see rig-busy-gate.sh), so this endpoint
                          only ever needs to be READ.

No authentication: the payload is a boolean + holder metadata (repo/run_id/job/timestamps) + a TTL
number -- nothing secret, matching the issue's own explicit call. Bind to a private interface only
(LAN 10.77.9.103 or tailscale 100.104.8.125; NEVER a public IP) -- dev1's firewall is already LAN-
open (verified in the issue before this was designed), so this widens reachable SURFACE, not access.

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


def _fail_closed_state() -> dict:
    """The shape served when lease_state() itself raises (should never happen -- it is pure and
    catches its own I/O errors -- but a request handler must NEVER 500 on bad lease contents)."""
    now = datetime.now(timezone.utc)
    return {
        "schema": rls.SCHEMA_VERSION,
        "now": rls.format_ts(now),
        "held": True,
        "holder": None,
        "heartbeat_age_s": None,
        "stale": None,
        "expected_release_at": None,
        "ttl_s": None,
    }


class RigLeaseHandler(BaseHTTPRequestHandler):
    # Overridden per-instance by make_server() via a bound subclass -- see make_server() below.
    lease_dir = "/var/tmp/rig-lease"
    stale_secs = rls.DEFAULT_STALE_SECS

    server_version = "rig-lease-server/1277"

    def log_message(self, fmt, *args):  # noqa: A003 -- BaseHTTPRequestHandler's own hook name
        log(f"{self.address_string()} {fmt % args}")

    def _write_json(self, status: int, payload: dict) -> None:
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass  # a client that disconnected mid-response is not this server's problem

    def do_GET(self):  # noqa: N802 -- BaseHTTPRequestHandler's own hook name
        if self.path == "/rig-lease.json":
            try:
                state = rls.lease_state(self.lease_dir, datetime.now(timezone.utc), self.stale_secs)
            except Exception as exc:  # pragma: no cover -- lease_state() is designed never to raise
                log(f"lease_state() raised {exc!r} -- serving fail-closed held=true")
                state = _fail_closed_state()
            self._write_json(200, state)
            return

        if self.path == "/healthz":
            body = b"ok"
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass
            return

        self.send_response(404)
        self.send_header("Content-Length", "0")
        self.end_headers()


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
