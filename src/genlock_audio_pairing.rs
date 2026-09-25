//! #1303 + issue 1367 — the pure receiver-side AUDIO ↔ video-FIFO pairing decision for a
//! genlocked NDI source.
//!
//! ## What the video does, and what the audio must do
//!
//! A `genlock_fifo` source releases each frame once its NDI stamp is due against the wall clock
//! (`vendor/obs-studio/libobs/obs-source.c`). The frame the tick presents is the queue HEAD, so it
//! reaches the program at `stamp + (tick − head stamp)`, the head's age at the render tick. That age
//! is the source's REAL stamp→present delay. It is `latency_ms` only on a source whose FIFO is
//! exactly one pin deep. A shallow cg feed sits 2–3 frames deep at a 3 ms pin, which makes its
//! delay 60–100 ms (the win-resolume `ts_head_skew_ms=97` at `latency_ms=3`).
//!
//! #1303 held the audio by the fixed `latency_ms` on its ARRIVAL clock. Audio therefore led video by
//! `delay − latency_ms − arrival lag`, about 94 ms on resolume, and `audio_pairing_offset_ms` was
//! computed against the same `latency_ms` and read 0.
//!
//! Issue 1367 (Option 3) makes the audio follow the video's MEASURED delay:
//!
//! 1. **Measure.** Every ts-align render tick samples `tick_scheduled_wall − head_stamp`
//!    ([`video_delay_sample_ns`]; the scheduled instant, so a late tick's processing lag is not
//!    counted). [`video_delay_track`] smooths it with an EMA ([`VIDEO_DELAY_EMA_SHIFT`]).
//! 2. **Quantize with hysteresis.** A change of the smoothed delay by half a frame or more
//!    ([`video_delay_moved`]) ARMS a re-application. The new whole-ms delay
//!    ([`video_delay_round_ms`]) is applied once the EMA has settled ([`VIDEO_DELAY_SETTLE_TICKS`]).
//!    Applying at the crossing itself would latch the audio half-way through a one-frame step, and
//!    the half-frame hysteresis would never correct it afterwards.
//! 3. **Place.** The audio ingest ([`audio_hold_mode`] / [`audio_place_term_ns`]) places a packet
//!    with timecode `tc` (the same wall-epoch basis DistroAV stamps the video with) at the OBS
//!    monotonic instant `tc + off_live + delay`. `off_live = mono_now − wall_now` is read on EVERY
//!    packet ([`audio_wall_to_mono_ns`]), never latched, because the wall and QPC clocks drift apart
//!    (318 ms on resolume). Packets that follow append back to back. The genlock ASRC rate servo
//!    (disciplined against the wall clock) and its level loop then hold the captured depth, so the
//!    drift is absorbed physically between placements.
//! 4. **Re-place at once.** When the applied delay (or the hold mode) changes, the ingest forces a
//!    fresh placement and shifts the ASRC level target by the same placement delta
//!    ([`audio_place_shift_ms`]), exactly like a sync-offset change. The slow level integral alone
//!    would take minutes to walk a 33 ms step.
//! 5. **Observe.** `audio_pairing_offset_ms` is the applied audio delay minus the MEASURED video
//!    delay ([`pairing_offset_ms`] with [`video_delay_reference_ns`]). The health verdict flags it
//!    above half a frame ([`decide_audio_health`]).
//!
//! Until the first measurement settles, or for an audio timestamp that is not a wall-clock
//! timecode, the ingest keeps the #1303 behaviour byte-for-byte: arrival basis + `latency_ms`
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

/// One stamp→present delay sample: the head frame's age at the render tick's SCHEDULED wall instant
/// (`genlock_n1_tick_wall_now`). Saturates at 0 for a head stamped after the tick.
///
/// Mirror of `genlock_video_delay_sample_ns`.
pub fn video_delay_sample_ns(tick_wall_ns: u64, head_stamp_ns: u64) -> u64 {
    tick_wall_ns.saturating_sub(head_stamp_ns)
}

