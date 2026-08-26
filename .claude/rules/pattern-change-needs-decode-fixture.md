---
paths:
  - "vendor/av-sync-dock/**"
  - "tests/av_sync_dock_video_decode*.rs"
  - "tests/fixtures/av-sync-dock-*/**"
  - "src/av_sync_dock.rs"
  - "src/aux_tick.rs"
  - "src/probe/painter.rs"
---

# Zmena vzoru => decode fixture test (#921, #690, #751/#754)

**The standing rule this repo cites everywhere by name but had never written down as a file**
(issue 690's investigation, this repo's own `.claude/skills/recording-decode/SKILL.md`, and the
#921 dispatch all reference `.claude/rules/pattern-change-needs-decode-fixture.md` as if it
existed — it didn't; this file closes that gap, #921 playbook-review): **any change to a
decode/recognition GEOMETRY or ALGORITHM (crop region, downscale factor, threshold method, retry
pass, module-sampling strategy) must be proven on a REAL captured frame offline, in a test, before
it lands** — never a geometry/algorithm tweak justified by math alone. A live decode-rate problem
that "should" be fixable by tuning parameters is not evidence the parameters are wrong; measure
first (see #921's finding below — the geometry was innocent, a completely different bug was real).

## How to build such a fixture harness AT TIER-0, without touching the rig or adding an `image`-crate dependency (#921 pattern)

The existing `tests/fixtures/burn-unreadable/*.png` pattern (`tests/burn_fixture_decode.rs`) needs
`--features probe` (the `image` crate) — fine for `src/probe/qr.rs` work, but the A/V-sync dock's
mirror headers (`camera-box-video.hpp`, `camera-box-qr.hpp`) are dependency-free STL-only C++ that
must stay Tier-0 testable (default features, no CI required). #921 established the Tier-0-native
version of the same fixture-proof pattern:

1. **Capture REAL frames via OBS-WS `GetSourceScreenshot`** on the live PROGRAM canvas (e.g. stream
   OBS scene PRO) during an active TEST session — no rig SSH needed, works from any session with
   OBS-WS access. This is exactly the pixel content `st_raw_video_camera_box_decode` receives (the
   dock taps the same raw program output). PNG, RGBA, native resolution.
2. **Convert to raw 8-bit grayscale luma** with PIL (`Image.open(...).convert('L').tobytes()`) —
   dimensions fixed/known (hardcode `1920x1080` etc. in the harness), no header. This sidesteps
   needing the `image` crate at Tier-0: the fixture is a tight row-major byte buffer the C++
   harness `fread()`s directly, byte-for-byte what the OBS pixel-format extraction step would hand
   the decode function on a real frame.
3. **Commit 2-3 REPRESENTATIVE fixtures**, not the whole capture set — `tests/fixtures/
   av-sync-dock-<issue>/*.y8`, ~2MB each at 1080p, well within this repo's existing binary-fixture
   norms (`tests/fixtures/burn-unreadable/` already carries 300KB-1.1MB PNGs + an `.mkv`).
4. **Write a temp-compiled C++ harness** (same `Command::new(cxx)` twin-harness shape as
   `tests/av_sync_dock_lock_926.rs` / `tests/av_sync_dock_cpp_mirror_gate.rs`) that `#include`s the
   REAL vendored headers and drives the REAL decode call sequence (top-band crop -> area-downscale
   -> quirc -> Otsu retry) — never a hand-simplified re-implementation, or the proof is worthless.

## GOTCHA — g++ CANNOT compile the vendored quirc `.c` sources directly; use a real C compiler

`vendor/av-sync-dock/deps/quirc/lib/*.c` is plain C89/C99 that relies on implicit `void*` ->
concrete-pointer-type conversions on `malloc`/`calloc` returns — legal C, but a **hard C++ compile
error** (`invalid conversion from 'void*' to 'T*' [-fpermissive]`) the moment `g++` (not `gcc`)
touches those files, even with no other flags. This is NOT the same false confidence as the
`camera-box-*.hpp` headers, which are genuinely dependency-free C++ and compile fine standalone —
quirc is real vendored C and must be treated as such.

**Fix:** compile each quirc `.c` file to a `.o` with a REAL C compiler (`cc`/`gcc`, respecting
`$CC`), then link those objects together with the C++ harness object via `g++`/`c++` (respecting
`$CXX`) — never pass a `.c` file to `g++` in the same invocation as the C++ harness source. See
`tests/av_sync_dock_video_decode_921.rs`'s `real_optical_frames_decode_and_resize_is_cached` test
for the working two-compiler-then-link pattern (compile `quirc.c`/`decode.c`/`identify.c`/
`version_db.c` with `cc -c`, compile the harness `.cpp` with `c++ -std=c++17 -c`, link all objects
with `c++`).

## A NEW painted-pattern ELEMENT: the synthetic round-trip lands with the change; the REAL-frame fixture is a hard PROMOTION precondition (issue 1196 precedent)

When the change ADDS a new painted element (not just a decode-parameter tweak), the two proof
layers split in time: (1) the painter-level synthetic render→decode round-trip (the
`CapturingPresenter` + `decode_qr_luma_all` pattern — see
`aux_tick_pair_round_trips_alongside_the_dual_qr_1196` in `src/probe/painter.rs`) MUST land in the
SAME PR as the pattern change — it proves geometry + wire format + decoder reach; (2) the
REAL-captured-frame fixture structurally CANNOT exist yet (the rig has not painted the new element
until the painter deploys), so it is mined from the FIRST rig run after the deploy and committed
then — and until it exists, the new element's signal stays REPORT-ONLY: the real fixture is a hard
precondition for flipping any gate that depends on the new element decoding through the real lossy
chain (projection → grabber → NDI → 4K upscale → mp4 — a synthetic crisp canvas proves nothing
about that). Worked example: the issue-1196 aux tick pair,
`.claude/rules/projection-tap-tear-detect.md`'s promotion-preconditions list.

## Mining the real-frame fixture from the E2E run's OWN retention — zero rig access (issue 1196 pattern)

When the fixture must come from a REAL rig recording, do NOT pull the multi-GB mkv or drive a new
extract: the E2E harness already retains flagged frames as `<partial>-pixels/frame-N.png` beside
each `/tmp/recording-e2e-<RUN>/{stream,strih}-partial-<RUN>.json` on dev1 — real 1920×1080
grayscale (mode `L`) frames, byte-identical to what the production decoder consumed. Validation is
double-anchored, entirely on dev1: (1) the partial's own `frames[]` entry for `frame_index` N is
the REAL production rqrr decode output for those exact pixels — if the payload you want to pin is
there, the committed test asserting the same decode function on the committed PNG is
deterministic; (2) `zbarimg -q --raw` (full-frame AND crops of the pattern's design rectangles) is
the independent second decoder, the #921/#186 "cv2 reads the same pixels" discriminator. Commit
the PNGs verbatim. Worked example: `tests/aux_tick_fixture_decode_1196.rs` +
`tests/fixtures/tear-781/stream-2099068429-frame-{1399,4792}.png`.

## #921's own finding — a useful discriminator for THIS class of problem

Before assuming a live decode-rate defect is a geometry/algorithm bug: build the fixture harness
FIRST and just measure the CURRENT unmodified pipeline against real frames. #921's own real
captured frames decoded 6/6 (100%) with **zero code changes** — proving the geometry/algorithm was
never the problem. The live diagnostic's own TEMPORAL shape (decode reliability degrading with
dock UPTIME, not a constant miss rate) pointed instead at an allocator-churn bug unrelated to
pixels at all (`quirc_resize()` called unconditionally every frame for an unchanging size — see
the `CbQrResizeCache`/`QrResizeCache` fix). **A live "it doesn't decode well" complaint is not
proof the decode ALGORITHM is at fault — measure against real frames before touching geometry.**
