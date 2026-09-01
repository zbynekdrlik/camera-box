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
CHEAP `_port4455_owning_pid()` probe. The expensive full resolution only re-runs when the observed
PID changes.

**The cheap probe's own implementation matters — even a "cheap" query can be dominated by
INTERPRETER startup, not the query itself.** The probe was FIRST implemented as its own PowerShell
one-liner (`Get-NetTCPConnection`) — live post-deploy timing showed that command alone costing
~4.1s on strih, PLUS PowerShell's own interpreter cold-start (~5-10s under load), so the "cheap"
probe still cost ~10-15s per request there and the cache above never got a chance to help (a
lighter-loaded sibling box, stream, dropped to 1.3-1.8s with the SAME code). Replaced with
`netstat -ano` (a native C tool, no interpreter startup cost at all) parsed by a new PURE
`_parse_netstat_listening_pid(text, port)` — same signature, same "" contract, so
`port4455_owner()`'s cache logic never needed to know which probe implementation feeds it. **Lesson
for any future "cheap probe": prefer a native tool over a scripting-language one-liner when the
query itself is genuinely trivial — the interpreter startup can dominate the whole cost.**

**Never restrict a diagnostic subprocess's address-family/protocol filter unless you have proven
every real caller only ever binds that one family.** `netstat -p tcp` and `-p tcpv6` are DISTINCT
filters on Windows — `-p tcp` silently returns IPv4 rows ONLY, even though both families display
literally "TCP" in the Proto column when unfiltered. Passing it here would have made the probe
permanently blind to a listener bound on IPv6 (a silent regression to `""` forever — port4455_owner()
short-circuits straight to `("", "")` without even trying the WMI fallback — not merely a
performance cost). Caught by a gated review pass, not by inspection. Fix: pass NO `-p` filter and
let the PURE PARSER's own proto/state check do the real filtering (it already handles both address
families and skips UDP rows correctly).

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

## Native tool over PowerShell one-liner — `obs_process_list()` / `_parse_tasklist_obs_process_names()`

`obs_process_list()` (feeds `bsg.obs_process_count_from_listing`) was a PowerShell `Get-Process
-Name 'obs*'` round-trip that regularly TIMED OUT at its own 15s ceiling under sustained OBS render
load — the exact interpreter-cold-start tax the port4455 netstat swap above already diagnosed on
this same box. Replaced with a native `tasklist /FO CSV /NH` subprocess, parsed by a PURE
`_parse_tasklist_obs_process_names()` that reproduces the OLD PowerShell output's exact contract
(newline-joined bare process names, `.exe` stripped) so `bsg.obs_process_count_from_listing` needed
ZERO changes downstream.

**Keep the "is this an OBS-shaped name" pattern in exactly ONE place: `bsg.OBS_PROCESS_NAME_RE`.**
A #1222c review caught this file's own tasklist parser carrying a private duplicate of the same
`obs<digits>` regex `bsg.obs_process_count_from_listing` already used — harmless today (filtering
twice with the identical pattern is a no-op) but a drift risk on a future rename. Any new facet
that needs this same "is this an OBS process name" check must import and reuse the shared constant,
never re-derive its own copy.

## File-stat-keyed process-lifetime cache — `resolve_shortcut()` / `ndi_runtime_version()`

Both facets resolve a value that is effectively STATIC between box changes — a Start-Menu `.lnk`'s
target only changes when an operator re-points it; an NDI runtime DLL's version only changes when
an SDK upgrade replaces the file itself — yet both were paying a full COM/PowerShell round-trip
(6.6s / 8.3s measured under OBS render load) on EVERY single request. Fixed with a process-lifetime
cache keyed by the TARGET FILE's own `(mtime_ns, size)` (a plain `os.stat()`, microseconds): a
changed stat re-resolves and re-caches; a file that cannot even be `stat()`'d skips BOTH the
cache-read check and the cache-write, so it keeps retrying every request rather than freezing on a
guessed value. Same never-cache-empty/never-cache-failure discipline as the PID-keyed cache above.

**When to reach for THIS pattern vs. the PID-keyed pattern above:** file-stat caching fits a facet
whose expensive resolve reads a FILE whose own mtime/size is a trustworthy freshness signal (a
shortcut, a DLL, a config file). The PID-keyed pattern fits a facet whose expensive resolve is tied
to a PROCESS identity that can be cheaply re-checked (a listening port's owning PID). Don't force
one shape onto the other's problem — e.g. don't file-stat-cache something with no backing file, and
don't PID-key something that has no owning process at all.

## Opt-in per-facet timing — `BUNDLE_STATE_TIMING=1`

`gather_bundle_state()`'s `_timed()` wrapper logs one `"gather timing: key=Xs ..."` line per
request when this env var is set — zero cost otherwise beyond a `perf_counter()` call per facet.
Use it live (redeploy + set the env + tail the server log) to find the NEXT bottleneck before
guessing; the ~18.7s fresh-log baseline had at least two independent culprits (the log read and
the port4455 WMI timeout) found only by measuring, not by inspection.
