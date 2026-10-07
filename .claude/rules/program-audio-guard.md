---
paths:
  - "scripts/program_audio.py"
  - "scripts/program_audio_ndi.py"
  - "scripts/program_audio_sampler.py"
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
  - "tests/python/qpsk_guard_shim_1404.py"
  - "tests/python/test_rig_marker_mirror_1404.py"
  - "tests/python/test_rig_serve_routes_1404.py"
---

# The stream program-audio guard + the cam2 marker mirror (issue 1404)

Two dev1 endpoints next to the rig lease on :8890. `scripts/rig-lease-server.py` serves them from
its SERVE dir (`scripts/rig_serve_files.py`): `$XDG_RUNTIME_DIR/rig-lease-serve`, overridable with
`$RIG_LEASE_SERVE_DIR`.
- **tmpfs:** the mirror rewrites a multi-MB file every 10 s, which must not wear the SSD.
- **0700:** no other dev1 account can plant a file.
- **Never the lease dir or inside it:** the lease dir's existence means `held=true`.
- **Only files this user owns are served.**

| Route | Writer | Contract |
|---|---|---|
| `/rig-qpsk-markers.csv` | `rig-marker-mirror` `--user` service (`scripts/rig-marker-mirror.sh` → `rig_marker_mirror.py`) | cam2's `/run/rig-qpsk-markers.csv`, complete rows; `text/csv`; `X-Mirror-Age-S` = seconds since new rows last arrived; 404 absent |
| `/program-audio.json` | `program-audio-sampler` `--user` service | `{schema, ts_utc, age_s, verdict, rms_dbfs, outside_band_pct, window_s, source, last_foreign_ts_utc, last_foreign_age_s, markers_decoded, marker_chain[, reason]}`; both ages recomputed by the server per request; the two marker counts are null without a full marker span; 404 absent; unreadable or foreign-owned = UNKNOWN |

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

**The sampler (`program_audio_sampler.py`):**
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
  - Fallback: only when a timestamp is undefined (`INT64_MAX` = `NDIlib_recv_timestamp_undefined`,
    or ≤ 0) the old arrival rule applies (`RECEIVE_GAP_S` = 1 s, logged `receive gap of … (no NDI
    sender timestamp …)`). An NDI error frame still restarts the span unconditionally (stricter
    than the fallback; 0 error frames in the 6 h live journal).
  - The 10-minute summary counts `timeline_breaks`, `late_bursts` and `receive_gaps` (fallback only),
    and reports `max_offset_ms`: the largest |offset| of a frame that continued, i.e. the sender's
    jitter against the 41.3 ms tolerance (the margin to watch);
    a restart after an NDI error frame shows as `error_frames`. Each restart and each late burst
    also logs one line.
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
      0 receive gaps,
      599 MEASUREMENT windows and only the start-up UNKNOWN.
    A probe can reuse the scratch recipe: subclass `NdiAudioReceiver.capture` to log
    `frame.timestamp`, run `program_audio_sampler.run` with a private serve dir, never the live
    one, and never restart the live unit for it.
  - **A dantesync date step** moves the sender's wall clock and so its timestamps once. A step over
    the tolerance reads as ONE discontinuity: one UNKNOWN warm-up window per step (the nightly
    1.12.0 step included), never FOREIGN, because a restarted span is never judged as a short chain.
    Micro-corrections of a few ms stay inside the tolerance. Accepted in the design.
  - Residual limits: a sender stall longer than the tolerance (OBS submitting a frame > ~20 ms
    later than its normal jitter) costs one warm-up although nothing was lost; a sender with no
    timestamps falls back to the arrival rule and its old limit.
  - Tests: the fakes use a local `_Block` with a timestamp field, so they exercise the loop, not the
    binding; `pan.AudioBlock` defaults `timestamp` to undefined, so an old fake (2 fields) runs the
    arrival fallback. `program_audio_marker_calibrate.py` stamps its blocks on a continuous
    timeline, so the bars run the same path as the service.
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
  - The lease server (`rig_serve_files.program_audio_response`) serves it as UNKNOWN with its ages
    kept. That is the one place every reader sees, restreamer's own reader included (review round 1).
  - The camera-box guard also refuses it (exit 2), for a server that still runs the old code.
  - The unit is already enabled on dev1, so the README steps are due when the checkout moves:
    restart the lease server (only while `held=false`), build the shim, restart the sampler.
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
