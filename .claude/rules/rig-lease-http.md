---
paths:
  - "scripts/rig-lease-server.py"
  - "scripts/rig_lease_state.py"
  - "scripts/lib/rig-lease.sh"
  - "systemd/rig-lease-server.*"
  - "tests/python/test_rig_lease_state_1277.py"
  - "tests/python/test_rig_lease_server_1277.py"
  - "scripts/lib/rig-heartbeat.sh"
  - "scripts/rig_lease_refresh.py"
  - "scripts/rig-busy-gate.sh"
  - "tests/python/test_rig_lease_refresh_1383.py"
  - "tests/python/test_rig_lease_refresh_races_1383.py"
  - "scripts/rig_serve_files.py"
  - "tests/python/test_rig_marker_mirror_1404.py"
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
| `heartbeat_age_s` | int or `null` | seconds since the heartbeat file's mtime; a HUGE sentinel (`999999999`) when the heartbeat file is missing; `null` only when `held=false`. A live holder beats it every ~30 s (issue 1383, the rolling keep-alive below) |
| `stale` | bool or `null` | `null` only when `held=false`. `true` when the heartbeat is too old (or missing), OR when `holder.json` itself is absent (unconditionally stale/reclaimable) |
| `expected_release_at` | string or `null` | copied from `holder.expected_release_at`; `null` when `holder` is `null` |
| `ttl_s` | int or `null` | `expected_release_at − now` in whole seconds — **may be negative** (an overdue holder that never released on time); `null` when `expected_release_at` is absent/unparseable/`holder` is `null`. A live holder rolls it to ≥ ~15 min on every beat (issue 1383), so it is a rolling look-ahead, not the run's end |

## The ROLLING keep-alive — a live holder always reads live (issue 1383)

Found 27.9.2026 22:15 UTC: a healthy release E2E read as "hung" to a peer session (`ttl_s -273`,
`heartbeat_age_s 2959`) — the lease heartbeat was touched only by `rig-busy-gate.sh` at acquire and
`expected_release_at` stayed acquire + 45 min (`RIG_LEASE_HOLD_SECS`) for a ~60-70 min run. Since
issue 1383 the holder keeps its own lease truthful from acquire to release:

- **One helper:** `rig_lease_refresh_if_mine <repo> <run_id>` (`scripts/lib/rig-lease.sh`, a thin
  wrapper over `scripts/rig_lease_refresh.py`). A beat bumps the lease `heartbeat` mtime and rolls
  `expected_release_at` to **max(current, now + `RIG_LEASE_LOOKAHEAD_SECS`)** (default 900 s) —
  never backward, so a holder that declared a longer run up front (the soak declares its whole run
  so a CI E2E fails fast) keeps that declaration. Return codes: 0 refreshed, 1 not ours, 2 an fs
  error, 3 past the hold ceiling.
- **Three beaters, so there is no unbeaten window:**
  1. `rig-busy-gate.sh` — its busy-wait beats through the helper, and on its success path it starts
     `rig_lease_keepalive_spawn`: a detached, lease-only loop (every `RIG_LEASE_KEEPALIVE_SEC`,
     default 30 s, all fds on `/dev/null`) that lives until the lease is released or goes foreign or
     the ceiling is reached, or after `RIG_LEASE_KEEPALIVE_MAX_ERRORS` (10) filesystem errors in a
     row; the runner's end-of-job orphan cleanup ends it at the latest. It covers
     the ~18 min between the acquire and the E2E's own refresher (the verdict-exe fetch step and the
     first preflight, measured on run 36351718907).
  2. The issue-281 refresher (`scripts/lib/rig-heartbeat.sh` `rig_heartbeat_start`) of
     `recording-e2e.sh` (unchanged — static-anchor minefield) and of `av-soak.sh`, at start and on
     every beat (`RIG_HEARTBEAT_REFRESH_SEC`, 30 s), AFTER its owner `kill -0` check. Its start beat
     prints one `[rig-heartbeat] lease keep-alive <repo>#<run_id>: RIG_LEASE_REFRESH=…` line on
     stderr, so a mismatched identity is visible in the run log.
  3. `av-soak.sh` itself at every slot start and in the between-slot wait (not ours → abort; past its
     window → abort with that reason; an fs error → logged, carry on).
