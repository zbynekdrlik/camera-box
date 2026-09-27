---
paths:
  - "scripts/lib/stream-dev-scene.sh"
  - "scripts/obs_phase2.py"
  - "scripts/rig-mode.sh"
  - "scripts/recording-e2e.sh"
  - "scripts/event_assert.py"
  - "scripts/rig-restore-watchdog.sh"
  - "tests/python/test_stream_dev_scene_1380.py"
  - "tests/python/test_event_assert_stream_program_1380.py"
  - "tests/harness_stream_dev_scene_1380.rs"
---

# The stream OBS `Development` scene — development never programs `PRO` (issue 1380)

Owner request 27.9.2026: on the stream OBS (10.77.9.204) development uses its own scene
`Development`, whose ONE item is the owner's production scene `PRO` as a nested scene source.
Development never programs `PRO` itself. EVENT mode puts `PRO` back.

## What it is

- `Development` renders exactly `PRO` (same pixels, the same warm `NDI 2ME PGM` receiver, the same
  calibrated hold). The 911004 burn, the latency pins and every NDI target stay on the input
  `NDI 2ME PGM`, so the recording and the verdict are unchanged.
- Both names are declared ONCE in `scripts/lib/stream-dev-scene.sh`
  (`STREAM_DEV_SCENE_DEFAULT` / `STREAM_PRODUCTION_SCENE_DEFAULT`). `scripts/obs_phase2.py`
  (`STREAM_DEV_SCENE` / `STREAM_PRODUCTION_SCENE`) is pinned to them by a pytest. Never retype
  `"PRO"` or `"Development"` in a consumer — derive from the lib.

## The seeder — `obs_phase2.py dev-scene` (`stream_dev_scene_ensure`)

- Pure decision `dev_scene_plan(scene_names, dev_items, dev, nested)`: missing scene → create it +
  the nested item; scene without the item → add the item; item present → nothing.
- Operator-wins: an existing item or transform is never edited. A hidden `PRO` item is left hidden
  and reported as a WARNING (the development program then renders without it).
- It reads only the scene list and the DEVELOPMENT scene's items, and writes only
  `CreateScene` / `CreateSceneItem` on the development scene, never with `ignore_err`. `PRO` is never
  read or written. A missing `PRO`, or the two names equal / empty, fails loud (`DevSceneError` → non-zero).
- Callers: `recording-e2e.sh` right after the `[4/8]` banner (before any program scene is touched),
  and `rig-mode.sh test` gap 2 (`verify_stream_program_dev`). Live seeding is the supervisor's step.

## Who uses which scene

| Path | Stream program | How |
|---|---|---|
| `recording-e2e.sh` `[4/8]` | `Development` | `STREAM_PROG_SCENE` default; `prod-scene` records it |
| `rig-mode.sh test` gap 2 | `Development` | `verify_stream_program_dev`: seed + `switch` (non-black proof) |
| `rig-mode.sh test` park (#985) | `Development` | `park_stream_program_dev`, a cheap re-assert |
| `rig-mode.sh event` | `PRO` | `restore_stream_program_production`, prod non-black floor 5 (#677) |
| EVENT contract item 9 | must read `PRO` | `event_assert.stream_program_production_ok`, fail-closed |

- `switch` skips `SetCurrentProgramScene` when the target is already on program (the #343
  same-scene hang with the heavy `NDI 2ME PGM`); the non-black proof still runs. So a park right
  after gap 2, or an EVENT run that finds `PRO` already live, costs no scene set.
- A failed EVENT restore is recorded, the burn-clear + contract + Discord confirmation still run,
  and EVENT exits non-zero at the end (the #868 fold).

## The retired stream probe

TEST gap 2 used to `obs_phase2.py setup` a stream probe scene `PHASE2-PROBE` + input
`phase2-probe-src` (a second receiver of the strih program; #988 re-created it every run). The owner
deleted both on 27.9.2026, so gap 2 now proves the stream program through the nested production
scene instead, and `rig-mode.sh` no longer calls `setup` or carries `STREAM_PROBE_UPSTREAM`.
`obs_phase2.py setup`/`teardown` and the `PHASE2-PROBE` constants stay (teardown's input idle is
`ignore_err`, so an absent input is harmless). The restore watchdog's `PHASE2-PROBE` known-test-scene
default is unchanged; `Development` is NOT a stranded scene (it is TEST mode's standing state).

## `program-rendered-input` descends into a nested scene

With `Development` on program, the first enabled item is the SCENE `PRO`.
`_resolve_rendered_input` follows a `OBS_SOURCE_TYPE_SCENE` item into that scene (cycle-guarded), so
the TEST gap-3 burn resolves `NDI 2ME PGM` and never attaches a burn filter to the scene `PRO`.

## Consumers that did NOT need a change

- `rig-restore-watchdog.sh` reads the program scene name-agnostically (`program-scene`); only its
  comment changed. `harness_rig_restore_watchdog.rs` pins that `Development` never triggers a restore.
- The handover check's `mode` item (`rig-dev-handover-check.sh` + `rig_dev_handover_decision.py`) is
  derived from the cam2 painter state (`rig-mode-state.sh`), never from a scene name, so
  `Development` on program already reads as TEST there.

## Companion

The Bitfocus Companion `PRODUCTION` trigger keys on program == `PRO` AND streaming
(`.claude/rules/strih-autorecord-coupling.md`), so development on `Development` no longer arms the
production auto-record. Stated from the trigger condition, not verified live.
