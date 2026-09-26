---
paths:
  - "src/burn_regions.rs"
  - "src/probe/burn_region_decode.rs"
  - "src/probe/qr.rs"
  - "src/probe/recording_decode.rs"
  - "src/probe/recording_decode_tests.rs"
  - "src/probe/colour_sample.rs"
  - "src/probe/recording_latency.rs"
  - "vendor/distroav/src/burn-geom.hpp"
  - "tests/burn_reframed_fixture_decode_1370.rs"
  - "tests/burn_regions_cpp_parity_1370.rs"
  - "src/probe/burn_echo.rs"
  - "tests/burn_echo_fixture_decode_1367.rs"
---

# Burn-isolated slot recovery — a crisp node burn decodes whatever the camera shows (issue 1370)

## Where the decode core lives (issue 1374)

The recording decode core is `src/probe/recording_decode.rs`, with its tests in the `#[path]`
child `src/probe/recording_decode_tests.rs`. It holds the fast-then-robust family, the
`_gated` core, the #202 tiles, the #754 top band, `optical_read_short`, `fast_path_gate_satisfied`,
`DecodePath` and the decode-path counters. `src/probe/qr.rs` keeps the QR primitives (render, the
plain/Otsu rqrr passes, `decode_qr_luma_all[_reads]`, `merge_payloads`, `OPTICAL_TOP_BAND_FRAC`,
the live-tap captures) and `pub use`-re-exports the public decode items, so every
`qr::decode_qr_luma_all_fast_then_robust_*` / `qr::DecodePath` path in this file and in callers
still resolves. Put a new decode pass in `recording_decode`, not `qr`. A test that needs the
`qr` fixture/blit helpers imports `crate::probe::qr::tests::{optical_fixture_luma, blit_burn_luma}`.

## What it is

The recording decode (`qr::decode_qr_luma_all_fast_then_robust_grouped_pathed_optical`) runs the
plain full-frame pass, then the #202 bottom tiles. As a third step on the ROBUST branch, every
EXPECTED burn still missing is decoded from a crop of its OWN known overlay slot
(`probe::burn_region_decode::recover_missing_burns`):
- the camera capture burn is centred at the bottom (`qr::cam1_burn_origin`, 320 px on 1080,
  scaled with the height);
- the four OBS corners come from `burn_geom::corner_placement` (strih BL, stream BR, imag BCL,
  cg BCR).

The slot table is the pure Tier-0 `src/burn_regions.rs`.

## Why a tile can lose a crisp burn

A tile is a third of the bottom 45 % band. rqrr runs ONE `detect_grids` pass per image, inside ONE
`catch_unwind`. When the camera's view of the cam2 monitor puts optical content (the dual-QR, the
aux marks) into that tile, the pass can return nothing and the burn in it is lost.

Live case, 25.9.2026: the camera was reframed and run 68573319 showed 280 `BURN-UNREADABLE` slots.
The strih in-place decode's `robust_fallback_frames` went from 7-20 to 2330. `zbarimg` read every
one of those burns. A slot crop holds the burn, its own white quiet zone and 8 px of pad. There is
nothing foreign in it for rqrr to group with.

## Contract — keep it

- Only MISSING expected ids trigger it; a clean frame never pays.
- Only those ids are MERGED. The result is a byte-identical superset of plain + tiles, and the pass
  never adds optical or aux (911013) payloads — the tear detector and the continuity metrics read
  those by run_id.
- The 2x CatmullRom look runs only when the 1x crop read no burn of that slot. One slot carries one
  burn, so a slot that read the deployed camera's burn never gets a 2x look for the other cams.
- An id without a reserved slot is never localized: SongPlayer 911014 is painted by the sender,
  911013 is optical content, and an operator `--burn-*-run-id` override has no slot. A
  non-reserved expected id logs ONE warning per process, so an override is not a silent loss.
- The verdict log line `recording analysis complete` carries `burn_region_recoveries`: how many
  burns only the slot crops read. A large count means the camera view pushed optical content into
  the burn tiles.

## Adding a node burn or corner — `burn_regions` is the ONE Rust copy

