---
paths:
  - "scripts/obs-burn-reconcile-watchdog.sh"
  - "scripts/lib/obs-burn-reconcile-decision.sh"
---

# dev1-side fresh-OBS-start burn reconciliation (#1060)

The dev1 `obs-burn-reconcile-watchdog` closes the UNATTENDED half of the measurement-burn
resurrection window (#1057 closed only the deliberate dev1-driven relaunch). All three unattended
strih/stream OBS starts — box boot autostart, `NL_STARTUP.ahk` obs64 respawn, the issue-411
self-heal Task-Scheduler relaunch — reuse `launch-obs-genlock.sh`'s emitted PowerShell, which never
touches the burn, and the Windows box has no on-box python/WS client. ONE dev1 timer covers all
three because it keys on the OBS RESTART, not on which path caused it.

## The discriminator is a FRESH START, never "a burn is present"

A persistent TEST-mode burn on strih/stream is LEGITIMATE operator state (the "TEST mode stays
alive" convention). Its `#281` rig-active heartbeat is a one-shot write that goes STALE after ~10
min while the burn should remain — so **"burn present + stale heartbeat" is idle TEST mode, NOT a
leak**. Never sweep on burn-presence alone. Only an OBSERVED OBS restart makes a reloaded saved
burn a definitive resurrection.

## Fresh-start signal = `GetStats.renderTotalFrames`, read over the SAME WS the enumerator speaks

`renderTotalFrames` is monotone since OBS process start and RESETS on restart, so a DROP vs a
persisted per-box baseline = a restart since the last pass — readable from dev1 with no ssh via
`obs_burn_filter.py session-probe` (reuses `_conn`/`_rpc`). Rules baked into
`obs_burn_reconcile_is_fresh_start`: an unknown/corrupt PREVIOUS baseline is **NOT** a restart
(seed only); an unreadable CURRENT read is NOT fresh (nothing to decide, don't advance the
baseline); only `cur < prev` (both numeric) is fresh.

## Two traps this cost real review time (don't re-hit them)

- **Baseline MUST be durable (`~/.camera-box`), never tmpfs.** A `XDG_RUNTIME_DIR`/`/tmp` baseline
  is wiped on every dev1 reboot; combined with "unknown baseline ⇒ fresh" it would false-clear a
  persistent TEST burn on the first post-reboot pass even though OBS never restarted. Durable
  baseline + "unknown ⇒ seed-only" together make it impossible to sweep without an observed drop,
  while still catching a real restart that coincides with a dev1 reboot (prev survives).
- **Coordination = defer while a live gate/TEST harness drives the rig** — a FRESH `#281` heartbeat
  (`rig_heartbeat_active`) OR a held `#830` rig lease (`[ -d rig_lease_dir ] && ! rig_lease_is_stale`;
  note the polarity — `rig_lease_is_stale` returns 0 when STALE, non-zero for a LIVE holder). Never
  sweep a burn a live gate deliberately set. A deferred fresh restart sets the `unresolved` flag so
  it retries after the gate releases.

## Reuse the shared seams; fail CLOSED

Burn presence/clear route through the `#938/#1011` enumerator (`obs_burn_filter.py
sweep-check`/`sweep-off`); a `sweep-check` exit 2 (`SWEEP_ENUM_FAILED`) is fail-closed — alert
"could not verify", never report clean. A partially-failed sweep sets a per-box `unresolved` flag
and retries on later passes until read-back confirms clean (only ever set off an observed restart,
so a retry can never sweep an untied burn). Ships DISABLED; the supervisor live-verifies.

## Testing at Tier-0 (no local `cargo test`, build-ok disabled)

The pure decision (`obs_burn_reconcile_decide` / `_is_fresh_start`) is proven by the runnable
python twin `tests/python/test_obs_burn_reconcile_decision.py` (shells into the lib). The watchdog
WIRING is proven by an offline smoke: a stubbed `obs_burn_filter.py` (env-driven frames + sweep-rc)
on `OBS_BURN_FILTER_PY` + a stub `AIRULESET_NOTIFY`, driving `main` per box across
seed/NOOP/SWEEP/retry/DEFER/enum-fail. The Rust harness (`tests/harness_obs_burn_reconcile_watchdog_1060.rs`)
is CI-only (content/wiring + the decision truth-table sourced from the lib).
