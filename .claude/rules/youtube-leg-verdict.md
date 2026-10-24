---
paths:
  - "scripts/youtube_leg_*.py"
  - "tests/python/test_youtube_leg_*_1404.py"
  - "tests/python/youtube_leg_fakes_1404.py"
  - "tests/fixtures/youtube_leg_1404/**"
  - "tests/python/test_reserved_origin_runs_1404.py"
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
- One reserved id can be a tick, but only when the caller asks: 911016, the measurement clip's
  painted dual-QR (the CG segments' painter, `.claude/rules/measurement-clip.md`), read with
  `runs=CLIP_RUNS` (threaded `decode_ticks` → `decode_raw` → `_decode_range` job field 5 →
  `half_ticks_run` → `painter_tick_run` → `painter_payload`). The default decode refuses it and is
  byte-identical to before (`DECODER_VERSION` stays 2). The clip carries the 60 Hz tick (2 per
  30 fps frame), so `continuity(step=2)` proves its frames like a painter recording. A decode with
  `runs` is RUN-SCOPED: next section.

## Run-scoped decode + timeline (issue 1404 Task 5 part b, design comment 6048239795)

Why: the clip restarts its tick on every play beside the painter's line, and the one-line timeline
(`TickClock`, `_adjacent_events`) read the painter -> clip cut as a replay of every window tick
(a perfect camera window next to a CG segment: 300-600 dups).

- **Decode.** `decode_ticks(..., runs=CLIP_RUNS)` rows gain the run as a 7th column (None where no
  tick resolved); `load_run_ticks` reads it back as `(index, pts, tick, run)`. Halves read under
  two different runs (a blend at a cut) read as nothing. A right-only frame takes the local phase
  of frames of ITS OWN run (`resolve_ticks`): the clip and the painter tick on independent phases.
- **Cache.** `tick_cache_key(src, runs)` appends ` runs=911016` only for a run-scoped decode; the
  default key is the one restreamer's gate always wrote.
- **Timeline.** `carries_runs(rows)` = 4-column rows; 3-column rows take the old code byte for byte
  (the public functions dispatch, the old bodies are `_dupskip` / `_clamp_window` /
  `_vod_content_times` / `_vod_pts_for` / `_coverage_counts`).
  - `run_segments`: a seam at the first decoded row of another run and at a tick fall-back over
    `RESTART_TICKS` (a loop of a clip shorter than 30 s is NOT split: its ticks stay ambiguous and
    its windows read UNKNOWN; the clip is decided at 128 s, ROZHODNUTE 6048179415 item 2, while the generator and the current deliverable are still 120 s). Undecoded rows stay with the segment
    before them.
  - `RunTimeline` (cached per session, `run_timeline`): one TickClock per rec segment; a VOD row maps
    to the ONE segment of its run whose tick span holds its tick (the painter counts on, so that is
    unique). A clip tick sits in every play's span: such a VOD segment is pinned by content ORDER
    (`_order_match`) against the rec segments between the mapped VOD rows around it; a missing
    neighbour = the VOD's start / end clamp (keep the last / first plays); any other count mismatch
    stays unmapped -> UNKNOWN, never another play's verdict.
  - A window is judged on its ONE segment (`window_segment`, decoded rows only): a cut between runs
    or a restart inside is an error (UNKNOWN), like a publish or a part seam. dupskip / clamp /
    vod_pts_for / audio run on that segment's rows and the VOD rows that show it; coverage sums the
    segments the window touches (a fade nobody decodes at a cut counts as unproven, never an event);
    publish joins read every mapped VOD row.
  - **Foreign VOD frames (review round 1, the false PASS):** a decoded VOD row inside the segment's
    VOD stretch that shows ANOTHER run or a frame of ANOTHER segment is a dup frame AND enters the
    frame-count balance, so a replaced frame reads dup + hidden skip and an inserted one a dup
    (`foreign_frames` in a run-scoped result). Dropping those rows silently kept the balance and
    read a spliced frame as a clean PASS. A row of the segment's own run that maps to NO segment
    is NOT foreign (review round 2): a tick the rig held over AMBIGUOUS_S or a stretch only the VOD
    decodes maps nowhere, and counting it read a 3 s freeze as 90 dup / 90 skip; it stays in the
    segment's rows and the one-line code judges it as before.
  - `join_part_rows(..., restarting=runs)`: a later part without @start is placed by the previous
    part's last NON-restarting run (the painter), never by the clip (a part 1 that ended in a CG
    segment made the verdict UNKNOWN or silently misplaced part 2).