`src/burn_regions.rs` holds the Rust burn geometry for both the recovery pass and the colour gate's
burn dodge. `colour_sample::node_burn_exclusions` is now just its slots padded by 6 px, so the
colour gate's former hand-copy of the corner math is gone.
- A new run_id goes into `slot_for_run_id`.
- A new corner goes into `BurnSlot` and `slot_rect`, including any fallback tier and the
  `band_cy` rounding (an odd side sits 1 px lower than `h - margin - side`).

Two pins catch drift:
- `tests/burn_regions_cpp_parity_1370.rs` (default features, needs `c++`) compiles the shipped
  `burn-geom.hpp` and checks every corner on 1080, 720p, 4K and narrow canvases.
- the probe-gated tests in `src/probe/burn_region_decode.rs`:
  `camera_slot_matches_the_cam1_burn_writer_at_the_design_height_1370` and
  `slot_ids_match_the_reserved_burn_run_ids_1370`.

The camera slot scales with the frame height, because the camera burn is rendered on the 1080
capture frame. Every colour-gate caller passes the 1920x1080 painter canvas, where the slot is
exactly `qr::cam1_burn_origin(320)`.

## Verifying a decode change here at Tier-0 (no cargo)

The probe path compiles at CI only, but the REAL rqrr can run locally:
1. Build `rqrr` 0.9.3 without its `img` feature, plus its chain (unicode-ident, proc-macro2 with
   `--cfg wrap_proc_macro`, quote, syn 2, g2poly, g2gen proc-macro, g2p, lru without hashbrown),
   with plain `rustc` from `~/.cargo/registry/src/*/`. It takes a few seconds.
2. Write a harness that reads a raw `.y8` luma buffer, calls
   `PreparedImage::prepare_from_greyscale` and runs plain + the production Otsu.
3. Cut the crops in PIL. `im.crop` is byte-identical to `image::imageops::crop_imm`. PIL BICUBIC is
   NOT byte-identical to image's CatmullRom resize, so a replica tile can read a burn the
   production tile misses (frame 357). Anchor a RED on the run's own partial JSON — it is the
   production decode of those exact pixels — never on a resized replica.

The glue can also be type-checked and clippy-linted: mount the real module through `#[path]` in a
test crate, with stub `image`/`tracing` rlibs shaped like the used API. Measured this way in
the issue-1370 lane: across every pixel proof of run 68573319 (65 frames), the 1x slot crops read
all 13 burns the production decode had missed. (That is a sample; the run's 280 slots are proven
fixed only by a post-merge E2E whose BURN-UNREADABLE count drops.)

Two extensions from the issue-1367 lane:
- WHERE a read sits: print `grid.bounds` (the mean of the four corners) next to each content in
  the harness, and map each crop's reads back through its offset and resize scale. That is how
  the multiview echoes were found (`zbarimg` and OpenCV `detectAndDecodeMulti` give positions too).
- Type-check a `qr.rs` edit without the image/qrcode crates: a python script extracts the edited
  functions VERBATIM (regex from `fn name(` to the next `\n}\n`) into a replica module. Mount it
  with the real `burn_regions.rs` / `burn_echo.rs` via `#[path]`, stub `rqrr_decode_all_catch`
  and `binarize_otsu`, and run `clippy-driver --test -D warnings`.
