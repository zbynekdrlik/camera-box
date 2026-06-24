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

FIXED in airuleset (#170 session): Gate-1 ("feature .rs changed but no test file") used to key
ONLY on the PATH (`(test|spec|e2e|playwright)`), so a PR adding only INLINE `#[cfg(test)]` tests
inside a `src/*.rs` was wrongly flagged. Gate-1 now ALSO scans the branch-diff ADDED lines for
`#[test]`/`assert!`/`fn test_` (mirroring Gate-2's existing inline detection), so a normal Rust
inline-test PR passes. If a fresh airuleset checkout ever re-blocks: an integration test under
`tests/` also satisfies it by path — but NEVER `[no-test:]` a real fix to dodge the gate.

## GOTCHA — grab-ts sidecar CSV parse policy (`parse_grab_ts`, recording-verdict.rs)

`parse_grab_ts` (`frame_index,grab_ts_ns`) has THREE distinct row outcomes — don't collapse them:

- **Kill-time partial trailing fragment** (file does NOT end in `\n`; the cam1 `--record-grab`
  BufWriter is killed with no flush) → the single final line is skipped whatever its shape.
- **Empty `grab_ts_ns` cell** (`<idx>,` newline-terminated, mid-file or final, #170) → WARN +
  skip that row. An empty cell = that frame has no recorded grab instant = no cam2→cam1 pairing;
  `cam2_cam1_samples` already yields no sample for a frame absent from the map, so skipping is the
  correct, lossless outcome. Do NOT error — run-163163's verdict computed every loss hop then
  crashed at the very end (`cannot parse integer from empty string`, VERDICT_EXIT=1), losing the
  whole latency computation over ONE empty cell.
- **Wrong column count OR a NON-empty unparseable cell** (e.g. `1,abc`) on a complete row → ERROR
  loudly. That is genuine corruption; a silently-shrunk map would drop real samples with no signal.

The discriminator is empty-vs-nonempty AFTER trim, not mid-file-vs-final. Tests:
`grab_ts_sidecar_empty_ts_row_is_skipped_not_crashed` (#170 regression) +
`grab_ts_sidecar_nonempty_garbage_ts_row_still_errors`.
