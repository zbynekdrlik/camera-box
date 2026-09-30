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

/// Issue 1381 (design 5901213031) — the arrival jitter a PENDING relabel's packet may add to one packet
/// duration: the repo's measured 15 ms arrival jitter budget (ROZHODNUTÉ 5842640404). A relabelling
/// sender re-phases its emit EARLIER by the remainder, so its packets keep arriving about one packet
/// apart; a pause or a restart shows the gap its stamps jumped by. Mirror of
/// `GENLOCK_AUDIO_RELABEL_ARRIVAL_JITTER_NS`.
pub const AUDIO_RELABEL_ARRIVAL_JITTER_NS: u64 = 15_000_000;

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
    /// Design 5901213031: a PENDING relabel (the sender's box stepped first) resolved -- this box's
    /// own step followed the stamps to within one packet ([`audio_step_hold`]).
    RelabelPending = 4,
}

impl AudioStepRelease {
    /// The log line's `released=` value. Mirror of `genlock_audio_step_release_token`.
    pub fn token(self) -> &'static str {
        match self {
            AudioStepRelease::None => "none",
            AudioStepRelease::Followed => "followed",
            AudioStepRelease::Timeout => "timeout",
            AudioStepRelease::Reset => "reset",
            AudioStepRelease::RelabelPending => "relabel-pending",
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
    /// render tick's `genlock-regrid` line; for a pending relabel the SENDER's step, its stamp jump).
    /// Kept after a release.
    pub step_ns: i64,
    /// Design 5901213031: the running (or the last) hold is a PENDING relabel -- the stamps jumped by
    /// J while this box's offset did not move (the sender's box stepped first); `held_off_ns` is the
    /// pre-jump offset minus J, so the packets keep appending on their continuous timeline. Set when
    /// a hold starts, kept after its release (the log's `pending=`).
    pub relabel_pending: bool,
    /// Design 5901213031: the previous timecode packet's arrival (OBS monotonic), for the arrival
    /// gap [`audio_relabel_pending`] reads.
    pub prev_arrival_ns: u64,
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
/// **A PENDING relabel (design 5901213031)** is a hold of its own kind: the stamps jumped by J while
/// this box's live offset did not move -- the SENDER's box stepped first
/// ([`audio_step_relabel_pending_starts`]). It holds the pre-jump offset minus J (the stamps read
/// shifted by −J, the landing continuous), with the same ASRC and render-thread freeze, and releases
/// `RelabelPending` on the packet whose live offset jumps (over `step_min_ns`) to within one packet of
/// the held offset: this box's own step followed, the landing moved by the remainder only. Otherwise
/// `Timeout` after [`AUDIO_STEP_HOLD_MAX_NS`] (J applied once), or `Reset`. The ordinary follow and
/// age releases do not apply to it. A stamp move under one packet folds into its held offset like
/// the skew hold's (review round 2: a raw-clock sender's late step packet comes back on the next
/// one); a move of one packet or more only when it is relabel-shaped ([`audio_relabel_pending`]:
/// continuous arrival) -- a pause, a duplicated or a skipped slot keeps it, so this box's own step
/// still resolves the pending (review round 1: folded, a 500 ms pause left it to the bound and
/// placed 484 ms).
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
    // design 5901213031: decided on the state BEFORE this packet moves it (the ingest asks the same)
    let pending_starts = !timeline_reset
        && audio_step_relabel_pending_starts(
            s,
            true,
            off_live_ns,
            raw_ts_ns,
            packet_ns,
            now_ns,
            step_min_ns,
        );
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
    let arrival_gap_ns = now_ns.wrapping_sub(s.prev_arrival_ns);
    let prev_off_ns = s.prev_off_ns;
    s.prev_off_ns = off_live_ns;
    s.prev_raw_ns = raw_ts_ns;
    s.prev_packet_ns = packet_ns;
    s.prev_arrival_ns = now_ns;
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
                s.relabel_pending = false;
                if off_nominal {
                    // the stamps got there first: a zero-length hold, released on its own packet
                    audio_step_track_nominal(s, age_ns, packet_ns, now_ns);
                    return (off_live_ns, AudioStepRelease::Followed);
                }
                s.active = true;
                return (held_ns, AudioStepRelease::None);
            }
        }
        if pending_starts {
            // design 5901213031: the sender's box stepped first -- keep the stamps on the
            // continuous timeline (read shifted by −J) until this box's own step follows
            s.held_off_ns = prev_off_ns.wrapping_sub(dev_ns);
            s.start_ns = now_ns;
            s.step_ns = dev_ns;
            s.active = true;
            s.relabel_pending = true;
            return (s.held_off_ns, AudioStepRelease::None);
        }
        audio_step_track_nominal(s, age_ns, packet_ns, now_ns);
        return (off_live_ns, AudioStepRelease::None);
    }
    if timeline_reset {
        s.active = false;
        return (off_live_ns, AudioStepRelease::Reset);
    }
    // inside a pending relabel a move under one packet folds like the skew hold's (review round 2: a
    // raw-clock sender's late stamp comes back on the next packet); a move of one packet or more only
    // when it is relabel-shaped (review round 1: a pause, a duplicated or a skipped slot keeps it)
    let fold_ns = if !s.relabel_pending || dev_ns.unsigned_abs() < packet_ns {
        followed
    } else if audio_relabel_pending(dev_ns, arrival_gap_ns, packet_ns, step_min_ns) {
        dev_ns
    } else {
        0
    };
    s.held_off_ns = s.held_off_ns.wrapping_sub(fold_ns);
    if s.relabel_pending {
        if jump_ns.unsigned_abs() > min
            && off_live_ns.wrapping_sub(s.held_off_ns).unsigned_abs() < packet_ns
        {
            s.active = false;
            return (off_live_ns, AudioStepRelease::RelabelPending);
        }
    } else if off_live_ns.wrapping_sub(s.held_off_ns).unsigned_abs() <= packet_ns || off_nominal {
        s.active = false;
        return (off_live_ns, AudioStepRelease::Followed);
    }
    if now_ns.wrapping_sub(s.start_ns) >= AUDIO_STEP_HOLD_MAX_NS {
        s.active = false;
        return (off_live_ns, AudioStepRelease::Timeout);
    }
    (s.held_off_ns, AudioStepRelease::None)
}