/// One EMA step of the smoothed stamp→present delay. `0` is the unseeded sentinel: the first sample
/// seeds the EMA. The step truncates toward zero, identically in C (`int64_t` division).
///
/// Mirror of `genlock_video_delay_smooth_ns`.
pub fn video_delay_smooth_ns(smoothed_ns: u64, sample_ns: u64) -> u64 {
    if smoothed_ns == 0 {
        return sample_ns;
    }
    let diff = sample_ns as i64 - smoothed_ns as i64;
    (smoothed_ns as i64 + diff / (1i64 << VIDEO_DELAY_EMA_SHIFT)) as u64
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
    /// Render ticks left until an armed re-application applies (0 = idle).
    pub settle_ticks: u32,
}

/// One render tick of the tracker: smooth the sample; an idle tracker ARMS a settle countdown when
/// the smoothed delay has moved half a frame or more; an armed one counts down and, at 0, applies
/// the rounded smoothed delay if it is STILL half a frame or more away (a transient that reversed
/// during the settle applies nothing, so the audio is never re-placed for a sub-half-frame change).
///
/// Mirror of `genlock_video_delay_track`.
pub fn video_delay_track(t: &mut VideoDelayTracker, sample_ns: u64, interval_ns: u64) {
    // RED stub: the old code never tracked the video delay.
    t.smoothed_ns = video_delay_smooth_ns(t.smoothed_ns, sample_ns);
    let _ = (interval_ns, VIDEO_DELAY_SETTLE_TICKS);
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
        }
    }
}

/// Pick the hold for one audio packet. `latency_ms == 0` is the unreachable floor-violating value
/// (the pin is seeded ≥ 3 ms) and holds nothing, as in #1303.
///
/// Mirror of `genlock_audio_hold_mode`.
pub fn audio_hold_mode(
    genlock_fifo: bool,
    latency_ms: u32,
    audio_ts_is_wallclock: bool,
    video_delay_ms: u32,
) -> AudioHoldMode {
    if !genlock_fifo || latency_ms == 0 {
        AudioHoldMode::Off
    } else {
        // RED stub: the old fixed-pin arrival hold only.
        let _ = (audio_ts_is_wallclock, video_delay_ms);
        AudioHoldMode::Latency
    }
}

/// The hold (ms) a mode applies — reported as `audio_delay_ms=`.
///
/// Mirror of `genlock_audio_hold_ms`.
pub fn audio_hold_ms(mode: AudioHoldMode, latency_ms: u32, video_delay_ms: u32) -> u32 {
    match mode {
        AudioHoldMode::Off => 0,
        AudioHoldMode::Latency => latency_ms,
        AudioHoldMode::Timecode => video_delay_ms,
    }
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
        AudioHoldMode::Off => 0,
        AudioHoldMode::Latency => hold_ns,
        AudioHoldMode::Timecode => off_live_ns
            .wrapping_add(hold_ns)
            .wrapping_sub(timing_adjust_ns as i64),
    }
}

/// The ASRC level-target shift (ms) for a re-placement: the difference between the new and the
/// previous placement term of the SAME packet (so the live offset and `timing_adjust` cancel for a
/// timecode→timecode change, which shifts by exactly the delay delta).
///
/// Mirror of `genlock_audio_place_shift_ms`.
pub fn audio_place_shift_ms(new_term_ns: i64, prev_term_ns: i64) -> f64 {
    new_term_ns.wrapping_sub(prev_term_ns) as f64 / 1e6
}

/// The video delay the pairing offset is measured against: the smoothed MEASURED stamp→present
/// delay when there is one, else the nominal `latency_ms` hold.
///
/// Mirror of `genlock_audio_video_delay_ref_ns`.
pub fn video_delay_reference_ns(smoothed_ns: u64, latency_ms: u32) -> i64 {
    // RED stub: the old offset was measured against the pin.
    let _ = smoothed_ns;
    genlock_audio_delay_ns(latency_ms) as i64
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
    if f.audio_enabled && f.pairing_offset_ms.saturating_abs() > f.frame_interval_ms {
        return AudioPairingHealth::PairingOffsetExceeded;
    }
    AudioPairingHealth::Ok
}

