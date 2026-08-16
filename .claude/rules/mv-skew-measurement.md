---
paths:
  - "scripts/mv_skew_snapshot.py"
  - "tests/python/test_mv_skew_snapshot.py"
  - "scripts/qr_screenshot_check.py"
---

# Measuring inter-source presentation SKEW via OBS-WS screenshots — latch-timing is the whole game (#761)

`scripts/mv_skew_snapshot.py` measures the per-camera skew between scene `MV Cam N` (the multiview
cell the strihač sees) and scene `Cam N` (program) on imag, by screenshotting both over the OBS
WebSocket and decoding the painter QR `gen_ts_ns`. The subsystem is REPORT-ONLY (never gates) and
lives next to the #756 pins snapshot (same "impure gatherer + pure formatter `_section_mv_skew`"
split). If you ever build ANOTHER "how much later does source A present than source B" measurement
from `GetSourceScreenshot`, the calibration below is the part that is non-obvious and cost real time.

## The trap: a `GetSourceScreenshot` is SLOW and its cost is ASYMMETRIC between sources

One screenshot RPC is ~0.5–2 s (a 2.3 MB PNG at 1920×1080; ~0.5 s at 960×540) and VARIES a lot.
Between capturing source A and source B the live NDI frame advances by that wall gap, which alone is
100× the skew you want. **Order-reversal alone does NOT fix it**: reversal cancels only the *common*
gap; the "Cam N" scene (2× full-bw `NDI CAM{n}` + overlay) and the "MV Cam N" scene (1 item) have
very different, variable screenshot costs, leaving an uncancelled residual `(s_mv − s_main)/2`. A
first live run read a FALSE **−695 ms** on a shared-source, truly-~0 rig.

## The fix: timestamp each capture at REQUEST-SEND and add the wall gap back

The frame LATCHES essentially when OBS RECEIVES the request (render is fast; the RPC's bulk is PNG
readback+transfer AFTER the latch). So stamp each capture with `time.monotonic_ns()` **just before
the RPC** (`t_send`, dev1 clock) and compensate directly:

```
skew_ms = (gen_ts_main − gen_ts_mv)/1e6  +  (t_send_mv − t_send_main)/1e6
```

The `gen_ts` delta encodes `S − (t_latch_mv − t_latch_main)`; adding back the locally-measured wall
gap recovers S regardless of the asymmetric screenshot costs. Median over ≥15 order-alternated
samples (default `--rounds 8`, 960×540). **Live-validated floor: median within ±5 ms of true, ~10 ms
stdev** → the 1-frame (16.7 ms @60fps) alarm threshold IS resolvable. Two hard lessons:

- **Stamp at `t_send`, NOT the RPC midpoint.** Midpoint puts the noise floor at 50–180 ms because
  the asymmetric readback time leaks in; `t_send` drops it to ~10 ms. This is THE calibration —
  verified live (`tsend` medians ±5 ms across reps/resolutions; `tmid` swung ±57 ms).
- **Fewer samples = wider spread.** `--rounds 2` (3 samples) can push a median past 16.7 ms on pure
  noise; `--rounds 8` (≥15) stabilises it. Always report `n_samples` + `stdev_ms` so a reader sees
  the confidence — a single alarming small-sample median is not a real regression.

## Painter QR tick — pick the UNIVERSAL run_id, not a camera-local burn

Every camera shows the SAME painter dual-QR via the optical loop (one camera → splitter → all
boxes), so its `run_id` is universal; cam1 ALSO carries its own `cam1-burn` QR with a DIFFERENT
`run_id`. `dominant_run_id()` picks the run_id present in the most screenshots (the universal one);
`pick_common_run_id()` requires a run_id common to BOTH legs, else drops the sample (honest N/A, never
a fabricated 0). Payload is `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}` (src/probe/payload.rs); CRC is
standard CRC-32 (`zlib.crc32`) — validate it to reject cv2 misreads. Use `gen_ts_ns` (exact emission
instant, one painter clock) not `frame_id` — no frame-rate assumption, no cross-box clock skew.

## Current rig reality: SHARED-SOURCE on both boxes → this is a REGRESSION GUARD, expect ~0

Since 2026-07-15 both strih (clones deleted) and imag (scene `MV Cam N` now references the same
`NDI CAM{n}` input as `Cam N`; `genlock_monitor=null`, no separate `MV CAM{n}` input exists) are
shared-source, so skew is ~0 by construction. The measurement is a guard that catches a re-introduced
separate-decode clone (an imag experiment, or #763's derived stream) as a large step change.

## imag OBS-WS auth: use `OBS_PASSWORD`, NEVER `IMAG_PW` (#761 review)

Any imag OBS-WebSocket consumer authenticates with **`OBS_PASSWORD`** (the WS credential;
OBS_PASSWORD-first, then the `--password` arg, default `""` — mirror `imag_latency_enforce.py` and the
#756 pins snapshot). `IMAG_PW` is the SSH/box password (used with `sshpass`) — semantically wrong for
a WebSocket. imag's OBS WS is auth-less today (`verify-imag.sh` runs `imag_scenes.py` bare), so the
password is ignored — but wiring the SSH password makes yours the ONE consumer that silently goes
dark the day WS auth is enabled, which for a report-only guard is a silent dead measurement.
