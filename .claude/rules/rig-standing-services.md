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

## The general shape

A hard gate that refuses in its first seconds is almost always reporting a **rig-side standing
service that is not running**, not a defect in the thing under test. Check liveness of the gate's
own dependencies first; never weaken the gate to get past it.
