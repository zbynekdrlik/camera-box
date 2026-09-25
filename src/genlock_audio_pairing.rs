//! #1303 + issue 1367 — the pure receiver-side AUDIO ↔ video-FIFO pairing decision for a
//! genlocked NDI source.
//!
//! ## What the video does, and what the audio must do
//!
//! A `genlock_fifo` source releases each frame once its NDI stamp is due against the wall clock
//! (`vendor/obs-studio/libobs/obs-source.c`). The frame a tick presents reaches the program at
//! `stamp + (tick − presented stamp)`, its age at the render tick. That age is the source's REAL
//! stamp→present delay. On a canvas-rate source the presented frame is the queue head; on a source
//! at N ≥ 2 × the canvas rate it is the NEWEST matured frame, so the head would over-read by
//! (N − 1) source intervals. The delay is `latency_ms` only on a source whose FIFO is exactly one
//! pin deep. A shallow cg feed sits 2–3 frames deep at a 3 ms pin, which makes its delay 60–100 ms
//! (the win-resolume `ts_head_skew_ms=97` at `latency_ms=3`).
//!
//! #1303 held the audio by the fixed `latency_ms` on its ARRIVAL clock. Audio therefore led video by
//! `delay − latency_ms − arrival lag`, about 94 ms on resolume, and `audio_pairing_offset_ms` was
//! computed against the same `latency_ms` and read 0.
//!
//! Issue 1367 (Option 3) makes the audio follow the video's MEASURED delay:
//!
//! 1. **Measure.** Every ts-align tick that PRESENTS a frame samples
//!    `tick_scheduled_wall − presented_stamp` ([`video_delay_sample_ns`]; the scheduled instant, so
//!    a late tick's processing lag is not counted). A tick off the per-second grid (a wall-clock
//!    step slewing back) is not sampled. [`video_delay_track`] smooths it with an EMA
//!    ([`VIDEO_DELAY_EMA_SHIFT`]).
//! 2. **Quantize with hysteresis.** A change of the smoothed delay by half a frame or more
//!    ([`video_delay_moved`]) ARMS a re-application. The new whole-ms delay
//!    ([`video_delay_round_ms`]) is applied once the EMA has settled ([`VIDEO_DELAY_SETTLE_TICKS`]).
//!    Applying at the crossing itself would latch the audio half-way through a one-frame step, and
//!    the half-frame hysteresis would never correct it afterwards.
//! 3. **Place.** The audio ingest ([`audio_hold_mode`] / [`audio_place_term_ns`]) places a packet
//!    with timecode `tc` (the same wall-epoch basis DistroAV stamps the video with) at the OBS
//!    monotonic instant `tc + off_live + delay`. `off_live = mono_now − wall_now` is read on EVERY
//!    packet that needs it ([`audio_wall_to_mono_ns`], [`audio_needs_live_offset`]), never latched,
//!    because the wall and QPC clocks drift apart (318 ms on resolume). Packets that follow append
//!    back to back. The genlock ASRC rate servo (disciplined against the wall clock) and its level
//!    loop then hold the captured depth, so the drift is absorbed physically between placements.
//! 4. **Slew, never step (ROZHODNUTÉ 5827497952).** A shallow N==1 source's audio follows its
//!    LATCHED per-lock video depth ([`video_delay_lock_ms`]), so its hold is constant between
//!    relocks. A change of the hold while audio PLAYS (a relock with a new depth, the late
//!    latency→timecode switch) is SLEWED ([`audio_hold_action`] → [`AudioHoldAction::Slew`]): the
//!    packets keep appending back to back and the ASRC resampler stretches or compresses at
//!    [`AUDIO_SLEW_PPM`] until the term delta is paid ([`audio_slew_step_ns`]), the level target
//!    moving with each step. The old immediate re-placement was the audible dropout the songplayer
//!    gate measured. A first placement and a timeline discontinuity still PLACE
//!    ([`audio_level_shift_ns`]); a source with no ASRC resampler keeps the legacy step.
//! 5. **Observe.** `audio_pairing_offset_ms` is the applied audio delay minus the MEASURED video
//!    delay ([`pairing_offset_ms`] with [`video_delay_reference_ns`]). The health verdict flags it
//!    above half a frame ([`decide_audio_health`]). It is a PROXY: it compares the applied hold with
//!    the measured delay and never observes where the audio samples actually sit, so a wrong
//!    placement, or a depth the rate servo walked, would still read 0.
//!
//! Until the first video delay is known, a wall-clock-timecoded source's audio is WITHHELD
//! ([`AudioHoldMode::Pending`]) so its first placement already lands on the right delay, for at
//! most [`AUDIO_WITHHOLD_MAX_NS`]; after that, and for an audio timestamp that is not a wall-clock
//! timecode, the ingest keeps the #1303 behaviour: arrival basis + `latency_ms`
//! ([`AudioHoldMode::Latency`]).
//!
//! This is orthogonal to the ASRC servo (#803/#912/#1084), which disciplines the audio
//! sample-clock RATE (ppm). This module owns the PHASE (where the audio is placed).
//!
//! ## Why crate-root + pure `std`
//!
//! The whole `probe` module is `#[cfg(feature = "probe")]` (pulls image/qr/drm deps that balloon
//! the shared `target/`, per the Local Build Policy). This decision needs none of that, so it
//! lives here as a pure module — the exact `src/genlock_lock_state.rs` / `src/genlock_backlog.rs`
//! pattern: it unit-tests Tier-0 (standalone-rustc,
//! `.claude/rules/vendored-libobs-change-safety.md` §"pure-std crate-root module"), and its C
//! mirror (the contiguous `genlock_audio_*` / `genlock_video_delay_*` `static inline` block in
//! `obs-source.c`) is held byte-identical by the committed parity gate
//! `tests/genlock_audio_pairing_parity.rs` (the #1003 lift-and-compile recipe). The two-clock
//! bench proving |A/V| ≤ 5 ms is the test-only `genlock_audio_pairing_bench.rs` sibling.

