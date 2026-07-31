---
paths:
  - "scripts/recording-e2e.sh"
  - "scripts/dantesync-gate.sh"
  - "scripts/clock-offset-painter-gate.sh"
  - "scripts/bundle-state-server.py"
  - "scripts/run-bundle-state-server.ps1"
  - "scripts/w32time-gate.sh"
  - "scripts/lib/win-status-args.sh"
  - "scripts/lib/stale-artifact-guard.sh"
  - "scripts/dantesync-version-gate.sh"
  - ".claude/skills/e2e/SKILL.md"
---

# The E2E gate's preconditions are STANDING SERVICES — they go quiet, and the gate blames itself

Two live incidents on 2026-07-27, both discovered only because the hard gate refused, both fixed by
restarting a service that was *installed* but not *running*. Before re-reading the harness code when
the gate refuses in its first seconds, check these.

## 1. DanteSync: `PTP DEGRADED` usually means the daemon's PTP loop is simply not running

Gate output that looks like a network/grandmaster problem:

```
cam2    DRIFT    (offset -6196 us, |6196| > 2000 us bound)
cam2    PTP DEGRADED (NTP-only sawtooth — GM 10.77.9.184 down? latency meaningless)
stream  DRIFT    (offset -21354 us, ...)
!! GATE FAILED: 2 node(s) DRIFTED or PTP-DEGRADED.
```

The GM was reachable from every node the whole time. The real state:

- **cam2** — `journalctl -u dantesync | grep -ci ptp` over the last 3000 lines returned **0**: the
  PTP loop had never (re-)engaged after a long uptime, leaving pure NTP sawtooth. `systemctl restart
  dantesync` → `=== LOCKED ===` within ~40 s, offset −20 µs.
- **stream** (Windows) — the `\\.\pipe\dantesync` status read showed `mode: ACQ`, `is_locked: false`,
  `gm_uuid: null` after 6 days of process uptime. `Restart-Service dantesync` → `mode: LOCK`,
  `settled: true`, NTP −563 µs.

**Diagnostic, in order:** count PTP mentions in the journal (Linux) or read the named pipe (Windows,
`PipeDirection.In`, strip the 4-byte header to the leading `{`) — a node with *zero* PTP lines while
the GM pings fine is a dead servo, not a network fault. Restarting the daemon is the WORK (the app
under test), not a gated destructive action.

## 2. `bundle-state-server` is `Ready` but not running → the version gate refuses with exit 11

`recording-e2e.sh`'s version-integrity gate fetches `http://<box>:8899/bundle-state.json`. When it
cannot, it prints *"the win-* MCP holder must write the drift-guard observed values to …"* and exits
**11** — which reads like a version-drift problem but is really "the state server is down". On
stream the `\BundleStateServer` Scheduled Task existed in state `Ready` with nothing listening;
`schtasks /run /tn BundleStateServer` restored it (`:8899` → 200) in seconds.

Quick probe from dev1 before blaming the harness:

```bash
curl -fsS --max-time 8 -o /dev/null -w '%{http_code}\n' http://10.77.9.202:8899/bundle-state.json  # strih
curl -fsS --max-time 8 -o /dev/null -w '%{http_code}\n' http://10.77.9.204:8899/bundle-state.json  # stream
```

Tracked as a real fault (the task must survive a reboot, and the harness must say
`bundle-state-server DOWN on <box>` instead of describing the manual workaround) — see the open
issue filed 2026-07-27.

## 3. When a gate's INPUT MECHANISM migrates (e.g. file-relay → live HTTP), the runbook prose and the old relay path both rot silently — grep before trusting either (#835)

`#648` moved `dantesync-gate.sh`'s Windows-node input from a human/agent pre-fetching
`\\.\pipe\dantesync` to a file (`--win-status NAME=FILE`) to a live `--win-http` fetch
(`http://<host>:8898/status`) — but nobody updated `.claude/skills/e2e/SKILL.md`'s runbook prose,
which kept telling an operator to hand-write `dante-{strih,stream}.json` before every run. Someone
following that stale advice dropped a 21-day-old cached snapshot into a live run's `$OUTDIR` — a
real false-GREEN hazard, since the surviving `--win-status` code path was **deliberately
age-blind** (a plain `cat`, no `updated_ts`/mtime check).

**The lesson, twice over:**

- **A migration that changes HOW a gate gets its data must also update every DOC that tells a
  human what to do before running it** — the runbook prose is a caller too, and it does not fail
  loudly when it goes stale; it just quietly walks someone into reproducing the old, worse
  mechanism next to the new one.
