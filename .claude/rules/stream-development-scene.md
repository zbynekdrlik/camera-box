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

# The stream OBS `Development` scene — our tooling NEVER programs `PRO` (issue 1380)

**Owner hard rule, 27.9.2026, verbatim: "nemas ti nikdy v stream obs davat do programu scenu PRO!!!!!"**

- No tool of ours may EVER put the owner's production scene `PRO` on the stream OBS program OR
  preview. That covers rig-mode TEST and EVENT, the E2E, the restore helpers and cleanup.
- `Development` is the ONLY scene our tooling programs on the stream box.
- EVENT mode does NOT touch the stream program; the owner cuts to `PRO` himself.

## What it is

- `Development` (on the stream OBS, 10.77.9.204) has ONE item: the scene `PRO` as a nested source.
  It renders exactly `PRO` (same pixels, the same warm `NDI 2ME PGM` receiver, the same calibrated
  hold). The 911004 burn, the latency pins and every NDI target stay on `NDI 2ME PGM`, so the
  recording and the verdict are unchanged.
- Both names are declared ONCE in `scripts/lib/stream-dev-scene.sh`
  (`STREAM_DEV_SCENE_DEFAULT` / `STREAM_PRODUCTION_SCENE_DEFAULT`). `scripts/stream_dev_scene.py`
  (`STREAM_DEV_SCENE` / `STREAM_PRODUCTION_SCENE`) and `obs_phase2.NEVER_PROGRAM_SCENES` are pinned
  to them by pytests. Never retype `"PRO"` or `"Development"` in a consumer.
- Code layout: `scripts/stream_dev_scene.py` is the PURE seeder (decision + apply, `rpc` injected;
  no program/preview writer, pinned). `obs_phase2.py` wires the CLI and imports it LAZILY inside
  `dev-scene` only: setup-imag.sh and setup-strih.sh install `obs_phase2.py` ALONE on the boxes
  (pinned by a pytest running a lone copy).

## The guard — `obs_phase2._rpc` refuses `PRO`

`_refuse_forbidden_scene` runs at the top of `_rpc`, the one choke point every obs_phase2 request
passes through (and `warm_cam_scenes.py` reuses). A `SetCurrentProgramScene` / `SetCurrentPreviewScene`
whose sceneName is in `NEVER_PROGRAM_SCENES` raises `ForbiddenSceneError` BEFORE anything is sent,
also with `ignore_err=True`; `main()` turns it into a non-zero exit naming the scene. So `switch
--program-scene PRO` and `prod-scene --program-scene PRO` fail loud. The guard is host-agnostic (only
the stream box has a `PRO` scene).

- `teardown` (E2E cleanup, the restore watchdog) never restores `PRO` onto program or preview: a
  saved `prev_scene`/`prev_preview` of `PRO` is skipped with a named line ("never programs the
  production scene"), the program stays on `Development`, and the rest of the restore (latency,
  pins, preload, probe-input idle) still runs. An operator scene (`PRE`...) is still restored.
- Residual, not our write: in Studio Mode with swap mode on (the OBS default), a cut FROM `PRO` to
  `Development` makes OBS itself put the old program (`PRO`) into the PREVIEW when the transition
  ends. That is harmless (preview is not on air) and is OBS behavior, not a tool setting the preview.

## The seeder — `obs_phase2.py dev-scene` (bash: `stream_dev_scene_ensure`)

- Pure `dev_scene_plan`: missing scene → create it + the nested item; scene without the item → add
  it; item present → nothing. Operator-wins: an existing item/transform is never edited; a hidden
  `PRO` item is left hidden and reported.
- It reads only the scene list and the DEVELOPMENT scene's items and writes only `CreateScene` /
  `CreateSceneItem` on the development scene (fail loud). `PRO`'s own items are never read or
  written. A missing `PRO`, or the two names equal/empty, fails loud.
