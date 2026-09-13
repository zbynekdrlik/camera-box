# Genlocked NDI Sender Contract

**Status:** normative. **Audience:** any project that emits NDI into the newlevel.media
broadcast chain and must lock to the fleet clock — today SongPlayer (on RESOLUME-SNV) and the
`cg` OBS, tomorrow any new sender. **Origin:** camera-box issue 1294 (owner request 2026-09-12).

This document is the single written contract for a genlocked NDI **sender**. Until now the
expectations lived only in code comments (`vendor/distroav/src/ndi-output.cpp:34-60`,
`src/ndi.rs:62-78`) and in the receiver's gate logic
(`vendor/obs-studio/libobs/obs-source.c`) — external projects could not implement what was
never written down. A sender that satisfies every MUST here locks to the fleet exactly as the
cam2 → strih → stream chain already does, with a bounded, verifiable zero-loss / correct-order
path and a LIVE-LOCKED indication.

## Normative keywords

The keywords **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**, **MAY**, and
**OPTIONAL** are used as defined in RFC 2119 / RFC 8174. They apply only where they appear in
capitals.

## Background: what the receiver does (why the contract is shaped this way)

The receiver is our vendored genlock OBS + DistroAV. These facts are load-bearing — every
sender rule below exists to satisfy them. Line numbers are re-verified against the camera-box
tree at the time of writing; treat the named symbol as authoritative if a later edit drifts the
line.

- **Timecode is the sync field.** DistroAV is forced to the certified setting
  `PROP_SYNC_NDI_SOURCE_TIMECODE` (`vendor/distroav/src/ndi-source.cpp:723` default,
  re-asserted `:1959-1960`) and maps the incoming NDI `timecode` field ×100 → the OBS frame
  `timestamp` in nanoseconds: **video** at `vendor/distroav/src/ndi-source.cpp:1680-1687` (the
  `×100` is `:1686`), **audio** at `:1619-1626` (the `×100` is `:1625`). NDI `timecode` is in
  100 ns units; OBS timestamps are in ns. The NDI `timestamp` field is NOT used in this mode.
- **Timestamp-aligned release engages only for a wall-clock timecode.** ts-align runs only when
  `genlock_is_wallclock_ts(ts)` holds — i.e. the mapped timestamp is an epoch-ns value inside
  `2020-01-01 .. 2100-01-01` (`GENLOCK_WALLCLOCK_TS_MIN_NS=1577836800000000000`,
  `GENLOCK_WALLCLOCK_TS_MAX_NS=4102444800000000000`,
  `vendor/obs-studio/libobs/obs-source.c:4735-4741`; consulted at `:5998`). A frame is released
  once it has aged to the shared deadline `present_ts = wall_now − latency_ms`
  (`genlock_present_ts_reserve`, `:4906`). `latency_ms` has a floor/default of **3 ms**
  (`#define GENLOCK_LATENCY_MS_MIN_INIT 3`, `:230`, seeded at init `:292`) and an operator
  override up to **2000 ms** (`:4594`).
  Because the release instant is the same shared wall-clock instant for every source, all
  sources that stamp the same grid are in sync by construction.
- **A non-wall-clock stamp falls back to the weak gate.** A sender that stamps
  `NDIlib_send_timecode_synthesize`, `0`, or a monotonic counter FAILS the wall-clock check and
  drops to the count gate `genlock_decide` (`:4673`) — a fixed-depth per-source jitter buffer
  that is documented as UNABLE to hold multiple sources in sync (measured ~300 ms / 9-frame
  spread, `:4715-4726`).
- **A future-dated stamp collapses the hold.** A plausible-but-FUTURE timecode (a strictly-next
  "ceil" boundary) trips the issue-147 backward-step guard — the mechanism behind the
  2026-08-07 overnight −900 ms hold collapse (issue 1009). This is exactly why the stamp MUST
  be the FLOOR boundary, never the ceil (see §4).
- **Audio timing is by arrival, not by timecode.** The receiver ASRC (issue 803 / 912) uses the
  audio ARRIVAL rate versus sample count, not the sender's audio timecodes; a sender MUST
  therefore deliver audio at real-time rate. The audio `timecode` only feeds OBS's own A/V
  pairing for that one source.
