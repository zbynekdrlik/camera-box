---
paths:
  - "scripts/youtube_leg_*.py"
  - "tests/python/test_youtube_leg_*_1404.py"
  - "tests/python/youtube_leg_fakes_1404.py"
  - "tests/fixtures/youtube_leg_1404/**"
---

# YouTube-leg verdict tool (issue 1404 Task 1)

`scripts/youtube_leg_verdict.py` (A/V, verdict, CLI) + `youtube_leg_ticks.py` (painter tick per
frame) + `youtube_leg_timeline.py` (join, clamp, coverage, dup/skip, publish joins) +
`youtube_leg_audio.py` (block xcorr). Pure Python + cv2/numpy + ffmpeg/yt-dlp; the only Rust it runs
is `recording-verdict --av-sync` (CI probe artifact) as a subprocess. Shared with restreamer's gate.

## The painter tick: read it like the Vernier is painted

- `painter::vernier_ids`: LEFT = latest EVEN tick, RIGHT = latest ODD tick. A frame captured on
  tick T reads right = left + 1 (T odd) or left - 1 (T even): its capture PHASE. The canonical tick
  is the even one (left's).
- The half the painter just repainted (the FRESH half) is captured mid-transition as a green/blue
  pattern: gray reads nothing, the BLUE channel reads it. Measured: session-3 part 1, left 0/120
  gray, 99/120 blue. Gray first, then blue, per half.
- The phase MOVES during a session (session-3 part 1: odd early, even from ~280 s). A fixed
  "right - 1" for a right-only frame is wrong for whole stretches: a right-only frame takes the
  phase of the nearest both-halves frames (60 frames), never of its own cadence (that would hide a
  real repeat).
- The fresh half can read STALE: VOD frames 20174 / 20210 read left = the previous frame's tick,
  right = left + 1, between even-phase neighbours. That looks exactly like a one-frame capture
  slip, so such a frame (own phase contradicts the phase on BOTH sides) is left undecoded ('x').
  The gray left-only session decoder turned these into a false repeat + skip.
- The payload CRC is checked (zlib.crc32 of `run.tick.gen`, like `Payload::decode`); only the
  6-digit 9110xx ids are node burns, a 9-digit E2E RUN_ID starting 9110 is a painter id.

## Fail-closed rules the review found holes in (each has a test)

- A clamp is tolerated only at the START (YouTube starts a VOD at its own live transition; the
  session-3 VOD opens with a 55 s QR-less silent pre-roll, content from 01:41:10). A VOD that ends
  > 2 s before the window end FAILS. A window outside the recording, spanning parts, holding a
  publish or a painter restart (tick falls back > 30 s) is UNKNOWN.
- A decode with a hole (a chunk read short, a seek that missed, pts not rising) is an error;
  coverage counts frames by index span so rows missing from a map count as unproven.
- Dup/skip also judges pairs around undecodable VOD frames (recording frame count between the two
  ticks vs VOD frame count) and backward jumps (a replay = dups). Baseline A rose 10/10 -> 10/12
  this way: real dup+skip pairs next to an undecodable frame. Too many unjudged pairs is UNKNOWN.
- Audio: VOD audio running out before the window end FAILS; fewer than 90 % of the expected blocks
  measured, or recording signal (> -50 dBFS, the only judged blocks) in under 25 % of them, is
  UNKNOWN; only reliable blocks move the tracked lag; the block search is clipped to the audio, so
  the last block is not lost to the +/-40 ms margin. The session-3 pre-LIVE program audio is quiet
  in about half its blocks: a "50 % reliable" rule read that real window as UNKNOWN.
- Every subprocess has a timeout; `entry()` maps any crash to exit 2, never FAIL's 1.
- OPEN (main's call): the opening publish of a fresh broadcast cannot be judged from the VOD
  (its start varies 24-42 s after the publish), so criterion 3 only gates RE-publishes.

## Fixtures and re-decoding

- base/s2 maps are the session's left-only `qrticks.py` decode (3 columns); s3 part 1 and the s3
  VOD are this decoder's output with raw left/right columns (`load_raw` -> `resolve_ticks`).
- Re-decode from the real files (stream box `C:/Users/newlevel/Documents/_NLMEDIA stream/RECORDINGS`,
  VOD via `yt-dlp -f 137`) with `youtube_leg_verdict.py --decode-ticks`; ~20-40 min per 20 min
  file with 4 workers on a loaded dev1. The cache key in the out dir carries the decoder version.
- Real-pixel crops: the QR band at 0.5 scale as JPEG q95 (decodes exactly like the lossless crop;
  q90 changed one frame's result).
- Synthetic test videos are lossless FFV1 (mp4v blurred one QR into a miss); a synthetic colour
  QR uses EQUAL-GRAY module colours ((0,255,255) on (255,255,158)), a yellow-on-white QR still
  reads in gray.
