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

**LEVERS (Fable-ranked; all need a rig deploy/config = supervisor steps):** (1) instrument → deploy →
read the pre-MV/MV split → reduce the fat pre-MV phase (the only path to ≥28 if the model holds);
(2) MV 4K→1080p A/B — near-free, LOW expected benefit (submission is resolution-INDEPENDENT, GPU
idle) but FALSIFIES the CPU-side story if it fixes it — run FIRST as an experiment; (3) fewer MV
cells (owner-stake); (4) move `bkshading` off strih (#808, marginal); (5) MV divisor 2 — FLAGGED:
structurally FAILS ≥28 (caps MV at 15 fps) + reverses #776, an owner call not a lever. Leave NVIDIA
Threaded-Optimization ON (it offloads submission off the single graphics thread).

**Re-arm scope: the `[4d1/8]` preflight measures BURNS-OFF** (issue 1261, production-shaped), so the
re-arm condition is **burns-OFF median rendered_fps ≥ 28** (~4 ms/tick from the current ~24), NOT
burns-ON. The burns-ON 7.5 fps is a measurement-window observer-effect artifact (burns are OFF in
event/production). NEVER widen floor 28 or re-loosen the strih term (issue 1263 report-only is the
stopgap; the goal is to re-arm it strict).

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