- **Audio is HELD to match the video FIFO (issue 1303).** The video FIFO holds every frame to
  `present_ts = wall_now − latency_ms`, so on the receiver the SAME source's audio is delayed by
  the same effective `latency_ms` at ingest (`source_output_audio_data`,
  `vendor/obs-studio/libobs/obs-source.c`, gated on `genlock_fifo`), so the A/V pair is presented
  at the same wall instant and survives the hold. This is a per-source PHASE hold, orthogonal to
  the ASRC servo's sample-clock RATE (ppm) discipline above — the two together are what makes the
  audio leg a first-class genlocked signal with the same evidence bar as video. The decision is
  the pure `src/genlock_audio_pairing.rs` (`genlock_audio_delay_ns` = `latency_ms`), mirrored
  byte-for-byte in the C and pinned by `tests/genlock_audio_pairing_parity.rs`. Receiver
  observability: the `genlock-fifo audit` line and `obs_genlock_stats` (v2) carry
  `audio_enabled` / `audio_delay_ms` / `audio_pairing_offset_ms` (0 = paired); a program source
  with audio disabled, an ASRC-saturated clock, or a pairing offset over one frame are the
  audio-leg DEGRADED reasons (`decide_audio_health`).

## The contract

### 1. Clock

The sending box's system clock **MUST** be disciplined by dantesync — PTP frequency to the
fleet grandmaster and NTP phase to the fleet NTP master. The sender **MUST** treat the dantesync
status document (`:8898/status`) `is_locked == true && mode ∈ {LOCK, NANO}` as the CLOCK-OK
precondition, and **MUST** degrade its lock indicator (§7) when it is not met.

Stamps **MUST** be taken from the realtime clock (`CLOCK_REALTIME` on Linux;
`GetSystemTimePreciseAsFileTime` on Windows). Scheduling **MUST** use the monotonic clock with a
monotonic→realtime offset that is re-sampled at least every ~100 emitted frames, so a realtime
clock step/slew (an NTP/PTP correction) cannot skew a stamp or a sleep.

*Reference implementation (camera-box):* `wall_clock_ns` = `CLOCK_REALTIME`
(`src/main.rs:40`); `monotonic_clock_ns` = `CLOCK_MONOTONIC` (`:57`);
`sample_mono_to_real_offset_100ns` (`:74`) re-sampled every
`OFFSET_RESAMPLE_INTERVAL_FRAMES = 100` frames (`src/genlock_stamp.rs:87`, gated by
`should_resample_mono_to_real_offset`, `:94`).

### 2. Sender create

Each NDI output **MUST** be created with `clock_video = false` and `clock_audio = false` — the
application owns the cadence; the NDI SDK's internal free-running pacing clock **MUST NOT** pace
emission. The sender **MUST** declare `frame_rate_N / frame_rate_D` as the integer grid rate
(59.94 → 60000/1001 expressed as the 60 grid, 29.97 → the 30 grid) and **MUST** emit progressive
frames. There **MUST** be exactly one `NDIlib_send_create` per output, created in a fixed order,
with stable source names and groups.

*Reference implementation (camera-box):* `clock_video = false, clock_audio = false`
(`src/ndi.rs:628-629`); progressive frame format (`NDILIB_FRAME_FORMAT_TYPE_PROGRESSIVE`,
`src/ndi.rs:1075`).

### 3. Grid

Each output **MUST** define a nominal grid rate equal to the receiving canvas rate (30 fps for
the `cg` OBS) or an integer multiple of it. Frame boundary *k* is `k · interval` on the
**Unix-epoch grid** (not a per-stream or session-relative grid). Content that arrives at a
different rate (a 23.976 or 29.97 file) **MUST** be presented at the first boundary at or after
its PTS (repeat or drop as needed) and **MUST NOT** be emitted at its free-running file rate.

### 4. Video timecode

The `NDIlib_video_frame_v2_t.timecode` of every emitted video frame **MUST** be:

```
timecode = floor(present_wall_100ns / interval_100ns) · interval_100ns
```

in **100 ns units since the Unix epoch**. It **MUST** be the FLOOR boundary (the boundary at or
before the emit instant) and **MUST NOT** be the strictly-next/ceil boundary. It **MUST NOT** be
`NDIlib_send_timecode_synthesize`, **MUST NOT** be `0`, and **MUST NOT** be a monotonic counter.

The floor is not a detail: a ceil stamp dates every frame 0..1 interval into the receiver's
future at the emit instant, leaving only network delay as margin against the receiver's
backward-step guard — the cause of the −900 ms collapse noted above.