#[cfg(test)]
#[path = "genlock_audio_pairing_bench.rs"]
mod bench;

#[cfg(test)]
mod tests {
    use super::*;

    const IV30: u64 = 33_333_333;
    const IV60: u64 = 16_666_666;

    #[test]
    fn delay_is_hold_in_ns() {
        assert_eq!(genlock_audio_delay_ns(3), 3_000_000);
        assert_eq!(genlock_audio_delay_ns(0), 0);
        assert_eq!(genlock_audio_delay_ns(2000), 2_000_000_000);
    }

    // ---- the video stamp→present delay measurement ---------------------------------------

    #[test]
    fn sample_is_the_head_age_at_the_scheduled_tick() {
        assert_eq!(
            video_delay_sample_ns(1_000_100_000_000, 1_000_000_000_000),
            100_000_000
        );
        // a head stamped after the tick (a sender stamping ahead) saturates at 0.
        assert_eq!(video_delay_sample_ns(5, 9), 0);
    }

    #[test]
    fn ema_seeds_on_the_first_sample_and_steps_an_eighth() {
        assert_eq!(video_delay_smooth_ns(0, 97_000_000), 97_000_000);
        assert_eq!(video_delay_smooth_ns(100_000_000, 108_000_000), 101_000_000);
        assert_eq!(video_delay_smooth_ns(100_000_000, 92_000_000), 99_000_000);
        // truncation toward zero on both signs (C int64 division).
        assert_eq!(video_delay_smooth_ns(100, 107), 100);
        assert_eq!(video_delay_smooth_ns(100, 93), 100);
    }

    #[test]
    fn round_ms_is_nearest_and_never_zero() {
        assert_eq!(video_delay_round_ms(66_666_666), 67);
        assert_eq!(video_delay_round_ms(66_499_999), 66);
        assert_eq!(video_delay_round_ms(66_500_000), 67);
        assert_eq!(video_delay_round_ms(0), 1);
        assert_eq!(video_delay_round_ms(400_000), 1);
        assert_eq!(video_delay_round_ms(u64::MAX), u32::MAX);
    }

    #[test]
    fn moved_is_half_a_frame_or_more() {
        assert!(
            video_delay_moved(0, 100_000_000, IV30),
            "nothing applied yet always moves"
        );
        // 30 fps: half a frame is 16.67 ms.
        assert!(!video_delay_moved(100, 116_666_666, IV30));
        assert!(video_delay_moved(100, 116_666_667, IV30));
        assert!(video_delay_moved(100, 83_333_333, IV30));
        assert!(!video_delay_moved(100, 83_333_334, IV30));
        // 60 fps: 8.33 ms.
        assert!(video_delay_moved(50, 58_333_333, IV60));
        assert!(!video_delay_moved(50, 58_333_332, IV60));
        // unknown interval never moves once something is applied.
        assert!(!video_delay_moved(50, 900_000_000, 0));
    }

    fn run(t: &mut VideoDelayTracker, sample: u64, iv: u64, ticks: u32) {
        for _ in 0..ticks {
            video_delay_track(t, sample, iv);
        }
    }

    #[test]
    fn tracker_applies_the_first_delay_after_the_settle() {
        let mut t = VideoDelayTracker::default();
        video_delay_track(&mut t, 100_000_000, IV30);
        assert_eq!(t.applied_ms, 0, "armed, not applied");
        assert_eq!(t.settle_ticks, VIDEO_DELAY_SETTLE_TICKS);
        run(&mut t, 100_000_000, IV30, VIDEO_DELAY_SETTLE_TICKS - 1);
        assert_eq!(t.applied_ms, 0);
        run(&mut t, 100_000_000, IV30, 1);
        assert_eq!(t.applied_ms, 100);
        assert_eq!(t.settle_ticks, 0);
    }