/// Nanoseconds per millisecond (the audit line + OBS timestamps are in ns; the operator knob is
/// in ms).
pub const NS_PER_MS: u64 = 1_000_000;

/// The EMA weight of one new stamp→present delay sample, as a right shift: `smoothed += (sample −
/// smoothed) / 2^SHIFT`. 3 = 1/8, a time constant of 8 render ticks (0.27 s at 30 fps). Single
/// held/shed ticks move the EMA by only an eighth of a frame, well inside the half-frame
/// hysteresis. Mirror of `GENLOCK_VIDEO_DELAY_EMA_SHIFT`.
pub const VIDEO_DELAY_EMA_SHIFT: u32 = 3;

/// The render ticks a re-application waits after the half-frame trigger fires before it applies the
/// smoothed delay. 64 ticks = 8 EMA time constants: a one-frame step has converged to
/// `33.3 ms · (7/8)^64 ≈ 6 µs`, so the applied value is the NEW delay, not a mid-step one. 2.1 s at
/// 30 fps, 1.1 s at 60 fps. Mirror of `GENLOCK_VIDEO_DELAY_SETTLE_TICKS`.
pub const VIDEO_DELAY_SETTLE_TICKS: u32 = 64;

/// The per-source audio hold, in nanoseconds, for a hold of `hold_ms`. Used for the #1303
/// latency-mode hold (`hold_ms = latency_ms`) and to express any applied hold in ns.
///
/// Mirror of `genlock_audio_present_delay_ns` in `obs-source.c`.
pub fn genlock_audio_delay_ns(hold_ms: u32) -> u64 {
    hold_ms as u64 * NS_PER_MS
}

/// One stamp→present delay sample: the PRESENTED frame's age at the render tick's SCHEDULED wall
/// instant (`genlock_n1_tick_wall_now`). Clamped to at least 1 ns, so a frame stamped at or after
/// the tick never produces the `0` "unseeded" sentinel of [`video_delay_smooth_ns`] (which would
/// re-seed the EMA on every such tick).
///
/// Mirror of `genlock_video_delay_sample_ns`.
pub fn video_delay_sample_ns(tick_wall_ns: u64, presented_stamp_ns: u64) -> u64 {
    tick_wall_ns.saturating_sub(presented_stamp_ns).max(1)
}

