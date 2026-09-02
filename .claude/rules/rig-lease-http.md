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
    # permanent block, always a bounded worst case.
    $waitSec = [Math]::Min(($lease.ttl_s + 60), 900)
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

## Why LAN, not tailscale, is the PRIMARY path here (an exception to "address by tailscale")

The global `machine-identities.md` rule says address dev1 by tailscale, not LAN IP, because the
LAN IP drifts when equipment travels to events. That rule assumes the CONSUMER has a tailscale
address to prefer. Here it does not: the stream box (10.77.9.204) has **no tailscale interface at
all** (verified before this was designed — `Test-NetConnection 10.77.9.103 -Port 22` succeeded
over LAN from the stream box; tailscale was never in the picture). So `http://10.77.9.103:8890/`
(LAN) is the path restreamer's runner actually uses; `http://100.104.8.125:8890/` (tailscale) is
served identically and stays available for any OTHER consumer that does have a tailscale address.

## Port choice

**8890.** Port 8898 is already claimed by an unrelated service on dev1; port 8899 is the Windows
`BundleStateServer` (a completely different protocol, on a different host) — reusing either would
either collide or invite confusion between two different lease-shaped endpoints.

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
