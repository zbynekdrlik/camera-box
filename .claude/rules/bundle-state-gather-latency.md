---
paths:
  - "scripts/bundle-state-server.py"
  - "scripts/bundle_state_gather.py"
  - "tests/python/test_bundle_state_gather.py"
  - "tests/python/test_bundle_state_server_log.py"
  - "tests/python/test_bundle_state_server_port4455.py"
---

# bundle-state gather latency + caching (#1222)

The strih/stream `:8899` server (`scripts/bundle-state-server.py` + pure parsers/builders in
`scripts/bundle_state_gather.py`) feeds `recording-e2e.sh`'s `[0/8]` version-integrity gate via
`curl --max-time 30`. Two facets grow expensive with real-world session length and had to be
bounded/cached — the SAME lesson applies to any future facet added here: **never let a per-request
gather cost scale with something that grows unbounded over a session (log size, process count,
uptime), and never let a per-request external call (PowerShell/WMI/SSH) run unconditionally when
its result rarely changes.**

## Bounded log read — `bsg.read_bounded_log_text()`

`newest_obs_log_text()` used to read the WHOLE current OBS log on every request; five
`*_from_log` parsers then re-scanned it whole. A ~75 MB, ~13h session log cost ~19s
(~0.25 s/MB), on top of everything else, blowing the gate's 30s budget. Fixed by reading only a
HEAD slice (`LOG_HEAD_BYTES`, 2 MiB — the startup banner, where obs_version/distroav_version/
output_fps/genlock_wall_clock/the first genlock_capability markers all sit) + a TAIL slice
(`LOG_TAIL_BYTES`, 5 MiB — the newest state), joined by `LOG_BOUNDED_READ_SEPARATOR` when the file
exceeds head+tail; a smaller file is returned whole, unmodified.

**Adding a NEW `*_from_log` facet:** confirm it lives in the head (startup-once) or the tail
(newest-state) — a fact only ever printed in the MIDDLE of a growing log is invisible to this
scheme and would need the bounds widened or a new sampling strategy.

**Keep `LOG_BOUNDED_READ_SEPARATOR` digit-free and free of every parser's own keyword tokens**
("OBS ", "DistroAV (Version", "video settings reset:", "genlock:") — a review pass caught it once
carrying the literal issue number, silently contradicting its own "no digits" doc comment (harmless
today only because every current parser's digit pattern is keyword-anchored). Verify with
`test_bounded_read_separator_matches_no_known_facet_pattern` before changing the separator text.

**The bounded read is BINARY** (`open(path, "rb")` + `errors="replace"` decode) — unlike the old
whole-file text-mode read, it does NOT translate a Windows CRLF line ending to a bare `\n`. Every
current parser tolerates this (`splitlines()` strips a trailing `\r`; no regex crosses a line
boundary) — `test_read_bounded_log_text_crlf_log_still_parses` locks it — but a NEW parser added
here must be re-checked against a CRLF fixture, not just an `\n`-only one.

## PID-keyed cache pattern — `port4455_owner()` / `_port4455_owning_pid()`

`port4455_owner()`'s single PowerShell round-trip (`Get-NetTCPConnection` + `Get-CimInstance
Win32_Process` + `Get-Item` VersionInfo) was regularly hitting its 15s subprocess timeout on EVERY
request (live strih evidence) — ~15s of the ~18.7s fresh-log baseline. Fixed with a module-level
cache (`_port4455_cache`, guarded by `_PORT4455_CACHE_LOCK = threading.Lock()` — needed because
`ThreadingHTTPServer` dispatches each request on its own thread, mirroring the existing `_State`
class's own lock-guarded pattern in this file) keyed by the CURRENT owning PID, read via a new
CHEAP `_port4455_owning_pid()` probe (`Get-NetTCPConnection` only, no WMI). The expensive full
resolution only re-runs when the observed PID changes.

**This pattern generalizes to any future per-request external-call facet in this server that is
expensive but rarely changes:** probe a cheap, fast-changing KEY every request; only pay for the
expensive resolution when the key differs from what is cached.

**Cache-lifecycle discipline a first-pass implementation missed, caught by the gated review pass —
apply this to ANY future cache added here:**
- **Never cache an empty/degenerate resolved value.** A full resolve that SUCCEEDS (exit 0) but
  returns nothing (the #1067 access-denied shape, a transient WMI flake) must NOT be written to
  the cache — caching it freezes the facet at empty for the rest of the process's cache lifetime
  instead of retrying on the next request, which is WORSE than the pre-fix uncached behavior.
- **Always clear the cache entry on a resolve FAILURE (exception), not just leave it standing.**
  Otherwise a later reuse of the SAME cache key (e.g. Windows PID recycling — a totally different
  process later gets the same PID) can serve an identity resolved long before the failure, under a
  key that has since changed meaning.

## Opt-in per-facet timing — `BUNDLE_STATE_TIMING=1`

`gather_bundle_state()`'s `_timed()` wrapper logs one `"gather timing: key=Xs ..."` line per
request when this env var is set — zero cost otherwise beyond a `perf_counter()` call per facet.
Use it live (redeploy + set the env + tail the server log) to find the NEXT bottleneck before
guessing; the ~18.7s fresh-log baseline had at least two independent culprits (the log read and
the port4455 WMI timeout) found only by measuring, not by inspection.
