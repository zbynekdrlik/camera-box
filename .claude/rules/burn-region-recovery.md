---
paths:
  - "src/burn_regions.rs"
  - "src/probe/burn_region_decode.rs"
  - "src/probe/qr.rs"
  - "src/probe/colour_sample.rs"
  - "src/probe/recording_latency.rs"
  - "vendor/distroav/src/burn-geom.hpp"
  - "tests/burn_reframed_fixture_decode_1370.rs"
---

# Burn-isolated slot recovery — a crisp node burn decodes whatever the camera shows (issue 1370)

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
  911013 is optical content, and an operator `--burn-*-run-id` override has no slot.

## Adding a node burn or corner — `burn_regions` is now ONE MORE mirror

The #463 four-mirror list in `.claude/skills/recording-decode` gains a fifth entry:
`src/burn_regions.rs`.
- A new run_id goes into `slot_for_run_id`.
- A new corner goes into `BurnSlot` and `slot_rect`, including any fallback tier.

The probe-gated parity tests in `src/probe/burn_region_decode.rs` fail when this table drifts:
- `corner_slots_match_the_colour_sample_burn_geom_mirror_1370` — corners vs
  `colour_sample::node_burn_exclusions` on 1080, 720p, 4K and narrow canvases;
- `camera_slot_matches_the_cam1_burn_writer_at_the_design_height_1370`;
- `slot_ids_match_the_reserved_burn_run_ids_1370`.

On a height other than 1080, the camera slot deliberately differs from `colour_sample`'s native
320 px. The camera burn is rendered on the 1080 capture frame, so it scales with the canvas it
lands in.

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
test crate, with stub `image`/`tracing` rlibs shaped like the used API. The issue-1370 lane measured this
way: across every pixel proof of run 68573319 (65 frames), the 1x slot crops read all 13 burns the
production decode had missed.
