# Recording-verdict QR decode path (`src/probe/qr.rs` + `src/probe/recording.rs`)

The offline recording verdict decodes every recorded frame's QR(s): the big optical
**cam2 dual-QR** (top band, always decodes full-frame) + the small ~300px **node burns**
(cam1 / strih / stream, bottom corners, run_ids `recording_latency::BURN_RUN_ID_*`).

## The decode functions (do NOT confuse them)

- `decode_qr_luma_all(img)` — full-frame rqrr pass UNION the Otsu-binarized rqrr pass, merged
  + de-duped (#363, BOTH passes always run). Cheap (~2 full-frame rqrr passes). Reads the big
  dual-QR + the SOFT optical dual-QR on every well-formed frame and ~99 %+ of node burns on a
  clean rec. (See "#363 — optical-robustness half" below: the Otsu pass used to be gated to
  run ONLY when the plain pass was empty, which silently dropped the soft optical when burns
  decoded — the bug this PR fixed.)
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

**This gotcha bites TESTS too, not just the production verdict (#423).** Any test that builds
its own synthetic frame and decodes it via `decode_recording_frame` (the 2-arg default, always
requires the FULL `NODE_BURN_RUN_IDS`) or passes the full set into `decode_stream_parallel` /
`decode_recording_frame_with_burns` pays the ~10× tile cost on EVERY call if the frame doesn't
actually carry all three burns — with no payoff, since the tiles can never find a burn that was
never rendered. This is exactly what made `probe::recording::tests::
parallel_decode_matches_single_threaded_result_exactly` take >300s (its `dual_qr_luma` frames
carry ONLY the optical dual-QR, zero node burns) and made
`tests/recording_latency_decode.rs::dual_vernier_cam2_real_pixels_canonical_tick_and_both_hops`
take ~50-59s for just 8 frames (each frame carries exactly ONE node burn). Before writing a new
recording-decode test: pass exactly the burn set the test's own frame-builder actually renders
(often `&[]` for optical-only synthetic frames), not the full `NODE_BURN_RUN_IDS` — same rule as
production, same fix (`&[]` / `&[the-one-burn]` instead of `decode_recording_frame`'s default).

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

**These fixtures also LOCK the burn WIRE FORMAT, not just the decoder.** They are real recordings
carrying `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}` and the test decodes them via `Payload::decode`.
So you CANNOT change the payload format (e.g. to shrink the QR matrix for a cheaper burn) off-rig —
it breaks these fixtures, which only a fresh rig recording run can regenerate. The #275 burn-render
speedup (`vendor/distroav/src/burn-qr.hpp` bulk row/run fills) deliberately keeps the OUTPUT BYTES
identical (white `FF FF FF FF`, black `00 00 00 FF`) precisely so this lock stays valid — see the
genlock skill "#275" section.

## #275b — cam1 capture-burn renders ASYNC (off the emit loop), or cam1 caps at 30fps

The #174 cam1 capture-burn (`src/probe/qr.rs::burn_qr_yuyv` → `render_payload_qr`) used to render
the per-frame QR SYNCHRONOUSLY on the capture/emit loop (`src/main.rs` `process_frame`), between
the genlock emit-gate and the NDI send. That per-frame render is too heavy for the 16.6ms 60fps
budget → cam1's NDI emit capped at **30fps** (`30.0 emitted / 62.5 captured, 0 dropped`; prod = 60),
so the chain couldn't be MEASURED at 60 (#11). **The burn path is MEASUREMENT-ONLY** (cargo feature
`probe` + env `CAMERA_BOX_BURN_RUN_ID`) — production is the unchanged zero-copy send, so this never
touches the live broadcast.

**The fix (the design the #275 first pass DROPPED as "risky" — now resolved):** a dedicated
`cam1-burn` thread fed by a bounded FIFO ring (`src/probe/genlock.rs`: `BurnFrameIdSource`,
`BurnRing`, `burn_ring`, `run_burn_ring`, `BURN_RING_DEPTH=3`). The burn thread OWNS the single
NDI sender. The 3 hazards the first pass feared, and how they're each handled — DON'T re-derive:

- **gen_ts can't be "pre-rendered ahead" (it's per-frame).** Don't try to. The capture thread
  stamps `frame_id` (monotonic `BurnFrameIdSource`, drawn once per emit IN emit order) + `gen_ts_ns`
  (emit-instant wall clock) + the emit timecode AT THE GATE, copies the frame, and submits the WHOLE
  job; the burn thread renders that job's QR (full payload already known). The pipeline overlaps
  render(N+1) with send(N) — no pre-render-without-ts needed.
- **genlock pacing jitter from sending on another thread.** The NDI timecode is computed on the
  CAPTURE thread at the gate instant (`ndi::boundary_timecode_100ns(fps)`) and CARRIED to the send
  (`NdiSender::send_frame_data_with_timecode`) — NOT re-derived on the burn thread. So the stamped
  timecode is the emitted frame's genlock boundary, immune to burn-thread queue jitter. (`send_frame_data`
  is now the timecode-computing wrapper for the normal path.)
- **1:1 burn-id↔emit under back-pressure.** `BurnRing::submit` uses a BLOCKING `sync_channel` send —
  it back-pressures the capture thread when the ring is full, NEVER drops. A dropped/reordered job
  would punch a burn-id GAP the verdict misreads as phantom chain loss. The RED→GREEN lock is
  `genlock.rs::async_burn_ring_preserves_1to1_mapping_in_order_under_backpressure` (RED = dropping
  `try_send` loses 497/500 under a slow consumer; GREEN = blocking `send` delivers all in order with
  the carried timecode). Shutdown: drop the ring (closes the channel) → join the thread → flush the
  tail + destroy the sender cleanly.

Throughput is then `min(emit-gate rate, burn-thread rate)`; if the rig E2E still shows <60, the next
lever is the DEFERRED payload-shrink (blocked by the wire-format fixture lock above — needs a rig
recording run to regen). The async move alone is what unblocks the 60fps cam1 leg.

**#280 — the capture→burn frame copy reuses a BOUNDED BUFFER POOL (no per-frame to_vec).** The mmap
is valid only inside the V4L2 callback, so a copy IS required to cross to the burn thread; #275b did
it with a per-frame `data.to_vec()` (~4MB at 1080p YUYV → a fresh heap alloc+free EVERY emitted
frame at 60fps). #280 adds `genlock.rs::BufferPool` (`Mutex<Vec<Vec<u8>>>` free list + `AtomicUsize`
alloc counter, cap `BURN_POOL_CAP = BURN_RING_DEPTH + 2 = 5`): capture `take()`s (reuse, or alloc
only when empty), `clear()`+`extend_from_slice` the frame (reuses the ~4MB capacity → no realloc),
submits; the burn thread `put()`s the buffer back AFTER the NDI send. **The pool carries NO frame
identity — it is a memory optimization ONLY, so it CANNOT change the frame ORDER, the 1:1
burn_id↔emit mapping, or the carried gate-stamped timecode (all stamped on the capture thread, in
the job). Keep it that way — never thread identity through the pool.** RED→GREEN lock (genlock.rs):
`buffer_pool_recycles_a_returned_buffer_instead_of_reallocating` +
`pooled_async_burn_recycles_buffers_and_preserves_1to1_ordering_under_backpressure` (the latter is
the #275b 1:1 harness with pooled buffers — proves recycle AND ordering together). The #275b/#279
async-burn tests must stay green.

## Decode-path observability (#207)

`qr::decode_path_counts() -> (fast, robust)` (process-wide AtomicU64) is logged at
recording-analysis-complete so the verdict log shows `fast ≫ robust` (the speedup is real).
Counters are global/cumulative across all recordings in one run.

## #208 per-box decode-in-place — the #186 pixel proof MUST be written ON-box

The verdict needs the strih recording (cam1 contiguity #133 + cam→strih) AND the stream recording
(full chain). #208 decodes each recording IN PLACE on its own box (`--extract-partial <box>` →
small partial JSON of ids+timestamps) and merges the partials on dev1 (`--merge-partials
strih=… stream=…`) — a recording is NEVER copied box-to-box nor to dev1.

**THE GOTCHA (#186 regression that hid here):** the merge runs on dev1 where NO recording exists,
so it CANNOT extract pixel proofs. If `--extract-partial` only writes the JSON, the #186 "SEE the
missing/undecodable frame" guarantee silently vanishes (the merge can't re-extract). So
`--extract-partial` MUST write the pixel proofs ON-box (`extract_partial_flagged_frames` selects
the same flagged frames the merge flags) into a sibling `<partial>-pixels` dir; the planner scripts
pull that dir back beside the partial; `run_merge` → `report_pulled_back_pixel_proofs` points at the
real dev1 path. NEVER print "pixel proofs written on the box" without actually writing them.

**Per-box authoritative ownership for pixel proof** (mirror `build_and_print_verdict`'s node
sourcing — get it WRONG and the PNGs don't match the merge-flagged slots):
- **strih box → cam1** (PerEmittedFrame; cam1's burn is crispest in the clean 1080p strih rec, #133).
- **stream box → strih + stream** (PerRenderTick; their burns are co-located with cam2 only there).
- Each box ALSO extracts its recording's UNDECODABLE frames (the `report_recording_diag` set).
- Stream-ONLY merge (no strih partial) has NO cam1 pixel proof → `run_merge` WARNs (cam1's clean
  source is the strih recording). The production two-box flow always supplies both.

**Merge consistency:** `args_expected_burns_for(box, args)` is the ONE source of truth for a box's
expected burns (strih=[cam1,strih], stream=[cam1,strih,stream]); `--extract-partial` decodes for it
and `run_merge` WARNs on a partial whose `expected_burns` disagree with the merge args (a manual
`--burn-*-run-id` mismatch between extract and merge would misverdict) and on a repeated box key.

## Per-box decode — rig EXECUTION gotchas (win-* MCP, cost real time 2026-06-26)

The harness only EMITS the per-box plan (`VERDICT_ON_STREAM=1`); you RUN it via the win-* MCP. Hits:

- **`recording-verdict.exe` spawns `ffprobe`/`ffmpeg` by name (PATH).** The **stream box** has them
  (WinGet `Gyan.FFmpeg`, shimmed in `…\WinGet\Links`). The **strih box does NOT** → the strih
  `--extract-partial` dies instantly with `spawn ffprobe … program not found` (err log, empty out).
  Fix: copy the real binaries (the WinGet `…\Packages\Gyan.FFmpeg…\ffmpeg-*-full_build\bin\
  ff{probe,mpeg}.exe`, ~201 MB each) from the stream box to **`C:\camera-box\` on strih** over SMB
  (`Copy-Item \\10.77.9.204\C$\…` — box↔box pass-through auth works, same `newlevel` account), then
  launch the decode with `$env:PATH="C:\camera-box;"+$env:PATH`. (They now live at `C:\camera-box\
  ff{probe,mpeg}.exe` on strih — leave them for future runs.)
- **Run the decode DETACHED + poll** (a 300s 1080p decode ≈ 2–3 min; 4K ≈ 7 min): `Start-Process …
  -PassThru -NoNewWindow -RedirectStandardOutput …`. The MCP `Shell` call itself often **times out
  at 30s even though the process launched and keeps running** — don't relaunch; verify with
  `Get-Process recording-verdict` + tail the `.out` (`recording decode progress frames_read=`).
- **QUOTE space-bearing recording paths inside `-ArgumentList`** — wrap the path element in embedded
  double-quotes (`'"D:\_REC\2026-06-26 17-03-17.mkv"'`); both rig recording dirs have spaces
  (`D:\_REC\<date time>.mkv`, `…\_NLMEDIA stream\RECORDINGS\<date time>.mp4`).
- **Pull results to dev1:** small individual PNGs (~320 KB) FileDownload fine; a **>~3 MB file
  (the partial JSON, a pixels .zip) overflows the tool context → it's saved to a tool-result file**
  whose `.result` is `[task:ID] base64:<N>bytes:<b64>` — decode with
  `jq -r '.result' FILE | sed -E 's/^\[task:[0-9a-f]+\] base64:[0-9]+bytes://' | base64 -d > out`.
  A FileDownload >~9 MB can **drop the win-strih session** mid-transfer — grab individual small PNGs
  instead of the whole pixels zip when you only need to eyeball a few frames.
- **The file-drop (`airuleset.py share`) binds to the TAILSCALE IP only** — the LAN Windows boxes
  can't reach it. To push a file (e.g. the fresh `recording-verdict.exe`) to a box, run a temp
  `python3 -m http.server <port> --bind 0.0.0.0` on dev1 and `Invoke-WebRequest` it from the box
  (dev1 LAN IP, e.g. `10.77.9.165`).
- **Cleanup leaves cam1 DOWN sometimes:** the harness EXIT trap's `systemctl restart camera-box` on
  cam1 can RACE `/dev/video0` release by the burn binary and fail silently → cam1 service inactive
  after the run. ALWAYS re-verify `systemctl is-active camera-box` + `fuser /dev/fb0` on BOTH cams in
  the rig-reset step and restart cam1 if needed (a retry a minute later succeeds).

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

## #267 GOTCHA — clamp the TRAILING teardown tail only (bounded), NEVER the leading edge

`in_window_burn_frames` anchors the window to cam2's OPTICAL (QR) span. At the TRAILING edge a node's
burn can be legitimately absent while cam2's painter is still up: at SHUTDOWN the node stops emitting
its burn while cam2 keeps painting a few frames (teardown tail, run 2606010 = 23 cam1-absent frames /
~0.77 s past id 9461). Those teardown frames are optical-present / node-burn-absent but NOT lost.

**THE TRAP:** an optical-present / burn-absent edge run is **IDENTICAL in the recording** whether the
node simply ended there (legit) OR it EMITTED those ids and they were LOST in transit right at shutdown
(REAL end-of-stream loss — must FAIL). The first #267 fix popped EVERY trailing burn-absent frame
unconditionally → it **silently clamped real end-of-stream loss into a false PASS** (violates the
user's HARD zero-loss bar). **No recorded signal distinguishes the two:** the cam1 burn id IS the
per-EMITTED-frame counter and rides only its own recordings (a cam1→strih loss is absent from BOTH
strih+stream — stream is downstream); `cam1-capture-stats.frames_captured` is CAPTURE-rate (~2× the
emit rate — NOT a burn id, can't map to a last-emitted id); the `--painter` sidecar is cam2's timeline
= the already-over-extended window boundary. So the ONLY sound discriminator is the **SIZE** of the
tail.

**FIX (current):** clamp a **TRAILING** burn-absent run ONLY when it is within
`TEARDOWN_TAIL_MAX_FRAMES` (=45, ~1.5 s @ 30 fps emit, ~2× the observed 23-frame overrun). A LONGER
tail is real loss → kept → charged BURN-UNREADABLE → FAILS, never clamped. The strict #186 bar is
untouched for the in-range span: an INTERIOR burn-less frame (present burn on BOTH sides) is neither
leading nor trailing → always kept → still FAILS. Rate-agnostic. If a future legit teardown exceeds 45
frames, RAISE the bound with evidence — do NOT remove the bound.

**LEADING edge is NEVER clamped** (deep-review #2 correction). A first cut also clamped a leading
(lead-in) burn-absent run — but the lead-in case is **UNOBSERVED** on the rig, and clamping it only
opens a NEW masking window where a real ≤45-frame START-of-stream loss (the node emitted those ids;
lost in transit at startup) would false-PASS. A leading burn-absent run stays CHARGED (BURN-UNREADABLE
→ FAILS): a false-FAIL is SAFE, masking start-of-stream loss is not. If a real lead-in artifact is ever
OBSERVED, give it its own evidence-backed fix — do not pre-emptively clamp an unobserved case.

## #273 GOTCHA — the optical window must honor `--cam2-run-id` (foreign-run lead-in residue)

`in_window_burn_frames` anchors the per-node burn window to cam2's OPTICAL span via
`frame_is_delivered_optical`. That check used to count ANY non-burn payload as cam2 — so when the
recording's lead-in still carried the **PREVIOUS run's** residual cam2 paint (the strih OBS
recording started before cam2 switched to this run AND before cam1 began its burn), those
foreign-paint frames counted as "delivered", anchoring the window at frame 0. The cam1-burn-absent
lead-in was then charged as false BURN-UNREADABLE → a false zero-loss FAIL (run 2706001: 43 false
cam1 misses, `overall_pass=false`, chain actually clean).

**FIX:** `frame_is_delivered_optical(f, burns, cam2_run_id: Option<u32>)` — when pinned (`Some(pin)`),
a frame is current-run delivered ONLY if it carries a payload `run_id == pin && !burns.contains`
(the `!burns` guard is defense-in-depth: pin misconfigured to a burn id ⇒ no optical frame ⇒ empty
window ⇒ FAILS closed, never masks). Unpinned (`--cam2-run-id 0` ⇒ `None`) keeps the old "any
non-burn = cam2" (safe for the strih recording, no foreign burn). Threaded through `NodeSpec.cam2_run_id`
so BOTH the fused path AND the `--merge-partials` path get it (run_merge → build_and_print_verdict
re-derives the pin via `Args::cam2_pin()` — the single `0⇒None` source of truth). `extract_partial`
threads it too (None for the strih box, which extracts without --cam2-run-id).

**This IS "trim the leading + trailing stabilization" (user: "odkroj začiatok a koniec"):**
- **Leading** trim = run-id-precise — ONLY foreign-run residue is excluded. A CURRENT-run-paint
  frame with no burn is KEPT and CHARGED (the #267 leading-edge guarantee — a real start-of-stream
  loss must never be masked). The pin trims warm-up, NOT loss.
- **Trailing** trim = the existing bounded #267 teardown clamp (`TEARDOWN_TAIL_MAX_FRAMES`).
- **NO masking:** empty window (all-foreign / mis-pinned) ⇒ `first_id None` ⇒ `is_contiguous()` is
  `first_id.is_some() && missing_ids.is_empty()` ⇒ **false** ⇒ node FAILS (burn_contiguity.rs:63).
  A real interior drop still FAILS. Tests: `pinned_foreign_run_lead_in_is_trimmed_to_a_clean_pass`,
  `pinned_real_interior_loss_in_steady_span_still_fails` (real-drop, missing_ids=[52]),
  `pinned_real_leading_current_run_loss_still_fails`, `frame_is_delivered_optical_pin_equal_to_a_burn_id_fails_closed`.

**RED→GREEN re-merge recipe (verify a verdict-WINDOW fix against an existing run, no rig):** the
per-box partials preserve full per-frame `payloads` (incl. the foreign paint), so re-run the merge
locally with `# airuleset:build-ok` + `AIRULESET_ALLOW_LOCAL_BUILD=1 cargo build --features probe
--bin recording-verdict`, then `./target/debug/recording-verdict --merge-partials strih=… stream=…
--cam2-run-id <pin> --burn-*-run-id … --json out.json --cam1-capture-stats …` (exact flags in the
run's `harness.log`). Compare `overall_pass` + `.full_chain.loss.cam1.{first_id,last_id,missing_ids}`
vs the on-disk RED `verdict-<run>.json`. The probe path compiles ON CI ONLY — this build is the
sanctioned RED→GREEN exception, `cargo clean`/purge after.

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
5. Verify locally **DEFAULT FEATURES ONLY** (Tier-0 — NEVER `--features probe`/`--all-features`
   locally; that pulls qrcode/rqrr/image/drm/lz4 + the 5 probe bins into the shared dev1 `target/`
   and balloons it, the #185 disk-fill): `cargo fmt --all --check` + `cargo check` + `cargo clippy
   --all-targets -- -D warnings` + `cargo test --no-run`. The probe code (recording-verdict.rs +
   its tests, recording_partial, burn_contiguity, the contiguity #216/#226 + verdict tests) does
   NOT compile under default features — it is compiled + run ON CI (the "Build" / "Windows probe
   build" / "Test" jobs). CI is the only compiler for the probe path; trust it, don't compile probe
   locally. shellcheck any touched scripts; the harness structural tests (`harness_recording_e2e_paths`)
   ARE default-feature and run locally + CI.

### Pre-push hook on a pure-deletion / cleanup commit

A dead-code-removal commit changes `.rs` but ADDS no tests, so pre-push Gate-1 ("feature .rs
changed, no test added") AND Gate-2 (`Closes #N` ⇒ expects a RED test) both fire. This is NOT a
bug fix — it's the documented `[no-test: <reason>]` case. Put the marker (e.g.
`[no-test: dead-code removal, no behavior change — remaining tests cover it]`) on the **LATEST**
commit of the push (the hook reads `git log -1`). Never `[no-test:]` a real fix.

## #360 — SUPERSEDES #11 for strih: the strih burn is a FREE-RUNNING tick, NOT a clean step-2

**READ THIS BEFORE TRUSTING THE #11 "strih steps by 2" PREMISE BELOW — the rig data refutes it.**
The strih burn is **NOT** a per-output-frame counter that steps by a clean `round(60/30)=2`. It is a
FREE-RUNNING DistroAV render-tick whose per-recorded-frame step is **IRREGULAR**: on the 30fps stream
recording it steps 0–10 (mean ~4), and on the 60fps strih recording ~2. So its forward gaps are
render-clock JITTER, not lost frames — PROOF (run 354003): EVERY strih gap > 8 coincided with a CLEAN
stream-burn step (the stream burn never gapped ⇒ zero stream-output loss). The old `node_render_step`
strih=2 charging manufactured ~17 300 phantom REAL DROPs out of a clean run.

Two cooperating #360 bugs, both fixed in `recording-verdict.rs`:
1. **cam2-optical-tick gating.** `in_window_burn_frames` windowed the strih/stream (PerRenderTick) burn
   contiguity to ONLY the optically-decoded frames. At ≥1000ms latency the filmed cam2 dual-QR went
   ~87% undecodable (run 354003) / ~91% (354001), so 87–91% of burn-present DELIVERED frames were
   excised and the surviving burn ids jumped ~30. FIX: membership is now `is_optical(f) ||
   has_node_burn(f)` for ALL rates (the digital CRC-validated burn proves delivery independent of the
   cam2 optical read — extends the cam1 #204 reasoning). Lead-in/out is still trimmed by the OPTICAL
   window BOUNDARIES (so #198/#267/#273 are preserved).
2. **`node_render_step` now returns 1 for ALL nodes** (strih gap-ignore). A delivered frame MISSING its
   strih burn is still BURN-UNREADABLE (FAILS); real loss is caught by the stream burn (per-output-frame)
   + cam1 (per-emitted). The `burn_contiguity_in_window_with_step` step≥2 capability + its #11 tests are
   RETAINED for a genuinely-clean-decimation hop, but no current node feeds it ≥2.

RED→GREEN lock: `node_verdict_strih_zero_loss_when_cam2_optical_mostly_undecodable_360`,
`strih_burn_on_a_non_optical_frame_inside_span_is_included_360`,
`node_render_step_is_gap_ignore_for_all_nodes_360` (all in `recording-verdict.rs`). Validated via the
sanctioned probe re-merge (below): 354003 strih 17300→0, 354001 strih 17829→0.

**Residual cam1 over-count (→ #356, NOT #360):** the cam1 burn read from the SOFTENED strih recording
at ≥1000ms is heavily blurred — ~35% misdecode to duplicates (BURN-UNREADABLE) and ~2300 ids are absent
from BOTH recordings (0% present downstream; the stream rec is decimated AND softened too), so they
classify as REAL DROP though cam1 is present 100% in the downstream stream rec. This is a cam1-burn
READABILITY measurement limit, distinct from the cam2-optical-tick bug; the verdict still FAILs on it
(safe — a false-FAIL never masks). The #356 "headline must agree with the per-frame CSV" is satisfied
for strih/stream after #360 but NOT yet for cam1.

---

## #11 — DECIMATION-AWARE contiguity (the mixed 60/30 topology) — strih part SUPERSEDED by #360 above

Final topology: cam(60)→strih(60, LED-wall IMAG)→stream(30, every-other-frame)→restreamer(30). ~~The
strih burn is the strih OBS render-tick counter at 60fps; read from the 30fps STREAM recording it
steps by **2** by design~~ (REFUTED — see #360 above: the strih tick is free-running/irregular). The
PerRenderTick path used to UNCONDITIONALLY ignore forward gaps — which #11 believed MASKED real
strih→stream loss, but on the real rig such loss shows as a SMALL strih step (a held frame), never the
large gap #11 charged, so the step≥2 charging only ever produced false positives for strih.

`burn_contiguity_in_window_with_step(node, frames, rate, expected_step)` (`src/probe/burn_contiguity.rs`):
- `expected_step >= 2` ⇒ a forward gap **== step** is the by-design decimation (not loss); a gap
  **> step** charges the excess `gap/expected_step − 1` (INTEGER div, so genlock beat jitter of
  `step ± 1` charges 0) as `RealDrop`. Never a false ZERO — a real drop always opens a gap ≥ 2·step.
- `expected_step == 1` ⇒ today's unconditional-ignore (unchanged): the strih burn in a 60-in-60
  recording, the stream burn (recorded by its own OBS), cam render ticks. The 3-arg
  `burn_contiguity_in_window` wrapper passes 1.
- cam1 (PerEmittedFrame) is UNAFFECTED — its real-drop detection is set-based and catches every real
  60fps drop regardless of the recording fps.

**THE GOTCHA — two DIFFERENT "steps", do not conflate:**
- the **LOSS decimation step** (`recording-verdict.rs` `node_render_step` → `NodeSpec.step`): strih=2,
  stream=1, cam1=ignored. Derived from rig-pinned `--strih-emit-fps`(60) / `--stream-capture-fps`(30),
  **DECOUPLED from `--capture-fps`** so it is always correct even when the merge runs `--capture-fps 60`
  (which it does, for cam1's diagnostic span read from the 60fps strih recording).
- the **OPTICAL diagnostic expected_step** (`VerdictConfig`, `refresh_hz / capture_fps`): the cam2
  Vernier beat, DIAGNOSTIC only. THIS one tracks `--capture-fps` (60 for strih rec, 30 for stream rec),
  which is why `recording-e2e.sh` splits `CAPTURE_FPS` into `STRIH_CAPTURE_FPS`/`STREAM_CAPTURE_FPS`.

`node_render_step("strih", emit, cap)` = `round(emit/cap).max(1)` = 2; `"stream"`/`"cam1"` = 1 (the
stream burn is emitted AND recorded by the same OBS ⇒ no decimation). RED→GREEN lock:
`in_window_decimation_step2_extra_missing_id_is_a_real_drop_not_masked` (gap 4 → 1 RealDrop) +
`..._two_lost_frames_charge_two_real_drops` (gap 6 → 2) + `..._every_other_id_is_zero_loss` (clean) +
`..._jitter_gap_of_one_or_three_is_not_loss` + `in_window_step1_strih_recording_..._none_still_caught`.

**#360 SUPERSEDES the above step-2 math at runtime:** `node_render_step` now returns **1 for ALL
nodes** (`node_render_step_is_gap_ignore_for_all_nodes_360`) — strih's burn is a FREE-RUNNING render
tick with an IRREGULAR step (run 354003: 0–10, mean ~4), NOT a clean 60/30=2, so a forward gap is
render-clock jitter, not loss. The step≥2 excess-gap charging stays in `burn_contiguity` as a tested
capability, but NO current node feeds it ≥2. (The decimation-step doc above is the historical design;
the live value is 1.)

## #363 — the cam2 OPTICAL dual-QR read is the HARD verdict gate (NEVER re-weaken it)

The verdict PASS gate (`src/bin/recording-verdict.rs` `NodeVerdict::is_zero`) is **two** conditions:
`contiguity.is_contiguous() && optical_undecodable == 0`. The cam2 OPTICAL dual-QR is the ONLY proof
of the real camera-captured pixel path; the digital node burns are injected at the OBS render tick
**AFTER capture**, so they prove node→node DIGITAL delivery only — **they can NEVER substitute for the
optical read.**

- **The trap (#360, reverted by #363):** do NOT make the in-window membership `is_optical(f) || has_node_burn(f)` for strih/stream. That let a frame with ONLY a digital burn (no optical read) count as delivered, so an 87%-optically-undecodable run PASSED on the burns alone (the fraud). **Membership for strih/stream (PerRenderTick) is OPTICAL-ONLY.**
- **cam1 (PerEmittedFrame) KEEPS `is_optical || has_cam1_burn`** — its burn is genuine per-emit delivery proof, so dropping it would orphan the cam1 id and manufacture a PHANTOM forward-gap REAL DROP (the #204 fix). The membership is rate-gated: `is_optical(f) || (matches!(rate, PerEmittedFrame) && has_node_burn(f))`.
- **An in-span frame whose cam2 optical QR did NOT decode is a DISTINCT `optical_undecodable` hard-fail** — never a phantom chain drop (pre-#360), never a pass (#360). Computed by `optical_undecodable_in_span()` over the optically-anchored span (`optical_span()` = first..=last `is_optical`). cam1 still reports NO phantom drop on such a frame (its burn keeps the id present); the run nonetheless FAILS via `optical_undecodable`.
- Removing the strih/stream burn fallback does NOT re-introduce a phantom drop: PerRenderTick uses gap-ignore (step=1, see above), so forward gaps between the surviving optical frames are ignored. The pre-#360 phantom came from the now-dead step-2 math, not from optical-only membership.
- **HARD means hard:** no flag / env / threshold / "tolerance" / "allow N undecodable" — ever. A fake green is worse than honest red. RED→GREEN lock (probe-gated, CI-only): `node_verdict_optical_undecodable_is_a_hard_fail_363`, `node_verdict_fails_when_cam2_optical_mostly_undecodable_363`, `strih_burn_on_a_non_optical_frame_inside_span_is_excluded_and_undecodable_363`, and the updated `cam1_burn_on_an_optical_blurred_frame_is_not_a_phantom_drop` (run FAILS on optical, cam1 still no phantom).

## #363 — optical-robustness half: `decode_qr_luma_all` must merge plain ∪ Otsu (the Otsu-gating bug)

The HARD gate above (`optical_undecodable == 0`) only PASSES if the cam2 optical dual-QR is
actually DECODED on the delivered frames. After the gate was restored (#372) the stream
recording still showed **~87% optical-undecodable** — but the optical QR pixels were PRESENT
and readable. The cause was a DECODER gating bug, NOT the camera.

**THE BUG (`src/probe/qr.rs::decode_qr_luma_all`):** it ran the Otsu-binarized rqrr retry ONLY
when the plain rqrr pass found NOTHING:
```rust
let first = rqrr_decode_all(img.clone());
if !first.is_empty() { return first; }      // <-- the trap
rqrr_decode_all(binarize_otsu(&img))
```
On the stream recording the plain pass reads the crisp DIGITAL BURNS (so `first` is non-empty)
but MISSES the SOFT optical dual-QR (a QR filmed off a monitor: low-contrast + moiré +
colour-cast). Because `first` was non-empty the Otsu retry was SKIPPED → the present optical QR
was never recovered → ~87% of stream frames marked phantom `optical_undecodable`. PROOF: on the
real failing frames `f-5.png` / `f-150.png` (run 354003) rqrr returns the optical dual-QR when
the image is `binarize_otsu`-ed but NONE on the plain pass.

**THE FIX:** ALWAYS run BOTH passes and `merge_payloads` (de-dup by `(run_id, frame_id)`):
```rust
let mut out = rqrr_decode_all(img.clone());
merge_payloads(&mut out, rqrr_decode_all(binarize_otsu(&img)));
out
```
Result is a SUPERSET of the plain pass (never fewer). Fixes `decode_qr_luma_all_robust` and
`decode_qr_luma_all_fast_then_robust` for free (both call it). Cost: +1 cheap full-frame Otsu
rqrr pass per OFFLINE-decoded frame; the ~10× tiled passes stay conditional behind the
fast/robust gate. `decode_qr_luma_all_tile` (the upscaled-tile path) keeps its own
"plain-then-Otsu-if-empty" — the tile gating is about small node burns, not the soft optical.

**SIDE EFFECT — the full-frame pass got STRONGER:** the Otsu union now reads, full-frame, some
burns that previously needed the #202 tiles (3 of the 5 real burn-unreadable fixtures flipped
to the FAST path). This is good (fewer frames need the expensive tile fallback) but it
INVALIDATED two synthetic qr.rs tests whose premise was "the full-frame pass MISSES a softened
260px/blur-2.8 burn": that synthetic perfect-QR+blur burn is now recovered by the full-frame
Otsu pass. The synthetic blit model can NO LONGER reproduce the genuine "full-frame misses /
tiles recover" gap (a size-disparity / detector-coverage effect that needs real
encoder-degraded 4K pixels). So:
- the gap is locked on REAL pixels — `fast_then_robust_falls_back_to_robust_on_a_real_burn_unreadable_frame`
  (uses `tests/fixtures/burn-unreadable/cam1-frame-1148.png`) + `tests/burn_fixture_decode.rs`;
- the synthetic test was repurposed to lock the #363 improvement itself —
  `full_frame_otsu_union_recovers_a_softened_burn_bare_rqrr_misses` (bare single rqrr misses →
  the Otsu union recovers).

RED→GREEN lock (probe-gated, CI-only, in `src/probe/qr.rs`):
`optical_soft_dual_qr_recovered_on_real_stream_frames` (fixtures `tests/fixtures/optical-soft-f5.png`
/ `optical-soft-f150.png`, run 354003): the plain pass returns NO run_id 354003; `decode_qr_luma_all`
returns BOTH optical halves; the recording per-frame path surfaces it too. Reverting to the
early-return makes `decode_qr_luma_all` return only the burns → the GREEN assert fails.

## #376 — the residual 0.24% optical-undecodable is a CALIBRATED moiré floor, NOT chased further

After #363's Otsu-union fix (above), the real run-354003 stream recording went from 86.9%
optical-undecodable down to **22/8999 = 0.2445%**. Those 22 frames are the cam2 dual-QR's
**RIGHT** half arriving soft/mottled with heavy diagonal moiré (a camera→monitor optical
artifact — visually confirmed: the finder patterns and data modules on the right QR are visibly
grayer/anti-aliased vs the crisp black/white left QR in the SAME frame), while the LEFT half of
the same frame decodes clean every time.

**THE DECISION (user, issue #376, 2026-07-01) — do NOT chase the decoder or the camera further.**
This explicitly SUPERSEDES the ticket's original "recover via more decoder robustness, or prove
it's a camera limit" plan. Same class of call as #364's bright-neutral cyan-cast calibration:
*"akceptovateľný optický/moiré artefakt rigu (nakalibrujem prah vyššie)? Ano akceptovatelne!"* —
accept the residual as the rig's real optical physics, calibrate the gate threshold, keep it
strict above the floor.

**THE FIX — a calibrated RATE ceiling, not a raw count.** `NodeVerdict::is_zero()`
(`src/bin/recording-verdict.rs`) used to require `optical_undecodable == 0` (the #363 hard gate).
It now requires `optical_undecodable_ok()`: `optical_undecodable / optical_span_frames <=
OPTICAL_UNDECODABLE_RATE_MAX` (0.5%, ~2× the measured 0.2445% floor). A RATE (not an absolute
count) scales correctly with recording length — a raw count ceiling would silently tighten on a
long recording and loosen on a short one. The gate stays HARD above the floor: a genuine dropout
(e.g. the #216 ~175 s slow-shutter gap) is two orders of magnitude above 0.5% and still FAILS —
locked by `optical_undecodable_just_above_the_calibrated_ceiling_fails_376` and
`optical_undecodable_materially_above_the_floor_still_fails_376`.

**Single GLOBAL threshold, not per-node.** cam1 reads from the strih recording (one hop, likely a
lower natural floor); strih/stream read from the stream recording (two hops, the measured 0.24%
floor). One conservative constant calibrated to the WORST-observed node is used for all — a
cleaner node just passes with headroom to spare, never masked. Don't invent per-node thresholds
without a per-node measured floor to calibrate against.

**Tests are SYNTHETIC, not real-PNG fixtures — this is a pure gate-arithmetic change.** Unlike
the #363/burn-unreadable fixtures (which lock a DECODER improvement and need real degraded
pixels), #376 only recalibrates a threshold on an already-computed count, so the RED→GREEN tests
(`optical_undecodable_within_the_moire_floor_passes_the_gate_376` +
`optical_undecodable_at_the_calibrated_ceiling_still_passes_376`, both RED against the pre-#376
`== 0` gate) build synthetic `RecordingFrame`s via a small helper
(`optical_run_with_undecodable`) — no PNG fixtures needed or committed. The two residual example
frames (frame-1924.png / frame-3050.png from the stream box's
`verdict-out\stream-redecode-354003-pixels`) were pulled and visually confirmed once for the
decision, not embedded as test fixtures.

## #461 — adding a BURN-LESS node (imag-nb) — reuse NodeContiguity's SHAPE, not `burn_contiguity`

**#463 update: imag is NO LONGER burn-less.** It now carries its own digital corner burn
(`BURN_RUN_ID_IMAG` = 911003, `Corner::BottomCenterLeft`), ANDed with the optical tick
contiguity via `imag_tick_gate::ImagVerdict` — see "#463 — adding a Nth burn corner" below for
the geometry side, and `NodeVerdict::imag_burn_ok()` / `node_verdict_for_imag` in
`recording-verdict.rs` for the AND-gate. The section below (steps 1-4) still describes the
CORRECT pattern for a genuinely burn-less proof (imag's optical fallback still uses it when no
burn is decoded in a recording) — read it for that shape; just don't assume imag has no burn any
more.

imag-nb originally had no digital node-burn; its zero-loss proof was the cam2 OPTICAL tick's own
first..=last integer contiguity instead. The clean way to add a node whose proof mechanism is
DIFFERENT from every other node's (burn id vs optical tick):

1. Write the ALGORITHM as a brand-new pure module OUTSIDE `probe::` (`src/imag_tick_gate.rs`),
   even though it duplicates `probe::burn_contiguity::burn_contiguity`'s exact first..=last logic.
   The whole `probe` module is `#[cfg(feature = "probe")]` (CI-only) — a pure decision that lives
   there can never be RED→GREEN-verified locally, so the duplication buys real Tier-0
   testability, not laziness.
2. In the probe-gated glue (`node_verdict_for_imag`), construct `NodeContiguity` DIRECTLY from
   your own computed values (`first_id`/`last_id`/`missing_ids`/`present_count`/`expected_count`
   — the struct's fields are `pub`) instead of calling `burn_contiguity()`. Since `NodeContiguity`
   is just a data shape, not burn-specific, feeding it tick-derived values makes `is_zero()` /
   `print_node_verdict` / `node_verdict_json` all work UNCHANGED for a node with no burn — zero
   changes needed to any of that shared machinery.
3. Gate the new node at TOP LEVEL in `build_and_print_verdict` (like the existing `--cam1-capture-
   stats` block), NOT nested inside `if let Some(stream_frames) = &stream_frames_opt` — a node
   whose recording is independent of strih/stream (imag has its own box) must work standalone.
4. `optical_span_facts(frames, &[], cam2_run_id)` (empty `all_burn_run_ids`) is the right call for
   a burn-less node's #373 duration-floor span — every non-burn payload counts as optical when
   there is nothing to exclude.

## #463 — adding a Nth burn corner: FOUR independent places have to agree

Adding imag's `Corner::BottomCenterLeft` (a 4th burn corner, after cam1-center + strih-BL +
stream-BR) touches FOUR separate implementations of "where does this burn sit" — miss one and
you get a silent geometry mismatch that only a real recording (or the C++ parity test) would
catch:

1. **`vendor/distroav/src/burn-geom.hpp`** (C++, the ACTUAL render geometry) — `Corner` enum +
   `corner_placement()`'s per-corner `if`/`else` branch. This is ground truth; everything else
   below is a MIRROR of it.
2. **`src/probe/colour_sample.rs::node_burn_exclusions`** (Rust, probe-gated) — the colour gate's
   dodge rects. Must reproduce the SAME formula (margin/side/band_x) as step 1, by hand, in Rust —
   there is no shared code between the C++ render path and this Rust dodge path.
3. **`src/colour_scale.rs` test module** (Rust, Tier-0) — a THIRD hand-written mirror of the same
   formula, as `const` test fixtures, used to prove (locally, RED→GREEN, no `--features probe`
   needed) that the new corner doesn't collide with any colour patch or any other burn.
4. **`tests/burn_payload_parity.rs`** (Rust, probe-gated, but embeds a C++ harness that calls the
   REAL `burn_geom::corner_placement` from step 1 directly) — extend `four_qr_rects`-style helpers
   + the vendored-source string-presence guards (`flt.contains("911003")` etc.) to cover the new
   corner.

**Do the arithmetic ONCE by hand** (e.g. `margin + side + margin` for "one margin clear of the
previous burn's trailing edge") and paste the SAME numbers into all four — do not let each mirror
independently "derive" a slightly different formula. Cross-check by computing the expected
`band_x` for the production canvas (1920×1080) and asserting the SAME number shows up in the C++
compile-check, the Rust Tier-0 test, and the probe-gated exclusion rects.

**When the formula gets a FALLBACK tier (a narrow-canvas clamp), the fallback is part of the
formula too — sync it across all four, not just the happy-path number.** #463's initial
BottomCenterLeft clamp (single-tier: wanted position, else flush to the frame's right edge) could
overlap BottomLeft on a narrow canvas — fixed with a 2-tier fallback (flush against BottomLeft's
own trailing edge first). The fix landed in mirror 1 (`burn-geom.hpp`) and mirror 2
(`colour_sample.rs`) in the SAME review pass, but mirror 4 (`tests/burn_payload_parity.rs`'s
`imag_burn_rect` test helper) was missed until a SECOND review pass caught it — because its
existing multi-resolution test only exercised 16:9 canvases (720p/1080p/1440p/4K), all of which
stay on tier 1 and never exercise the fallback at all. **Whenever you add a fallback tier to the
geometry, add a narrow-canvas test THAT SPECIFIC MIRROR too** — a multi-resolution sweep over
real aspect ratios is not enough if none of those resolutions are narrow enough to reach tier 2/3.

## Verifying a freestanding vendored C++ header LOCALLY without `--features probe`

`burn-geom.hpp` is explicitly "header-only, freestanding (no OBS, no chrono)" — you do NOT need
the OBS SDK or the `probe` feature to compile-check it. Write a throwaway `.cpp` in `/tmp` that
`#include`s the header by ABSOLUTE path and exercises the function you changed, then:

```bash
g++ -std=c++17 -O2 -Wall -Wextra -Werror /tmp/check.cpp -o /tmp/check && /tmp/check
```

This gave REAL RED→GREEN evidence for the C++ corner-placement change in this ticket (a
deliberately-wrong `band_x` failed an assertion, then the correct formula passed) even though the
actual `tests/burn_payload_parity.rs` harness that also exercises this header requires
`--features probe` (CI-only, banned locally per this repo's Tier-0 policy). Do this for ANY
freestanding vendored header (`burn-geom.hpp`, `burn-payload.hpp`, `burn-clock.hpp`, `burn-qr.hpp`
all qualify — none pull OBS SDK headers) before trusting a C++ logic change to CI alone.

## #467 — extending the ALL-CAMBOX `--switch-schedule` sweep to a SECOND own-recording node

The #312 sweep (`segment_frames_from_recording` + `segment_continuity`, both generic — they
operate on any `Vec<SegmentFrame>` and any schedule, not stream-specific) was originally wired
for the stream recording only. #467 added imag's OWN recording as a SECOND independent input to
the SAME schedule/functions — imag never gets scene-switched by the harness (fixed on CAM1,
#462), so this proves imag's OWN delivery stayed continuous across the WHOLE sweep, segmented at
the SAME ~30s granularity, NOT "did imag show cambox X". The pattern for adding a THIRD such node
later:

1. **No new algorithm needed** — `segment_frames_from_recording`/`segment_continuity` are already
   generic. Call them again with the new node's OWN frames + its OWN anchor burn run_id (falling
   back to the cam2-optical anchor exactly like strih/stream already do) + the SAME schedule.
2. **The by-design decimation step is rate-derived per node**, never assumed. Extracted into
   `src/recording_span_gate.rs::painted_tick_step(refresh_hz, capture_fps)` — a Tier-0 pure
   function (was previously computed INLINE in the probe-gated binary and thus untestable). Reuse
   it for every new node at ITS OWN native capture rate (imag: 60Hz/60fps = step 1; stream:
   60Hz/30fps = step 2).
3. **Report under a NEW key inside `all_cambox_continuity`** (e.g. `all_cambox_continuity.imag`),
   not folded into the existing `segments` array — that array's `cambox` field is the SCHEDULE's
   per-window label (which cambox was live on strih), not the new node's identity; conflating them
   would misattribute. Fold the new node's `overall_pass` into the run's `all_pass` — but ONLY when
   that node's frames were actually supplied (optional signal: absent never fails the sweep,
   present must pass — mirrors `imag_tick_gate::optional_signal_ok`'s "absent is fine, present
   broken fails" rule elsewhere in this same imag machinery).
4. **Ownership gotcha:** the node's `Option<DecodedRec>` parameter into `build_and_print_verdict`
   is normally CONSUMED once by the existing top-level per-node check (`if let Some(d) = imag {
   let imag_frames = d.frames; ... }`). To reuse the same frames later inside the sweep block,
   capture into a local `Option<Vec<RecordingFrame>>` ONCE up front and borrow it (`&imag_frames_opt`)
   at BOTH call sites instead of moving it — `if let Some(x) = &opt` twice, never `if let Some(x) = param`.
5. **Testing:** the wiring itself only compiles under `--features probe` (recording-verdict.rs is
   `required-features=["probe"]`), so its RED→GREEN can't be observed locally — write the
   integration test (mirroring `merge_path_computes_all_cambox_continuity_like_the_fused_path`'s
   shape: synthetic schedule + synthetic stream frames + synthetic new-node frames, asserting on
   `v["all_cambox_continuity"]["<node>"]["overall_pass"]`) and trust CI to compile+run it. Pull the
   genuinely NEW, rate-derived arithmetic (the step formula) out to a crate-root Tier-0 module
   (mirrors `imag_tick_gate.rs`) so AT LEAST that piece is locally RED→GREEN-observable.
