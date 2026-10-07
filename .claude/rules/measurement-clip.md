---
paths:
  - "scripts/gen_measurement_clip.py"
  - "tests/python/test_gen_measurement_clip_1404.py"
  - "tests/measurement_clip_decode_1404.rs"
  - "tests/fixtures/measurement-clip-1404/**"
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
  default broke the timeline (issue 1404 review): the clip restarts its tick on every play and the
  timeline keys a whole session on ONE tick line, so a perfect camera window next to a CG segment
  read 600 replay dups, and a second play made every clip tick ambiguous. **Task 5 part b
  blocker:** before a CG window is decoded with `runs`, the timeline must keep the clip's run apart
  from the painter's (run-scoped rows, a seam at a run change, `TickClock` per run and window), and
  the tick cache must be keyed on `runs`.
- **The stream av-sync dock reads the clip too.** `cb_video_qr_record`
  (`vendor/av-sync-dock/src/sync-test-output-video.cpp`) records any camera-box QR, with no run-id
  filter, and the clip's QR and marker agree with each other. So during a CG segment on the stream
  program the dock measures the CG path's A/V. That feeds its offset cluster, the
  `LOCK-CORRECT SUGGESTED` lines and the disabled av-step watchdog. Whether that is wanted is an open
  part-b decision on the ticket.

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

## Known limits (Task 5 part b)

- **`recording-verdict --av-sync` cannot pair the clip yet.** It pairs a marker through
  `RecordingFrame::tick` (`av_sync_recording.rs`), which excludes `NODE_BURN_RUN_IDS`. The CG A/V
  through the probe needs a Rust change, for example an explicit `--av-sync` tick run id. That is a
  CI/live step; the probe never compiles on dev1.
- **A loop seam (open decision on the ticket).** 120 s is not a whole number of index wraps and
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
  with plain `rustc --extern rqrr=<rlib> -L <deps>`.
