//! #1303 — the pure receiver-side AUDIO ↔ video-FIFO pairing decision for a genlocked NDI
//! source.
//!
//! The video leg is genlocked: the FIFO releases each frame at `present_ts = wall_now −
//! latency_ms` (`vendor/obs-studio/libobs/obs-source.c` `genlock_present_ts_reserve`), so video
//! presentation tracks the fleet wall clock. This module defines how the SAME source's audio is
//! aligned to that held instant so the A/V pair SURVIVES the hold: the audio is delayed by the
//! SAME `latency_ms` the video is held (a sample stamped `T` is then mixed/played when the
//! playback position reaches `T + latency_ms`, exactly when the FIFO shows the frame stamped
//! `T`). This is orthogonal to the ASRC servo (#803/#912/#1084), which disciplines the audio
//! sample-clock RATE (ppm) against `genlock_wall_now_ns()`, NOT the phase/latency this module
//! owns.
//!
//! ## Why crate-root + pure `std`
//!
//! The whole `probe` module is `#[cfg(feature = "probe")]` (pulls image/qr/drm deps that balloon
//! the shared `target/`, per the Local Build Policy). This decision needs none of that, so it
//! lives here as a pure module — the exact `src/genlock_lock_state.rs` / `src/resolume_playback.rs`
//! / `src/genlock_backlog.rs` pattern: it unit-tests Tier-0 (default features, standalone-rustc,
//! `.claude/rules/vendored-libobs-change-safety.md` §"pure-std crate-root module"), and its C
//! mirror (`genlock_audio_*` `static inline` helpers in `obs-source.c`) is held byte-identical by
//! the committed parity gate `tests/genlock_audio_pairing_parity.rs` (the #1003 lift-and-compile
//! recipe). The vendored C CALLS the mirror at the audio-ingest seam
//! (`source_output_audio_data`), and the LOCK indicator (`genlock_decide_lock_state`) + the
//! `genlock-fifo audit` audio facet consume the same decision — defined ONCE, here.

/// Nanoseconds per millisecond (the audit line + OBS timestamps are in ns; the operator knob is
/// in ms).
pub const NS_PER_MS: u64 = 1_000_000;

/// The per-source audio hold, in nanoseconds, that keeps the audio paired with the video FIFO
/// hold: the audio is delayed by the SAME `latency_ms` the video FIFO holds video
/// (`present_ts = wall_now − latency_ms`). Added to a genlocked, wall-clock-stamped audio frame's
/// timestamp at ingest, so the pair is presented at the same wall instant.
///
/// Mirror of `genlock_audio_present_delay_ns` in `obs-source.c`. `latency_ms` is the EFFECTIVE
/// held latency (the per-source override if set, else the global floor) — the SAME value the video
/// hold uses, so a change to the source's latency moves both legs together.
pub fn genlock_audio_delay_ns(latency_ms: u32) -> u64 {
    latency_ms as u64 * NS_PER_MS
}

/// The residual A/V pairing offset, in ms, between the audio delay actually applied and the video
/// FIFO latency: `applied_audio_delay_ns/1e6 − video_latency_ms`. Zero = perfectly paired; a
/// non-zero value is the observability signal surfaced on the `genlock-fifo audit` line and fed
/// into the LOCK-indicator health decision below. Signed (positive = audio held LONGER than the
/// video).
///
/// Mirror of `genlock_audio_pairing_offset_ms` in `obs-source.c`.
pub fn pairing_offset_ms(applied_audio_delay_ns: i64, video_latency_ms: u32) -> i64 {
    applied_audio_delay_ns / NS_PER_MS as i64 - video_latency_ms as i64
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
    /// The residual `|pairing_offset_ms|` exceeds one frame interval — the audio is more than a
    /// frame off the held video.
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
    /// One frame interval in ms (33 at 30 fps, 16 at 60 fps) — the pairing-offset bound.
    pub frame_interval_ms: i64,
}

/// Decide the audio-parity health from the scalarised facets. Precedence:
/// audio-disabled-on-a-program-source > asrc-saturated > pairing-offset-exceeded > Ok.
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
    if f.audio_enabled && f.pairing_offset_ms.abs() > f.frame_interval_ms {
        return AudioPairingHealth::PairingOffsetExceeded;
    }
    AudioPairingHealth::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_is_latency_in_ns() {
        assert_eq!(genlock_audio_delay_ns(3), 3_000_000);
        assert_eq!(genlock_audio_delay_ns(0), 0);
        assert_eq!(genlock_audio_delay_ns(2000), 2_000_000_000);
    }

    #[test]
    fn pairing_offset_zero_when_delay_matches_latency() {
        // audio delayed by exactly the video latency ⇒ paired.
        assert_eq!(pairing_offset_ms(genlock_audio_delay_ns(3) as i64, 3), 0);
        assert_eq!(
            pairing_offset_ms(genlock_audio_delay_ns(923) as i64, 923),
            0
        );
    }

    #[test]
    fn pairing_offset_signed() {
        // audio held 10 ms while video only 3 ms ⇒ +7 (audio later).
        assert_eq!(pairing_offset_ms(10 * NS_PER_MS as i64, 3), 7);
        // audio held 3 ms while video 10 ms ⇒ -7 (audio earlier).
        assert_eq!(pairing_offset_ms(3 * NS_PER_MS as i64, 10), -7);
        // no audio delay applied at all while video held 33 ms ⇒ -33.
        assert_eq!(pairing_offset_ms(0, 33), -33);
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
        // audio off on a non-program source ⇒ Ok even if a stale asrc flag lingers.
        let mut f = healthy();
        f.is_program_source = false;
        f.audio_enabled = false;
        f.asrc_saturated = true;
        assert_eq!(decide_audio_health(&f), AudioPairingHealth::Ok);
    }

    #[test]
    fn pairing_offset_within_one_frame_is_ok() {
        let mut f = healthy();
        f.pairing_offset_ms = 33; // exactly one frame — OK (bound is strict >)
        assert_eq!(decide_audio_health(&f), AudioPairingHealth::Ok);
        f.pairing_offset_ms = -33;
        assert_eq!(decide_audio_health(&f), AudioPairingHealth::Ok);
    }

    #[test]
    fn pairing_offset_beyond_one_frame_is_degraded() {
        let mut f = healthy();
        f.pairing_offset_ms = 34;
        assert_eq!(
            decide_audio_health(&f),
            AudioPairingHealth::PairingOffsetExceeded
        );
        f.pairing_offset_ms = -50;
        assert_eq!(
            decide_audio_health(&f),
            AudioPairingHealth::PairingOffsetExceeded
        );
    }

    #[test]
    fn pairing_offset_bound_follows_frame_interval() {
        // at 60 fps the bound is 16 ms.
        let mut f = healthy();
        f.frame_interval_ms = 16;
        f.pairing_offset_ms = 20;
        assert_eq!(
            decide_audio_health(&f),
            AudioPairingHealth::PairingOffsetExceeded
        );
        f.pairing_offset_ms = 16;
        assert_eq!(decide_audio_health(&f), AudioPairingHealth::Ok);
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
