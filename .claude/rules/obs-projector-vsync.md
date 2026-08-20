---
paths:
  - "vendor/obs-studio/libobs/obs-display.c"
  - "vendor/obs-studio/libobs-opengl/gl-x11-egl.c"
  - "vendor/obs-studio/libobs-opengl/gl-wayland-egl.c"
  - "vendor/obs-studio/frontend/widgets/OBSProjector.cpp"
  - "tests/gl_egl_present_vsync_1107.rs"
  - "tests/gl_egl_present_vsync_observability_1146.rs"
---

# imag HDMI tearing → OBS projector present-vsync (issue-1107 fix + issue-1146 observability)

**The tear + why a compositor is the WRONG cure.** imag-nb (Intel iGPU, Linux/X11, openbox,
NO compositor, NO TearFree — modesetting driver, issue-841) drives TWO 60 Hz outputs on
separate crystals: a fullscreen **Program** projector on HDMI-1 (0,0 1920x1080) + a
**Multiview** projector on eDP-1. A GL/composited present vsyncs to only ONE CRTC, so the HDMI
output BEATS the two clocks — clean while phases align, a walking torn line when they drift
("raz dobre raz zle"). A picom vsync compositor was tried and **REVERTED** (issue-1146,
2026-08-20): it cost **21.57 % render skips** on the 25 W box (real lost output frames in
record+NDI+projection), vs 0.00 % with it off. Do NOT reach for a compositor here — the owner
ruled render budget wins.

**The deployed cure = OBS's OWN per-display EGL vsync (issue-1107, on main).** With no
compositor the fullscreen HDMI window is unredirected, so OBS's own `eglSwapInterval(1)` on
that surface directly governs the HDMI scanout present (tear-free), synced to HDMI vblank. The
chain, targeted at EXACTLY the fullscreen non-multiview (Program) projector:
`OBSProjector.cpp` marks `obs_display_set_vsync(GetDisplay(), true)` only for
`savedMonitor > -1 && !isMultiview` (+ re-arm in `OpenFullScreenProjector`, clear in
`OpenWindowedProjector`) → `render_display()` per-tick `gs_present_vsync(display->vsync)` →
`gl-x11-egl.c` `eglSwapInterval(edisplay, device->present_vsync ? 1 : 0)`.

- **NEVER arm vsync on BOTH projectors.** `obs_graphics_thread` renders all displays SERIALLY
  in one thread; a blocking vsync present on the Program projector is fine (one frame period,
  measured `program-render-audit: 60fps lagged=0`), but arming the MV (eDP) too would stack a
  SECOND blocking present per tick on a DIFFERENT, non-synchronous CRTC and throttle the 60 fps
  render to the slower clock. MV tear on the internal panel is acceptable operator monitoring.
- **Linux/EGL-only. No Windows pwsh mirror needed.** The fix lives in `gl-x11-egl.c` /
  `gl-wayland-egl.c`; the Windows renderer (strih/stream) is libobs-d3d11 (not WGL). The
  optional `device_present_set_vsync` export is NULL on D3D11/Metal → `gs_present_vsync` is a
  no-op there → the Windows present path is byte-identical (pinned by the issue-1107 test).

**The armed state IS observable (issue-1146).** `obs_display_set_vsync()` (libobs, the single
source of truth, called ONLY from OBSProjector.cpp) emits a ONE-SHOT-ON-CHANGE line:
`projector-vsync: present-vsync ARMED|cleared (GL/EGL swap interval N; no-op on D3D11)`. The
Program projector logs one `ARMED` at open; the MV (flag stays false) logs nothing; the hot
per-tick `gs_present_vsync()` path is untouched. Grep the OBS log for
`projector-vsync: present-vsync ARMED` to confirm the mechanism is engaged. Log at the
`obs_display` level (NOT `device_present_set_vsync`) — the device flag flip-flops true/false
per-display within one tick, so a device-level one-shot would spam every frame.

**Verification honesty.** The marker proves the mechanism is ARMED. Objective SCANOUT
tear-measurement needs the physical HDMI tap (issue-781, `ops-wait` hardware) — until it
exists, NEVER claim "tearing solved", only "present-vsync armed" + the owner is the sensor.
Follow-up: wire a drift-guard `--check-imag` / E2E `[0/8]` reader that greps the OBS log for
`projector-vsync: present-vsync ARMED` once the marker is deployed and its exact shape is
confirmed live on imag.

**Anchor tests (Tier-0 vendored-source, `squish()` + `contains()`, no probe/GPU).**
`tests/gl_egl_present_vsync_1107.rs` pins the whole EGL vsync chain; the sibling
`tests/gl_egl_present_vsync_observability_1146.rs` pins the marker AND (belt-and-suspenders) the
issue-1107 EGL read, so a vendored `git subtree pull` can't silently drop either the fix or its
observability. Edit obs-display.c's `obs_display_set_vsync()` carefully: keep the substring
`display->vsync = vsync;` intact (the 1107 test asserts it).
