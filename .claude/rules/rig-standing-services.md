---
paths:
  - "scripts/recording-e2e.sh"
  - "scripts/dantesync-gate.sh"
  - "scripts/clock-offset-painter-gate.sh"
  - "scripts/bundle-state-server.py"
  - "scripts/run-bundle-state-server.ps1"
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

## The general shape

A hard gate that refuses in its first seconds is almost always reporting a **rig-side standing
service that is not running**, not a defect in the thing under test. Check liveness of the gate's
own dependencies first; never weaken the gate to get past it.
