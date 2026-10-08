---
paths:
  - "scripts/gen_measurement_clip.py"
  - "tests/python/test_gen_measurement_clip_1404.py"
  - "tests/measurement_clip_decode_1404.rs"
  - "tests/fixtures/measurement-clip-1404/**"
  - "src/av_run_pairing.rs"
  - "tests/av_run_pairing_clip_1404.rs"
  - "tests/av_run_recording_1404.rs"
  - "tests/av_sync_dock_reserved_origin_1404.rs"
  - "src/av_sync_decode_plan.rs"
  - "src/probe/av_sync_recording.rs"
  - "tests/av_sync_decode_plan_1404.rs"
  - "tests/av_sync_painter_head_1404.rs"
  - "tests/av_sync_painter_request_1404.rs"
---

# The camera-box measurement clip (issue 1404 Task 5 part a)

Nothing copyrighted may reach YouTube (owner amendment, issue 1404 comment 6016489928). SongPlayer's
test item and the cg OBS test scene `E2E test (cg)` play `measurement-clip-v1.mp4` instead of
music, so the CG segments carry the camera chain's own instrument.
`scripts/gen_measurement_clip.py` writes it, and the CLI prints the sha256.

## What the clip is: a synthesized 30 fps recording of the cam2 painter

Every value a decoder reads is the painter's, so the existing decoders read the clip with the
painter's rules (the YouTube-leg tick decoder only when asked, see the 911016 section):

| | value | pinned against |
|---|---|---|
| picture | 1920x1080, 30 fps, H.264 High yuv420p | frame-probe `--canvas-w/-h` defaults |
| QR | frame f = `vernier_ids(2f)`: left `P911016.{2f}.{pts_ns}.{crc32}`, right `{2f-1}` | `painter.rs`, `payload.rs` |
| geometry | EC-H, 4-module quiet zone, module = `700 // modules`, centred in its half, top 24 | `--qr-size 700`, `TOP_MARGIN_PX` |
| counter | 4 seven-segment digits (the frame number) in the gap between the QRs | clear of both QR images |
| marker | QPSK word 0xF / zero nibble / index / CRC-4, 442 Hz, c = 1, smoothing, amplitude 0.8 | `qpsk_marker.rs`, `camera-box-audio.hpp`, the shim's own params; the waveform within 1 int16 step of the vendored norihiro encoder `videogen.py` for all 256 indices |
| cadence | every 30 ticks (0.5 s), the first at tick 30, index = `tick & 0xFF` | `--audio-marker-cadence-ticks`, `frame_id_to_index` |
| bed | `MEASUREMENT_TONE_LINES_HZ[0]` (imported) at -30 dBFS as the guard measures it | `program_audio.py` |
| sound | 48 kHz stereo, L == R, int16 before the AAC encode | `to_stereo_i16` |
| log | `<clip>.markers.csv`: `# qpsk-params ...`, `index,frame_id,emit_ts_ns` (emit = content time) | `rig_marker_mirror` headers |

`pts_ns` of tick t = `round(t * 1e9 / 60)`. A side's gen_ts is its own tick's time, as the painter
bakes it (the settled half keeps its fresh-time stamp).

## Why the 60 Hz tick and the painter's index, not the frame number

The plan's literal "QR = frame number, index = frame / 15" fails two consumers already on dev. Both
were measured, Design-question issue 1404 comment 6046375956, pending the main's ruling:
- `youtube_leg_timeline.continuity(step=2)` proves a frame only when the tick advances 2 per frame.
  A frame-number tick proves 1 of 300 frames, so every CG window would read UNKNOWN.
- The program-audio guard's chain needs `idx_j - idx_i == round(60 * dt)`. Index = frame / 15
  gives chain 1, so the guard reads FOREIGN and stops every CG broadcast.

The tick rule lives in `TICK_HZ` / `TICKS_PER_FRAME` / `MARKER_EVERY_TICKS`; `TICK_HZ` IS
`program_audio.MARKER_INDEX_RATE_HZ`.

## 911016 — a reserved origin id, tick-excluded