*Reference implementation (camera-box):* `floor_boundary_100ns` (`src/ndi.rs:78`, the #1009
FLOOR-never-ceil doctrine is documented at `src/ndi.rs:62-78`), applied in
`genlock_emit_timecode_100ns` (`src/genlock_stamp.rs:52`) and stamped on the outgoing frame at
`src/ndi.rs:1079`. OBS-as-sender does the identical floor: `vendor/distroav/src/ndi-output.cpp`
stamps `video_frame.timecode = genlock_emit_timecode_100ns(...)` at `:613` (doctrine block
`:34-60`).

### 5. Pacing

The sender **MUST** emit exactly one video frame per boundary. It:

- **MUST** catch up at most one interval per emit when it is late by **≤ 8 intervals**;
- **MUST** resync forward to the next boundary (dropping the intervening boundaries) when it is
  late by **> 8 intervals with nothing queued** — a genuine wall-clock discontinuity;
- **MUST** re-latch to the rewound clock on a backward clock step (a latched boundary more than
  one interval in the future);
- **MUST**, on an underrun, repeat the last frame stamped with the NEW boundary timecode — never
  leave a hole and never emit two frames inside one interval.

*Reference implementation (camera-box):* `genlock_emit_gate` (`src/genlock_pacing.rs:69`) on the
epoch grid `now % interval`; catch-up bound `GENLOCK_MAX_CATCHUP_INTERVALS = 8` (`:67`);
backward-step re-latch (`:82-90`, `genlock_latched_boundary` `:134`); starvation repeat with the
new boundary timecode (`starvation_repeat_timecode_100ns` `:239`).

### 6. Audio

Audio **MUST** be sent on the same sender. It **MUST** be 48 kHz, planar float. The chunk(s) for
a boundary **MUST** be submitted BEFORE that boundary's video frame. The audio `timecode`
**MUST** be the raw wall clock at submission (in 100 ns units, with no boundary snap). Samples
**MUST** be delivered at real-time rate (`samples_per_boundary = 48000 · interval_seconds`); a
sustained file-clock vs wall-clock residual greater than **±50 ppm MUST** be resampled on the
sender (the receiver ASRC absorbs less).

*Reference implementation (camera-box OBS-as-sender):* `audio_frame.timecode =
genlock_wall_now_100ns()` (raw wall clock, no snap) at `vendor/distroav/src/ndi-output.cpp:697`.

### 7. Observability

The sender **MUST** publish a 1 Hz health document per output (an HTTP endpoint is REQUIRED)
carrying at least:

- `lock_state ∈ {LOCKED, DEGRADED, UNLOCKED}` plus a human `reason`;
- `clock { is_locked, mode, offset_ns, ntp_age_s }`;
- `pacing { seq, late_frames, max_late_us, jitter_p99_us, repeats, resyncs }`;
- `receiver { connections }`.

The application UI **MUST** render a visible LIVE-LOCKED badge using the SAME three-state
vocabulary (`LOCKED` / `DEGRADED` / `UNLOCKED`) as the OBS indicator, so an operator reads one
consistent signal across every surface.

### 8. Acceptance

Acceptance **MUST** be measured as ground truth on the RECEIVER, never from the sender's own
counters. On the receiving genlock OBS, for the input under test:

- the `genlock-fifo audit` line **MUST** show `locked=1` and
  `underruns = dropped_due = relocks = late_holds = backward_steps = 0` sustained over **≥ 1
  hour**;
- `ts_head_skew_ms` **MUST** stay within **±20 ms** at latency 3 ms over that window;
- there **MUST** be no drift over **24 hours**;
- where the burn overlay is enabled, `recording-verdict` burn-id contiguity and ordering across
  the chain **MUST** hold.

*Reference implementation (camera-box):* the raw `genlock-fifo audit` counters
(`locked`, `underruns`, `backward_steps`, `dropped_due`, `relocks`, `late_holds`) are parsed in
`src/jitter_audit.rs:41-52`; the pass/fail verdict that encodes these exact criteria is
`src/resolume_playback.rs` `evaluate()` (`:91`) — `skew_bound_ms = 20` (`:46`, `:56`), the
per-window pathology-delta checks `delta_{dropped_due, underruns, relocks, late_holds}`
(`:71-74`) and `delta_backward_regime_ticks` (`:75`), with a minimum-sample guard so a too-short
window cannot read as "flat" (`:47`).

## Reference implementation index

