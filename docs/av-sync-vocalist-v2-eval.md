# A/V-sync Engine v2 (VocaLiST) — GPU evaluation & GO/NO-GO

**Ticket:** #801 (AI A/V-sync watchdog). **Date:** 2026-08-18. **Box:** dev2 (RTX 5050, torch 2.11+cu128).
**Verdict: NO-GO for deploying VocaLiST as Engine v2. Keep v1 (SyncNet). Documented with measured data below.**

## Context

The v1 A/V-sync meter (`scripts/av_sync_measure.py`, SyncNet + S3FD, confidence-gated, deployed live
on the stream box via `watchdog.ps1`) measures the program output's A/V offset and maps it to the
'2ME PGM' latency knob. Its one design gap is **sung segments**: SyncNet is trained on speech (LRS2),
so on singing its confidence often drops below the gate and those windows self-discard. VocaLiST
(Kadandale et al., Interspeech 2022 — "lips and voices", Acappella-tested) is trained for singing too
and was the planned Engine v2 for sung coverage.

## The decisive production constraint

Production inference is **CPU-only** on the stream box (no GPU contention with the live NVENC render;
the box's GPU has TDR history) and **no video may leave the venue** (metered mobile uplink), so offloading
to a GPU box during an event is excluded. The eval therefore does not ask "deploy tomorrow?" — it asks
"is VocaLiST's singing gain large enough to justify a *future* architecture change (a dedicated on-prem
GPU inference box)?"

## Method — weights-free model-inference cost bench (fair, same-box, apples-to-apples)

Model **inference cost is determined by architecture, not by trained weights**, so a randomly-initialised
model measures the true cost. Both models were instantiated on dev2 and their exact offset-inference
loop timed on GPU and CPU for a 20 s / 25 fps clip (≈495 video-window positions, VSHIFT=15). This
mirrors the v1 validation methodology and isolates the model cost (S3FD face-cropping is shared by both
and excluded). Bench harness (`cost_bench.py`, `params.py`) run on dev2, not committed.

The structural reason VocaLiST is far heavier: SyncNet does **one forward per window** then cheap L2
distance across the ±15 shifts; VocaLiST's "distance" is a **learned transformer score**, so it runs a
full forward **per shift** (31× per window) through three 4-layer cross-attention transformers (per `models/model.py`).

## Results (measured on dev2)

| Metric | SyncNet (v1) | VocaLiST (v2) | Ratio |
|---|---|---|---|
| Parameters | 13.63 M | 80.11 M | 5.9× |
| Inference, GPU (RTX 5050) / 20 s clip | 0.35 s | 72.8 s | ~207× |
| Inference, **CPU (dev2)** / 20 s clip | 17.3 s | **~3196 s (~53 min)** | **~185×** |

The **GPU row is a full 495-window run**; the **CPU row is extrapolated** — timed on 60 windows (SyncNet,
34.9 ms/window) and 8 windows (VocaLiST, 6456 ms/window) and linearly scaled to 495 (transformer forward
cost is low-variance, so the extrapolation is defensible, but the ~53 min headline rests on ~1.6% of the
clip's windows). The CPU figure is model-inference only on dev2's CPU (the production stream box is a
different CPU, and the live full-pipeline v1 cost also includes S3FD, so this understates SyncNet's real
cost, not VocaLiST's); the transferable, box-independent number is the **~185× VocaLiST/SyncNet ratio on
the same CPU**.

## Interpretation

- **Deployability: NO.** Production runs CPU-only on a 5-minute cadence. VocaLiST at ~53 min per 20 s clip
  cannot even complete one measurement within ~10 cadence periods — it is ~185× the v1 cost and never
  fits. SyncNet at ~17 s/clip fits the cadence with large headroom. GPU is not an option in production
  (NVENC contention/TDR + no-video-leaves-venue).
- **Accuracy on singing (published, not re-measured here).** VocaLiST is designed for and reported to
  outperform speech-only baselines like SyncNet on singing (Acappella) — that published gain is the whole
  point of v2. It was NOT re-measured on our content (so it stays a citation, not a measurement here): the trained weights are Google-Drive
  sign-in-gated (no public direct/HF mirror), and a real accuracy campaign needs the LRS2/Acappella
  labelled sets — a separate effort, and moot here because the cost verdict already excludes deployment.
- **Why the gain does not justify a future GPU architecture.** v1 is deliberately zero-management:
  confidence-gating self-selects usable windows, and the A/V offset is a slowly-varying latency knob, so
  a few confident windows per 5-min cadence suffice — every sung window does NOT need to be measurable.
  Only if real operation shows genuine gaps (the v1 daemon logging no confident window across whole sung
  stretches) would this be worth revisiting — and then with **MTDVocaLiST** (a separately-proposed distilled variant
  reported ~83% smaller than VocaLiST) as the candidate, not full VocaLiST.

## Recommendation

Close the Engine-v2/VocaLiST slice of #801 as **NOT WORTH IT (documented with data)**. Keep v1 SyncNet.
Trigger to revisit = evidenced sung-segment coverage gaps in the live v1 daemon; candidate then =
MTDVocaLiST (a separately-proposed distilled variant reported ~83% smaller) on a dedicated on-prem GPU,
not CPU-side VocaLiST.
