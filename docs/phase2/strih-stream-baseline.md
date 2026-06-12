# strih→stream hop: frame-loss baseline + root cause (#21)

**Status:** strih→stream is **not** zero-loss. The loss is a quantified, currently
**irreducible** OBS render-clock artifact (pending genlock #8). This document is the
standing baseline the harness gates against; the JSON run artifacts live in
`docs/phase2/artifacts/`.

## What #21 asked

Prove or restore zero frame loss on the strih→stream hop (closed #14 saw ~39% loss,
investigated only) before #7 composes the full-path gate. Either drive
`dropped_ids` empty over ≥3 sustained runs, **or** document the quantified
irreducible bound and gate to it.

## Topology measured

```
cam2 frame-probe --paint-only  (QR id per frame, ~12 fps unique on /dev/fb0)
   → HDMI out → HDMI capture → camera-box capture→NDI  "CAM2 (usb)"      [cam tap]
   → strih OBS ndi_source → program → DistroAV NDI Main Output "2ME PGM" [strih tap]
   → stream OBS ndi_source → program → DistroAV NDI Main Output "stream" [stream tap]
```

dev1 taps all three NDI outputs and differences adjacent pairs (`cam→strih`,
`strih→stream`). Runs orchestrated by `scripts/multitap-e2e.sh`; OBS routed via
obs-websocket (`scripts/obs_phase2.py`), production program scenes snapshotted and
restored. Both OBS boxes confirmed off-air (not streaming/recording) before each run.

## Finding 1 — the loss is REAL frame loss, not QR tearing

The tap reader only recorded a frame when its QR decoded, so a frame that arrived
but whose QR was torn was indistinguishable from a frame the hop dropped. Added a
raw `captured` count (incremented before decode) per tap:

```
decode_failed = captured − decoded  ≈ 0  on ALL taps, ALL runs
```

So every NDI frame that arrived decoded cleanly. The dropped ids genuinely never
reached the downstream tap — real frame loss, not a measurement tear.

## Finding 2 — the loss is oversample-masked render-clock drop

Per-run analysis of the raw dumps (`--dump-raw`):

| run (180 s) | cam oversample | strih→stream single-copy loss | strih→stream multi-copy loss |
|---|---|---|---|
| 1 | 2.75 | 1/33  = 3.0% | 0/2121 = 0.00% |
| 2 | 2.26 | 16/353 = 4.5% | 23/1772 = 1.30% |
| 3 | 2.25 | 7/242 = 2.9% | 7/1895 = 0.37% |
| (120 s clean) | 2.50 | 0/8 = 0% | 0/1429 = 0% |

**Single-copy frames (no oversample redundancy) drop at ~3-4.5%; multi-copy frames
drop at 0-1.3%.** Loss scales inversely with oversample. The mechanism:

- camera-box, strih OBS and stream OBS each run a **free-running 30 fps clock**
  (`GetVideoSettings`: 30/1 on both boxes), none genlocked to the others.
- OBS is a compositor: it samples its NDI source on its own render tick. Two
  un-genlocked 30 fps clocks drift/jitter, so ~1-4.5% of source frames are never
  sampled — dropped and replaced by a duplicate of the adjacent frame. Output
  frame **count** stays ~constant (stream raw 5357 ≈ strih raw 5340), which is why
  count-parity hides the loss; only the unique-id content is lost.
- A unique frame survives if **any** of its oversampled copies lands on a render
  tick. The sub-fps QR painter creates ~2.5x oversample that masks most drops at
  the unique-id level — single-copy ids are the only ones that expose the true
  per-frame rate. Real 60 fps content (oversample = 1) would see the full
  ~1-4.5% loss = the operator-reported stutter.

cam→strih shows the same pattern (1.5-4.6% single-copy loss) — this is the generic
OBS-NDI-hop characteristic, not a strih→stream-specific defect. #14's ~39%
catastrophe (wrong ingest name on an unstabilised OBS) is genuinely fixed; this
residual is a different, smaller, clock-bound phenomenon.

## Finding 3 — not fixable via OBS/DistroAV config

The stream `ndi_source` exposes only `ndi_behavior` (Keepalive/Reset/Pause —
visibility, not timing), `yuv_range`, and `ndi_bw_mode` (Highest/Lowest/AudioOnly).
There is **no frame-accuracy / sync-timing knob**. The drop is at the render-clock
sampling stage, downstream of any receive buffer. Eliminating it requires cluster
**genlock / clock-truth (#8)** and a frame-accurate path (#7 / #11), not a setting.

## Resolution — strict zero-loss by default, documented bound opt-in (#35)

- The differ now reports `single_copy_total` / `single_copy_dropped` — the
  oversample-independent per-frame-loss estimate — so a high-oversample run can no
  longer false-green (it previously could show `dropped_ids=0` while dropping
  frames). See #29 for the min-sample guard / full-fps painter that makes the green
  fully trustworthy.
- **The canonical gate is STRICT zero-loss (#35).** `multitap-e2e.sh` passes NO
  `--max-loss-pct` by default, so `multitap-probe` fails on ANY dropped frame at ANY
  hop. The ~0.5-4.5% OBS render-clock loss documented above is REAL frame loss, so the
  strict run is EXPECTED to FAIL on the current rig — and that failure is the forcing
  function for the reason-fix (#8 genlock / #7 full-path gate / #11 60 fps capstone).
  A green test while frames drop would be a lie; #21's earlier ≤10% default was exactly
  that and is removed.
- The #21 documented bound (`HopInput.max_loss_pct` / `--max-loss-pct DOWNSTREAM=PCT`,
  hop passes when single-copy loss `<= PCT`) is **not gone — it is now an explicit
  OPT-IN only**, for tracking progress as the clock work lands:
  `MAX_LOSS_STRIH=10 MAX_LOSS_STREAM=10 ./scripts/multitap-e2e.sh` re-enables the
  bounded gate per hop. Tighten the opt-in bound toward 0 as #8 lands; the default
  never relaxes off strict.

## Acceptance-criteria mapping

- **Reproduce + quantify ≥3 sustained runs, JSON artifacts** — `docs/phase2/artifacts/final{1,2,3}.json` (+ diagnostic `run{1,2,3}.json`, `diag2.json`). ✅
- **Root-cause with evidence** — Findings 1-3 above; OBS 30 fps free-running render clock, no genlock, no DistroAV sync knob. ✅
- **Fix to zero OR document+gate the bound** — root-caused + quantified; the canonical gate is now STRICT zero-loss (#35, default fails on any drop), with the ≤N% documented bound kept as an opt-in progress tracker. True zero-loss tracked by #8/#7/#11. ✅
- **Per-hop latency captured (report-only first)** — strih→stream rel-latency p99 ~285-500 ms (mean ~190-254 ms), report-only; cam→strih rel-latency is receiver-scheduling noise (often negative) and is NOT gated. ✅
- **OBS changes via MCP/WebSocket, snapshot before/after** — obs_phase2.py over obs-websocket; program scenes saved + restored each run; before-snapshot recorded. ✅

## Harness honesty — single-copy guard + decode-race fix (#29, #30)

- **#29 oversample-masking guard — RESOLVED.** A passed loss gate is now only
  CERTIFIED (verdict `Pass`) when the hop carried at least its `--min-single-copy`
  oversample-independent frames; below that the verdict is `Inconclusive` (exits
  non-zero, distinct from a regression `Fail`), so a lucky high-oversample run can
  no longer false-green. The guard is **per-hop** (keyed by downstream tap, like
  the latency/freeze/loss gates) because the single-copy yield is sharply
  hop-dependent and, on the second hop, NOT duration-stable:

  | hop | single-copy frames (run1/2/3 @120s, run4 @300s) |
  |---|---|
  | cam→strih | 48 / 68 / 49 / 60 — reliably ~50-68 |
  | strih→stream | 12 / 63 / 17 / **2** — starved, often too few |

  At the 12 fps painter this starved strih→stream (2 single-copy on the 300 s run4),
  so it was originally left **UNGATED** while cam→strih guarded at 20. **#32 (below)
  then made full-rate painting feed both hops abundant single-copy, so BOTH are now
  gated at 100** (see the #32 section for the rig measurements).
- **#30 first-run `decode=0` race — RESOLVED.** `multitap-e2e.sh` now brings the
  camera-box `CAM2 (usb)` NDI sender up FIRST and only then wires strih/stream's
  `ndi_source` to it, so OBS binds to the live sender that persists for the whole
  run instead of one restarted mid-setup. Verified by consecutive cold-restart
  runs with all taps decoding well above `min_frames`.

## Full-rate painter — strong single-copy guard (#32) — RESOLVED

The issue's premise (the painter is render-capped at ~12 fps and needs a faster
painter — atlas / GPU / lower-res QR) was **falsified by measurement** on the live
rig (cam2 is x86_64, not a weak SoC):

- `frame-probe --paint-only` sustains the requested rate exactly — 30.1 fps at
  target 30, 60.1 fps at target 60, render ceiling ~156 fps. The 12 fps figure was
  just the **coverage-mode default**, never a cap. No atlas/GPU needed.
- The real binding constraint is the **QR decoder**, and only at the cam2 60 fps
  *capture* loopback: rqrr caps ~37 fps at qr_size 700, so at oversample 1 a skipped
  capture became a false single-frame loss (7% at 30 fps, 37% at 60 fps on the cam2
  loopback). Shrinking the QR lifts decode (qr 300 → 56 fps), but…
- …the **binding strih→stream hop runs at 30 fps NDI, where qr 700 decodes 100 %**
  (`decode_failed=0`). So painting at the 30 fps pipeline rate with the *unchanged*
  qr_size 700 already gives oversample→1 and abundant single-copy. A smaller QR is
  unnecessary and would only risk robustness across the NDI/OBS compression hops.

Live-rig measurement (60 s taps, `PAINT_FPS=30`, qr 700), two consecutive runs:

| hop | up_unique | single-copy | per-frame loss | decode_failed |
|---|---|---|---|---|
| cam→strih | ~1640 | **~1210** | 0–0.08 % | 0 |
| strih→stream | ~1770 | **~1760** | 0.46–0.56 % | 0 |

(vs 12 fps: cam→strih 48–68, strih→stream 2–63.) The gated config
(`--max-loss-pct 10` + `--min-single-copy 100` on both hops) verified **PASS** end
to end. So `multitap-e2e.sh` now paints at `PAINT_FPS=30` and gates **both** hops at
100 single-copy — the floor to certify ~<5 % per-frame loss at ~95 % confidence,
met many times over (≥1200 per 60 s). The 60 fps capstone (#11) still exceeds the
decoder even at qr 300 and remains future work (faster/parallel decode).

## True-zero-loss path (tracked, not this issue)

- **#8** cluster NTP/PTP genlock — removes the free-running-clock drift that causes the drop.
- **#7** full-path source→endpoint gate (clock source of truth) — consumes this baseline.
- **#11** sustain 60 fps end-to-end zero loss — the capstone bar.
