---
paths:
  - "scripts/rig-lease-server.py"
  - "scripts/rig_lease_state.py"
  - "scripts/lib/rig-lease.sh"
  - "systemd/rig-lease-server.*"
  - "tests/python/test_rig_lease_state_1277.py"
  - "tests/python/test_rig_lease_server_1277.py"
---

# rig-lease HTTP exposure (#1277) — the read-only window onto the #830 lockdir for a foreign host

## The premise correction #1277 exists to fix

`scripts/lib/rig-lease.sh` (issue #830) implements the cross-repo rig lease as a `/var/tmp/`
lockdir, on the premise that both lease participants run on dev1's local filesystem. That is TRUE
for camera-box's own `full-path-e2e.yml` (`runs-on: [self-hosted, ..., dev1]`) but FALSE for
restreamer's OBS-driving E2E jobs (`e2e-obs-youtube-test`, `e2e-fb-push-stream-lan`,
`e2e-streaming-test`), which run `runs-on: [self-hosted, windows, stream-lan]` — the Windows
**stream box** (10.77.9.204), as a SYSTEM-level runner on an entirely different host/filesystem.
A lockdir under dev1's `/var/tmp/` is invisible there. `scripts/rig-lease-server.py` is the
read-only HTTP window onto the SAME lockdir that closes that gap without a new SSH credential
(see the issue's own design comment for the full root cause + the two rejected alternatives).

## JSON schema — `GET /rig-lease.json`

Computed FRESH from `RIG_LEASE_DIR` at every single request, never a cached/timer-refreshed
snapshot. Full mirror contract (which field is null under which condition) lives in
`scripts/rig_lease_state.py`'s own module doc — this table is the field-by-field consumer summary:

| Field | Type | Meaning |
|---|---|---|
| `schema` | int | always `1` today |
| `now` | string | server's own UTC time, `YYYY-MM-DDTHH:MM:SSZ` |
| `held` | bool | `true` iff the lockdir (`/var/tmp/rig-lease/`) exists at all |
| `holder` | object or `null` | `{repo, run_id, run_url, job, acquired_at, expected_release_at}` — `null` when `held=false`, OR when `holder.json` is absent/unparseable (fail-closed: `held` stays `true` even then) |
| `heartbeat_age_s` | int or `null` | seconds since the heartbeat file's mtime; a HUGE sentinel (`999999999`) when the heartbeat file is missing; `null` only when `held=false` |
| `stale` | bool or `null` | `null` only when `held=false`. `true` when the heartbeat is too old (or missing), OR when `holder.json` itself is absent (unconditionally stale/reclaimable) |
| `expected_release_at` | string or `null` | copied from `holder.expected_release_at`; `null` when `holder` is `null` |
| `ttl_s` | int or `null` | `expected_release_at − now` in whole seconds — **may be negative** (an overdue holder that never released on time); `null` when `expected_release_at` is absent/unparseable/`holder` is `null` |

## Consumer contract for restreamer#349 (the OTHER repo's own implementation)

Before every `StartStream`, restreamer's stream-box runner does:

```powershell
try {
    $lease = Invoke-RestMethod -Uri "http://10.77.9.103:8890/rig-lease.json" -TimeoutSec 5
} catch {
    # connection refused / timeout -> PROCEED + log. Fail-OPEN: an endpoint that is down is NOT
    # the same as camera-box holding the rig, and camera-box's OWN OBS-state gate
    # (rig-busy-gate.sh) already protects the OPPOSITE direction. Never block a real E2E run on
    # this server's own liveness.
    Write-Warning "rig-lease-server unreachable — proceeding without a lease check"
    return
}

if ($lease.held -and -not $lease.stale) {
    # A LIVE camera-box holder. Wait with a BOUNDED budget, then re-poll once — never an
    # unbounded wait. min(ttl_s + grace, budget) mirrors the #657 self-heal doctrine: never a
    # permanent block, always a bounded worst case. CLAMP THE LOWER BOUND TO 0: an overdue holder
    # (a fresh heartbeat but a PAST expected_release_at) yields a NEGATIVE ttl_s, and
    # Start-Sleep -Seconds rejects a negative value outright (throws, uncaught here) — the exact
    # opposite of this block's own fail-open intent. ttl_s can also be $null (holder.json present
    # but its own expected_release_at is missing/unparseable) — PowerShell arithmetic treats $null
    # as 0, so the ?? below is defensive belt-and-braces, not strictly required.
    $waitSec = [Math]::Max(0, [Math]::Min((($lease.ttl_s ?? 0) + 60), 900))
    Start-Sleep -Seconds $waitSec
    # re-poll once more; if STILL held-and-fresh, proceed anyway logging the override rather than
    # blocking forever — camera-box's own gate is the hard backstop for the reverse direction.
}
elseif ($lease.held -and $lease.stale) {
    # A stale/reclaimable lease (heartbeat too old, or the holder's holder.json never got
    # written) — PROCEED. This is the #657 self-heal doctrine: a stale lease is treated as
    # abandoned, never a permanent deadlock.
}
else {
    # held=false -> genuinely free. Proceed.
}
```

Never a write from restreamer's side — it participates in NEITHER `mkdir`/`rm` on the lockdir NOR
any acquire/release protocol. Restreamer's OWN "currently streaming" state is already ITS lease
signal toward camera-box (unchanged by this ticket — see `scripts/rig-busy-gate.sh`'s existing
OBS-state busy-check).

### Known, INHERITED race — a fresh acquire's few-millisecond "absent holder.json" window

`scripts/lib/rig-lease.sh::rig_lease_write_holder` is NOT atomic: it `mkdir`s the lockdir (or reuses
an existing one on reclaim), THEN writes `holder.json`, THEN touches `heartbeat` — three separate
syscalls, not one transaction. A GET landing in the few-millisecond window between the `mkdir` and
the `holder.json` write sees `held=true, holder=null, stale=true` (an absent holder.json is treated
as unconditionally stale/reclaimable, per this server's own fail-closed contract) — so restreamer
could proceed into a lease that is, in fact, being actively (re)acquired at that exact instant. This
race is **inherited from `rig-lease.sh`'s own pre-existing acquire path** (the SAME window already
exists for a fellow camera-box gate calling `rig_lease_acquire` concurrently with another) — #1277's
HTTP mirror does not introduce it, and does not widen it beyond what the bash implementation already
tolerates. Given its consequence is a rare, few-millisecond race with a low-severity outcome
(two acquires landing within milliseconds of each other, not a corruption), fixing it would mean
changing `rig-lease.sh`'s own write ordering (e.g. write-to-temp-then-rename before the `mkdir`
becomes externally visible) — a change to the ALREADY-SHIPPED, heavily-tested #830 acquire/release
protocol, out of scope for this ticket. Tracked here as a known, accepted, pre-existing limitation
rather than silently ignored.