- **A fourth holder, `scripts/cambox-ro-units-apply.sh --apply` (issue 1394),** takes the lease as
  `camera-box-ro-units-apply` + `ro-units-apply-<stamp>-<pid>` for its whole run (a root write on
  the camboxes, minutes per box) and gives it back on every exit. It declares the rolling look-ahead
  (`RIG_LEASE_LOOKAHEAD_SECS`, 900 s), never its whole run, so a CI E2E that starts meanwhile WAITS
  for the release instead of failing fast (the gate refuses a holder whose release lies past its
  1800 s wait budget). It beats only before each box, so while one box runs (the netconsole arm can
  wait minutes for dev1) its `heartbeat_age_s` reads minutes old although the holder is alive.
  restreamer's :8890 consumer waits its 900 s cap for any live holder, so an apply delays a
  restreamer stream E2E by up to ~15 min although it never touches OBS.
- **Checklist for a NEW holder script** (both items were review-round-4 findings on the apply,
  reproduced):
  - Declare `expected_release_at` = now + the look-ahead and keep it rolling with
    `rig_lease_refresh_if_mine`, unless you WANT a CI E2E to fail fast (the soak does). The gate
    fails fast (exit 44) on any holder whose release lies past its 1800 s wait budget.
  - Store the run id the cleanup releases BEFORE calling `rig_lease_acquire`, never from its result.
    On a TERM, bash finishes the `$(rig_lease_acquire …)` substitution (the holder gets written) and
    only then runs the trap, so an id set after it leaves a live lease nobody releases. Releasing an
    id that never became the holder is safe: `rig_lease_release` checks the run_id.
- **The hold CEILING keeps the #830 "never a permanent deadlock" backstop:** no beat past
  `acquired_at + RIG_LEASE_MAX_HOLD_SECS` (default 4500 s = `full-path-e2e.yml`'s `timeout-minutes:
  75`, lock-stepped by a test; the soak sets its declared run window). An unparseable `acquired_at`
  is refused. So "alive" means "within its declared hold", not merely "the process still exists": a
  stuck-but-alive holder stops being beaten and ages into the `RIG_LEASE_STALE_SECS` reclaim.
- **How a beater knows the lease is its own:** holder.json `repo` + `run_id` must equal the identity
  it was started with — explicit arguments (the soak: `camera-box-av-soak` + its
  `av-soak-<stamp>-<pid>`; the gate: its own `RIG_LEASE_REPO`/`RIG_LEASE_RUN_ID`), else for the
  refresher the same env precedence the gate uses (`RIG_LEASE_REPO`/`RIG_LEASE_RUN_ID`, else
  `GITHUB_REPOSITORY`/`GITHUB_RUN_ID` — the E2E step (`exec bash scripts/recording-e2e.sh`) carries
  the run id the gate step took the lease for), but WITHOUT the gate's local fallback
  (`camera-box-local`/`local-<pid>`): an empty identity never touches the lease.
- **Safety of the write:** ONE python process anchored on a directory FD of the lease dir. A missing
  lease dir, a missing/corrupt `holder.json`, a foreign holder or an empty identity is a no-op that
  creates nothing (a foreign heartbeat never moves, a released lease is never re-created);
  `holder.json` is replaced by an O_EXCL temp + rename (the :8890 reader sees the old or the new
  complete file); the heartbeat and the temp are opened O_NOFOLLOW. Just before the rename,
  holder.json is re-read (a concurrent reclaim wins) and the lease path is checked to still name the
  directory the FD was opened on (a concurrent release, #857, or a release + a peer's new acquire,
  wins; the write only ever lands in the detached copy). The remaining window between those checks
  and the rename is microseconds and unreachable while the holder beats (a reclaim needs a heartbeat
  stale for 5400 s). The re-read guard, the same-directory check, the ceiling, the never-backward
  roll, the O_NOFOLLOW heartbeat and the keep-alive exits have tests that a scratch-copy mutation
  run kills (`tests/python/test_rig_lease_refresh_races_1383.py`, `…_1383.py`); O_EXCL/O_NOFOLLOW on
  the temp are defence in depth only (the same-name unlink just before the open masks them in a
  test).

**What a consumer reads during a live run:** `held=true` and `heartbeat_age_s` ≤ ~30 s once the
gate's keep-alive or the E2E refresher beats (≤ ~60 s while the gate is still in its busy-wait,
which beats once per poll, `RIG_BUSY_GATE_SLEEP_SECS` 60 s). `ttl_s` is the larger of the declared
release and the rolling look-ahead: ≈ 1980-2700 s in the first ~30 min of a CI run (acquire +
45 min still wins the max), ≈ 870-900 s after that — NOT the run's end time (the run ends when
`held` goes `false`). A held lease whose heartbeat is many minutes old is a dead holder or one past
its hold ceiling — reclaimable once `stale` flips — not a long step; the one exception is the
`camera-box-ro-units-apply` holder above, which beats only once per box. Supervisor live check: during a
release E2E, read `curl -s http://127.0.0.1:8890/rig-lease.json` twice ~40 s apart, once during the
verdict-exe fetch step and once past minute 45 of the run: `heartbeat_age_s` < 60 and `ttl_s` > 0
both times, and past minute 45 `ttl_s` ≈ 870-900 with `expected_release_at` moving forward between
the two reads; the E2E job log carries the gate's `RIG_LEASE_KEEPALIVE=started pid=…` line and the
refresher's `lease keep-alive …: RIG_LEASE_REFRESH=refreshed …` line.

## Consumer contract for restreamer#349 (the OTHER repo's own implementation)

Before every `StartStream`, restreamer's stream-box runner does:

```powershell
try {
    $lease = Invoke-RestMethod -Uri "http://dev1:8890/rig-lease.json" -TimeoutSec 5
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
    # NOTE (issue 1383): a live camera-box holder rolls ttl_s to ~900 s on every beat, so ttl_s no
    # longer predicts the release; this wait simply hits the 900 s cap before the re-poll.
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
all** (verified before this was designed — `Test-NetConnection` to dev1's LAN address on port 22 succeeded
over LAN from the stream box; tailscale was never in the picture). So the LAN path is the one
restreamer's runner actually uses, addressed by the NAME `http://dev1:8890/` (it resolves on the rig LAN
from the stream box), never a literal LAN IP: dev1's DHCP address drifted from 10.77.9.103 to 10.77.9.109,
and restreamer's fail-open consumer then silently never waited (found 5.10.2026, issue 1404). Tested from
stream.lan on 5.10.2026: `dev1` OK, `10.77.9.109` OK, tailscale times out, `dev1.lan` does not resolve; `http://100.104.8.125:8890/` (tailscale) is
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

