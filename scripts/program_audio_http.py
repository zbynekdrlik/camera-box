#!/usr/bin/env python3
"""issue 1404 -- the program-audio sampler's OWN read-only HTTP endpoint (host-agnostic).

The sampler moved off dev1 to strih-lx (owner, 7.10.2026: dev1 is shared and loaded by other
projects' CI; ROZHODNUTÉ 6039368611), and the process serves the file it writes itself. This is the
only place the verdict is served: the dev1 rig-lease server's :8890 route was retired on 8.10.2026.

  GET/HEAD /program-audio.json  -> rig_serve_files.program_audio_response (ages recomputed per
                                   request, a file another user owns or an unreadable / non-JSON
                                   one = UNKNOWN, a MEASUREMENT without a marker chain = UNKNOWN),
                                   404 while the file is absent
  GET/HEAD /healthz             -> ok
  anything else                 -> 404; any other method -> 501 (read-only, never a write)

The response framing is rig_serve_files.ReadOnlyHandler, shared with the lease server. stdlib only.
The port and bind address come from program_audio_sampler (default 0.0.0.0:8891). Routine requests
are not logged (a consumer polls every ~10 s); an error the server reports is.
"""
from __future__ import annotations

import json
import os
import sys
import threading
import time
from datetime import datetime, timezone
from http.server import ThreadingHTTPServer
from typing import Callable

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rig_serve_files as rsf  # noqa: E402

HTTP_THREAD_NAME = "program-audio-http"


def _log(msg: str) -> None:
    # The same dead-stdout guard as the sampler's own log(): logging must never take the endpoint down.
    # airuleset:script-ok a dead stdout is the one resource this swallow is for; it cannot be logged
    try:
        print(f"{time.strftime('%Y-%m-%d %H:%M:%S')} program-audio http: {msg}", flush=True)
    except (OSError, ValueError):
        return


class ProgramAudioHandler(rsf.ReadOnlyHandler):
    """/program-audio.json + /healthz from `serve_dir` (set on a bound subclass by make_server)."""

    serve_dir: str | None = None
    server_version = "program-audio-sampler/1404"
    log_fn: Callable[[str], None] = staticmethod(_log)

    def log_request(self, code="-", size="-"):
        """Routine requests are not logged (a consumer polls every ~10 s)."""
        return

    def log_message(self, fmt, *args):
        # Reached only through log_error() now (log_request is silent): a malformed request, a
        # timeout, an unsupported method.
        self.log_fn(f"{self.address_string()} {fmt % args}")

    def _handle(self):
        path = self._request_path()
        if path == "/healthz":
            self._send(200, "text/plain", b"ok")
            return
        if path == "/program-audio.json" and self.serve_dir:
            payload = rsf.program_audio_response(
                os.path.join(self.serve_dir, rsf.PROGRAM_AUDIO_NAME), datetime.now(timezone.utc))
            if payload is not None:
                self._send(200, "application/json", json.dumps(payload).encode("utf-8"), no_store=True)
                return
        self._send(404, "text/plain", b"")


def make_server(bind: str, port: int, serve_dir: str, log: Callable[[str], None] = _log,
                timeout: float | None = None) -> ThreadingHTTPServer:
    """A ThreadingHTTPServer on (bind, port) serving `serve_dir`. Binding happens here, so a port in
    use raises OSError at once (the sampler then fails loud). Request threads are daemons; an idle
    client is dropped after `timeout` s (default rig_serve_files.ReadOnlyHandler.timeout)."""
    attrs = {"serve_dir": serve_dir, "log_fn": staticmethod(log)}
    if timeout is not None:
        attrs["timeout"] = timeout
    bound = type("BoundProgramAudioHandler", (ProgramAudioHandler,), attrs)
    server = ThreadingHTTPServer((bind, port), bound)
    server.daemon_threads = True
    return server


def serve_in_thread(server: ThreadingHTTPServer) -> threading.Thread:
    """Run `server` on its own daemon thread (HTTP_THREAD_NAME); stop it with stop()."""
    t = threading.Thread(target=server.serve_forever, name=HTTP_THREAD_NAME, daemon=True)
    t.start()
    return t


def stop(server: ThreadingHTTPServer, thread: threading.Thread, join_s: float = 5.0) -> None:
    """Stop serving, close the socket, wait for the serving thread."""
    server.shutdown()
    server.server_close()
    thread.join(join_s)
