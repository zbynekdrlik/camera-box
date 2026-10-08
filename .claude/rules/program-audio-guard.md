---
paths:
  - "scripts/program_audio.py"
  - "scripts/program_audio_ndi.py"
  - "scripts/program_audio_sampler.py"
  - "scripts/program_audio_capture.py"
  - "scripts/program_audio_http.py"
  - "scripts/lib/strih-program-audio.sh"
  - "scripts/lib/program-audio-mode.sh"
  - "scripts/program_audio_guard.py"
  - "scripts/program_audio_marker.py"
  - "scripts/program_audio_marker_calibrate.py"
  - "scripts/qpsk_guard_shim.cpp"
  - "scripts/build-qpsk-guard-shim.sh"
  - "scripts/rig_serve_files.py"
  - "scripts/rig-marker-mirror.sh"
  - "scripts/rig_marker_mirror.py"
  - "systemd/program-audio-sampler.*"
  - "systemd/rig-marker-mirror.*"
  - "tests/python/test_program_audio_1404.py"
  - "tests/python/test_program_audio_guard_1404.py"
  - "tests/python/test_program_audio_marker_1404.py"
  - "tests/python/test_program_audio_timeline_1404.py"
  - "tests/python/test_program_audio_bridge_1404.py"
  - "tests/python/qpsk_guard_shim_1404.py"
  - "tests/python/test_rig_marker_mirror_1404.py"
  - "tests/python/test_rig_serve_routes_1404.py"
  - "tests/python/test_program_audio_capture_1404.py"
  - "tests/python/test_program_audio_datestep_1404.py"
  - "tests/python/test_program_audio_http_1404.py"
  - "tests/python/test_strih_program_audio_1404.py"
---

# The stream program-audio guard + the cam2 marker mirror (issue 1404)

Two read-only endpoints:
- **The cam2 marker mirror is a dev1 endpoint** next to the rig lease on :8890.
  `scripts/rig-lease-server.py` serves it from its SERVE dir (`scripts/rig_serve_files.py`):
  `$XDG_RUNTIME_DIR/rig-lease-serve`, overridable with `$RIG_LEASE_SERVE_DIR`.
  - **tmpfs:** the mirror rewrites a multi-MB file every 10 s, which must not wear the SSD.
  - **0700:** no other dev1 account can plant a file.
  - **Never the lease dir or inside it:** the lease dir's existence means `held=true`.
  - **Only files this user owns are served.**