    #[test]
    fn a_one_frame_step_lands_on_the_new_delay_not_mid_step() {
        let mut t = VideoDelayTracker::default();
        run(&mut t, 100_000_000, IV30, 200);
        assert_eq!(t.applied_ms, 100);
        // the FIFO settles one frame shallower.
        run(&mut t, 66_666_667, IV30, 400);
        assert_eq!(
            t.applied_ms, 67,
            "the re-application must apply the NEW delay (66.7 ms), not the value at the half-frame crossing (~83 ms)"
        );
        // and deeper again.
        run(&mut t, 100_000_000, IV30, 400);
        assert_eq!(t.applied_ms, 100);
    }

    #[test]
    fn single_tick_holds_never_rearm() {
        let mut t = VideoDelayTracker::default();
        run(&mut t, 100_000_000, IV30, 200);
        for i in 0..3000u32 {
            // one tick in ten presents the frame a tick older (a hold).
            let s = if i % 10 == 0 {
                133_333_333
            } else {
                100_000_000
            };
            video_delay_track(&mut t, s, IV30);
            assert_eq!(
                t.settle_ticks, 0,
                "a single held tick must not arm a re-application"
            );
        }
        assert_eq!(t.applied_ms, 100);
    }

    #[test]
    fn a_transient_that_reverses_during_the_settle_applies_nothing() {
        let mut t = VideoDelayTracker::default();
        run(&mut t, 100_000_000, IV30, 200);
        // a 20-tick excursion one frame deeper arms the settle, then the delay comes back.
        run(&mut t, 133_333_333, IV30, 20);
        assert!(t.settle_ticks > 0);
        run(&mut t, 100_000_000, IV30, VIDEO_DELAY_SETTLE_TICKS);
        assert_eq!(t.settle_ticks, 0);
        assert_eq!(
            t.applied_ms, 100,
            "the reversed transient must leave the applied delay alone"
        );
    }

    // ---- the audio hold + placement --------------------------------------------------------

    #[test]
    fn hold_mode_selection() {
        assert_eq!(audio_hold_mode(false, 3, true, 97), AudioHoldMode::Off);
        assert_eq!(audio_hold_mode(true, 0, true, 97), AudioHoldMode::Off);
        assert_eq!(audio_hold_mode(true, 3, true, 0), AudioHoldMode::Latency);
        assert_eq!(audio_hold_mode(true, 3, false, 97), AudioHoldMode::Latency);
        assert_eq!(audio_hold_mode(true, 3, true, 97), AudioHoldMode::Timecode);
        assert_eq!(audio_hold_ms(AudioHoldMode::Off, 3, 97), 0);
        assert_eq!(audio_hold_ms(AudioHoldMode::Latency, 3, 97), 3);
        assert_eq!(audio_hold_ms(AudioHoldMode::Timecode, 3, 97), 97);
        assert_eq!(AudioHoldMode::Timecode.code(), 2);
        assert_eq!(AudioHoldMode::Latency.token(), "latency");
    }

    #[test]
    fn wall_to_mono_offset_is_signed() {
        // QPC-style monotonic (seconds since boot) vs a 2026 wall epoch.
        let wall = 1_790_000_000_000_000_000u64;
        let mono = 86_400_000_000_000u64;
        assert_eq!(audio_wall_to_mono_ns(mono, wall), mono as i64 - wall as i64);
        assert_eq!(audio_wall_to_mono_ns(wall + 5, wall), 5);
    }