## Why LAN, not tailscale, is the PRIMARY path here (an exception to "address by tailscale")

The global `machine-identities.md` rule says address dev1 by tailscale, not LAN IP, because the
LAN IP drifts when equipment travels to events. That rule assumes the CONSUMER has a tailscale
address to prefer. Here it does not: the stream box (10.77.9.204) has **no tailscale interface at
all** (verified before this was designed — `Test-NetConnection 10.77.9.103 -Port 22` succeeded
over LAN from the stream box; tailscale was never in the picture). So `http://10.77.9.103:8890/`
(LAN) is the path restreamer's runner actually uses; `http://100.104.8.125:8890/` (tailscale) is
served identically and stays available for any OTHER consumer that does have a tailscale address.

## Port choice

**8890.** Port 8898 is already claimed on dev1 by dev1's OWN `dantesync` daemon (dev1 is a
clock-sync fleet participant like every other box, per `.claude/rules/dantesync-version-reading.md`
— `dantesync --version` is on dev1's own PATH), which serves the SAME `/status` HTTP endpoint every
fleet box does (see `.claude/rules/dantesync-clock-offset-gate.md`); live-verified while writing
this: `curl http://127.0.0.1:8898/status` on dev1 returns a real dantesync JSON status blob
(`mode: LOCK`, `gm_source_ip: 10.77.9.184`), and `ss -ltnp` shows `0.0.0.0:8898 LISTEN`. Port 8899
is the Windows `BundleStateServer` (a completely different protocol, on a different host —
strih/stream, not dev1). Reusing either would either collide with a live service or invite
confusion between two different lease-shaped endpoints on the same box.

## Fail-open direction — restated, because it is easy to get backwards

The rig-lease HTTP server being unreachable/refused/timed-out is **restreamer's problem to
degrade gracefully around, never camera-box's problem to fix reactively**. The camera-box→
restreamer direction was ALREADY protected before #1277 (camera-box's `rig-busy-gate.sh` refuses
while stream OBS is streaming/recording) — this server only closes the REVERSE direction, and only
as an ADVISORY signal for restreamer to wait a bounded time. If this server is down, restreamer
proceeding anyway is the correct, documented behavior — it is not a silent hole, it is the explicit
design (Prístup 1's own trade-off statement: the server is new coordination surface, not a new
hard dependency either side must have to function at all).

## Supervisor install step

See `systemd/rig-lease-server.README.md` for the full install/verify/enable procedure. Summary:
this ships **intended to run ENABLED** (unlike every alert-watchdog unit in this repo, which ships
disabled) — install the `--user` unit, live-verify both the free and held shapes via curl on the
LAN address, then `systemctl --user enable rig-lease-server.service`. No Windows-side change is
made by this ticket; restreamer's own runner implements the consumer-contract PowerShell above in
ITS OWN repo (restreamer#349), not here.

## Testing note — a worktree worker cannot exercise the sourced-bash side locally

`scripts/rig_lease_state.py`'s staleness mirror is verified against `scripts/lib/rig-lease.sh`'s
OWN bash functions only by READING them (this repo's `ci-testing-gotchas.md` worktree-isolation
note: `bash -c '…source lib…'` is refused for a worktree-isolated worker). The pytest suite proves
the Python side's OWN behavior end-to-end (12 pure-decision tests + 5 real-server integration
tests against a genuine `ThreadingHTTPServer` on an ephemeral port via `http.client`) — a
cross-language parity harness (running BOTH the bash functions and the Python mirror against the
same fixture lockdir and diffing their verdicts) was considered out of scope for this ticket
(the mirror is a doc-comment-verified manual port, not a generated one) and is a reasonable
follow-up if the two ever need machine-verified lock-step parity beyond the one shared constant
(`RIG_LEASE_STALE_SECS`/`DEFAULT_STALE_SECS`) this ticket's tests already lock-step.
