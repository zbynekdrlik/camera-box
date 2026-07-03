# imag-nb topology change — strih→30fps, new 60fps IMAG box, full-topology E2E

**Date:** 2026-07-03
**Root request:** (1) strih OBS drops to 30fps — it is the cut-to-stream box only; (2) a NEW
Linux notebook **imag-nb (imag-pc, 10.77.9.182)** becomes the low-latency 60fps IMAG cutter of
all 6 NDI cameras, with the OBS **program out shown as a fullscreen projector on its HDMI
monitor**; (3) the final E2E tests must cover the whole new topology in every direction —
zero frame loss, no dropouts.

## Target topology

```
6× cam box (NDI 1080p60, CAM1..CAM6 (usb), genlock emit CAMERA_BOX_GENLOCK_FPS=60)
   ├─→ imag-nb OBS (Linux, 1080p60, low-latency IMAG)  → HDMI fullscreen program projector
   └─→ strih  OBS (Windows, 1080p30, cut-to-stream)    → stream OBS (1080p30) → broadcast
```

- Cam boxes keep emitting 60fps (imag-nb needs it); strih decimates 60→30 at ingest.
- The 60→30 "beat" (every-other-frame) hop MOVES from strih→stream to **cam→strih**.
- strih→stream becomes 30→30 (step=1 burn pairing).
- cam→imag is 60→60 (step=1 optical tick pairing — no beat, no decimation ambiguity).

## imag-nb hardware (recon 2026-07-03)

Ubuntu 24.04.2, X11/GNOME (Xorg on tty2), i5-13450HX (16 threads), 15 GB RAM, 432 GB free.
Intel iGPU drives ALL display connectors incl. both HDMI (verified: every
`/sys/class/drm/card1-*` connector belongs to 0000:00:02.0 / 0x8086) — the projector does NOT
need the NVIDIA dGPU. NVIDIA GPU (2dd8) present but driver 580 does not support it
(`nvidia-smi: No devices were found`; `nvidia-driver-595` is the recommended fix) — OPTIONAL,
not a blocker; OBS renders on Intel, recordings use VAAPI/x264 (16 CPU threads are ample).
Network: USB-ethernet `enx6c1ff773c91f`, currently DHCP 10.77.9.182/23 → make STATIC .182.
avahi active (NDI discovery works).

## Phase 1 — provision imag-nb (one-shot `scripts/setup-imag.sh`)