/// One EMA step of the smoothed stamp→present delay. `0` is the unseeded sentinel: the first sample
/// seeds the EMA. The step truncates toward zero, identically in C (`int64_t` division), and every
/// sum wraps like the C `uint64_t` arithmetic (no signed overflow at any input). With samples ≥ 1
/// (the [`video_delay_sample_ns`] clamp) a seeded EMA never returns to 0.
///
/// Mirror of `genlock_video_delay_smooth_ns`.
pub fn video_delay_smooth_ns(smoothed_ns: u64, sample_ns: u64) -> u64 {
    if smoothed_ns == 0 {
        return sample_ns;
    }
    let diff = sample_ns.wrapping_sub(smoothed_ns) as i64;
    smoothed_ns.wrapping_add((diff / (1i64 << VIDEO_DELAY_EMA_SHIFT)) as u64)
}

/// The smoothed delay rounded to whole ms, never 0 (`0` means "no delay applied yet").
///
/// Mirror of `genlock_video_delay_round_ms`.
pub fn video_delay_round_ms(smoothed_ns: u64) -> u32 {
    let ms = smoothed_ns.saturating_add(NS_PER_MS / 2) / NS_PER_MS;
    if ms == 0 {
        1
    } else if ms > u32::MAX as u64 {
        u32::MAX
    } else {
        ms as u32
    }
}

/// Has the smoothed delay moved by HALF A FRAME or more from the applied one (or is nothing applied
/// yet)? `2·|smoothed − applied| ≥ interval`. An unknown interval (0) never counts as a move.
///
/// Mirror of `genlock_video_delay_moved`.
pub fn video_delay_moved(applied_ms: u32, smoothed_ns: u64, interval_ns: u64) -> bool {
    if applied_ms == 0 {
        return true;
    }
    if interval_ns == 0 {
        return false;
    }
    let applied_ns = applied_ms as u64 * NS_PER_MS;
    let diff = smoothed_ns.abs_diff(applied_ns);
    diff.saturating_mul(2) >= interval_ns
}

/// The per-source video-delay tracker state (the three `obs_source` fields
/// `genlock_video_delay_smoothed_ns` / `_applied_ms` / `_settle_ticks`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoDelayTracker {
    /// The EMA of the stamp→present delay, ns (0 = unseeded).
    pub smoothed_ns: u64,
    /// The quantized delay the audio follows, ms (0 = not measured yet → the #1303 latency hold).
    pub applied_ms: u32,
    /// Render ticks left until an armed re-application applies (0 = idle). Under a lock it counts
    /// UP the consecutive ticks the realized delay sat half a frame or more off the applied one.
    pub settle_ticks: u32,
    /// Issue 1367 (design 5830750134) — the lock last applied (0 = the free tracker): a NEW lock
    /// applies at once, the same lock lets the realized delay bound the hold.
    pub locked_ms: u32,
}

/// Issue 1367 (design 5830750134) — under the SAME lock, the render ticks the smoothed realized
/// delay must stay half a frame or more off the applied hold, CONSECUTIVELY, before the hold
/// follows it. Longer than the hold's climb onto a capped latched depth (3 holds × the 30-tick
/// throttle = 90 ticks), so a normal climb onto D never moves the audio; a disturbance's one-frame
/// excursion (≤ one throttle window) resets it. Mirror of `GENLOCK_VIDEO_DELAY_FOLLOW_TICKS`.
pub const VIDEO_DELAY_FOLLOW_TICKS: u32 = 180;

/// Issue 1367 (ROZHODNUTÉ 5827497952) — the `lock_ms` of [`video_delay_track`] while a SHALLOW N==1
/// source measures its first per-lock depth: smooth only, apply nothing, so the audio waits for the
/// latched depth instead of following the floating one. Mirror of `GENLOCK_VIDEO_DELAY_LOCK_PENDING`.
pub const VIDEO_DELAY_LOCK_PENDING: u32 = u32::MAX;

