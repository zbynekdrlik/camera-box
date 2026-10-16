---
paths:
  - "vendor/obs-studio/libobs/obs-genlock-audio-buffering.h"
  - "vendor/obs-studio/libobs/obs-audio.c"
  - "vendor/obs-studio/libobs/obs.c"
  - "src/genlock_audio_buffering.rs"
  - "tests/genlock_audio_buffering_parity_1367.rs"
  - "tests/genlock_audio_buffering_wiring_1367.rs"
  - "tests/genlock_audio_mix_guard_1381.rs"
  - "tests/c/genlock_audio_mix_guard_1381_harness.c"
  - "vendor/obs-studio/libobs/obs-genlock-mix-guard.h"
  - "vendor/obs-studio/libobs/obs-output.c"
  - "tests/genlock_audio_buffering_freeze_wiring_1425.rs"
  - "tests/genlock_audio_slice3_1425.rs"
  - "scripts/obs-guarded-launch.ps1"
  - "scripts/rig-health-audit.py"
  - "scripts/launch-obs-genlock.sh"
---

# The genlock audio buffering: no floor (issue 1425 slice 3), the 1381 guard, the 1425 freeze

## The floor is 0 (issue 1425 E2 slice 3, design 6220191978)

- **What ships.** `GENLOCK_AUDIO_BUFFERING_FLOOR_MS 0u` (`obs-genlock-audio-buffering.h`) ↔ Rust
  `FLOOR_MS = 0` (`src/genlock_audio_buffering.rs`). A launch starts at 0 ms like stock OBS, and its
  own startup race grows the buffering in the first seconds: a clean launch draws what stock OBS drew,
  **0, 64 or 85 ms** on stream (the history below; the lift's 21 / 42 ms launch is a model, not a
  measurement), or logs no growth line. The FLOOR action, `set_floor_audio_buffering` and the plan's
  floor field stay as the design's mechanism (`raise_audio_buffering` is still the fixed mode's body);
  at 0 they never run, and `plan_with_floor` keeps the Rust plan's floor rules tested. Deleting the
  path is a design call (reported in the slice-3 lane return).
- **Why it could go.** The 85 ms floor (issue 1367, ROZHODNUTÉ 5857354949) existed only so the stream
  `mbc` could reach the #1355 absolute ASRC level target (100 ms + sync offset) inside half of OBS's
  70 ms TS-smoothing reach. Since slice 1 a directly stamped source is judged by its own stamp
  (setpoint 0) and its A/V does not depend on the buffering; since slice 2 the buffering cannot grow
  while an audio encoder runs; slice 3 deleted the absolute target and its UNREACHABLE fallback
  (`asrc-bench-harness.md`, the issue-1425 slice-3 section). The ASRC level band
  (`band_error_ns` / `band_ok`, `LEVEL_REACH_NS`, the `ASRC level band ok|BROKEN` note) went with it.
- **The log.**
  - Reset block: `buffering type:  dynamically increasing from 0 ms (genlock floor, issue 1425)`; a
    fixed / low-latency request is still `OVERRIDDEN` (one WARNING, dynamic from 0).
  - Every growth: ONE `LOG_WARNING` `genlock audio buffering ABOVE the floor (issue 1367): adding N
    milliseconds of audio buffering, total audio buffering is now M milliseconds (source: <name>)`.
    The source ends the line now (the band note is gone); the gather's parser takes both shapes.
  - No floor line at the first tick any more. The audio-mixer watchdog's BUFFERING arm takes its
    session start from the reset line instead (`audio-mixer-watchdog.md`).
