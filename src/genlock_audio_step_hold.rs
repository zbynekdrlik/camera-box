//! Issue 1381 (design 5882391108, piece 2) — the per-source audio SKEW HOLD of a genlock timecode
//! source across a wall step: the pure decision the C `genlock_audio_step_*` helpers in the
//! contiguous audio-pairing block of `obs-source.c` mirror (parity:
//! `tests/genlock_audio_step_hold_parity_1381.rs`). A `#[path]` child of `genlock_audio_pairing`,
//! which re-exports every item.

/// Issue 1381 (design 5882391108) — the longest a source holds its pre-step offset across a wall
/// step before it takes the live one anyway (the sender never followed). Mirror of the C
/// `GENLOCK_AUDIO_STEP_HOLD_MAX_NS`.
pub const AUDIO_STEP_HOLD_MAX_NS: u64 = 10_000_000_000;

/// Issue 1381 (review round 1) — an in-band packet moves the source's NOMINAL stamp age by
/// 1/`AUDIO_STEP_NOMINAL_GAIN_DIV` of its difference (about a 34 s time constant at 30 packets/s):
/// it follows the transport lag's slow drift and averages the arrival jitter, but never a step or a
/// catch-up slide (those leave the one-packet band within a few packets). Mirror of
/// `GENLOCK_AUDIO_STEP_NOMINAL_GAIN_DIV`.
pub const AUDIO_STEP_NOMINAL_GAIN_DIV: i64 = 1024;

/// Issue 1381 (review round 1) — an age that stays out of the nominal band this long WITHOUT a hold
/// is the sender's new transport lag (a re-buffered sender), and becomes the nominal. Long, so a
/// sender that stepped first keeps its pre-step nominal until the receiver's own step lands. Mirror
/// of `GENLOCK_AUDIO_STEP_NOMINAL_REANCHOR_NS`.
pub const AUDIO_STEP_NOMINAL_REANCHOR_NS: u64 = 600_000_000_000;

/// Issue 1381 (review round 2) — after the seed (a source's first timecode packet) the nominal age
/// WARMS UP for this many packets (about 1 s): every packet, in band or not, moves it by
/// 1/[`AUDIO_STEP_NOMINAL_WARM_DIV`] of its difference, so a backlog queued at connect (the first
/// packets read several slots old) never stays the reference. Mirror of
/// `GENLOCK_AUDIO_STEP_NOMINAL_WARM_PACKETS`.
pub const AUDIO_STEP_NOMINAL_WARM_PACKETS: u32 = 30;

/// Issue 1381 (review round 2) — the warm-up gain divisor. Mirror of
/// `GENLOCK_AUDIO_STEP_NOMINAL_WARM_DIV`.
pub const AUDIO_STEP_NOMINAL_WARM_DIV: i64 = 4;

/// Issue 1381 — why a skew hold ended on this packet (0 = it did not). Discriminants match the C
/// `GENLOCK_AUDIO_STEP_*` defines and the log line's `released=` token.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioStepRelease {
    /// No hold ended on this packet (none running, or it still holds).
    None = 0,
    /// The source's own stamps followed the step to within one packet.
    Followed = 1,
    /// [`AUDIO_STEP_HOLD_MAX_NS`] passed without the stamps following.
    Timeout = 2,
    /// The ingest reset the source's timeline in this packet, or the source left timecode mode.
    Reset = 3,
}

impl AudioStepRelease {
    /// The log line's `released=` value. Mirror of `genlock_audio_step_release_token`.
    pub fn token(self) -> &'static str {
        match self {
            AudioStepRelease::None => "none",
            AudioStepRelease::Followed => "followed",
            AudioStepRelease::Timeout => "timeout",
            AudioStepRelease::Reset => "reset",
        }
    }
}

