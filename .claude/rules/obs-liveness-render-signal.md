---
paths:
  - "src/obs_watchdog.rs"
  - "src/bin/obs-watchdog-gate.rs"
  - "scripts/obs-liveness-probe.py"
  - "scripts/obs-liveness-watchdog.sh"
---

# obs-liveness watchdog (#391) — GetStats `activeFps` LIES during a render stall (#935)

## The trap: `activeFps` is the CONFIGURED canvas fps, not the render-loop rate

OBS WebSocket `GetStats.activeFps` returns the configured video/canvas fps — it keeps reporting the
target (e.g. **30.0**) even when the graphics render thread has FULLY stalled. Proven live at the
#935 00:35 strih freeze: `renderTotalFrames` delta was **0 over 3 s** while `activeFps` read 30.0
and the WS thread answered. So `activeFps` is NOT a render-liveness signal on its own.

Two compounding effects make a frozen render loop serialize as a HEALTHY WS-only sample:
- `active_fps` filled from the lying `activeFps` gauge → the `FPS_ZERO` check (`active_fps < 1.0`)
  never fires.
- `render_skipped_frac = r_skip / r_tot if r_tot > 0 else 0.0` → a frozen loop (`r_tot == 0`) reads
  a perfectly healthy **0.0** skip fraction.

## The true render-liveness signal: `renderTotalFrames` advancement (`render_advanced`)

`ObsHealthSample.render_advanced: Option<bool>` = "did `renderTotalFrames` advance at all over the
GetStats window": `Some(true)` = advancing, `Some(false)` = full stall (pages FPS-ZERO), `None` =
counter reset (`r_tot < 0`, OBS restarted between snapshots) / WS unreachable / not sampled → NEVER
pages (fail-safe). `classify()` checks `render_advanced == Some(false)` BEFORE the legacy
`active_fps` fallback. The probe computes it from the delta it already gathers:
`render_advanced = (r_tot > 0) if r_tot >= 0 else None`. It works on the plain **dev1 WS-only pass**
(no process/MCP signals) — which is exactly how `obs-liveness-watchdog.sh` invokes the probe
(`--box ... --window-s`, no `--process-state`/`--log-audit`).

## Adding a signal to the WS-only sample path — the seam list

A new WS-derived signal must be threaded through THREE places or it silently does nothing:
1. `scripts/obs-liveness-probe.py::_render_sample` — compute + add to the returned dict.
2. `src/bin/obs-watchdog-gate.rs::sample_from_json` — parse via `opt_bool`/`opt_f64` (absent =
   `None`, backward-compatible JSON — never make it required) + update the doc-comment example JSON.
3. `src/obs_watchdog.rs` — add the `ObsHealthSample` field + the `classify()` branch. Every FULL
   struct literal in the tests needs the field (the `..healthy_ws_only(...)` / `..Default::default()`
   spreads inherit it automatically). `classify()` is Tier-0 unit-tested; the probe pure helpers are
   pytest-tested (`tests/python/test_obs_liveness_probe.py`) — pytest runs locally, cargo tests do
   NOT (camera-box Tier-0 bans `# airuleset:build-ok`).

Reuse the existing `FpsZero` verdict for a render stall — its meaning is already "render loop stalled
while WS alive"; do NOT add a new verdict (every consumer — `obs-liveness-watchdog.sh`,
`obs-self-heal`, `obs-self-heal-gate` — matches on the verdict name).

## What #391 does and does NOT cover (post-#935)

Catches (once the supervisor installs + live-verifies + ENABLES it — it ships DISABLED): a strih/
stream render-loop stall (`render_advanced`), a wedged/pegged obs64 (process signals, agent-sampled),
WS-dead, wrong obs64 count, DXGI device-lost (#89). Does NOT catch a frozen strih cambox INPUT while
strih keeps compositing (a DistroAV receiver freeze) — that is a separate per-input `received=` watch
scope (the #935 forensics' Class A; frozen-input-watchdog #1052 taps only the STREAM box's
`NDI 2ME PGM`).
