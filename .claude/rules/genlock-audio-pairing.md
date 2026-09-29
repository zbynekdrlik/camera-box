---
paths:
  - "src/genlock_audio_pairing.rs"
  - "vendor/obs-studio/libobs/obs-audio.c"
  - "tests/audio_telemetry_800.rs"
  - "src/genlock_audio_pairing_bench.rs"
  - "tests/genlock_audio_pairing_parity.rs"
  - "tests/genlock_audio_timecode_placement_1367.rs"
  - "src/genlock_shallow_av_bench.rs"
  - "src/genlock_forced_table_audit.rs"
  - "scripts/lib/genlock-forced-table-audit.sh"
  - "tests/genlock_forced_table_audit_1303.rs"
  - "src/genlock_audio_step_hold.rs"
  - "tests/genlock_audio_step_hold_parity_1381.rs"
  - "tests/genlock_audio_step_hold_wiring_1381.rs"
  - "tests/genlock_audio_pairing_lift/mod.rs"
---

# Receiver-side AUDIO genlock parity (#1303)

The owner directive (2026-09-13): genlock must lock BOTH audio and video. The video leg was
already genlocked (the FIFO holds every frame to `present_ts = wall_now − latency_ms`); #1303 makes
the AUDIO leg a first-class genlocked signal with the same evidence bar.

## The mechanism (three orthogonal pieces — don't conflate)

1. **The audio HOLD (phase)** — since issue 1367 (Option 3) a `genlock_fifo` source's audio is
   PLACED at its NDI timecode + the video's MEASURED stamp→present delay, mapped timecode → wall →
   OBS monotonic through the LIVE wall-vs-QPC offset. The original #1303 hold (arrival + the fixed
   `latency_ms`) was WRONG for any source whose FIFO sits deeper than one pin: see the issue-1367
   section below. Wired at the ONE seam `source_output_audio_data`
   (`vendor/obs-studio/libobs/obs-source.c`, right after the `sync_offset`/`resample_offset`
   adjusts): `in.timestamp += (uint64_t)genlock_term_ns;`. Default-safe: camera inputs keep
   `ndi_audio=false`, so only program-audio sources (cg/SongPlayer) carry audio at all.
2. **The ASRC servo (rate)** — `media-io/asrc-compensator.{h,c}` (#803/#912/#1084) disciplines the
   audio sample-clock RATE (ppm) against `genlock_wall_now_ns()`. ALREADY default-on and correct;
   #1303 changed NOTHING here. It is ORTHOGONAL to the hold above (rate vs phase). NB the constants
   are `ASRC_MAX_PPM`/`ASRC_MAX_SLEW_PPM_PER_S`/`ASRC_REGRESSION_*` — the old
   `ASRC_TIME_CONSTANT_S`/`ASRC_MIN_LOCK_S` were REMOVED in #1084 (a stale brief may still name
   them).
3. **Observability** — `obs_genlock_stats` (v2) + the `genlock-fifo audit` line carry
   `audio_enabled` / `audio_delay_ms` (the applied hold: the measured video delay in timecode mode,
   `latency_ms` before it settles, 0 = not held) / `audio_pairing_offset_ms` (applied hold minus the
   MEASURED video delay; 0 = paired, `-video delay` = the hold never fired). The audit line also
   carries the audit-line-only `audio_hold=off|latency|timecode|pending video_delay_ms=<smoothed>
   audio_health=<decide_audio_health, -1 = fps unknown>` (issue 1367), printed BEFORE
   `audio_enabled=` so `(long long)gs.audio_pairing_offset_ms);` stays the last argument (a
   `genlock_lock_indicator_guards.rs` anchor). Parsed by `src/jitter_audit.rs` (additive,
   forward-compatible; the three new keys are ignored there). Since ROZHODNUTÉ 5827497952 the line
   also carries `shallow_depth= shallow_capped= shallow_latches= audio_slew_ms= audio_slews=
   audio_steps= audio_withheld= audio_place_err_ms=` right after `n1_grows=` (audit-line-only;
   `audio_place_err_ms=` = the measured placement error of the samples, below).

## The pure decision + parity discipline

