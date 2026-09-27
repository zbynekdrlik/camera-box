//! Issue 1367 (design 5845361166, Approach 1) — the TIMECODE mode of the per-source ASRC.
//!
//! A genlock source whose audio the #1303/#1367 pairing places at its own NDI timecode
//! (`audio_hold=timecode`, the resolume `sp-*` inputs) must not be judged by ARRIVAL timing. The
//! arrival-based servo counted SongPlayer's send pacing, the network and the DistroAV receive thread
//! (a SHOWN source's receive thread adds several ms of jitter), read a skipped sender slot as a loss
//! while the buffer moved the other way (no 1000 ppm booking, ~130 ppm for ~9 min), and read a
//! restart catch-up as a +200…+288 ppm "rate". In timecode mode the caller (obs-source.c
//! `source_output_audio_data`) feeds the servo per packet:
//!
//! - the PLACEMENT error `e` = where the packet actually lands in the buffer minus where its RAW stamp
//!   says it belongs (before the 70 ms TS smoothing snap), plus the placement slew still owed (the
//!   #1367 hold slew is its own 1000 ppm term). It is the level the loop holds, at a setpoint of 0
//!   ([`RealtimeAsrcCompensator::observe_placement`] books its jumps);
//! - the stamp ADVANCE (raw stamp + the live wall→mono offset) between two APPENDED packets as the
//!   regression's master block, the previous packet's pre-resample duration as its raw advance — the
//!   rate follows the timestamps (≈ 0 on a correct sender). A placed packet or a non-positive advance
//!   adds no point and never flushes.
//!
//! A jump of `e` against the servo's own expectation (setpoint + smoothed error) of at least
//! max(half the packet, [`PLACE_JUMP_MIN_MS`]) is booked with its own sign into the issue 1372
//! `step_recover_ms` and paid at `STEP_RECOVER_PPM` (1 ms per second), the setpoint moving with the
//! booking so the P term, the restore arms and the unreachable bound never read it as an error. A
//! PLACED packet lands on its stamp, so what was still owed is dropped. Mirror of the C
//! `asrc_compensator_set_timecode` / `asrc_compensator_observe_placement` /
//! `asrc_compensator_take_step_recover_ppm` in `media-io/asrc-compensator.c` — keep numerically
//! identical (the parity gate `tests/asrc_compensator_parity_1367.rs`, scenario `tc`).

use super::*;

/// Issue 1367: the smallest placement jump, in ms, the timecode error books. The band is
/// max(half the packet, this): a whole-packet jump (a skipped or duplicated 1600-sample SongPlayer
/// slot = 33.3 ms) always books, sub-half stamp jitter never does, and a small-packet source cannot
/// book on a few ms of jitter. Mirror of asrc-compensator.h ASRC_PLACE_JUMP_MIN_MS — keep identical.
pub const PLACE_JUMP_MIN_MS: f64 = 10.0;

impl RealtimeAsrcCompensator {
    /// Issue 1367: enter or leave timecode mode (obs-source.c passes, per packet, whether the
    /// source's audio is placed by the genlock pairing at its timecode). A CHANGE discards the open
    /// window and flushes: the regression points, the capture and anything owed were measured on the
    /// other basis. Mirror of the C `asrc_compensator_set_timecode`.
    pub fn set_timecode(&mut self, timecode: bool) {
        if timecode == self.timecode {
            return;
        }
        self.timecode = timecode;
        self.window_raw_s = 0.0;
        self.window_master_s = 0.0;
        self.window_block_count = 0;
        self.window_level_sum_ms = 0.0;
        self.window_level_count = 0;
        self.regression_flush();
    }

    /// Issue 1367: whether the servo runs in timecode mode (the C `asrc:` line's `timecode=`).
    pub fn timecode(&self) -> bool {
        self.timecode
    }

    /// Issue 1367: cumulative booked placement jumps (the C `asrc:` line's `place_jumps=`).
    pub fn place_jump_count(&self) -> u32 {
        self.place_jump_count
    }