/// Issue 1367 — the `lock_ms` the render thread passes to [`video_delay_track`] for an N==1 source
/// with the shallow per-lock depth state (`genlock_n1_depth::ShallowDepth`): a latched depth D →
/// `round(D · interval)` ms (the audio follows the LOCKED video delay, constant until the next
/// relock); no D yet but a window measuring → [`VIDEO_DELAY_LOCK_PENDING`]; otherwise 0 (the free
/// Option-3 tracker, e.g. an N>=2 source). Mirror of `genlock_video_delay_lock_ms`.
pub fn video_delay_lock_ms(target_frames: u64, measuring: bool, interval_ns: u64) -> u32 {
    if target_frames != 0 && interval_ns != 0 {
        video_delay_round_ms(target_frames.saturating_mul(interval_ns))
            .min(VIDEO_DELAY_LOCK_PENDING - 1)
    } else if measuring {
        VIDEO_DELAY_LOCK_PENDING
    } else {
        0
    }
}

/// One render tick of the tracker: smooth the sample; an idle tracker ARMS a settle countdown when
/// the smoothed delay has moved half a frame or more; an armed one counts down and, at 0, applies
/// the rounded smoothed delay if it is STILL half a frame or more away (a transient that reversed
/// during the settle applies nothing, so the audio is never re-placed for a sub-half-frame change).
///
/// Issue 1367 (ROZHODNUTÉ 5827497952): `lock_ms` ([`video_delay_lock_ms`]) overrides the apply —
/// `0` = the free tracker above; [`VIDEO_DELAY_LOCK_PENDING`] = smooth only, apply nothing; any other
/// value = the LOCKED delay of a shallow source's latched depth, applied as-is (the EMA keeps
/// running for the audit's `video_delay_ms=`).
///
/// Mirror of `genlock_video_delay_track`.
pub fn video_delay_track(
    t: &mut VideoDelayTracker,
    lock_ms: u32,
    sample_ns: u64,
    interval_ns: u64,
) {
    t.smoothed_ns = video_delay_smooth_ns(t.smoothed_ns, sample_ns);
    if lock_ms != 0 {
        t.settle_ticks = 0;
        if lock_ms != VIDEO_DELAY_LOCK_PENDING {
            t.applied_ms = lock_ms;
        }
        return;
    }
    if t.settle_ticks > 0 {
        t.settle_ticks -= 1;
        if t.settle_ticks == 0 && video_delay_moved(t.applied_ms, t.smoothed_ns, interval_ns) {
            t.applied_ms = video_delay_round_ms(t.smoothed_ns);
        }
    } else if video_delay_moved(t.applied_ms, t.smoothed_ns, interval_ns) {
        t.settle_ticks = VIDEO_DELAY_SETTLE_TICKS;
    }
}

/// How a source's audio is held. Discriminants match the C `GENLOCK_AUDIO_HOLD_*` defines.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioHoldMode {
    /// Not a genlock source: no hold.
    Off = 0,
    /// The #1303 hold: arrival basis + `latency_ms` (before the first measurement settles, or for
    /// an audio timestamp that is not a wall-clock timecode).
    Latency = 1,
    /// Issue 1367: timecode → wall → OBS clock through the live offset + the measured video delay.
    Timecode = 2,
    /// Issue 1367 (ROZHODNUTÉ 5827497952): a timecode-capable genlock source whose video delay is
    /// not known yet — its audio is WITHHELD (not placed) so the first placement already lands on
    /// the right delay, at most [`AUDIO_WITHHOLD_MAX_NS`] after its first packet.
    Pending = 3,
}

impl AudioHoldMode {
    /// The integer the C helpers use.
    pub fn code(self) -> u8 {
        self as u8
    }
    /// The audit-line token (`audio_hold=`).
    pub fn token(self) -> &'static str {
        match self {
            AudioHoldMode::Off => "off",
            AudioHoldMode::Latency => "latency",
            AudioHoldMode::Timecode => "timecode",
            AudioHoldMode::Pending => "pending",
        }
    }
    /// Is audio PLAYING under this mode (placed into the mix)? Off plays unheld; Pending plays
    /// nothing.
    pub fn is_active(self) -> bool {
        matches!(self, AudioHoldMode::Latency | AudioHoldMode::Timecode)
    }
}

