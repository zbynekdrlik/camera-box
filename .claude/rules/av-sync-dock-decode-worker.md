---
paths:
  - "vendor/av-sync-dock/src/sync-test-output.cpp"
  - "vendor/av-sync-dock/src/camera-box-decode-mailbox.hpp"
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
- **The analysis runs on the dock's worker thread**, fed through the two-buffer latest-pending
  mailbox in `camera-box-decode-mailbox.hpp`:
  - the producer fills the pending buffer under the mailbox lock, and the worker swaps it with its
    working buffer, so the producer only ever waits for a pointer swap;
  - a frame published while the worker is busy REPLACES the pending one;
  - `dropped()` counts every replaced frame, reported as `decode_dropped=` at the END of the dock
    diag line (the existing tokens stay byte-identical; `bundle_state_gather.py` keys on
    `locked=yes`).
- **Decoders take the frame's OWN timestamp from the job**, never "now", so the marker/QR timing
  is unchanged by the hand-off.
- **Lifecycle**:
  - the worker starts in `st_start` before `obs_output_begin_data_capture`;
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

Signals (`qrcode_found`, `video_marker_found`, `sync_found`) are now emitted from the worker. The
dock UI copies each calldata and queues it to the Qt thread with `QMetaObject::invokeMethod`, so
the emitting thread does not matter.

Outside camera-box mode, the norihiro marker search reads four circle patches the video thread
cut around the corners the worker published after its PREVIOUS decode. That is one frame of
geometry lag. It is harmless because the phone's QR is stationary, and it is the price of never
reading the full frame on the worker.

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
