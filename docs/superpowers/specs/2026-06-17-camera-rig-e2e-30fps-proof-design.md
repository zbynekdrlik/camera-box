# Real-Camera-Rig End-to-End 30 fps Zero-Loss Proof — Design

**Status:** approved (brainstorming 2026-06-17)
**Supersedes the source side of:** `2026-06-08-ndi-frame-loss-e2e-phase2.md` (synthetic cam2 loopback → real camera rig)

## Goal

Prove — with the **user's own physical rig** — that the broadcast chain delivers **every
one of the 30 fps frames** end to end with **zero loss**, and present the proof as a **visual
report** (graphs) that makes per-frame delivery and latency stability obvious at a glance.
No more per-hop-only claims, no synthetic injection, no sub-5-minute "zero loss" assertions.

## The rig (measured live 2026-06-17, not assumed)

```
cam2: frame-probe paints QR (KMS/DRM card1) ──HDMI──> MONITOR (HDMI-A-2, 1080p, connected)
                                                          │  (real broadcast camera, optical)
                                                          ▼
cam1: camera HDMI ──> ShadowCast 2 USB capture (/dev/video0, YUYV 1080p) ──> camera-box ──> NDI "CAM1 (usb)"
                                                          ▼
strih (10.77.9.202): vendored OBS 32.1.2 + DistroAV 6.2.1 ──render──> NDI Main Output (PGM)
                                                          ▼
stream (10.77.9.204): vendored OBS 32.1.2 + DistroAV 6.2.1 ──render──> NDI Main Output (PGM)
```

**Measured facts that drive the design:**

- `cam1` ShadowCast currently runs **60 fps capture → 30 fps NDI emit** — camera-box drops
  every second captured frame *inside cam1* (`300 captured / 150 sent` per 5 s in the live log).
  That internal 2:1 downsample is removed: ShadowCast is set to **native 30 fps** so capture =
  emit = 30 fps, 1:1.
- ShadowCast supports 30 fps natively (`v4l2-ctl` enum: 1920×1080 @ 60/50/**30**/20/10).
- `cam2` HDMI-A-2 is connected to a real monitor at 1080p (KMS, `card1`). `cam1` is headless
  (no `/dev/fb0`), capture-only.
- strih/stream already advertise the DistroAV **NDI Main Output** = the **rendered program**
  (post scene-composition + genlock FIFO + render), tapped at the `raw_video` callback. A frame
  counted there has passed OBS's full pipeline — it is NOT the pre-render NDI source input.

## Decisions (brainstorming forks the user chose)

1. **Reference = painted sequence, loss decomposed per hop**, but the **strict 1:1 zero-loss
   verdict is anchored at cam1 capture (TAP A) → stream PGM (TAP C)**. The optical hop is
   measured and reported, never folded into the strict verdict.
2. **True 30 fps across the whole chain** — ShadowCast 60→30, camera HDMI set to 1080p30 (user
   action on the camera), OBS already 30. No 60→30 downsample anywhere.
3. **Dedicated separate probe scenes** on strih + stream (the existing PHASE2-PROBE approach).
   **Do NOT touch production scenes.** Copy the genlock per-input settings *from* the production
   cam1 (on strih) / strih (on stream) inputs onto the probe input so the path is representative.
   Leave the probe scene stable and clearly named so the user can run their own visual test on it.
4. **Per-frame proof = QR id matching + per-output 30 fps timecode-grid continuity.** A drop shows
   as a missing grid slot (output starved), a repeated QR (FIFO underrun), a skipped id (frame
   discarded), or a backward jump (reorder). Strict zero-loss: any of these in-span = FAIL.
5. **Optical hop (monitor → camera) measured separately** as a readability %, NOT counted against
   the digital verdict. It is the only un-genlocked link (camera samples the monitor
   asynchronously); minimised by a short camera shutter, eliminated only by hardware genlock.
6. **Visual report** — two PNG graphs + a per-hop table, delivered as a **clickable LAN URL**
   (`airuleset.py share`), never a /tmp path.
7. **Duration policy** — ≥ **300 s** to declare zero-loss, ideal **1800 s**, **early-abort on ANY
   detected loss** (then keep investigating). The harness **refuses to emit a zero-loss pass for a
   run shorter than 300 s** and refuses to claim zero-loss from ad-hoc captures.

## Measurement points

| Tap | Source | Position | Role |
|---|---|---|---|
| **A** | `CAM1 (usb)` | cam1 NDI (real 30 fps capture) | **ANCHOR** (strict-verdict source) |
| **B** | strih NDI Main Output | rendered program (post-render) | strict hop 1 endpoint |
| **C** | stream NDI Main Output | rendered program (post-render) | **ENDPOINT** (headline) |
| painter log | cam2 frame-probe `(frame_id, gen_ts_ns)` | pre-monitor | optical-hop reference only |