## One more read-only route: the cam2 marker log (issue 1404)

The same server serves `GET/HEAD /rig-qpsk-markers.csv` (`text/csv`, `X-Mirror-Age-S`) from
`--serve-dir` (`$RIG_LEASE_SERVE_DIR`, default `$XDG_RUNTIME_DIR/rig-lease-serve` = tmpfs, 0700,
`scripts/rig_serve_files.py`). It is a 404 while its file is absent, or when the server runs
without a serve dir. A file another user owns is never served.
- **The serve dir is NEVER the lease dir or inside it.** The lease dir's mere existence is
  `held=true`, so a writer's `mkdir -p` there would fake a held lease. `main()` refuses it; a
  bad owner/mode of the serve dir is checked per file, so it never stops the lease endpoint.
- **`/rig-lease.json`, `/healthz` and the 404 are byte-identical** with and without a serve dir,
  pinned to golden bytes captured from the pre-change server. The marker route sits after them in
  `_handle()`; `_send()` gained only an optional `extra_headers`. An empty `$RIG_LEASE_DIR` now
  means the default, as in `scripts/lib/rig-lease.sh` (`rsf.default_lease_dir()`).
- **The server stays stdlib-only.** The numpy analysis lives in the sampler, never here.
- **`/program-audio.json` is no dev1 route any more (retired 8.10.2026, design 6054654255).** The
  program-audio sampler moved to strih-lx and serves its own `:8891/program-audio.json`
  (`.claude/rules/program-audio-guard.md`, "The host"); every consumer reads that. Running this
  code (a restart while `held=false`, `systemd/program-audio-sampler.README.md` post-merge step),
  the dev1 server answers its plain 404 for the path even when an old payload sits in its serve dir
  (`test_the_dev1_server_no_longer_serves_program_audio`), and the dev1 sampler unit file is gone.
  The payload rules (ages per request, a chain-less MEASUREMENT = UNKNOWN) stay in
  `rig_serve_files.program_audio_response`, used by the sampler's endpoint.
- **Proving a retired file route live: read it while the stale file is still there.** Every file
  route here is already a 404 on the OLD code once its file is absent. A check that deletes the
  leftover first and then reads 404 passes whether or not the server was restarted. Order: restart
  (while `held=false`), wait for `/healthz` (the unit is `Type=simple`, the restart returns before
  it listens), read the route (want 404), then delete the file. On a re-run the file is gone and
  the 404 proves nothing (`systemd/program-audio-sampler.README.md`, the dev1 post-merge block).
- Writer, contract and runbook: `systemd/rig-marker-mirror.README.md`. The running unit picks up a
  change to the routes only after `systemctl --user restart rig-lease-server.service`; do it while
  `held=false`.

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
