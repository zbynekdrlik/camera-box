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

## Per-frame latency CSV pairing (#209/#216) — optical-decode dropout ≠ chain loss

`recording_latency::per_frame_latency_csv_rows` builds the #209 continuous-line CSV
(`frame_id,gen_ts_ns,flip_ts_ns,cam1_strih_ms,strih_stream_ms,cam1_stream_ms`). Two distinct QR
classes drive a row:

- the **node BURNS** (cam1/strih/stream) give the THREE hop columns — these need ONLY the burns,
  paired WITHIN one frame. They survive any optical dropout (the burns are digitally generated).
- the **cam2 OPTICAL tick** (`frame_id`, `Option<u32>` since #216) is the per-frame optical
  identity AND the only key for `flip_ts_ns`. It can go undecodable for a stretch (cam1 filming
  cam2's monitor briefly fails to read the QR) — an **optical-DECODE dropout**, which is a
  MEASUREMENT gap, NOT a chain loss.

**#216 fix / rule:** a row is emitted whenever the frame has a valid x-anchor (a positive
cam1/strih burn or cam2 paint stamp), EVEN WHEN the cam2 tick is absent — so the three burn-hop
lines stay UNBROKEN across an optical dropout (`frame_id` empty, `flip_ts` empty, three hops
filled). Only a frame with NEITHER a cam2 tick NOR any positive stamp is skipped. The earlier
"skip the whole row when no cam2 tick" blanked all three lines for a ~150s stretch (#216 band).

**Diagnosing a latency-CSV gap (do this FIRST, no rig needed):** read the artifacts on disk —
`proof-*/latency-per-frame.csv` + `verdict-clean.json`. `stream_frames − csv_rows == nodes.stream
.undecodable` ⇒ the gap is undecodable (no cam2 tick) frames, not lost frames. Check the cam2
`frame_id` jump vs the `gen_ts_ns` jump: if `tick_jump / 60 ≈ gen_ts_gap_seconds`, the cam2
painter kept running (optical-decode dropout), not a chain stall. cam1 burn `present_count ==
expected_count` with no clustered missing run confirms the burns were present the whole time.

## GOTCHA — pre-push Gate-1 false-positives on inline Rust tests

The airuleset `pre-push-test-check.sh` Gate-1 ("feature .rs changed but no test file") keys on the
PATH (`(test|spec|e2e|playwright)`), so a PR that adds only INLINE `#[cfg(test)]` tests inside a
`src/*.rs` is wrongly flagged. Gate-2 (the real RED-before-GREEN ordering) DOES detect inline-test
diffs and passes. Fix: add a genuine integration test under `tests/` (real coverage, not a
workaround) — e.g. `tests/recording_latency_decode.rs` renders QR pixels + decodes through the
real rqrr path. That satisfies Gate-1 by path AND adds end-to-end value. Never `[no-test:]` a real
fix to dodge it.