    #[test]
    fn timecode_term_places_the_packet_at_tc_plus_offset_plus_delay() {
        // the ingest computes in.timestamp = tc + timing_adjust, then adds the term.
        let tc = 1_790_000_000_123_000_000u64;
        let arrival_wall = tc + 2_500_000;
        let mono_at_arrival = 86_400_000_000_000u64;
        let timing_adjust = mono_at_arrival.wrapping_sub(tc); // reset_audio_timing
        let off_live = audio_wall_to_mono_ns(mono_at_arrival, arrival_wall);
        let term = audio_place_term_ns(AudioHoldMode::Timecode, 97, off_live, timing_adjust);
        let placed = tc.wrapping_add(timing_adjust).wrapping_add(term as u64);
        // tc mapped to mono through the live offset, plus the video's delay.
        let want = (tc as i64 + off_live + 97_000_000) as u64;
        assert_eq!(placed, want);
        // the latency term keeps the #1303 arrival basis.
        let lat = audio_place_term_ns(AudioHoldMode::Latency, 3, off_live, timing_adjust);
        assert_eq!(
            tc.wrapping_add(timing_adjust).wrapping_add(lat as u64),
            mono_at_arrival + 3_000_000
        );
        assert_eq!(
            audio_place_term_ns(AudioHoldMode::Off, 97, off_live, timing_adjust),
            0
        );
    }

    #[test]
    fn a_later_placement_follows_the_live_offset_not_the_first_one() {
        // the wall-vs-mono offset walked 300 ms since the first packet (resolume: 318 ms).
        let tc0 = 1_790_000_000_000_000_000u64;
        let mono0 = 50_000_000_000_000u64;
        let timing_adjust = mono0.wrapping_sub(tc0);
        let tc = tc0 + 3_600_000_000_000; // an hour later
        let off_live = audio_wall_to_mono_ns(mono0 + 3_600_000_000_000 + 300_000_000, tc);
        let term = audio_place_term_ns(AudioHoldMode::Timecode, 67, off_live, timing_adjust);
        let placed = tc.wrapping_add(timing_adjust).wrapping_add(term as u64);
        assert_eq!(placed, mono0 + 3_600_000_000_000 + 300_000_000 + 67_000_000);
    }

    #[test]
    fn shift_is_the_delay_delta_for_a_timecode_change() {
        let off = -1_789_000_000_000_000_000i64;
        let ta = 17_000_000_000_000_000_000u64;
        let a = audio_place_term_ns(AudioHoldMode::Timecode, 100, off, ta);
        let b = audio_place_term_ns(AudioHoldMode::Timecode, 67, off, ta);
        assert_eq!(audio_place_shift_ms(b, a), -33.0);
        let l = audio_place_term_ns(AudioHoldMode::Latency, 3, off, ta);
        assert_eq!(audio_place_shift_ms(l, 0), 3.0);
    }

    // ---- the pairing offset + health -------------------------------------------------------

    #[test]
    fn video_reference_is_the_measurement_else_the_pin() {
        assert_eq!(video_delay_reference_ns(97_400_000, 3), 97_400_000);
        assert_eq!(video_delay_reference_ns(0, 3), 3_000_000);
    }

    #[test]
    fn pairing_offset_is_measured_against_the_real_video_delay() {
        // the #1303 fixed 3 ms hold against a 97 ms video delay: the defect is VISIBLE now.
        assert_eq!(pairing_offset_ms(3_000_000, 97_000_000), -94);
        // audio following the measured delay.
        assert_eq!(pairing_offset_ms(97_000_000, 97_400_000), 0);
        assert_eq!(pairing_offset_ms(100_000_000, 83_400_000), 16);
        assert_eq!(pairing_offset_ms(67_000_000, 83_700_000), -16);
        // never held.
        assert_eq!(pairing_offset_ms(0, 97_000_000), -97);
        assert_eq!(pairing_offset_ms(923_000_000, 923_000_000), 0);
    }

    fn healthy() -> AudioPairingFacets {
        AudioPairingFacets {
            audio_enabled: true,
            is_program_source: true,
            asrc_saturated: false,
            pairing_offset_ms: 0,
            frame_interval_ms: 33,
        }
    }

    #[test]
    fn healthy_program_audio_is_ok() {
        assert_eq!(decide_audio_health(&healthy()), AudioPairingHealth::Ok);
    }

