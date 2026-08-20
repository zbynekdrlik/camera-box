---
paths:
  - "scripts/lib/mv-reverify-escalate.sh"
  - "tests/harness_mv_reverify_escalate_1093.rs"
  - "tests/harness_mv_reverify_resolve_wait_1114.rs"
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
`MV_REVERIFY_OBS_RESTART_CMD`, `MV_REVERIFY_SWEEP_CMD`, `MV_REVERIFY_REOPEN_MV_CMD` (#1098),
`MV_REVERIFY_OBS_WS_WAIT_ITERS=0`,
`MV_REVERIFY_*_GAP_S=0`) let the orchestrator's decision flow run offline with zero ssh/OBS/network.
The LIVE strih-OBS restart itself is NOT exercisable at Tier-0 — flag it UNVERIFIED for the E2E run.

## After a CLEAR-then-SET receiver reset, give the fresh finder a bounded re-resolve WINDOW (#1114)

The merged WS-side `strih_mv_scenes.py reattach()` is a CLEAR-then-SET that TEARS DOWN + rebuilds
strih's NDI receiver, so its fresh DistroAV finder must RE-RESOLVE the live post-bounce burn sender
by URL — MEASURED at up to ~2 min on the live rig — far longer than the ~52s [2/8] attempt budget.
`mv_reverify_resolve_wait` (called once inside `preflight_mv_reverify`, right after the once-only
attempt-1 reattach kick, DEPLOY context only via `!= "cleanup"`) polls the SAME `frozen-camera-gate.py`
gate at `PREFLIGHT_MV_REVERIFY_RESOLVE_CADENCE_S` until a pixel changes OR a SECONDS-based wall-clock
deadline `PREFLIGHT_MV_REVERIFY_RESOLVE_SETTLE_S` (default 120s). This rides out the re-resolve in
ONE place and returns success early, removing the false "camera leg is dead" FAIL + the destructive
#1093 force-kill escalation on a genuinely-live-but-still-resolving leg.

- **Bound the WALL CLOCK, not accumulated sleeps.** Each poll iteration ALSO spends a
  frozen-camera-gate.py probe, so a `waited += cadence` counter runs ~2x past the documented window;
  use a `SECONDS`-based deadline so `RESOLVE_SETTLE_S` is a truthful bound (#1114 review 🔵-2).
- **Cleanup context stays fast:** the re-resolve window is DEPLOY-only — cleanup (`attempts=1`,
  `CONTEXT=cleanup`) must never outlast a GH-Actions cancellation grace window.
- **Anchor gotcha:** the resolve-wait uses `frozen-camera-gate.py`, never a 2nd `strih_mv_scenes.py`,
  so the #758 "reattach once" body-count invariant holds — and a comment inside
  `preflight_mv_reverify` must NEVER contain the literal `strih_mv_scenes.py` (it would become a 2nd
  body occurrence and break that count).
