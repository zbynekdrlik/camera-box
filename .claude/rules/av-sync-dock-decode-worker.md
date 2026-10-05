---
paths:
  - "vendor/av-sync-dock/src/sync-test-output.cpp"
  - "vendor/av-sync-dock/src/sync-test-output-video.cpp"
  - "vendor/av-sync-dock/src/sync-test-output-audio.cpp"
  - "vendor/av-sync-dock/src/sync-test-output-internal.hpp"
  - "vendor/av-sync-dock/src/camera-box-decode-mailbox.hpp"
  - "vendor/av-sync-dock/src/camera-box-frame-copy.hpp"
  - "vendor/av-sync-dock/test/decode-mailbox-selftest.cpp"
  - "tests/av_sync_dock_decode_mailbox_1367.rs"
  - "vendor/av-sync-dock/src/camera-box-audio-worker.hpp"
  - "vendor/av-sync-dock/test/audio-worker-selftest.cpp"
  - "tests/av_sync_dock_audio_worker_1381.rs"
  - "tests/c/av_sync_dock_demod_bench_1381.cpp"
  - "vendor/av-sync-dock/test/cb-thread-faults.hpp"
  - "vendor/av-sync-dock/src/sync-test-dock.cpp"
---

# No analysis on libobs's video-output thread (issue 1367)

A raw OBS output's `raw_video` callback (the dock's `st_raw_video`) runs on libobs's ONE
video-output thread. Every raw output on the box shares that thread: the NDI outputs, the
recordings, the dock. Whatever `raw_video` spends past the frame budget, video-io pays for by
SKIPPING output frames for all of them.

Live 26.9.2026 on the resolume cg OBS (build afd7cc184):

- the dock decoded QR codes inline (the top-band gather, then up to two quirc passes);
- with QR content on screen OBS skipped 28 % of output frames (`output_skipped_frames` 2745 / 9821);
- the `cg-obs` NDI output fell to 16-18.7 fps, and the render itself stayed clean at 30.0;
- stopping `sync-test-output` restored 30.0 fps at once.

The strih-lx receiver saw the loss as `stamp_dup` / `stamp_gap` pairs plus relocks, which looked
like a genlock problem and was not one.

## The rule

- **`raw_video` does bounded copying only**: pixel extraction into a reused buffer, plus the frame
  timestamp. It never runs quirc, a marker search, a signal emit, a log line per frame, or any loop
  whose cost depends on the picture content.
  - The copies are the pure functions in `camera-box-frame-copy.hpp`: the top band (a row
    `memcpy` for 8-bit planar luma), norihiro's step grid, and the marker-circle patches. The
    self-test proves each against a plain reference read, and proves that every pixel
    `cb_marker_circle_row` hands the marker search lies inside the copied patch, over every frame
    edge. A new copy goes in that header with a self-test case, never inline in the dock.
  - What the copy still costs is measured: `publish_max_us=` on the diag line is the longest
    `st_raw_video` since the previous line (`cb_atomic_max_u64`, reset with `exchange(0)`). A
    4K RGBA output copies about 6 M pixels per frame on the video thread; read this value before
    blaming anything else for output skips.
- **The analysis runs on the dock's worker thread**, fed through the two-buffer latest-pending
  mailbox in `camera-box-decode-mailbox.hpp`:
  - the producer fills the pending buffer under the mailbox lock, and the worker swaps it with its
    working buffer, so the producer only ever waits for a pointer swap;
  - a frame published while the worker is busy REPLACES the pending one;
  - `dropped()` counts every replaced frame, reported as `decode_dropped=` near the END of the
    dock diag line, before `publish_max_us=` (the existing tokens stay byte-identical;
    `bundle_state_gather.py` keys on `locked=yes`).
  - `video_frames=` now counts the frames the WORKER processed, so `video_decoded(%)` is a decode
    rate per processed frame. Every frame OBS delivered is `video_frames + decode_dropped`.
