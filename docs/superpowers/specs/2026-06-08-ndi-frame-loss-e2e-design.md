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
             └─ /dev/video0 (YUYV) ─> camera-box [UNCHANGED] ─> NDI "CAM2 (usb)"
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

Painter emits **slower than the capture samples** (default **12 fps** vs 30 fps capture —
see §15 for why 12, not 24). Each painted frame persists on the framebuffer ~83 ms (≥2.5
capture periods at 30 fps), guaranteeing **≥2 capture samples per ID**. The framebuffer is
written once per ID (~0.8 ms) and captures are ~33 ms apart, so **at most one** sample per
ID can be torn → **≥1 clean sample always exists**. Therefore:

- **Sustained loss = any emitted ID that never appears** in *any* of its samples — a real,
  ≥83 ms pipeline gap. No beat tolerance, no tearing false positives.
- Duplicates are **expected** (capture faster than paint) and ignored.
- Single-frame drops are intentionally *not* gated here — over-sampling masks them, and on
  the HDMI resample they are rig-confounded anyway (see §15 "Gate semantics").

This is the run that backs the **deterministic sustained-loss + reorder gate**.

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
| Coverage paint rate | 12 fps | each ID displayed ~83 ms (≥2.5 capture periods) → ≥2 samples/ID, ≥1 always tear-free |
| Full-rate paint rate | 30 fps | production stress |
| Run length | 5 min (~9,000 frames) | parametrized (smoke / soak presets) |
| QR EC level | H (30%) | survives subsample + scale |
| Freeze detection | > 6 capture periods (`--freeze-periods`) | populates the freeze list |
| Loss gate | 0 (coverage mode) | hard fail on any real loss/reorder |
| Latency gate | `--max-p99-latency-ms 250` (rig default) | hard fail if p99 > bound; `None` ⇒ off (see §15.1) |
| Freeze gate | `--max-freeze-periods 6` (rig default) | hard fail if a stall repeats > N frames; `None` ⇒ off (see §15.1) |

## 15. Phase-1 implementation outcome (2026-06-08)

Built and verified on cam2 (the off-air rig). Key realities discovered during
hardware bring-up and how the design adapted:

- **cam2 holds `/dev/fb0`.** The service runs `camera-box --display "STRIH-SNV
  (interkom)"`, whose framebuffer thread owns fb0. The orchestration script
  (`scripts/loopback-e2e.sh`) therefore stops the service and runs camera-box
  **without `--display`** for the run (keeps capture→NDI, frees fb0), then always
  restores the service via a trap.
- **NDI source name is `CAM2 (usb)`** (`<machine> (<ndi_name>)`), not `usb (CAM2)`.
- **Decode must be cropped.** Full-frame 1080p `rqrr` decode was too slow (>33 ms),
  causing NDI backlog → inflated/growing latency and dropped frames. Decoding only
  the centered ROI (`qr_size+120`) fixed it; latency dropped 446 ms → ~115 ms.
- **Settle window.** Frames painted within `--settle-ms` (500 ms) of the run end are
  excluded from the loss check — they may still be in flight (latency ≤190 ms).
- **Tearing & the deterministic gate.** Direct fb0 writes tear (~0.07 %/frame false
  loss). The i915 DRM-emulated fbdev rejects legacy double-buffering
  (`FBIOPUT_VSCREENINFO` keeps `yres_virtual=1080`), so `VsyncFb` logs that and falls
  back to a direct write. The **deterministic** guarantee instead comes from the
  **12 fps coverage paint rate**: each ID is displayed ~83 ms (≥2.5 capture periods)
  → ≥2 samples/ID; the fb is written once per ID (~0.8 ms) and captures are ~33 ms
  apart, so at most one sample per ID is torn → ≥1 clean sample always exists.
  (`VsyncFb` is kept as best-effort for hardware that does support panning.)

**Gate semantics (important).** On the HDMI loopback, "every painted frame appears
exactly once" is **not** achievable — the ShadowCast samples on its own clock, an
async resample that adds dups/skips by beat. So:

- **Coverage (12 fps) gate** = zero **sustained** loss + zero reorder (+ freeze/latency
  reported). Deterministic, false-positive-free. Catches pipeline stalls, sustained
  drops, reordering, and latency regressions.
- **Full-rate (30 fps)** is report-only for loss: at paint≈capture the beat produces
  ~5 % periodic skips (measured) that are a rig artifact, not pipeline loss.
- **Single-frame-accurate loss** is therefore a **Phase-2** capability — diff the
  digital ID stream between two points (NDI-in vs OBS-out) where there is no resample.

**Measured baseline (cam2, 2026-06-08):**

| Run | Result | Frames | missing | reorders | freezes | latency mean / p50 / p95 / p99 / max (ms) |
|---|---|---|---|---|---|---|
| Coverage 5 min @12 fps | **PASS** | 3594 | 0 | 0 | 0 | 112.0 / 108.6 / 141.0 / 157.1 / 190.1 |
| Full-rate 90 s @30 fps | PASS (report-only) | 2685 | 138 (~5 % beat) | 0 | 0 | 122.1 / 131.7 / 165.1 / 165.4 / 167.3 |

End-to-end latency ~112–122 ms is dominated by the GENKI ShadowCast 2 USB capture
dongle. A hard latency/freeze bound can now be ratcheted in from these numbers.

### 15.1 Hard latency + freeze gate (issue #10, 2026-06-08)

The latency/freeze "ratchet after baseline" deferred in §9/§14 is now closed. The
analyzer verdict gates on two **optional** bounds (`None` ⇒ report-only, the
Phase-1 default — so nothing changes until thresholds are passed):

- `AnalysisInput.max_p99_latency_ms` — FAIL if `latency.p99_ms` exceeds it.
- `AnalysisInput.max_freeze_periods_gate` — FAIL if any detected freeze's
  `repeat_count` exceeds it (distinct from `freeze_periods`, which only *detects*).

Both use strict `>` (a value exactly at the bound passes) and apply to **both**
modes: full-rate loss stays report-only, but a latency or freeze regression there
still fails the run. `frame-probe` exposes `--max-p99-latency-ms` /
`--max-freeze-periods`; `scripts/loopback-e2e.sh` ships them via `MAX_P99_MS` /
`MAX_FREEZE_PERIODS` env overrides.

**Default thresholds (RIG-SPECIFIC — derived from the table above):** coverage
p99 = 157 ms, max = 190 ms ⇒ `MAX_P99_MS=250` (≈1.6× p99 margin so jitter does not
flake), `MAX_FREEZE_PERIODS=6` (the detection threshold — any genuine multi-period
stall fails). These are tuned to the cam2 / ShadowCast 2 rig; **re-baseline and
retune when the capture dongle, cabling, device, or fps changes** (USB capture
dominates the latency). The current baseline run still PASSES against these bounds
(p99 157 ms < 250 ms, 0 freezes).
