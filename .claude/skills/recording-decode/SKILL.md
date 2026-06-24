# Recording-verdict QR decode path (`src/probe/qr.rs` + `src/probe/recording.rs`)

The offline recording verdict decodes every recorded frame's QR(s): the big optical
**cam2 dual-QR** (top band, always decodes full-frame) + the small ~300px **node burns**
(cam1 / strih / stream, bottom corners, run_ids `recording_latency::BURN_RUN_ID_*`).

## The decode functions (do NOT confuse them)

- `decode_qr_luma_all(img)` — plain full-frame rqrr pass (+ one Otsu-binarized retry). Cheap.
  Reads the big dual-QR on every well-formed frame and ~99 %+ of node burns on a clean rec.
- `robust_tile_passes(&img, &mut out)` — the EXPENSIVE part (#202): bottom-band split into 3
  overlapping cubic-upscaled column tiles, rqrr each, merge-dedup into `out`. ~10× the plain
  cost. Recovers the rare small burn the full-frame finder misses (the #186 coverage gap).
- `decode_qr_luma_all_robust(img)` = plain + ALWAYS the tiles. Max-robust. Used by the
  recording.rs undecodable self-check and the diagnostic dump tools.
- `decode_qr_luma_all_fast_then_robust(img, expected_burn_run_ids)` (#207) = plain FIRST; run
  the tiles ONLY when an `expected_burn_run_ids` id is missing from the plain pass. This is the
  per-frame recording decode — ~10× faster on a clean recording, identical reads.

## THE GOTCHA — pass the burns THIS recording actually carries (cost me real time)

The fast gate skips the tiles only when EVERY expected burn is already present. So the expected
set must be the burns the recording REALLY carries, NOT the full `NODE_BURN_RUN_IDS`:

- **strih recording** → `[cam1, strih]` (the stream burn is downstream; never recorded here).
- **stream recording** → `[cam1, strih, stream]` (chain endpoint, all three forwarded).
- **cam1 grab** → `[cam1]`.

Requiring all three on a strih recording would force the tiles on EVERY frame (chasing a stream
burn that was never recorded) → zero speedup. `decode_recording_frame` (the 2-arg default) keeps
the full-set / max-robust behavior for the diagnostic tools; the verdict's `ticks_of(path,
expected)` passes the per-recording set via `analyze_recording_with_burns`.

## Robust is always a SUPERSET of plain

`robust_tile_passes` only ADDS (merge-dedup by `(run_id, frame_id)`), never removes. So any
"robust must equal plain ∪ tiles" / "fast == robust" invariant holds. The node BURNS are
EXCLUDED from `RecordingFrame::tick` (which is the optical cam2 Vernier tick) — a recovered burn
must never hijack the tick.

## Regression lock — never weaken it

`tests/burn_fixture_decode.rs` + the real grayscale fixtures under
`tests/fixtures/burn-unreadable/` are the #186/#202/#207 lock: on each real frame the plain
pass MISSES the node burn, robust/fast-then-robust RECOVERS it. A present digitally-burned QR
MUST decode — a non-decoding present burn is a decoder defect, never a real drop. Any change to
the decode path must keep these green.

## Decode-path observability (#207)

`qr::decode_path_counts() -> (fast, robust)` (process-wide AtomicU64) is logged at
recording-analysis-complete so the verdict log shows `fast ≫ robust` (the speedup is real).
Counters are global/cumulative across all recordings in one run.