- **Verdict CLI.** `--runs 911016` (choices = CLIP_RUNS) + `--clip-markers <clip>.markers.csv`; each
  window records its `run`; a clip window's A/V calls `recording-verdict --av-sync --av-run 911016`
  with the clip's marker log (no `--clip-markers` = that window's A/V UNKNOWN). Without `--runs` the
  JSON has no `run` / `tool.runs` / `foreign_frames` key.
- **Rule:** a CG window must never hold a loop point or a cut; place E2E windows inside one segment.
- Tests: `tests/python/test_youtube_leg_runs_1404.py` (synthetic sessions with two plays, the VOD
  starting / ending inside a play, a spliced frame, a loop, a fade at a cut, multi-part placement,
  the real s2/s3 sessions read identically through one run, the committed clip fixture decoded, the
  CLI end to end with a fake probe that records its argv).
- Adding a parameter to the per-frame decode chain: the decode-mechanics tests swap `half_ticks`
  for a 3-argument lambda (`cheap_halves`), so `_decode_range` must keep the plain
  `half_ticks(frame, det, scale)` call on the default path (the `runs` keyword only when non-empty),
  and a default job keeps its 4 fields. A 4th positional argument failed 5 of those tests.

## The known-position read (design 6387919956 part a, 24.10.2026)

Why: a full `detectAndDecodeMulti` per plane (finder-pattern search over the whole plane) was most
of the decode. The E2E run 40110304642 attempt 3 recording (3840x2160, 9316 frames) took ~20 min on
dev1 and the VOD decode never finished inside the tool's 1500 s.

- `KnownPositionDetector` (`_decode_range` uses it per chunk; a caller passing a plain
  `cv2.QRCodeDetector` keeps the full read on every plane): per half and plane (gray / blue) the
  corner points of the painter QR the full detector last read. `_half_read` decodes the next frame
  at those points first (`QRCodeDetector.decode`, gray then blue) and runs the full detector (gray
  then blue) only when neither gives a valid painter payload: the FALLBACK, which stores the new
  points (a cut to another framing, a frame without the QR, the run-scoped clip at another place).
- **It reads a SUPERSET, never a different tick.** A decode at the stored points also reads a QR the
  finder search missed. Measured on the run's real files (dev2, old vs new decoder, same workers):
  the yu1zFSHFdu8 VOD gains 309 of 26940 halves and 5 frames with a tick, the 4K recording 380 of
  18632 halves and 6 frames; every added tick is the +2 step of its neighbours; no tick the full
  detector read changes; the 'x' frames stay 6 / 6. The committed crop of frame 2100 reads its settled
  right half too (2196753 under 2196754). So `DECODER_VERSION` stays 2 (the committed maps and every
  decode test are unchanged); the tests pin the superset property, not row identity
  (`tests/python/test_youtube_leg_known_position_1404.py`, synthetic ground truth + the real crops).