/// Issue 1367 — how long a timecode-capable genlock source's audio is withheld at most while its
/// video delay is unknown (a box whose ticks never land on the grid never measures one). After it,
/// the #1303 latency hold plays, and a later video delay is SLEWED in. Mirror of
/// `GENLOCK_AUDIO_WITHHOLD_MAX_NS`.
pub const AUDIO_WITHHOLD_MAX_NS: u64 = 10_000_000_000;

/// Issue 1367 — has the withhold window of a source whose first genlock audio packet arrived at
/// `first_packet_ns` (OBS monotonic; 0 = none yet) run out at `now_ns`? Mirror of
/// `genlock_audio_withhold_expired`.
pub fn audio_withhold_expired(first_packet_ns: u64, now_ns: u64) -> bool {
    first_packet_ns != 0 && now_ns.saturating_sub(first_packet_ns) >= AUDIO_WITHHOLD_MAX_NS
}

/// Pick the hold for one audio packet. `latency_ms == 0` is the unreachable floor-violating value
/// (the pin is seeded ≥ 3 ms) and holds nothing, as in #1303. Issue 1367: a wall-clock-timecoded
/// source with no video delay yet is WITHHELD ([`AudioHoldMode::Pending`]) until
/// [`audio_withhold_expired`], then falls back to the latency hold.
///
/// Mirror of `genlock_audio_hold_mode`.
pub fn audio_hold_mode(
    genlock_fifo: bool,
    latency_ms: u32,
    audio_ts_is_wallclock: bool,
    video_delay_ms: u32,
    withhold_expired: bool,
) -> AudioHoldMode {
    if !genlock_fifo || latency_ms == 0 {
        AudioHoldMode::Off
    } else if audio_ts_is_wallclock && video_delay_ms > 0 {
        AudioHoldMode::Timecode
    } else if audio_ts_is_wallclock && !withhold_expired {
        AudioHoldMode::Pending
    } else {
        AudioHoldMode::Latency
    }
}

/// The hold (ms) a mode applies — reported as `audio_delay_ms=`.
///
/// Mirror of `genlock_audio_hold_ms`.
pub fn audio_hold_ms(mode: AudioHoldMode, latency_ms: u32, video_delay_ms: u32) -> u32 {
    match mode {
        AudioHoldMode::Off | AudioHoldMode::Pending => 0,
        AudioHoldMode::Latency => latency_ms,
        AudioHoldMode::Timecode => video_delay_ms,
    }
}

/// Does this packet need the live wall→mono offset? Only when the new or the previous hold is the
/// timecode placement (its term, or the re-placement shift's previous term, contains the offset).
/// Every other audio source (a mic, desktop audio, a latency-mode genlock source) skips the two
/// clock reads.
///
/// Mirror of `genlock_audio_needs_live_offset`.
pub fn audio_needs_live_offset(mode: AudioHoldMode, prev_mode: AudioHoldMode) -> bool {
    mode == AudioHoldMode::Timecode || prev_mode == AudioHoldMode::Timecode
}

/// The live wall→OBS-monotonic offset: `mono_now − wall_now` (two's-complement, so a monotonic
/// clock far below the wall epoch gives the right negative value). Read on every packet.
///
/// Mirror of `genlock_audio_wall_to_mono_ns`.
pub fn audio_wall_to_mono_ns(mono_now_ns: u64, wall_now_ns: u64) -> i64 {
    mono_now_ns.wrapping_sub(wall_now_ns) as i64
}

/// The term added to the audio packet's timestamp AFTER the #1303 ingest's `+ timing_adjust`
/// (and the sync-offset / resample-offset adjusts). `in.timestamp` there is `tc + timing_adjust`,
/// so:
///
/// - `Off` → 0;
/// - `Latency` → `hold`, placing the packet at `arrival + hold` (the #1303 arrival basis);
/// - `Timecode` → `off_live + hold − timing_adjust`, placing the packet at `tc + off_live + hold`:
///   the OBS monotonic instant of the wall instant the same-timecode video frame presents at.
///
/// All arithmetic wraps like the C `uint64_t`/`int64_t` sums.
///
/// Mirror of `genlock_audio_place_term_ns`.
pub fn audio_place_term_ns(
    mode: AudioHoldMode,
    hold_ms: u32,
    off_live_ns: i64,
    timing_adjust_ns: u64,
) -> i64 {
    let hold_ns = genlock_audio_delay_ns(hold_ms) as i64;
    match mode {
        AudioHoldMode::Off | AudioHoldMode::Pending => 0,
        AudioHoldMode::Latency => hold_ns,
        AudioHoldMode::Timecode => off_live_ns
            .wrapping_add(hold_ns)
            .wrapping_sub(timing_adjust_ns as i64),
    }
}

