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

## Fail-closed rules the three review rounds found holes in (each has a test)

- Dup/skip is judged twice: between ADJACENT decoded VOD frames (the session tool's rule, which
  finds balanced dup+skip pairs), and by the FRAME-COUNT BALANCE between consecutive anchors (ticks
  both files show exactly once between decoded, adjacent, other ticks). Between two anchors the VOD
  must hold as many frames as the recording: rig repeats and skips are in both and cancel, an
  undecodable frame still counts, so content lost or repeated behind ANY number of undecodable VOD
  frames is counted (a 10 s splice behind 1 or 31 black frames FAILS). Never judge a gap pair by
  "the last copy of a0 / the first copy of a1": a rig repeat with one undecodable VOD copy made that
  read a false dup. A backward jump is a replay (dups). The real s2 / s3 windows balance to 0 over
  thousands of segments.
- VOD-only undecodable runs of 2+ frames where the recording decodes (>= 80 %) add up as
  `vod_blind_s`; over 0.1 s is UNKNOWN: a black, slate or flash VOD keeps its frame count, so the
  balance cannot see it. In the clean real windows every such run is ONE frame, so the floor is 2
  frames, not 30 (a 0.83 s black flash used to pass). Over 2 s of a window without an anchor pair,
  or more than 5 unjudged adjacent pairs (clean real windows: 0), is UNKNOWN.
- A clamp is tolerated only at the START (YouTube starts a VOD at its own live transition; the
  session-3 VOD opens with a 55 s QR-less silent pre-roll, content from 01:41:10). "The VOD ends
  early" is measured against the recording's own last decoded frame of the window, so a tail
  without the painter (a CG segment) is not a VOD defect; coverage runs to the window end, so that
  tail counts as unproven (UNKNOWN past 10 %), and audio is judged to the window end (it needs no
  painter). A window outside the recording, spanning parts, holding a publish, a StopStream
  (`--unpublish`, also under 1 s after the window) or a painter restart (tick falls back > 30 s) is
  UNKNOWN.
- A decode must tile the file: a chunk that read short or a missed seek (also of the LAST chunk:
  the file's tail used to vanish silently) is an error; a full chunk grabs one more frame, so a file
  ending exactly on a chunk boundary is the end, not a missed seek. A pts step outside 0.5..1.5 x
  the median is refused only when the decoded count differs from the container's own packet count
  (ffprobe). OpenCV seeks a frame number by TIMESTAMP, so in a file with a real gap the parallel
  chunks land a frame off (240 rows from a 239-frame file): on such a mismatch the file is decoded
  again in one pass from frame 0, no seek. A gap really in the file (OBS skipped a frame under
  encoding lag) is then kept, and the
  timeline makes a recording window with one UNKNOWN (the stream encoder may have kept that
  frame), while a VOD gap is simply a skip. Coverage counts frames by index span.
- Audio: VOD audio running out before the window end FAILS; fewer than 90 % of the expected blocks
  measured, or recording signal (> -50 dBFS, the only judged blocks) in under 25 % of them, is
  UNKNOWN; VOD sound > 20 dB above a quiet recording block is foreign (FAIL); only reliable blocks
  move the tracked lag; the block search is clipped to the audio. The session-3 pre-LIVE program
  audio is quiet in about half its blocks: a "50 % reliable" rule read that real window as UNKNOWN.
- Every subprocess runs through `youtube_leg_proc.run_bounded`: a timeout kills the whole process
  group (the probe's or yt-dlp's ffmpeg too), a failure carries the child's stderr tail into the
  reason, and `install_cleanup()` kills every running group when the tool gets SIGTERM / SIGINT (a
  cancelled CI job). `entry()` maps any crash to exit 2, never FAIL's 1.
- OPEN (main's call): the opening publish of a fresh broadcast cannot be judged from the VOD
  (its start varies 24-42 s after the publish), so criterion 3 only gates RE-publishes, and a late
  opening join is absorbed by the start clamp.
- The VOD loops its last ~0.6 s after the final stop (session 2: the last 34 ticks 13 times), so
  a window reaching the stop reads replay dups: pass every StopStream as `--unpublish` (Task 4
  wiring). Whether the loop is restreamer's drain or YouTube's is not established.

## Fixtures and re-decoding

- base/s2 maps are the session's LEFT-only `qrticks.py` decode (3 columns): a stale fresh half
  cannot be told from a real repeat in them, so their dup/skip counts (baseline A 11/11, R 53/53,
  B 2/2, all balanced) pin this tool's reading of those maps, not proven downstream events. s3 part
  1 and the s3 VOD are this decoder's output with raw left/right columns (`load_raw` ->
  `resolve_ticks`).
- Re-decode from the real files (stream box `C:/Users/newlevel/Documents/_NLMEDIA stream/RECORDINGS`,
  VOD via `yt-dlp -f 137`) with `youtube_leg_verdict.py --decode-ticks`; ~20-40 min per 20 min
  file with 4 workers on a loaded dev1. The cache key in the out dir carries the decoder version and
  the OpenCV version; CI pins `opencv-python-headless==4.13.0.92`, dev1's.
- Real-pixel crops: the QR band at 0.5 scale as JPEG q95 (decodes exactly like the lossless crop;
  q90 changed one frame's result). Frame 2100's expected tick is pinned only to its bracketing
  anchors (2196752 or 2196754): the session decoder never read it.
- Synthetic test videos are lossless FFV1 (mp4v blurred one QR into a miss); a synthetic colour
  QR uses EQUAL-GRAY module colours ((0,255,255) on (255,255,158)), a yellow-on-white QR still
  reads in gray. A synthetic dup/skip test needs enough frames for anchor pairs (a 5-frame toy
  window has none and is, correctly, an error).
