---
paths:
  - "scripts/camera-set.sh"
  - "scripts/recording-e2e.sh"
  - "scripts/rig-mode.sh"
  - "scripts/set-ndi-mapping.py"
  - "scripts/latency_pins_snapshot.py"
  - "scripts/phase_sync_active_floor_check.py"
  - "scripts/phase_sync_calibrate.py"
  - "scripts/phase_sync_reanchor.py"
  - "rig-fleet.txt"
---

# CAMERA_ACTIVE_SET — every fleet-enumeration consumer MUST derive from it, never a literal range

`CAMERA_ACTIVE_SET` (`scripts/camera-set.sh`, #827) is the ONE declared list of cameras physically
installed and active TODAY (default `cam2 cam3` — cam1 was retired #1134 2026-08-19, briefly returned
#1130 the same day, then RE-RETIRED for real 2026-08-22 #1110 because its ShadowCast USB grabber is
hardware-defective beyond software compensation (chronic over-rate, constant corruption, USB re-auth
does not cure); cam1/cam4/cam5/cam6/cam7 retired but fully resolvable — see the
header comment in `camera-set.sh`). The parallel `CAMERA_ALIGN_SET` (the on-air alignment superset,
default `cam2 cam3 cam4`) dropped cam1 in the SAME #1110 retirement — a dead grabber cannot go on-air
to be aligned. **Every place that needs "the list of
cameras to check/sample/sweep right now" must derive it from `CAMERA_ACTIVE_SET`, not from a
literal range or its own hardcoded list.** A retired camera's facts (IP, NDI source name, genlock
fps, strih scene/route) stay fully resolvable forever (`camera_resolve`/`camera_strih_route` never
gate on the active set) — only membership in `CAMERA_ACTIVE_SET` decides whether a camera is
currently swept/checked.

## The bug shape, twice now

1. **#827 initial pass** fixed the STATIC-table consumers (`camera-set.sh`'s own `CAMERA_SET`
   default, `recording-e2e.sh`'s `[0/8]` fleet-preflight target list via
   `camera_active_secondary_set()`, the `CAMBOX_SWEEP` default via `camera_active_sweep_pairs()`,
   `set-ndi-mapping.py`'s `--active` flag).
2. **#827 follow-up** (2026-07-28) found THREE more call sites that still enumerated the fleet via
   a **literal `for _n in 1 2 3 4 5 6 7` range**, only subtracting `PREFLIGHT_EXCLUDED_CAMS` (the
   *temporary* acked-offline list from `CAMBOX_OFFLINE_ACK` — a completely different mechanism
   from the *permanent* `CAMERA_ACTIVE_SET` retirement): the `[0/8]` genlock_burn-OFF pre-check,
   the `[1/8]` frozen-camera-gate MV-liveness preflight, and the `[5/8 pre]` live-freeze-watch
   arming. A retired camera is never a member of `PREFLIGHT_EXCLUDED_CAMS` (it was never evaluated
   for exclusion — it's not even in the preflight target list), so it silently stayed in these
   three derived source lists and got sampled anyway. Live hardware gate run 30310110884 proved it:
   the `[1/8]` preflight sampled `NDI cam5`/`NDI cam6`/`NDI cam7` (retired, unplugged, never emit)
   and failed FROZEN on all three.

**The tell:** any `for _n in 1 2 3 4 5 6 7` (or similar hand-rolled numeric range over the camera
fleet) anywhere in `recording-e2e.sh`/`rig-mode.sh` is the bug pattern, regardless of what
exclusion logic sits next to it — an exclusion list built from a DIFFERENT mechanism (acked-offline)
can never substitute for intersecting with `CAMERA_ACTIVE_SET`.

## The PRIMARY/source role is ALSO derived now (#1134), not hardcoded to cam1

The E2E chain's SOURCE node (the "cam1 role": films cam2's monitor, carries the #174 capture burn,
routed onto strih PROGRAM) used to be hard-pinned to the literal `cam1` in `recording-e2e.sh`.
Since #1134 it is DERIVED via `camera_source_box()` (`camera-set.sh`) = the FIRST strih-routable
member of `CAMERA_ACTIVE_SET` (cam2 the painter is skipped — `camera_strih_route` rejects it), or
the explicit `CAMERA_SOURCE_BOX` env override (same trust model as `CAM=`). So a cam1-first legacy
set still resolves source=cam1 (byte-identical back-compat), while the `cam2 cam3` default resolves
source=cam3. `camera_source_box` probes `camera_strih_route` in a **subshell** so it never leaks
`CAMERA_STRIH_SCENE`/`CAMERA_STRIH_SOURCE`, and reuses that function as the single source-eligibility
authority (never a second cam list). `recording-e2e.sh` reads the role at exactly three sites
(`E2E_SOURCE_BOX="$(camera_source_box)"` → the `camera_resolve "${CAM:-$E2E_SOURCE_BOX}"` default,
the ALL_CAMBOX guard `[ "$CAMERA_NAME" != "$E2E_SOURCE_BOX" ]`, and the `[0/8]` fleet-preflight
label `PREFLIGHT_TARGETS=("$CAMERA_NAME=$CAM1_IP" ...)`), and `camera_active_secondary_set` now
excludes the DERIVED source + cam2 (not the literal `cam1|cam2`). The VERDICT side needs NO change
for a non-cam1 source: the ALL_CAMBOX MERGE path passes every `--burn-camN-run-id` and the
`--extract-partial` decode uses the binary DEFAULT `BURN_RUN_ID_CAM<N>` (== the shell
`BURN_CAM<N>_RUN_ID`), so a cam3 source (deployed with `SRC_BURN_RUN_ID=$BURN_CAM3_RUN_ID`) is
mapped generically.