- **The worker runs at NORMAL priority, never lower** (review round 2). The video-output thread
  takes `st->mutex` and the mailbox lock every frame, and the worker holds both briefly. A Windows
  `std::mutex` (SRW lock) has no priority inheritance. A below-normal worker preempted inside one
  of those sections on a busy box can therefore be starved for seconds, and the video thread waits
  that long: the very stall the worker removes.
  - `tests/av_sync_dock_decode_mailbox_1367.rs` bans `SetThreadPriority(` / `setpriority(` in the
    dock.
  - `st_decode_worker_thread_setup` (the mailbox's `on_thread_start`) only names the thread
    `avsync-decode`, which fits the 15-character Linux limit; `os_set_thread_name` truncates longer
    names. The name shows in `top -H` / gdb on Linux, and only to an attached debugger on Windows.
  - To make the worker yield in the future, first make the video-thread side lock-free (an atomic
    `cb_mode_active`, a seqlock for `marker_corners`). Only then lower the priority.
- **Lifecycle**:
  - `st_start` first stops any worker left from a previous start (it rewrites the quirc size and
    the video geometry the worker reads), then starts it before `obs_output_begin_data_capture`;
  - it is stopped and joined in `st_stop` (after `obs_output_end_data_capture`), in `st_destroy`,
    and FIRST in `~sync_test_output`, before `quirc_destroy` frees what it decodes with;
  - libobs disconnects the raw callbacks on its own end-capture thread, so one `raw_video` can
    arrive after `st_stop` returned. `publish()` on a stopped mailbox is a no-op returning false.

## Thread ownership after the split

| State | Owner | Guard |
|---|---|---|
| `qr`, `cb_qr`, `cb_qr_resize_cache`, `qr_corners`, `qr_data`, `video_level_prev*`, `video_marker_max_ts` | decode worker | none needed |
| `marker_corners` (the corners the video thread cuts the NEXT frame's marker window around) | worker writes, video thread reads | `st->mutex` |
| the ring `cb_video_ts_ns/valid`, `cb_mode_active`, `cb_video_last_decode_ts_ns`, `f/c/q_ms`, `sync_indices` | worker writes, audio thread reads | `st->mutex` (unchanged) |
| `start_ts` | video thread writes once; worker and audio thread read | `std::atomic` |
| `cb_video_frames_seen/decoded`, the mailbox's `dropped()` | worker / producer; the audio diag reads | atomics |
| `cb_publish_max_ns` (publish_max_us) | video thread raises it (`cb_atomic_max_u64`); the audio diag reads and resets it (`exchange(0)`) | atomic |

Since issue 1381 "the audio diag" and every other "audio thread" row above means the dock's AUDIO
DECODE WORKER, not libobs's audio thread -- see the next section.

Signals (`qrcode_found`, `video_marker_found`, `sync_found`) are now emitted from the worker. The
dock UI copies each calldata and queues it to the Qt thread with `QMetaObject::invokeMethod`, so
the emitting thread does not matter.

Outside camera-box mode, the norihiro marker search reads four circle patches the video thread
cut around the corners the worker published after its PREVIOUS decode. That is one frame of
geometry lag. It is harmless because the phone's QR is stationary, and it is the price of never
reading the full frame on the worker. When frames are dropped, the marker search's zero-crossing
interpolation spans the two frames the worker PROCESSED, not two adjacent frames. The phone method
therefore loses timing precision under decode load; the camera-box path is unaffected, because it
pairs by frame_id, not by crossing time.

## Verifying a change here (Tier-0)

- **The mailbox policy**: `g++ -std=c++11 -O2 -Wall -Wextra -Werror -pthread
  vendor/av-sync-dock/test/decode-mailbox-selftest.cpp`, then run the binary. It covers a 50 ms
  fake decode (producer never blocked: publish p95 < 2 ms and none reaching 15 ms -- the same pair as
  the audio-worker self-test, a single worst-case 2 ms bound flaked on a loaded CI runner at
  2.119 ms on 28.9.2026), latest wins, counted drops, no torn
  frame, and stop/destroy joining an in-flight decode.
  - For a race check, rebuild with `-fsanitize=thread` and run it under `setarch -R`. dev1's ASLR
    layout makes TSAN abort with "unexpected memory mapping" otherwise.
  - Any NEW mailbox guarantee needs a mutant that breaks it and a check that fails on that mutant.
    A timed run alone was blind to an oldest-wins mailbox and to a single shared buffer. The
    deterministic burst case (worker held inside a decode while frames 2..5 arrive) catches both.
- **The dock source**: each output TU (`sync-test-output.cpp`, `-video.cpp`, `-audio.cpp`; the
  video path is in `-video.cpp`, the audio path in `-audio.cpp`, `struct sync_test_output` in
  `sync-test-output-internal.hpp` since issue 1386) type-checks locally with `g++ -std=c++17
  -fsyntax-only -I<stub dir> -Ivendor/obs-studio/libobs -Ivendor/av-sync-dock/deps/quirc/lib`. The
  stub dir holds two generated files:
  - `plugin-macros.generated.h`, filled in from `src/plugin-macros.h.in`;
  - a 7-line `obsconfig.h` defining `OBS_DATA_PATH`, `OBS_PLUGIN_PATH`, `OBS_PLUGIN_DESTINATION`,
    `OBS_RELEASE_CANDIDATE` and `OBS_BETA`.
  The Windows compile proof is `windows-genlock-fast.yml`'s av-sync-dock build.
- **The anchors**: `tests/av_sync_dock_decode_mailbox_1367.rs` slices `st_raw_video`'s
  balanced-brace body, comment-stripped, and bans the decoders, `quirc_` and
  `signal_handler_signal(` in it. The pwsh step "Assert dock decode runs off the video-output
  thread" in BOTH windows-genlock workflows mirrors it; see `av-sync-dock-anchor-refactor-safety.md`
  for the three-place lock-step.
  - The pwsh slice is the brace-balanced body (`Get-DockBody`, issue 1386) WITH comments left in.
    A comment inside
    `st_raw_video` must therefore never spell a banned call with its parenthesis.
  - The worker body's anchor is its full named signature. Its declaration (in
    `sync-test-output-internal.hpp`) uses UNNAMED parameters so that `find()` cannot land on it.
    Since issue 1386 `st_raw_video`, `st_video_decode_job_run` and the audio worker handlers
    have external linkage, so their anchors carry no `static`
    (`av-sync-dock-anchor-refactor-safety.md`).

# No analysis on libobs's AUDIO thread either (issue 1381)

`raw_audio` (the dock's `st_raw_audio`) runs on libobs's audio thread: the thread that mixes every
source for every output. Live 27.9.2026 on the resolume cg OBS:

- the dock demodulated the whole program mix there, re-decoding its 3-marker window every push
  (~223 preamble magnitudes per passing position);
- camera-box mode had latched on ONE burn QR from a CG_CHAIN E2E run hours earlier and never
  cleared;
- with music on program the mixer ran 13-22 s behind real time and the FOH VBAN feed turned into
  silence and dropped audio, until OBS exited.

## The rule

- **`raw_audio` runs the gate and a copy, nothing else** (`cb_audio_gate_and_publish`): no decode,
  no log line, no source lookup, no signal. Its cost is on the diag line as `audio_publish_max_us=`.
- **The gate** (`cb_audio_decode_gate`, `camera-box-audio-worker.hpp`) opens only while ALL hold:
  - camera-box mode is on (`cb_mode_active`, still the latched "QR seen" flag for the video side);
  - the box has the measurement source `mbc` (`CAMERA_BOX_MEASURE_SOURCE_NAME`, declared once in
    `camera-box-audio.hpp`; the dock UI's `CAMERA_BOX_ASRC_SOURCE_NAME` is defined from it), looked
    up on the VIDEO decode worker in `cb_refresh_measure_source`, at most every 5 s of frame time,
    and read on the audio thread from an atomic -- `obs_get_source_by_name` takes the sources mutex
    and never runs on the audio thread. Its first answer and every change are logged at INFO: "not
    found" is the designed state on resolume and strih;
  - a camera-box QR was decoded within `CAMERA_BOX_TEST_SIGNAL_FRESH_NS` (20 s).
  On resolume and strih (no `mbc`) the audio decode never starts. The norihiro audio path stays
  unreachable once camera-box mode latched, so a closed gate means NO audio work at all.
- **A FIFO, not the video's latest-pending mailbox** (`CbAudioBlockFifo`, 64 reused slots, one
  worker `avsync-audio` at normal priority). The marker decoder needs CONTIGUOUS samples, so a
  latest-wins mailbox (which silently skips blocks) would stitch audio:
  - a full FIFO drops the NEW block and counts it (`audio_dropped=`);
  - the next block carries `CB_AUDIO_GAP_DROPPED`, and `st_audio_block_gap` resets every marker
    decoder before it is decoded (`decode_resets=`), so a marker cut by the gap never decodes from
    the stitched halves (the self-test proves both: no marker with the reset, marker 77 without);
  - the first block of a session carries `CB_AUDIO_GAP_SESSION`: decoders reset, the staleness and
    pairing watchdogs re-seeded, the lock forgotten, the dock shown LIVE again
    (`cb_audio_session_begin`).
- **A closed gate ends the session** (`st_audio_session_end`, after every block published before
  it): decoders reset, the lock forgotten (`cb_audio_forget_lock`: tracker cleared +
  `lock_state_changed(false)`), `sync_stale_changed(true)`, one `camera-box audio decode OFF --
  <reason>` line. No diag lines while the gate is closed.
  - **Every end is delivered** (review round 1): the pending ends are a ring of `(after, reason)`
    entries, so a gate that closes, reopens and closes again while the worker is still behind gets
    both ends, in order. A single "end pending" slot overwrote the first end and let the worker
    decode the second session's blocks on the first session's lock.
  - **Two ends with no accepted block between them are ONE end** (the second session's blocks were
    all dropped, so the worker never saw it). That merge is what bounds the ring: pending ends have
    distinct `after` values within [handled, accepted] blocks, at most `slots` blocks are unhandled,
    so there are at most `slots + 1` ends. The `+ 1` is an end queued after the worker handled
    every block and before it retakes the lock: a race, reached in most rounds of the self-test's
    stress loop on dev1, and a ring of only `slots` entries corrupts ends there.
    `end_session()` never allocates on the audio thread.
  - `stop()` discards pending ends (an output stop / restart), which is why a session BEGIN forgets
    the lock too.
- **The worker runs `st_raw_audio_camera_box` unchanged** on an `audio_data` view of the copied
  block (`st_audio_block_run`, the block's own timestamp), so everything that function and its
  helpers touch (the picker, the cluster, the lock audit/corrector, the diag tick, the ring reads)
  is owned by the audio worker. `decode_ms_max=` / `decode_ms_sum=` are its per-block handling time
  since the previous diag line.
- **Lifecycle** is the mailbox's: `st_start` stops a previous worker before rewriting the channel
  layout and starts it before `obs_output_begin_data_capture` (slots pre-sized for
  `AUDIO_OUTPUT_FRAMES` AND written, so the audio thread never allocates and never takes a
  first-touch fault -- see "No first touch on libobs's threads" below); joined in `st_stop`
  (right after the video mailbox), in `st_destroy`, and in `~sync_test_output` before
  `delete cb_audio_dec`.

## Verifying (Tier-0)

- `g++ -std=c++11 -O2 -Wall -Wextra -Werror -pthread vendor/av-sync-dock/test/audio-worker-selftest.cpp`
  then run it; `-fsanitize=thread` under `setarch -R` for races (clean at issue 1381). Mutants it
  kills: a single overwritten pending end, no merge over a dropped session, a merged end keeping
  the first reason, an end due one block late, a ring of `slots` entries instead of `slots + 1`
  (the stress loop), and the FIFO lock held across the handler (the latch waits are bounded, so
  that deadlock FAILS the run instead of hanging CI; stdout is line-buffered so the FAIL lines
  survive). The producer check is a paced publish loop judged on p95 (< 2 ms) + max (< 15 ms): an
  unpaced loop only waits once for a lock-holding worker and its p95 cannot see it, and a single
  worst-case 2 ms bound flakes on a loaded CI runner. No outcome rests on a sleep: a session end is
  proven delivered by a block published after it being handled, the lifecycle test waits for a
  flag the handler sets.
  - Test-writing trap: `taken()` counts a block BEFORE the worker frees its slot, so right after
    `wait_taken(n)` block n can still occupy one slot. A test that then publishes `slots` more
    blocks can see the last one dropped: wait for the previous blocks first, and MODEL a drop the
    race allows (the unseen session merges its end) instead of asserting it never happens.
  - A state only a race reaches (the ring's `+ 1` end) needs a stress loop that checks every
    delivered event; a correct FIFO can never fail it, and a mutant is caught in most rounds.
    Measure the kill rate (the lane ran the correct FIFO 8x and the mutant 8x) before trusting it.
- Shared dock constants go in `camera-box-audio.hpp` (both TUs include it). Do not include
  `camera-box-audio-worker.hpp` in the Qt TU `sync-test-dock.cpp` just for a name: it uses
  `std::min`, which a stray Windows `min` macro would break there.
- The bench `tests/c/av_sync_dock_demod_bench_1381.cpp` (`-Ivendor/av-sync-dock/src
  -Ivendor/av-sync-dock/test`, arg: the stereo mbc fixture) measures the worker decode and the
  audio-thread share in THREAD CPU time (dev1 runs at load ~20; wall time there is scheduler noise)
  and checks the decoder against a frozen copy of the pre-1381 kernel. Measured on the N100 (thread
  CPU): gate + copy <= 0.1 ms per stereo push for every signal (mean ~0.01 ms); worker 0.18-0.24 ms
  (music) / 0.56-0.7 ms (442 Hz tone), against 6.8 / 34.8 ms for the pre-1381 decoder.
- Anchors: `tests/av_sync_dock_audio_worker_1381.rs` (comment-stripped) + the pwsh step "Assert
  dock audio decode runs off the audio thread (issue 1381)" in both windows-genlock workflows
  (comments kept). A comment inside `st_raw_audio` or `cb_audio_gate_and_publish` must never spell
  a banned call with its parenthesis (`blog(`, `->push(`, `obs_get_source_by_name(`, ...), and no
  comment may sit between the tokens of a multi-statement needle.

# No first touch on libobs's threads (issue 1381)

A page fault is kernel work in the faulting thread's own context: a fresh page is zeroed, and under
memory pressure the kernel may reclaim first. On libobs's audio or video-output thread one fault
can stall the tick for milliseconds. So no producer-side buffer may be written for the FIRST time
on those threads.

## The rule

- **Every buffer `publish()` writes is sized AND written before the worker starts**, on the
  starting thread (`st_start`):
  - `CbAudioBlockFifo::start()` writes every slot plane with `assign(reserve_frames, 0.0f)` and
    then `clear()`s it (capacity and written pages kept). `reserve()` alone allocates but leaves
    the pages untouched: the first `publish()` into each slot page then faulted on the audio
    thread, on the first pass over the slots after each output start. `assign()`, not
    `resize()`: on a restart a plane can already hold a block, and `resize()` to that size writes
    nothing.
  - The video mailbox is generic over its job, so `CbDecodeMailbox::prepare_slots(fn)` runs `fn`
    on BOTH slots on the caller's thread under the lock, and refuses (calling nothing) while the
    worker runs. `st_start` calls it right before `st->cb_decode_mailbox.start(` with
    `st_video_decode_job_prepare`, which `assign()`s the top band (1.5 MB at 1080p, 6 MB at 4K) and
    norihiro's grid (filled every frame until camera-box mode latches). The sizes come from the
    same helpers the per-frame fills use (`st_cb_top_band_bytes`, `st_norihiro_grid_bytes`). The
    top band's plan is derived ONCE, in `st_cb_top_band_rows`: the byte size, the fill's buffer
    size and the rows it copies all come from it. A second plan anywhere (review round 3) lets a
    width/height swap copy more rows than the buffer holds -- a heap overrun on the video thread.
  - The ONE exception: the phone-mode marker `patches` grow on use. Their size follows the circle
    radius of a decoded PHONE QR (norihiro mode, never the camera-box rig path); a worst-case
    pre-size (a QR as tall as the frame) would hold ~9 MB per 4K output.
- **A new producer-side buffer gets the same treatment and the same check.** `reserve()` is never
  the pre-fault.
- **What it guarantees, and what not:** every slot page is RESIDENT as of `start()`, so libobs's
  threads never take a FIRST-touch fault. The audio FIFO's first `publish()` comes at the first
  gate opening (a fresh camera-box QR + `mbc`), which can be hours after the output auto-starts. A
  page the OS takes back in between (a Windows working-set trim or memory combining, a Linux
  swap-out) can still fault there. Only locking the pages would prevent that, and the OBS process
  cannot (the design's rejected `mlock` approach). Check it on the stream box after a deploy:
  `audio_publish_max_us=` on the first diag line after the gate opens.
- **Read the mode and norihiro's phone params in ONE lock section** (`st_raw_audio`). The decode
  worker sets `cb_mode_active` together with the rig's fixed `f` / `c` under `st->mutex`. With two
  sections, a block that read the mode just before the latch and the params just after ran
  norihiro's whole inline demod on the audio thread (growing its sample deque there), once per
  output start. Pinned by `the_audio_callback_reads_the_mode_and_the_phone_params_in_one_lock` and
  its pwsh twin in the 1381 audio step of both workflows.

## The check is a FAULT COUNT, never a timing bound

- `vendor/av-sync-dock/test/cb-thread-faults.hpp`: `cb_thread_page_faults()` = the calling
  thread's `ru_minflt + ru_majflt` from `getrusage(RUSAGE_THREAD)` (Linux; aborts on a refused
  read). The self-tests and the bench include it; the dock never does.
- The audio-worker self-test (`test_publish_never_faults_after_start`) and the mailbox self-test
  (`test_prepared_slots_never_fault`) bracket every publish after `start()` with it and require 0.
  The worker is held inside the first block / frame, so the next publishes fill every slot once
  with nothing dropped.
- **Test-writing traps** (each one made a check blind or flaky while it was written):
  - glibc `malloc` writes a chunk header into the page each chunk starts on, so `reserve()` of a
    ONE-page plane (the dock's 1024 frames = 4 KB) already makes nearly every page resident: 0-1
    faults per FIFO without the pre-fault. Test a MULTI-page plane too (4096 frames: 385 faults
    on a `reserve()`-only start). The Windows heap, where the dock runs, lays blocks out differently.
  - A new FIFO reuses the freed, already-resident heap of a destroyed one: run the fault test
    FIRST in `main()`, and keep every FIFO / mailbox of the test alive until its end.
  - The first execution of `publish()`'s own code can fault on a text page: a warm-up FIFO /
    mailbox (kept alive) publishes once before anything is counted.
  - The source block the test copies from is written before the first count (a fresh vector
    faults on its first read too).
- Mutants killed (lane proof): `start()` back to `reserve()` only (1 + 385 faults),
  `prepare_slots()` that calls nothing (770 faults) or only one slot (385), `prepare_slots()`
  that runs while the worker runs (the refusal checks fail).
- **The dock wiring is pinned in both languages** (review round 1: the self-tests use their own
  jobs, so deleting the dock's call kept every test green). `worker_lifecycle_follows_the_output`
  in `tests/av_sync_dock_decode_mailbox_1367.rs` and the pwsh step "Assert dock decode runs off
  the video-output thread (issue 1367)" in both workflows require `prepare_slots` with
  `st_video_decode_job_prepare` between `quirc_resize(` and the worker start, the prepare's two
  `assign()`s through the size helpers, the bytes helper as `video_width x st_cb_top_band_rows`,
  and the fills sizing and copying with the same helpers and no plan of their own. Killed in both
  languages: the call dropped, the band fill with its own formula, the grid not prepared, a fill
  copying with its own (swapped) plan, a bytes helper with its own plan.
- **The bench reports, it does not gate.** `tests/c/av_sync_dock_demod_bench_1381.cpp` keeps the
  mean / p99 audio-thread budget as CHECKs and REPORTS the worst single push against
  `CB_BENCH_AUDIO_THREAD_MAX_MS` with the producer's faults beside it (`faults=<first pass over
  the slots>/<every push>`). Thread CPU time still carries what a shared CI runner charges to the
  thread. Run 37345820890 read 2.42 ms (cpu ~= wall) on one push of a FIFO whose slots reused
  resident memory (the bench reads `faults=0/0` on every signal), so it was not a first touch of
  its slots. Whether it was another fault (a reclaimed page) or time charged to the thread is
  unattributed: that run had no fault counter, the new `faults=` column attributes the next one.
  A 2 s run on a memory-pressured runner may also legitimately refault a reclaimed page, which is
  why the deterministic check lives in the self-tests' short window right after `start()`.
- **Sanitizer builds report, they do not check.** Under `-fsanitize=thread` the TSAN runtime
  faults on its own pages inside the bracketed window (the mailbox self-test reads 1 fault there,
  on a slot an earlier publish already wrote). `CB_THREAD_FAULTS_EXACT` (in `cb-thread-faults.hpp`,
  0 under TSAN/ASAN) turns the two fault CHECKs into REPORT lines in such a build; the plain build
  CI compiles still checks. The TSAN race run stays clean.
