---
paths:
  - "scripts/mv-fps-alert-watchdog.sh"
  - "scripts/lib/mv-fps-health.sh"
  - "scripts/lib/mv-fps-preflight.sh"
  - "src/mv_audit.rs"
  - "src/bin/mv-fps-gate.rs"
  - "systemd/mv-fps-alert-watchdog.*"
  - "tests/harness_mv_fps_preflight_1091.rs"
  - "vendor/obs-studio/libobs/obs-display-budget.h"
  - "vendor/obs-studio/libobs/obs-display.c"
  - "vendor/distroav/src/ndi-burn-filter.cpp"
---

# MV-fps observability + live alarm (#771 core, #1083 live watchdog)

The Multiview render-cadence stack has THREE layers, don't conflate them:

1. **EMIT (#771):** vendored libobs `render_display()` prints `multiview-audit: monitor=N divisor=D
   rendered_fps=X target=Z floor=F cx=.. cy=..` ~every 5 s per throttleable MV projector.
   `floor = obs_multiview_floor_fps(target)` = `target − MULTIVIEW_AUDIT_FLOOR_TOLERANCE_FPS`,
   clamped ≥0, at EVERY render area. target = `canvas/effective_divisor`, the ~30fps-cell rate the
   projector actually renders at (#776). In `obs-display-budget.h`, byte-mirrored in
   `src/mv_audit.rs::mv_floor_fps`. **#1212 retired the issue-1110 render-area report-only sentinel:**
   the floor is now AREA-INDEPENDENT (strih's 4K MV holds median 30fps — mined 29.8–30.0 in every
   window — so floor 28 is achievable at 4K; the issue-1110 premise that 4K "can't hold 30fps" came
   from a single collapsed-state observation window). The bursty single-sample noise the sentinel
   papered over now lives in the GATE (layer 2, median window), not in an un-gated area class. The
   `cx=`/`cy=` fields stay on the printed line for observability. Changing the floor is a VENDORED-C
   change (CI-first-compile + lock-step anchors, per `vendored-libobs-change-safety.md`).
2. **PARSE/GATE (#771; median #1212):** `src/mv_audit.rs` (Tier-0, default features) parses the line
   + `gate_log`; `src/bin/mv-fps-gate.rs` reads a log on stdin/arg → exit `0`=all-above-floor /
   `1`=below-floor / `2`=no-samples. **#1212: `gate_log` judges the MEDIAN `rendered_fps` over each
   projector's most recent `MV_GATE_MEDIAN_WINDOW` (=12, ~60 s) samples, not one latest sample** — a
   multiview render is bursty (individual samples dip into the teens inside a median-30 window), so a
   single-sample decision false-alarmed. The median tolerates the bursts and still catches a
   SUSTAINED collapse (a lone recovered latest sample no longer hides one). Trade-off (by design): a
   genuinely fast freeze takes ~N/2 samples (~30 s) to drop the median below floor — acceptable
   because the fast-freeze class is the render-liveness watchdog's job (issue 391,
   `obs-liveness-render-signal.md`) and this gate runs behind a 2-pass confirm at ~5-min cadence.
   This is the E2E-preflight / drift-guard / watchdog decision engine — REUSE it, never re-implement
   the fps-vs-floor decision.
3. **LIVE ALARM (#1083):** `scripts/mv-fps-alert-watchdog.sh` (dev1-side systemd `--user` timer) +
   pure seam `scripts/lib/mv-fps-health.sh`. Reads each OBS box's newest log over ssh, runs
   `mv-fps-gate`, pages Discord on a SUSTAINED below-floor collapse. Ships DISABLED (supervisor
   enables — `systemd/mv-fps-alert-watchdog.README.md`). Same dev1-side framework as
   `frozen-input-alert-watchdog` / `network-reach-alert-watchdog` (`obs_watchdog_confirm` 2-pass +
   `obs_watchdog_alert_throttle` from `obs-watchdog-decision.sh`, per-box state, no-double-page
   guard reading #1001's netreach state, recovery ping, tap-blind WARN). NEVER reboots.

## The E2E-PREFLIGHT consumer (#1091) — a 4th consumer, SYNCHRONOUS gate-time, never false-aborts

`scripts/lib/mv-fps-preflight.sh` (`mv_fps_preflight_assert`) is the E2E gate's synchronous consumer,
wired into `scripts/recording-e2e.sh` via the #675 sourced-lib pattern (source + ONE call line, no
anchored line edited). Reuses `mv_fps_verdict` (layer 2/3) + the `mv-fps-gate` binary — it re-derives
NO floor and re-parses NO audit line. Learnings for the next preflight that consumes a probe-tools bin:

- **Placement is forced by `PROBE_BIN_DIR`.** `PROBE_BIN_DIR` (the CI probe-tools artifact dir holding
  `mv-fps-gate`/`frozen-camera-gate`/`render-budget-gate`) is resolved at ~l.1680 of recording-e2e.sh,
  AFTER the `[0/8]` preflight. A preflight consuming `$PROBE_BIN_DIR/<bin>` must therefore be placed
  AFTER that, not at `[0/8]`. This one sits at `[4d1/8]`, immediately BEFORE the `[4b/8]` burns-ON gate
  (issue 1261: it must measure the burns-OFF, production-shaped Multiview state — the 7-cam burns-ON MV
  collapse (issue 1260) would otherwise false-abort every run; the `[4d1/8]` label is deliberately kept
  out of alpha order to avoid breaking static anchors). It originally sat just before the `[4d/8]`
  render-budget banner (burns-ON); the anchor caution below dates from that first placement.
- **A probe-tools gate binary is BLIND on CI unless it is in BOTH resolution lists AND ci.yml (issue 1261).**
  A step calling `$PROBE_BIN_DIR/<bin>` needs `<bin>` in BOTH `recording-e2e.sh` lists — the
  `USE_PREBUILT_PROBE_DIR` presence/chmod loop (`for b in camera-box …`, ~l.2183) AND the local-build
  fallback (`cargo build --release --bin …`, ~l.2206) — AND built+uploaded by `ci.yml` (its
  default-feature gate-bin `cargo build` step + the `probe-tools-linux-amd64` upload list). Miss any one
  and the gate exec fails → `mv_fps_verdict` maps the non-zero exit to UNKNOWN → a silent report-only
  NOTE, so the gate NEVER decides on CI. `mv-fps-gate` shipped missing from BOTH `recording-e2e.sh` lists
  (present only in the `[4d1/8]` call), so it was blind from day one; `full-path-e2e.yml` does NOT set
  `USE_PREBUILT_PROBE_DIR`, so the local-build branch is the one that bit (NOTE path
  `target/release/mv-fps-gate`). Mirror how `frozen-camera-gate`/`render-budget-gate` appear in BOTH lists.
- **Anchor discipline near `[4d/8]` (a general caution — since issue 1261 this preflight sits before `[4b/8]`, not here).**
  The strih→`fi`→`[4e/8]` region around the `[4d/8]` banner is heavily anchored by `harness_render_budget_imag_report_only_888.rs`
  (which FORBIDS the strings `REPORT-ONLY`/`NOT aborting`/`SKIP_IMAG_RENDER` there) and sliced by
  `harness_imag_topology.rs` from `--box "strih=` — never add report-only WARN text inside it. Use a
  unique banner (`[4d1/8]` — NOT a substring of `[4d/8]`) and keep the call's function name off the
  source-block comment so `s.find("mv_fps_preflight_assert")` lands on the call, not the comment.
- **`set -e`-SAFE gate call (recording-e2e.sh has `set -euo pipefail`, the watchdog does NOT).** A lib
  sourced into recording-e2e.sh inherits `-e`, so a non-zero gate exit (BELOW=1/UNKNOWN=2) would ABORT
  the function mid-flight. Capture the exit with `gate_ec=0; out="$(printf … | "$gate_bin")" || gate_ec=$?`
  (the `||` list is `-e`-exempt and `$?` in its RHS is the substitution's exit) — never a bare
  `out="$(… | gate_bin)"; ec=$?`. The watchdog (`set -uo pipefail`, no `-e`) can use the bare form; a
  recording-e2e.sh consumer cannot.
- **NEVER false-abort a CI gate (the hardest constraint).** PASS→proceed; UNKNOWN (unreadable log / no
  audit line / a box not yet on the #771 genlock build / a missing gate binary)→report-only NOTE, never
  abort; BELOW→a GRACE RE-READ (one short sleep, override `MV_FPS_PREFLIGHT_REPROBE_SLEEP=0` in tests)
  and only a STILL-below-floor second read aborts (`exit 1`). A box lacking the emit must not block the
  whole fleet. Tested offline with a fake probe (`MV_FPS_PREFLIGHT_PROBE_CMD`) + a counter-driven fake
  gate under `set -euo pipefail` (proves the `-e`-safety), no ssh/rig.

## Floor calibration is DATA-FIRST — mine live `rendered_fps`, don't trust the placeholder

The `target − 2` floor (originally the #771 `canvas/2 − 2` PLACEHOLDER, retargeted to the effective
target in #776) should still be validated/adjusted from measured healthy state (the same data-first
discipline as `window-gate-tolerance-walkdown.md`). Read the live distribution:
- **imag (Linux):** `ssh newlevel@10.77.9.182 'grep multiview-audit: "$(ls -t ~/.config/obs-studio/logs/*.txt|head -1)"'`
- **strih (Windows):** via win-strih MCP Shell — `Select-String -LiteralPath <newest .txt> -Pattern
  'multiview-audit:'` (a 3 MB OBS log makes `Get-Content|Select-String` time out; `Select-String
  -LiteralPath` streams, or `-Tail N`). From an AGENT session use MCP for the Windows read, NOT ssh
  (win-ssh-vs-mcp); the headless watchdog's ssh+powershell read is the sanctioned session-agnostic path.

**The floor is `target − tol`, tight for BOTH boxes (#776 fix).** Originally the floor was `canvas/2 − tol`
— TIGHT for a divisor-2 projector (imag: canvas 60, target 30, floor 28 → ~2 fps margin) but LOOSE for a
divisor-1 projector (strih: canvas 30, target 30, floor was `30/2−2 = 13` → a 17 fps margin, so a MODERATE
strih MV collapse to ~14–27fps slipped under it UNALARMED even though the MV renders 30fps). #776 fixed
that: the floor now tracks the effective TARGET (`canvas/effective_divisor`), so BOTH boxes floor at
`target − tol = 28` and any divisor-1 collapse below 28 alarms. The 2026-08-17 (#1083) `tol=2.0`
validation still holds under the new model — imag healthy ≥29.0 vs 28, strih healthy 30 vs 28 (both
cleanly above), imag collapse ~12fps + strih 9–11fps (both below 28) — with the 2-pass confirm covering
the now-uniform ~2 fps margin. Note: strih's floor rose 13→28, so a moderate contended dip (its 8–18fps
band) now alarms too — that is the intended tightening, not a false page (never re-loosen strih's floor to
accommodate a contended DEV box; production strih runs OBS alone and holds 30). Raising `tol` further now
widens the alarm on BOTH boxes (no longer just imag); do not change it without re-mining BOTH boxes. Full
distribution: issue #1083 comment.

**strih's 4K MV is render-bound and collapses under CONTENTION, not by itself.** Its healthy 30fps
holds only when strih runs OBS alone; a non-OBS app stealing GPU/CPU (observed: an `Arena` process at
236k CPU-s) drops the 4K multiview to a wide 8–18fps band with deep 9–11fps dips. That contention drop
IS the collapse the floor catches — not a broken multiview; never "fix" strih MV by lowering its floor to
accommodate a contended dev-box reading.

## ROOT CAUSE of the 7-cam burns-ON collapse to exactly 30/4 = 7.5 fps (issue 1260)

The collapse is the #278 budget gate + #293 anti-starvation floor doing exactly what they were
designed to — the MV render genuinely exceeds the per-tick budget, so it is throttled. It is NOT a
broken code path, a vsync/present artifact, or a mis-calibrated floor. Trace (file:line, dev tip):

- The MV is NOT on its own thread. `render_displays()` (`obs-video.c:1334`) runs on the SAME single
  graphics thread, AFTER `output_frames()` (the PROGRAM, `obs-video.c:1322`), within one canvas tick.
  Issue-508's "decouple" == the #278 budget gate on the shared thread (`obs-display.c:282–351`).
- On strih's 30 fps canvas the derived `effective_divisor = 1` (`obs-display.c:298–305`), so there is
  NO cadence skipping — the MV is PURELY budget-gated: skipped when `program_elapsed + MV_ewma > 90%
  of 33.3ms (= 30ms)`. Over budget EVERY tick → skipped up to `OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS = 3`
  in a row (`obs-display-budget.h:57`,`:121`), the 4th tick FORCED → renders 1-in-4 = **exactly 7.5
  fps**. Borderline (fits ~half the ticks) → ~15–21 fps. The observed 7.4–7.6 / 13–21 are these two
  regimes, not noise.
- WHY over budget with burns ON: `obs-source.c:2990` — a source's filter chain re-runs on EVERY draw;
  there is NO within-tick cache of a filtered source's output. The MV re-draws all 7 cam sources, and
  each redraw re-runs the burn filter's FULL render — a `gs_texrender` of the 1080p source + CPU QR
  raster + `gs_texture_set_image` upload (`ndi-burn-filter.cpp:436–464`). Studio-Mode-always-on means
  program + preview + MV = 3 full burn renders per source per tick; the MV alone adds 7, pushing the
  MV `render_ewma` past the ~5–11 ms of slack (live: `program-render-audit: avg_frame_ms=18.94–24.81`
  of the 30 ms budget). Burns OFF the filter is a pass-through (`ndi-burn-filter.cpp:411`), so the MV
  is only borderline (17–21 fps, confirmed live burns-OFF, warmed instance).
- Latent correctness point in the same path: `burn_draw_qr` does `f->frame_id++` on EVERY call
  (`ndi-burn-filter.cpp:370`), so the monitoring re-renders (preview + MV) pollute the recorded
  (program) frame_id sequence.

**The fix is cost REDUCTION, not floor recalibration.** 7.5 fps is a fixable perf defect (a juddery
monitoring MV the operator watches), not a physical optical artifact — per the calibrate-vs-fix
discriminator (`calibrate-artifact-vs-fix-robustness`), a fixable collapse is FIXED, never masked, and
recalibrating floor 28 down to accept 7.5 is the gate-weakening the owner rejects ("multiview musí byť
plynulé"). Keep floor 28 (issue 1263 is the report-only stopgap + walk-back tracker).

**The fix (IMPLEMENTED, issue 1260): within-tick burn cache in the DistroAV filter.** A `video_tick`
callback (`burn_filter_videotick`) clears a per-instance `struct burn_tick_cache tick_cache`
(`vendor/distroav/src/burn-tick-cache.hpp`) once per video tick; `burn_filter_videorender` calls
`burn_tick_cache_on_render()` — the FIRST draw of the tick (always the PROGRAM, since
`output_frames()` runs before `render_displays()`) does the full prep (base `gs_texrender` +
`burn_draw_qr` which advances `frame_id`/`gen_ts` + `gs_texture_set_image`), and the later
within-tick draws (Studio-Mode preview, Multiview cells) REUSE the cached `f->texrender` +
`f->qr_texture` via the always-run sprite blit — an idiomatic per-tick render cache (stock filters
via `obs_source_process_filter_begin` already cache their target render per tick; the #404 overlay
burn opted out by resetting its texrender every draw). Stamps the recorded `frame_id` once per tick
(fixes the pollution). A prep failure calls `burn_tick_cache_abort_prepare` to re-arm the next
within-tick draw (never reuse a stale composite). **Two honesty caveats the review flagged (🟡):**
(1) the first draw of a tick is NORMALLY the program but NOT structurally always — DistroAV's preview
NDI output is an earlier `obs_add_main_render_callback`, so a program+preview cam preps in the preview
draw and the recorded program frame reuses it: pixels + `frame_id` identical, burn present, NO verdict
fault, only a bounded low-ms downward `gen_ts` bias on `latency.cam_strih`. (2) Efficacy is PARTIAL —
only the MV cells of cams ALREADY drawn on program/preview become blits; a MV-ONLY cam's first draw is
the MV projector itself, so it still preps there. Whether the reduced burn work clears the 30 ms budget
is UNPROVEN, so the rig validation below is mandatory and a residual throttle is possible (a follow-up
could decouple the prep from the draw or cut the base 4K composite cost).

**Verdict-cadence safety (verified before touching the cadence — the load-bearing check).** Reducing
`frame_id` from once-per-DRAW (recorded step ~3, `burn_render_step:3` in old fixtures) to once-per-TICK
(step ~1) is SAFE for every node: **strih/stream** use `node_render_step == 1` gap-ignore
(`src/bin/recording-verdict.rs::node_render_step` returns a hardcoded `1` — the strih burn is a
free-running render tick with an irregular step the verdict must NOT charge; forward gaps are
unconditionally ignored, so a smaller step is inert; only a delivered frame with NO readable burn or a
BACKWARD jump faults, and per-tick stamping stays monotonic → no backward jump). **imag** derives its
step from the data (`src/imag_tick_gate.rs::calibrate_burn_step` → `burn_step_contiguity`) AND its
per-frame content gate is report-only (`imag-leg-report-only.md`, imag leg flows 0/76) → it adapts and
cannot flip a verdict. **Holds** (`burn_hold.rs` MAX_HOLD_FRAMES=4) detect a repeated id on a genuinely
repeated frame; strih records 1:1 so per-tick stamping creates no false holds. If you ever change the
burn cadence again, re-verify these three before assuming green.

**Tier-0 seam + local nets.** The pure prepare-once-per-tick decision is `src/burn_tick_cache.rs`
(`BurnTickCache`, pure std, RED→GREEN via the `#771`/`#1026` standalone-rustc recipe) mirrored
byte-identically by the C header, locked by `tests/burn_tick_cache_parity.rs` (compiles the header +
drives the same event sequences, `render_budget.rs ↔ obs-display-budget.h` pattern). Local proof with
NO cargo: `rustc --test src/burn_tick_cache.rs` (5/5), `gcc -Wall -Wextra -Wconversion -Wformat=2` the
header + a truth-table selftest, the parity harness's 7 sequences C-vs-Rust, `cargo fmt --all --check`,
and the brace-delta-vs-origin structural check. The FULL filter compiles only on CI (`linux-genlock.yml`
+ `windows-genlock*.yml`) — a TYPE error surfaces there. **Still MANDATORY before it reaches the rig:**
the supervisor's genlock-fleet deploy + rig validation (burns-ON `multiview-audit rendered_fps` back
≥ 28 AND a clean E2E recording-verdict burns-ON — proving the cache didn't corrupt the recorded stamp).

**The budget-gate terms are now ON the `multiview-audit:` line (issue 1260 lane, LANDED — CI-compile
+ supervisor deploy pending).** The line was enriched (APPEND-only) with `pre_mv_ms` (window mean) +
`pre_mv_max_ms` + `mv_ewma_ms` + `budget_ms` — the #278 budget-gate terms `render_display()` already
computes each tick (`elapsed = now - graphics_frame_start_ns`, `render_ewma_ns`, 90%-of-interval),
now printed so which phase eats the budget is READABLE from the log (a `rendered_fps` below floor with
`pre_mv_ms` already near `budget_ms` = a heavy PRE-MV phase, not the MV). REPORT-ONLY: the skip
decision (`obs_display_should_skip`) is untouched; accumulated in the audit block only; three new
`obs_display` fields (`render_audit_pre_mv_sum_ns`/`_max_ns`/`render_audit_tick_count`), reset per
window. `mv_audit.rs` ignores unknown keys so every consumer is unaffected (proven by
`parse_tolerates_appended_1260_budget_fields`). Lock-step anchors updated: `tests/genlock_preload.rs`
+ `tests/mv_audit_emit.rs` + BOTH `windows-genlock*.yml` (the format string) — verify a further field
add the same way (lift-compile the `blog` under `-Wformat=2`, offline `re.sub`-squish pwsh check,
`mv_audit_emit.rs` via `rustc --test`, the #1212 substitute-import harness for `mv_audit.rs`).

### Per-cell MV instrumentation (issue 1260 lever (1), LANDED — CI-compile + supervisor deploy pending)

Because the budget-split above showed the MV PHASE itself is the variable cost (`mv_ewma_ms` 15-16
healthy vs 21-22 collapsed) and the 4K→1080p A/B falsified fill-rate as the lever, the audit line was
enriched AGAIN (still APPEND-only) with WHICH cells cost what. Five more tokens after `budget_ms=`:
`mv_cells=<N> mv_cell_ms=<window-mean of the per-render scene-cell CPU sum> mv_cell_max_ms=<window-max>
mv_top1=<sanitized-name>:<ms> mv_top2=<sanitized-name>:<ms>`.

- **What it measures.** The FRONTEND multiview draw callback (`Multiview::Render`,
  `frontend/components/Multiview.cpp` — the ONLY place the items are iterated) times EVERY draw with
  `os_gettime_ns()` (CPU wall-clock of async-texture upload + convert + draw submission) — the scene
  cells AND the preview/program big cells AND the labels (review finding: a fat PREVIEW re-render,
  Studio-Mode-always-on strih, must NOT be excluded or `mv_ewma_ms − mv_cell_ms` mis-routes the
  lever) — sums them, tracks the two fattest ITEMS, and publishes the per-render aggregate ONCE per
  render via the new libobs API `obs_display_report_multiview_cells()` (declared `obs.h`, defined
  `obs-display.c`); `render_display()` folds it into the #771 audit window and emits. `mv_top1`/
  `mv_top2` are the two fattest items of the window's WORST render (largest per-render sum), so they
  always describe the same render (a scene name, or `preview`/`program`/`labels`). `mv_cells` counts
  SCENE cells only. Names are sanitized (any byte outside printable ASCII / `=` / `:` → `_`) at the
  single libobs copy point so the whitespace-tokenized, pure-ASCII line stays parseable and never
  emits torn UTF-8 (the #1258/#1262 byte-safety invariant — the first operator string on this line);
  `-` = no draws this window.
- **HOW to read it on the rig (the whole point):** the DECISIVE signal is `mv_ewma_ms − mv_cell_ms`.
  Because `mv_cell_ms` covers EVERY timed draw, that residual is the UNtimed tail (begin/clear/region
  setup + `gs_present`/GPU-fence/flush wait) — NOT skewed by a fat preview. A SMALL `mv_cell_ms`
  under a LARGE `mv_ewma_ms` = the GPU/present/thermal path (sub-lever 1b — the RTX 2070 SUPER
  SW-thermal-slowdown finding). A `mv_cell_ms` NEAR `mv_ewma_ms` = per-item CPU draw-submission bound
  (sub-lever 1a — cut cell scenes / skip re-rendering an item with no new frame). `mv_top1`/`mv_top2`
  name the fat item to target (a browser/Ableset cell? the dead CG-bridge NDI? a 4K camera? the
  preview big cell?). The residual is still not a GPU-timestamp measurement (begin/clear/region
  setup live in it too), so it picks 1a vs 1b, not the exact GPU-execution ms (that is Approach C,
  deferred as perturbing) — but it is no longer confounded by the big-cell renders.
- **REPORT-ONLY, thread-safe by construction.** The skip decision (`obs_display_should_skip`) is
  untouched. The frontend timing + the libobs fold + the emit/reset all run on the SINGLE graphics
  thread (the draw callback executes inside `render_display()`), the same single-writer discipline as
  the other `render_audit_*` fields — NO locks. New `obs_display` fields:
  `render_audit_cell_sum_ns`/`_max_ns`/`_render_count`/`_count` + `render_audit_top{1,2}_ns` +
  `render_audit_top{1,2}_name[64]`, reset per window.
- **Deferred (not dropped):** a per-cell "had a NEW async frame this tick" count needs a source-
  internal field (`async_update_texture`) exposed via a new accessor; the per-cell CPU sum already
  bounds the upload cost, so it was left as a follow-up candidate.
- **Verify a further token/field the same way:** the pure parser add is Tier-0 (`src/mv_audit.rs`
  optional `Option<>` fields, unknown-key tolerant — RED→GREEN via the #1212 substitute-import
  rustc replica); the FRONTEND↔libobs chain is guarded by `mv_audit_emit.rs`'s
  `render_display_and_frontend_carry_the_per_cell_instrumentation_1260` test (std-only, runs via the
  standalone-rustc recipe, reads the vendored source files — it also guards that preview + program
  are timed, per the review fix); the pure name sanitizer (`obs_audit_copy_cell_name`) has a
  COMMITTED lift-test `tests/mv_audit_cell_name_sanitizer_1260.rs` (extracts the real static from
  obs-display.c, `cc -Wall -Wextra -Wconversion -Wformat=2` + a truth table incl. multibyte→`_` and
  the 63-byte cap); the format string is lock-stepped across `genlock_preload.rs` + BOTH
  `windows-genlock*.yml`. Full frontend+libobs compile is CI-only.

## Contention profile (issue 1260 hard-debug lane, 2026-09-02) — CPU-SIDE-DOMINATED, not GPU-bound, not scheduling-starved

Read-only profile of obs64 on strih during the live E2E (one ~12 s window, one OBS session, build
a0b6cac7f, burns-OFF regime; Ryzen 7 5800X 8C/16T, NVIDIA RTX 2070 SUPER). The tick-cache (c62fb9bbe)
did NOT cure the collapse because burns-OFF is ALSO below floor (14–27 fps) — the base cost, not the
burns, is the issue. Per-role CPU (`GetThreadTimes` deltas; threads NAMED via `GetThreadDescription`
+ Win32-start-address→module, NEVER by CPU rank — the top thread `video-io: video thread` is NOT the
graphics thread):

- `libobs: graphics thread` **0.534 core**; `nvwgf2umx.dll` D3D driver workers (Threaded-Optimization
  ON, 74 threads) **0.259 core** → render-submission CPU ≈ **0.793 core**; NDI receive/decode
  (`Processing.NDI.Lib.x64.dll`, ~180 threads, 7×60fps, CPU-decoded — GPU VideoDecode 0%) **4.43 core**
  (largest, but SEPARATE threads); video-io outputs (2ME PGM/PVW + aux) **1.0 core**.
- GPU 3D engine **38.7%** (obs64 28.4%, Arena 9%) → ~61% IDLE; graphics-thread ThreadState (45 samples)
  Running 40% / Wait-other-blocked 44% / **Ready-starved 0%**; processor-queue not sustained.
- `avg_frame_ms` = 26 ms whole-tick; program render lossless (renderSkipped 0). MV budget-gated.

**Attribution (honest): CPU-side-dominated** — GPU 61% idle rules out GPU-fill-bound; graphics-thread
Ready=0% + no queue backlog rules out run-queue scheduling-starvation; the render WORK is CPU-side
draw-call submission (graphics thread + its NVIDIA driver workers, GPU idle). The graphics thread's
~44% in-tick blocked time (driver-submission wait vs GPU-fence latency vs lock) is UNRESOLVED by
`GetThreadTimes` — needs an ETW (`wpr`/`xperf` CSwitch+stacks) trace to split. The pre-MV phase (strih
renders TWO mixes 2ME PGM+PVW + aux MULTIVIEW + 7×1080p60 uploads + genlock FIFO before the 4K MV) is
the budget consumer (est. ~16–17 ms of the 30 ms budget, UNMEASURED until the enrichment deploys).
**MEASURED once the enrichment deployed (2026-09-03, issue-1260 comment 5520554898): the estimate
above was INVERTED** — `pre_mv_ms` is a flat 7.5–9 ms in every state, and the MV PHASE is the
variable one: `mv_ewma_ms` 15–16 when healthy (29.7 fps) vs 21–22 when collapsed (13–16 fps). The
collapse is `pre_mv + mv_ewma` crossing the #278 `budget_ms=30` cliff; burns, the post-run decode,
GPU thermal slowdown and box contention each add the missing ~6 ms.

**LEVERS (Fable-ranked; all need a rig deploy/config = supervisor steps):** (1) instrument → deploy →
read the pre-MV/MV split → reduce the fat phase (now known to be the MV phase itself — per-cell
source render + async-texture upload + submission). **PER-CELL instrumentation LANDED (issue 1260
lane, CI-compile + supervisor deploy pending) — see the "per-cell MV instrumentation" subsection
below.** The next step after the deploy is a rig read of the new tokens to decide sub-lever (1a)
CPU-bound (cut per-cell cell scenes / re-render only cells whose source has a new frame) vs (1b)
GPU/present-wait tail (the thermal/cooling issue);
(2) MV 4K→1080p A/B — **RUN 2026-09-03, FALSIFIED**: on the idle rig, burns OFF, the same collapsed
state measured 13.1 fps / `mv_ewma` 21.6 at 3840×2160 fullscreen, 15.3 fps / 20.4 at a 1920×1080
windowed projector, 17.5 fps / 19.7 back at 4K (n=29/35/34 ticks) — quartering the pixels moved the
MV phase ≤1 ms (inside drift), so the cost is NOT fill-rate/present-bound; (3) fewer MV
cells (owner-stake); (4) move `bkshading` off strih (#808, marginal — 1.25 cores steady) + the GPU
share Arena.exe takes (9–38 % 3D); (5) MV divisor 2 — **CLOSED by the same A/B** (renders the same
cells at half size = the same ≤1 ms; it was always the "unneeded cap" the owner ruled out building
before the cheaper test — never build it). Leave NVIDIA Threaded-Optimization ON (it offloads
submission off the single graphics thread).

**The A/B recipe (reusable — no display-mode change, no OBS restart):** the MCP `FocusWindow` fails
with `SetForegroundWindow` (foreground lock), so close the fullscreen projector by posting `WM_CLOSE`
(0x0010) to its hwnd from a FILE-based P/Invoke script run via `powershell -File` (inline `Add-Type`
through the MCP shell prints nothing, a file works — `C:\camera-box\tmp\winmsg.ps1`), open the test
projector over OBS-WS `OpenVideoMixProjector {videoMixType: OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW,
projectorGeometry: <base64 Qt saveGeometry>}` (Qt 6 blob: `>IHH` magic 0x1D9D0CB/3/0, frame QRect,
normal QRect, screen 0, maximized 0, fullScreen 0, screenWidth, geometry QRect — l,t,r,b int32
big-endian; logical px = physical ÷ 1.5 at strih's 150 % DPI, so 1280×720 logical = `cx=1920
cy=1080` in the audit line), read `cx=`/`cy=` from `multiview-audit:` as the ground truth, restore
with `OpenVideoMixProjector {…MULTIVIEW, monitorIndex: 0}` after `WM_CLOSE` on the windowed hwnd,
and screenshot-verify the operator view. Each `mcp__win-strih__Shell` is a fresh process; `Start-
Process … -PassThru -RedirectStandardOutput` of a long-running child holds the MCP task past its
timeout — the child still runs (check `Get-Process`), just do not wait on it.

**Idle collapse with burns OFF and no decode is a REAL state (2026-09-03 06:18→):** after the
fleet deploy's sender restarts the MV slid from 29.7 to 14–16 fps over four minutes with no OBS-log
event, while `nvidia-smi` showed `SW Thermal Slowdown: Active` at 73–74 °C / fan 65 % / SM 1740–1875
of 2115 MHz / 69–88 W, and a cumulative **HW Thermal Slowdown 33 s** since boot (the card has hit its
hardware limit — a cooling problem: 74 °C at <90 W is far too hot for a 2070 SUPER). Read the
throttle state with `nvidia-smi --query-gpu=temperature.gpu,fan.speed,clocks.sm,power.draw,
clocks_throttle_reasons.active --format=csv` (bit 0x20 = SW thermal); a `-l 10` logger to
`C:\camera-box\tmp\gpu-log.csv` is the correlation tool. `program-render-audit: avg_frame_ms` is NOT
an independent GPU-slowness signal — it measures the whole tick, so it DROPS (25.5 → 18 ms) when the
MV skips; do not read it as "program got faster".

**Re-arm scope: the `[4d1/8]` preflight measures BURNS-OFF** (issue 1261, production-shaped), so the
re-arm condition is **burns-OFF median rendered_fps ≥ 28** (~4 ms/tick from the current ~24), NOT
burns-ON. The burns-ON 7.5 fps is a measurement-window observer-effect artifact (burns are OFF in
event/production). NEVER widen floor 28 or re-loosen the strih term (issue 1263 report-only is the
stopgap; the goal is to re-arm it strict).

## The post-E2E on-strih/on-stream decode is a CONFIRMED trigger — mitigated at BelowNormal priority (issue 1260)

The supervisor's dip-vs-decode correlation table (issue-1260 comment 5518052846, mining 22:40-01:40
local strih OBS-log data) proves the on-strih `recording-verdict.exe --extract-partial` decode
launched by `recording-e2e.sh` `[8/8a]` is one CONCRETE cause of the collapse, not just theoretical
CPU contention: **0-3 dips/10min idle** (and even **1-9/10min with burns ON but no decode running**,
TEST-mode windows) vs **89-117 dips/10min during exactly the .613/.614/.615 on-strih partial-decode
windows** — the decode's ffmpeg-backed CPU load starves the MV render thread of scheduler priority
on the SAME box, while program `lagged=0` throughout (MV is the victim, program is not — matching
the ticket title). This is a DIFFERENT mechanism from the contention profile above (which measured
DURING an E2E run's normal operation, not the post-run decode) — both compete for the same ~5ms of
program headroom (idle baseline `obs64` ~42%, ~6.8 of 16 cores, per the same comment).

**Mitigation (LANDED, cheap step before lever (b)):** `build_onbox_command` in both
`scripts/recording-verdict-on-strih.sh` and `-on-stream.sh` now sets the PowerShell host process's
`PriorityClass` to `BelowNormal` (default, env-overridable via `E2E_ONBOX_DECODE_PRIORITY` —
`Idle|BelowNormal|Normal`) BEFORE the `&`-invoked `recording-verdict.exe` call, which inherits it
(Win32 `CreateProcess` semantics). The env override is a plain shell var read by the on-box planner
scripts — it works for a manual/dev1-driven `recording-e2e.sh` run today; `full-path-e2e.yml` does
NOT currently forward `E2E_ONBOX_DECODE_PRIORITY` in its `env:` blocks, so a PR-triggered CI run
always gets the BelowNormal default (which is the correct steady state) — reaching Normal/Idle from
a PR-triggered run needs a one-line workflow `env:` addition first if that A/B is ever wanted there.
Mirrors imag-nb's issue-767 `nice -n 19` in `build_onimag_command` for the same class of problem.
Resolved by the shared, pure `onbox_decode_priority_class()` in `scripts/lib/win-ssh-exec.sh` (both
planners already source that file unconditionally). This does NOT touch `recording-e2e.sh` or add
CPU affinity (strih's OBS core-pin mask is not established, unlike imag's — the deferred next lever
if BelowNormal proves insufficient). Verify on the rig during the next E2E's post-run decode:
`Get-Process recording-verdict,ffmpeg | Select ProcessName,PriorityClass` (both should read
`BelowNormal` — `ffmpeg` is where the CPU actually burns), and re-run the dip-vs-decode correlation
over a fresh OBS log window to confirm the dip density during decode drops toward the idle
baseline. **VERIFIED 2026-09-03 (.617 PR E2E attempt 2, RUN_ID 132866162):** both planners printed
`decode priority: BelowNormal (E2E_ONBOX_DECODE_PRIORITY)`, the executed remote command carries the
`PriorityClass = "BelowNormal"` statement verbatim, and the strih on-box decode block 05:50–05:57
local produced **3 sub-25-fps MV ticks in 8 min** (per-minute 1,0,0,0,0,0,2,0) vs 89–117 per 10 min
at Normal — ~30× fewer dips. The live `Get-Process … PriorityClass` read-back was MISSED (decode
finished before the wake) — schedule it at run-start + ~28 min next time, not +37.

## Autostart-aware = reset the confirm streak on an OBS-log IDENTITY change

The audit line carries no cumulative counter, so the watchdog detects an OBS restart (autostart via
`imag-obs.service`, an operator relaunch) by the newest OBS-log FILENAME changing, and RESETS the confirm
streak (`mv_fps_restart_reset` — the #391/#799 "counter reset → never page" fail-safe). This + the 2-pass
confirm at the 5-min cadence is what keeps a fresh OBS start's seconds-long warm-up from false-paging. The
log id can contain a space (OBS names logs `YYYY-MM-DD HH-MM-SS.txt`); store/compare it as a whole quoted
string (state `logid_<box>=...` and `mv_fps_restart_reset "$prev" "$curr"` both handle the space).

## GOTCHA — a `cargo build --release` literal in a watchdog script's COMMENT trips the Tier-0 build hook when you `bash scripts/...sh`

`block-tier0-local-build.sh` scans an invoked script for a hidden heavy build and BLOCKS
`bash scripts/mv-fps-alert-watchdog.sh` if the script's TEXT contains `cargo build --release` — even
inside a build-HINT comment (`# airuleset:build-ok` is a disabled no-op here, #477). Phrase any build
hint in these scripts without the literal `cargo build --release` (e.g. "built + uploaded by CI"). To
smoke-test the watchdog against the live rig, the gate bin from `cargo test --no-run` lands at
`target/debug/mv-fps-gate` — point `MV_FPS_GATE_BIN` at it and run `--dry-run` (a compiled binary is not
a build; the block was only ever the comment).

## Verifying `src/mv_audit.rs` locally: it is NO LONGER pure-std — append a `render_budget` stub before `rustc --test` (#1212)

The `vendored-libobs-change-safety.md` recipe calls `src/mv_audit.rs` a pure-std module you can run
via `rustc --test --edition 2021 src/mv_audit.rs`. That is now STALE: one unit test
(`floor_tracks_the_effective_target_not_canvas_over_two`) does `use crate::render_budget::effective_render_divisor;`,
so a bare `rustc --test src/mv_audit.rs` fails (`crate::render_budget` does not exist as its own
crate). To get the local RED→GREEN (Tier-0 #557 blocks every compiling cargo shape, `--no-run`
included), APPEND a tiny `render_budget` stub AFTER the module and compile the concatenation:

```bash
# stub.rs:  pub mod render_budget { pub fn effective_render_divisor(configured_divisor: u32,
#             frame_interval_ns: u64) -> u32 { /* copied verbatim from src/render_budget.rs */ } }
cat src/mv_audit.rs stub.rs > /tmp/mv_standalone.rs    # APPEND, never prepend:
rustc --test --edition 2021 /tmp/mv_standalone.rs -o /tmp/mvtest && /tmp/mvtest
```

APPEND (module first, stub last), never prepend — the module's `//!` inner doc comments must stay at
the top of the crate or `rustc` errors `expected outer doc comment`. Copy `effective_render_divisor`
verbatim from `src/render_budget.rs` (it maps interval→divisor: 33.3ms→1, 16.7ms→2). The C
`obs_multiview_floor_fps()` half still lift-compiles standalone with `gcc -Wformat=2 -Wconversion`
exactly as the vendored-libobs rule describes (it takes only `target_fps` since #1212).
