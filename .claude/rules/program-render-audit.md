---
paths:
  - "src/program_render_audit.rs"
  - "vendor/obs-studio/libobs/obs-video.c"
---

# PROGRAM-render observability line (`program-render-audit:`, #1029)

The OBS log now carries **THREE independent audit-line families**, each a different stage of the
cam→canvas→scanout path — keep them straight when diagnosing a stutter/jump:

| Marker | Emitter | What it measures |
|---|---|---|
| `genlock-fifo audit '<src>'` | `obs-source.c` (per input) | receive-side NDI FIFO: received/consumed/holds/dropped_due/relocks/underruns (`src/jitter_audit.rs`) |
| `multiview-audit:` | `obs-display.c render_display()` (per throttleable projector, `divisor>1`) | MONITORING-surface render cadence (`src/mv_audit.rs`, #771) |
| `program-render-audit:` | `obs-video.c obs_graphics_thread_loop()` (once, ~5s) | **PROGRAM output** render cadence: `render_fps target_fps avg_frame_ms lagged total` (`src/program_render_audit.rs`, #1029) |

The three markers are **mutually non-substring**, so each parser family runs over one log
independently — a new marker MUST keep that property.

## Why the program line exists (the gap it fills)

`render_display()`'s `multiview-audit` covers only `divisor>1` monitoring surfaces; the PROGRAM
output (the mix that feeds the imag HDMI fullscreen projector, where the measurement burn square
lives) renders EVERY tick (`divisor<=1`) and had **no durable render-cadence signal** — renderSkipped
lived only in the transient WS GetStats, whose `activeFps` LIES during a stall (returns the canvas
fps even when the render loop is frozen, #935 / `obs-liveness-render-signal.md`). `program-render-audit`
is the HONEST, durable, offline-readable signal: `render_fps` comes from the real
`obs->video.total_frames` delta (NOT `activeFps`), `lagged` from the `obs->video.lagged_frames`
delta (= renderSkipped, `count-1` per late `video_sleep` wake).

## Using it to attribute a burn-square forward JUMP (#1029)

A jump = several held frames then a counter leap. `program_render_audit::is_render_path_jump(lagged)`
is the discriminator: `lagged>0` in a window ⇒ the RENDER path caused the jump (renderSkipped);
`lagged==0` with a clean paired `genlock-fifo audit` window ⇒ origin is downstream (display/scanout)
or the FIFO. Report-only — **no gate/floor** (the gate for this class is issue 798); the `is_render_path_jump`
discriminator never fails a run.

**Root cause of #1029's live jump = HARDWARE, not software.** The imag FIFO is clean at rest; the
jumps are renderSkipped bursts under iGPU throttle (issue 880 floor-not-holding + physical thermal
ceiling, fixed only by issue 1043 repaste/cooling; SW mitigation exhausted in issue 1040 PL1 raise).
At the mandated **3 ms live-edge latency** the forward jump is the CORRECT FIFO behavior — presenting
the freshest frame is required to hold 3 ms; presenting next-after-last to keep the burn contiguous
would ratchet latency up per stall and diverge, breaking the 3 ms imag contract. So there is no
software smoothness fix; this ticket delivered the durable MEASUREMENT, live smoothness confirmation
depends on the hardware fix.

## Changing the emit or the fields

It is the #771 observability pattern (`vendored-libobs-change-safety.md` → "Adding a NEW observability
audit line"): lift-compile the `blog()` format string under `-Wformat=2 -Wconversion` before pushing
(vendored C compiles only on CI); the C format string must stay BYTE-IDENTICAL to what
`program_render_audit.rs` parses; the window state lives on `struct obs_graphics_context`
(field names `program_render_audit_*`, DISTINCT from the obs_display `render_audit_*` #771 fields to
avoid the `genlock_preload.rs`/ymls anchor collision). `tests/program_render_audit_emit.rs` is the
std-only RED→GREEN anchor (run via `rustc --test`, the #1026 recipe).

## #1320 — the `program_render_lagged` bundle-state facet (a `lagged>0` window == a render-thread freeze)

A `program-render-audit` window with `lagged>0` is a PROGRAM render-thread freeze (renderSkipped). The
issue-1320 live root cause: `ndi_source_update` is DEFERRED to `obs_source_video_tick` for an
ASYNC_VIDEO source, so it runs on THIS same graphics thread; a CLEAR-then-SET NDI reattach whose
`ndi_source_thread_stop` → `pthread_join` waited on a ~7.5 s blocking `NDIlib_recv_destroy` froze the
render for the whole join (see `distroav-receiver-lifecycle.md` → the #1320 section for the cure).

`scripts/bundle_state_gather.py` `program_render_lagged_from_log(text)` exposes this as a report-only
bundle-state facet `(program_render_lagged, program_render_lagged_age_s)`: the MAX `lagged` over the
#1222 bounded TAIL + the in-log age (whole seconds) of the MOST RECENT window achieving that max, so a
dev1 watchdog can page on a RECENT freeze (not one that scrolled out of the tail). Omit-when-empty
contract: `"0"` (render telemetry live, no freeze) is a truthy string and is KEPT — distinct from `""`
(no `program-render-audit` line at all → dropped → UNKNOWN downstream, never a fabricated 0). Wired
through `build_bundle_state` + `bundle-state-server.py` so it appears on `:8899/bundle-state.json`;
pytest `tests/python/test_program_render_lagged_gather_1320.py`. The dev1 render-freeze watchdog that
pages on this facet + on `relock_bursts>=1` (issue 1318's `summarize_relock_bursts`, ported to the
gather as `relock_bursts_from_log`) is SHIPPED — `scripts/render-freeze-alert-watchdog.sh` +
`scripts/render_freeze_decision.py`, production-critical time-bucketed re-ping, ships DISABLED. See
`.claude/rules/render-freeze-watchdog.md` (RENDER arm's magnitude-floor-vs-relaunch discriminator,
the `render-freeze` fleet facet, the relock parity mirror).