- Counted: known-position reads, `misses` (point reads with no painter payload), `fallback` (halves
  whose stored points all missed, so the full detector ran: always <= `full`), full detections per
  decode, on stderr, through
  `decode_raw(reads=...)` / `LAST_READS`, and next to each cached map as `<map>.reads.json` (the E2E
  block's `decode_reads`). A VOD's QR-less pre-roll costs full detections every frame (4 per frame,
  as before): yu1zFSHFdu8 has ~3490 such frames.
- Speed on dev2 (4 workers at nice 10; SyncNet, bakerion ffmpeg/nvenc jobs and node test runs load
  it, so the numbers move with the load): VOD 13470 frames 213.6 s (63 frames/s, load ~10) -> 131 s
  (load ~13), 139 s (load ~13), 254 s (load ~36); 4K recording 9316 frames 464 s (20.1 frames/s, load
  ~13) -> 308 s (load ~15), 355 s (load ~17), 462 s (load 20-36). Old vs new at the same load: ~1.6x
  (VOD) and ~1.5x (4K). On the 4K file the cost per frame is now
  the 4K video decode + the plane resize + two ~27 ms point decodes of a ~600 px QR, no longer the
  finder search (33 full detections in the whole file).
- `--prime-ticks <file> --out <dir>` (the E2E's early decode): `cached_ticks` into
  `<dir>/ticks-rec-1.tsv` under the normal cache key, so the verdict run with the same `--out` and the
  same recording path finds it; exit 2 on a failed decode (the verdict run decodes again). The real
  handoff is pinned by `test_the_early_decode_primes_the_cache_the_verdict_run_reads`.
- `DECODER_VERSION` stays 2 on purpose: a v2 map of either decoder is a valid map of the file (the
  superset property). The map can depend on the chunk boundaries (each chunk starts with no stored
  points) only by which extra halves it reads.

## Fail-closed rules the five review rounds found holes in (each has a test)

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
- A decode must hold EXACTLY the container's frames. OpenCV seeks a frame number by TIMESTAMP, so
  a chunk's own frame numbers say where the seek was AIMED, not where it landed: behind a real gap
  a chunk starts one frame early (240 rows from a 239-frame file). So the chunks are merged by pts
  (a frame two chunks read is kept once) and renumbered, and the merge must hold the container's
  own packet count (ffprobe; the real s3 recording and VOD: packets = decoded frames = rows). Any
  chunk failure takes ONE sequential pass from frame 0 (no seek), which must hold the count too: a
  missed seek (also of the LAST chunk: the file's tail used to vanish silently), a chunk that
  claims the end while a later one still reads, pts that do not rise inside a chunk (a merge by a
  misreported pts would reorder frames silently), a merge short of the count (a seek that landed
  late, or an early + late pair that would cancel in a plain count and mislabel every row between
  them). A full chunk grabs one more frame, so a file ending exactly on a chunk boundary is the
  end. A gap really in the file (OBS skipped a frame under encoding lag) is kept by the parallel
  decode, and the timeline makes a recording window with one UNKNOWN (the stream encoder may have
  kept that frame), while a VOD gap is simply a skip. The pts-step rule lives only in
  `timeline.timestamp_gaps`. Coverage counts frames by index span.
- The worker pool is SPAWNED, never forked, and bounded. A decode in the tool's own process (the
  one pass, or workers=1) starts OpenCV's threads there; a pool forked after that inherited their
  locked mutexes and hung forever (reproduced with the tool's module graph: no verdict, the rig
  lease held to the job timeout). The bound (1 s a frame per worker, at least 10 min) turns a lost
  worker into an error. A spawned worker imports the caller's main module: a script that calls
  `decode_raw` keeps its work under `if __name__ == "__main__":`.
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
  VOD via `yt-dlp -f 137`) with `youtube_leg_verdict.py --decode-ticks`; with the known-position
  read ~100 frames/s for a 1080p file and ~26-30 frames/s for a 4K file on dev2 with 4 workers (the
  full-detector decoder: 20-40 min per 20 min file on a loaded dev1). Decode on dev2, never on dev1
  (rig data stays off dev1). The cache key in the out dir carries the decoder version and
  the OpenCV version; CI pins `opencv-python-headless==4.13.0.92`, dev1's and dev2's.
- **The three VODs are gone** (`EK_cqSvsCKo`, `0q5ZdDMwRwQ`, `bsf_M-HdabY`: "Video unavailable",
  8.10.2026). Only the stream recordings are left, so a VOD fixture can no longer be re-made.
- **The 5.10 session recordings are burns-OFF**: no node burn on any frame, only the painter run and
  the aux pair 911013. The `*.avsync.out` A/V clips (40 s, `avabs2.py`) can be re-cut from those
  recordings. The offsets and `cut.sh` are in `~/.claude/work-products/issue-1404/request-proof/`
  (dev1-local). How `--av-sync` decodes them is in `.claude/rules/measurement-clip.md` ("The
  painter path's full-decode request").
- Real-pixel crops: the QR band at 0.5 scale as JPEG q95 (decodes exactly like the lossless crop;
  q90 changed one frame's result). Frame 2100's expected tick is pinned only to its bracketing
  anchors (2196752 or 2196754): the session decoder never read it.
- Test runtime: writing a clip costs ~10 s (a QR payload per frame) and a QR read ~60 ms a frame,
  so both modules share ONE written recording (`shared_rec_video`), a derived clip is cut by
  ffmpeg (`drop_frame`), and decode-MECHANICS tests (chunks, seeks, counts) read a frame
  fingerprint (`cheap_halves`) instead of the QR. Only the decode-content tests read QRs.
- Synthetic test videos are lossless FFV1 (mp4v blurred one QR into a miss); a synthetic colour
  QR uses EQUAL-GRAY module colours ((0,255,255) on (255,255,158)), a yellow-on-white QR still
  reads in gray. A synthetic dup/skip test needs enough frames for anchor pairs (a 5-frame toy
  window has none and is, correctly, an error).
