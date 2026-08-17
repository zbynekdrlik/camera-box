---
paths:
  - "scripts/cg-bridge-alert-watchdog.sh"
  - "scripts/lib/cg-bridge-health.sh"
  - "tests/python/test_cg_bridge_alert_watchdog_1006.py"
  - "tests/python/test_obs_phase2_republish_black_1006.py"
  - "systemd/cg-bridge-alert-watchdog.service"
  - "systemd/cg-bridge-alert-watchdog.timer"
---

# CG-bridge (Spout republish) black-on-air detection — DIFFERENTIAL, never blanket (#1006)

strih's `CG bridge` scene (one item, `spout CG`, fed by Resolume Arena's "CG_Bridge light"
composition over Spout) renders fully BLACK on air when Arena's composition output is black while
its own upstream NDI feed is live — no process/liveness check catches it (Arena up, plugin up,
sender registered). Root cause is INTERNAL to Resolume Arena (a third-party app); camera-box can
only DETECT it (issue 941 rejected rewiring Spout→NDI, so the fix must keep Spout). Same silent-
black-on-air class as #721/#860 — only reading the rendered pixels catches it.

## Do NOT build a blanket "every production scene renders non-black" gate — it false-fails

Measured live on a HEALTHY rig (2026-08-17): `CG bridge` AND `Ableset` (lyrics) are BOTH
legitimately black at idle (no lower-third / no lyric currently triggered), while program (`Cam 1`)
is live. Overlay/republish scenes being black is the NORMAL idle state. A "every scene must be
non-black" check fails whenever no overlay is currently on air — unusable, disabled within a day.

## The correct signal: DIFFERENTIAL (upstream-live-but-republish-black)

A republish is a FAULT only when its live upstream REFERENCE is delivering content but the republish
shows black — the exact 2026-08-06 signature (direct NDI `cg` / `RESOLUME-SNV (cg-obs)` peak=180
while `spout CG` peak=0). Both-black = legitimately idle = OK; both-live = OK; reference-black =
nothing-to-republish = IDLE (never an alarm). The live upstream IS the schedule-free "content is
expected now" signal, so no on-air-window config is needed.

- Pure decision: `obs_phase2._republish_black_verdict(ref_max, ref_mean, subj_max, subj_mean,
  min_mean)` → OK/FAULT/IDLE/UNKNOWN. Default `min_mean=0` = peak-only (the ticket's semantics).
- Read-only probe: `obs_phase2.py republish-black-check --reference <ndi> --subject <spout>` — never
  switches program (Studio-Mode preview untouched); exit 3=FAULT, 4=UNKNOWN, 0=OK|IDLE.
- Default pair: `spout CG` (subject) ↔ `cg` (reference). `spout moderatori` has NO external-NDI
  reference (its content originates inside Arena), so it is OUT of the differential's scope.

## The alert follows the dev1-side sibling-watchdog convention exactly — reuse, don't reinvent

`scripts/cg-bridge-alert-watchdog.sh` mirrors `scripts/optical-chain-alert-watchdog.sh`: `set -uo
pipefail` (survive every per-pass failure), source `scripts/lib/obs-watchdog-decision.sh` for the
SHARED `obs_watchdog_confirm` (2-pass) + `obs_watchdog_alert_throttle`, page via `airuleset.py
notify` from dev1 (dev1 has the checkout + Discord creds). `scripts/lib/cg-bridge-health.sh` is a
source-only pure classifier (rc→incident string) carrying `# airuleset:script-ok` because a `set
-euo pipefail` in a sourced lib leaks `-e` into the caller (see `ci-testing-gotchas.md`). A
connectivity/UNKNOWN pass = "nothing to decide", NEVER a false alert, and clears the throttle state.
Ships DISABLED (`systemd/cg-bridge-alert-watchdog.timer` is enabled by no installer — the operator
runs `systemctl --user enable --now` after review, same as #732/#794/#860).

## Inspecting Arena's internal state is a READ-ONLY MCP screenshot, never a control op

To see WHY Arena publishes black (composition/layer/clip state), take a read-only `win-strih`
MCP `Snapshot` — the composition monitor shows the CG-bridge output directly (2026-08-17: title
"CG_Bridge light (1920×1080)", Composition Monitor fully black, Group 7 "moderatori" live). NEVER
click/focus/type in Arena or restart Arena/OBS from a worktree lane — the fix is Arena-side and
operator-driven.

## Testing is Tier-0 pytest (no cargo, no rig)

The pure verdict + subcommand: native pytest (`test_obs_phase2_republish_black_1006.py`). The bash
watchdog: a pytest that SHELLS OUT to bash (`test_cg_bridge_alert_watchdog_1006.py`) for the
classifier + `--help`/`--dry-run`/unknown-arg + the ships-disabled contract (assert no
`systemctl enable` of the timer in any installer, excluding doc comments). Both run under CI's
`pytest tests/python` job — no Rust harness needed for this shell script.