`src/genlock_audio_pairing.rs` is the Tier-0 authority: the video-delay tracker
(`video_delay_sample_ns` / `_smooth_ns` / `_round_ms` / `_moved` / `video_delay_track`), the hold
(`audio_hold_mode` / `audio_hold_ms` / `AudioHoldMode::token`), the placement
(`audio_wall_to_mono_ns` / `audio_place_term_ns`; the level shift of a placement is
`audio_level_shift_ns`), the offset
(`video_delay_reference_ns` / `pairing_offset_ms`) and `decide_audio_health` (Ok /
AudioDisabledOnProgram / AsrcSaturated / PairingOffsetExceeded — precedence in that order; the
pairing bound is HALF a frame, strict, since issue 1367). The C mirror is ONE contiguous block of
`static inline` helpers in `obs-source.c`, from `genlock_audio_present_delay_ns` through
`genlock_audio_decide_health` (self-contained: stdint/stdbool only, the `GENLOCK_VIDEO_DELAY_*` /
`GENLOCK_AUDIO_HOLD_*` defines INSIDE the block). `tests/genlock_audio_pairing_parity.rs` lifts it,
`cc`-compiles under `-Wall -Wextra -Wconversion -Wformat=2 -Werror`, asserts every helper is inside
it, and requires byte-identical results over vector spreads plus a tick-by-tick tracker sequence
(the #1003 lift-and-compile recipe; 16/16 mutations of the block go RED at landing). Keep the block
CONTIGUOUS and numerically identical to the Rust — every arithmetic wraps the same way (two's
complement u64/i64) on both sides.

**Lock-step anchors** (the #269 3-copy discipline): `tests/genlock_preload.rs`
(`audio_genlock_parity_present_1303` + the #1355 hold-change shift anchor), the std-only
`tests/genlock_audio_timecode_placement_1367.rs` (every wiring seam, runs locally), and the pwsh gate
in BOTH `windows-genlock.yml` and `windows-genlock-fast.yml`. A change to the wiring / the helper
signatures / the audit tokens must update all of them.

## Tier-0 (worktree worker: no cargo, no local sourced-lib)

- Pure module + the two-clock bench RED→GREEN: `rustc --test --edition 2021
  src/genlock_audio_pairing.rs -o <scratch>/t && <scratch>/t` (the bench is a `#[cfg(test)]`
  `#[path]` child, so this runs it too). `clippy-driver --edition 2021 --test -D warnings` on the
  same file gives CI's lint verdict.
- Parity gate LOCALLY: build a stub `camera_box` rlib from the one module (`lib.rs` =
  `#[path = "<wt>/src/genlock_audio_pairing.rs"] pub mod genlock_audio_pairing;`,
  `rustc --crate-type rlib --crate-name camera_box`), then `CARGO_MANIFEST_DIR=<wt>
  CARGO_TARGET_TMPDIR=<scratch> rustc --test tests/genlock_audio_pairing_parity.rs --extern
  camera_box=<rlib> -L <dir>`. Mutation-proof by pointing `CARGO_MANIFEST_DIR` at a scratch repo
  holding a mutated `obs-source.c`.
- The whole `obs-source.c` TYPE-CHECKS locally: `gcc -std=gnu11 -fsyntax-only -Wall -Wextra
  -Wformat=2 -I<wt>/vendor/obs-studio/libobs -I<wt>/vendor/obs-studio/deps
  -I<wt>/vendor/obs-studio/deps/libcaption -I<gen> -DHAVE_OBSCONFIG_H obs-source.c` with a
  hand-written `<gen>/obsconfig.h` (`OBS_DATA_PATH` / `OBS_PLUGIN_PATH` / `OBS_PLUGIN_DESTINATION` /
  `OBS_INSTALL_PREFIX` string defines). `blog()` carries a printf format attribute, so this also
  checks every audit-line specifier against its argument. Only the pre-existing `-Wcomment` at the
  fps-seqlock comment prints.
- `jitter_audit.rs` uses `serde_json` (not pure-std) — a plain `rustc --test` fails E0463; strip
  the `summaries_to_json` fn + its tests into a copy, or point rustc at a sibling worktree's
  `libserde_json-*.rlib`.
- `tests/genlock_preload.rs` is probe-gated and runs on CI only.

## Issue 1367 (Option 3) — the audio follows the video's MEASURED delay

**The defect.** A genlock FIFO presents the queue HEAD, so a frame reaches the program at
`stamp + (tick − head stamp)` — the head's age, logged as `ts_head_skew_ms`. That is `latency_ms`
only when the FIFO is exactly one pin deep. A shallow cg feed (resolume `sp-*_video`, pin 3 ms)
sits 2–3 frames deep: `ts_head_skew_ms=97` at `latency_ms=3`. The #1303 hold put the audio at
ARRIVAL + 3 ms, so it led the video by ≈ 94 ms (songplayer's own A/V gate measured +98 / +106 ms),
and `audio_pairing_offset_ms`, computed against the same pin, read 0.

**The decision (ROZHODNUTÉ on the ticket).** Leave the shallow VIDEO depth alone. Make the audio
follow whatever delay the video actually has. **Superseded in part by ROZHODNUTÉ 5827497952:** the
floating shallow depth re-timed the audio by a whole frame on every depth change (the audible
33 ms dropout), so a shallow source now LATCHES its video depth per lock
(`genlock-n1-pin-derived-depth.md`, the shallow section), its audio follows that latched depth, and
every hold change while audio plays is SLEWED (below). The measurement + live-offset placement
described here stay as they are.

- **Measure on the render thread at the PRESENT TAIL of `genlock_release_tick`**, on the frame the
  tick actually presents (`next_frame`, after every erase / drain / converge shed):
  `genlock_video_delay_sample_ns(genlock_delay_tick_wall, next_frame->timestamp)` with
  `genlock_delay_tick_wall = genlock_n1_tick_wall_now(wall_now)`. Three things were wrong with the
  first cut, which sampled `array[0]` at the head-skew site (review round 1):
  - on a source at N ≥ 2 × the canvas rate the STEADY branch presents the NEWEST matured frame, so
    the head over-read by (N − 1) source intervals (16.7 ms for a 60p source on a 30p canvas; the
    bench's `HeadSample` variant shows it);
  - the processing-wall skew carries tick lateness, so the sample uses the SCHEDULED instant;
  - a tick off the per-second grid (a wall step slewing back) is not sampled
    (`genlock_n1_tick_is_on_grid`), or a long step could apply a transient delay.

  A hold tick presents nothing new and is not sampled. **Dependency:** the tracker samples only an
  ON-GRID scheduled tick (the same dependency as the N==1 depth rule). On a box whose render tick
  never lands on the per-second genlock grid (not wall-slaved, or a non-integer canvas rate) the
  audio silently stays on the latency hold. Because nothing was measured, the pairing offset falls
  back to the pin and reads 0 (`audio_health=0`). The tell is `audio_hold=latency` +
  `video_delay_ms=0` on an audible source while `ts_head_skew_ms` shows the real, larger delay.
  Check that first when a box's audio does not follow its video. The sample is clamped to ≥ 1 ns, so it never
  produces the EMA's `0` "unseeded" sentinel. EMA 1/8 per sample, computed in wrapping u64 on both
  sides (the first cut's signed difference could overflow). This makes the tracker the THIRD
  `genlock_n1_tick_wall_now(wall_now)` reader: the count anchor is 3 in
  `tests/genlock_release_cadence.rs` and both ymls (`Count -ne 3`), and
  `const uint64_t genlock_delay_tick_wall = genlock_n1_tick_wall_now(wall_now);` is pinned count-1.
- **Quantize with hysteresis, apply once settled.** A move of the EMA by half a frame or more ARMS
  a 64-tick settle; at its end the rounded delay is applied only if it is STILL half a frame away.
  Applying at the crossing would latch the audio mid-step (≈ 83 ms for a 100 → 67 ms step), and the
  half-frame hysteresis would never fix it. A transient that reverses inside the settle applies
  nothing (pinned by a parity sequence: excursion to 100, back to 70 with 67 applied → stays 67).
- **Place through the LIVE offset.** In `source_output_audio_data`, `in.timestamp` is
  `tc + timing_adjust` at that point, so the term is `off_live + delay − timing_adjust` with
  `off_live = os_gettime_ns() − genlock_wall_now_ns()` read on EVERY packet whose new or previous
  hold is the timecode one (`genlock_audio_needs_live_offset`; a mic or a latency-mode source skips
  the two clock reads, and `GetSystemTimePreciseAsFileTime` is not free). The result is
  `tc + off_live + delay`. A mapping latched at the first packet walks by the wall-vs-QPC drift
  (resolume: 318 ms); the bench's `LatchedOffset` variant fails at 150 ms.
- **A change SLEWS, never steps (ROZHODNUTÉ 5827497952 — superseded the immediate re-placement,
  which WAS the songplayer gate's 33 ms dropout).** `audio_hold_action` decides every packet:
  `Withhold` (Pending), `Place` (a first placement after a withhold / genlock toggle, or a timeline
  discontinuity: `push_back = false`, level target shifted by `audio_level_shift_ns` = new term −
  (previous term − slew still owed)), `Continue`, `Slew` (a hold change while audio PLAYS:
  `genlock_audio_slew_remaining_ns += new term − previous term` of the SAME packet, so for a
  timecode→timecode change exactly the delay delta), or `Replace` (the legacy step, only for a
  source with no ASRC resampler, counted as `audio_steps=`). The slew rides the resampler:
  `asrc_process_audio` consumes `audio_slew_step_ns(remaining, callback dt)` at `AUDIO_SLEW_PPM`
  (1000 ppm = 1 ms per second, 0.1 % pitch) and passes `slew_ppm − applied_ppm` to
  `audio_resampler_set_compensation_ppm` (the plain `-applied_ppm` call — the #1325 anchor — stays
  for the no-slew case); `source_output_audio_data` BOOKS each step: it subtracts it from
  `next_audio_ts_min` through the one wrapping helper `audio_slew_book_ts_ns` /
  `genlock_audio_slew_book_ts_ns` (so the 70 ms `TS_SMOOTHING_THRESHOLD` guard never snaps a slew
  back to the old placement — the bench's `NoBooking` variant snaps once, 70.03 ms) and shifts the ASRC level setpoint by it (`asrc_compensator_shift_level_target`
  also shifts the open window's readings, so a ramp stays error-free for the level loop). The servo's
  own `applied_ppm` and its telemetry are untouched. **An owed slew is FOLDED when the ingest
  places the packet anyway** (review round 1): a `Continue` / `Slew` packet that still ends up
  placed (`!(push_back && audio_ts)` — a sync-offset change, or no `audio_ts` yet) lands at the
  full new term, so `audio_placed_slew_fold_ns` shifts the level setpoint by the remaining slew
  and clears it, or the resampler would keep stretching past the placement.
- **Withhold, then place once.** A wall-clock-timecoded genlock source with no video delay yet is
  `AudioHoldMode::Pending` (`audio_hold=pending`): its packets never enter the mix (they still reach
  the audio callbacks/monitoring), for at most `AUDIO_WITHHOLD_MAX_NS` (10 s after the first genlock
  packet, `genlock_audio_first_packet_ns`), so the first placement lands straight on the right delay.
  After the window, and for a non-wall-clock audio timestamp, `AudioHoldMode::Latency` (the #1303
  hold); a video delay that appears later is then SLEWED in.
- **A shallow N==1 source's audio follows its LATCHED per-lock depth** (`genlock-n1-pin-derived-depth.md`,
  the shallow section): the tracker takes `genlock_video_delay_lock_ms(D, measuring, interval)` —
  a latched D → `round(D · interval)` applied as-is (constant between relocks, the EMA keeps running
  for `video_delay_ms=`); no D yet but a window measuring → `GENLOCK_VIDEO_DELAY_LOCK_PENDING` (apply
  nothing, so the audio stays withheld); otherwise 0 = the free Option-3 tracker (N>=2 sources).
- **Between placements** packets append back to back, and the genlock ASRC (the rate servo
  disciplined against the wall clock plus its level loop) holds the captured depth. That is what
  absorbs the wall-vs-QPC drift; the placement only has to be right when it happens.
- **The pairing offset is a PROXY.** `audio_pairing_offset_ms` = (applied hold − the slew still
  owed, `audio_applied_delay_ns`) − measured delay: mid-slew it reads how far the audio still trails
  (a one-frame re-time reads −33 ms at the start and walks to 0 over ~33 s; review round 2). That is
  honest and has two visible effects: the audit's half-frame `audio_health=` reads
  PairingOffsetExceeded for the first ~16 s of every one-frame re-time; a one-frame re-time can
  also turn the LOCK widget DEGRADED (its strict 33 ms bound) for ~1–2 s at the slew start (the
  settled residual on top of −33), and a TWO-frame re-time (a relock onto a much slower band,
  −66 ms) for ~33 s — shorter than the lock-alert watchdog's 2-pass confirm (a 5 min timer), so a legitimate
  relock cannot page; read `audio_slew_ms=` on the audit line before treating either as a fault. It
  never observes where the audio samples actually sit, so a wrong placement, or a depth the rate
  servo walked, would still read 0. Real A/V proof stays with an end-to-end measurement (the
  songplayer A/V gate, the camera-box E2E A/V gate).
- **Health.** The audit's `audio_health=` uses `decide_audio_health` at half a frame (the
  program-source / ASRC-saturation inputs pass 0 at that seam). The LOCK widget's own
  `GENLOCK_AUDIO_PAIRING_BOUND_MS` stays one frame (33 ms). It now sees REAL residuals, which were
  structurally 0 before, so a single-frame step settling (≤ 33 ms truncated) does not degrade it.
  It DOES degrade for the ~2 s latency-mode window after a source starts (offset ≈ 3 − delay), and
  permanently for an audio-enabled genlock source whose audio timestamps are not wall-clock
  timecodes (it stays on the latency hold, and its audio really is off by that much). Both are
  honest readings; the start window is far shorter than the lock-alert watchdog's 2-pass confirm
  (a 5 min timer), so it cannot page on it.
- **Deep sources are unaffected in effect.** Stream `NDI 2ME PGM` and every strih/stream/imag input
  carry no NDI audio (the certified table below), and a deep source's video keeps the pin rule: the
  shallow halves act only on a non-deep source, and a deep source latches the pin rule's own
  `base + 1` (the code path changed, the presented depth did not).

**A timeline reset must PLACE, and the pairing offset reads the SAMPLES (live 25.9.2026 12:31).**
After each SongPlayer song change on resolume the `sp-slow_video` mix-buffer level fell
111 → 46 → 13 ms (the `asrc:` line's `level_avg`, the ASRC then re-capturing the low depth) while the
audit read `audio_delay_ms=133 audio_pairing_offset_ms=0`; songplayer measured the audio +101 ms
early. Mechanism: a >2 s audio timestamp jump runs `handle_ts_jump`, whose `reset_audio_data` puts the
buffer start AND `next_audio_sys_ts_min` on the ARRIVAL instant, so the packet's pre-term timestamp
equals it and OBS APPENDS it there — an append ignores `in.timestamp`, so the genlock term never
reaches the samples. The latched D keeps the hold constant across the relock (`Continue`), so nothing
re-placed it; the floating depth of 8151a12ac used to mask it by changing the hold. Fix, all in the
pure module + mirror:
- `audio_push_back_allowed(push_back, timeline_reset, mode)`: an ACTIVE hold never appends right
  after the ingest reset its timeline in this packet. The ingest flags both reset sites
  (`genlock_timeline_reset`) and corrects `push_back` BEFORE `genlock_audio_hold_action` reads it.
- The ingest MEASURES every packet under `audio_buf_mutex`: `audio_actual_place_ns` (appended → the
  buffer end `audio_ts + buffered`; placed → its own timestamp) minus the intended `in.timestamp`
  (`audio_place_error_ns`), EMA 1/16 per packet (`audio_place_error_smooth_ns`), reset whenever the
  hold is not active. **Design 5845361166:** the intended timestamp is the RAW stamp's
  (`audio_intended_raw_ns` / `genlock_audio_intended_raw_ns`: `data->timestamp` + the same offsets
  and term), measured BEFORE the 70 ms TS smoothing snap — against the snapped `in.timestamp` a
  skipped or duplicated sender slot read 0. This holds for BOTH active holds (a latency-hold
  source's `audio_place_err_ms` / pairing offset now also read the raw-stamp error, the arrival
  jitter of its stamps included). The same error (+ the owed slew) is the TIMECODE ASRC's
  input: `asrc_timecode_ingest` books its jumps at 1000 ppm and feeds the rate the stamp advance
  (`asrc-bench-harness.md`, the issue-1367 TIMECODE section). `audio_pairing_offset_ms` now uses `audio_realized_delay_ns` = hold + that error
  (an owed slew is in it; hold − owed slew only until a measurement exists), and the audit carries
  `audio_place_err_ms=`. A hold missing from the samples reads as the gap, never 0.
- The benches model OBS's append-after-reset (the `AudioLeg` used to re-place on every resync, which
  hid the defect); every bench with a sender restart went RED on the old verdict (|A/V| 96.9 ms). The
  measurement runs the PRODUCTION formula (`audio_actual_place_ns` over `audio_ts` = the mixer read
  position 64 ms behind real time + the buffered length of an OBS-style buffer; after a reset
  `audio_ts` = the arrival instant and an empty buffer), cross-checked against the modelled truth on
  every packet (≤ 0.00001 ms). The shallow bench compares the REPORTED pairing offset with the TRUE
  A/V of the samples on every settled tick (≤ 1.03 ms), and the anti-tautology `legacy_append` run
  loses the hold (96.9 ms) and the audit reports it within 1.03 ms.
- **Known limit: rejects at the FIRST lock can outlast the withhold.** While a first window measures,
  the tracker is PENDING; three spread-rejects plus the latching window is 4 × 90 on-grid ticks
  (12 s at 30 fps), past `AUDIO_WITHHOLD_MAX_NS` (10 s). The audio then plays on the #1303 latency
  hold and the latched delay is slewed in (the existing, tested withhold-expiry path) — only on a
  start that itself carries a multi-second transient.

**Audio-thread stall probe (the FOH-click report, 25.9.2026).** The obs-vban raw-audio output on
resolume sent with 308–378 ms gaps while the recording was clean. `obs-audio.c` `audio_callback`
records the gap since the previous tick's entry and its own duration; the 60 s `#800` dump logs
`audio-stall #1367: tick_gap_max_ms= callback_max_ms= ticks= ticks_over= tick_ms=` (ticks_over = gaps
over 1.5 ticks) and resets. Healthy: `tick_gap_max_ms` near `tick_ms` (21.3 at 48 kHz), `ticks_over=0`.
The genlock audio path takes no lock beyond the pre-existing `audio_buf_mutex`, calls no blocking API
(two clock reads) and has no loop, so it is not a stall candidate. **Reading the probe (review round
1):** `media-io/audio-io.c` runs `audio_callback` and then `do_audio_output` — every raw-audio output
callback, obs-vban included — on the SAME thread, so a blocking output callback delays the NEXT entry:
- `tick_gap_max_ms ≈ callback_max_ms` (both large) → the mixer / `execute_audio_tasks` stalled;
- `tick_gap_max_ms ≫ callback_max_ms` → the output callbacks (obs-vban's send) or thread scheduling
  stalled — NOT the mixer;
- a clean probe (`ticks_over=0`) while VBAN still gaps → obs-vban's own send path (its socket /
  sender thread), outside this thread.
Anchored by `tests/audio_telemetry_800.rs` (both `audio_callback` returns close the probe's tick).

**Lock-step anchors of THIS change** (all must move together): the std-only
`tests/genlock_audio_timecode_placement_1367.rs` (tracker at the present tail after the presented
frame, exactly one call site, the ingest seams incl. the withhold / action / slew / booking, the
asrc_process_audio slew, the offset reference, the audit tokens; the old unconditional
`push_back = false; asrc_compensator_shift_level_target(&source->asrc, genlock_audio_place_shift_ms(…));`
must stay ABSENT), the `tests/genlock_preload.rs` slew + placement anchors, the
`genlock_n1_tick_wall_now(wall_now)` count of 3 + the count-1 tracker read in
`tests/genlock_release_cadence.rs`, and the matching `-notmatch` / `-match` / `Count -ne 3` anchors
in BOTH `windows-genlock*.yml`.

**The two-clock bench** (`src/genlock_audio_pairing_bench.rs`, a test-only child): wall and QPC
clocks with 300 ms of drift over an hour, scripted depth changes of the FREE tracker (the N>=2
path), a sender restart (3 s silence, audio resync) and an OBS restart (all receiver state zeroed),
a first-order ASRC (rate servo that locks after 5 s with τ = 20 s, plus a clamped P level loop).
The audio leg is `AudioLeg` (the production withhold / action / slew / level-shift decisions),
shared with the shallow bench. The A/V error is measured on every tick outside a 6 s window after
each event and outside a deliberate slew (1 ms per second toward the new depth). Production:
|A/V| ≤ 5 ms, 3 placements (start, sender restart, OBS restart), 0 steps, every depth change a
slew. `LatchedOffset` and `LegacyLatency` still fail (> 50 ms). A 2× source pairs on the presented
frame; `HeadSample` over-reads by a source interval.

**The shallow two-clock bench** (`src/genlock_shallow_av_bench.rs`, a child of the bench above)
drives the REAL N==1 release port (`genlock_grid_bench::Fifo`, now with the shallow rule) from a
jittery sender at the live floors (`NDI test` 22–31 ms → D 2, `sp-slow_video` 40–64 ms → D 3,
`CG-obs` 28–40 ms straddling a frame → D 3), with an OBS restart and a sender restart per hour.
Clean feed: the depth latches once per lock and never moves, 0 hold/shed/drain/underrun/late-hold,
3 placements, 0 slews, 0 steps, |A/V| ≤ 1.97 ms. Disturbed feed (lost frames + 45 ms late spikes):
≤ 1 correction per disturbance, back on D within the throttle window, the audio never moves.
`shallow_depth_rule = false` on the same feed: the depth random-walks (modal depth < 90 % of
presents) and the free tracker re-times the audio 36 times. A min-latency box REPORTS a floor
over base + 1 and applies no depth (0 corrections). Review-round-1 scenarios: a band straddling
the SECOND frame edge (50–80 ms) locks 4 frames; a band that rises mid-run with no gap
(28–40 → 70–95 ms, every rounded floor = D) re-measures 3 → 4 at the rise — D 4 is presented from
~1510 s, not only after the later sender restart — and slews the audio exactly once; a sender
restart onto a slower band (60–80 ms) relatches 3 → 4 and slews once — 0 steps in all of them.
(A 60–80 ms RISE floors at 2 or 3 and never re-measures; the first round's scenario used it and
only passed through the sender-restart relatch — corrected in review round 2.) The SLEW window is
measured on its own, on every presenting tick from the re-latch on (review round 3 — the settled
gate reopens seconds after the slew starts, and the first seconds are the largest): while the audio
walks onto the new hold it trails the video by ≤ 35.0 ms (rising) / 33.2 ms (sender restart) —
bound `SLEW_MAX_AV_MS` = 40, the songplayer gate, and the peak must be ≥ 30 ms so the start is
provably inside — for 990 ticks (33 s); the settled `|A/V| ≤ 5 ms` excludes those ticks. The
40 ms slew bound is proven for ONE-frame re-times only: a two-frame re-time trails by up to ~66 ms
for ~26 s and has no bench scenario yet (a follow-up candidate). The bench's rate estimate converges on the TRUE
drift by construction, so it ASSUMES drift is absorbed between placements (the real servo is
proven by `src/asrc_bench.rs`). What it proves is that each placement lands on the live offset.

**Live acceptance (supervisor).** Full-bundle deploy on resolume, strih-lx and stream. On resolume
the `sp-*_video` audit shows `audio_hold=timecode`, `audio_delay_ms` = the latched
`shallow_depth × 33` (constant between restarts), `audio_pairing_offset_ms` within ±16 with
`audio_health=0`, and `audio_slews=` / `audio_steps=` flat at 0 in steady state (`audio_withheld=`
grows only in the first seconds after an OBS start). A depth change slews at 1 ms per second, so
a 2-frame re-time takes ~67 s: read `audio_slew_ms=0` before trusting a post-restart A/V
measurement (the E2E settle-wait does not wait on it yet). The songplayer post-deploy A/V gate
(±40 ms, 0 dropouts) passes across two OBS restarts. The camera-box release E2E A/V gate stays
green.

**Known limit (not this slice): the ASRC servo's own stretch still drifts the TS-smoothing
timeline.** The rate servo stretches the samples to follow the wall-vs-QPC drift, so
`next_audio_ts_min` walks away from the source timestamps by the accumulated correction; at 70 ms
(`TS_SMOOTHING_THRESHOLD`) the ingest re-places at the raw timestamp (a 70 ms snap, every
`70 ms / drift` — ~2 h at resolume's ~10 ppm). The slew books its own steps out of that timeline;
the servo's correction does not. Reported to the supervisor as a follow-up candidate.

**The global audio buffering is a FLOOR (issue 1367).** Every genlock OBS launch now starts the mix
window 85 ms behind real time (4 ticks at 48 kHz, `obs-genlock-audio-buffering.h`), and OBS's own
dynamic increase stays active above it. The genlock audio holds above ride on top of that, so a cg
feed's audio on resolume still grows the buffering on a media start (a loud `ABOVE the floor` line,
never dropped). Full rule: `genlock-audio-buffering-floor.md`.

## Issue 1381 — the timecode audio across a WALL STEP: a per-source skew hold (design 5882391108)

A dantesync date step (29.9.2026: +682 ms at 02:36 CEST, +89.7 ms at 04:00) moves the receiver's live
wall→mono offset at once, while the sender's stamps follow later. SongPlayer
(`sp-server/src/playback/audio_emitter.rs`) never jumps its grid stamps under 1 s: it re-anchors only
when more than 1 s late. A forward step becomes a catch-up burst, a backward step a pause, and its
WallClock follows after 2 confirming resamples (3.3–6.7 s). Until then the old code read the step
as PLACEMENT ERROR. The 682 ms step then booked the capped 100 ms owed amount over and over, and the
excess leaked into the smoothed error. The result was restore=1, ~151 ppm applied, and a 70 ms
TS-smoothing re-placement every ~80 s for minutes. The 89.7 ms step kept the audio +81…87 ms off for
over a minute.

- **The hold** (`audio_step_hold` in `src/genlock_audio_step_hold.rs`, a path child re-exported by
  `genlock_audio_pairing`; C port `genlock_audio_step_hold` in the contiguous pairing block, wrapper
  `genlock_audio_step_hold_source`). A timecode packet whose live offset jumps by more than
  `GENLOCK_WALL_STEP_MIN_NS` (2 ms, the render tick's own threshold, from
  `obs-genlock-wall-step.h`) keeps the PRE-step offset. The term, the previous term and the
  timecode ASRC's stamp all map through `genlock_off_ns`. A step within one packet, a first packet,
  a timeline reset or a joint step (the stamps jump in the same packet) never holds.
- **The reference is the NOMINAL stamp age** (review round 1): the stamp's live-wall age while the
  two walls agree. It is seeded by the source's first timecode packet, then WARMS UP for 30 packets:
  every packet moves it by 1/4 of the difference, in band or not, so a backlog queued at connect
  never stays the reference (review round 2). After that, every in-band packet (within one packet)
  outside a hold moves it by 1/1024 of the difference, about a 34 s time constant. An out-of-band
  age leaves it alone, and one that stays out of band for 10 min (`AUDIO_STEP_NOMINAL_REANCHOR_NS`,
  a re-buffered sender) re-anchors it. A hold freezes it. A timeline reset KEEPS it and never starts
  a hold: a sender whose stamps jumped past OBS's 2 s limit stepped first, and the receiver's own
  step brings its age back (review round 2). The known costs:
  - A sender that restarts (a timeline reset) with a DIFFERENT transport lag keeps the old nominal
    until the 10 min re-anchor. In that window an ordinary receiver-first step can hold until the
    10 s bound and then place once. A step of about minus the lag change (within a packet) brings
    the age back onto the stale nominal and is misread as sender-first: the stale stamps are placed
    at once, and a stamp-jumping sender's later follow is booked on top. Review bench, lag +60 ms
    after a restart, a -60 ms step 200 s later: a stamp-jumping sender was 60 ms off for about 62 s
    (one placement, one booking); a catch-up sender 60 ms off for about 3.3 s. A restart with the
    same lag behaves exactly as with none. Accepted for the over-2 s sender-first case above.
  - A wall step inside the first second after a connect is followed by the warm-up.
- **A sender that stepped FIRST is never held** (review round 1). Example: a cross-box source
  whose sender is the date master. Its stamps (a jump, or a caught-up burst or pause) are already on
  the new wall when the receiver steps, so the receiver's step brings their age back to nominal. That
  packet is a zero-length hold, released `followed` with the whole step as its residual, so it is
  placed once. The window before left the audio a step off its stamps, and that placement repairs it.
  Appended instead, a step over OBS's 70 ms smoothing but under the owed cap would be booked and
  paid over about 90 s (under 70 ms the append would have un-booked it). A backward sender step
  over 2 s leaves every packet in the window a step late, so OBS resets its own timeline
  on each; the receiver-step packet is then placed by that reset, with no release line.
- **Release.** The hold ends as `followed` when the held offset (moved by every stamp jump over
  2 ms) is back within one packet of the live one, OR when the stamps' age is back within one packet
  of the nominal age (a sender that CAUGHT UP with continuous stamps). It ends as `timeout` at
  `GENLOCK_AUDIO_STEP_HOLD_MAX_NS` (10 s), and as `reset` on a timeline reset or on leaving
  timecode mode.
- **A release whose residual exceeds one packet PLACES that packet** (`audio_step_release_places`).
  This covers the catch-up, the timeout and the zero-length release: the new offset lands ONCE,
  never as a W-second payment. The placement lands at the packet's RAW-stamp landing
  (`in.timestamp = genlock_intended_ns`), and so does a backstop placement (review round 2). OBS's
  70 ms smoothing may have snapped a sub-70 ms stamp jump onto the continuous timeline, and at the
  smoothed timestamp a sender-first 35-65 ms step landed early and was booked again (up to 63 ms off
  for about a minute).
- **Frozen while holding.** The ASRC is not fed (`asrc_tc_have_prev = false`, so the first packet
  after the hold starts a fresh stamp pair). The render thread leaves the shallow latch and BOTH
  video-delay tracker calls alone through `genlock_audio_step_video_frozen`, which reads the audio
  thread's flag and start (benign aligned reads) and is bounded by the same 10 s on the render
  thread's own clock. So a source whose audio stops inside a hold never freezes its video side for
  longer. The helper tests the flag first, so a source that holds nothing costs no clock read. A
  backlog relock inside the hold is absorbed. A latch lock (ACQUIRE / GAP RESYNC) that lands inside
  it is kept in `genlock_audio_step_relock_pending` and replayed on the first tick after the hold. A
  pin change re-arms the latch window itself.
- **The log:** one `genlock-audio-step-hold '<src>': step_ms= held_ms= released=followed|timeout|reset
  residual_ms= holds= (issue 1381)` line per released hold, NULL-safe on the source name. It is
  mutually non-substring vs every other `genlock-*` family. `held_ms=0.0` with a step-sized
  `residual_ms=` is the sender-first case. A one-packet hold with `residual_ms` near 0 is a clock-read
  glitch: the thread was preempted between the two reads of the live offset for more than a packet.
  The hold kept the pre-glitch offset for that packet, so it is harmless.
- **Known limit, not fixed.** The age test reads ONE packet's age against the smoothed nominal, so
  arrival jitter can release a small step early. With 2-5 ms jitter that reaches steps up to about
  37 ms; with about 20 ms bursts, up to about 55 ms. For a sender that then JUMPS its stamps the cost
  is two events: the early placement, then the jump booked and paid at 1000 ppm, about S seconds for
  S ms. A catch-up sender keeps one event. The bench pins this bound
  (`heavy_arrival_jitter_bounds_a_small_step_to_two_events_1381`).
- **Tier-0 proof:** `tests/genlock_audio_step_hold_parity_1381.rs` drives the verbatim C and the
  Rust authority through one packet script. The script covers the exact 10 s edge (20 ms packets),
  the exact 2 ms jump (1 ms packets), sub-threshold stamp jitter inside a hold, a 4x catch-up burst,
  both sender-first shapes, the nominal warm-up (steps inside it too), the slow track (both signs),
  its timer at now = 0 and the exact 600 s re-anchor. It also checks the bounded render-thread
  predicate.
  `tests/genlock_audio_step_hold_wiring_1381.rs` holds ONE needle list for the ingest, the helpers,
  the render-thread freezes and the relock replay; both `windows-genlock*.yml` pwsh gates require
  the same list. 36/36 C mutants (compensator 10, hold 26) and 20/20 wiring mutants are killed by
  the Rust gate AND the real pwsh lines. A mutant that only removes a write the code path already
  guarantees (the seeded flag at a placement, the out-of-band timer in the warm-up) was an equivalent
  mutant; those writes were removed rather than tested. The shared lift helpers live in
  `tests/genlock_audio_pairing_lift/mod.rs`.
- **Deploy + live acceptance (supervisor).** The change is in libobs (`obs.dll` / `libobs.so.30`)
  plus the shared header, so it ships as a FULL-bundle deploy on every genlock box
  (`rig-state-inspection.md`). At the next nightly dantesync date step, read the cg OBS log on
  resolume:
  - `place_jumps` on the `sp-*` `asrc:` lines rises by at most 2;
  - `restore=0` again within 10 s;
  - one `genlock-audio-step-hold` line per timecode source, `released=followed` normally;
  - `obs-vban pacing:` shows 0 new `discontinuities`;
  - the SongPlayer A/V gate stays green after the step.
- **The compensator half (piece 1 re-seed, piece 3 backstop) and the two-clock bench:**
  `asrc-bench-harness.md`, the issue-1381 section.

## LOCK-indicator audio DEGRADE term — audible-but-expected-silent (#1303 part 3b/c — DONE)

Part 3c (the `audio_unexpected` axis) landed atop part 3b: `GenlockFacets`/`genlock_lock_facets_t`
gained a second bool `audio_unexpected` → `LockReason::AudioUnexpected` (=10), the LOWEST-precedence
DEGRADED branch (below `AudioPairing`), parity-gated in the now-2^9 sweep. The widget flags an
audio-ENABLED source that is silent-by-contract per the certified table — the SHIPPED subset is
box-class-agnostic (an audible CAMERA input via the parity-gated C mirror `genlock_name_is_camera`
of `genlock_forced_table_audit::is_camera_input`), named on the human `genlock-lock:` line
(`audio unexpected: <src>`) and the v3→v4 `genlock-lock-json:` line
(`audio_unexpected_inputs:[{name}]`, omit-when-empty), parsed by `bundle_state_gather`, enriched to
`audio_unexpected:<name>` by `genlock_lock_decision.analyze`. The box-class-DEPENDENT cases (a
non-camera audible on a Dante-fed box; a program source silent on the cg box) are DEFERRED — the
widget has no box identity today — and stay covered at DEPLOY time by the part-4 preflight below.
Full contract: `genlock-lock-indicator.md` + `genlock-lock-facet.md`.

## LOCK-indicator audio DEGRADE term (#1303 part 3b — DONE)

Landed as an additive term in the parity-gated LOCK decision: `GenlockFacets` (Rust
`src/genlock_lock_state.rs` + the C `genlock_lock_facets_t` in `GenlockLockState.hpp`) gained a bool
`audio_unpaired`; `decide` / `genlock_decide_lock_state` gained a lowest-precedence DEGRADED branch
mapping it to a new `LockReason::AudioPairing` (=9); the statusbar widget `OBSBasicStatusBar.cpp`
aggregates the per-source pairing-offset breach (`st.version >= 2 && st.audio_enabled &&
|audio_pairing_offset_ms| > GENLOCK_AUDIO_PAIRING_BOUND_MS`, 33 ms) into that one facet — the twin of
its existing qpc-drift reduction — so an unpaired audio leg turns the indicator DEGRADED (`audio
unpaired: <src>`). The C↔Rust decision stays parity-gated (`tests/genlock_lock_state_parity.rs`, now
a 2^8 flag sweep), lock-stepped by `tests/genlock_lock_indicator_guards.rs` +
`tests/genlock_preload.rs` + the #1298 pwsh gate in BOTH `windows-genlock{,-fast}.yml`. Per the
scope, audio disabled/absent never degrades (the `audio_enabled` guard); `decide_audio_health`'s
AudioDisabledOnProgram + AsrcSaturated branches are NOT surfaced here (they need is-program-source /
asrc-ppm data the v2 stats don't carry) — a followup. Full contract: `genlock-lock-indicator.md`.

## Per-box certified AUDIO table (#1303 part 4 — DONE)

**The audit is NOT a name heuristic — it is a per-box CERTIFIED table.** Owner ruling 2026-09-15
15:10, verbatim: „žiadny — zvuk na strih/stream ide cez Dante, NDI audio ostáva vypnuté (odporúčam,
inak hrozí dvojitý zvuk)". Program audio over NDI exists on the cg OBS (RESOLUME-SNV) ONLY; on
strih/stream/imag the mastered mix arrives over Dante/ASIO (VB-Matrix, `mbc`), so NDI audio on ANY
input there would be DOUBLE audio in the mix.

| box | expected `ndi_audio` |
|---|---|
| **resolume** (cg OBS) | camera inputs → silent; `sp-*`/SongPlayer/`cg`/music program inputs (the 8 keys) → **audio**; any other input → audio (the cg-box default). The ONLY program-audio box. |
| **strih** / **stream** / **imag** | **EVERY** NDI input silent — cameras AND `cg` AND `2ME PGM` alike (the stream music input and its `hudba` key were removed 27.9.2026, issue 1380). Program audio comes from Dante/ASIO, never NDI. |

The classifier `src/genlock_forced_table_audit.rs` (canonical) + the byte-for-byte bash replica
`scripts/lib/genlock-forced-table-audit.sh` encode exactly this; `tests/genlock_forced_table_audit_1303.rs`
pins the two together over a fixed vector set (bash verdict == Rust `audio_verdict` for every
box×name×`ndi_audio`). That verdict parity is BLIND to the program key set (no verdict depends on
it), so `bash_program_key_set_matches_rust_1380` also pins the two program predicates
(`is_program_audio_input` / `is_program_video_input`) over names that each match ONE key only (one per key), so a key on
one side only goes RED there (issue 1380; mutation-proven for `hudba` and `vban`). Verdicts: `OK`, `MISMATCH-PROGRAM-SILENT` (a cg program source with audio off
— the #1295 event-morning defect), `MISMATCH-CAMERA-AUDIBLE` (a camera audible), and
`MISMATCH-AUDIBLE` (a NON-camera input audible on a Dante-fed silent box — the double-audio hazard;
added per the owner's own term). `deploy-genlock-fleet.sh` emits the report-only preflight
(`PREFLIGHT (report-only, #1303 part 4)`) that pipes the box's live `GetInputList`/`GetInputSettings`
TSV into the classifier BEFORE the swap; it NEVER writes and NEVER gates.

**Why a static table, not a live WS read or a data file:** the audit is emitted by the plan builder
as deterministic pre-swap guidance, so it must be pure (no live-box coupling) and self-contained (no
runtime file lookup); the owner handed down a FIXED per-box table, so a static two-replica table
pinned by the parity gate is exactly the right shape. The old heuristic assumed "a program input
carries audio on every box" and produced five false `MISMATCH-PROGRAM-SILENT` rows on strih/stream
— that assumption is now dead.

### Gotcha — narrowing the audit's classification can silently disable an advisory gated on it

`classify` derives TWO things: the audio verdict AND the report-only `yuv_range=partial` advisory.
The advisory was originally gated on `expected == ExpectedAudio`. When the certified-table change
flipped every strih/stream/imag input to `ExpectedSilent`, that advisory silently STOPPED firing
for those boxes' program VIDEO inputs (`cg`, `NDI 2ME PGM`) — a coverage loss a self-review missed
and a fresh-context review caught. The advisory is a VIDEO concern; keying it on the AUDIO
expectation coupled it to a table that legitimately changed. It is now decoupled via
`is_program_video_input(box, name)` (`!camera && (program_keyed || box==resolume)`), computed
independently of `expected_audio`. **General rule: when you NARROW a classification (more inputs
land in a "silent"/"off"/"excluded" bucket), audit every report-only NOTE/advisory/secondary
signal gated on the OLD classification — a `matches!(expected, …)`-style gate can silently go dark.**

## Deferred followups (NOT in the #1303 code lane)

- **Box-class-aware LIVE audio-mismatch** — surface the box-class-DEPENDENT cert-table cases in
  the LOCK indicator LIVE: a NON-camera input audible on a Dante-fed box (strih/stream/imag) and a
  program source SILENT on the cg box (resolume). Part 3c wired only the box-class-AGNOSTIC
  audible-camera subset (the widget has no box identity); a robust version needs a deploy-written
  box-role marker (read like `GENLOCK_BUILD_SHA.txt`). Already covered at DEPLOY time by the part-4
  preflight, so the live version is defense-in-depth.
- **Audio DEGRADE full taxonomy** — surface `decide_audio_health`'s AudioDisabledOnProgram +
  AsrcSaturated branches in the LOCK indicator; needs the widget to know is-program-source +
  per-source asrc-saturation, neither in `obs_genlock_stats` v2.
- Live A/V soak acceptance (±20 ms over 1 h, cg OBS `locked=1` + audio facet green) is a
  post-merge SUPERVISOR rig step — never rig-verified from the code lane.
