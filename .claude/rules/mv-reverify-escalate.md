---
paths:
  - "scripts/lib/mv-reverify-escalate.sh"
  - "tests/harness_mv_reverify_escalate_1093.rs"
---

# Sender-bounce reverify: painter-order proof + receiver-wedge escalation (#1093)

`preflight_mv_reverify()` (recording-e2e.sh) probes strih's `NDI camN` for pixel change after each
cam-box redeploy. `mv-reverify-escalate.sh` wraps it (`mv_reverify_or_escalate`, the deploy-site
drop-in for `preflight_mv_reverify … || exit 1`). Load before touching either.

## The dual root cause of a false "no pixel change" — and the DISCRIMINATOR

Every cambox leg's picture is **cam2-painter's HDMI** (one camera → splitter → all boxes). "No
pixel change" has TWO causes, both false-aborting the run:
1. **SOURCE dark** — cam2-painter mid-restart (its `KmsPresenter` start line lags the probe).
2. **RECEIVER wedge** — strih's DistroAV never re-locks after a sender bounce (issue 1096; only an
   OBS restart cures it).

**The discriminator is strih's `genlock-fifo audit '<src>': received=` counter:** a frozen SOURCE
keeps SENDING 60 fps of identical frames so `received=` keeps ADVANCING; a wedged RECEIVER stops it.
So (a) prove the painter is PAINTING before the probe, and (b) use the `received=` delta to fire the
heavy OBS restart ONLY for the receiver case — never a dead painter.

## Placement gotchas (both learned live, #1093)

- **The painter-up wait fires ONCE, before the cam1 probe ONLY.** The ALL_CAMBOX loop runs while
  cam2-painter is DELIBERATELY stopped (`[2b/8]` `systemctl stop cam2-painter` → `[3/8]` launches the
  transient painter), so a painter-up wait there would hang/always-warn. The reverify at `[2b/8]`
  passes on analog-grabber capture noise, not the painter being live.
- **A frozen SOURCE still advances `received=`; only a wedged RECEIVER freezes it** — this is why the
  painter-up proof and the `received=` delta are complementary, not redundant: the proof avoids a
  false OBS restart during a transient painter restart (HDMI drops → no recv → looks like a wedge).

## Headless strih-OBS restart — force-kill over ssh, AHK respawns (NEVER an ssh GUI launch)

The E2E harness is HEADLESS (no win-* MCP). To restart strih OBS: **clear `.sentinel` + `Stop-Process
obs64` over ssh** (session-agnostic file-delete + process-kill, allowed per `win-ssh-vs-mcp.md`
Context B) and let strih's session-1 `NL_STARTUP.ahk` respawn ONE clean genlock obs64. **NEVER
`Start-Process obs64` over ssh** (a session-1 GUI launch lands in session 0, invisible — obs-ops
"AHK on strih"). `launch-obs-genlock.sh --force` is a PURE PLANNER that PRINTS a session-1 program
for an agent's MCP — the harness cannot run it; it runs only the kill+sentinel half.

**MANDATORY guard: never kill obs64 unless `Get-Process AutoHotkey64` is alive** — a force-kill with
no respawner leaves strih OBS DOWN (worse than `exit 1`). The PS `exit 2`/`MV_REVERIFY_NO_AHK` before
the kill; the orchestrator fails loud (obs64 untouched) on that. (The `NL_STARTUP.ahk` `SafeLoop=0`
"No"-latch, #774, is NOT detectable over ssh — accepted residual.) A force-kill reload can restore a
saved `genlock_burn=true` (obs-ops) → sweep-off strih burns after the restart.

## Two disciplines that prevent a spurious OBS force-kill

- **READ_FAIL ≠ no-recv:** a healthy 400-line OBS-log tail is NEVER empty, so an EMPTY read = the
  READ itself failed (ssh blip / log absent), NOT "no recv". Never act on absence-of-evidence (mirror
  `frozen_input_classify`'s UNKNOWN). Split the reader: `mv_reverify_probe_raw` + `_extract_received`.
- **Sample gap ≥ 2× the ~5 s audit emit cadence** (default 12 s) so emit jitter/flush can't read the
  same newest `received=` line twice = false WEDGE.

## Recovery activates the input WITHOUT the Multiview projector

The reverify's `--warm-settle 0` relies on strih's built-in Multiview projector keeping `NDI camN`
inputs active — but a fresh OBS after a force-kill may NOT reopen it (SaveProjectors). So the
post-restart re-check sets `PREFLIGHT_MV_REVERIFY_WARM_SETTLE` > 0 (new env seam on the
`--warm-settle` line) → frozen-camera-gate PREVIEW-activates the input itself (#747, Studio Mode,
restores the operator's preview) — no projector dependency, no operator-display manipulation.

Restoring the operator's own strih Multiview after a restart is now done ACTIVELY (**#1098**):
after the WS-return wait + burn sweep-off, `mv_reverify_reopen_multiview_run` re-opens a FULLSCREEN
Multiview projector over OBS-WS (`obs_phase2.py open-multiview`; monitorIndex DERIVED from
`GetMonitorList` — strih is single-monitor, so `open-projectors`' dual-monitor panel+HDMI split
does NOT apply and would fail loud on the absent HDMI monitor). WARN-only (`MV_REVERIFY_REOPEN_MV_CMD`
seam for tests), so a failed re-open never fails a recovered run. strih runs `SaveProjectors=true`
but with an EMPTY `SavedProjectors`, and a force-kill never repopulates it — so OBS's own
save/restore cannot cover this gap; the active re-open is what restores the operator's view.

## Tier-0 testing

All pure/builder pieces are unit-tested with fakes on PATH (the #833/#716 pattern) —
`tests/harness_mv_reverify_escalate_1093.rs`. The env override seams (`MV_REVERIFY_RECEIVED_CMD`,
`MV_REVERIFY_OBS_RESTART_CMD`, `MV_REVERIFY_SWEEP_CMD`, `MV_REVERIFY_OBS_WS_WAIT_ITERS=0`,
`MV_REVERIFY_*_GAP_S=0`) let the orchestrator's decision flow run offline with zero ssh/OBS/network.
The LIVE strih-OBS restart itself is NOT exercisable at Tier-0 — flag it UNVERIFIED for the E2E run.
