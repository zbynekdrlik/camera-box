# obs-burn-reconcile-watchdog — install note (#1060, a 1057 follow-up)

The fresh-OBS-start burn-reconcile watchdog (`scripts/obs-burn-reconcile-watchdog.sh`) closes the
UNATTENDED half of the measurement-burn resurrection window that issue 1057 left open.

Issue 1057 fixed the **deliberate dev1-driven relaunch**: `launch-obs-genlock.sh`'s printed PLAN
now directs a post-launch `obs_burn_filter.py sweep-off --host <ip>`. Still open were the
**unattended** strih/stream OBS start paths, where dev1 is not in the loop at start:

- box **boot autostart**,
- `NL_STARTUP.ahk` **obs64 auto-respawn** (strih),
- the **issue-411 self-heal** Task-Scheduler relaunch.

All three reuse `launch-obs-genlock.sh`'s emitted PowerShell PROGRAM, which never touches the burn
(the Windows box has no on-box python/OBS-WebSocket client, and `obs_burn_filter.py` is not
deployed there). So a saved `genlock_burn=true` reloads and renders the QR measurement burn onto
the LIVE program until the next dev1 gate run's `[0/8]` sweep. This ONE dev1 `systemd --user`
timer covers all three at once, because it keys on the OBS **restart** — not on which path caused
it — polling both boxes over the SAME OBS WebSocket `obs_burn_filter.py` already speaks (no ssh).

## How it decides (the load-bearing discriminator)

A persistent TEST-mode burn on strih/stream is a **legitimate, deliberately-persistent operator
state** whose rig-active heartbeat (#281) goes stale after ~10 min while the burn should remain —
so "burn present + stale heartbeat" is idle TEST mode, **not** a leak. The watchdog therefore acts
only on an **observed fresh OBS start** — a *drop* in `GetStats.renderTotalFrames` vs its persisted
per-box baseline (a restart since the last pass). An unknown/first/wiped baseline is only **seeded**,
never treated as a restart (so a dev1 reboot that wipes state can never false-clear a persistent
burn), and the baseline lives in the durable `~/.camera-box` (not tmpfs) so a real restart that
coincides with a dev1 reboot is still caught. When acting, even then:

- **DEFER** while a live gate/TEST harness is coordinating the rig — a fresh #281 rig-active
  heartbeat (`recording-e2e.sh` / `rig-mode.sh test`) OR a held #830 rig lease (a CI gate). This
  is the "gate-run coordination so it never clears a burn a live gate deliberately set mid-run".
- **SWEEP** (force burns OFF via `obs_burn_filter.py sweep-off`, then fire ONE Discord alert) when
  a fresh unattended restart resurrected a burn with no coordination — forcing a measurement burn
  OFF at a fresh start is unconditionally safe (it is never legitimate operator state, per 1057).
- **CLEAN** — fresh start, nothing to clear (log only).
- **NOOP** — no restart: persistent state (incl. a legitimate TEST-mode burn) untouched.

If a reconcile after an observed restart cannot confirm the box clean (a `sweep-off` that leaves a
burn, a failed enumeration, or a reconcile deferred to a live gate), the box is marked `unresolved`
and **retried on later passes** until read-back confirms clean — so a transient WS hiccup can't
leave a resurrected burn on the live program behind a single one-time alert. `unresolved` is only
ever set off an OBSERVED restart, so a retry can never sweep a burn not already tied to a restart.

All "should I sweep?" logic is the PURE `scripts/lib/obs-burn-reconcile-decision.sh` (unit-tested
offline). Burn presence/clearing route through the shared #938/#1011 enumerator
(`obs_burn_filter.py sweep-check`/`sweep-off`) and **fail CLOSED** on an un-enumerable box (a
failed `GetInputList` alerts "could not verify", never reports the box clean — guard #246/#844).

## DETECT + RECONCILE only — no OBS relaunch, no GUI/desktop action

The sweep is a session-agnostic dev1-side OBS-WebSocket op (`win-ssh-vs-mcp`), exactly like 1057's
dev1-driven sweep — never an on-box GUI/desktop op, and never an OBS relaunch. A hand-rolled on-box
WS client (candidate 2 in the ticket) was rejected: the boxes have no on-box python/WS, and issue
866 already rejected that deployed-dependency cost on the imag side.

## It ships DISABLED by default — on purpose

These units are committed but **NOT installed and NOT enabled** by this repo. Before this watchdog
ever runs unattended, the **SUPERVISOR** must install it, **live-verify** it, and only then enable
the timer. Do **not** enable it as part of merging the PR.

## Supervisor install + live-verify procedure

```bash
# 1. Dry-run a single pass — probe + decide + LOG only, NEVER sweep/alert:
scripts/obs-burn-reconcile-watchdog.sh --dry-run     # inspect the per-box probe + decision

# 2. Install the --user units (dev1):
mkdir -p ~/.config/systemd/user
cp systemd/obs-burn-reconcile-watchdog.service ~/.config/systemd/user/
cp systemd/obs-burn-reconcile-watchdog.timer   ~/.config/systemd/user/
# OBS WebSocket password (default "" — matches recording-e2e.sh); override if the rig sets one:
#   mkdir -p ~/.config/environment.d
#   printf 'OBS_PASSWORD=...\n' > ~/.config/environment.d/obs-burn-reconcile-watchdog.conf
systemctl --user daemon-reload

# 3. Live-verify BEFORE enabling the timer (all three branches):
#    a) NO restart + a persistent rig-mode TEST burn present -> a manual pass must be NOOP
#       (the persistent burn is untouched, no sweep, no alert):
scripts/rig-mode.sh test        # sets a legitimate persistent burn
scripts/obs-burn-reconcile-watchdog.sh --dry-run   # expect fresh_start=0 -> NOOP for both boxes
#    b) a genuine unattended restart with a resurrected burn (relaunch OBS out-of-gate, confirm a
#       saved genlock_burn reloaded) -> the next pass must SWEEP + alert.
#    c) a fresh restart DURING a live gate (recording-e2e.sh running, rig heartbeat fresh) -> the
#       pass must DEFER (never clear the gate's deliberate burn).

# 4. Only after all three check out, enable the recurring timer:
systemctl --user enable --now obs-burn-reconcile-watchdog.timer
systemctl --user list-timers | grep obs-burn-reconcile-watchdog

# Disable later:
systemctl --user disable --now obs-burn-reconcile-watchdog.timer
```

## Tunables (env, override in the unit or environment.d)

| Var | Default | Meaning |
|---|---|---|
| `STRIH_HOST` / `STREAM_HOST` | `10.77.9.202` / `10.77.9.204` | broadcast box OBS-WS addresses |
| `OBS_PASSWORD` | `""` | OBS WebSocket password for both boxes |
| `RIG_LEASE_STALE_SECS` | `5400` | age beyond which a held rig lease is treated as dead (so a genuinely-running gate is never mistaken for stale and its deliberate burn wrongly swept) |
| `OBS_BURN_RECONCILE_WATCHDOG_STATE_FILE` | `$HOME/.camera-box/camera-box-obs-burn-reconcile-watchdog.state` | per-box `renderTotalFrames` baseline + `unresolved`-burn flag — in a DURABLE dir (not tmpfs), so a dev1 reboot doesn't wipe the baseline; deliberately DIFFERENT from #391's and #979's state files |
| `OBS_BURN_FILTER_PY` | sibling `scripts/obs_burn_filter.py` | the shared WS/enumerator tool (session-probe / sweep-check / sweep-off) |