/// Issue 1367 (ROZHODNUTÉ 5827497952) — what the audio ingest does with one packet. Discriminants
/// match the C `GENLOCK_AUDIO_ACT_*` defines.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioHoldAction {
    /// Drop the packet: the hold is [`AudioHoldMode::Pending`].
    Withhold = 0,
    /// A genuine (re)placement: the first placement after a withhold or genlock toggle, or a
    /// timeline discontinuity. Places at the full new term and settles any slew.
    Place = 1,
    /// Nothing changed: append as the ingest decided.
    Continue = 2,
    /// The hold changed while audio plays: keep appending back to back and SLEW the placement by
    /// the term delta through the ASRC resampler ([`audio_slew_step_ns`]).
    Slew = 3,
    /// The hold changed but the source has no ASRC resampler to slew with: the legacy step
    /// re-placement. Counted as an audible STEP.
    Replace = 4,
}

impl AudioHoldAction {
    /// The integer the C helpers use.
    pub fn code(self) -> u8 {
        self as u8
    }
}

/// Issue 1367 — decide one packet. `continuous`: the ingest's own continuity verdict (`push_back`)
/// before any hold logic. `can_slew`: the source's ASRC resampler is active. `slew_pending`: a slew
/// has not finished yet. A hold change never STEPS while audio plays unless the source cannot slew.
///
/// Mirror of `genlock_audio_hold_action`.
pub fn audio_hold_action(
    prev_mode: AudioHoldMode,
    prev_hold_ms: u32,
    mode: AudioHoldMode,
    hold_ms: u32,
    continuous: bool,
    can_slew: bool,
    slew_pending: bool,
) -> AudioHoldAction {
    if mode == AudioHoldMode::Pending {
        return AudioHoldAction::Withhold;
    }
    let changed = mode != prev_mode || hold_ms != prev_hold_ms;
    if !changed {
        if slew_pending && (!continuous || !can_slew) {
            return AudioHoldAction::Place;
        }
        return AudioHoldAction::Continue;
    }
    if !prev_mode.is_active() || !mode.is_active() || !continuous {
        return AudioHoldAction::Place;
    }
    if can_slew {
        AudioHoldAction::Slew
    } else {
        AudioHoldAction::Replace
    }
}

/// Issue 1367 — the ASRC level-target shift (ns) a (re)placement moves the buffer by: the new term
/// minus the effective previous placement (the previous term minus the slew still owed), only when
/// audio was playing before (a first placement has no captured level to move). Every other action
/// shifts nothing at once (a slew shifts the target step by step with each consumed increment).
///
/// Mirror of `genlock_audio_level_shift_ns`.
pub fn audio_level_shift_ns(
    action: AudioHoldAction,
    prev_mode: AudioHoldMode,
    new_term_ns: i64,
    prev_term_ns: i64,
    slew_remaining_ns: i64,
) -> i64 {
    match action {
        AudioHoldAction::Place | AudioHoldAction::Replace if prev_mode.is_active() => new_term_ns
            .wrapping_sub(prev_term_ns)
            .wrapping_add(slew_remaining_ns),
        _ => 0,
    }
}

/// Issue 1367 — the ASRC-rate SLEW of the audio placement: 1000 ppm (0.1 %, ~1.7 cents — the NTSC
/// pull-down magnitude, inaudible), i.e. 1 ms of placement per second. A one-frame (33 ms) relock
/// change settles in 33 s. Mirror of `GENLOCK_AUDIO_SLEW_PPM`.
pub const AUDIO_SLEW_PPM: u64 = 1000;