- **The #786 launch gates** (`obs-guarded-launch.ps1`, `launch-obs-genlock.sh` (3b),
  `rig-health-audit.py`) still parse `total audio buffering is now (\d+) milliseconds` against their
  100 ms bound and still redraw above it. Only the wording moved: the norm is "under the 100 ms
  bound" (the guarded launcher's popup: `norma pod 100 ms`), and a high draw is added LATENCY on every
  output, never "A/V MIMO" (a stamped source's A/V does not depend on it).
- **The known limit (pinned: the 1381 lift's `encoder-at-launch`).** With no floor, an output that
  starts BEFORE the launch's startup race (an audio encoder from the first tick) freezes the mix at
  0 ms, so every member that reaches the mixer even 1.3 ms behind real time is re-anchored on every
  tick and muted until the output stops. **Something CAN start one at launch:** stream-snv has an
  `OBSStream` scheduled task that runs a bare `obs64.exe --startstreaming` (`av-restart-matrix.md`,
  read 28.9.2026; its trigger was not checked), and a Companion button or the frontend's auto-start
  options can do the same. Whether the frontend's `--startstreaming` (after the scene collection
  loads) lands before or after the sources' first late packets is NOT measured. The 85 ms floor
  absorbed a member up to 85 ms late here, so this path can go from working to muted; reported as the
  open question of the slice-3 lane return (read the task's trigger read-only, or arm the freeze only
  after the launch's startup race). The same holds for a direct-stamp member that joins later while an
  encoder runs and is later than the launch drew (the slice-2b limit, now without the 85 ms margin).
  The 1381 lift's E1 replays start their encoder at tick 100 (after the race) for this reason.
- **Other boxes.** strih-snv, stream-snv and the resolume cg OBS all start at 0: the mix runs only
  as far behind real time as their own sources need, so wall-clock-stamped outputs (the DistroAV NDI
  output audio, obs-vban) are 85 ms earlier than with the floor.
- **History (1367).** Stock OBS drew 0 or 85 ms per launch (27.9.2026: `01-33-25` 85 ms, `mbc` held
  its absolute target; `12-18-15` none, `mbc` 91 ms under it, the fallback re-latched at 54.8 / 73.5
  ms, release A/V -28 ms). The floor fixed that draw for the absolute target; slices 1-3 removed the
  target, so the draw no longer matters for A/V.

## Tests and Tier-0

- `src/genlock_audio_buffering.rs`: the rounding, the plan and override, the floor rule (on a 4-tick
  floor and on the shipped 0), the freeze; `the_floor_is_zero_ticks_at_both_rates_1425`.
- `tests/genlock_audio_buffering_parity_1367.rs` compiles the header as-is against the Rust over
  vector spreads, lifts every define (the floor must read 0) and holds a launch's own draw (the floor
  plus one or two ticks) under all three #786 launch-gate bounds.
- `tests/genlock_audio_buffering_wiring_1367.rs` (std-only): the wiring anchors (ONE `WIRING` list,
  also required by the pwsh step "Assert the genlock audio-buffering floor is present" in both
  `windows-genlock*.yml`) and a C LIFT of the shipped reset block, tick decision and buffering
  functions: a launch with its startup race (21 / 42 ms), a late source (149 ms), the low-latency
  toggle, 44.1 kHz, a maximum, and a 4-tick floor set by hand (`max(floor, stock)`). The lift models
  the window `audio_callback` really processes (the front of `buffered_timestamps`, `start − total ×
  tick`) and runs ONCE per test binary (`OnceLock`).
- `tests/genlock_audio_slice3_1425.rs` (std-only): the floor define is 0 and the absolute target, its
  fallback and the band are gone from the C and the Rust mirror; the launch-gate wording.
- Local: the module via a plain-rustc replica + `clippy-driver --test -D warnings`; the parity gate
  via a stub `camera_box` rlib; the wiring and slice-3 files with plain `rustc --test` and
  `CARGO_MANIFEST_DIR`; the C files with the `obs-drm-output.md` `-fsyntax-only` recipe.

## Live acceptance (supervisor, FULL bundle on every OBS box)

After each of at least two OBS restarts per box: the reset block reads `dynamically increasing from
0 ms`, a launch's `ABOVE the floor` lines total under the 100 ms bound (stock drew 0, 64 or 85 ms
on stream), the #786 launcher reads it clean, a stream started by `--startstreaming` (or a recording
started within the launch's first seconds) still carries `mbc` and the mic, and on stream `#800 buffered_ms` and the per-camera A/V are identical within ±5 ms
across two restarts whose draws differ. The `asrc:` lines carry no `fallbacks=` field; an arrival
source's `target=` is its depth at lock.