- **The program-audio verdict is served by the sampler itself on strih-lx**
  (`http://10.77.9.202:8891/program-audio.json`, "The host" below). Its serve dir is its own
  `$XDG_RUNTIME_DIR/program-audio-sampler` (override `$PROGRAM_AUDIO_SERVE_DIR`), never the lease
  server's. The dev1 copy was retired on 8.10.2026 (design 6054654255): the dev1 unit file is
  deleted, and once the dev1 lease server runs this code (a restart while `held=false`, the
  README's post-merge step) it answers its plain 404 for `/program-audio.json`, so a stale file
  left in its serve dir never reads as a live verdict.

| Route | Writer | Contract |
|---|---|---|
| dev1 `:8890/rig-qpsk-markers.csv` | `rig-marker-mirror` `--user` service (`scripts/rig-marker-mirror.sh` → `rig_marker_mirror.py`) | cam2's `/run/rig-qpsk-markers.csv`, complete rows; `text/csv`; `X-Mirror-Age-S` = seconds since new rows last arrived; 404 absent |
| strih-lx `:8891/program-audio.json` | the `program-audio-sampler` `--user` service on strih-lx, which serves it itself | `{schema, ts_utc, age_s, verdict, rms_dbfs, outside_band_pct, window_s, source, last_foreign_ts_utc, last_foreign_age_s, markers_decoded, marker_chain, holes_bridged, bridged_ms, queue_drops, lag_ms, sender_stalls[, reason]}`; both ages recomputed by the endpoint per request; the two marker counts are null without a full marker span; `holes_bridged`/`bridged_ms` count the bridged holes, `queue_drops` the frames the sampler's own capture queue dropped and `sender_stalls` the sender stalls the look-ahead found, since the sampler started (null while it is not sampling); `lag_ms` = how long the block that completed the judged window waited in the capture queue before the consumer took it (the consumer's backlog, the window's own ~17–85 ms of processing on top; null with no window); 404 absent; unreadable or foreign-owned = UNKNOWN |

The consumer CLI is `scripts/program_audio_guard.py`, used by both YouTube gates (camera-box and
restreamer issue 357):
- exit 0: MEASUREMENT or SILENT, fresh (within `--max-age`), and no FOREIGN window within `--latch-s`.
  A MEASUREMENT must also carry a numeric `marker_chain`: one without it comes from a sampler older
  than the marker requirement and exits 2;
- exit 1: FOREIGN. That includes a stale FOREIGN, and a clean current window when a FOREIGN window
  ended within `--latch-s` (default 30 s, its own hold, longer than `--max-age`): a gate that polls
  at least every ~25 s (a 10 s poll plus the guard's runtime has margin) never misses one;
- exit 2: UNKNOWN, stale (more than 1 s in the future counts as stale), unreachable, or a broken
  HTTP response (fail closed).

It prints one line: `program-audio verdict=<V> rms=<x> outside_band=<y>% age=<s> markers=<n> chain=<c>[ reason=…]`.
Consumers act on the exit code; the line is for logs and people.

## Why spectral, not a level bar

The owner rule (issue 1404 comment 6016489928): nothing copyrighted on YouTube. A level bar cannot
tell the loud healthy QPSK marker from music. The marker (carrier 442 Hz) and its room sit in
200–800 Hz, so the verdict is the share of energy OUTSIDE that band:
- the FFT is per channel and the channel POWERS are summed. Never a mono downmix: the marker is on
  L and R ~10 ms apart, and an anti-phase pair would cancel to zero;
- the level gates SILENT;
- a NaN/Inf sample is UNKNOWN.

**The declared measurement tone lines** (`MEASUREMENT_TONE_LINES_HZ = (1000.0,)`, ±3 Hz) are
removed before measuring, from the level and from both sides of the share.
- The plan's Task 5 CG clip plays the QPSK marker over a −30 dBFS 1 kHz bed. Marker + bed read
  84 % outside the band, which would make every CG session stop itself.
- Why not count the bed as measurement: the louder bed would then dilute foreign content under it.
- A bed-only program reads SILENT.
- The Task 5 generator must import this constant.

## Calibration (6.10.2026) — the constants in `scripts/program_audio.py`, pinned by the tests

| Audio | windows | outside_band_pct | rms dBFS | verdict |
|---|---|---|---|---|
| Session recordings rec2/rec3a/rec3b/session (48 k stereo) | 1851 | 9.1 … 25.3 (p99 23.7) | −37.0 … −34.9 | MEASUREMENT |
| LIVE stream program via NDI (`STREAM-SNV (stream)`) | 15 + 11 | 12.4 … 22.1 | −35.9 … −35.5 | MEASUREMENT |
| LIVE SongPlayer program via NDI (`RESOLUME-SNV (SP-program)`, music) | 8 | 87.6 … 94.0 | −15.4 … −14.6 | FOREIGN |
| Generated white / pink / speech-shaped noise | — | 97.6 / 81.2 / 42.2 | (−20) | FOREIGN |

`FOREIGN_OUTSIDE_BAND_PCT = 30` (4.7 points over the measurement maximum) and
`SILENT_RMS_DBFS = −60` (23 dB under the quietest measurement window).

Known limits:
- **Quiet foreign content is missed.** Foreign content mixed well BELOW the measurement level is
  not caught: pink noise at −6 dB under it reads 26.2 % (at −3 dB, 32.3 %, caught). Music at
  program level is ~20 dB over the measurement and reads ~90 %.
- **In-band music mixed UNDER a marker that still decodes reads MEASUREMENT.** The chain stands and
  the share stays in band. The measurement-clip-only rule for SongPlayer and the cg OBS (plan
  Task 5) is the control for it.

Re-calibrate only from real program audio. Use `analyse()` over 2 s windows of a stream recording
or of a live NDI receive, and never tune the threshold to pass a single run.

## MEASUREMENT needs the QPSK marker itself (ROZHODNUTÉ 6026577906 + 6026826572)

The spectral share is necessary, not sufficient: a soft chord or a melody inside 200–800 Hz has
almost nothing outside the band (Design-question 6026559236). The only property unique to the
measurement is the cam2 QPSK marker. So MEASUREMENT = the spectral condition on the current 2 s
window AND a timecode chain of at least `MARKER_CHAIN_MIN` = 4 markers over the trailing 4 s of
contiguous non-silent audio, read per channel (never a downmix), best channel.

**The decoder is the dock's own**, never a third copy. `scripts/qpsk_guard_shim.cpp` is a thin C ABI
(`qpsk_guard_decode_channel`) over `cb_scan_markers` in
`vendor/av-sync-dock/src/camera-box-marker-scan.hpp`, the C++ port pinned to `src/qpsk_marker.rs`.
- It runs with the dock's constants: 442 Hz, c = 1, threshold 0.35. They match `rig60()`,
  `DOCK_QPSK_THRESHOLD` and the painter log's `# qpsk-params` line, pinned by a test.
- It returns each CRC-valid word as (start sample, index) and decides nothing.
- `scripts/build-qpsk-guard-shim.sh` builds it with g++ (no cargo) into
  `~/.local/lib/camera-box/libqpsk-guard-shim.so` (override: `QPSK_GUARD_SHIM`).
- `scripts/program_audio_marker.py` loads it with ctypes.

**A raw CRC-valid word is not a marker.** Preamble + zero nibble + CRC-4 is only 12 bits per screen
pass, and in-band tonal audio passes the screen at thousands of positions. A raw count of ≥ 2 per
window passed held chords (75/300 windows) and band-limited noise (297/300). The rule, per channel,
in `program_audio.py` (`marker_candidates`, `marker_chain`, `span_markers`):
1. **Merge one marker's re-hits.** Same-index words less than 0.25 s apart are one marker.
2. **Drop repeating indices.** An index whose words lie 0.25 s or more apart is dropped entirely.
   - The emitter's index is frame_id mod 256 at 60 fps, so it wraps every 256/60 = 4.27 s, longer
     than the span. That is why `MARKER_SPAN_S` must stay < 4.27 s (pinned).
   - Decided on the whole index, never by chaining re-hits: a dense run of one index (every 70 ms)
     spans more than 0.25 s and is dropped, not merged into one marker.
   - Without this step a held tremolo chord read a chain of 4 window after window (each dense
     same-index run crosses any 60/s line once; 6026817074).
3. **Count the chain.** The most remaining markers, ≥ 0.25 s apart, on one line
   `idx_j − idx_i ≡ round(60·Δt)` (mod 256, ±2).
4. **Decide.** MEASUREMENT needs chain ≥ 4; below = FOREIGN; no chain = UNKNOWN.

**Calibration (7.10.2026)**, through the real sampler loop and the real shim:
`scripts/program_audio_marker_calibrate.py` (exit 1 on a failed bar).

| Bar | Audio | Result |
|---|---|---|
| (a) real: chain ≥ MIN + 2, 0 FOREIGN | rec2 / rec3a / rec3b / session, 1847 judged windows | chain 6–8, minimum 6 (rec3b at 292 s, committed as `tests/fixtures/program_audio_1404/rec3b-290s-stereo-48k.flac`) |
| (a) | Task 1 fixtures, 51 judged windows | minimum 6 (`s3-A-vod`, the first span after the stream began) |
| (b) synthetic in-band: a FOREIGN in every 3 consecutive windows | 50 trials × 10 windows, −30 and −15 dBFS | worst chain held over 3 windows: chords 1, tremolo chords 1, melody 1, band-limited noise 3 |

The tests run bar (a) on the fixtures + the committed clip, and bar (b) on 3 trials per class and
level. Re-run the full calibration after any decoder or rule change:
`python3 scripts/program_audio_marker_calibrate.py --real <the four recordings> --synthetic-trials 50`
(`--classes` splits the synthetic run; each class takes ~1.5 min on dev1).

**The sampler (`program_audio_sampler.py`):** the rules below were found and measured while the
sampler ran on dev1 (until 7.10.2026), so their text says "dev1". Since then it runs only on
strih-lx, where "dev1's wall step" and "dev1's arrival time" mean strih-lx's own; strih-lx is the
fleet's date master.
- **Warm-up.** Until 4 s of audio arrived since the start, a span restart or a format change,
  nothing reads MEASUREMENT. A window whose spectrum alone says FOREIGN (`spectral_foreign`: level
  ≥ −60 dBFS and ≥ 30 % outside the band) reads FOREIGN and starts the latch (ROZHODNUTÉ 6027706292
  item 1: only MEASUREMENT needs the marker chain). Every other warm-up window, SILENT included,
  reads UNKNOWN.
- **The span restarts on a hole in the SENDER's audio timeline, never on dev1's arrival time**
  (design 6030385284, Approach 1; `frame_continues` in `program_audio.py`, pinned by
  `tests/python/test_program_audio_timeline_1404.py`).
  - Why: the old rule (no audio block for over 1 s) read 57 spurious `MEASUREMENT -> UNKNOWN` in 6 h
    on 7.10.2026, all `receive gap of 1.0–2.0 s` while worktree lanes loaded dev1. The sampler was
    starved and the NDI SDK handed the queued audio over in one late burst; nothing was lost.
    Restreamer's watchdog stops a YouTube session on 2 consecutive UNKNOWN polls or 3 within 60 s.
  - The rule: every NDI audio frame carries the SDK `timestamp` (100 ns, the sender's submission
    time). `expected = prev_timestamp + prev_samples / sample_rate`. Within ± (one frame + 20 ms)
    = ±41.3 ms at 1024 samples / 48 kHz it CONTINUES, whatever the arrival gap (logged as a
    `late burst … the marker span is kept`). Farther off it is a DISCONTINUITY and the window and
    span restart (`audio timeline discontinuity: the frame sits +X ms off …`).
  - It also closes the old reverse hole: audio LOST while blocks kept arriving under 1 s apart was
    stitched into one span and could read one false FOREIGN (review of 6027557132: 4 of 762 cut
    clips). A lost stretch longer than the tolerance now moves the timestamps and restarts the
    span. A shorter one is still stitched: one missing frame (21.3 ms) always, two or three when the
    sender's jitter pulls the next stamp back inside 41.3 ms. The lane's review probe cut 1–3 frame
    holes into the three real fixtures (45 positions per case, exact and jittered stamps, the real
    decoder shim): 0 FOREIGN windows. A frame the sampler drops itself (sample rate ≤ 0) is such a
    hole too.
  - **A hole up to 250 ms AHEAD of the timeline is BRIDGED, never a restart** (design 6036098516,
    Approach 1; `HOLE_BRIDGE_MAX_MS`, `frame_continues` returns `Continuity(kind, missing_samples)`,
    pinned by `tests/python/test_program_audio_bridge_1404.py`).
    - Why: on 7.10.2026 11:30–12:35 the journal held 55 discontinuities, all POSITIVE, 42 of them
      +41.4…+52.6 ms = two NDI frames (2048 samples) plus send jitter, arrival gap 0.1 s, never
      answered by a negative step. The receiver dropped frames while dev1 was loaded, and every hole
      cost a 4 s warm-up (`timeline_breaks=33 UNKNOWN=22` in one summary); restreamer's watchdog
      stopped its YouTube gate on it (dev CI 37602415434).
    - The rule: tolerance < offset ≤ 250 ms → `round(offset·sr)` zero samples go through the window
      accumulator before the frame and the span is kept (one `audio timeline hole: … bridged with N
      zero samples (X ms), the marker span is kept` line). Behind the timeline beyond the tolerance
      (an overlap, a backward jump), a hole over 250 ms, a sample-rate change at the hole and an
      undefined timestamp restart as before. The bridged frame's offset is a hole, never jitter, so
      it stays out of `max_offset_ms`.
    - **The chain is decoded over the REAL samples only.** The accumulator and `MarkerSpan` carry a
      real-sample mask; `decode_real_samples` hands the decoder each delivered stretch on its own and
      moves the word times to their timeline place, so the zeros (and the edges next to them) can
      never add a word. Probe before the change: decoding the zero-filled span found words starting
      inside the zeros (4 in one chord trial) and a chain one higher than the delivered stretches in
      3 of 40 in-band trials. A span with no bridge decodes in ONE call as before, so the bars cannot
      move (re-run 7.10.2026: bar a 1847 windows min 6, bar b chords/tremolo/melody 1, bandnoise 3).
    - **The level is the delivered samples' own** (`analyse(samples, sr, real)`, review round 1).
      Over the window with its zeros the level falls (a 250 ms hole = −0.58 dB, two thirds zeros
      = −4.8 dB), and quiet broadband music just over the −60 dBFS bar read SILENT, which the gate
      passes. A window with nothing delivered is UNKNOWN.
    - **The spectral share stays a ratio of the delivered signal.** Zeros add no energy to either
      side. Music windows with 2–11-frame holes or two thirds zeros keep their share within 5 points
      (pink ~80 %, speech-shaped ~42 %, all over the bar).
    - **Residual: a hole can lift a REAL measurement window over the bar (a false FOREIGN).** The
      measurement's in-band energy comes in marker bursts (a decoded word every ~0.5 s), so a hole
      that removes a burst raises the share of what was delivered. Random positions on the full
      rec3b + rec2 recordings, 5340 window cases each: one 2-frame hole crossed the bar 3 times
      (worst 32.8 %), two holes in one window 8 times (worst 53 %). A 5 ms fade of the hole edges
      gave 2 and 10: a fade does not lower the rate (in one case, rec3b window 64, the edge step alone
      carried the window over the bar, 32.8 % -> 29.3 % faded), so no fade is applied. Before the
      bridge the window holding a hole was never judged (the span restarted, UNKNOWN). Reported on
      the Design-question thread (6036260703) next to the date step.
    - Restreamer's safety checks, pinned: BROADBAND music (pink) arriving before, across or after a
      bridged hole reads FOREIGN in the same window and starts the latch with the same payload as
      without the hole; music two thirds bridged zeros never reads MEASUREMENT and the decoder never
      sees a bridged sample. An in-band chord (FOREIGN only through a short chain) now reads UNKNOWN
      while its span holds bridged audio and FOREIGN at most two windows later: the rule-A
      trade-off below (ROZHODNUTÉ 6037765523), never MEASUREMENT.
    - Replay of the 55 live offsets as real holes (frames dropped, the rest as send jitter) through
      the real decoder on rec2 and session (20 min each): 49 bridged, 6 UNKNOWN (only the holes over
      250 ms: 256.3, 337.8, 357.6, 543.0, 556.2, 571.7 ms), 0 FOREIGN, minimum chain 7.
    - **A FORWARD timestamp step with no sample lost** (Design-question 6036260703) pushed every later
      marker round(60·δ) indices off the line when bridged: +50…+250 ms on the committed clip gave
      2 FOREIGN windows, minimum chain 3. The fleet date step is now matched to dev1's own wall step
      (DATE_STEP, below), and a sender stall by the look-ahead (below).
  - Fallback: only when a timestamp is undefined (`INT64_MAX` = `NDIlib_recv_timestamp_undefined`,
    or ≤ 0) the old arrival rule applies (`RECEIVE_GAP_S` = 1 s, logged `receive gap of … (no NDI
    sender timestamp …)`). An NDI error frame still restarts the span unconditionally (stricter
    than the fallback; 0 error frames in the 6 h live journal).
  - The 10-minute summary counts `timeline_breaks`, `late_bursts` and `receive_gaps` (fallback only),
    and reports `max_offset_ms`: the largest |offset| of a frame that continued, i.e. the sender's
    jitter against the 41.3 ms tolerance (the margin to watch), then `holes_bridged` and
    `bridged_ms`; a restart after an NDI error frame shows as `error_frames`. Each restart, each
    late burst and each bridge also logs one line. Since design 6037613222 the line also carries
    `queue_drops`, `max_lag_ms` and `date_steps`, and since the look-ahead `sender_stalls` and
    `max_stall_ms` (all between `bad_rate_frames` and `timeline_breaks`, so the older field anchors
    keep their neighbours). A held frame's decision lands in the interval in which the look-ahead
    decides it (up to 4 frames later).
  - STEP 0 (7.10.2026): a second, read-only sampler instance (private serve dir) took the live
    `STREAM-SNV (stream)` for 25 min while dev1 ran test suites and the marker calibration.
    70 304 frames (comment 6030714990):
    - the one arrival gap over 1 s (1.25 s; the old loop logged `receive gap of 1.2 s` and
      `MEASUREMENT -> UNKNOWN`) sat +0.87 ms on the timeline, and the burst after it delivered
      1259 ms of audio within 50 ms: nothing lost;
    - all 14 gaps of 0.5–0.96 s and all 140 over 0.2 s were on the timeline;
    - the submission jitter reached −21.1 / +24.5 ms (p99 11.7 ms) against the ±41.3 ms
      tolerance: no false discontinuity in 70 302 continuous pairs;
    - ONE real hole was off it: +199.8 ms with a 0.19 s arrival gap. Its frames were never
      delivered, the old rule stitched across it, and the stream OBS log shows nothing at that
      moment, so where they were lost is not known;
    - the SDK `timestamp` and the sender's `timecode` agree to 2 µs: both are its wall clock at
      submission (`vendor/distroav/src/ndi-output.cpp`, `genlock_wall_now_100ns`);
    - a second 20-min run of the NEW loop on the live sender (56 250 frames): jitter up to 29.5 ms
      (p99 18.2, so the margin to 41.3 ms is ~12 ms: watch `max_offset_ms`), 0 timeline breaks,
      0 receive gaps, 599 MEASUREMENT windows and only the start-up UNKNOWN.
    A probe can reuse the scratch recipe: subclass `NdiAudioReceiver.capture` to log
    `frame.timestamp`, run `program_audio_sampler.run` with a private serve dir, never the live
    one, and never restart the live unit for it.
  - **A dantesync date step is DATE_STEP, never a hole** (design 6037613222; `frame_continues(...,
    wall_steps=...)`, `WallSteps`, pinned by `tests/python/test_program_audio_datestep_1404.py`).
    - dev1 runs the same fleet dantesync as the stream box (both followers, the same announced
      `date_offset_seq`), so dev1's own wall clock steps at the same instant. On strih-lx, the
      date MASTER, its own wall step is announced together with the stream box's follower step.
    - The capture path reads dev1's wall-minus-monotonic offset with every block: ONE bracketed read
      (`read_wall_offset_ns`: monotonic, wall, monotonic; the wall placed at the middle; a bracket over
      1 ms is retried 3 times, else no reading). Frequency slewing moves both clocks alike, so the
      offset changes only on a STEP; a change of 5 ms or more between two readings is a dev1 step.
    - A FORWARD timestamp jump over the tolerance that matches a forward dev1 step of the same size
      (±20 ms), seen within the last 2 s, is a DATE_STEP: no zeros, no restart; the next frame is
      judged against the stepped one, so the timeline is re-based. One dev1 step excuses one jump
      (it is consumed). A `audio timeline date step: … nothing lost` line, `date_steps` in the summary.
    - Without a matching dev1 step (dev1's dantesync missed it, or stepped more than 2 s earlier, or
      the sender's jump came first) the rules stay: a forward jump up to 250 ms is bridged, a larger
      one or a backward one restarts the span (one UNKNOWN, never FOREIGN).
    - Only FORWARD jumps (the design). A backward date step still costs one warm-up.
    - The match only looks BACK: dev1 must have stepped before the first stepped frame arrives (the
      stream box and dev1 step at the same announced instant, the frame then needs the network and
      the SDK). If dev1 steps later, or corrects its date by slewing / micro-steps instead (its
      `/status` shows `date_slew_active` / `date_micro_active`), the jump falls back to the bridge or
      a restart. Confirm the order at the next nightly step before calling DATE_STEP live: the
      journal's `timeline date step` line, never a `bridged with` line, at the step time.
    - The two windows whose span holds a DATE_STEP count as holed for the short-chain rule below:
      the ±20 ms match can absorb a small real loss (one frame plus negative jitter).
    - Micro-corrections of a few ms stay inside the tolerance.
  - Residual limits: a sender stall whose catch-up frames do not bring the timeline back within the
    tolerance in 4 frames (a stall of ~150 ms or more) is bridged as a hole of its smallest offset
    although nothing was lost (a holed span, rule A); a sender with no timestamps falls back to the
    arrival rule and its old limit.
  - **Test trap: a fake decoder must follow the stretch it is handed.** With a bridge in the span
    the decoder is called once per delivered stretch, and `FixedChain` returns the same 8 words for
    every call, so the copies collide on rule 2 and a measurement span reads FOREIGN. Loop tests
    with a bridge use `_TimelineMarkers` (markers written into channel 0 at ~−100 dBFS on the
    sender timeline, read back where the sampler puts them) or the real shim.
  - **Test trap: a hole exactly AT a limit is decided by the stamps' floor rounding.** 1024-sample
    frames at 48 kHz are 213 333.3 units of 100 ns, so a test hole meant as "+250.0 ms" lands a few
    units over or under the limit depending on the frame index (one landed as a discontinuity).
    Pin a boundary with 4800-sample frames (exactly 1 000 000 units), and keep loop tests a little
    inside it (249 ms).
  - Tests: the fakes use a local `_Block` with a timestamp field, so they exercise the loop, not the
    binding; `pan.AudioBlock` defaults `timestamp` to undefined, so an old fake (2 fields) runs the
    arrival fallback. `program_audio_marker_calibrate.py` stamps its blocks on a continuous
    timeline, so the bars run the same path as the service.
  - **Trap: the tolerance scales with the block size.** The committed fixtures
    (`tests/fixtures/youtube_leg_1404/*.flac`) are 16 kHz MONO, not the live 48 kHz stereo. The old
    sampler tests feed 1600-sample blocks = 100 ms frames, so the tolerance there is 120 ms and a
    50 ms date step would read as continuous. The timeline tests use `sr // 50` (20 ms) blocks, so
    the tolerance (40 ms) matches the live 41.3 ms, and 6 s is a whole block count.
  - The previous-frame rule and the `max_offset_ms` bookkeeping are pinned by mutants: judging with
    the current block's size, a break feeding the max, a dropped `abs`, no reset, and late bursts
    left out each fail a test.
- **A chain cut short by bridged audio is never FOREIGN on its own** (ROZHODNUTÉ 6037765523,
  restreamer run 37602415434; `classify(..., holed=True)`).
  - When the trailing span holds bridged samples (a bridged hole or a queue drop) and the spectrum
    is measurement-like, a chain under 4 reads UNKNOWN with the reason `marker chain N < 4 over a
    span holding X ms of bridged audio -- … never FOREIGN on its own`. A spectral FOREIGN stays
    immediate, holed or not.
  - The live case: 14:17:37 `MEASUREMENT -> FOREIGN … marker_chain=3` after 16 "holes" of
    +41…+54 ms in 13 s. Replayed on the committed clip through the real decoder: the STALL shape
    (below) gives 3 FOREIGN windows with chain 3 before, 3 UNKNOWN after; the same 16 holes as REAL
    loss give chain 4–8 and no FOREIGN either way.
  - Trade-off, accepted by the owner: an in-band chord during a holed span reads UNKNOWN instead of
    FOREIGN, and FOREIGN comes once the 4 s span no longer holds the bridged audio (at most two
    windows later). Every consumer fails closed on UNKNOWN; restreamer stops on 2 consecutive
    UNKNOWN or 3 within 60 s. Broadband music keeps its same-window FOREIGN.
- **The capture thread** (design 6037613222; `scripts/program_audio_capture.py`, pinned by
  `tests/python/test_program_audio_capture_1404.py`).
  - Why: the single loop called `NDIlib_recv_capture_v3` and did the window work (FFT, ctypes decode,
    JSON writes) in one thread. Under load it stopped calling the capture for up to 2 s, and the
    SDK, which holds about 1.3 s of audio, dropped the oldest.
  - The thread only blocks in the capture call (ctypes releases the GIL), stamps the arrival time and
    the dev1 wall offset, and appends to a queue bounded at 10 s of audio. It never runs the FFT, the
    decode or a write (a test spies the thread names). The consumer is the old loop, unchanged except
    that it takes items from the queue.
  - A full queue drops the NEW frame and counts it (`queue_drops`, summary + JSON). The drop rides on
    the next queued AUDIO block (never an error item or an empty block: the consumer does not judge
    those, so the hole would be lost), and the consumer reads it as a hole of exactly the dropped
    audio: a BRIDGE of those samples up to 250 ms, or of the whole offset when the timestamps show
    more missing; a span restart beyond 250 ms or behind the dropped audio. It is logged
    `queue overflow: N frames (X ms) dropped by the sampler's own capture queue -- …`. Every branch
    is pinned in the pure decision, including a drop with send jitter (bridged with exactly the
    dropped samples, never round(offset·sr)). Without timestamps a drop is bridged too, but an
    arrival gap over 1 s still takes the arrival-fallback restart.
  - An NDI error frame is queued as an error item (the span restarts as before), and the thread waits
    one capture timeout. Any other exception in the capture call is handed to the consumer, whose
    next get raises it, so the sampler exits non-zero and systemd restarts it (never a live consumer
    reading "no audio" forever).
  - Shutdown: SIGTERM sets the flag, the consumer returns, `CaptureThread.stop()` waits for the thread
    to leave the SDK call, and only then is the receiver closed (destroying it under a running
    capture would crash the SDK; if the thread is still inside after the join, the receiver is left
    to the process exit). The JSON then reads UNKNOWN `sampler stopped` as before.
  - `max_lag_ms` in the summary is the oldest captured item the consumer took (its backlog), and
    `lag_ms` in the JSON is the lag of the window it judges: a consumer that falls behind its 10 s
    queue never passes for a fresh one (`ts_utc` is the write time). The arrival gaps (late bursts)
    are now the capture thread's own.
  - `run(capture=None)` (the calibration CLI and every loop test) keeps the capture call in the loop
    through `SyncCapture`, the same items without a thread, so the bars and the loop tests run the
    same consumer deterministically. It reads NO wall clock unless `wall_offset` is passed: a real
    dantesync step during a test or a calibration run must never turn a bridge into a date step.
  - **STEP 0 (7.10.2026, Design-question 6037861831):** two read-only probes on the live
    `STREAM-SNV (stream)`, private serve dirs, side by side at nice 10, 720 s under the natural load
    (load avg 6–18, other projects' CI). Single loop: 5 losses from its own starvation (arrival
    gaps 1.74–2.03 s, 362–826 ms lost each, a span restart each); capture thread: 0 (its largest
    gap between two captures 0.51 s). Both also saw 2 losses at the same moments (418/579 ms and
    224/449 ms): a whole-process stall that also starved the SDK's own threads. The receiver holds
    more than one thread of its own (`ndir:audio`, `ndir:reconn` ×N for the RUDP connections).
  - **CPU priority (STEP 0 run 2, comment 6038303132).** Four busy loops at nice 0 in the lanes' own
    cgroup, 600 s, three capture-thread probes side by side: nice 10 lost audio 13 times (7.2 s, 9 of
    them its own), nice 0 and nice 0 + `CPUWeight=1000` 4 times each (2.9–3.1 s), and those 4 were ONE
    box-wide event that hit every receiver on dev1 at once, the live unit included (+1638.5 ms at
    12:48:10Z; dev1 IO pressure, 4.7 GB swap in use; the stream OBS log quiet). So `Nice=10` is
    gone from the unit; that is the measured cure.
    - **No host tuning (coordinator, 7.10.2026): both units run the sampler at NORMAL priority** (no
      `Nice=`, no `CPUWeight=`). A `--user` unit cannot lower nice on dev1 anyway (`systemd-run
      --user -p Nice=-5` runs at nice 0 with no error, RLIMIT_NICE 0), and on strih-lx a CPUWeight
      would rank the sampler ahead of OBS in the same slice. The sampler logs one start line,
      `scheduling nice=N cpus=<list> cpu.weight=W` (a WARNING only at a positive nice).
    - A `--user` CPUWeight competes only inside the user's own slice; against other users' load the
      passive run's capture thread at nice 10 had no loss of its own.
    - A system unit with a realtime capture thread was weighed and not taken: the remaining losses
      are box-wide, and the SDK's own receive threads (`ndir:*`) stall with them. The real answer
      to dev1's load was the move to strih-lx (below).
  - **Most "holes" are not lost audio: a SENDER stall.** 20 of the 22 forward steps over the
    tolerance in the 720 s run were identical in all three receivers (both probes and the live
    unit, to 0.1 ms) and each was followed by two frames at −21.1 ms: the cumulative offset returns
    to within ±1.1 ms. The stream OBS stamps an NDI audio frame with its wall clock at SUBMISSION;
    its audio thread stalls ~64 ms (`audio-stall #1367: tick_gap_max_ms=58…68` every minute) and
    then submits three frames back to back. The bridge inserts ~43 ms of zeros for audio that was
    never lost, and every later marker sits 2.6 indices off the line. That is the 14:17:37 false
    FOREIGN. Rule A alone turned it into UNKNOWN, which did NOT end the YouTube stop: the live
    pattern replayed read `U U M M M M U M U U M` (two UNKNOWN in a row, three within ~20 s), enough
    for restreamer's 2-consecutive / 3-within-60 s rule. The look-ahead (next section) ends it.
- **The sender-stall look-ahead** (ROZHODNUTÉ on issue 1404, the answer to Design-question
  6037861831; `program_audio.resolve_ahead` + `program_audio_sampler.HeldFrames`, pinned by
  `tests/python/test_program_audio_stall_1404.py`).
  - A frame more than the tolerance AHEAD of the timeline (beyond any known queue drop), with no
    matching dev1 wall step and no format change, is HELD (`judge_continuity` kind `ahead`), and so
    are up to `STALL_LOOKAHEAD_FRAMES` = 4 frames after it (~85 ms).
  - Each held frame's CUMULATIVE offset is measured against the frame before the step: its stamp
    minus (that frame's stamp + its duration + every held frame before it + the first frame's known
    drop). Summed as integer differences: a live stamp (~1.8e16) does not fit a float exactly.
  - The first held frame back at or under the tolerance decides at once: a SENDER STALL. Nothing
    lost: no zeros (only the first frame's known queue drop), the span kept, `sender_stalls` +1
    (summary + JSON) and `max_stall_ms`. The frames before it are filed at their TIMELINE place,
    never at their late stamps, so the next frame is judged against the timeline (a +100 ms stall
    spread over three frames is one stall, never a −58 ms backward jump). A live stall (+42/−21/−21)
    decides at its first follower, so it costs no wait. No log line per stall (~2 a minute live);
    the summary carries them.
  - None back after 4 followers: the hole = the SMALLEST cumulative offset. Up to 250 ms (with the
    known drop) a BRIDGE of exactly that (`audio timeline hole: the frame sits +X ms ahead … the
    smallest offset +H ms is the hole … bridged with N zero samples`), else a restart (`audio
    timeline discontinuity: … the smallest offset over the next N frame(s) …`). A late-stamped frame after a real loss is filed at
    its place behind the hole, so the zeros are the lost audio, never the late stamp's offset.
  - Every held frame is decided, never lost: a frame that cannot join (a known queue drop, another
    rate or channel count, no stamp) decides the held ones first with what arrived (`complete`), and
    so do a quiet capture poll, an NDI error frame (the held audio belongs to the old span) and the
    loop's stop.
  - The window holding a stall is NOT holed (no zeros), so rule A never touches it: the in-band chord
    around a stall reads FOREIGN in the same window as without it (restreamer's safety test). Rule A
    and the spectral FOREIGN are unchanged.
  - Evidence (7.10.2026): the STEP-0 probes' recorded stamps replayed through the real loop: the
    capture-thread probe's 20 forward steps under 250 ms are 20 sender stalls, 0 zeros (its 2 losses
    of 417/448 ms still restart); the in-loop probe at nice 10 reads 19 stalls, 3 bridges, 5 restarts
    (its own starvation). The live 16-step pattern replayed on the committed clip through the real
    decoder reads MEASUREMENT in every window after the warm-up (was `U U M M M M U M U U M`).
    Calibration unchanged: bar a minimum chain 6, bar b chords/tremolo/melody 1, band noise 3.
  - **Residual: a real 2-frame loss can read as a stall.** A 2-frame loss (42.7 ms) is only 1.3 ms
    over the tolerance, so a follower stamped 1.3 ms early brings it "back". Cut into the real STEP-0
    stamps (fresh-context review), 16.4 % / 37.0 % of 2-frame losses read as a stall (the old
    one-frame rule stitched 8.9 % / 20.3 % the same way), 3-frame losses 0 %. Such a loss gets no
    zeros and its span is not holed, so rule A does not cover it, and no `bridged with` line names
    it. Consequence probe: 2- and 3-frame losses every 4.5 s cut into rec2 / rec3a / rec3b / session
    (1841 windows per run), stamped with the live capture-thread probe's jitter and stalls, through
    the real decoder: 0 FOREIGN, 0 UNKNOWN after the warm-up, minimum chain 4; the pre-look-ahead
    code read 1 FOREIGN (session, 3-frame) and 1 UNKNOWN (rec3b) on the same input.
  - `max_offset_ms` counts a stall's catch-up frame with its offset against the TIMELINE (up to
    the tolerance), while the stalled frames themselves go to `max_stall_ms`: a large stall can
    raise `max_offset_ms` towards 41.3 ms without any jitter.
  - Tests that changed on purpose: a 2-frame hole with a 5 ms late stamp bridges 42.7 ms (was
    47.7); a loop test whose summary boundary fell on a held frame moves it to the 5th frame after
    the hole; a test whose music ended within a few samples of a window boundary got more music
    (exact bridges no longer pad the timeline); the holed-UNKNOWN reason test now uses an in-band
    chord with real losses and pins its 4 holed windows.
  - The limits are pinned to the 100 ns unit with 4800-sample frames (1 000 000 units each); re-held
    groups, a queue drop or a channel change after a held frame, and a frame 300 ms late are pinned
    by scripted runs, and so are the glue's own steps (a follower with no stamp never joins, the
    first frame's known drop reaches resolve_ahead, zeros only before the first held frame, each
    frame taken in once). 22
    mutants (flushes, stamps, limits, the join rules, the re-feed, the glue) all fail a test.
  - A bridge after the look-ahead is the smallest offset, so a follower stamped early shortens it
    by its jitter (a 64 ms loss with followers 8 ms early bridges 56 ms): the later markers sit that
    much early. One marker index is 16.7 ms and the chain allows +-2 (33 ms); live jitter reaches
    p99 18.2 ms (about 1.1 index), max 29.5 ms (about 1.8). The consequence probe above (live jitter
    and stalls, 2- and 3-frame losses) read 0 FOREIGN, 0 UNKNOWN, minimum chain 4.
- **A SILENT window empties the span.** The next non-silent window holds only its own markers, so it
  reads UNKNOWN ("marker span") until the span is full again. Without this a silence→measurement
  start read a short chain and could latch a false FOREIGN.
- **A missing or unloadable shim** = UNKNOWN + exit 1 before the NDI receiver is created, like a
  missing libndi. A decode error on one window = UNKNOWN for that window. The path is made absolute
  before dlopen: a bare name (`QPSK_GUARD_SHIM=libm.so.6`) would make dlopen search the system
  library paths and load a different file than the one checked (review round 1).
- **A shim built from other sources** (sha256 of the shim + the two headers, embedded at build
  time) still loads, with a WARNING asking for a rebuild.
- **A MEASUREMENT without `marker_chain` is refused twice.** After a pull, an old sampler process
  keeps writing spectral-only MEASUREMENT until it restarts.
  - The sampler's own endpoint (`rig_serve_files.program_audio_response`) serves it as UNKNOWN
    with its ages kept. That is the one place every reader sees, restreamer's own reader included
    (review round 1).
  - The camera-box guard also refuses it (exit 2), for an endpoint that still runs the old code.
  - On strih-lx, setup-strih step 16e rebuilds the shim when its sources changed and try-restarts
    the sampler, so a redeploy picks up the new code.
- **Install order matters.** The build renames the new library over the old one, never writes it in
  place: a running sampler keeps its mapped copy (writing a mapped `.so` in place can SIGBUS it).
- **Cost:** ~70 ms of decode per 2 s window (4 s stereo span at 48 kHz) plus the FFT.

## The marker mirror: ONE ssh connection, never a login per pass

One cam2 login writes **11 lines** into cam2's PERSISTENT journal on its USB stick
(`Storage=persistent`, `SystemMaxUse=200M`, measured 6.10.2026). A 10 s scp timer would have
meant ~95 000 lines a day, and a full multi-MB copy every 10 s, over the metered link when the rig
is at a venue. So the mirror is a long-running service holding one
`ssh … exec tail -c +1 -F --pid=$PPID /run/rig-qpsk-markers.csv`:
- **The replay is counted, never guessed.** The remote prints the file size (`stat`, 0 when
  absent) before `tail -c +1`.
  - The replay is complete once that many bytes arrived, or once a second session header arrived
    (the painter restarted mid-replay).
  - Nothing is written before that: not after a stall, not when the connection dies or the service
    stops mid-replay. An idle-gap rule failed review: a 50 ms marker cadence leaves no gap at all.
  - A replay is cut only when no byte arrived for 60 s, or after the larger of 120 s and the
    announced size at 5 kB/s (the size-scaled cap ends the millisecond race of a file replaced
    between `stat` and the tail's open). A fixed 120 s cap re-downloaded the 2.7 MB log forever over
    the 20–65 kB/s venue link (review round 3).
  - A copy equal to, or a prefix of, the served file is not written, so a reconnect with no new
    rows keeps the served file and its age.
  - A new header that follows a half row on the same line (a truncation mid-row) starts the new
    session there.
- **Name following.** `-F` follows the name through a painter restart (`File::create` = truncate +
  a new `# qpsk-params` header, which restarts the copy) and through an EVENT purge + re-creation.
- **No leftover tail.** `--pid=$PPID` (the sshd session) ends the remote tail when the connection
  drops. Checked live: no leftover tail on cam2.
- **Writes.** Only complete rows are kept, written by temp + rename at most every 10 s and only when
  something changed. Live, about 63 bytes/s of new rows.
- **Rig away = no mirroring.** Before each connection the mirror pings cam2. An RTT over 20 ms is
  the rig at a venue behind tailscale over METERED mobile data (~70 ms; dev1 stays at church), so
  nothing is connected or replayed over that link (owner rule: no dev1↔rig transfers during events).
  It is logged once per state change. An unknown RTT still tries ssh. The probe is ICMP, never a
  TCP connect to :22, which would make sshd log a pre-auth line into cam2's stick journal.
  **dev1's ping needs its `cap_net_raw` file capability** (`net.ipv4.ping_group_range = 1 0`), so
  the mirror unit must NOT set `NoNewPrivileges` (it drops the capability: ping exits 2,
  'Operation not permitted'). A ping that cannot run is an `RttProbeError`, logged as `ERROR` once,
  never read as 'cam2 did not answer'. Pinned by a test; review round 4.
- **Reconnects.** A dropped connection logs `ERROR` with ssh's stderr and backs off
  10 → 300 s (back to 10 s after a connection that lived 300 s). The previous file is kept. The
  backoff is waited in 0.5 s slices: `time.sleep` resumes after SIGTERM, so one long sleep held a
  `systemctl stop` past its 90 s timeout. A failed write still stops the ssh process group.
- **Memory.** The copy grows with the painter session (~5 MB a day) in RAM and on tmpfs; cam2 holds
  the same file in its own tmpfs, and a painter restart starts it over. A line over 4096 bytes and
  bytes before the first session header are dropped and logged.
- **The password** goes through `sshpass -e` (`$SSHPASS`), never argv. A oneshot timer cannot hold
  a connection: systemd kills a ControlPersist master with the oneshot's cgroup.

## Traps

- **The NDI receiver is ctypes over `/usr/lib/ndi/libndi.so.6`.** The in-repo `src/ndi.rs`
  receiver is video-only (a NULL audio frame). Struct layouts come from the vendored SDK headers,
  pinned by a test.
- **The receiver is created BY NAME, audio-only.** It runs with a private, empty
  `NDI_CONFIG_DIR` (mDNS only, enforced in code; an extra-IP finder opens TCP discovery connections
  into senders, `.claude/rules/ndi-discovery.md`). Checked live: it still finds the sender.
- **Away at an event, the sampler finds nothing.** The rig sits behind tailscale, so the verdict is
  UNKNOWN and no audio crosses the mobile link.
- **The sampler writes UNKNOWN at every point it is not sampling:** at start, after 5 s without
  audio, when it cannot load libndi, and when it stops. A sample rate ≤ 0 is dropped (it once
  looped forever), and an NDI error frame sleeps instead of spinning.
- **Logging.** Both services log state changes, never per window or row. Private env files
  `~/.config/camera-box/*.env`, never the global `environment.d`.
- **Read-only on the rig.** Never play a test sound there (only the QPSK marker may sound):
  FOREIGN was verified live on the SongPlayer program sender, which already carries music.
- **Restart the lease server only while `held=false`.** `/rig-lease.json`, `/healthz` and the 404
  are pinned to golden bytes captured from the pre-change server.

## The host: strih-lx, its own endpoint :8891 (ROZHODNUTÉ 6039368611)

The owner rejected dev1 for the sampler (shared, loaded by other projects' CI: "naozaj to musi bezat tu
na dev1?!"). It needs only NDI reach to `STREAM-SNV (stream)` and an HTTP endpoint, so it moves to
strih-lx (16 cores, the E-cores 12-15 nearly idle; stream.lan was rejected: a Windows port of the
shim + numpy next to restreamer's broadcast encoder).
- **Its own read-only endpoint** (`scripts/program_audio_http.py`, served from the sampler process):
  `GET/HEAD /program-audio.json` through `rig_serve_files.program_audio_response` (ages per request,
  foreign-owned / garbage / a chain-less MEASUREMENT = UNKNOWN), 404 while absent, `/healthz`,
  anything else 404, other methods 501. `--http-port` / `PROGRAM_AUDIO_HTTP_PORT` default **8891**
  (0 = none), `--http-bind` / `PROGRAM_AUDIO_HTTP_BIND` default 0.0.0.0, `--serve-dir` /
  `PROGRAM_AUDIO_SERVE_DIR` default `$XDG_RUNTIME_DIR/program-audio-sampler` (the sampler's own
  tmpfs dir, created 0700; never the dev1 lease server's `$RIG_LEASE_SERVE_DIR`). Routine requests
  are not logged.
  - **One response framing for both servers:** `rig_serve_files.ReadOnlyHandler` (GET/HEAD through one
    `_handle`, the query-string strip, `_send`, no Python version in `Server:`), moved verbatim out of
    `rig-lease-server.py`; the lease routes' golden bytes are unchanged.
  - **Bound after the receiver exists, stopped after the final UNKNOWN.** A port in use closes the
    receiver, writes UNKNOWN `sampler cannot serve http on …` and exits 1 (fail loud; systemd restarts).
  - Tests never bind the default port (`--http-port 0`, or a free ephemeral port).
- **The dev1 copy is retired (8.10.2026, design 6054654255).** The consumers read
  `http://10.77.9.202:8891/program-audio.json`: restreamer on main (its PRs 384 and 385) and the
  camera-box guard's `DEFAULT_URL`. So the dev1 `--user` unit file is deleted, the dev1 lease server
  answers its plain 404 for `/program-audio.json` (after its restart on this code: the README's
  post-merge step), and the sampler has no dev1 default left. The dev1 unit was disabled live the
  same day. A rollback to dev1 is a revert of this slice's `[green]` commit (`fix(#1404): [green]
  retire the dev1 program-audio sampler path`), never a live need: the owner ruled the sampler off
  dev1. Pinned: `test_the_dev1_sampler_unit_is_gone`,
  `test_the_dev1_server_no_longer_serves_program_audio`, `test_the_default_serve_dir_is_the_samplers_own`.
- **The unit** `systemd/program-audio-sampler.strih-lx.service` (a TEMPLATE) is installed as the
  operator's `--user` `program-audio-sampler.service`:
  - `CPUAffinity=` = the box's own `/sys/devices/cpu_atom/cpus` (12-15 on strih-lx), read the way
    `strih_lx_lowprio_prefix` reads it (whitespace stripped; a value that is not a cpu list = no pin;
    no cpu_atom = no affinity line). Normal priority (no Nice/CPUWeight: never ahead of OBS).
  - `ExecCondition=/usr/bin/test -e %h/.config/camera-box/program-audio-sampler.test-mode`: an OPT-IN
    TEST marker (review round 3). Without it every start is skipped, so the sampler is DOWN by default:
    a fresh provisioning, and a reboot during a production (strih-lx lingers the user manager,
    `Linger=yes`), never start it. An opt-out EVENT marker would not exist before the first
    `rig-mode.sh event` with this code. strih-lx's `/usr/bin/test` is uutils; its exit 1 on a missing
    file was read live (systemd 259 there).
  - `ExecStart` runs the installed checkout-layout copy `/usr/local/lib/camera-box/scripts/…`;
    `WantedBy=default.target` (no X needed; a reboot in TEST mode brings it back).
  - No `After=network-online.target`: a user manager cannot see system targets (review round 4); the
    receiver waits for the source itself.
  - libndi: `/usr/local/lib/libndi.so.6` (strih-lx's NDI 6.3.2 runtime) is a lookup candidate.
- **Provisioning = setup-strih step 16e** (`scripts/lib/strih-program-audio.sh`, as root):
  - apt `python3-numpy` only when missing, then an `import numpy` preflight;
  - the sampler's import closure + the shim's C++ source, its two vendored headers and the build
    script into `/usr/local/lib/camera-box` in the checkout layout (pinned against the real import
    closure by a test), each written only when it differs. The sampler compares the decoder it loads
    with those sources, and the build script builds from them;
  - the shim built AS THE OPERATOR (`sudo -u newlevel env HOME=…`) into the default
    `~/.local/lib/camera-box/libqpsk-guard-shim.so`, only when missing / unloadable / built from other
    sources. A library in the operator's home is never loaded as root (the state check runs as the
    operator too);
  - the rendered unit written only when it differs, `enable`, a daemon-reload when the unit changed, a
    `try-restart` when anything changed. NEVER a start (enable-only).
  - newlevel has no passwordless sudo on strih-lx: everything root goes through setup-strih itself.
    The firewall is off; no ufw rule is added.
- **verify-strih item 41** (`strih_program_audio_grade_report`, 3 rows): files + unit + enabled; the
  shim current; the unit's state against the TEST marker:
  - the state is re-read up to 3 x (1 s apart) while it is in a transition: a starting sampler
    settles, a crash loop (`activating`, auto-restart) stays and is a FAIL. `failed` is a FAIL. An
    EMPTY answer (the operator's user manager unreachable) reads `unreadable`, a FAIL naming linger;
  - running WITHOUT the TEST marker = a FAIL (review round 4): EVENT mode, yet running -- rig-mode.sh
    event's stop failed or timed out, or the marker was removed by hand;
  - running in TEST mode = its endpoint must answer a FRESH verdict at the port/bind of its env file
    (`PROGRAM_AUDIO_HTTP_PORT` / `_BIND`, parsed like systemd reads it -- whitespace around `=`
    dropped, never sourced; a wildcard bind read on 127.0.0.1), read up to 3 x (a grade right after
    step 16e's try-restart, before the bind). Fresh = the guard's own window, -1 s <= `age_s` <= 10 s
    (`STRIH_PROGRAM_AUDIO_FUTURE_TOLERANCE_S` / `_MAX_AGE_S`, pinned to `program_audio_guard`'s
    `NEGATIVE_AGE_TOLERANCE_S` / `DEFAULT_MAX_AGE_S`): strih-lx is the dantesync date master, so a grade
    right after its nightly step can read a verdict slightly in the future. A running sampler with
    port 0 is a FAIL;
  - down WITH the TEST marker = a FAIL; down without it = a NOTE (EVENT mode, or never put in TEST
    mode: setup-strih's own step 17 runs right after an enable-only install).
- **rig-mode.sh** (`scripts/lib/program-audio-mode.sh`, sourced; one call each, after the relay step):
  TEST leaves the marker, clears a failed state (`reset-failed`: a crash loop that hit StartLimitBurst
  refuses the next start for up to 300 s), `systemctl --user start`s the unit and reads its state 2 s
  later (a Type=simple unit reads active the moment it is forked, so a sampler that dies on import
  would pass an immediate read); EVENT removes the marker, stops it and clears a failed state (`stop`
  leaves a failed unit failed, which item 41 would FAIL in EVENT mode). Over plain ssh as the operator
  (`sshpass … timeout … ssh`, `UserKnownHostsFile=/dev/null`; a plain ssh command gets
  `XDG_RUNTIME_DIR=/run/user/1000` on strih-lx); report-only: a WARNING naming the state, never
  rig-mode's exit status (a stopped sampler fails closed for its consumers). A Windows strih is one
  SKIP line.
- **The shared read-only handler drops an idle client after 10 s** (`ReadOnlyHandler.timeout`): the
  endpoint listens on 0.0.0.0 on a production box with no firewall; the lease server gets it too.
- **strih-lx's stack:** Python 3.14.4 and apt's numpy 2.3.5 (dev1: 3.12 + 2.4.6). The sampler suites
  and the marker calibration (bar a min chain 6, bar b unchanged) also pass under Python 3.14.2 +
  numpy 2.3.5 on dev1 (`uv run --no-project --python 3.14 --with numpy==2.3.5 --with pytest`).
- strih-lx's USB 5 GbE NIC still has rx_missed bursts (issue 1242 / 1387): NDI rides TCP/RUDP, so they
  show as late bursts, which the sender-timeline logic keeps.