    /// Issue 1367: the most recent booked placement jump, ms (the C `asrc:` line's `last_jump_ms=`).
    pub fn last_place_jump_ms(&self) -> f64 {
        self.last_place_jump_ms
    }

    /// Issue 1367: set what is owed to `owed_ms`. The setpoint moves by the change of the owed amount
    /// (a booked loss lowers it to where the early audio sits, a payment walks it back) and the open
    /// window's readings move with it, so the window mean stays in one frame.
    fn step_recover_set(&mut self, owed_ms: f64) {
        let shift_ms = self.step_recover_ms - owed_ms;
        self.level_target_ms += shift_ms;
        self.window_level_sum_ms += shift_ms * f64::from(self.window_level_count);
        self.step_recover_ms = owed_ms;
    }

    /// Issue 1367 (design 5845361166): observe one packet's placement error `place_err_ms`
    /// (actual − intended, the owed placement slew excluded; negative = the audio sits EARLY) in
    /// timecode mode, BEFORE its reading enters the window. `packet_ms` is the packet's own duration,
    /// `placed` whether the ingest PLACED it at its stamp (not appended). A placed packet lands on its
    /// stamp: whatever was still owed is dropped. A jump of at least max(half the packet,
    /// [`PLACE_JUMP_MIN_MS`]) against the servo's expectation (setpoint + smoothed error) is booked
    /// with its own sign — an early packet is owed as a loss (stretch), a late one as a duplicate
    /// (compress) — and paid at `STEP_RECOVER_PPM` by the next accepted calls. Inert outside timecode
    /// mode and before the setpoint is captured. Mirror of the C `asrc_compensator_observe_placement`.
    pub fn observe_placement(&mut self, place_err_ms: f64, packet_ms: f64, placed: bool) {
        if !self.timecode || !self.level_captured {
            return;
        }
        if placed {
            self.step_recover_set(0.0);
        }
        let jump_ms = place_err_ms - (self.level_target_ms + self.level_err_ema_ms);
        let half_packet_ms = 0.5 * packet_ms;
        let band_ms = if half_packet_ms > PLACE_JUMP_MIN_MS {
            half_packet_ms
        } else {
            PLACE_JUMP_MIN_MS
        };
        if jump_ms.abs() < band_ms {
            return;
        }
        let owed_ms =
            (self.step_recover_ms - jump_ms).clamp(-STEP_RECOVER_MAX_MS, STEP_RECOVER_MAX_MS);
        // review round 1: at the owed cap a packet still beyond it books nothing and counts nothing
        if owed_ms == self.step_recover_ms {
            return;
        }
        self.step_recover_set(owed_ms);
        self.place_jump_count = self.place_jump_count.saturating_add(1);
        self.last_place_jump_ms = jump_ms;
    }