- Rust: `recording_latency::MEASUREMENT_CLIP_RUN_ID`, in `NODE_BURN_RUN_IDS` (never the cam2
  Vernier tick), in every `all_burns` / `other_burns` exclusion in `recording-verdict.rs`, not
  logged as an unlocalized override (`burn_region_decode.rs`), no slot in `burn_regions.rs` (the
  echo gate never drops it). Never in `CAMERA_UNDER_TEST_NODES`.
- Python mirrors: `qr_align_pins.NODE_BURN_RUN_IDS`, `mv_skew_snapshot.RESERVED_RUN_IDS` (the
  issue-1159 drift guard parses `MEASUREMENT_CLIP_RUN_ID` too).
- `youtube_leg_ticks` reads 911016 as a tick ONLY when the caller passes `runs=CLIP_RUNS`; its
  default decode refuses it, byte-identical to before (`DECODER_VERSION` stays 2). Accepting it by
  default broke the one-line timeline (a perfect camera window next to a CG segment read 600 replay
  dups, a second play made every clip tick ambiguous). Since Task 5 part b the run-scoped decode
  keeps each frame's run and the timeline judges every run segment on its own TickClock
  (`.claude/rules/youtube-leg-verdict.md`, "Run-scoped decode + timeline"); the verdict reads CG
  windows with `--runs 911016 --clip-markers <clip>.markers.csv`.
- **The stream av-sync dock never pairs the clip** (ROZHODNUTE issue 1404 comment 6048179415 item
  3, Task 5 part b). Its audio is the cam2 painter's `mbc` room marker, so pairing the clip's tick
  fed a meaningless offset into its cluster, the LOCK-CORRECT suggestions and the av-step watchdog.
  `cb_video_qr_record` refuses a QR whose run is in `CAMERA_BOX_RESERVED_ORIGIN_RUN_IDS`
  (`camera-box-qr.hpp`: 911014, 911015, 911016) as its first statement, and both QR decodes drop it
  before any signal. During a CG segment the dock's QR freshness expires and its audio decode
  closes; a box seeing only reserved-origin QRs never latches camera-box mode. The ignore line is
  rate-limited per run (`CameraBoxIgnoredOriginLog`, one line per run per minute of frame time).
  The list is parity-pinned to the Rust ids and the python mirrors
  (`tests/av_sync_dock_reserved_origin_1404.rs`, `tests/python/test_reserved_origin_runs_1404.py`),
  behaviour by `camera-box-selftest.cpp`, the wiring also by the pwsh step "Assert dock never pairs
  a reserved origin QR (issue 1404)" in both windows-genlock workflows. Live after a FULL-bundle
  deploy on the stream box. Camera-node burns (9110xx below 911013) are still paired, as decided;
  see the ticket's follow-up note.