## Hops

- **Hop 0 (optical + capture):** painter → cam1. Reported (readability %, capture rate).
  **Excluded from the strict verdict** (un-genlocked optics).
- **Hop 1 (strih):** cam1 (A) → strih PGM (B). **Strict 1:1.**
- **Hop 2 (stream):** strih PGM (B) → stream PGM (C). **Strict 1:1.**
- **Full-span (headline):** cam1 (A) → stream PGM (C). **Strict 1:1.**

## Per-frame proof mechanism

For each tap, build the decoded `(frame_id → recv_ts, emit_tc)` series plus the **raw 30 fps
timecode grid** (every NDI frame, decodable QR or not — an optically-blurred frame is still a real
delivered frame and still occupies a grid slot). The differ then asserts, per strict hop:

- **Identity & order:** every upstream `frame_id` present downstream within the active span, in
  monotonic order, none missing, none repeated (oversample discriminator separates true single-copy
  loss from genlock repeats — keep the existing `single_copy_dropped / single_copy_total`).
- **Grid continuity:** each output's 30 fps timecode grid has no missing slot (no starvation) and
  no duplicate slot (no underrun repeat) across the measured window.
- **Latency:** per-frame `emit_tc(down) − emit_tc(up)` on the shared DanteSync wall-clock (hop 1,
  hop 2) and absolute `recv(C) − gen(painter)` (full span). Report p50/p99 and the per-frame series.

## Components / files

Modify (existing):

- `src/bin/frame-probe.rs` — lock paint to 30 fps; QR sized/contrasted for an optical camera shot
  of a monitor (large module size, max EC, high contrast); paint reference log unchanged.
- `src/bin/multitap-probe.rs` — source tap = `CAM1 (usb)` (was `CAM2 (usb)`); add per-output
  30 fps timecode-grid continuity capture; anchor = cam1; emit the per-frame series into the JSON.
- `src/probe/differ.rs` — add per-output grid-continuity check; add hop-0 readability
  decomposition; anchor the strict verdict at cam1; keep existing oversample/active-span logic.
- `scripts/obs_phase2.py` — create/refresh the dedicated probe scene; **read production genlock
  per-input settings and copy them onto the probe input**; never modify production scenes; verify
  Main Output enabled.
- `scripts/multitap-e2e.sh` — set ShadowCast to 30 fps on cam1 before the run; enforce the ≥300 s
  duration gate; generate + `share` the report; restore cam1 capture state on teardown.

Create (new):

- `scripts/e2e-report.py` — read the probe JSON, render the two PNG graphs + the per-hop table,
  print the `airuleset.py share` URL.

## Report (the deliverable that was missing)

- **Graph 1 — Delivery continuity:** x = frame_id (time), four lanes (Painted / cam1 / strih PGM /
  stream PGM). A dot per delivered frame; a gap = a black hole = a dropped frame. The same picture
  as the user's moving-dot test, but quantified and per-tap.
- **Graph 2 — Per-frame latency:** x = frame_id, y = ms; one line per hop (cam1→strih, strih→stream)
  plus absolute (painter→stream); p50/p99 bands so jitter is obvious.
- **Per-hop table:** unique up/down, dropped, %, p50/p99 latency, verdict; plus the optical-hop
  readability % (hop 0, informational).
- Delivered as a single PNG (or small bundle) via a clickable LAN URL.

## Duration & honesty gates

- `--duration-secs` default 300; harness sets `zero_loss_claim = false` and FAILS the claim if the
  measured steady-state window < 300 s.
- Early-abort: if the live differ detects any in-span drop, stop the timer early, mark FAIL, keep
  the partial data for investigation (an early FAIL is allowed; an early PASS is not).
- A zero-loss PASS requires: all strict hops Pass + full-span Pass + grid continuity clean +
  measured window ≥ 300 s.

## TDD / CI

- Unit tests (RED→GREEN) for the new differ grid-continuity check and hop-0 decomposition.
- `full-path-e2e.yml` (self-hosted, manual) runs the harness against the real rig; it is the
  regression gate for any genlock change.
- Bug-fix changes follow RED→GREEN commit order per `regression-test-first.md`.

## User action required (hardware, outside the repo)

- Set the broadcast camera HDMI output to **1080p30** (so the capture card receives 30, not 60).
- Set a **short camera shutter** (e.g. 1/250–1/500 s) to minimise optical-transition blur on the
  monitor→camera hop.
