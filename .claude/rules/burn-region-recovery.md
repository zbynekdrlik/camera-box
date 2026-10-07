---
paths:
  - "src/burn_regions.rs"
  - "src/probe/burn_region_decode.rs"
  - "src/probe/qr.rs"
  - "src/probe/qr_tests.rs"
  - "src/probe/recording_decode.rs"
  - "src/probe/recording_decode_tests.rs"
  - "src/probe/colour_sample.rs"
  - "src/probe/recording_latency.rs"
  - "vendor/distroav/src/burn-geom.hpp"
  - "tests/burn_reframed_fixture_decode_1370.rs"
  - "tests/burn_regions_cpp_parity_1370.rs"
  - "src/probe/burn_echo.rs"
  - "tests/burn_echo_fixture_decode_1367.rs"
  - "src/burn_quiet_zone.rs"
  - "tests/burn_tight_box_fixture_decode_1367.rs"
  - "tests/fixtures/burn-tight-box-1367/**"
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
still resolves. Put a new decode pass in `recording_decode`, not `qr`. `qr`'s own tests are the
`#[path]` child `src/probe/qr_tests.rs`; a test that needs its fixture/blit helpers imports
`crate::probe::qr::tests::{optical_fixture_luma, blit_burn_luma}`.

Proving a future probe-module split is a pure move (no local compile): moving an inline
`mod tests` into a `#[path]` child de-indents it, and rustfmt then re-joins calls that now fit and
drops their trailing commas. So compare whitespace-stripped text with `,)` / `,]` / `,}` normalized
(token-identical modulo trailing commas), check no multi-line string literal lacks a `\`
continuation (de-indenting would change it), and use `git diff --color-moved=plain
--color-moved-ws=allow-indentation-change` (after `git add -N` on new files) to count moved lines.

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
- The looks run in order 1x, tight box, 2x CatmullRom. Each later look runs only when the looks
  before it read no burn of that slot. One slot carries one burn, so a slot that read the deployed
  camera's burn never gets another look for the other cams.
- An id without a reserved slot is never localized: SongPlayer 911014 is painted by the sender,
  911013 is optical content, and an operator `--burn-*-run-id` override has no slot. A
  non-reserved expected id logs ONE warning per process, so an override is not a silent loss.
- The verdict log line `recording analysis complete` carries `burn_region_recoveries`: how many
  burns only the slot crops read. A large count means the camera view pushed optical content into
  the burn tiles.

## The tight-box look — the burn's own white box, decoded alone (issue 1367)

The camera burn is not 320 px. `render_payload_qr(payload, 320)` builds the QR with whole-pixel
modules (`qrcode`'s `max_dimensions` wins), and the burn payload encodes as a version-4 EC-H code:
33 modules plus a 4-module quiet zone on each side, 41 in all. 320 / 41 floors to 7 px, so the
burn is 287 px. `cam1_burn_origin` centres it at (816, 769) on 1080p. A longer payload needs
version 5 (45 modules, still 7 px): 315 px. In every real camera-slot fixture in the repo the burn
SITS at (24, 41) inside the 336 px crop, 287 px (286 wide after the recording scale on the
multiview frames).

So the fixed camera crop (792, 728, 336 x 336) always holds a 24 px strip left of the burn and a
41 px strip above it. Over ordinary camera picture that is harmless. When cam2 films the strih-lx
multiview, those strips hold multiview QR content, and rqrr's one grid pass over the crop reads
nothing. Run 324220913 had 7 such cam2 frames (1775, 2008, 7808, 8149, 8150, 8167, 8170), counted
as `BURN-UNREADABLE`.

The fix, in `burn_region_passes`, after a 1x look that read no burn of the slot:
- `burn_quiet_zone::locate_burn_box` (pure, Tier-0) finds the white quiet-zone box inside the crop.
  It keeps the columns and rows whose longest near-white run reaches 0.8 x the slot's design side
  (`slot_rect(..).w`). The quiet zone's side columns and top/bottom rows carry such runs; inside the
  code every run is broken by dark modules. It returns the span of those columns and rows when both
  sides are within 0.8-1.05 x the design side. Otherwise nothing is decoded.
- A clearly non-square span (sides differing by more than a twentieth, about two modules) is
  squared: the longer span is cut to the shorter one, at the end where both new edge lines still
  qualify, else nothing is decoded. On 4 real fixtures (burn-reframed-1370 frames 355/532,
  tear-781 stream-2099068429 frames 1399/4792) light rows above the burn join its top quiet band
  and the raw span is 287 x 328 at (24, 0); squaring gives the 287 x 287 box at (24, 41).
- Near-white = the midpoint between the crop's Otsu threshold and its 99th-percentile white level.
- `tight_box_reads` copies exactly that box into a white image with a border of a tenth of its side
  (about four modules), decodes it plain then Otsu (`decode_qr_luma_all_reads`), and maps the reads
  back to the frame (`bordered_origin`). They pass the same missing-id + own-slot filter as every
  other look.
- The box never leaves the crop, so this look reads nothing the crop does not hold (an echo
  inside the camera crop stays the known limit below). Only missing ids merge.

Do NOT locate the box as the bounding box of the largest bright connected component. On the
run-324220913 crops the quiet zone touches bright multiview pixels, and the component leaks to the
crop edge at every threshold from Otsu to 235. At the Otsu threshold itself the light rows above the
box also join its top edge, so the threshold is the midpoint, not Otsu.

Evidence (a local real-rqrr 0.9.3 harness, built like the one in "Verifying a decode change here
at Tier-0" above):
- 7 of 7 missed frames read, each centred at (963, 916), each id between its neighbours.
- A sweep of every real 1080p fixture in the repo (23 frames x the 5 slots): wherever a box was
  found, the tight box read the same payload as the 1x crop or nothing. The only new reads were
  the cam2 burns on the 3 committed fixtures, and no read fell outside the crop.
- The functional replica mounts the real `burn_region_decode.rs` / `burn_echo.rs` /
  `burn_regions.rs` / `burn_quiet_zone.rs` with the real rqrr and a behaviour-faithful `image` stub
  (crop, replace, from_pixel, pixels, as_raw). It proved the glue RED (pre-fix: nothing on
  2008-8170) and GREEN. A stub `camera_box` rlib built from the same files ran the fixture
  integration test file locally. The stub's `image::open` reads a PIL `.y8` dump of each PNG, and
  the grouped-decode test stays CI-only. The stub's 2x `resize` is NEAREST, not CatmullRom, so it
  reads frame 1775 where production did not. Never count a replica 2x result as production.

Lock: `tests/burn_tight_box_fixture_decode_1367.rs` + `tests/fixtures/burn-tight-box-1367/`
(strih frames 1775, 2008, 8150, verbatim pixel proofs). Guards: the probe tests in
`burn_region_decode.rs` (box out of the size band = nothing decoded, no burn = nothing read) and the
Tier-0 tests in `burn_quiet_zone.rs`.

Not changed here: `slot_rect(CameraCapture)` still models 320 px where the writer renders 287 or
315. The issue-1367 design (comment 5851384821) rejected changing the fixed crop; the rendered-size
finding is on the ticket (comment 5851477988). A tighter slot would still depend on the QR version.

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

The probe path compiles at CI only, but the REAL rqrr can run locally. Shortest path (issue 1404):
link a small std-only harness against the self-hosted runner's release build,
`rustc --edition 2021 -O h.rs --extern rqrr=<…/target/release/deps/librqrr-*.rlib> -L <that deps dir>`
(`~/actions-runner-camera-box/_work/camera-box/camera-box/target/release/deps`, same toolchain), and
feed it a PIL `.y8` dump through `PreparedImage::prepare_from_greyscale`. Otherwise build it:
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
  `merge_payloads` and the real `qr_tests.rs` helpers (extracted verbatim), plus stub `image` /
  `tracing` rlibs shaped like the used API. Set `CARGO_MANIFEST_DIR` for the `env!` in the test
  helper, and run `clippy-driver` for the lib and for `--test` with `-D warnings`. Prove the
  harness bites with a negative control (e.g. drop a helper's `pub(in crate::probe)` → E0603).
- Before trusting "flat vs grouped" routing, follow the call chain: `analyze_recording_with_burns`
  (the flat-looking one) goes through the GROUPED per-frame decode.

Two more from the issue-1367 tight-box lane:
- **RUN the real glue, not just type-check it.** Make the `image` stub behave, not only
  type-check: keep the pixels plus working `crop_imm().to_image()`, `replace`, `from_pixel`,
  `pixels` and `as_raw`. Give the stub `qr` module a `decode_qr_luma_all_reads` that calls the real
  rqrr (plain, then the production Otsu). Then mount the real `burn_region_decode.rs` /
  `burn_echo.rs` / `burn_regions.rs` and call `burn_region_passes` on `.y8` frames: that is a
  RED/GREEN of the actual glue. Swap in `git show HEAD:<file>` for the RED side.
  - Build the same modules as a `--crate-name camera_box` rlib, and add an `image::open` stub that
    reads `$Y8_DIR/<png name>.y8` (a PIL `convert('L')` dump). A probe-gated `tests/*.rs` file then
    compiles with `--cfg 'feature="probe"'` and runs.
  - Stub any function the file needs but the stub crate lacks with `unimplemented!()`, and skip
    that test at run time with `--skip`.
  - The `tracing` stub needs one macro arm per call shape. When a field list changes, the pre-fix
    file needs its old arm too.
  - The stub's 2x `resize` is NEAREST, not CatmullRom, so it can read a frame production missed.
- **Synthetic burns in python:** `qrcode.QRCode(error_correction=ERROR_CORRECT_H, box_size=7,
  border=4)` on the payload string renders the same 287 px version-4 burn as the Rust writer.
  Paste it at `((1920 - 287) / 2, 1080 - 287 - 24)`. The mode choice matters (digits plus `P`
  and `.` go alphanumeric): a short payload like `P911009.1.1.<crc>` is version 3 and renders
  about 296 px.

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