/// Issue 1367 — the placement slew one audio callback of `dt_ns` consumes from `remaining_ns`
/// (signed: positive = the audio moves LATER, the resampler stretches): `remaining` clamped to
/// `±dt · AUDIO_SLEW_PPM / 1e6`. Mirror of `genlock_audio_slew_step_ns`.
pub fn audio_slew_step_ns(remaining_ns: i64, dt_ns: u64) -> i64 {
    let cap = (dt_ns.saturating_mul(AUDIO_SLEW_PPM) / 1_000_000).min(i64::MAX as u64) as i64;
    remaining_ns.clamp(-cap, cap)
}

/// Issue 1367 — the resampler ppm of one slew step (`step · 1e6 / dt`, 0 for an empty callback).
/// Positive = stretch (the swresample-native sign). Mirror of `genlock_audio_slew_ppm`.
pub fn audio_slew_ppm(step_ns: i64, dt_ns: u64) -> f64 {
    if dt_ns == 0 {
        0.0
    } else {
        step_ns as f64 * 1e6 / dt_ns as f64
    }
}

/// Issue 1367 — BOOK one consumed slew step out of the ingest's smoothing timeline
/// (`next_audio_ts_min`): the resampler stretched this packet by `step_ns` (positive = more
/// samples, the audio later), a deliberate placement move and not source time, so the next expected
/// source timestamp is `next_ts_min − step`. Without it a slew over 70 ms walks the smoothing
/// timeline past `TS_SMOOTHING_THRESHOLD` and the ingest snaps the audio back to the old placement.
/// Wraps like the C `uint64_t` arithmetic. Mirror of `genlock_audio_slew_book_ts_ns`.
pub fn audio_slew_book_ts_ns(next_ts_min_ns: u64, step_ns: i64) -> u64 {
    next_ts_min_ns.wrapping_sub(step_ns as u64)
}

/// Issue 1367 (review round 1) — the slew still owed when the ingest PLACED a packet anyway
/// (`push_back` turned false after the action was decided: a sync-offset change, or no `audio_ts`
/// yet): the placement lands at the full new term, so the owed amount is paid at once and must be
/// folded into the level setpoint and cleared, or the resampler would keep stretching past it.
/// Only for an action that left a slew owed (`Continue` / `Slew`); `Place` / `Replace` already
/// settled it, `Withhold` placed nothing. Returns the level shift (ns) to apply and clear.
/// Mirror of `genlock_audio_placed_slew_fold_ns`.
pub fn audio_placed_slew_fold_ns(
    action: AudioHoldAction,
    placed: bool,
    slew_remaining_ns: i64,
) -> i64 {
    match action {
        AudioHoldAction::Continue | AudioHoldAction::Slew if placed => slew_remaining_ns,
        _ => 0,
    }
}

/// The video delay the pairing offset is measured against: the smoothed MEASURED stamp→present
/// delay when there is one, else the nominal `latency_ms` hold.
///
/// Mirror of `genlock_audio_video_delay_ref_ns`.
pub fn video_delay_reference_ns(smoothed_ns: u64, latency_ms: u32) -> i64 {
    if smoothed_ns > 0 {
        smoothed_ns as i64
    } else {
        genlock_audio_delay_ns(latency_ms) as i64
    }
}

/// The residual A/V pairing offset, in ms (truncated toward zero), between the audio hold actually
/// applied and the video's stamp→present delay: `(applied_audio_delay_ns − video_delay_ns)/1e6`.
/// Zero = paired. Signed (positive = audio held LONGER than the video). An audio source that never
/// held (`applied = 0`) reads `−video delay`.
///
/// Mirror of `genlock_audio_pairing_offset_ms` in `obs-source.c`.
pub fn pairing_offset_ms(applied_audio_delay_ns: i64, video_delay_ns: i64) -> i64 {
    applied_audio_delay_ns.wrapping_sub(video_delay_ns) / NS_PER_MS as i64
}