/// Issue 1381 (design 5900385541) — is this timecode packet a RELABEL: did the sender's stamps jump
/// WITH a wall step, so that its landing moved by less than one packet?
///
/// At a date step of S a sender that follows the genlock sender contract (sections 5 and 6) relabels
/// its stamps by N = floor(S / slot) slots within one interval while its samples stay continuous. The
/// live wall→mono offset jumps by −S at the same time, so the packet's intended landing moves only by
/// −r (r = S − N·slot, under one slot = one block). Stock OBS still turns that into a loss: a stamp
/// jump of 70 ms or more is PLACED r early (r ms of queued audio overwritten), one over 2 s resets the
/// whole buffer. A relabel is instead APPENDED, and the timecode ASRC books the −r as ordinary
/// placement error.
///
/// - `stamp_jump_ns`: the raw stamp against the continuous timeline (the previous packet's stamp plus
///   its duration);
/// - `off_jump_ns`: the live offset against the offset the previous packet was mapped through
///   ([`audio_step_relabel_jumps`]).
///
/// True when BOTH moved by more than `step_min_ns` (the render tick's `GENLOCK_WALL_STEP_MIN_NS`: a
/// steady packet has neither, a sender whose stamps leap without a wall step has no offset jump, a
/// catch-up or a pause has no stamp jump) and they cancel to strictly under one packet. Every
/// arithmetic wraps in two's complement, like the C mirror `genlock_audio_relabel`.
pub fn audio_relabel(
    stamp_jump_ns: i64,
    off_jump_ns: i64,
    packet_ns: u64,
    step_min_ns: i64,
) -> bool {
    let min = step_min_ns.unsigned_abs();
    stamp_jump_ns.unsigned_abs() > min
        && off_jump_ns.unsigned_abs() > min
        && stamp_jump_ns.wrapping_add(off_jump_ns).unsigned_abs() < packet_ns
}

/// Issue 1381 (design 5900385541) — the two jumps [`audio_relabel`] reads, from the skew-hold state
/// BEFORE [`audio_step_hold`] takes this packet: `(stamp_jump_ns, off_jump_ns)`.
///
/// - the stamp jump is the raw stamp against the previous packet's stamp plus its duration;
/// - the offset jump is the live offset against the offset the previous packet was MAPPED through:
///   the held one while a hold runs, the previous live one otherwise.
///
/// A relabel that arrives one packet after the receiver's step (the hold started on the packet before,
/// whose stamp had not moved yet: the split shape the contract names) therefore reads the whole step,
/// and the sum of the two is exactly the residual the hold releases that packet with. `None` outside
/// timecode mode or with no previous timecode packet. Mirror of `genlock_audio_step_relabel_jumps`.
pub fn audio_step_relabel_jumps(
    s: &AudioStepHold,
    timecode: bool,
    off_live_ns: i64,
    raw_ts_ns: u64,
) -> Option<(i64, i64)> {
    if !timecode || s.prev_packet_ns == 0 {
        return None;
    }
    let stamp_jump_ns = raw_ts_ns.wrapping_sub(s.prev_raw_ns.wrapping_add(s.prev_packet_ns)) as i64;
    let mapped_ns = if s.active {
        s.held_off_ns
    } else {
        s.prev_off_ns
    };
    Some((stamp_jump_ns, off_live_ns.wrapping_sub(mapped_ns)))
}

