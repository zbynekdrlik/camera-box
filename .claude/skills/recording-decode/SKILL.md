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

## Per-frame latency CSV pairing (#209/#216) — optical-read dropout ≠ chain loss

`recording_latency::per_frame_latency_csv_rows` builds the #209 continuous-line CSV. The header
(SINGLE source of truth = `LatencyCsvRow::HEADER`; the Python plotter cross-checks it) is now
**7 columns** (#216 added the last):
`frame_id,gen_ts_ns,flip_ts_ns,cam1_strih_ms,strih_stream_ms,cam1_stream_ms,cam2_cam1_ms`.
Two distinct QR classes drive a row:

- the **node BURNS** (cam1/strih/stream) give the THREE burn-hop columns — these need ONLY the
  burns, paired WITHIN one frame. They survive any optical dropout (burns are digitally generated)
  → these three lines draw CONTINUOUSLY (a gap there = a real chain loss).
- the **cam2 OPTICAL tick** (`frame_id`, `Option<u32>`) is the per-frame optical identity, the
  `flip_ts_ns` key, AND the cam2 reference for `cam2_cam1_ms`. It can go undecodable for a stretch
  (cam1's camera briefly fails to OPTICALLY READ cam2's monitor QR) — an **optical-READ dropout**,
  a real readability failure on the cam2→cam1 OPTICAL-injection leg, NOT a chain frame loss.

**#216 rule (honest, NOT a cover-up):** a row is emitted whenever the frame has a valid x-anchor
(positive cam1/strih burn or cam2 paint stamp), even when the cam2 tick is absent — so the three
BURN lines stay UNBROKEN across a dropout. BUT the **`cam2_cam1_ms` column (= cam1_capture −
cam2_display, flip #194 else paint #179) is EMPTY on those frames — the HONEST GAP**. The plotter
(`latency-line-report.py`) draws cam2→cam1 dashed with a TRUE NaN break + a shaded annotated
window; the verdict JSON reports `cam2_cam1_optical_read_dropouts` (windows/duration via
`recording_latency::optical_read_dropouts`, ≥2 s floor). NEVER draw the optical line across the
gap — that was the reverted #216 cover-up. Burn hops continuous + optical line gapped = the honest
picture.

## #216 GOTCHA — cam1 burn "over-count" is a contiguity-WALK reorder artifact, NOT a CRC gap

The 30-min proof reported 235 cam1 `real_drops`. **CRC is a red herring:** `Payload::decode`
(src/probe/payload.rs:46) ALREADY validates CRC32 — a HEVC-corrupted read fails CRC → `None`
(BURN-UNREADABLE), never a wrong VALID id. The real cause: read the proof JSON —
`cam1.present_count == expected_count == (last_id − first_id + 1)` AND `burn_unreadable == 0` ⇒
EVERY emitted integer WAS present, nothing lost. Yet `real_drops > 0`. The per-emit forward-gap
walk (`burn_contiguity_in_window`) flags ids by recorded-frame ORDER, so when the softened stream
recording (2 NDI hops + 2 HEVC re-encodes of the small QR — **NOT a 4K upscale; both boxes record
1080p, #196 premise invalid**) delivers an id one frame LATE (a 60→30 straddle reorder), the walk
manufactures a phantom drop. **Fix (burn_contiguity.rs):** build the global present-set; for cam1
(PerEmittedFrame) skip any forward-gap / backward-jump id that appears ELSEWHERE in the set (a
reorder, not a lost frame); only a genuinely-absent integer counts. Per-render stays strict.
Diagnostic tell: `present_count == span == burn_ids_present` with `real_drops > 0` ⇒ over-count,
not loss.

## #226 GOTCHA — blurred-but-PRESENT cam1 burn is BURN-UNREADABLE, not REAL DROP (the DUPLICATE case)

The #216 fix above handles a reordered id that appears ELSEWHERE in the present-set. #226 is the
DISTINCT case its diagnostic tell flags: `present_count == expected_count` (NO `None` frame),
`burn_unreadable == 0`, yet `real_drops > 0` on a periodic decode beat (run 1924001: 107 phantom).
The blurred cam1 burn on a DELIVERED frame can't become a wrong-but-valid id (CRC), so it re-decodes
to a CRC-valid **DUPLICATE of a neighbor id** — its own id is then absent from the set with NO
`None` charging it, so #216's set-based loop leaked it as a phantom REAL DROP.

**DISCRIMINATOR (the deep-review catch — do NOT count all duplicates):** the per-emit chain ALSO
legitimately re-samples a PRESENT id onto several recorded frames via the 60→30 beat (a BENIGN
oversample — the DOMINANT duplicate source). A global "duplicate budget"
(`present_ids.len() − present_set.len()`) MASKS genuine drops that merely coexist with oversamples
(false-negative on real loss — the exact distinction the user trusts). The honest signal is
**PER GAP**: a recorded frame that sits STRICTLY BETWEEN two DIFFERENT present ids (in recorded
order) is a misdecode that fell in that gap; a benign oversample sits AT its own id's position
(before/adjacent, not inside a gap) and credits nothing. **Fix (burn_contiguity.rs):** walk recorded
order tracking interstitial frames since the last forward step; a frame counts as interstitial ONLY
if its id was ALREADY seen (a genuine DUPLICATE — the misdecode fingerprint), NOT if it is a
present-set member arriving late/out of order (the #216 reorder — a single occurrence accounted for
by its own presence; counting it would mislabel a genuine drop in a LATER gap). When a forward gap
opens, the accumulated count is the gap's BURN-UNREADABLE budget. Spend it on the gap's absent ids
(lowest first) → BURN-UNREADABLE; the rest of the gap → genuine REAL DROP. **Label-only:**
`missing_ids` is unchanged, so the verdict
still FAILS (`is_zero` = no missing id) on any absent id — the fix can never create a false ZERO, it
only moves an honest not-zero id from REAL-DROP to BURN-UNREADABLE. Diagnostic tell:
`present_count == expected_count` AND distinct-set < span AND `real_drops > 0` ⇒ duplicate-misdecode
over-count (blurred-but-present burns), a burn-readability defect, not loss.

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

## The LIVE verdict path vs the OLD loss helpers (#197 cleanup — what's actually called)

The headline loss verdict is the **per-node burn-id contiguity** check
(`burn_contiguity::burn_contiguity_in_window` → `NodeContiguity`, consumed by the
`recording-verdict` binary's `NodeVerdict`) + the V4L2 capture-drop sidecar. That is the
ONLY production loss path. The older set-compare / hop-verdict helpers were removed in #197 as
dead code — do NOT resurrect them or pattern-match off them:

- GONE: `recording_verdict::{strih_stream_verdict, StrihStreamVerdict, burn_hop_verdict,
  BurnHopVerdict, overlap_set_verdict}`, `recording_latency::chain_hop_loss_from_stream`, and
  the whole `recording_4node` module. They survived only via their own unit tests.
- STILL LIVE (don't confuse the look-alikes): `recording_latency::chain_hop_samples_from_stream`
  (LATENCY, not loss), `burn_ids_in`, `cam2_cam1_samples*`, `write_latency_csv`;
  `recording_verdict::{verdict, cam_strih_assessment}`. All binary-reachable.

### Safe dead-code removal procedure (cost real time the FIRST time)

A verdict "loss" function rarely stands alone — it forms a **call cluster**: e.g.
`burn_hop_verdict` and `chain_hop_loss_from_stream` both delegated to `overlap_set_verdict`,
which returned `BurnHopVerdict`. A symbol is only dead if its WHOLE cluster is dead. Steps:
1. For EVERY candidate, grep all refs and classify each: definition / `///`+`//!` doc-link /
   `#[cfg(test)]` body / `use` import / real production call. A fn called only by another fn
   that is itself test-only is dead; reachable-from-`src/bin/` is LIVE. Dispatch `ticket-validator`
   for the transitive call-graph — a glance is not enough.
2. **Cross-reference hazard:** removing fn X breaks any *kept* test that calls X (e.g. the kept
   `independent_burn_counters_give_zero_overlap_the_181_bug` called the dead `burn_hop_verdict`).
   Search dead-symbol refs across ALL test fns, not just the obvious ones — remove those tests too.
3. **Orphan cleanup (clippy `-D warnings` WILL fail CI otherwise):** a removed fn leaves
   orphaned `use` imports (`use …BurnHopVerdict`, an over-broad `BTreeSet`) and now-unused test
   helpers (`loss_frame`) — `cargo check --all-features` flags them as `unused`/`dead_code`. Fix.
4. **rustdoc intra-doc links:** strip/rewrite any surviving `[`removed_item`]` doc-link (module
   `//!`, and docs on kept items). CI does NOT run `cargo doc -D warnings`, so it won't hard-fail,
   but it's real bit-rot — fix it.
5. Verify CI-equivalent locally (Tier-0): `cargo fmt --all --check` + `cargo check --all-features
   --all-targets` + `cargo clippy --all-targets --all-features -- -D warnings` + `cargo test
   --no-run --all-features`. The contiguity (#216/#226) + verdict tests must stay green in CI.

### Pre-push hook on a pure-deletion / cleanup commit

A dead-code-removal commit changes `.rs` but ADDS no tests, so pre-push Gate-1 ("feature .rs
changed, no test added") AND Gate-2 (`Closes #N` ⇒ expects a RED test) both fire. This is NOT a
bug fix — it's the documented `[no-test: <reason>]` case. Put the marker (e.g.
`[no-test: dead-code removal, no behavior change — remaining tests cover it]`) on the **LATEST**
commit of the push (the hook reads `git log -1`). Never `[no-test:]` a real fix.
