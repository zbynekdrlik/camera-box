---
paths:
  - "scripts/mv-fps-alert-watchdog.sh"
  - "scripts/lib/mv-fps-health.sh"
  - "src/mv_audit.rs"
  - "src/bin/mv-fps-gate.rs"
  - "systemd/mv-fps-alert-watchdog.*"
  - "vendor/obs-studio/libobs/obs-display-budget.h"
---

# MV-fps observability + live alarm (#771 core, #1083 live watchdog)

The Multiview render-cadence stack has THREE layers, don't conflate them:

1. **EMIT (#771):** vendored libobs `render_display()` prints `multiview-audit: monitor=N divisor=D
   rendered_fps=X target=Z floor=F cx=.. cy=..` ~every 5 s per throttleable MV projector.
   `floor = obs_multiview_floor_fps(canvas) = canvas/2 − MULTIVIEW_AUDIT_FLOOR_TOLERANCE_FPS` in
   `obs-display-budget.h`, byte-mirrored in `src/mv_audit.rs::mv_floor_fps`. Changing the floor is a
   VENDORED-C change (CI-first-compile + lock-step anchors, per `vendored-libobs-change-safety.md`).
2. **PARSE/GATE (#771):** `src/mv_audit.rs` (Tier-0, default features) parses the line + `gate_log`;
   `src/bin/mv-fps-gate.rs` reads a log on stdin/arg → exit `0`=all-above-floor / `1`=below-floor /
   `2`=no-samples. This is the E2E-preflight / drift-guard / watchdog decision engine — REUSE it,
   never re-implement the fps-vs-floor decision.
3. **LIVE ALARM (#1083):** `scripts/mv-fps-alert-watchdog.sh` (dev1-side systemd `--user` timer) +
   pure seam `scripts/lib/mv-fps-health.sh`. Reads each OBS box's newest log over ssh, runs
   `mv-fps-gate`, pages Discord on a SUSTAINED below-floor collapse. Ships DISABLED (supervisor
   enables — `systemd/mv-fps-alert-watchdog.README.md`). Same dev1-side framework as
   `frozen-input-alert-watchdog` / `network-reach-alert-watchdog` (`obs_watchdog_confirm` 2-pass +
   `obs_watchdog_alert_throttle` from `obs-watchdog-decision.sh`, per-box state, no-double-page
   guard reading #1001's netreach state, recovery ping, tap-blind WARN). NEVER reboots.

## Floor calibration is DATA-FIRST — mine live `rendered_fps`, don't trust the placeholder

The `canvas/2 − 2` floor was a #771 PLACEHOLDER; validate/adjust it from measured healthy state
(the same data-first discipline as `window-gate-tolerance-walkdown.md`). Read the live distribution:
- **imag (Linux):** `ssh newlevel@10.77.9.182 'grep multiview-audit: "$(ls -t ~/.config/obs-studio/logs/*.txt|head -1)"'`
- **strih (Windows):** via win-strih MCP Shell — `Select-String -LiteralPath <newest .txt> -Pattern
  'multiview-audit:'` (a 3 MB OBS log makes `Get-Content|Select-String` time out; `Select-String
  -LiteralPath` streams, or `-Tail N`). From an AGENT session use MCP for the Windows read, NOT ssh
  (win-ssh-vs-mcp); the headless watchdog's ssh+powershell read is the sanctioned session-agnostic path.

**The `canvas/2 − tol` model is TIGHT for a throttled (divisor≥2) projector, LOOSE for divisor=1.**
A divisor-2 projector runs at canvas/2 = its target, so the floor sits `tol` below healthy (imag: canvas
60, healthy ~30, floor 28 → ~1–2 fps margin). A divisor-1 projector runs at canvas, floor = canvas/2−tol
(strih: canvas 30, healthy 30, floor 13 → 17 fps margin). Measured 2026-08-17 (#1083): both healthy
states sit cleanly above floor and BOTH observed collapses fall below it, so `tol=2.0` was VALIDATED
(imag healthy ≥29.0 vs floor 28; strih 30 vs floor 13; imag collapse ~12fps, strih 9–11fps — all caught;
the 2-pass confirm covers the tight imag margin). Raising `tol` helps imag but lowers strih's floor under
its own deep dips — do not change it without re-mining BOTH boxes. Full distribution: issue #1083 comment.

**strih's 4K MV is render-bound and collapses under CONTENTION, not by itself.** Its healthy 30fps
holds only when strih runs OBS alone; a non-OBS app stealing GPU/CPU (observed: an `Arena` process at
236k CPU-s) drops the 4K multiview to a wide 8–18fps band with deep 9–11fps dips. That contention drop
IS the collapse the floor catches — not a broken multiview; never "fix" strih MV by lowering its floor to
accommodate a contended dev-box reading.

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
