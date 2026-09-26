---
paths:
  - "vendor/av-sync-dock/src/sync-test-output.cpp"
  - "vendor/av-sync-dock/src/camera-box-decode-mailbox.hpp"
  - "vendor/av-sync-dock/src/camera-box-frame-copy.hpp"
  - "vendor/av-sync-dock/test/decode-mailbox-selftest.cpp"
  - "tests/av_sync_dock_decode_mailbox_1367.rs"
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
  fake decode (producer never blocked for more than 2 ms), latest wins, counted drops, no torn
  frame, and stop/destroy joining an in-flight decode.
  - For a race check, rebuild with `-fsanitize=thread` and run it under `setarch -R`. dev1's ASLR
    layout makes TSAN abort with "unexpected memory mapping" otherwise.
  - Any NEW mailbox guarantee needs a mutant that breaks it and a check that fails on that mutant.
    A timed run alone was blind to an oldest-wins mailbox and to a single shared buffer. The
    deterministic burst case (worker held inside a decode while frames 2..5 arrive) catches both.
- **The dock source**: `sync-test-output.cpp` type-checks locally with `g++ -std=c++17
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
  - The pwsh slice runs to the next ` static ` WITH comments left in. A comment inside
    `st_raw_video` must therefore never spell a banned call with its parenthesis.
  - The worker body's anchor is its full named signature. The forward declaration uses UNNAMED
    parameters so that `find()` cannot land on it.
