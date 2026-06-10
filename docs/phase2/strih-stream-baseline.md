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

## Resolution — documented bound + honest gate

Per #21's second acceptance branch (document the irreducible bound and gate to it):

- The differ now reports `single_copy_total` / `single_copy_dropped` — the
  oversample-independent per-frame-loss estimate — so a high-oversample run can no
  longer false-green (it previously could show `dropped_ids=0` while dropping
  frames). See #29 for the min-sample guard / full-fps painter that makes the green
  fully trustworthy.
- `HopInput.max_loss_pct` / `multitap-probe --max-loss-pct DOWNSTREAM=PCT` gates a
  hop on single-copy loss `<= PCT` instead of strict any-drop-fails. The harness
  ships a standing bound of **10%** on both OBS hops — ~2x the observed ~4.6%
  ceiling, far below the #14 catastrophe (~39%) — so the chain passes at the
  documented floor and FAILS on regression past it. Tighten as #8 lands.

## Acceptance-criteria mapping

- **Reproduce + quantify ≥3 sustained runs, JSON artifacts** — `docs/phase2/artifacts/final{1,2,3}.json` (+ diagnostic `run{1,2,3}.json`, `diag2.json`). ✅
- **Root-cause with evidence** — Findings 1-3 above; OBS 30 fps free-running render clock, no genlock, no DistroAV sync knob. ✅
- **Fix to zero OR document+gate the bound** — documented bound + single-copy gate (10%); true zero-loss tracked by #8/#7/#11. ✅
- **Per-hop latency captured (report-only first)** — strih→stream rel-latency p99 ~285-500 ms (mean ~190-254 ms), report-only; cam→strih rel-latency is receiver-scheduling noise (often negative) and is NOT gated. ✅
- **OBS changes via MCP/WebSocket, snapshot before/after** — obs_phase2.py over obs-websocket; program scenes saved + restored each run; before-snapshot recorded. ✅

## Known harness limitations (filed, not this issue)

- **#29** oversample masks per-frame loss — a high-oversample run can show
  `dropped_ids=0` while frames drop; the single-copy metric exposes it but needs a
  min-sample guard / full-fps painter to make a green fully trustworthy.
- **#30** intermittent first-run `decode=0` — strih's `ndi_source` is wired before
  the camera-box NDI sender restarts, so a cold run can bind to the dead sender and
  decode nothing (the gate correctly FAILs it on min_frames — never a false green).

## True-zero-loss path (tracked, not this issue)

- **#8** cluster NTP/PTP genlock — removes the free-running-clock drift that causes the drop.
- **#7** full-path source→endpoint gate (clock source of truth) — consumes this baseline.
- **#11** sustain 60 fps end-to-end zero loss — the capstone bar.
