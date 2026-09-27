---
paths:
  - "scripts/lib/stream-dev-scene.sh"
  - "scripts/stream_dev_scene.py"
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
  (`STREAM_DEV_SCENE_DEFAULT` / `STREAM_PRODUCTION_SCENE_DEFAULT`). The python module
  `scripts/stream_dev_scene.py` (`STREAM_DEV_SCENE` / `STREAM_PRODUCTION_SCENE`) is pinned to them
  by a pytest. Never retype `"PRO"` or `"Development"` in a consumer — derive from these.
- Code layout: `scripts/stream_dev_scene.py` is the PURE module (the seeder decision + apply, the
  Studio Mode preview re-assert; the obs-websocket `rpc` is injected). `obs_phase2.py` only wires the
  CLI and imports that module LAZILY inside `dev-scene` / `switch --replace-preview`: setup-imag.sh
  and setup-strih.sh install `obs_phase2.py` ALONE on the boxes, so no other subcommand may need the
  module next to it (pinned by a pytest that runs a lone copy).

## The seeder — `obs_phase2.py dev-scene` → `stream_dev_scene.ensure_dev_scene` (bash: `stream_dev_scene_ensure`)

- Pure decision `dev_scene_plan(scene_names, dev_items, dev, nested)`: missing scene → create it +
  the nested item; scene without the item → add the item; item present → nothing.
- Operator-wins: an existing item or transform is never edited. A hidden `PRO` item is left hidden
  and reported as a WARNING (the development program then renders without it).
- It reads only the scene list and the DEVELOPMENT scene's items, and writes only
  `CreateScene` / `CreateSceneItem` on the development scene, never with `ignore_err`. `PRO` is never
  read or written. A missing `PRO`, or the two names equal / empty, fails loud (`DevSceneError` → non-zero).
- Callers: `recording-e2e.sh` right after the `[4/8]` banner (after the pre-`[4/8]` rig-busy
  re-check, before any program scene is touched), and `rig-mode.sh test` gap 2
  (`verify_stream_program_dev`). Live seeding is the supervisor's step.
- The bash wrapper `stream_dev_scene_ensure` seeds ONLY the default `Development`: an env override
  of `STREAM_PROG_SCENE` naming another scene is never filled (a note, rc 0; that scene must already
  exist), and an override naming the production scene itself is refused (rc 1).

## Who uses which scene

| Path | Stream program | How |
|---|---|---|
| `recording-e2e.sh` `[4/8]` | `Development` | `STREAM_PROG_SCENE` default; `prod-scene` records it |
| `rig-mode.sh test` gap 2 | `Development` | `verify_stream_program_dev`: seed + `switch --prod-floor` (non-black proof) |
| `rig-mode.sh test` park (#985) | `Development` | `park_stream_program_dev`: `switch --prod-floor`, a cheap re-assert |
| `rig-mode.sh event` | `PRO` | `restore_stream_program_production`: `switch --prod-floor --black-report-only --replace-preview Development` |
| EVENT contract item 9 | must read `PRO` | `event_assert.stream_program_production_ok`, fail-closed |

- `switch` skips `SetCurrentProgramScene` when the target is already on program (the #343
  same-scene hang with the heavy `NDI 2ME PGM`); the non-black proof still runs. So a park right
  after gap 2, or an EVENT run that finds `PRO` already live, costs no scene set.
- EVENT's `switch` flags: `--prod-floor` = the ONE #677 production floor (`_prod_nonblack_floor`,
  env `OBS_NONBLACK_MIN_MEAN_PROD`, default 5 — never retyped in bash); `--black-report-only` = a dark
  production scene (cameras off before a service) is a WARNING once the scene is SET;
  `--replace-preview Development` = keep the Studio Mode preview off `Development`. Cause: with swap
  mode on (the OBS default, `SwapScenesMode`), a program change is a transition and when it ENDS OBS
  puts the OLD program into the preview (`TransitionStopped`). After development the program is
  `Development`, so the cut to `PRO` leaves `Development` in the preview a moment LATER — a one-shot
  check runs before that. `reassert_stale_preview` polls for the transition duration
  (`GetCurrentSceneTransition`) + `OBS_PREVIEW_SWAP_MARGIN_S` (1.5 s) and moves the preview every
  time it shows `Development`; an operator's own preview is untouched (pure
  `stale_preview_target`). Otherwise a Transition click would put `Development` back on air and skip
  the Companion `PRODUCTION` trigger.
- TEST gap 2 and the park use `--prod-floor` too: the development program is production content,
  proven with the same #677 floor `prod-scene` uses for the same scene.
- A failed EVENT restore (a failed set or transport, not a dark picture) is recorded, the
  burn-clear + contract + Discord confirmation still run, and EVENT exits non-zero at the end
  (the #868 fold).

## The retired stream probe

TEST gap 2 used to `obs_phase2.py setup` a stream probe scene `PHASE2-PROBE` + input
`phase2-probe-src` (a second receiver of the strih program; #988 re-created it every run). The owner
deleted both on 27.9.2026, so gap 2 now screenshots the development program (the production
composite: `NDI 2ME PGM` plus the scene's other layers). That proves the stream program renders,
not the strih→stream leg alone; gap 3 then resolves `NDI 2ME PGM` as the rendered input (the
input the pinned burn is on). `rig-mode.sh` no
longer calls `setup` or carries `STREAM_PROBE_UPSTREAM`.
`obs_phase2.py setup`/`teardown` and the `PHASE2-PROBE` constants stay (teardown's input idle is
`ignore_err`, so an absent input is harmless). The restore watchdog's `PHASE2-PROBE` known-test-scene
default is unchanged; `Development` is NOT a stranded scene (it is TEST mode's standing state).

## `program-rendered-input` descends into a nested scene

With `Development` on program, the first enabled item is the SCENE `PRO`.
`_resolve_rendered_input` follows a `OBS_SOURCE_TYPE_SCENE` item into that scene (cycle-guarded; an
OBS group, also `OBS_SOURCE_TYPE_SCENE` with `isGroup`, is read with `GetGroupSceneItemList`), so
the TEST gap-3 burn resolves `NDI 2ME PGM` and never attaches a burn filter to the scene `PRO`.

The descent applies to every caller: the strih gap-3 resolve in `rig-mode.sh test` and the imag
issue-1204 burn cross-check (`recording-e2e.sh`, `scripts/lib/imag-burn-verify.sh`). A program
scene whose first enabled item is a nested scene used to resolve the nested SCENE's name (a burn on a
scene / a cross-check mismatch); it now resolves the real input. Pinned with a strih-shaped fixture.

## Decided NOT to do (review round 2, Y-B)

EVENT puts `PRO` on program whatever scene is live, and item 9 requires `== PRO`. A reviewer proposed
switching only when program is `Development` (so an operator already on `PRE`/`POST` is not cut to
`PRO`). Not done here: the ticket's acceptance says "EVENT mode puts `PRO` back on program" and the
main session's design says the same; the question is recorded on the ticket for the main session.

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