- **The CG A/V is measured offline from the clip's own marker:** `recording-verdict --av-sync <rec>
  --av-marker-log <clip>.markers.csv --av-run 911016` (`src/av_run_pairing.rs`: the clip's own tick
  per frame, refused when the clip restarts inside the cut; `--av-run` requires `--av-sync`). On the
  committed fixture it reads -0.15 ms clean, +99.85 ms with the audio 100 ms early.

## The deliverable

- `~/.claude/work-products/issue-1404/measurement-clip-v1.mp4`: sha256
  `5eaf3f9d93187fc0777a1d66e60f02de36eecf21cb5f4ae0bedea3a6dc28d2e2`, 19 232 425 bytes. Its marker
  log `measurement-clip-v1.mp4.markers.csv` has sha256 `0bd7f2b1...`.
- It took 212 s under `nice -n 19`, peak RSS 450 MB.
- It was checked with the real consumers:
  - ticks: 3599 of 3600 frames read the right tick, 0 wrong;
  - markers: 239 of 239 on both channels at the right time and index, 0 other words;
  - guard: 59 MEASUREMENT and the one start-up UNKNOWN, minimum chain 7, outside-band max 12.6 %.
- Deterministic on one machine for the same generator version with the same ffmpeg and numpy builds:
  x264 threads are pinned, muxing is bitexact and the metadata carries no time. Another CPU or build
  can take other float paths (the AAC encoder, sin/cos), and CI's BtbN ffmpeg gives other bytes than
  dev1's ffmpeg 6.1.1, so the published sha256 is dev1's.
- A clip is written only when ffmpeg read every frame, exited 0 within `timeout_s` (a watchdog kills
  its process group, the frame feed included) and the file holds exactly seconds x 30 frames
  (ffprobe); a clip longer than the 4-digit counter (333 s) is refused before any encode.
- The decode fixture `tests/fixtures/measurement-clip-1404/clip-v1-frame-1801.png` is frame 1801
  of the deliverable. zbarimg, OpenCV and the real rqrr plain pass all read both halves.

## Known limits

- **The painter path still never reads the clip** (by design): `recording-verdict --av-sync` without
  `--av-run` pairs through `RecordingFrame::tick`, which excludes `NODE_BURN_RUN_IDS`, so on a clip it
  measures nothing (pinned by `tests/av_run_recording_1404.rs`). Since the CI-timeout fix it stops
  after a cheap 60-frame head with "no cam2 painter tick ... measure the clip with --av-run 911016"
  (next section). Use `--av-run 911016`.
- **A loop seam (ROZHODNUTE 6048179415 item 2 decided 128 s; the generator's `SECONDS` and the
  published deliverable below are still 120 s, a follow-up on the ticket).** 120 s is not a whole number of index wraps and
  marker periods; 1920 frames (64 s) is, so 128 s is too. When a player loops the 120 s clip, the
  guard's chain over a span that holds the seam drops to exactly `MARKER_CHAIN_MIN` (4; the review
  probe swept every span offset), below the guard's own calibration bar of `MIN + 2`. One lost
  marker then reads FOREIGN and stops the broadcast once per loop. A 128 s clip keeps the marker
  line seamless (one marker slot at the seam is empty, the line continues). The loop also restarts
  the QR tick, so a YouTube-leg window must never hold a loop, whatever the length.
- **Level.** The marker plays at the emitter's digital level (the guard reads about -19 dBFS),
  about 16 dB over the camera chain's acoustic -35 dBFS.

## Tests and Tier-0

- `tests/python/test_gen_measurement_clip_1404.py` generates a 10 s clip once (about 16 s) and
  decodes it with the real consumers: the tick decoder with `runs` (and its default decode refusing
  the id), the timeline's own `continuity`, the dock shim on both channels, the guard's sampler
  loop. The shim is built into tmp by `build-qpsk-guard-shim.sh`. Determinism is checked on two 2 s
  clips. Fake ffmpegs prove the failure paths: one that exits 0 without reading, a hung one, one
  whose output ffprobe cannot count.
- The parameter pins read the Rust and C++ sources and the shim's compiled-in params; the waveform
  is pinned to `vendor/av-sync-dock/tool/videogen.py` (a smoothing mutant passed every other test).
- `tests/measurement_clip_decode_1404.rs` is probe-gated and runs in CI only: the flat recording
  decode (tick stays None), the echo-gated grouped decode the strih/stream analysis runs, the robust
  optical decode.
- `recording-verdict.rs` takes every cam2-optical exclusion list from ONE builder pair,
  `cg_segment_excluded_ids` (SongPlayer, cg OBS, the clip) and `optical_exclusion_ids`, pinned by a
  unit test, so a new CG id is added once.
- RED-proving the fail-fast tests against an older generator: the old code has no up-front length
  check, so `test_a_clip_longer_than_the_counter...` runs a full 334 s encode there, and a killed
  pytest leaves an orphaned ffmpeg plus a 64 MB `/tmp/measurement-clip-*` dir (the old code had no
  SIGTERM unwind). Leave that test out of a RED run against old code, and after any killed run
  `pgrep -af 'f rawvideo -pix_fmt gray'` and remove the leftover temp dirs.
- Re-verify the real rqrr on dev1 without cargo: link a small harness against the runner's release
  `librqrr-*.rlib` (`~/actions-runner-camera-box/_work/camera-box/camera-box/target/release/deps`)
  with plain `rustc --extern rqrr=<rlib> -L <deps>`. The same deps dir type-checks a probe-gated
  CLI test without compiling the crate: `clippy-driver --test --cfg 'feature="probe"' --extern
  serde_json=<rlib> -L <deps>` with a dummy `CARGO_BIN_EXE_recording-verdict` (and checks a clap
  attribute's behaviour: a 20-line scratch `#[derive(Parser)]` against `libclap-*.rlib`).