/// Issue 1381 — one source's skew-hold state, field for field the `genlock_audio_step_*` members of
/// `obs_source`. Zeroed = no previous packet, no hold.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioStepHold {
    /// A hold is running: this source maps its stamps through `held_off_ns`, its timecode ASRC is not
    /// fed and the render thread leaves its shallow latch and video-delay tracker alone.
    pub active: bool,
    /// The live wall→mono offset of the previous timecode packet.
    pub prev_off_ns: i64,
    /// The previous packet's raw stamp.
    pub prev_raw_ns: u64,
    /// The previous packet's duration (0 = no previous packet).
    pub prev_packet_ns: u64,
    /// The source's NOMINAL stamp age on the live wall ([`audio_stamp_age_ns`]): the age while the
    /// sender's wall and the receiver's agree. Seeded by the first packet and warmed up over the next
    /// [`AUDIO_STEP_NOMINAL_WARM_PACKETS`], then followed slowly by every in-band packet outside a
    /// hold, frozen inside one. A timeline reset keeps it (a sender whose stamps jumped past OBS's 2 s
    /// limit has stepped first: the receiver's own step brings its age back).
    pub nominal_age_ns: i64,
    /// When the age left the nominal band (OBS monotonic, 0 = it is in band); after
    /// [`AUDIO_STEP_NOMINAL_REANCHOR_NS`] out of band without a hold the nominal re-anchors.
    pub nominal_dev_since_ns: u64,
    /// Warm-up packets left after the seed (0 = the slow in-band track).
    pub nominal_warm: u32,
    /// The offset a held packet maps through: the pre-step offset, moved by every stamp jump the
    /// source made since (a follow in pieces never opens a hole). Kept after a release (the log's
    /// `residual_ms=`).
    pub held_off_ns: i64,
    /// When the hold started (OBS monotonic). Kept after a release (the log's `held_ms=`).
    pub start_ns: u64,
    /// The wall step that started it (wall − mono, + = the wall jumped forward, the sign of the
    /// render tick's `genlock-regrid` line). Kept after a release.
    pub step_ns: i64,
}

/// Issue 1381 — how old a packet's stamp is on the live wall when it arrives, ns: `now − (raw +
/// off)` on the OBS monotonic clock (`off` = the live wall→mono offset). Steady on a sender whose
/// stamps are its wall at emit (the arrival lag); a wall step the sender has not followed moves it by
/// the whole step. Mirror of `genlock_audio_stamp_age_ns`.
pub fn audio_stamp_age_ns(now_ns: u64, raw_ts_ns: u64, off_live_ns: i64) -> i64 {
    now_ns.wrapping_sub(raw_ts_ns.wrapping_add(off_live_ns as u64)) as i64
}