## The mix buffering GUARD: only a MIXED source can grow it (issue 1381, design 5862336131)

**Why.** Stock libobs let EVERY audio source move the mix window: `find_min_ts` /
`mark_invalid_sources` walked `data->first_audio_source`, and `audio_callback`'s catch-all loop
renders every audio source whether any output mix reaches it or not. 27.9.2026 on resolume the
hidden "NDI test" took the cg mix +85/+42/+106/+128/+106/+490 ms to the 960 ms maximum between
09:11 and 09:23, one hole in every output per step (the added ticks drain with `audio_callback`
returning false). The floor above does not stop growth above it; this guard stops a source nobody
hears from causing it.

**How OBS handled a newly activated source's timeline (read in the code, not assumed).**
- Activation never touches the audio timeline: `obs_source_activate` only bumps `activate_refs`
  (obs-source.c), the next video tick calls the `activate` callback.
- A source's audio is placed whether it is active or not (`obs_source_output_audio` ->
  `source_output_audio_data` -> `source_output_audio_place`, no activation gate).
- So there was no "entry" event: a hidden late source's lateness was already in the global
  buffering by the time it was cut in.
- The only per-source re-anchor upstream has is `ignore_audio`, and only at the MAXIMUM: drop the
  samples behind the window; with none left set `audio_pending`, `audio_ts = 0`,
  `timing_set = false`. The next packet then maps the source's stamp to its arrival
  (`reset_audio_timing`) -- but only when its placement FOLLOWS `timing_adjust`:
  - a stamp within `MAX_TS_VAR` (2 s) of the OBS clock is "direct" (ASIO/WASAPI capture, media):
    `timing_adjust` is forced to 0, the source keeps its own time;
  - a genlock source in the TIMECODE hold is placed at its timecode + the measured video delay
    through the live wall-vs-mono offset, a term that CANCELS `timing_adjust`
    (`genlock_audio_place_term_ns`), so the restart does not move it either;
  - an NDI source with genlock off, or on the latency hold, follows `timing_adjust` (NDI stamps
    are 100 ns epoch timecodes, never direct): the restart puts it at its arrival.
- A hidden late source's buffer is never discarded (`discard_audio` returns at "can't discard"), so
  under a filter alone it would pile up to `MAX_BUF_SIZE` (~21 s) and push the mix to the maximum
  at the cut. The non-mixed path therefore keeps the source at the window every tick.
- Composites (scenes, transitions) are never in `first_audio_source`: `is_audio_source` is
  `OBS_SOURCE_AUDIO` only and obs-module.c refuses a composite audio source. A scene reaches the
  render order only through a view's active tree, i.e. as a member.
- The frontend starts the audio thread (`ResetAudio`) before it loads the scene collection, so the
  program sources JOIN the mix on some later tick -- a launch is a series of entries.

**What ships.**

| Piece | Where |
|---|---|
| the pure decisions: membership (`genlock_mix_is_member`), the entry rule (`genlock_mix_joined`: a never-marked source joins on its first membership, a marked one when it was not a member on the previous tick), the reason (`genlock_mix_guard_reason`: NOT_MIXED / ENTERED / NONE), what stock OBS would have added (`genlock_mix_guard_would_add_ms`: add_audio_buffering's rounding + clamp), the log cadence (`genlock_mix_guard_log_due`) | `obs-genlock-mix-guard.h` (stdint/stdbool only) |
| the mixer tick: `genlock_mix_tick_now`, a FILE-SCOPE static in obs-audio.c, never reset (obs_free_audio zeroes `struct obs_core_audio` on an audio reset while the sources keep their last tick; a counter there brought a stale member back) | obs-audio.c |
| the mark: `genlock_mix_mark_members(audio)` right after the output mixes' active trees are in the render order, BEFORE the catch-all loop; stamps `obs_source.genlock_mix_tick` + `genlock_mix_entered` | obs-audio.c, obs-internal.h |
| min_ts: `find_min_ts` and `mark_invalid_sources` count members only (`genlock_mix_source_is_member`) | obs-audio.c |
| the re-anchor: `genlock_mix_guard_reanchor`, under `audio_buf_mutex` in the render loop before upstream's maxed block (then `continue`) | obs-audio.c |

- **Membership = any output mix's active tree** (every `obs->video.mixes` view, the design's words);
  a source active in a non-audio canvas stays upstream (conservative). Every root node, including
  `push_audio_tree2`'s duplicates, is a member.
