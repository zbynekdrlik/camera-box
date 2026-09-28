---
paths:
  - "scripts/obs_phase2.py"
  - "scripts/strih_scenes.py"
  - "scripts/imag_scenes.py"
  - "scripts/set-ndi-mapping.py"
  - "scripts/apply_latency_pins.py"
  - "scripts/glk_wire.py"
  - "vendor/distroav/src/ndi-source.cpp"
---

# OBS-WS writes to a genlocked NDI input: the lockdown rewrites them, and the read-back lies (issue 1242)

Two facts to check before any harness or tool changes a DistroAV `ndi_source` setting over
obs-websocket. Both were missed by the first design of the (since removed, 28.9.2026) E2E twin hold
(Design-question 5859304945).

## 1. The #150 genlock lockdown overwrites every forced key on EVERY update

- `ndi_source_update` runs `force_genlock_certified_settings(settings)` whenever `genlock_fifo` is on
  (`vendor/distroav/src/ndi-source.cpp`, the `genlock_lockdown` block).
- It rewrites the forced keys: `ndi_sync`, `ndi_behavior`, `ndi_bw_mode`, `latency`, `timeout`, the
  two `yuv_*`, `ndi_recv_hw_accel`, `ndi_framesync`, `ndi_fix_alpha_blending`, `ptz`.
- For a `genlock_monitor` source it then forces `ndi_bw_mode` to LOWEST.
- It writes into the SAVED settings, so a WS write of a forced key on a genlocked input silently
  comes back to the certified value. Only the whitelist is operator-settable: `ndi_source_name`,
  `genlock_latency_ms_src`, `genlock_burn`, the `genlock_monitor` role flag, `ndi_audio` (issue
  1295), and `genlock_fifo` itself.
- To change a forced key, step OUTSIDE the lockdown in the same write (`genlock_fifo: false`), e.g.
  `{"genlock_fifo": false, "ndi_bw_mode": 2}` for audio-only; turning genlock back on puts the
  certified value back by itself.
- `genlock_fifo` DEFAULTS to true in this build (`ndi_source_getdefaults`). An absent key therefore
  means genlocked, so compare EFFECTIVE settings (type defaults under the explicit ones).

## 2. OBS applies the input update one VIDEO TICK after the WS overlay

- `obs_source_update` (`vendor/obs-studio/libobs/obs-source.c`) applies the overlay to
  `context.settings` at once. For a video source it only bumps `defer_update_count`; `info.update`
  runs on the next video tick.
- So a `GetInputSettings` right after `SetInputSettings` returns the OVERLAY, even when the deferred
  update (the lockdown above) is about to revert it: a false PASS.
- A read-back of anything the plugin's update can change must be a SETTLE poll:
  - start a few render ticks after the write;
  - need two consecutive matching reads;
  - count a request error (no `inputSettings` key under `ignore_err`) as a mismatch, never as the
    type default;
  - bound the wait.
- The one implementation of that poll lived in the E2E twin hold (`e2e_bandwidth_hold.await_settled`),
  removed with the strih low-bandwidth mechanism on 28.9.2026 (issue 1242) -- a future tool that
  writes a plugin-rewritten key needs its own settle poll (git history has the tested one).
  A whitelist key (a name, a latency pin) is never rewritten, so its immediate read-back stays
  honest; `reenforce_ndi_name` is correct as it is.