/// Issue 1381 (design 5882391108, piece 2) — the per-source SKEW HOLD of the genlock timecode audio
/// across a wall step, for one packet. Returns the wall→mono offset this packet is mapped through
/// (the live one unless a hold runs) and why a hold ended on it.
///
/// A fleet date step moves the receiver's wall clock at once; the sender (SongPlayer on resolume)
/// follows only seconds later, so for that long every packet's stamp is on the OLD wall while the
/// live offset is on the NEW one. Placed through the live offset, each packet reads the whole step as
/// its placement error: the booking and the level loop act on a skew that is no error of the audio.
///
/// - **Start.** Between two timecode packets the live offset moved by more than `step_min_ns` (the
///   render tick's `GENLOCK_WALL_STEP_MIN_NS`), and this packet has NOT followed it (see Release):
///   its stamp is still over one packet off the live wall. A step up to one packet, a timeline reset,
///   a first packet, or a timeline-reset packet never starts one. A step that brings the stamps BACK to their nominal age
///   (the sender's box stepped first: its stamps jumped, or it caught up, and the receiver's own
///   step puts them on its wall again) is a zero-length hold, `Followed` on its own packet with the
///   whole step as its residual, so the ingest places it once (review round 1).
/// - **Hold.** Every stamp jump over `step_min_ns` against the source's own sample count moves the
///   held offset by the jump, so the landing stays continuous however the sender follows.
/// - **Release.** `Followed` once the stamps are back on the live wall, either way a sender can get
///   there: the held offset is within one packet of the live one (the stamps JUMPED by the step), or
///   the stamp's live-wall age ([`audio_stamp_age_ns`]) is within one packet of its nominal age (a
///   sender whose grid stamps stay continuous and CATCH UP instead -- SongPlayer's audio emitter
///   re-anchors only past 1 s, a forward step is a burst of the missed slots, a backward one a
///   pause). `Timeout` after [`AUDIO_STEP_HOLD_MAX_NS`], `Reset` on a timeline reset or when the
///   source leaves timecode mode (`timecode` false clears the state). A released packet maps through
///   the live offset; what that moves is [`audio_step_residual_ns`].
///
/// The NOMINAL age (review round 1) is the reference for both age tests: a slow, one-packet-band
/// track of the age outside a hold ([`AUDIO_STEP_NOMINAL_GAIN_DIV`]), so neither a one-sample
/// jitter nor the shifted age of a sender that stepped first is mistaken for the pre-step age.
///
/// Every arithmetic wraps in two's complement, like the C mirror `genlock_audio_step_hold`.
#[allow(clippy::too_many_arguments)]
pub fn audio_step_hold(
    s: &mut AudioStepHold,
    timecode: bool,
    off_live_ns: i64,
    raw_ts_ns: u64,
    packet_ns: u64,
    now_ns: u64,
    timeline_reset: bool,
    step_min_ns: i64,
) -> (i64, AudioStepRelease) {
    if !timecode {
        let was = s.active;
        s.active = false;
        s.prev_packet_ns = 0;
        let release = if was {
            AudioStepRelease::Reset
        } else {
            AudioStepRelease::None
        };
        return (off_live_ns, release);
    }
    let had_prev = s.prev_packet_ns != 0;
    let dev_ns = if had_prev {
        raw_ts_ns.wrapping_sub(s.prev_raw_ns.wrapping_add(s.prev_packet_ns)) as i64
    } else {
        0
    };
    let jump_ns = if had_prev {
        off_live_ns.wrapping_sub(s.prev_off_ns)
    } else {
        0
    };
    let age_ns = audio_stamp_age_ns(now_ns, raw_ts_ns, off_live_ns);
    let prev_off_ns = s.prev_off_ns;
    s.prev_off_ns = off_live_ns;
    s.prev_raw_ns = raw_ts_ns;
    s.prev_packet_ns = packet_ns;
    let min = step_min_ns.unsigned_abs();
    let followed = if dev_ns.unsigned_abs() > min {
        dev_ns
    } else {
        0
    };
    let off_nominal = age_ns.wrapping_sub(s.nominal_age_ns).unsigned_abs() <= packet_ns;
    if !s.active {
        if !had_prev {
            s.nominal_age_ns = age_ns;
            s.nominal_dev_since_ns = 0;
            s.nominal_warm = AUDIO_STEP_NOMINAL_WARM_PACKETS;
            return (off_live_ns, AudioStepRelease::None);
        }
        if !timeline_reset && jump_ns.unsigned_abs() > min {
            let held_ns = prev_off_ns.wrapping_sub(followed);
            if off_live_ns.wrapping_sub(held_ns).unsigned_abs() > packet_ns {
                s.held_off_ns = held_ns;
                s.start_ns = now_ns;
                s.step_ns = 0_i64.wrapping_sub(jump_ns);
                if off_nominal {
                    // the stamps got there first: a zero-length hold, released on its own packet
                    audio_step_track_nominal(s, age_ns, packet_ns, now_ns);
                    return (off_live_ns, AudioStepRelease::Followed);
                }
                s.active = true;
                return (held_ns, AudioStepRelease::None);
            }
        }
        audio_step_track_nominal(s, age_ns, packet_ns, now_ns);
        return (off_live_ns, AudioStepRelease::None);
    }
    if timeline_reset {
        s.active = false;
        return (off_live_ns, AudioStepRelease::Reset);
    }
    s.held_off_ns = s.held_off_ns.wrapping_sub(followed);
    if off_live_ns.wrapping_sub(s.held_off_ns).unsigned_abs() <= packet_ns || off_nominal {
        s.active = false;
        return (off_live_ns, AudioStepRelease::Followed);
    }
    if now_ns.wrapping_sub(s.start_ns) >= AUDIO_STEP_HOLD_MAX_NS {
        s.active = false;
        return (off_live_ns, AudioStepRelease::Timeout);
    }
    (s.held_off_ns, AudioStepRelease::None)
}