| Rule | Receiver / sender fact | Location (re-verify the symbol, not the line) |
| --- | --- | --- |
| §1 Clock | realtime + monotonic + 100-frame offset resample | `src/main.rs:40,57,74`; `src/genlock_stamp.rs:87,94` |
| §2 Create | `clock_video/clock_audio=false`, progressive | `src/ndi.rs:628-629,1075` |
| §4 Timecode | FLOOR boundary, 100 ns epoch | `src/ndi.rs:78,1079` (doctrine `:62-78`); `src/genlock_stamp.rs:52`; `vendor/distroav/src/ndi-output.cpp:613` (doctrine `:34-60`) |
| §5 Pacing | grid gate, catch-up ≤ 8, resync, re-latch, repeat | `src/genlock_pacing.rs:67,69,82-90,134,239` |
| §6 Audio | raw wall-clock timecode, no snap | `vendor/distroav/src/ndi-output.cpp:697` |
| §8 Acceptance | `genlock-fifo audit` counters + verdict | `src/jitter_audit.rs:41-52`; `src/resolume_playback.rs:46,47,56,71-75,91` |
| Receiver gate | `PROP_SYNC_NDI_SOURCE_TIMECODE` ×100 → ns | `vendor/distroav/src/ndi-source.cpp:723,1619-1626,1680-1687,1959-1960` |
| Receiver gate | `genlock_is_wallclock_ts` epoch bounds | `vendor/obs-studio/libobs/obs-source.c:4735-4741` |
| Receiver gate | `present_ts = wall_now − latency_ms`, floor 3 ms / ≤ 2000 ms | `vendor/obs-studio/libobs/obs-source.c:230,292,4594,4906` |
| Receiver gate | weak count gate (~300 ms spread) when stamp is not wall-clock | `vendor/obs-studio/libobs/obs-source.c:4673,4715-4726` |

## Per-box forced-table audit — audio + colour (#1303 part 4)

The receiver's DistroAV fork forces a certified `GENLOCK_FORCED_SETTINGS` table on every NDI input
at every `ndi_source_update` (`vendor/distroav/src/ndi-source.cpp`). Those values were read off a
CAMERA input on strih (where audio is irrelevant), so `ndi_audio` was forced `false` fleet-wide.
On a box whose NDI inputs carry PROGRAM audio (the cg OBS on RESOLUME-SNV, whose `sp-*_video`
inputs carry SongPlayer audio+video) that certified `false` silently disabled program audio and
only surfaced on air (2026-09-13 event morning). `43de2f16f` moved `PROP_AUDIO` back to the
per-source whitelist (stock default true), so the audio knob is operator-visible again — but a
genlock build onboarded onto a NEW box needs its forced table reviewed against THAT box's sources
BEFORE the swap, because the certified values were derived from camera inputs only.

The per-box-class AUDIO expectation (the canonical table is
`src/genlock_forced_table_audit.rs`; the deploy-preflight bash replica is
`scripts/lib/genlock-forced-table-audit.sh`, pinned byte-for-byte by
`tests/genlock_forced_table_audit_1303.rs`):

| Box class | Role | Camera inputs (`CAM* (usb)`) | Program / music / SongPlayer inputs (`sp-*`, `cg`, `NDI 2ME PGM`, `mbc`, `NDI obs hudba`, `NDIAr *`, `VBAN *`) | Unknown-name default |
|---|---|---|---|---|
| strih | camera switcher | `ndi_audio=false` (expected-silent) | `ndi_audio=true` (expected-audio: the `cg` program input) | silent |
| stream | program encoder | expected-silent | expected-audio (`NDI 2ME PGM` program + `mbc` / music) | silent |
| imag | 60fps projection | expected-silent | expected-audio | silent |
| resolume | cg OBS | expected-silent | expected-audio (SongPlayer `sp-*`, `NDIAr`, `VBAN`) | audio |

A MISMATCH is either an expected-audio source with `ndi_audio=false` (the #1303 live defect: silent
program audio) or an expected-silent camera source with `ndi_audio=true` (audio bleeding into the
camera chain). A program source with a forced `yuv_range=partial` gets a report-only advisory (a
full-range sender then colour-shifts — the owner's "distorted picture" secondary symptom). The
audit is REPORT-ONLY: `scripts/deploy-genlock-fleet.sh` prints it BEFORE the swap (never a write,
never a gate) so the operator sets `ndi_audio`/`yuv_*` per source over OBS-WS before deploying; it
never blocks the deploy.