**Retiring a source-eligible camera = membership + an ack, nothing else.** Drop it from
`CAMERA_ACTIVE_SET` (the source role moves to the next strih-routable member automatically) AND add
a `<box>:<reason>` line to `rig-fleet.txt`. A box OUTSIDE the active set is never a preflight target,
so its ack never trips the stale-ack guard (the cam4 precedent — `cam4:on-air-but-outside-measured-set`).
RE-ENABLE = add the name back to `CAMERA_ACTIVE_SET` + delete its `rig-fleet.txt` ack line.

**`rig-mode.sh` (the TEST/EVENT switch) derives the source role too since #1135.** It resolves ONE
`RIG_SOURCE_BOX="$(camera_source_box)"` in its pinned-constants block (above the source-guard, so
`tests/rig_mode.rs` sees the derived facts on source), then `camera_resolve` → `RIG_SOURCE_IP`,
`camera_strih_route` → `RIG_SOURCE_STRIH_SOURCE`, and `imag_scene_for_camera`/`imag_source_for_camera`
(`scripts/lib/imag-scene-route.sh`) → the imag scene/input. The five former cam1 hardcodes now read
the role: `CAM1_IP` is gone (override is `CAMERA_SOURCE_BOX`), `STRIH_PROG_SOURCE`/`IMAG_PROG_SOURCE`/
`IMAG_PROG_SCENE` default to the derived values, `EVENT_ASSERT_TARGETS=("${RIG_SOURCE_BOX}=$RIG_SOURCE_IP" "cam2=$PAINTER_IP")`
sweeps the resolved source (so a cam1-retired rig sweeps cam3, not the broken cam1), and the
TEST-mode prints name the resolved box. NOTE the imag pair `imag_scene_for_camera`/`imag_source_for_camera`
must stay lock-step (same cam1-cam6 `case` set) so a source resolves BOTH or fails loud on BOTH.
`recording-e2e.sh`'s own `IMAG_PROG_SOURCE="NDI CAM1"` is a residual left to the E2E-gate/#1134 side
(imag leg is report-only + offline-ackable), NOT folded into #1135.

## The fix pattern — two small pure helpers, not three separate inline loops

`scripts/camera-set.sh` now has two derivation helpers built for exactly this:

- `camera_active_excluding <excluded_space_list>` → cam names in `CAMERA_ACTIVE_SET` minus any
  word in `excluded` (for callers that need to iterate individual cam names, e.g. per-camera SSH
  or OBS checks).
- `camera_active_ndi_sources_excluding_csv <excluded_space_list>` → `"NDI cam1,NDI cam2,..."` CSV
  built from the same filtered list (for callers passing a comma-joined source list to
  `frozen-camera-gate.py --sources` or `live_freeze_watch_start`).

Both take the SAME `PREFLIGHT_EXCLUDED_CAMS`-shaped argument the existing call sites already had —
swapping a literal-range loop for one of these two calls preserves the acked-offline exclusion
behavior byte-for-byte, only fixing which candidates are considered in the first place. **When
adding a NEW fleet-wide consumer, reuse one of these two helpers — never re-invent a third
"active minus excluded" loop.**

## Testing without a rig