## The 4 s A/V fixture (Task 5 part b)

- `clip-v1-4s.mp4` = `write_clip(out, 4)` of this generator (607 700 bytes; 120 frames, 7 markers
  at 0.5 .. 3.5 s), its `clip-v1-4s.mp4.markers.csv`, and `clip-v1-4s.ticks.tsv` = its run-scoped
  YouTube-leg decode (7 columns; a test pins it to a fresh decode). `git add` of the mp4 trips the
  secret scan (hex runs in the H.264 bitstream): `# airuleset:secret-ok <reason>` on add and commit.
- `tests/av_run_pairing_clip_1404.rs` (default features, Tier-0 via a plain-rustc replica of the
  crate-root qpsk modules + `av_window` + `av_run_pairing`): the fixture's tick map + its REAL audio
  through the probe glue's own crate-root calls read 0 within a frame, +100 / -100 +/- 17 ms with the
  audio shifted by ffmpeg (`atrim=start=0.1,asetpts=PTS-STARTPTS` = audio early = picture lags = +;
  `adelay=100:all=1` = -). `tests/av_run_recording_1404.rs` (probe, CI) runs the compiled CLI on it.

## The `--av-sync` decode request and the painter head (the 480 s CI timeout)

The first CI run of `tests/av_run_recording_1404.rs` killed three tests at nextest's 480 s limit.
`--av-sync` decoded the clip with `analyze_recording`, which requires the cam1/strih/stream burns. The
clip carries none, so every frame ran the robust recovery: the issue-423 class (`.config/nextest.toml`).
`src/av_sync_decode_plan.rs` now decides the request:
- **`--av-run <run>`** requires only the run's own dual-QR (both halves), no node burn.
- **The painter path** first reads a 60-frame head that requires nothing
  (`probe::recording::analyze_recording_head`, which stops ffmpeg). It stops only when the head
  shows the clip and no rig signal (no cam2 tick, none of the three burns). Two residual edges are
  written in the module doc.
- **The painter path's full decode** asks for exactly the node burns that head read
  (`painter_full_request`, ROZHODNUTÉ 6051603225, next section). A head that read no QR at all (a
  QR-less pre-roll; the s3 VOD opens on 55 s of one) keeps the cam1/strih/stream request.

Measured with the runner's real release decoder on the clip (one thread):
- per frame: robust 434 ms; own-run request 124 ms; no request 114 ms; the plain rqrr pass alone
  is 55 ms, and the always-run Otsu pass is the other half of the fast path;
- per test (CPU s): `--av-run` 13.5-15, painter path 7.3 (the head only); before the fix ~52 each.

CI's Test job is a DEBUG build. Calibration from run 37708407889: the coverage job ran the three
old tests together, 360 robust frames in 4 vCPU x 522 s, about 13x the local release CPU time.
Estimate before you push: release CPU s x 13, divided over the 4 vCPU the concurrent tests share.

The real YouTube-leg windows: see "The painter path's full-decode request" below.

**Running probe glue locally without cargo:** link a replica crate against the self-hosted runner's
RELEASE probe rlib (`~/actions-runner-camera-box/_work/camera-box/camera-box/target/release/deps/libcamera_box-*.rlib`;
pick the one whose `.d` lists `src/probe/`). Its `lib.rs` contains:
- `extern crate cb_real;`
- `#[path]` mounts of the EDITED files: the crate-root modules, `probe/av_sync_recording.rs`, and
  `probe/recording.rs` cut before the pixel-proof code;
- `pub use cb_real::probe::{qr, payload, recording_decode, ...}` for the untouched decoder;
- a `recording_latency` shim that re-exports cb_real's and adds any const newer than that build.

