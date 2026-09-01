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

**The chosen fix (issue 1260 design comment): within-tick burn composite cache in the DistroAV
filter** — stamp `frame_id`/`gen_ts` and render the base+QR composite ONCE per video tick (first draw,
which is always the program), cache the composite texture, reuse it on the within-tick preview + MV
draws, invalidate in the filter's `video_tick`. Drops the MV per-source burn cost to a sprite draw
(→ MV back under budget) AND stamps `frame_id` once per tick (fixes the pollution). Extract the pure
stamp-once-per-tick decision to a crate-root Rust seam + a C parity header (the `render_budget.rs` ↔
`obs-display-budget.h` pattern) for Tier-0 unit proof; the graphics glue is CI-first-compile
(`linux-genlock.yml`) + MANDATORY rig validation (E2E verdict clean + `multiview-audit` back ≥ 28)
before merge. **Do NOT ship it blind** — a wrong cache-invalidation → STALE recorded QR → corrupts
every future E2E verdict; a graphics-context slip → live OBS graphics-thread crash. High blast radius
on the core measurement infra; it belongs in a rig-capable follow-up with CI, not a blind worktree
push.

**The log does NOT carry the budget-gate terms — this is the profiling gap.** The `multiview-audit:`
line reports only `rendered_fps` (the outcome); `program-render-audit:` reports program `render_fps` +
whole-tick `avg_frame_ms`. Neither carries the MV `render_ewma_ns`, the program-elapsed-BEFORE-the-MV,
or the budget — so which cost term dominates (QR raster+upload vs the extra per-draw texrender vs base
composite) can only be settled by a rig experiment (or by enriching the audit emit with those terms,
which is a lock-step vendored-C change: format anchor `tests/genlock_preload.rs:2592` + the
`windows-genlock*.yml` pwsh gates; the `mv_audit.rs:159` parser is already key-based + tolerant of
added fields).

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
