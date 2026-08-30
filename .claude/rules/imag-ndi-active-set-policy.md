---
paths:
  - "scripts/imag_scenes.py"
  - "scripts/lib/imag-active-cams-state.sh"
  - "scripts/verify-imag.sh"
  - "scripts/setup-imag.sh"
  - "tests/python/test_imag_scenes_active_set_1218.py"
  - "tests/setup_imag_guards.rs"
---

# imag active-set NDI idle policy (#1218)

imag-nb thermal-throttles when it decodes camera NDI feeds OUTSIDE the active set for nothing
(an inactive camera's `NDI CAM{n}` receiver runs a full 1080p60 decode). The cure: an
active-set-aware policy in `scripts/imag_scenes.py` — inactive cameras' receivers are idled
(`ndi_source_name ""` + `genlock_fifo False`), active ones keep their baseline name.

## The ONE policy point + its pure core

- `desired_ndi_state(n, active_cams)` (pure) is the payload: active → `{name, genlock_fifo:True}`,
  inactive → `{"", genlock_fifo:False}`. It is byte-for-byte `obs_phase2._idle_restore_settings(name)`
  / `("")` — there is a parity test; keep them equal if you touch either.
- `enforce_ndi_active_policy(obs, active_cams)` is the SINGLE enforcement point (the seed's
  `--bootstrap` block + the `--enforce-ndi-policy` mode both call it). Active → the shared #795-safe
  `obs_phase2.reenforce_ndi_name` (discoverability-gated + read-back); inactive → idle payload +
  read-back verify `""`. Every write uses `overlay:True` so the 3ms `genlock_latency_ms_src` pin is
  preserved — never drop overlay.
- `active_cams` reaches the module: `--active-cams "$CAMERA_ACTIVE_SET"` flag (a dev1 caller sourcing
  `camera-set.sh`) → else the on-box state file `~/.config/camera-box/imag-active-cams` (self-heal
  reads a fresh copy) → else `None`. A dev1 seed/enforce pass with `--active-cams` ALSO writes that
  state file to the box (local or ssh).
- `None` (no set knowledge) = baseline-heal (the pre-#1218 behavior), EXCEPT the **#1158 wedge
  discriminator**: a deliberate idle (`name==""` AND `genlock_fifo is False`) is preserved; an
  accidental wedge (empty name, `genlock_fifo` True/absent) is healed to baseline.

## Gotchas that cost time here

- **On-box import safety.** `imag_scenes.py` runs on the box (openbox autostart + the watchdog
  reseed), and `imag-obs-start.sh` preflights `import imag_scenes` before launching OBS. A NEW
  top-level `import <sibling>` in imag_scenes.py therefore HARD-requires that sibling be installed by
  `setup-imag.sh` (the #1156 pattern: gh-api fetch + chmod, guarded by a `setup_imag_installs_*`
  test) — or the box crash-loops OBS. `obs_phase2` is imported **lazily** (inside
  `_obs_phase2_module()`, which returns `None` and degrades to a direct set if it is absent), so a
  stale box never crash-loops; setup-imag.sh still installs it so a fresh box gets the gated path.
- **verify_parity output is grep-parsed.** `verify-imag.sh`'s `imag_parity_output_ok` matches
  `ndi sources: OK` with `grep -qxF` (WHOLE line). Keep it a standalone line — the active-set idle
  report goes on its OWN separate `ndi idle (active-set): …` line, never an inline suffix.
- **imag's OBS WebSocket is passwordless.** `verify-imag.sh` drives `imag_scenes.py --host` with no
  `--password` and passes, so the enforce/E2E callers do the same; `Obs.__init__` only demands a
  password when the WS `hello` carries `authentication`.
- **New params default `None`.** `seed(obs, active_cams=None)`, `verify_parity(obs, active_cams=None)`,
  `ndi_source_mismatches(actual, expected=None, active_cams=None)` — existing callers/tests pass no
  active_cams; keep the defaults so the pre-#1218 behavior (and the FakeObs without `.ws`) still work
  (`enforce_ndi_active_policy` uses `getattr(obs,"ws",None)` and falls back to the direct set).
- **E2E / rig-mode wiring is via a sourced lib** (`scripts/lib/imag-active-cams-state.sh`,
  `imag_enforce_ndi_active_policy`) — the #675 prevention pattern, so the recording-e2e.sh /
  rig-mode.sh static-anchor tests never see the logic. The helper is best-effort (ALWAYS returns 0).
  After touching those two scripts run the anchor occurrence sweep; adding `"$CAMERA_ACTIVE_SET"`
  only bumped `.contains()` presence checks (safe). Use `--active-cams`, never `--active` (a distinct
  existing anchor in set-ndi-mapping wiring).