- **Not mixed + behind the window** (by more than discard_audio's 1 ns rounding): re-anchored every
  tick, at any buffering level. It never reaches min_ts even when the ingest thread re-places it
  late between the render loop and `calc_min_ts` (the find_min_ts filter's real job).
- **Entered the mix this tick + behind**: re-anchored once instead of growing the buffering -- a
  cut to it, or a program source joining after the scene collection loads.
- **Mixed and not entering**: upstream, byte-identical: the dynamic increase above the floor,
  `ignore_audio` at the maximum. That includes a source whose audio STARTS only after it joined the
  mix (a media start on program, a receiver that connects on show): no timeline at the entry tick,
  so it is a mixed source starting late.
- **Composites** are never re-anchored themselves (their timestamp is their children's).
- **The re-anchor MIRRORS `ignore_audio`** (drop `ceil(behind)` samples with the `- 1 ... + 1`
  rounding, the `audio_ts == start - 1` adjust, nothing left = restart). `ignore_audio` itself stays
  byte-identical for rebases; the lift test compares the two on every probe, so a rebase that
  changes `ignore_audio` fails until the copy follows. It re-checks the timeline under the lock (the
  ingest thread may have moved or reset it since the unlocked decision) and re-renders a source
  that is back in sync, so the mix never uses an output buffer peeked from the dropped front.
- **The line**: `buffering-guard: '<src>' is not mixed|entered the mix late: its audio ran X ms
  behind the mix window; re-anchored (dropped Y ms[, timeline restarted]) instead of adding Z ms to
  the whole mix's T ms of audio buffering (issue 1381; events=N, +K since the last line,
  dropped_total=D ms)` -- LOG_WARNING; every entry and a source's first event log at once, later
  not-mixed events at most once a MINUTE per source (the #800 cadence; a hidden source that keeps a
  late stamp is re-anchored on nearly every tick). It never carries the #786 `is now` text, so no
  launch gate reads it as a buffering draw; the marker is not a substring of `audio-stall #1367:` /
  `audio-telemetry #800` / `genlock audio buffering`. Both 60 s bounds are spelled
  `(60ULL * 1000000000ULL)`: `tests/audio_telemetry_800.rs` pins the #800 rate limit by the literal
  `60000000000ULL`, which obs-audio.c must keep exactly once.

**Known limit (reported to the supervisor, not a different shape).** The entry re-anchor's timing
restart moves only a source whose placement follows `timing_adjust`. A source that KEEPS its stamp
-- a direct stamp, or a genlock TIMECODE hold -- and stays late after the entry drop is placed late
again by its next packet; a tick or two later (the restarted source is pending for a tick while it
refills) it is a late MIXED source and upstream grows the mix (one hole). The hidden path covers
every kind (the 27.9 incident class). Removing the limit needs a persistent per-source placement
offset in `source_output_audio_data` (it moves that source's audio off its own stamps) or a fix of
whatever made a genlock source's audio late against its measured video delay -- a separate design
call. `cut-in-kept-stamp-limit` pins the limit so a change to it is visible. A launch is the same
shape: a direct source late when it joins (the stream ASIO startup race) is re-anchored once and
then reaches the same upstream increase as before (`launch-late`: 128 ms).

**Tests and Tier-0** (`tests/genlock_audio_mix_guard_1381.rs`, std-only):
- Wiring anchors (8, `WIRING`: obs-audio.c, obs-internal.h, obs-genlock-mix-guard.h) + the SAME
  list in a pwsh step of both `windows-genlock*.yml`.
- A C LIFT of the shipped `audio_callback` path, verbatim: the render-order tail (mark + catch-all
  loop), the render loop, `calc_min_ts`, the 1367 tick decision, mix + discard, and every function
  they call (`push_audio_tree`, `convert_time_to_frames`, `ignore_audio` .. `calc_min_ts` incl. the
  guard, `audio_frames_to_ns` / `ns_to_audio_frames` from audio-io.h), each lift anchor unique. They
  are substituted into `tests/c/genlock_audio_mix_guard_1381_harness.c` (six at-sign markers, each
  checked to occur once), which `#include`s the header: a stub libobs with a real timestamp deque,
  byte-only input buffers, a render stub with process_audio_source_tick's pending rule, a mix stub
  with mix_audio's window test plus a stale-output check, and a model of source_output_audio_place
  + the timing restart (`h_keeps_stamp` for the class the restart cannot move).
- Scenarios, the printed trace is the truth table: hidden NDI jumping later 4x a minute apart (the
  last past the maximum, so would-add is clamped), a hidden kept-stamp source late for good (a line
  a minute), the ingest-thread race, an audio reset (a stale tick colliding with a backlog), a
  mixed source going late (upstream), a cut with a 300 ms receiver backlog, a cut with a timeline
  jump, a program scene cut in, the kept-stamp limit, a launch in the frontend's order; plus direct
  probes at the exact-sample edges, the locked re-check, the `ignore_audio` parity and the entry
  rule.
- RED against the pre-fix mixer (the first RED commit): the hidden NDI source took the mix to
  960 ms (41 hole ticks), the cuts cut 10-12 hole ticks; the upstream cases already held. The
  review-round RED against the first GREEN: the audio-reset collision grew the mix by 213 ms.
- Mutation proof (27 C mutants of the header + obs-audio.c, scratch tree, the test compiled once
  with `CARGO_MANIFEST_DIR=<scratch>` because the files are read at run time): 25 fail by behaviour;
  the mark_invalid_sources filter fails only the wiring anchor (behaviourally equivalent: it only
  ever sets `pending` on a source that is never mixed); the `continue` after a re-anchor is
  equivalent by construction (the timeline is then 0 or at/after the window start, and upstream's
  maxed branch needs a non-zero stamp before it).
- Local: `CARGO_MANIFEST_DIR=<wt> rustc --edition 2021 --test tests/genlock_audio_mix_guard_1381.rs`
  + `clippy-driver ... -D warnings`; the real obs-audio.c type-checks with
  `gcc -fsyntax-only -Wall -Wextra -Wformat=2 -Werror` against the real libobs headers plus a
  scratch `obsconfig.h` (the obs-drm-output.md recipe, `-Ivendor/obs-studio/deps/libcaption`); the
  header through a one-line TU under `-Wconversion -Wsign-conversion`.
- The 1367 floor lift (`tests/genlock_audio_buffering_wiring_1367.rs`) lifts `audio_buffering_maxed`
  .. `audio_buffer_insufficient`: keep the guard's obs-audio.c block AFTER `audio_buffer_insufficient`
  or that lift has to stub it.

**Live acceptance (supervisor, FULL-bundle deploy -- obs.dll/libobs).** On resolume with a hidden
late NDI source (a test NDI input in a scene that is not on program): 0 `ABOVE the floor` /
`adding N milliseconds` lines from it, a `buffering-guard: '<name>' is not mixed` line instead, the
#800 `total_buffering=` stays at the launch's own draw, the `audio-stall #1367` `ticks` stay at ~2812/min. A cut
to it: one `entered the mix late` line; for an NDI source with genlock off or on the latency hold no
ABOVE line and no obs-vban underflow / discontinuity at the cut; a genlock TIMECODE-hold source that
stays late may still log one ABOVE line a tick or two later (the known limit). A media start on
program still logs its ABOVE line (upstream).

## The FREEZE: no growth while an audio encoder runs (issue 1425, E2 slice 2, design 6216736061 + ROZHODNUTÉ 6217557472)

**Why.** A buffering increase while an encoder runs makes that encoder's audio permanently later:
after the output start, `obs-encoder.c` counts audio pts in samples (`cur_pts += framesize`), and
the added ticks drain with `audio_callback` returning false. On stream-snv, while it streamed, the
E1 events grew the mix +128 (15.10 04:00, an ASIO outage on `mbc`), +192 (11.10 07:09, a longer one)
and +85 ms (15.10 17:43, `pre-fotky video` after a 432 ms mixer callback stall), each an
audio-after-picture step on every live output until an OBS restart.

**Only a running audio encoder freezes it (ROZHODNUTÉ 6217557472).** The first cut also froze a mix
that had gone 30 s without a growth or a member join. Its own scenario showed that this mutes, on a
box with no audio encoder (the resolume cg OBS feeding FOH), a clip started in place whose stamps
stay late, where stock OBS grows the mix once and plays it. With no encoder a growth moves no live
output's A/V, and slice 1 already makes the stamp-judged sources' A/V independent of the buffering,
so that arm was dropped: a box with no audio encoder keeps stock growth exactly.

**What ships.**

| Piece | C | Rust / test |
|---|---|---|
| the decision: frozen while an output with an audio encoder runs, and only then | `genlock_audio_buffering_frozen(encoder_active)` | `frozen` |
| the action: `frozen` turns DYNAMIC into NONE; the FLOOR is never frozen | `genlock_audio_buffering_action(..., source_behind, frozen)` | `action` |
| a member behind the window while frozen is re-anchored | `GENLOCK_MIX_GUARD_MIXED_LATE` (3) from `genlock_mix_guard_reason(..., frozen)` -> `genlock_mix_guard_reanchor` (the 1381 path: drop the behind samples, restart the timeline when nothing is left) | the 1381 lift (no Rust authority: the guard's reasons are pinned by its truth table) |
| the encoder count | `obs-output.c`: `os_atomic_inc_long` in `hook_data_capture` and `os_atomic_dec_long` in `end_data_capture_thread`, both in the ENCODED branch; read by `obs_output_genlock_audio_encoder_outputs()` | `tests/genlock_audio_buffering_freeze_wiring_1425.rs` |

- **Per tick, in `audio_callback`:** `genlock_mix_mark_members` -> the freeze decision, read once into
  `audio->genlock_buffering_frozen` -> the render loop (the reason gets it) -> `calc_min_ts` -> the
  tick decision (the action gets it). A stream or recording that starts or stops takes effect on the
  next tick (`encoder-on-off`).
- **Only audio ENCODERS count.** A raw-audio output (the DistroAV NDI main output, obs-vban) never
  freezes the mix, so the cg OBS on resolume grows on a late media / `NDI test` start as before.
- **No step-down.** A mid-run decrease would skip a tick of every output and move every running
  encoder's audio a tick earlier. Nothing grows while an encoder runs, so nothing steps down; a
  growth taken while no encoder ran stays until the next launch.
- **The log.**
  - `buffering-guard: '<src>' fell behind the mix (frozen at T ms): its audio ran X ms behind the
    mix window; re-anchored (dropped Y ms[, timeline restarted]) instead of adding Z ms to the whole
    mix's T ms ...` -- the same LOG_WARNING family and cadence as 1381 (a source's first event at
    once, later at most once a minute; `+K since the last line` counts the rest).
  - `audio-telemetry #800 mix: frozen=0|1 mixed_late=N` -- a NEW line right after the #800 summary
    (the summary line is parsed to end of line by `bundle_state_audio.py` and formatted with four
    arguments by its pytest, so it stays byte-identical); the per-source #800 line ends with
    `mixed_late=N`.
  - Per source `genlock_mixed_late_events` / `genlock_mixed_late_dropped_ns`; a MIXED_LATE also
    counts in the guard's `events=` / `dropped_total=`.
- **Known limit (slice 2b, an owner product call).** While an encoder runs, a member that KEEPS its
  stamp and stays late for good (a device whose buffer really grew, a media clip whose stamps run
  late) is re-anchored on every tick: its audio is dropped, the mix never grows (pinned by
  `late-start-kept-stamp-encoder`: a clip 128 ms late started in place is never mixed; and the frozen
  span of `encoder-on-off`: 700 events, one line). Delaying that one source instead is slice 2b,
  built only if a `mixed_late` storm is seen live (the owner decides delay vs drop). Nothing pages on
  it yet: the audio-mixer watchdog does not read `mixed_late`, and the per-source meters read the
  source before the mix. Paging on a growing `#800 mix: mixed_late=` was reported to the supervisor
  as a follow-up candidate in the slice-2 lane return.
- **Log cadence.** The guard's "first event logs at once" flag is shared across reasons: a source
  that logged a not-mixed or entered line in the last minute logs its first MIXED_LATE only with the
  next minute's line (`+K since the last line` counts it). The counters and `#800` see it at once.

**Tests and Tier-0.**
- `src/genlock_audio_buffering.rs` unit tests + the 1367 parity gate (the action's `frozen`, the
  decision over its one input).
- The 1381 lift (`tests/genlock_audio_mix_guard_1381.rs`; the harness stub has an encoder count
  `h_encoders`):
  - the three E1 replays with the stream's audio encoder running (a member that keeps its stamp
    delivers a backlog stamped at capture; stock libobs reproduces exactly the logged +128 / +192 /
    +85 ms, the first RED commit); GREEN: the mix stays at the launch's draw, the mic heard on every
    tick after it, one counted dropout of the late span. Since issue 1425 slice 3 (no floor) the
    stream starts 2 s after launch (`H_STREAM_TICK`), every scenario starts with the launch draw
    `waits=2 total_ms=42 mic_missed=2` (the mic's startup race, before any output), and the dropouts
    read 166.0 / 226.0 / 116.0 ms (measured against a 42 ms draw, not an 85 ms floor);
  - `e1-asio-outage-128-no-encoder`: the same outage with no encoder grows the mix to 213 ms as stock;
  - `encoder-at-launch` (issue 1425 slice 3): an encoder from the first tick freezes the mix at 0 and
    mutes the late members -- the known limit of no floor, pinned;
  - `encoder-on-off`: unfrozen growth, a frozen span (MIXED_LATE on every tick), unfrozen growth
    again when the stream stops;
  - `late-start-kept-stamp` (no encoder: the mix grows to 149 ms and the clip plays) and its twin
    `late-start-kept-stamp-encoder` (muted while late, the 2b limit);
  - `frozen-ingest-race` (the action's frozen term) and the probes `reason-1425` (the 32-row truth
    table over member / entered / composite / behind / frozen) and `mixed-late` (the counters).
- `tests/genlock_audio_buffering_freeze_wiring_1425.rs` (std-only): ONE `FREEZE_WIRING` /
  `FREEZE_ABSENT` list (the absent half refuses the retired settle: `genlock_settle_tick`, the
  settle define, the settle clock), required by the pwsh step "Assert the audio buffering freeze is
  present (issue 1425)" in both `windows-genlock*.yml`; the per-tick order; the count's two capture
  sites in the encoded branch.
- Mutation (scratch tree, the tests compiled once against it, both pwsh steps run for real): see the
  lane returns on the ticket (round 1: 36/38 killed, 2 equivalent; round 2: the re-run on the
  encoder-only code).
- **Live acceptance (supervisor):** FULL bundle on every OBS box; `#800 mix: frozen=1` while a
  stream / recording runs and `frozen=0` otherwise; `total_buffering=` flat while frozen; a forced
  member outage while streaming locally logs one `fell behind the mix (frozen at …)` line and no
  `total audio buffering is now` growth, and `mixed_late=` rises by that one event and then stays
  flat (a rising count = a storm, the 2b limit); on resolume (no encoder) a late media / `NDI test`
  start still logs its ABOVE line and FOH hears it; the release E2E A/V gate green.
- **Size.** `audio_callback` is about 265 lines and `obs-audio.c` about 1105 (already over 1000
  upstream): the next audio-thread addition first moves the #800 telemetry block into a helper, as
  `audio_stall_dump` already was.
