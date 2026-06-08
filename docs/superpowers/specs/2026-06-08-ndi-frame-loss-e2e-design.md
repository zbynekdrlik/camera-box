# NDI Frame-Loss & Latency E2E Test Harness — Design

**Date:** 2026-06-08
**Status:** Approved (brainstorming) — ready for implementation plan
**Scope of this spec:** Phase 1 in full detail; Phases 2–3 outlined for architectural continuity.

## 1. Summary

Build a Test-Driven, evidence-producing harness that detects **frame loss** and measures
**per-node latency** across the NDI → OBS delivery path, replacing today's human "I see
frame lag/loss" observation with a deterministic pass/fail signal that catches regressions.

The mechanism: generate a video where **every frame carries a QR code encoding a
monotonic frame ID** (plus a generation timestamp). Decode the QR at each point in the
pipeline. Missing IDs = lost frames; the timestamp delta = latency. Phase 1 proves the
approach on a single box (cam2) using a physical HDMI-out → HDMI-in loopback.

## 2. Motivation

- Today, delivery quality is judged by a person watching the output. Frame lags and drops
  are seen but not measured, located, or regression-guarded.
- Quality fluctuates as code changes land — an improvement in one area silently regresses
  another, with no automated detector.
- We need machine-checkable evidence: "over a defined run, zero frames were lost and
  latency stayed within bound," runnable on demand and eventually on every change.

## 3. Goals / Non-goals

**Goals (Phase 1):**
- Deterministic, false-positive-free detection of frame loss on the cam2 loopback.
- Per-frame latency measurement (single monotonic clock, no NTP required on one box).
- A TDD structure: hardware-free logic unit-tested in normal CI; the real loopback run
  produces a metrics artifact as evidence.
- A runnable harness (one command on dev1) — not yet wired as a CI job.

**Non-goals (Phase 1):**
- Cross-machine (strih/stream) measurement — Phase 2.
- Cross-machine latency / shared clock / OBS clock source-of-truth — Phase 3.
- Modifying the production `camera-box` capture→NDI path. It runs **unchanged**; Phase 1
  measures *its* behavior.
- Wiring a self-hosted GitHub Actions runner — deferred until the harness is proven green.

## 4. Verified environment facts (probed 2026-06-08 on cam2 / 10.77.9.62)

- **Board:** Intel Alder Lake-N mini-PC, x86_64. UHD Graphics (i915).
- **HDMI OUT:** Intel iGPU → `/dev/fb0` (`i915drmfb`, 1920×1080, 32bpp). DRM connector
  `HDMI-A-2` = **connected**. A 1080p mode is active (fb virtual size 1920×1080).
- **HDMI IN:** **GENKI ShadowCast 2** USB capture (`uvcvideo`) → `/dev/video0`, 1920×1080
  YUYV. (UVC device — does **not** expose `DV_TIMINGS`, so HDMI-in lock state can't be
  queried that way; verify the loop functionally instead.)
- **camera-box service:** active (holds `/dev/video0`). `libndi.so.6` present.
- **Clock:** **NOT synchronized** — NTP/chrony/timesyncd/ptp4l all inactive. This
  invalidates absolute cross-machine timing until fixed, and is the seed of the Phase-3
  "clock source of truth" work. Single-box (Phase 1) latency uses cam2's own monotonic
  clock and is unaffected.
