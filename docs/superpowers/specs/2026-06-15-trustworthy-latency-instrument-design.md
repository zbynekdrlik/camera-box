# Trustworthy Frame-Loss + Exact Per-Hop Latency Instrument — Design

**Date:** 2026-06-15
**Status:** Draft for review
**Supersedes the latency claims of:** the #68/#70 QR harness (loss detection is sound; latency + the "underruns=0 ⇒ zero-loss" shortcut are NOT trustworthy).

## Why this rebuild (recon evidence, 2026-06-15)

Two independent recons established that the current instrument cannot honestly answer "how long does each hop take, and was every generated frame delivered at every node":

1. **Clock: the whole cluster IS synced sub-millisecond via DanteSync (VERIFIED 2026-06-15).** (An earlier recon claimed cam2/dev1 were ~0.7 s adrift — that was a BOGUS measurement; it checked chrony/ntp/ptp4l but not `dantesync`, and its NTP probe was wrong.)
   - `DanteSync` (zbynekdrlik/dantetimesync, the user's cross-platform Rust PTPv1 tool, hybrid NTP+PTP, target <50 µs) runs as a service on **every node**: cam1-4, dev1 (develbox), strih, stream — all locked to the Dante grandmaster `AIC128-D-080214` = **10.77.9.184**.
   - Measured offsets to strih: cam1 0.50 ms, cam2 0.23 ms, cam3 0.07 ms, dev1 0.16 ms (DanteSync logs show `[PTP] LOCK Drift ~0 µs/s`, NTP residuals tens-to-hundreds of µs).
   - ⇒ the clock foundation already exists; absolute timestamps across nodes are comparable within sub-ms. NO clock tool needs installing — and NO other NTP/PTP tool may be added to the cam boxes (DanteSync owns the clock; it disables chrony/timesyncd/ptp4l on install).

2. **All three taps run centrally on dev1** (NDI receivers of the cam/strih/stream NDI sources). "Per-hop" latency = difference of two **dev1 receive instants** of independently-buffered, oversampled streams → conflates real transit with dev1 NDI-receiver buffer jitter → **goes negative** (`cam→strih min_ms -182`). It never measured node-to-node transit.

3. **`underruns=0` is an OBS-internal FIFO counter, not cross-node delivery.** It says the genlock consumer didn't starve; it does NOT prove the frame cam generated arrived exactly once at strih output and stream output. The only valid loss proof is **pairing every QR id at every node**, over a long run.

4. **Source-side rig blind spot.** The painter free-runs (12 fps wall-clock sleep) into a 60 fps capture (5× oversample by design) on a **single-buffer fbdev** with only an `FBIO_WAITFORVSYNC`-gated direct write → a measured ~2.2 % torn-QR "emission artifact" that pollutes pairing and is reported as a caveat instead of being eliminated.

5. **NDI timecode is the key that survives OBS — and every node IS synced (point 1).** camera-box's NDI sender already stamps the frame `timecode` from its DanteSync-disciplined wall clock at the genlock boundary (`src/ndi.rs:792-805`). On re-emit OBS/DistroAV **regenerates** the timecode from the *emitting node's* clock (`NDIlib_send_timecode_synthesize`). So **each node's NDI frame carries that node's own emit time on the shared DanteSync clock.** Reading the embedded timecode at each tap yields exact per-node emit times → per-hop = difference (sub-ms valid, since every node is locked to the same GM). (Must verify empirically that OBS `synthesize` uses the DanteSync-disciplined OS wall clock and not a free counter.)

## Goal

A CI/operator-runnable instrument that PROVES, from its own measurements with no human eyeballing:
- **Zero frame loss** end-to-end: every QR id the generator emitted is delivered exactly once at cam output, strih output, and stream output (per-node pairing, long run).
- **Exact per-hop latency** (cam→strih, strih→stream) and exact absolute latency (paint→stream), in real milliseconds on a shared high-precision clock, with NO physically-impossible values.
- A **tear-free, 1:1, phase-locked source** so pairing has no "emission artifact" caveat.

## Architecture (5 parts, one coherent design)

### Part 1 — Clock foundation: ALREADY DONE (DanteSync) — assert, never replace

The cam boxes are ALREADY synced sub-ms to the broadcast clock by **DanteSync** (the user's own cross-platform Rust PTP tool, `zbynekdrlik/dantetimesync`), running as a service on cam1-4 + dev1 + strih + stream, all locked to the Dante GM 10.77.9.184. Verified offsets to strih: cam1-3 0.07–0.50 ms, dev1 0.16 ms. **No clock tool is to be installed; NO chrony/ptp4l/timesyncd on the cam boxes** — DanteSync owns the clock and disables those on install.

- **Camera-box installer integration = ASSERT, not install another tool.** `scripts/setup.sh` should *verify* DanteSync is present + active + LOCKED (and, if genuinely missing, install it via DanteSync's own `install.sh` / release `dantesync-linux-amd64`, configured `--ntp-server strih.lan`), then fail loud if it is not locked. It must never lay down a competing time daemon.
- **Verification gate (reused everywhere):** `scripts/clock-offset-guard.sh` (master=strih) confirms each node is within a tight bound (target ≤ a few hundred µs; hard-fail above ~1 frame) before any latency run is trusted — exactly the existing run-start guard, now grounded in the real (already-working) DanteSync sync.
- **Status source:** DanteSync self-reports lock/drift/NTP-offset in `/var/log/dantesync/dantesync.log` (Linux) and the `\\.\pipe\dantesync` IPC (Windows) — use these as ground truth, not ad-hoc NTP probes (one such probe produced the bogus "0.7 s adrift" that derailed the first draft).

### Part 2 — Receiver reads the NDI timecode (per-node emit time)

- camera-box `NdiReceiver::capture_frame` currently discards the frame timecode. Add timecode read-out to `ReceivedFrame` (`src/ndi.rs`).
- `multitap-probe` / `multi_reader` carry it into `Observed` as a new field `node_emit_tc_ns` (the embedded emit time of the node whose NDI stream this tap subscribes to), alongside the existing `frame_id`, `gen_ts_ns`, `recv_ts_ns`.
- Keep `recv_ts_ns` (dev1) only as a liveness/debug aid — it is NOT used for latency anymore.

### Part 3 — Differ: exact per-hop + absolute from synced timestamps

- **Per-hop latency** = `first(node_emit_tc of downstream tap for id) − first(node_emit_tc of upstream tap for id)`, per id paired across the two taps. Both are PTP-domain emit times → an absolute transit, never negative (a negative now correctly indicates a real clock-sync breach and hard-fails, like the existing absolute gate).
- **Absolute latency** (paint→stream) = `stream node_emit_tc − painter gen_ts`, both on the shared PTP clock (requires Part 1 so cam2's `gen_ts` is in-domain).
- **Oversampling:** "first emit per id per node" is well-defined because ids are unique-per-frame + contiguous (`painter.rs` wrapping_add); Part 4 removes source oversampling entirely so first==only.
- Replace the recv−recv `diff_hop` latency block; keep the loss logic (full_span / single_copy / endpoint_pipeline_loss) — it is correct.
- New honest gates: per-hop p99 bound, absolute p99 bound, negative-latency ⇒ FAIL (clock breach), and the existing strict zero-loss.

### Part 4 — Painter ↔ grabber: tear-free, 1:1, phase-locked

Decisive hardware fact (recon): the HDMI output CRTC and the ShadowCast 2 capture are **both exactly 60.000 Hz**.

- **Presenter:** new DRM/KMS path (`/dev/dri/card1`, i915 atomic) replacing the single-page fbdev. Allocate two dumb BOs, render the QR off-screen, `drmModePageFlip`/atomic-commit with `DRM_MODE_PAGE_FLIP_EVENT`, block on flip-complete (vblank) before advancing to the next id. Requires DRM master (detach fbcon). ⇒ **true tear-free + exactly one unique id per HDMI vblank (60 fps, 1:1)**.
- **Capture clock recovery:** stop discarding the V4L2 per-frame metadata (`src/capture.rs:153,:175`). Surface `metadata.timestamp` (CLOCK_MONOTONIC, kernel SOF) + `sequence` to the reader → the capture clock becomes observable, so 1:1 capture is *proven per id*, not inferred.
- **Result:** the ~2.2 % torn-QR blind spot and the 5× oversample both disappear; every painted frame is a unique, fully-formed, captured-once frame.
- Phase-1 single-box loopback (cam2 HDMI→capture) uses this; CAM1 still lacks a loopback cable (#24) so remains a cam2-proxy until hardware is added.

### Part 5 — Verdict + the long proving run

- Run ≥ 30–60 min (configurable), leading-discard window, pairing **every** id at cam + strih + stream taps.
- Pass = strict zero pipeline loss (single-copy, all hops) AND per-hop/absolute latency within bounds AND no negative latency AND enough single-copy frames to be conclusive (#29).
- Output: per-hop exact ms (p50/p95/p99/max), absolute ms, total ids generated vs delivered per node, and the clock-offset evidence for the run window.

## Components / files

- `scripts/setup.sh` (+ `scripts/ptp/camera-box.conf`, `ptp4l@.service`) — Part 1 installer.
- `scripts/clock-offset-guard.sh` — reused as the Part 1/Part 5 verification gate.
- `src/ndi.rs` — Part 2 receiver timecode read.
- `src/probe/analyzer.rs` (Observed +field), `src/probe/multi_reader.rs`, `src/bin/multitap-probe.rs` — Part 2 plumbing.
- `src/probe/differ.rs` — Part 3 per-hop/absolute from emit timecodes.
- `src/probe/kms.rs` (new), `src/probe/painter.rs`, `src/probe/fb.rs`, `src/capture.rs`, `src/bin/frame-probe.rs` — Part 4 DRM presenter + V4L2 metadata.
- `scripts/multitap-e2e.sh`, `scripts/obs_phase2.py` — Part 5 long run + the already-fixed teardown.

## Testing

- **Unit (Rust, CI):** differ per-hop/absolute math from emit timecodes incl. negative⇒FAIL and oversample-first selection; payload round-trip; V4L2 metadata decode; KMS mode/flip decision logic (pure parts). cargo-mutants on the new pure functions.
- **Integration (on cam2, live):** Part 1 — `setup.sh` brings ptp4l up, offset-guard passes. Part 4 — DRM page-flip presents tear-free, V4L2 sequence advances 1:1 with painted ids. Part 5 — the long run on the real strih/stream chain.
- **No mocks of internal code; OBS/DRM/NDI glue verified by the on-device E2E, excluded from coverage/mutation as today.**

## Phasing (build order — user chose "full rebuild at once")

Built as one design; landed as a short sequence of PRs so each is reviewable and revertable, in dependency order:
1. **Part 1** (clock) — ALREADY DONE (DanteSync, verified). Only work: add the assert-DanteSync-locked guard to `setup.sh`/run-start. Not a build phase, a check.
2. **Part 2 + 3** (NDI timecode → exact per-hop/absolute) — answers "how long each hop takes." First real build.
3. **Part 4** (DRM 1:1 painter) — removes the source blind spot; clean pairing.
4. **Part 5** (long proving run + CI wiring) — the standing regression gate.

(This is not a "progressive rollout of one feature"; these are genuinely separable subsystems with hard dependencies, each shippable and testable on its own.)

## Open questions for review

- **Clock:** RESOLVED — DanteSync already syncs every node sub-ms; `setup.sh` only asserts it. (No linuxptp, no other time tool on cam boxes.)
- **OBS timecode:** does DistroAV's `synthesize` on Main Output stamp the strih/stream DanteSync wall clock (so `strih_emit_tc − cam_emit_tc` is true hop latency)? Must confirm empirically by reading live timecodes before trusting Part 3. If OBS does NOT emit a usable wall-clock timecode, fall back to: tap-on-each-node (a small reader on strih/stream stamping recv on its DanteSync clock) — heavier, but every node is synced so it works.
- **DRM master:** detaching fbcon to take DRM master on cam2 is required for true page-flip. Acceptable (the painter owns the HDMI output during a run; off-air).
- **Precision:** DanteSync targets <50 µs (logs show LOCK drift ~0 µs/s); per-hop accuracy is bounded by that + OBS timecode granularity — ample for frame-level (16–33 ms) latency.
