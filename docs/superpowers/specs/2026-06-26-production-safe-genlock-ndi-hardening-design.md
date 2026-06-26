# Production-Safe Genlock / NDI Hardening — Design

**Date:** 2026-06-26
**Status:** approved (architecture forks decided with the user 2026-06-26)

## Goal

Make the forked OBS + DistroAV NDI source PRODUCTION-SAFE: a hard-locked source UI that exposes
only a whitelist, genlock on by default, exactly ONE latency knob in ms (floor 3), NO hidden env,
and EVERY setting hot-applied at runtime (NO OBS restart) — including the measurement-burn toggle.
The fork exists to be stable in production, not to add more ways to mis-configure.

## Why

Today `force_genlock_certified_settings` (#150) silently FORCES the certified values but the
DistroAV source still SHOWS ~12 overridden knobs (behavior, timeout, bandwidth, sync, framesync,
hw_accel, alpha, yuv×2, audio, ptz) — the operator sees knobs that do nothing. Genlock + burns are
controlled by 9 env vars (`OBS_GENLOCK_*`, `OBS_BURN_*`) no operator can track, and changing the
burn or genlock config needs a full OBS relaunch. That "mis-set → drop all of OBS" model is not
production-viable.

## Architecture decisions (brainstorming 2026-06-26)

- **Measurement burn:** a PER-SOURCE bool in the source properties, runtime hot-apply, default OFF.
- **Genlock:** a PER-SOURCE bool, default ON.
- **Latency:** per-source int ms, default 3, **min 3** (UI min + setter clamp); reserve / ts-align /
  preload are internal, auto-derived from the ms — never user-facing, never env.
- **Delivery:** ONE PR (the whole refactor), one rig deploy + validation.

## Components

### 1. DistroAV NDI source — hard whitelist UI (`vendor/distroav/src/ndi-source.cpp`)
`ndi_source_getproperties` exposes ONLY:
- `PROP_SOURCE` — NDI source selection (unchanged).
- `PROP_GENLOCK_FIFO` (bool "Genlock") — **default ON**.
- `PROP_GENLOCK_LATENCY_MS_SRC` (int "Latency (ms)") — default 3, **min 3**, max 2000, suffix " ms".
- `PROP_BURN` (NEW bool "Measurement burn (test only)") — default OFF, runtime.

REMOVE from the UI: `PROP_BEHAVIOR`, `PROP_TIMEOUT`, `PROP_BANDWIDTH`, `PROP_SYNC`,
`PROP_FRAMESYNC`, `PROP_HW_ACCEL`, `PROP_FIX_ALPHA`, `PROP_YUV_RANGE`, `PROP_YUV_COLORSPACE`,
`PROP_AUDIO`, `PROP_PTZ`, and the read-only info labels (`PROP_GENLOCK_LATENCY_MS` global label,
`PROP_GENLOCK_PRELOAD_MS`, `PROP_GENLOCK_LATENCY_MS_SRC_HINT`).

Keep FORCING the certified values for the removed settings in ONE place
(`force_genlock_certified_settings`), driven by a single const list that is the COMPLEMENT of the
whitelist — so an upstream DistroAV property add/remove can never reintroduce a live knob. A test
asserts the exposed property set equals exactly the whitelist.

### 2. Genlock latency — one ms knob, floor 3, genlock default-on (`vendor/obs-studio/libobs` + DistroAV)
- Per-source `genlock_latency_ms`: default 3, **clamp to [3, GENLOCK_SOURCE_LATENCY_MS_MAX=2000]** in
  the setter (`GENLOCK_LATENCY_MS_MIN=3`); DistroAV UI `min=3`.
- Genlock render tick is **ON by default in the fork build** (no `OBS_GENLOCK_WALL_CLOCK` gate).
  ts-align always on (the ms path). reserve = the per-source ms. preload (internal FIFO depth)
  auto-derived from the ms.
- REMOVE env: `OBS_GENLOCK_WALL_CLOCK`, `OBS_GENLOCK_RESERVE_MS`, `OBS_GENLOCK_TS_ALIGN`,
  `OBS_GENLOCK_PRELOAD_FRAMES`, `OBS_GENLOCK_LATENCY_MS`. The 3 ms default is a build const.

### 3. Measurement burn — runtime per-source toggle (`libobs` + DistroAV + the burn render path)
- Per-source `genlock_burn` bool (default OFF). ON → the QR burn is rendered for that source's
  frames; OFF → none. Toggling applies LIVE (no restart) — same runtime path the per-source latency
  already uses (#245), via `obs_source_set_*` resolved by DistroAV `ndi_source_update`.
- REMOVE env: `OBS_BURN_QR`, `OBS_BURN_QR_PX`, `OBS_BURN_RUN_ID`, `OBS_BURN_CORNER`.
- The burn's run_id / px / corner keep the verdict contract (cam1=911001, strih=911002, stream=911004)
  WITHOUT env. **Open implementation choice for the plan:** a fixed per-box/role default (preferred —
  the run_ids are already fixed constants) vs a per-source advanced int field. Whichever, the verdict's
  911001/911002/911004 pairing must keep working and the operator-facing control stays a simple
  on/off bool.

### 4. Harness + tooling (no env)
- `scripts/launch-obs-genlock.sh`: drop all `OBS_GENLOCK_*` / `OBS_BURN_*` env set/read; relaunch =
  force-kill → clear `.sentinel\*` → relaunch cwd=`bin\64bit` → verify render tick ON (build default)
  + distroav loaded. (Keeps the #128 stale-env protection moot — there is no genlock env to carry.)
- `scripts/rig-mode.sh` (#247): test-mode = toggle the per-source `genlock_burn` ON via OBS WebSocket
  (no relaunch); event-mode = toggle OFF. No env.
- `scripts/recording-e2e.sh`: the #195 pre-record burn-ON check + #246 cleanup burn-OFF now toggle the
  per-source `genlock_burn` via WS (extends the existing obs_burn_filter.py path).
- `scripts/drift-guard.sh`: drop the `OBS_GENLOCK_*` env pins (genlock is a build default); the #246
  burn check becomes "no source has `genlock_burn=on` in prod" (read via WS), not "no `OBS_BURN_*` in
  Machine env". Update `vendor/README.md` pins + the obs-ops skill accordingly.

### 5. Tests
- DistroAV properties == exactly the whitelist (the removed props are NOT added; a single source-text
  / structural guard).
- libobs latency clamp: set 1 ms → effective 3 (floor); set 0 → 3 (default).
- genlock default-on with no env (the render tick arms without `OBS_GENLOCK_WALL_CLOCK`).
- burn runtime toggle: setting `genlock_burn` live flips the burn without a restart.
- forced-certified set still applied to the hidden knobs.
- harness/structural tests updated to the no-env world.

## Migration / deploy
ONE PR. CI windows-genlock build → deploy obs.dll + distroav.dll to strih (10.77.9.202) + stream
(10.77.9.204) off-air → rig-validate: the source UI shows ONLY source + Genlock + Latency(ms) + burn;
genlock on by default; latency floors at 3; the burn toggles live (no restart); nothing needs env.

## Non-goals
- Observability EPIC #138-143 (deferred). A/V audio sync #145 (separate design).