Idempotent, fail-loud (`set -euo pipefail`), the same one-shot discipline as
`setup-device.sh` (#450). Steps:

1. Static IP 10.77.9.182/23 via netplan (keep gw/DNS from current lease).
2. Power: `performance` governor persisted (cpu-performance.service + rc.local pattern from
   the cam fleet), `HandleLidSwitch=ignore` + `HandlePowerKey=ignore`, sleep/suspend targets
   masked, GNOME idle/blank/screensaver off (`gsettings` as the desktop user).
3. NDI runtime: libndi 6.3.2 copied from cam1 into `/usr/lib/ndi` + `/etc/ld.so.conf.d/ndi.conf`
   + ldconfig (setup-device.sh STEP 4 recipe verbatim).
4. OBS: `ppa:obsproject/obs-studio` (32.x — same major as the genlock base 32.1.2) +
   DistroAV Linux .deb (stock, latest 6.x) as the BOOTSTRAP plugin. Phase 3 swaps in our
   genlock build; scenes/profile survive the swap.
5. OBS profile: canvas+output 1920×1080@60, WebSocket :4455 **no auth** (= stream box
   convention; all our WS scripts take `--host`), Studio Mode ON, `SaveProjectors=true`.
6. Scene seeding over WS (obs_phase2.py CreateScene/CreateInput pattern, new
   `scripts/imag_scenes.py`): scenes `Cam 1`..`Cam 6`, inputs `NDI CAM1`..`NDI CAM6` →
   `CAMx (usb)` 1:1, latency mode LOW on every NDI source, NDI audio NOT routed (IMAG is
   video-only; avoids echo into the hall).
7. Fullscreen PROGRAM projector on the HDMI monitor (WS `OpenVideoMixProjector`
   videoMixType=PROGRAM on the HDMI monitorIndex); `SaveProjectors=true` restores it on OBS
   restart. HDMI monitor is currently unplugged — the projector call is re-runnable
   (`setup-imag.sh --projector`) once the user connects it.
8. Desktop: OBS .desktop icon on the GNOME desktop + autostart entry
   (`~/.config/autostart/obs.desktop`, `obs --startprojector` equivalent handled by
   SaveProjectors) so a reboot lands directly in cutting-ready state.
9. targets.md: new "Linux OBS Targets" section with imag-nb.

## Phase 2 — strih to 30fps + re-pin everything

1. Live switch over WS: `SetVideoSettings fpsNumerator=30` (strih idle-verified; profile
   persists it).
2. `vendor/README.md`: `output_fps_strih` 60→30 (+ rationale rewrite: strih no longer the
   IMAG renderer; the 60fps role moved to imag-nb), `output_fps_stream` rationale (no 60→30
   decimation on strih→stream any more).
3. `scripts/drift-guard.sh`: strih pinned fps 60→30 (host-keyed switch, ~lines 851/1050/1160).
4. `scripts/recording-e2e.sh`: `--strih-emit-fps` semantics — the RIG-PINNED topology
   constants become cam=60, strih=30, stream=30, imag=60 (comment block lines 83–98).
5. `scripts/render-budget-gate.py` call sites + usage example: `strih=...:30`.
6. Docs/skills text sweep: `.claude/skills/genlock` (60fps role → imag-nb), `obs-ops`
   (scope note), `e2e` (decimation math moves to cam→strih), `camera-set.sh`/`setup-device.sh`
   rationale comments. Burns: strih keeps 911002, stream keeps 911004, **911003 reserved for
   imag**. The #399 4-distinct NDI mapping on strih stays (strih is still the broadcast cut).

## Phase 3 — genlock OBS+DistroAV Linux build (parity)

New `.github/workflows/linux-genlock.yml` (ubuntu runner): build vendored OBS 32.1.2 +
genlock-patched DistroAV for Linux (the patches carry `#ifdef _WIN32` guards in
ndi-burn-filter.cpp / ndi-source.cpp / curl-helper.h — the Linux fallback paths must be
compile-verified, that IS the first task of this phase). Artifacts: obs deb/tarball +
distroav.so. Deploy to imag-nb (Linux plugin path), giving imag the latency knob (3 ms
floor), ts-align, render tick, and the burn filter. drift-guard learns a THIRD host case:
imag read over ssh (log + `obs --version` + plugin path) — simpler than the Windows MCP path.
Until Phase 3 lands, imag runs stock DistroAV (bootstrap) and the E2E imag gate uses the
optical path only.

## Phase 4 — E2E zero-loss for the full topology

**recording-verdict changes (Tier-0 testable):**

1. `--imag <rec>` CLI flag + 6th NodeSpec entry.
2. NEW burn-less zero-loss gate: pure fn (sibling of `burn_contiguity`) running the same
   first..=last contiguity check over the cam2 OPTICAL tick ids (`RecordingFrame::tick`) at
   step=1. Unlike `cam_strih_assessment` (claims_zero_loss=false BECAUSE of the 60→30 beat),
   imag's 60fps capture of the 60Hz painter has NO beat → tick contiguity IS a zero-loss
   proof. `NodeVerdict::is_zero()` accepts the optical criterion when the node intentionally
   has no burn id.
3. `node_capture_fps` (src/recording_span_gate.rs:46): third rate slot — imag=60 (span floor
   #373 computed against imag's own rate).
4. Hop re-model: cam→strih inherits the 60→30 beat handling (existing logic, re-pointed);
   strih→stream becomes step=1.
5. `--switch-schedule` sweep: parallel per-window contiguity over imag_frames with the same
   schedule + guard_ns (imag recording is invisible to the stream-anchored sweep today).
6. Phase 3 follow-up: imag digital burn 911003 (bottom-center-left free corner) + colour-gate
   burn-dodge geometry re-check.

**recording-e2e.sh:** record imag program over WS (StartRecord/StopRecord), pull recording
over scp (Linux→Linux), decode with the LINUX probe-tools verdict binary ON imag itself
(16-thread CPU; no Windows-MCP detach dance needed), merge as a third partial. Reachability +
render-budget (`--box imag=10.77.9.182:60`) gates add imag. DanteSync NOT required on imag
for v1: the optical tick gate is self-contained (single-recording contiguity, no cross-clock
pairing).

**Acceptance (the user's hard bar, extended):** every node's id sequence contiguous — cam→imag
(optical ticks 60fps), cam→strih (60→30), strih→stream (30→30) — ≥300 s, restart-survival
(OBS restart + PC restart on all three boxes), pixels shown for any anomaly.

## Out of scope (tracked separately)

- NVIDIA driver 595 on imag-nb (optional NVENC) — only if VAAPI/x264 recording proves
  insufficient.
- A/V-sync offset re-measure (#420 mandate) — after strih fps switch (topology change shifts
  video latency), rides the existing av-sync skill recipe.
- rig-mode.sh painter fb0 regression (painter alive, not writing fb0 — hit 2026-07-03).

## Issue map

Phase 1 = #A (setup-imag.sh + scenes + projector), Phase 2 = #B (strih 30 re-pin),
Phase 3 = #C (linux-genlock.yml) then #F (imag burn 911003 + drift-guard imag),
Phase 4 = #D (verdict --imag + optical gate) + #E (recording-e2e/render-budget integration).
Order: A ∥ B → D → E; C → F. Issue numbers filled in at filing time.