    #[test]
    fn program_source_with_audio_off_is_degraded() {
        let mut f = healthy();
        f.audio_enabled = false;
        assert_eq!(
            decide_audio_health(&f),
            AudioPairingHealth::AudioDisabledOnProgram
        );
    }

    #[test]
    fn non_program_source_with_audio_off_is_ok() {
        // a camera input keeps ndi_audio=false by design — NOT a fault.
        let mut f = healthy();
        f.is_program_source = false;
        f.audio_enabled = false;
        f.pairing_offset_ms = -97;
        assert_eq!(decide_audio_health(&f), AudioPairingHealth::Ok);
    }

    #[test]
    fn asrc_saturated_is_degraded() {
        let mut f = healthy();
        f.asrc_saturated = true;
        assert_eq!(decide_audio_health(&f), AudioPairingHealth::AsrcSaturated);
    }

    #[test]
    fn asrc_saturated_ignored_when_audio_disabled_non_program() {
        let mut f = healthy();
        f.is_program_source = false;
        f.audio_enabled = false;
        f.asrc_saturated = true;
        assert_eq!(decide_audio_health(&f), AudioPairingHealth::Ok);
    }

    #[test]
    fn pairing_offset_within_half_a_frame_is_ok() {
        let mut f = healthy();
        f.pairing_offset_ms = 16; // 2*16 = 32, not > 33
        assert_eq!(decide_audio_health(&f), AudioPairingHealth::Ok);
        f.pairing_offset_ms = -16;
        assert_eq!(decide_audio_health(&f), AudioPairingHealth::Ok);
    }

    #[test]
    fn pairing_offset_beyond_half_a_frame_is_degraded() {
        let mut f = healthy();
        f.pairing_offset_ms = 17;
        assert_eq!(
            decide_audio_health(&f),
            AudioPairingHealth::PairingOffsetExceeded
        );
        f.pairing_offset_ms = -94; // the resolume #1303 defect
        assert_eq!(
            decide_audio_health(&f),
            AudioPairingHealth::PairingOffsetExceeded
        );
    }

    #[test]
    fn pairing_offset_bound_follows_frame_interval() {
        // at 60 fps half a frame is 8 ms.
        let mut f = healthy();
        f.frame_interval_ms = 16;
        f.pairing_offset_ms = 9;
        assert_eq!(
            decide_audio_health(&f),
            AudioPairingHealth::PairingOffsetExceeded
        );
        f.pairing_offset_ms = 8;
        assert_eq!(decide_audio_health(&f), AudioPairingHealth::Ok);
    }

    #[test]
    fn pairing_offset_extremes_do_not_overflow() {
        let mut f = healthy();
        f.pairing_offset_ms = i64::MIN;
        assert_eq!(
            decide_audio_health(&f),
            AudioPairingHealth::PairingOffsetExceeded
        );
    }

    // ---- precedence ----------------------------------------------------------------

    #[test]
    fn audio_disabled_program_beats_asrc_and_offset() {
        let mut f = healthy();
        f.audio_enabled = false;
        f.asrc_saturated = true;
        f.pairing_offset_ms = 999;
        assert_eq!(
            decide_audio_health(&f),
            AudioPairingHealth::AudioDisabledOnProgram
        );
    }

    #[test]
    fn asrc_saturated_beats_pairing_offset() {
        let mut f = healthy();
        f.asrc_saturated = true;
        f.pairing_offset_ms = 999;
        assert_eq!(decide_audio_health(&f), AudioPairingHealth::AsrcSaturated);
    }

    #[test]
    fn codes_match_the_c_enum_values() {
        assert_eq!(AudioPairingHealth::Ok.code(), 0);
        assert_eq!(AudioPairingHealth::AudioDisabledOnProgram.code(), 1);
        assert_eq!(AudioPairingHealth::AsrcSaturated.code(), 2);
        assert_eq!(AudioPairingHealth::PairingOffsetExceeded.code(), 3);
    }
}