- A `recording_decode.rs` edit needs no extraction (issue 1374): mount the WHOLE real file via
  `#[path]` (its `#[path]` tests child follows under `--test`), next to the real
  `burn_echo.rs` / `burn_regions.rs` / `colour_scale.rs` with their own test modules stripped.
  Give it a `qr` stub that carries qr.rs's REAL `pub use` re-export block, the real
  `merge_payloads` and the real `qr::tests` helpers (extracted verbatim), plus stub `image` /
  `tracing` rlibs shaped like the used API. Set `CARGO_MANIFEST_DIR` for the `env!` in the test
  helper, and run `clippy-driver` for the lib and for `--test` with `-D warnings`. Prove the
  harness bites with a negative control (e.g. drop a helper's `pub(in crate::probe)` → E0603).
- Before trusting "flat vs grouped" routing, follow the call chain: `analyze_recording_with_burns`
  (the flat-looking one) goes through the GROUPED per-frame decode.

## The echo gate — a node burn counts only in its own slot (issue 1367)

A camera that films a monitor showing OBS captures decodable copies of node burns. On run
386740541 cam2 filmed the strih-lx HDMI multiview, and its Preview, Program and camera cells held
burns, cam2's own among them. rqrr reads them like any QR.
- The echoes put stale ids into cam2's contiguity (copies/gaps 145/151 and 51/53).
- On frame 1521 an echo of strih's burn and one of cam3's burn satisfied the fast-path gate, so
  the real burns were never read. The unpinned optical-short check was fooled the same way: two
  cam2 ids (the real one and an echo) looked like a complete dual-QR.

The gate:
- `burn_regions::node_burn_in_own_slot(run_id, cx, cy, w, h)` is the pure predicate. A slotted
  run_id counts only when its detected centre is inside its own `recovery_crop` (slot + pad,
  half-open). An unslotted id (optical, aux 911013, SongPlayer 911014) always counts. A frame with
  no room for the slot never counts a slotted read.
- rqrr reads keep their grid centre (`probe::burn_echo::LocatedPayload`, the mean of the four
  `bounds`). Each pass maps its reads back to frame pixels with its crop offset and resize scale:
  tiles use `tw / tile.width()`, the 2x slot look uses 1/2.
- `qr::decode_qr_luma_all_fast_then_robust_gated` is the one decode core. `NodeBurnGate::OwnSlot`
  gates EVERY read of every pass BEFORE reads merge by id: full frame (plain, then Otsu), top band,
  tiles and slot crops (`qr::decode_qr_luma_all_reads` returns the raw reads, no identity merge).
  So an echo never reaches the optical-short check, the fast-path gate or the missing-burn list,
  and an echo that carries the current id cannot shadow the in-slot read of that id.
- EVERY recording analysis runs `OwnSlot`: `recording::analyze_recording*` all go through the
  grouped decode, so strih, stream, imag, cg, the cam1 grab and the A/V / forensic tools are gated.
  Only the per-frame helpers `qr::decode_qr_luma_all_fast_then_robust[_pathed]` and
  `recording::decode_recording_frame[_with_burns]` run `Off`, byte-identical to before. No
  recording goes through them; the synthetic latency tests use them with burns drawn anywhere.
- The slot-crop pass also gates, but its crop IS the acceptance region, so for the slot's own ids
  it can never reject. It is there so that no pass admits an echo, by construction.
- Report-only count: distinct echoes per frame, summed per process
  (`burn_echo::burn_echo_rejection_count`). Every partial carries `burn_echoes_rejected`
  (additive, no schema bump). The verdict writes `burn_echoes_rejected: {strih, stream, imag, cg,
  gates_overall_pass: false}`, null when not carried.

Rules for a future change here:
- A synthetic decode test that goes through the GROUPED decode or any `analyze_recording*` must
  draw every node burn at its real slot (`qr::cam1_burn_origin` for a camera burn, `slot_rect`
  for a corner). A camera burn drawn in a corner is an echo now. Two tests were fixed for this:
  `grouped_gate_fast_path_when_deployed_camera_is_cam3_not_cam1` draws cam3 at the camera slot,
  and `analyze_recording_recovers_a_softened_bottom_burn` uses strih's id for its bottom-left burn.
- Before changing the slot geometry or the pad, re-run the fixture sweep. Every real decode
  fixture (20 frames: burn-reframed-1370, burn-unreadable incl. two 4K, optical-soft, the #754
  sweep frame, qr-align-moire-1239, tear-781) had ZERO slotted reads outside its own slot. The
  sweep uses the real rqrr harness, with the grid centre printed from `bounds`, over the
  production regions (full, top band, halves, tiles, slot crops).
- Known limit: the seven camera ids share ONE slot, so an echo of another camera's burn whose
  centre falls inside the camera recovery crop would count. In practice the opaque 320 px real
  burn covers the slot and leaves only the 8 px pad, where no decodable echo fits.
- A #202 tile whose plain pass read only echoes still gets its Otsu retry under `OwnSlot`
  (`burn_echo::any_read_counts`); under `Off` the retry keeps the old "plain read nothing" rule.
- A frame's echoes can hold the CURRENT id if a monitor shows the program with no delay. The echo
  list then has an identity that also counts from the slot. That is harmless: the count is a
  diagnostic, and the slot read is the burn (gating before the id merge is what keeps it).