Every helper in `camera-set.sh` is a pure bash function over `CAMERA_ACTIVE_SET` + an argument —
test it by sourcing the script with an env override and calling the function directly (see
`tests/harness_camera_set.rs`'s `active_excluding`/`active_ndi_sources_excluding_csv` helpers). The
property that matters: (a) the default active set never includes a retired camera in the derived
list, even with an empty exclusion, and (b) overriding `CAMERA_ACTIVE_SET` to re-add a retired
camera makes it flow through to every derived consumer, proving the reversal actually works (not
just a comment claiming it does).

## Retiring a camera — the default LITERAL is independently duplicated in FIVE standalone Python scripts, grep the whole repo before trusting one file's change is enough

Changing which cameras are in the default `CAMERA_ACTIVE_SET` is NOT a one-file edit, even though
`camera-set.sh` is the "ONE declared list" for every SOURCED bash consumer. FIVE standalone Python
subprocesses (`set-ndi-mapping.py`'s `DEFAULT_ACTIVE_SET`, `latency_pins_snapshot.py`'s
`active_camera_numbers()`, `phase_sync_active_floor_check.py`'s `active_camera_names()`,
`phase_sync_calibrate.py`'s `active_ndi_sources()`, and `phase_sync_reanchor.py` — the 5th, which
the pre-#1134 version of this rule MISSED) each carry their OWN fallback literal matching
camera-set.sh's default — by design (they read the same `$CAMERA_ACTIVE_SET` env var but are never
`source`d, so each needs its own Python-side default for when the caller invokes them directly
without exporting the var). This is the exact same class of gotcha `ci-testing-gotchas.md`
documents for a shared numeric constant duplicated across languages (#707) — it applies just as
much to a shared STRING default. **Before changing `CAMERA_ACTIVE_SET`'s default membership, `grep
-rn "cam1 cam2 cam4"` (or whatever the current literal is) across `scripts/*.sh` AND `scripts/*.py`
AND `tests/**/*.rs` AND `tests/python/*.py`** — missing any one of the four Python fallbacks (or
their own default-set unit tests) leaves a script that silently disagrees with camera-set.sh the
moment it runs without the env var exported (e.g. invoked directly by hand, or by a caller that
forgot to pass `--active-set`/export the var).

## A "default active set proves parallelism" test can lose its power when the default shrinks

`tests/harness_cambox_parallel_restore_712.rs`'s `all_cambox_restore_loop_runs_in_parallel_not_sequentially`
measured wall-clock time against the DEFAULT active set specifically to prove 2+ boxes are
contacted CONCURRENTLY, not sequentially. Retiring cam3 shrank the default secondary set
(`camera_active_secondary_set()`, i.e. active minus cam1/cam2) to a SINGLE camera (cam4 alone) —
with only one box, sequential and parallel execution are indistinguishable by wall-clock, so the
test would have kept passing while silently losing its ability to catch a real sequential-loop
regression. **When a retirement shrinks a "must run in parallel" test's DEFAULT-derived box count
below 2, widen that specific test via an explicit `CAMERA_ACTIVE_SET` override** (this ticket used
`"cam1 cam2 cam4 cam5"` — the real default plus a temporarily-reactivated retired camera, reusing
the same reversibility mechanism the adjacent reactivation test already proves) rather than leaving
the timing assertion trivially true. The SIBLING test in `_713.rs` (the whole device-restore phase:
cam1 + painter + the secondary set) didn't need this treatment because it still had 3 boxes
(cam1/painter/cam4) after the retirement — check the ACTUAL box count each parallelism test's
default resolves to, not just whether "some test still covers it".

## The mirror-drift risk is now LOCK-TESTED (#1134), and how to verify recording-e2e.sh edits under Tier-0

Two lock-tests now enforce every Python default-mirror matches `camera-set.sh`:
`tests/harness_rig_ndi_mapping.rs`'s `default_active_set_env_var_matches_camera_set_sh_exactly`
(set-ndi-mapping.py) and `tests/harness_source_box_1134.rs`'s
`every_python_camera_active_set_default_mirror_matches_camera_set_sh_1134` (the other four). If you
change the default, these two tests go RED until every mirror is updated — so you can no longer
silently miss one. Still `grep -rn "<old literal>"` the whole repo first; the lock-tests are the net.

**Editing `recording-e2e.sh` (the static-anchor minefield, see the top-level CLAUDE.md) with cargo
Tier-0-blocked:** the local proof that no OTHER test's `.find()`/`.split()`/`.contains()` anchor
broke is a python occurrence-count sweep — `git show HEAD:scripts/recording-e2e.sh` (OLD) vs the
edited file (NEW), extract every string literal from every `tests/*.rs` that references
`recording-e2e.sh`, and flag any literal whose occurrence count went **1→0** (an anchor you
removed/renamed that another test needs) or **1→2** (an anchor you duplicated, making a `.find()`
ambiguous). A count change on a SHORT common fragment (`"strih"`, `"cam1"`) is a false positive
(it's inside an assertion-MESSAGE string, not an anchor); only a full-literal 1→0 / 1→2 matters.
Also confirm your edit site is OUTSIDE any region a test slices between two anchors (e.g. the
`AV_RESTART_GATE:-0`..`[5/8] StartRecord` block, the `# #947` dantesync-secondary region) — an
inserted line only breaks adjacency if it lands BETWEEN two anchored-adjacent lines. This sweep +
`cargo fmt --all --check` (rustfmt parses probe-gated code too) + `shellcheck` is the full local
net when `cargo test` cannot run.