/// Issue 1367 (review round 2) — where the audio actually sits: the applied hold minus the slew it
/// still owes (a hold change is slewed in at [`AUDIO_SLEW_PPM`], so mid-slew the audio is not yet at
/// the new hold). The pairing offset's audio side, so a slew still owing 33 ms reads −33, not 0.
/// Mirror of `genlock_audio_applied_delay_ns`.
pub fn audio_applied_delay_ns(hold_ms: u32, slew_remaining_ns: i64) -> i64 {
    genlock_audio_delay_ns(hold_ms).wrapping_sub(slew_remaining_ns as u64) as i64
}

/// The audio-parity health of one genlocked source — the reason the LOCK indicator DEGRADES on the
/// audio axis (mirrors the video-side `LockReason` discriminant model). Discriminants match the C
/// `genlock_audio_health` enum and are compared as `u8` by the parity gate.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPairingHealth {
    /// Audio is paired within bound (or the source legitimately carries no audio and is not a
    /// program source — a camera input with `ndi_audio=false` is NOT a fault).
    Ok = 0,
    /// A PROGRAM-feeding source has NDI audio disabled — the audio leg is dark where it must not
    /// be (the acceptance criterion: "the indicator turns DEGRADED when audio is disabled on a
    /// program source").
    AudioDisabledOnProgram = 1,
    /// The ASRC servo is saturated (`|estimated ppm|` at/over the clamp) — the audio sample clock
    /// cannot be disciplined onto the wall clock, so the pairing is not trustworthy.
    AsrcSaturated = 2,
    /// The residual `|pairing_offset_ms|` exceeds HALF a frame interval (issue 1367; it was one
    /// frame while the offset was measured against the pin) — the audio is off the held video by
    /// more than the re-application hysteresis allows.
    PairingOffsetExceeded = 3,
}

impl AudioPairingHealth {
    /// The integer the C `genlock_audio_decide_health` returns — used by the parity gate.
    pub fn code(self) -> u8 {
        self as u8
    }
}

/// The scalarised inputs to the audio-parity health decision — every field a plain scalar so the C
/// mirror is a byte-for-byte port (no floats: the ASRC-saturation float comparison is reduced to a
/// bool by the caller/widget, keeping the parity'd decision integer-exact).
#[derive(Debug, Clone, Copy)]
pub struct AudioPairingFacets {
    /// This source's NDI audio is enabled (`ndi_audio` / `obs_source_audio_active`).
    pub audio_enabled: bool,
    /// This source feeds the program (so its audio being off IS a fault). A monitoring-only /
    /// non-program source with audio off is legitimately silent.
    pub is_program_source: bool,
    /// The ASRC servo is saturated for this source (`|estimated_ppm| >= ASRC_MAX_PPM`, computed
    /// upstream where the float lives). Only meaningful when `audio_enabled`.
    pub asrc_saturated: bool,
    /// The residual pairing offset (ms, signed) from [`pairing_offset_ms`].
    pub pairing_offset_ms: i64,
    /// One frame interval in ms (33 at 30 fps, 16 at 60 fps). The bound is half of it.
    pub frame_interval_ms: i64,
}

/// Decide the audio-parity health from the scalarised facets. Precedence:
/// audio-disabled-on-a-program-source > asrc-saturated > pairing-offset-exceeded > Ok. The pairing
/// bound is HALF a frame, strict: `2·|offset| > frame_interval_ms`.
///
/// Mirror of `genlock_audio_decide_health` in `obs-source.c` — keep both in lock-step (the parity
/// gate compares `decide_audio_health(f).code()` against the C return value over a vector spread).
pub fn decide_audio_health(f: &AudioPairingFacets) -> AudioPairingHealth {
    if f.is_program_source && !f.audio_enabled {
        return AudioPairingHealth::AudioDisabledOnProgram;
    }
    if f.audio_enabled && f.asrc_saturated {
        return AudioPairingHealth::AsrcSaturated;
    }
    if f.audio_enabled
        && f.pairing_offset_ms.saturating_abs().saturating_mul(2) > f.frame_interval_ms
    {
        return AudioPairingHealth::PairingOffsetExceeded;
    }
    AudioPairingHealth::Ok
}

#[cfg(test)]
#[path = "genlock_audio_pairing_bench.rs"]
mod bench;

#[cfg(test)]
#[path = "genlock_audio_pairing_tests.rs"]
mod tests;