- Callers: `recording-e2e.sh` right after the `[4/8]` banner (after the rig-busy re-check, before any
  program switch) and `rig-mode.sh test` gap 2. Live seeding is the supervisor's step.
- The bash wrapper seeds ONLY the default `Development`: an override naming another scene is left
  alone (a note, rc 0); an override naming the production scene is refused (rc 1).

## Who programs what on the stream box

| Path | Stream program | How |
|---|---|---|
| `recording-e2e.sh` `[4/8]` | `Development` | `STREAM_PROG_SCENE` default; `prod-scene` records it |
| `rig-mode.sh test` gap 2 | `Development` | `verify_stream_program_dev`: seed + `switch --prod-floor` |
| `rig-mode.sh test` park (#985) | `Development` | `park_stream_program_dev`: `switch --prod-floor` |
| `rig-mode.sh event` | untouched | the owner cuts to `PRO` himself |
| E2E cleanup / restore watchdog | `prev_scene` unless it is `PRO` | `teardown` skips `PRO` |

- `switch` skips `SetCurrentProgramScene` when the target is already on program (the #343
  same-scene hang with the heavy `NDI 2ME PGM`); the non-black proof still runs.
- `--prod-floor` = the ONE #677 production floor (`_prod_nonblack_floor`, env
  `OBS_NONBLACK_MIN_MEAN_PROD`, default 5), shared with `prod-scene`.
- The EVENT contract (#722, `event_assert.py`) has NO stream-program item. `event_mode_assert`
  prints the current program scene as a REPORT-ONLY line; it is never a FAIL and never a switch.
- History: an earlier cut of this ticket made EVENT restore `PRO` (with `--only-from`,
  `--black-report-only`, `--replace-preview` and a Studio Mode swap preview re-assert). The owner's
  rule removed all of it; do not bring any of it back.

## The retired stream probe

TEST gap 2 used to `obs_phase2.py setup` a stream probe scene `PHASE2-PROBE` + input
`phase2-probe-src`. The owner deleted both on 27.9.2026, so gap 2 screenshots the development
program (the production composite: `NDI 2ME PGM` plus the scene's other layers) — it proves the
stream program renders, not the strih→stream leg alone; gap 3 then resolves `NDI 2ME PGM` as the
rendered input. `rig-mode.sh` no longer calls `setup` or carries `STREAM_PROBE_UPSTREAM`. The
restore watchdog's `PHASE2-PROBE` known-test-scene default is unchanged; `Development` is NOT a
stranded scene (it is TEST mode's standing state).

## `program-rendered-input` descends into a nested scene

With `Development` on program the first enabled item is the SCENE `PRO`. `_resolve_rendered_input`
follows an `OBS_SOURCE_TYPE_SCENE` item into that scene (cycle-guarded; a group is read with
`GetGroupSceneItemList`), so the TEST gap-3 burn resolves `NDI 2ME PGM` and never targets the scene
`PRO`. The descent applies to every caller (strih gap 3, the imag issue-1204 cross-check).

## The removed stream input and the CG-chain tool

The same cleanup removed the stream input `NDI obs hudba`. `scripts/cg-chain-verify.sh` defaults to
`cg-obs strih`; its stream hop is on request only and reports `ABSENT` when its input has no audit
line (`.claude/rules/cg-chain-verify.md`).

## Consumers that did NOT need a change

- `rig-restore-watchdog.sh` reads the program scene name-agnostically; its restore goes through
  `teardown`, which skips `PRO`.
- The handover check's `mode` item is derived from the cam2 painter state (`rig-mode-state.sh`),
  never from a scene name, so `Development` on program reads as TEST there.

## Companion

The Bitfocus Companion `PRODUCTION` trigger keys on program == `PRO` AND streaming
(`.claude/rules/strih-autorecord-coupling.md`); since no tool programs `PRO`, development never arms
the production auto-record. Stated from the trigger condition, not verified live.
