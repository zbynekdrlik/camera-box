---
paths:
  - "scripts/imag_scenes.py"
  - "scripts/verify-imag.sh"
  - "scripts/setup-imag.sh"
  - "tests/python/test_imag_scenes_ndi_heal_1230.py"
  - "tests/setup_imag_guards.rs"
---

# imag NDI name-healing — the idle policy was REMOVED (#1230; the #1158 healing stays)

**Owner ruling 2026-08-30 (verbatim: „ziadne nechcem mat hlupe idle policy"):** the #1218
active-set idle policy is GONE. imag now keeps **all seven cameras NAMED + alive, always** — there
is no active/inactive split, no idle payload, no `--active-cams`/state-file/`--enforce-ndi-policy`
plumbing. What is KEPT from that lineage is the **#1158 name-healing** via the shared #795-safe
`obs_phase2.reenforce_ndi_name`.

## Why the idle policy was reverted

#1218 idled an INACTIVE camera's `NDI CAM{n}` receiver (`ndi_source_name ""` + `genlock_fifo
False`) so imag stopped decoding it and thermal-throttling. It bit live on 2026-08-30: cam4/cam5
came physically back but were NOT in the (stale) active set, so all three enforcement vectors
(on-box `--bootstrap` seed, the E2E/rig-mode `--enforce-ndi-policy` pass, and the OBS-restart
scene collection that persisted the idle) kept their receivers idled — invisible on imag. Owner:
no receiver-sleeping at all. (The thermal-throttle root cause is addressed separately by the
#1040 power-envelope guard, not by refusing to decode cameras.)

## The ONE heal point

- `enforce_ndi_names(obs)` (`scripts/imag_scenes.py`) is the SINGLE imag NDI-name write path. For
  EVERY camera in `CAMS` it heals the baseline name `CAM{n} (usb)` via the shared #795-safe
  `obs_phase2.reenforce_ndi_name` (discoverable → set + read-back-verify; not in the finder →
  left as-is, never a #795 mangle). On the gated path, when the name **HEALED**, `genlock_fifo
  True` is restored too — `reenforce_ndi_name` writes ONLY the name, so a camera whose saved scene
  carried `genlock_fifo False` (e.g. an older #1218 idle persisted in the scene collection) would
  otherwise decode again but silently BYPASS the genlock FIFO. An **OFFLINE/unhealed** name is
  NOT touched (no empty-queue consume path, #70). When `obs_phase2` is unavailable (older box) OR
  the connection exposes no raw `ws` (a unit-test fake), it degrades to a direct overlay
  `SetInputSettings` of the baseline name + `genlock_fifo True`.
- It is called from the seed's `--bootstrap` block only (the boot/watchdog-reseed durable
  enforcement). A bare (non-bootstrap) reseed never enforces names here (the #785 operator-wins
  discipline). Every write uses `overlay:True` so the 3ms `genlock_latency_ms_src` pin is
  preserved — never drop overlay.
- Name recovery outside boot is covered by the existing paths (the [4c/8] frozen-camera gate,
  `set-ndi-mapping --heal`, `ndi_source_name` recovery — see `.claude/rules/ndi-name-recovery.md`),
  NOT by a dedicated E2E/dev1 imag pass — that immediate-enforce lib existed only for the idle
  policy and was removed with it.

## Gotchas that still apply

- **On-box import safety.** `imag_scenes.py` runs on the box (openbox autostart + the watchdog
  reseed), and `imag-obs-start.sh` preflights `import imag_scenes` before launching OBS. `obs_phase2`
  is imported **lazily** (inside `_obs_phase2_module()`, which returns `None` and degrades to a
  direct set if it is absent), so a stale box never crash-loops OBS; `setup-imag.sh` still installs
  it (guarded by `setup_imag_installs_obs_phase2_sibling_1218`) so a fresh box gets the gated path.
- **verify_parity output is grep-parsed.** `verify-imag.sh`'s `imag_parity_output_ok` matches
  `ndi sources: OK` with `grep -qxF` (WHOLE line). Keep it a standalone line. Since #1230 there is
  no `ndi idle (active-set): …` line any more — every camera is always named, so an empty binding is
  always a `verify_parity` mismatch.
- **imag's OBS WebSocket is passwordless.** `verify-imag.sh` drives `imag_scenes.py --host` with no
  `--password` and passes; `Obs.__init__` only demands a password when the WS `hello` carries
  `authentication`.
- **`enforce_ndi_names` uses `getattr(obs,"ws",None)`** and falls back to the direct set, so a
  FakeObs without `.ws` (the unit tests) exercises the ungated path.

## History (for the archaeology)

The #1218 lineage — `desired_ndi_state` / `is_deliberate_idle` / `ndi_policy_action` /
`enforce_ndi_active_policy` / `parse_active_cams` / `resolve_active_cams` / the
`~/.config/camera-box/imag-active-cams` state file / `scripts/lib/imag-active-cams-state.sh` /
`--active-cams` / `--enforce-ndi-policy` / the `verify_parity idle(active-set)` report — was all
removed by #1230. The commits are `f562011dc`…`eb42adb53`; do not resurrect the idle behaviour.
A future thermal fix must NOT sleep receivers (owner ruling); the #1040 power-envelope guard is the
sanctioned direction.