/// Issue 1381 (design 5901213031) — is a STAMP-ONLY jump a pending relabel: did the sender's wall step
/// while this box's has not yet?
///
/// - `stamp_jump_ns`: the raw stamp against the continuous timeline (J);
/// - `arrival_gap_ns`: this packet's arrival minus the previous packet's (OBS monotonic).
///
/// True when the stamps jumped by more than `step_min_ns` AND by more than one packet (a relabel moves
/// them N whole slots; a stamp move of one packet or less is a skipped or duplicated sender slot, or
/// the jitter of a sender that stamps its raw submission clock -- the paths the timecode ASRC already
/// books), and the packet arrived within one packet plus [`AUDIO_RELABEL_ARRIVAL_JITTER_NS`] of the
/// previous one: a relabelling sender re-phases its emit earlier, never later, while a pause or a
/// restart shows the gap its stamps jumped by. Mirror of `genlock_audio_relabel_pending`.
pub fn audio_relabel_pending(
    stamp_jump_ns: i64,
    arrival_gap_ns: u64,
    packet_ns: u64,
    step_min_ns: i64,
) -> bool {
    let jump = stamp_jump_ns.unsigned_abs();
    jump > step_min_ns.unsigned_abs()
        && jump > packet_ns
        && arrival_gap_ns <= packet_ns.saturating_add(AUDIO_RELABEL_ARRIVAL_JITTER_NS)
}

/// Issue 1381 (design 5901213031) — does this packet START a pending relabel, read on the skew-hold
/// state BEFORE [`audio_step_hold`] takes it (the ingest continues the source's timelines on it, the
/// hold starts the pending on the same predicate)?
///
/// Outside a hold, in timecode mode, with a previous timecode packet: this box's live offset moved by
/// at most `step_min_ns` (no receiver step on this packet -- that is the skew hold or a joint relabel),
/// the stamps jumped AWAY from this box's wall (their live-wall age leaves the one-packet band around
/// the nominal age; a jump that brings the age back is a late follow after an early age release,
/// today's path) and [`audio_relabel_pending`] holds for the stamp jump and the arrival gap. Mirror of
/// `genlock_audio_step_relabel_pending_starts`.
pub fn audio_step_relabel_pending_starts(
    s: &AudioStepHold,
    timecode: bool,
    off_live_ns: i64,
    raw_ts_ns: u64,
    packet_ns: u64,
    now_ns: u64,
    step_min_ns: i64,
) -> bool {
    if !timecode || s.active || s.prev_packet_ns == 0 {
        return false;
    }
    let stamp_jump_ns = raw_ts_ns.wrapping_sub(s.prev_raw_ns.wrapping_add(s.prev_packet_ns)) as i64;
    let age_dev_ns =
        audio_stamp_age_ns(now_ns, raw_ts_ns, off_live_ns).wrapping_sub(s.nominal_age_ns);
    off_live_ns.wrapping_sub(s.prev_off_ns).unsigned_abs() <= step_min_ns.unsigned_abs()
        && age_dev_ns.unsigned_abs() > packet_ns
        && audio_relabel_pending(
            stamp_jump_ns,
            now_ns.wrapping_sub(s.prev_arrival_ns),
            packet_ns,
            step_min_ns,
        )
}

/// Issue 1381 (design 5901213031, ROZHODNUTÉ on finding 5900705310) — what a relabel adds to the
/// source's placement SLEW (`genlock_audio_slew_remaining_ns`): its landing move (−r: the stamp jump
/// plus the offset jump of a joint or split relabel, a pending relabel's release residual), whatever
/// its size, when the packet APPENDS on the timecode ASRC path; else 0 (a placed packet lands on its
/// raw stamp, and without the timecode ASRC there is no resampler to pay a slew). The slew is paid at
/// 1000 ppm and every consumed step is booked out of the smoothing timeline, so a remainder of r ms
/// is repaid in r seconds -- never through the timecode ASRC's half-packet booking band, which left a
/// 15.8 ms remainder to the level loop for ~725 s. Mirror of `genlock_audio_relabel_book_ns`.
pub fn audio_relabel_book_ns(move_ns: i64, relabel: bool, appended: bool, asrc_tc: bool) -> i64 {
    if relabel && appended && asrc_tc {
        move_ns
    } else {
        0
    }
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
/// appending? When the move it applies is over one packet: a `Timeout` (of a hold, or of a pending
/// relabel: J applied once), or a `Followed` sender that caught up without jumping its stamps. A
/// jumped follow (within one packet) appends or books as usual, a `RelabelPending` release is within
/// one packet by construction (its remainder is slewed, [`audio_relabel_book_ns`]), and a `Reset`
/// packet is placed by the ingest's own timeline reset. Never a W-second payment at 1000 ppm for a
/// step. Mirror of `genlock_audio_step_release_places`.
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