    /// Issue 1367: read and clear the recovery rate the last accepted call paid (servo sign). In
    /// timecode mode the servo runs in the ingest AFTER the packet was resampled, so the NEXT packet's
    /// resampler carries this payment exactly once. Mirror of the C
    /// `asrc_compensator_take_step_recover_ppm`.
    pub fn take_step_recover_ppm(&mut self) -> f64 {
        std::mem::take(&mut self.step_recover_ppm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKET_S: f64 = 1600.0 / 48000.0;
    const PACKET_MS: f64 = PACKET_S * 1000.0;

    /// A locked, captured timecode servo on a steady feed (placement error 0, stamps on time).
    fn locked_timecode() -> RealtimeAsrcCompensator {
        let mut c = RealtimeAsrcCompensator::new();
        c.set_timecode(true);
        for _ in 0..(120.0 / PACKET_S) as usize {
            c.observe_placement(0.0, PACKET_MS, false);
            c.compensate_with_level(PACKET_S, PACKET_S, 0.0);
            c.take_step_recover_ppm();
        }
        assert!(
            c.level_captured,
            "the servo must lock and capture within 120 s"
        );
        c
    }

    #[test]
    fn timecode_capture_is_zero_never_the_lock_time_depth_1367() {
        let mut c = RealtimeAsrcCompensator::new();
        c.set_level_absolute(false);
        c.set_timecode(true);
        for _ in 0..(120.0 / PACKET_S) as usize {
            // a steady 7 ms early placement: the level loop must hold it toward 0, not capture it
            c.observe_placement(-7.0, PACKET_MS, false);
            c.compensate_with_level(PACKET_S, PACKET_S, -7.0);
        }
        assert_eq!(
            c.level_target_ms(),
            0.0,
            "issue 1367: the timecode setpoint is 0"
        );
    }

    #[test]
    fn a_skipped_slot_is_booked_as_its_own_signed_loss_1367() {
        let mut c = locked_timecode();
        c.observe_placement(-PACKET_MS, PACKET_MS, false);
        assert!(
            (c.step_recover_ms() - PACKET_MS).abs() < 1e-9 && c.place_jump_count() == 1,
            "issue 1367: an early whole-packet jump is owed as a loss (stretch): owed {} jumps {}",
            c.step_recover_ms(),
            c.place_jump_count()
        );
        assert!(
            (c.level_target_ms() + PACKET_MS).abs() < 1e-9,
            "issue 1367: the setpoint moves with the booking, so the level loop reads no error"
        );
        // the next accepted call pays at the 1000 ppm budget, in the servo's sign (negative = stretch)
        c.compensate_with_level(PACKET_S, PACKET_S, -PACKET_MS);
        let paid_ppm = c.take_step_recover_ppm();
        assert!(
            (paid_ppm + STEP_RECOVER_PPM).abs() < 1e-6 && c.take_step_recover_ppm() == 0.0,
            "issue 1367: the booking pays at -STEP_RECOVER_PPM once per call: {paid_ppm}"
        );
    }

    #[test]
    fn a_duplicated_slot_is_booked_as_a_compress_1367() {
        let mut c = locked_timecode();
        c.observe_placement(PACKET_MS, PACKET_MS, false);
        assert!(
            (c.step_recover_ms() + PACKET_MS).abs() < 1e-9,
            "issue 1367: a late whole-packet jump is owed as a duplicate (compress): {}",
            c.step_recover_ms()
        );
    }

    #[test]
    fn jitter_under_half_a_packet_never_books_and_the_band_floor_holds_1367() {
        let mut c = locked_timecode();
        for e in [-16.0, 16.0, -9.9, 9.9] {
            c.observe_placement(e, PACKET_MS, false);
        }
        // a tiny-packet source keeps the 10 ms floor
        c.observe_placement(-9.9, 2.667, false);
        assert_eq!(
            c.place_jump_count(),
            0,
            "issue 1367: sub-band jitter must never book"
        );
        c.observe_placement(-10.0, 2.667, false);
        assert_eq!(
            c.place_jump_count(),
            1,
            "issue 1367: the floor band is live at 10 ms"
        );
    }

    #[test]
    fn a_placed_packet_drops_what_is_owed_and_books_no_jump_1367() {
        let mut c = locked_timecode();
        c.observe_placement(-PACKET_MS, PACKET_MS, false);
        assert!(c.step_recover_ms() > 0.0);
        // the ingest re-places the next packet at its stamp: it lands where it belongs (e = 0)
        c.observe_placement(0.0, PACKET_MS, true);
        assert!(
            c.step_recover_ms() == 0.0 && c.level_target_ms() == 0.0 && c.place_jump_count() == 1,
            "issue 1367: a placement lands on its stamp, so nothing stays owed and no jump is booked: \
             owed {} target {} jumps {}",
            c.step_recover_ms(),
            c.level_target_ms(),
            c.place_jump_count()
        );
    }

    #[test]
    fn a_setpoint_shift_is_a_no_op_in_timecode_mode_1367() {
        let mut c = locked_timecode();
        let before = c.level_target_ms();
        c.shift_level_target(12.0);
        assert_eq!(
            c.level_target_ms(),
            before,
            "issue 1367: the placement error already carries a deliberate move"
        );
        assert!(!c.level_restore(), "issue 1367: a no-op shift arms nothing");
    }

    #[test]
    fn entering_or_leaving_timecode_mode_flushes_1367() {
        let mut c = locked_timecode();
        c.observe_placement(-PACKET_MS, PACKET_MS, false);
        c.set_timecode(false);
        assert!(
            !c.level_captured && c.step_recover_ms() == 0.0 && !c.timecode(),
            "issue 1367: a mode change drops the capture and anything owed"
        );
        // inert outside the mode
        c.observe_placement(-PACKET_MS, PACKET_MS, false);
        assert_eq!(c.step_recover_ms(), 0.0);
    }

    #[test]
    fn timecode_mode_never_falls_back_to_a_nonzero_setpoint_1367() {
        // Review round 1: a placement error the loop cannot pull in (this plant ignores the stretch)
        // for longer than the #1355 unreachable bound. The arrival rule falls back to the live depth;
        // in timecode mode that would bake a lasting A/V offset in as the new truth, so the setpoint
        // must stay 0 and no fallback may fire.
        let mut c = locked_timecode();
        let windows = f64::from(LEVEL_TARGET_UNREACHABLE_WINDOWS) + 200.0;
        for _ in 0..(windows / PACKET_S) as usize {
            c.observe_placement(-8.0, PACKET_MS, false);
            c.compensate_with_level(PACKET_S, PACKET_S, -8.0);
            c.take_step_recover_ppm();
        }
        assert!(
            c.level_fallback_count() == 0
                && !c.take_level_fallback_pending()
                && c.level_target_ms() == 0.0,
            "issue 1367: the timecode setpoint stays 0: fallbacks {} target {}",
            c.level_fallback_count(),
            c.level_target_ms()
        );
    }

    #[test]
    fn a_jump_at_the_owed_cap_is_not_counted_again_1367() {
        // Review round 1: once the owed amount sits at the cap, a packet still beyond it books
        // nothing more, so it must not count as another placement jump either.
        let mut c = locked_timecode();
        c.observe_placement(-60.0, PACKET_MS, false);
        c.observe_placement(-120.0, PACKET_MS, false);
        assert!(
            c.step_recover_ms() == STEP_RECOVER_MAX_MS && c.place_jump_count() == 2,
            "issue 1367: the second jump books up to the cap: owed {} jumps {}",
            c.step_recover_ms(),
            c.place_jump_count()
        );
        for _ in 0..30 {
            c.observe_placement(-120.0, PACKET_MS, false);
        }
        assert!(
            c.place_jump_count() == 2 && c.level_target_ms() == -STEP_RECOVER_MAX_MS,
            "issue 1367: a packet beyond the cap books nothing and counts nothing: jumps {} target {}",
            c.place_jump_count(),
            c.level_target_ms()
        );
    }

    #[test]
    fn the_arrival_path_is_untouched_outside_timecode_mode_1367() {
        // Two identical servos on an arrival feed: one never touches the timecode API, the other
        // calls it with timecode off. Their state must be bit-identical every call.
        let mut a = RealtimeAsrcCompensator::new();
        let mut b = RealtimeAsrcCompensator::new();
        let mut buf = 100.0_f64;
        for i in 0..20_000_u32 {
            let master = 0.01 + f64::from(i % 7) * 1e-4;
            b.set_timecode(false);
            b.observe_placement(-50.0, 33.3, false);
            b.shift_level_target(0.0);
            let ra = a.compensate_with_level(0.01, master, buf);
            let rb = b.compensate_with_level(0.01, master, buf);
            assert_eq!(ra.to_bits(), rb.to_bits());
            buf += (ra - master) * 1000.0;
        }
        assert_eq!(a.applied_ppm().to_bits(), b.applied_ppm().to_bits());
        assert_eq!(a.level_target_ms().to_bits(), b.level_target_ms().to_bits());
        assert_eq!(b.place_jump_count(), 0);
    }
}