Then the probe-gated `tests/*.rs` compile with `--cfg 'feature="probe"' --extern camera_box=<replica>
--extern image=<runner image rlib>` (a 30-line `tempfile` shim when a test needs it), run in release,
and `clippy-driver -D warnings` lints them. That ran the edited `av_sync_from_recording` end to end on
the fixture: clean -0.15 ms, audio early +99.87 ms, painter path stops with the head reason.

## The painter path's full-decode request (ROZHODNUTÉ 6051603225)

**The YouTube-leg fixtures are burns-OFF.** The 5.10 sessions ran with every measurement burn off.
Each `tests/fixtures/youtube_leg_1404/*-rec-*` clip carries only the painter run and the aux pair
911013 on every frame: no strih, stream or camera burn, even after the robust recovery. The old
"they carry strih + stream" premise was wrong.

`av_sync_decode_plan::painter_full_request(head, NODE_BURN_RUN_IDS)` builds the full-decode request
from what the painter head read:
- the strih / stream burns of `PAINTER_PATH_NODE_BURNS` the head read are mandatory; imag, cg and
  SongPlayer never are;
- the camera group is the issue-632 any-of group, only when the head read a camera burn. The group
  is the table's ids whose `burn_regions::slot_for_run_id` is the camera slot, never a literal list;
- a head that read QRs but no node burn requires nothing (a burns-off recording);
- a head that read no QR keeps the cam1/strih/stream request.

`--av-sync` pairs the painter tick with the audio marker, so the burns carry no A/V information.

Measured on dev2 (8 workers, the runner's release decoder):
- the 9 re-made stream-recording clips went from 0/1200 to 1200/1200 fast frames;
- the video decode alone: 32-58 s per 40 s clip with nothing required, against 101-190 s with
  the old request (`cmp.rs`, decode only). The whole new `--av-sync` run (head + full decode +
  audio) took 45-73 s on dev2 at load ~20;
- the JSON is byte-identical to each committed `.avsync.out` block;
- three committed burns-on rig frames keep strih / stream required and read the same tick.

Re-making the clips:
- `~/.claude/work-products/issue-1404/request-proof/README.md` (dev1-local) holds the source
  recordings, the exact `-ss` offsets, `cut.sh` (the avabs2.py command) and the
  today-vs-candidate comparison tool `cmp.rs`.
- Cut on dev1 (the original clips were cut there): with its ffmpeg 6.1.1 every re-made clip gives
  its committed `.avsync.out` JSON block byte for byte. The original clips are gone, so the clip
  bytes themselves were never compared.
- The 9 VOD fixtures cannot be re-made: YouTube says "Video unavailable" for all three broadcasts.

**The committed CI clip** is `base-rec-A-4s-540p.mp4`: the first 4 s of window A, 960x540, crf 28.
- The 540p scale keeps the debug decode short (60 head + 120 full frames).
- `base-rec-A-4s-540p.avsync.json` is the OLD code's output on that clip. It is identical with
  dev1's ffmpeg 6.1.1 and CI's pinned N-126264.
- `tests/av_sync_painter_request_1404.rs` pins that JSON with the compiled CLI, plus >= 95 % fast
  frames read from the last `recording analysis complete` line (`this_analysis_fast`).
- If the pinned ffmpeg or the decoder changes, regenerate the JSON with the OLD request on the same
  clip. Never write it with the new code. No CLI path runs the old request any more, so use a small
  plain-rustc harness against the runner's release probe rlib (the replica recipe above):
  - decode with `analyze_recording_with_grouped_burns_optical(clip, &PAINTER_PATH_NODE_BURNS, &[], None)`
    (= `av_decode_request(None)`);
  - run the rest of `av_sync_from_recording`'s painter path on those frames (first sample per tick,
    the coverage guard, the best audio channel, `av_offset_candidates_deduped`,
    `cluster_offset_ms(.., 4, 25.0)`);
  - print `run_av_sync`'s JSON keys with `serde_json::to_string_pretty`.

  The kit's `cmp.rs` is exactly that (its `today` decode). It matched the old
  `av_sync_from_recording` byte for byte (its `real` mode); that check works only on a runner rlib
  built before this change.
