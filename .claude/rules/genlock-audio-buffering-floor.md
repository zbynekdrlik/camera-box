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
  - "scripts/obs-guarded-launch.ps1"
  - "scripts/rig-health-audit.py"
  - "scripts/launch-obs-genlock.sh"
---

# The genlock audio-buffering FLOOR (issue 1367, ROZHODNUTÉ 5857354949)

## Why

Stock libobs starts the mix window with 0 audio buffering. It grows the buffering only when a
source happens to arrive late at startup (`add_audio_buffering`, the dynamic increase), so every
launch drew its own value. On the stream box that draw decided whether the `mbc` ASRC could reach
its #1355 absolute level target (100 ms + the sync offset) at all. The two stream OBS logs of
27.9.2026:

| Session | Buffering | `mbc` result |
|---|---|---|
| `01-33-25` | `adding 85 milliseconds … (source: ASIO Input Capture)` | Target held for 10 h, through the trims 41 → 26 → 23 → 18 ms. `level_avg` 118.1 at target 118, 0 fallbacks. |
| `12-18-15` | none | Level 27 ms against 118. The restore pushed −136 … −141 ppm for 40 min; the level sawtoothed 27 → 78 → 36 → 86 ms. Two `UNREACHABLE … fell back` lines re-latched the target at 54.8 and 73.5 ms. The release E2E then read −28 ms on every camera. |

## The physics the constant is sized by

- **Natural depth.** The depth a direct-timestamp source (`mbc`, ASIO) settles on without a servo
  correction is `buffering + base + sync_offset`.
  - `base` is its arrival latency against the mix window plus the mean of the 21.33 ms tick
    sawtooth.
  - It measured 8.4 ms (`01-33-25`, first `level_avg` 134.74 at offset 41) and 8.9 ms
    (`12-18-15`, 26.94 at offset 18), with the buffering 85 ms apart.
- **The sync offset CANCELS.** The #1355 target is `100 + last_sync_offset`, and the placement adds
  the same offset to the depth (`in.timestamp += sync_offset`), so the servo must bridge
  `100 − (buffering + base)`, whatever the #1333 split / #856 trim writes.
  - The first design said "100 + the largest sync offset + headroom ≤ buffering". With the code's
    own ±500 ms clamp (`AUDIO_OFFSET_CLAMP_MS`) that asked for ~600 ms of buffering and a target out
    of reach from ABOVE (Design-question 5857343807).
- **Reach is limited in BOTH directions.** Stretching or compressing moves the source's smoothed
  timeline off its raw stamps; at `TS_SMOOTHING_THRESHOLD` (70 ms, a symmetric `uint64_diff` in
  `source_output_audio_data`) the next packet is re-placed at its raw stamp and the correction is
  gone. Live, the `12-18-15` restore got the level at most +51 … +59 ms above natural before the
  snap.
- **The band:** `|100 − (buffering + base)| ≤ 35 ms` (half the threshold), for a base of 0–25 ms,
  at 44.1 and 48 kHz.
  - `FLOOR_MS` 85 = 4 ticks = 85.33 ms at 48 kHz (92.88 ms at 44.1 kHz): the servo stretches ~6 ms
    at the measured base. This is the live-proven `01-33-25` configuration.
  - 0, 3 ticks (64 ms) and 6 ticks (128 ms) all leave the band. 5 ticks (106.67 ms) would still
    hold it.
  - The ±35 ms band is a chosen margin; the observed physical reach is about 51–59 ms.

## What ships