/// Issue 1381 (review round 1) — one packet outside a hold moves the nominal age. In the warm-up after
/// the seed it follows by 1/[`AUDIO_STEP_NOMINAL_WARM_DIV`], in band or not (review round 2). After
/// it: in band (within one packet) by 1/[`AUDIO_STEP_NOMINAL_GAIN_DIV`]; out of band it is left
/// alone, and after [`AUDIO_STEP_NOMINAL_REANCHOR_NS`] out of band the age becomes the nominal.
/// Mirror of `genlock_audio_step_track_nominal`.
fn audio_step_track_nominal(s: &mut AudioStepHold, age_ns: i64, packet_ns: u64, now_ns: u64) {
    let dev_ns = age_ns.wrapping_sub(s.nominal_age_ns);
    if s.nominal_warm > 0 {
        // the timer is still 0 here: the seed that started the warm-up cleared it
        s.nominal_warm -= 1;
        s.nominal_age_ns = s
            .nominal_age_ns
            .wrapping_add(dev_ns / AUDIO_STEP_NOMINAL_WARM_DIV);
    } else if dev_ns.unsigned_abs() <= packet_ns {
        s.nominal_age_ns = s
            .nominal_age_ns
            .wrapping_add(dev_ns / AUDIO_STEP_NOMINAL_GAIN_DIV);
        s.nominal_dev_since_ns = 0;
    } else if s.nominal_dev_since_ns == 0 {
        s.nominal_dev_since_ns = now_ns.max(1);
    } else if now_ns.wrapping_sub(s.nominal_dev_since_ns) >= AUDIO_STEP_NOMINAL_REANCHOR_NS {
        s.nominal_age_ns = age_ns;
        s.nominal_dev_since_ns = 0;
    }
}

/// Issue 1381 (review round 1) — the RENDER thread's view of a skew hold: its shallow latch and
/// video-delay tracker stay frozen while the hold runs, but never past [`AUDIO_STEP_HOLD_MAX_NS`] on
/// the render thread's own clock (a source whose audio stops inside a hold has no packet left to end
/// it). Mirror of `genlock_audio_step_freezes_video`.
pub fn audio_step_freezes_video(active: bool, start_ns: u64, now_ns: u64) -> bool {
    active && now_ns.wrapping_sub(start_ns) < AUDIO_STEP_HOLD_MAX_NS
}

/// Issue 1381 — the placement move a release applies, ns: the live offset minus the held one (the
/// log's `residual_ms=`; negative = the packets land earlier from here on). Mirror of
/// `genlock_audio_step_residual_ns`.
pub fn audio_step_residual_ns(held_off_ns: i64, off_live_ns: i64) -> i64 {
    off_live_ns.wrapping_sub(held_off_ns)
}

/// Issue 1381 — does this packet's release PLACE it (apply the new offset once) instead of
/// appending? When the move it applies is over one packet: a `Timeout`, or a `Followed` sender that
/// caught up without jumping its stamps. A jumped follow (within one packet) appends or books as
/// usual, and a `Reset` packet is placed by the ingest's own timeline reset. Never a W-second payment
/// at 1000 ppm for a step. Mirror of `genlock_audio_step_release_places`.
pub fn audio_step_release_places(
    release: AudioStepRelease,
    residual_ns: i64,
    packet_ns: u64,
) -> bool {
    matches!(
        release,
        AudioStepRelease::Followed | AudioStepRelease::Timeout
    ) && residual_ns.unsigned_abs() > packet_ns
}