- **Existing building blocks in the repo** (reuse, don't reinvent):
  - `src/display.rs` — writes BGRA frames to `/dev/fb0` (the painter's output path).
  - `src/ndi.rs` — `NdiReceiver` connects to an NDI source and returns UYVY frames (the
    reader's input path); also the genlock pacing helper `wait_for_next_boundary_100ns`.

## 5. Architecture overview

### 5.1 Signal chain (Phase 1)

```
frame-probe (painter thread)
   └─ QR(frame_id, gen_ts) → BGRA 1080p → /dev/fb0
        └─ iGPU HDMI-A-2 OUT ── cable ──> ShadowCast 2 HDMI IN
             └─ /dev/video0 (YUYV) ─> camera-box [UNCHANGED] ─> NDI "usb (CAM2)"
                  └─ frame-probe (reader thread): NdiReceiver → UYVY → Y(gray) → rqrr decode
                       └─ (frame_id, recv_ts)
analyzer: correlate emitted vs observed → classify → latency stats → JSON artifact
dev1: orchestrates over SSH, pulls the JSON artifact, asserts the gate.
```

The painter and reader run in **one process on cam2** sharing **one monotonic clock**, so
latency = `recv_ts(id) − gen_ts(id)` is exact with no clock sync. `gen_ts` is also embedded
in the QR so the *same* decoder works against remote taps in Phase 2.

### 5.2 The central problem — the loop is an async resample, not a digital pipe

Independent clock domains exist between paint and capture:

1. Painter software paint rate (jittery).
2. iGPU HDMI scanout rate (fixed mode).
3. ShadowCast capture crystal (independent ~30 fps).
4. V4L2 delivery to camera-box.
5. NDI send pacing.

Because the ShadowCast samples on its own clock, a **perfect** system still yields
occasional duplicate or skipped QR at the capture boundary purely from rate beat. A naive
"QR IDs must be contiguous" rule would false-positive on this beat. The methodology below
separates **beat noise** from **real loss**.

## 6. Measurement methodology

Two painter modes (both built — user decision):

### 6.1 Coverage mode → the clean zero-loss gate

Painter emits **slower than the capture samples** (default ~24 fps vs 30 fps capture). Each
painted frame persists on the framebuffer for its full paint interval (~41 ms), which is
**longer than one capture period (~33 ms) with margin** — so every painted frame's display
window contains at least one capture instant. This **guarantees every emitted ID is sampled
at least once** in a healthy system. Therefore:

- **Loss = any emitted ID that never appears** in the decoded stream. No beat tolerance,
  no fuzziness — an absent ID is a real drop.
- Duplicates are **expected** (capture faster than paint) and ignored.
- Holding each frame stable across the sample also defeats HDMI tearing.

This is the run that backs the **zero-loss hard gate**.

### 6.2 Full-rate mode → the realistic stress / soak

Painter emits at the production rate (30 fps). Reproduces the real-path lag/freeze the
operator observes. A **beat-aware analyzer** classifies the decoded stream rather than
demanding contiguity (see 6.3). Reports beat/dup/freeze statistics; failures here are
freezes/reorders/large gaps, not single-step beat.

### 6.3 Classification rules (analyzer)

Given the ordered decoded `(frame_id, recv_ts)` stream and the emitted set:

- **Missing (LOSS):** an emitted ID absent from the decoded stream. (Coverage mode: hard
  fail. Full-rate: a gap larger than the rate-ratio beat can explain.)
- **Freeze (STALL / "lag"):** the same ID decoded for longer than a freeze threshold
  (e.g. > 3 capture periods) → the pipeline stalled. This is the operator-visible "lag".
- **Reorder:** a backward jump in decoded IDs (non-monotonic).
- **Healthy:** monotonic non-decreasing IDs; steps of 0 (dup) or within the beat bound.

### 6.4 Latency

For each decoded ID, `latency = recv_ts(first appearance) − gen_ts`. Report
min / mean / p50 / p95 / p99 / max and the freeze-duration distribution. Phase 1 **reports**
latency (no hard bound yet); a bound is ratcheted in once we have baseline numbers.

## 7. Components (new code)

### 7.1 `frame-probe` binary (in-repo, deps feature-gated)

- New `[[bin]] name = "frame-probe"` with `required-features = ["probe"]`.
- New optional deps behind a `probe` feature: `qrcode` (encode), `rqrr` (decode),
  `image` (buffer ops). Production `camera-box` is built **without** `--features probe`, so
  these never link into the deployed binary.
- **Painter thread:** build payload → QR (EC level **H**, large module size, quiet zone) →
  draw QR + a plain large-digit frame number (redundancy/debug) into a 1080p BGRA buffer →
  write `/dev/fb0` via the existing `display.rs` framebuffer path → pace to the selected
  rate. Records `frame_id → gen_ts` (monotonic).
- **Reader thread:** `NdiReceiver` → per frame extract **Y from UYVY** (luma is every other
  byte — cheap, lossless enough for QR) → `rqrr` decode → `(frame_id, recv_ts)`.
- Writes a **JSON metrics artifact** (run_id, mode, counts, losses with IDs, latency
  percentiles, freeze list, verdict).

### 7.2 Analyzer (pure library module, fully unit-testable)

Takes emitted IDs + decoded `(id, ts)` stream + mode → produces the classification and
stats in 6.3/6.4. **Zero hardware, zero I/O** — this is the regression guard's testable core.

### 7.3 QR payload

`frame_id` (u32) + `gen_ts` (i64 ns) + `run_id` (u32) + CRC. Compact, fixed layout, so it
fits a low-version high-EC QR that survives 4:2:2 subsampling and scaling.

## 8. TDD strategy

### 8.1 Unit tests — run in existing GitHub CI, no hardware (RED first)

- QR payload encode→decode roundtrip (incl. CRC rejects corruption).
- QR render → **simulated degradation** (downscale + 4:2:2 subsample + additive noise) →
  decode still succeeds. Proves robustness offline before any hardware.
- UYVY → Y(grayscale) extraction correctness.
- **Analyzer**: feed synthetic decoded streams with injected `missing` / `freeze` /
  `reorder` / `healthy-with-dups` → assert exact PASS/FAIL + classification. (Core guard.)
- Latency-stats math (percentiles, freeze-duration detection) on known inputs.

### 8.2 Hardware E2E — runs on dev1, drives cam2 (the evidence)

- Orchestrator (script / cargo xtask) over SSH: deploy `frame-probe` to cam2, run a
  coverage-mode run for the configured duration, pull the JSON artifact, assert the gate.
- Default: 5 min / ~9k frames. Parametrized for shorter smoke / longer soak.
- Coverage run → **zero-loss gate**. Full-rate run → stress report (artifact, not gated yet).

## 9. Pass/fail contract (Phase 1)

- **Hard gate (coverage mode):** zero missing IDs, zero reorders over the run. Any single
  real loss or reorder = FAIL.
- **Latency:** measured and reported; **no hard bound in Phase 1**. Bound ratcheted in once
  baseline numbers exist.
- **Freeze:** reported; becomes a gate once a threshold is calibrated from baseline.
- **Default run length:** 5 min / ~9,000 frames; parameter with shorter/longer presets.

## 10. CI wiring

- **Phase 1 deliverable:** a single runnable target on dev1 (e.g. `cargo xtask loopback`
  or a `just` recipe) that runs the loopback and prints pass/fail + writes the artifact.
- Unit tests (§8.1) join the existing `test` job immediately (they need no hardware).
- The self-hosted GitHub Actions runner + on-PR hardware job is a **follow-on**, added once
  the harness is proven green locally.

## 11. Phasing (2–3 outlined)

- **Phase 1 (now):** cam2 single-box loopback; clock-free loss + single-clock latency.
- **Phase 2:** add per-frame taps at OBS **strih** then **stream**. Localize loss by
  **differencing the decoded ID-set between adjacent points** (the upstream resample beat
  cancels out). Per-frame NDI tap — **not** OBS WebSocket screenshots (they sample at a few
  fps and cannot see single-frame loss). Cross-machine latency needs a shared clock.
- **Phase 3:** establish a **clock source of truth** across nodes (enable PTP/NTP and/or an
  OBS/DistroAV fork — the repo already carries `distroav-timestamp-fix.patch`, directly
  relevant), enabling absolute end-to-end latency and full-path zero-loss gating.

## 12. Risks / open items

- **Loop cable physically present?** Probe couldn't confirm the ShadowCast input is the
  iGPU output (UVC has no dv-timings). Verify functionally early: paint a known pattern →
  read it back from `/dev/video0` (needs a brief camera-box stop — allowed, CAM2 is the rig).
- **fb0 ownership / fbcon:** `/dev/fb0` is i915 with `fbcon`. Direct writes may tear or be
  overwritten by console output. Mitigations: full-screen QR, blank/redirect the VT, hold
  each frame stable (coverage mode already does). Escalate to DRM dumb-buffer + vsync
  page-flip only if direct fb writes prove unreliable.
- **Read-only rootfs:** the device rootfs is normally read-only (currently rw). The artifact
  must be written to `/run` (tmpfs) or streamed over the network / SSH back to dev1 — never
  assume a writable `/`.
- **ShadowCast clock vs scanout:** ppm crystal drift over a 5-min run → occasional
  dup/skip. Coverage mode is immune (paint < capture); full-rate analyzer accounts for it.
- **HDMI output mode:** confirm the active iGPU mode (1080p, and its rate — 60 vs 30 Hz);
  pin it so paint timing is well-defined.
- **rqrr robustness** at speed on 1080p: keep the QR large + high EC; if per-frame decode is
  too slow, crop to the known QR region before decoding.
- **Dependency bloat:** QR/image deps must stay behind the `probe` feature so the deployed
  `camera-box` binary is unaffected (size, attack surface).

## 13. Acceptance criteria (Phase 1)

- [ ] `frame-probe` (feature-gated) builds; production `camera-box` build unchanged and not
      linking QR deps.
- [ ] Unit tests (§8.1) exist, were RED before implementation, and pass in GitHub CI.
- [ ] Loop verified functionally (painted pattern read back from the capture).
- [ ] Coverage-mode run on cam2 over the default duration reports **zero loss, zero
      reorder**, and emits the JSON metrics artifact.
- [ ] Full-rate run emits a stress report with latency + freeze statistics.
- [ ] One runnable command on dev1 executes the loopback end-to-end and asserts the gate.
- [ ] Latency baseline captured so a hard latency/freeze bound can be set next.

## 14. Parameters / defaults

| Parameter | Default | Notes |
|---|---|---|
| Capture rate | 30 fps | ShadowCast / camera-box native |
| Coverage paint rate | ~24 fps | < capture (paint interval ~41 ms > capture ~33 ms), guarantees each ID sampled |
| Full-rate paint rate | 30 fps | production stress |
| Run length | 5 min (~9,000 frames) | parametrized (smoke / soak presets) |
| QR EC level | H (30%) | survives subsample + scale |
| Freeze threshold | > 3 capture periods | reported in Phase 1, gated later |
| Loss gate | 0 (coverage mode) | hard fail on any real loss/reorder |
| Latency gate | none (report only) | ratchet a bound in after baseline |