| Piece | C | Rust / test |
|---|---|---|
| the plan (floor, max, override) | `genlock_audio_buffering_make_plan` (→ `struct genlock_audio_buffering_plan`) in `obs-genlock-audio-buffering.h`, called by `obs_reset_audio2` | `plan` in `src/genlock_audio_buffering.rs` |
| the tick rule (floor first, then OBS's dynamic increase) | `genlock_audio_buffering_action` (`GENLOCK_AUDIO_BUFFERING_ACTION_*`) → `set_floor_audio_buffering` / `add_audio_buffering` in `audio_callback` | `action` |
| the band | `genlock_audio_buffering_band_error_ns` / `_band_ok` | `band_error_ns` / `band_ok` / `floor_holds_band` |
| raise-to-N-ticks body | `raise_audio_buffering` (upstream's fixed-mode body, shared by fixed and floor) | the C lift in `tests/genlock_audio_buffering_wiring_1367.rs` |

- **A FLOOR, not a cap.** The resolume cg OBS legitimately grows 128–362 ms on media / `NDI test`
  starts (logs `2026-09-26 09-29-12`, `14-50-56`, `16-35-04`); a hard 85 ms cap would drop that
  late audio on FOH/VBAN. The maximum stays the caller's (45 ticks by default). Since issue 1381
  only a MIXED source grows it: a hidden one is re-anchored instead (the guard section below).
- **Fixed buffering is never used.** The frontend LowLatencyAudioBuffering toggle (fixed 20 ms) is
  overridden: one `genlock audio buffering (issue 1367): … OVERRIDDEN` WARNING at reset, max back to
  45 ticks. The upstream fixed branch in `audio_callback` stays byte-identical for rebases
  (`set_fixed_audio_buffering` is now a thin wrapper over the shared `raise_audio_buffering`); the
  plan never sets `fixed_buffer`, so that branch is unreachable.
- **The log:**
  - `buffering type:  fixed floor 85 ms, dynamically increasing above` in the reset block;
  - `genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds …`
    at the first tick;
  - every later increase is ONE `LOG_WARNING` line, `genlock audio buffering ABOVE the floor
    (issue 1367): adding N … total audio buffering is now M milliseconds (source: <name>); ASRC
    level band ok|BROKEN: …`. BROKEN means buffering + the 9 ms nominal base left the band (M above
    126 ms): a mixed source on an absolute ASRC level target (the stream `mbc`) cannot reach it at
    this buffering. libobs has no box identity, so the note prints on every box; on resolume it only
    matters for a mixed non-genlock source on an absolute target there.
- **The #786 launch gates keep working.** `obs-guarded-launch.ps1`, `launch-obs-genlock.sh` (3b)
  and `rig-health-audit.py` parse `total audio buffering is now (\d+) milliseconds` against a 100 ms
  bound.
  - Both new lines keep that text.
  - The floor alone reads 85 / 92 ms, a clean draw.
  - A late-source increase above 100 ms is still a BAD draw, as before (the 960 ms ASIO ratchet of
    #786 is unchanged: the dynamic increase above the floor is stock OBS).
  - A clean launch now logs exactly the floor line (85 ms at 48 kHz); the scripts' comments, the
    guarded launcher's popup (`norma 85 ms`) and the obs-ops skill say so since issue 1367.
- **The #1355 UNREACHABLE fallback is only a logged safety net now — on the stream box.** Its line
  also names `total_audio_buffering=` and `floor=` at that moment: above the floor, a late source
  raised the buffering (its own ABOVE line says which); at the floor, the cause is not the buffering.
  On resolume a media / `NDI test` start on program still grows the buffering past the band
  (128–362 ms), so a mixed, non-genlock, non-monitor-only source there (the ASRC is on by default and its target is
  absolute for such a source) can still fall back — reported to the supervisor as a follow-up
  candidate, not this slice.
- **Other boxes.** The floor applies to every genlock OBS (same libobs). The mix output lags real
  time by the buffering, and wall-clock-stamped audio outputs follow it: on resolume the DistroAV
  NDI output audio and the obs-vban send to FOH run ≥ 85 ms behind from the first tick, where they
  used to run at 0 until the first media start grew it to 128–362 ms. No rig input consumes
  resolume's NDI audio (the certified table: every strih/stream NDI input is silent), so the
  visible effect is the FOH VBAN latency while idle. strih-lx carries no mixed program audio over
  NDI either; its mix now starts 85 ms behind.

## Tests and Tier-0

- `src/genlock_audio_buffering.rs`, the authority: the rounding, the plan and override, the tick
  rule, the band (4/5 ticks hold, 0/3/6 fail), the offset cancellation over ±500 ms, and the two
  measured sessions.
- `tests/genlock_audio_buffering_parity_1367.rs` compiles the header as-is against the Rust over
  vector spreads. It then holds the band on LIFTED values:
  - `ASRC_LEVEL_TARGET_MS` (and equal to `asrc_bench::LEVEL_TARGET_MS`);
  - `TS_SMOOTHING_THRESHOLD`, `AUDIO_OUTPUT_FRAMES`;
  - the split's `AUDIO_OFFSET_CLAMP_MS`;
  - the three launch-gate bounds.
- `tests/genlock_audio_buffering_wiring_1367.rs` is std-only.
  - It checks the wiring anchors and the retired upstream lines.
  - It checks the pwsh mirror in both `windows-genlock*.yml` (ONE `WIRING` list).
  - It runs a C LIFT of the shipped reset block, tick decision and buffering functions, driven
    through a launch, late sources, the low-latency toggle, 44.1 kHz, a maximum at the floor and a
    clamp past the maximum.
  - The lift runs ONCE per test binary (`OnceLock`): two parallel tests compiling into one scratch
    dir raced.
- Local runs:
  - the module: `rustc --test` + `clippy-driver --test -D warnings`;
  - the parity gate: a stub `camera_box` rlib of `genlock_audio_buffering` + `asrc_bench`;
  - the wiring file: plain `rustc --test` with `CARGO_MANIFEST_DIR`;
  - the three C files: the `obs-drm-output.md` `-fsyntax-only` recipe.
- Mutation proof: point `CARGO_MANIFEST_DIR` at a scratch tree holding the mutated header /
  `obs.c` / `obs-audio.c` plus the lifted files, and recompile each test per mutant. 23/23 C mutants
  died after review round 2 (incl. each band edge made strict, the raise no-op guard removed, the
  UNREACHABLE total/floor arguments swapped), and 3/3 Rust constant mutants.
- The C lift models the window `audio_callback` really processes: the front of
  `buffered_timestamps`, which stays `start − total × tick` behind real time (the queue keeps
  `total_buffering_ticks` windows forever; while ticks wait that is `buffered_ts − wait × tick`).
  Delays are behind REAL time: 10 ms is absorbed by the floor; the stream ASIO startup race runs
  undrained (85 ms absorbed, 100 ms adds one tick); every total is `max(floor, stock)`.
  `raise_audio_buffering` to the current total is a no-op. (Review round 2 caught the first model,
  which put the window back at real time once the waits drained.)

## Live acceptance (supervisor)

Full-bundle deploy on stream (and resolume + strih-lx, same libobs), then at least two OBS
restarts. After each:
- the log shows `buffering type:  fixed floor 85 ms` and exactly one floor line;
- stream: `mbc` `target=` = 100 + offset and `level_avg` within ~5 ms of it after ~15 min,
  `fallbacks=0`; the release A/V gate is green and the per-camera A/V is the same across the
  restarts;
- resolume: the floor line, then an `ABOVE the floor` line per media / `NDI test` start ON PROGRAM
  (expected; a hidden one logs a `buffering-guard:` line instead, issue 1381);
  the `sp-*_video` audio pairing (`audio_pairing_offset_ms`, `audio_health=0`) and the songplayer
  A/V gate unchanged; FOH hears the program audio ≥ 85 ms later than before while idle;
- strih-lx: the floor line, no UNREACHABLE line, OBS audio monitoring normal.

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
#800 `total_buffering=` stays at the floor, the `audio-stall #1367` `ticks` stay at ~2812/min. A cut
to it: one `entered the mix late` line; for an NDI source with genlock off or on the latency hold no
ABOVE line and no obs-vban underflow / discontinuity at the cut; a genlock TIMECODE-hold source that
stays late may still log one ABOVE line a tick or two later (the known limit). A media start on
program still logs its ABOVE line (upstream).
