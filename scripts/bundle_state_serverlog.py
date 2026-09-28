#!/usr/bin/env python3
"""The :8899 bundle-state server's timestamped stdout logger, shared by the server
(scripts/bundle-state-server.py) and its Windows identity readers (bundle_state_windows).

Issue 1386 slice D: the readers moved out of the hyphenated server, which a sibling cannot import
back, so the ONE logger lives here and both import it (never a second copy). Server-side, not a
gather facet: it is imported directly, not through `bundle_state_gather`. Stdlib only. Ships in the
server tree declared in `scripts/lib/bundle-state-files.txt`.
"""
from __future__ import annotations

import time


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