- **Before deciding to GUARD vs REMOVE a surviving-but-superseded relay path, grep the WHOLE repo
  for its actual callers** (`grep -rn '\-\-win-status\b'`, or whatever the flag/function is),
  not just the one script you're looking at. `dantesync-gate.sh --win-status` had zero live
  callers left (only its own tests) once `recording-e2e.sh` switched to `--win-http` — the
  superior mechanism already covered the same nodes, so #835 removed it outright rather than
  bolting a freshness check onto dead code. **But check for SIBLING scripts sharing the same
  helper before deleting the helper itself** — `scripts/lib/win-status-args.sh`'s
  `win_status_parse_entry` is shared by `dantesync-gate.sh` (now-removed use) AND
  `scripts/w32time-gate.sh` (still actively uses `--win-status` for the W32Time service-state
  invariant, which has no HTTP equivalent to migrate to) — the shared helper stayed, only
  `dantesync-gate.sh`'s own flag/loop was deleted.

## 4. A NEW gate's read-path can be built on an UNVERIFIED assumption, not just a stopped service (#862)

The three incidents above all had a REAL service that merely stopped running. `dantesync-version-gate.sh`
(#862) is a different shape: the gate hard-blocked EVERY run from its very first deploy, because
its design comment ASSERTED "dantesync has no readable version on Windows, only a startup log
line" without ever running `dantesync --version` against a live box first. Both assumed sources
(`journalctl -u dantesync` on Linux, the Windows service log via bundle-state) were empty on the
REAL fleet — the log line the parser looked for is simply never logged there. `dantesync --version`
answers directly on every platform (Linux bare command on PATH; Windows the quoted full exe path
over SSH — OpenSSH-for-Windows runs it via `cmd.exe` directly, no PowerShell wrapper needed, unlike
several OTHER Windows facets this repo reads via `powershell -NoProfile -Command "..."`).

**The lesson generalizes beyond dantesync:** when a NEW gate's read path depends on a claim like
"X can only be read this way" / "Y has no direct way to answer Z", verify that claim against the
REAL target with one live command BEFORE designing the whole gate around it — especially for a
fail-closed gate that will hard-block real work the moment the assumption is wrong. A design
comment stating the premise as settled fact is not the same as having checked it live.

## 5. `[1/8]` imag render-health preflight can fail on marginal fps (~57.5-57.65) with NO code change involved — check #865/#880/#886/#888 before suspecting your own diff (2026-07-31)

The `[1/8]` imag render-health preflight (burn OFF, PROGRAM must hold 60fps with MV open) is
**deliberately kept STRICT** even though the LATER `[4d/8]` render-budget gate's imag term was
relaxed to report-only by #888 — #888's own body states this explicitly: *"The `[1/8]` imag
render-health preflight (burn OFF) — unchanged, still aborts. This keeps a genuinely sick imag
[box from silently passing]."* So a `[1/8]` failure is NOT covered by #888's relaxation and will
still abort the run, exit 1, well before StartRecord — and well before any LATER preflight you
may have just added gets a chance to run at all (confirmed live, 2026-07-31: a batch adding a
brand-new `[4h/8]` preflight never even reached it because `[1/8]` aborted first).

Two independent, ALREADY-TRACKED, unrelated-to-your-diff root causes can produce this:
- **#865/#886** — the measurement burn itself costs ~11.5ms of imag's 16.67ms budget (this is
  what #888 relaxed `[4d/8]` for — burn ON, mid-run). Not the cause of a `[1/8]` (burn OFF)
  failure, but the same family.
- **#880** — imag's iGPU clock floor (`gt_min_freq_mhz` pinned to the hardware ceiling per
  `.claude/rules/imag-nb-provisioning.md`'s `#841` section) "may not hold": `gt_act_freq_mhz` has
  been observed at 650-750MHz against a pinned 1400MHz floor — an open, tracked "candidate for
  transient render-budget spikes". A `[1/8]` window reading 57.5-57.65fps (just ~2.5fps under
  target, window 2 of 5 — NOT the tolerated warm-up window 1) is exactly this class of marginal,
  load-dependent shortfall, not a deterministic regression.

**Before touching any code over a `[1/8]` failure:** grep `gh issue list --search "render-health"`
/ `"imag render fps"` for the open tracking tickets (#865/#880/#886/#888 as of 2026-07-31) — if
your diff never touched `scripts/recording-e2e.sh`'s `[1/8]` block, `render-health-warmup.sh`, or
imag's own render/GPU code, this is very likely the SAME known marginal-headroom class. Per
`ci-monitoring.md`'s "one rerun rules out a transient" — `gh run rerun <run-id>` (never a fresh
`gh workflow run`, which loses the `pull_request` event context, see this project's own
`gh workflow run` GOTCHA in the top-level CLAUDE.md) is the correct, cheap way to check before
investigating further or filing anything new.

## The general shape

A hard gate that refuses in its first seconds is almost always reporting either (a) a **rig-side
standing service that is not running** (incidents 1-3 above — check liveness of the gate's own
dependencies first), (b) a **read-path built on an unverified assumption about how to reach the
signal at all** (incident 4 — re-derive the value with one live command against the real target
before trusting the design comment's claim), or (c) a **known, already-tracked marginal-headroom
preflight** (incident 5 — check the open tracking tickets before suspecting your own diff). Never
weaken the gate to get past any of them.
