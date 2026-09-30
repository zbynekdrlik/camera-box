//! Issue 1381 (design 5901213031, receiver slice 2) — the unit tests of a PENDING relabel (the
//! sender's box stepped first: its stamps jumped by J while this box's live offset did not move) and
//! of the relabel remainder booked on the placement slew. A `#[path]` child of
//! `genlock_audio_pairing` beside `genlock_audio_pairing_step_tests.rs` (split to keep each file
//! under the ~1000-line budget).

use super::*;

const WALL: u64 = 1_790_000_000_123_456_789;
const PACKET: u64 = 33_333_333;
const MIN: i64 = 2_000_000;
const OFF: i64 = -1_789_950_000_000_000_000;
const BASE: u64 = 1_000_000_000_000;
/// Past the nominal age's warm-up (the seed plus `AUDIO_STEP_NOMINAL_WARM_PACKETS`).
const WARM: u64 = AUDIO_STEP_NOMINAL_WARM_PACKETS as u64 + 10;
/// The design's scripted steps: the live +260 ms and +682 ms, and one past each OBS limit.
const STEPS: [i64; 4] = [260_000_000, 682_474_000, -1_500_000_000, 2_500_000_000];

/// One slot of a 30 fps sender (1600 samples at 48 kHz) on the per-second grid.
fn slot_ns(k: i64) -> i64 {
    k * 1_000_000_000 / 30
}

/// A relabelling sender's stamp jump at a wall step of `step_ns`: N = floor(S / slot) slots.
fn relabel_jump(step_ns: i64) -> i64 {
    slot_ns((i128::from(step_ns) * 30).div_euclid(1_000_000_000) as i64)
}

/// A timecode feed: packet `k` carries the stamp `WALL + k·PACKET + shift` and arrives `early` ns
/// before `BASE + k·PACKET`; `off` is this box's live wall→mono offset.
struct Feed {
    s: AudioStepHold,
    k: u64,
    shift: i64,
    off: i64,
    early: u64,
}

impl Feed {
    fn new() -> Self {
        Feed {
            s: AudioStepHold::default(),
            k: 0,
            shift: 0,
            off: OFF,
            early: 0,
        }
    }

    fn raw(&self) -> u64 {
        (WALL + self.k * PACKET).wrapping_add(self.shift as u64)
    }

    fn now(&self) -> u64 {
        BASE + self.k * PACKET - self.early
    }

    /// What the ingest asks before the hold takes this packet.
    fn starts(&self) -> bool {
        audio_step_relabel_pending_starts(
            &self.s,
            true,
            self.off,
            self.raw(),
            PACKET,
            self.now(),
            MIN,
        )
    }

    fn take(&mut self) -> (i64, AudioStepRelease) {
        self.take_with(true, false)
    }

    /// One packet with an explicit timecode flag and timeline reset.
    fn take_with(&mut self, timecode: bool, reset: bool) -> (i64, AudioStepRelease) {
        let (raw, now) = (self.raw(), self.now());
        let r = audio_step_hold(
            &mut self.s,
            timecode,
            self.off,
            raw,
            PACKET,
            now,
            reset,
            MIN,
        );
        self.k += 1;
        r
    }

    /// `n` packets that must map through the live offset with no release.
    fn steady(&mut self, n: u64) {
        for _ in 0..n {
            let k = self.k;
            assert_eq!(
                self.take(),
                (self.off, AudioStepRelease::None),
                "issue 1381: packet {k} must map through the live offset"
            );
            assert!(!self.s.active);
        }
    }

    /// The landing of packet `k` through `off`: continuous when it equals the pre-jump mapping.
    fn continuous(&self, off: i64) -> bool {
        let k = self.k - 1;
        let raw = (WALL + k * PACKET).wrapping_add(self.shift as u64);
        raw.wrapping_add(off as u64) == (WALL + k * PACKET).wrapping_add(OFF as u64)
    }
}

#[test]
fn a_stamp_only_jump_of_more_than_one_packet_with_continuous_arrival_is_a_pending_relabel_1381() {
    let p = PACKET;
    let j = AUDIO_RELABEL_ARRIVAL_JITTER_NS;
    // a relabelling sender's jump at each scripted step: its emit re-phases r EARLIER
    for step in STEPS {
        let jump = relabel_jump(step);
        let r = (step - jump) as u64;
        assert!(
            audio_relabel_pending(jump, p - r, p, MIN),
            "issue 1381: a sender that stepped {} ms first relabels by N slots with continuous arrival",
            step as f64 / 1e6
        );
    }
    // the arrival bound: one packet plus the jitter budget, inclusive
    assert!(audio_relabel_pending(666_666_666, p + j, p, MIN));
    assert!(!audio_relabel_pending(666_666_666, p + j + 1, p, MIN));
    // under one packet, or one packet BACK, is a duplicated slot (a dup resends the same stamp a few
    // ms later) or a raw-clock sender's submission jitter: the timecode ASRC books those, as today
    for (jump, gap) in [
        (-(p as i64), 3_000_000),
        (3_000_000, p),
        (-10_000_000, p),
        (MIN + 1, p),
    ] {
        assert!(
            !audio_relabel_pending(jump, gap, p, MIN),
            "issue 1381: jump {jump} gap {gap} must keep today's path"
        );
    }
    // ROZHODNUTÉ 5902983227 (slice 3) reverses slice 2 here: a FORWARD jump of exactly one packet
    // with continuous arrival is a one-slot sender-first step (N = +1), a pending relabel
    assert!(audio_relabel_pending(p as i64, p, p, MIN));
    assert!(audio_relabel_pending(p as i64 + 1, p, p, MIN));
    assert!(audio_relabel_pending(-(p as i64) - 1, 0, p, MIN));
    // a pause, a restart and the 1367 stamp leap (a missing slot + 47 ms): the arrival gap shows it
    for (jump, gap) in [
        (500_000_000_i64, 533_333_333),
        (3_000_000_000, 3_033_333_333),
        (80_000_000, 66_666_666),
        (66_666_667, 2 * p + 1),
    ] {
        assert!(
            !audio_relabel_pending(jump, gap, p, MIN),
            "issue 1381: jump {jump} after a {gap} ns arrival gap is a pause, never a relabel"
        );
    }
    // the 2 ms step minimum still applies with packets under it (1 ms packets)
    assert!(!audio_relabel_pending(MIN, 1_000_000, 1_000_000, MIN));
    assert!(audio_relabel_pending(MIN + 1, 1_000_000, 1_000_000, MIN));
    // two's-complement extremes never panic (the bound saturates like the C)
    assert!(audio_relabel_pending(i64::MIN, 0, 1, 0));
    assert!(!audio_relabel_pending(i64::MIN, u64::MAX, u64::MAX, 0));
    assert!(!audio_relabel_pending(
        i64::MAX,
        u64::MAX,
        u64::MAX - 1,
        MIN
    ));
    assert!(!audio_relabel_pending(5, 0, 1, i64::MIN));
}

#[test]
fn a_sender_first_relabel_appends_continuously_until_the_receiver_step_releases_it_1381() {
    // the sender's box steps first by S: its stamps jump N slots (J), its emit re-phases r earlier;
    // this box's own step comes 0.5 s or 3 s later and lands the packets within one packet
    for step in STEPS {
        for lag_packets in [15_u64, 90] {
            let mut f = Feed::new();
            f.steady(WARM);
            let jump = relabel_jump(step);
            let r = step - jump;
            f.shift = jump;
            f.early = r as u64;
            assert!(
                f.starts(),
                "issue 1381: step {step}: the stamp-only jump starts a pending relabel"
            );
            let (off, rel) = f.take();
            assert_eq!(
                (off, rel),
                (OFF - jump, AudioStepRelease::None),
                "step {step}"
            );
            assert!(
                f.s.active && f.s.relabel_pending && f.s.step_ns == jump && f.continuous(off),
                "issue 1381: step {step}: held on the continuous timeline (the stamps read -J): {:?}",
                f.s
            );
            for _ in 0..lag_packets {
                assert!(!f.starts(), "a pending never starts a second one");
                let (off, rel) = f.take();
                assert_eq!(
                    (off, rel),
                    (OFF - jump, AudioStepRelease::None),
                    "step {step}"
                );
                assert!(f.continuous(off) && f.s.active && f.s.relabel_pending);
            }
            // this box's own step: the landing moves by the remainder only
            f.off = OFF - step;
            let (off, rel) = f.take();
            assert_eq!(
                (off, rel),
                (OFF - step, AudioStepRelease::RelabelPending),
                "issue 1381: step {step} lag {lag_packets}: the receiver's step resolves the pending"
            );
            let residual = audio_step_residual_ns(f.s.held_off_ns, off);
            assert!(
                residual == -r
                    && !f.s.active
                    && f.s.relabel_pending
                    && !audio_step_release_places(rel, residual, PACKET),
                "issue 1381: step {step}: released with the landing move -r, never placed (the \
                 pending flag kept for the log's pending=)"
            );
            // from then on the live offset, and the age is back on its nominal: no hold ever again
            f.steady(300);
        }
    }
}

#[test]
fn a_pending_relabel_the_receiver_never_follows_times_out_and_applies_the_jump_once_1381() {
    let mut f = Feed::new();
    f.steady(WARM);
    let jump = relabel_jump(682_474_000);
    f.shift = jump;
    f.take();
    let start = f.s.start_ns;
    let mut timed_out = false;
    while !timed_out {
        let at = f.now();
        let (off, rel) = f.take();
        if at - start < AUDIO_STEP_HOLD_MAX_NS {
            assert_eq!(
                (off, rel),
                (OFF - jump, AudioStepRelease::None),
                "held until the bound"
            );
        } else {
            assert_eq!(
                (off, rel),
                (OFF, AudioStepRelease::Timeout),
                "issue 1381: released at the bound, onto the live offset"
            );
            let residual = audio_step_residual_ns(f.s.held_off_ns, off);
            assert!(
                residual == jump && audio_step_release_places(rel, residual, PACKET),
                "issue 1381: the timeout applies J once (a placement), like the hold's timeout"
            );
            assert!(
                !f.s.active && f.s.relabel_pending,
                "kept for the log's pending="
            );
            timed_out = true;
        }
    }
}

#[test]
fn only_a_receiver_step_that_follows_the_jump_within_one_packet_resolves_it_1381() {
    let jump = relabel_jump(682_474_000);
    // exactly one packet off is NOT a resolution (strict, like audio_relabel); one ns less is
    for (miss, resolves) in [(PACKET as i64, false), (PACKET as i64 - 1, true)] {
        let mut f = Feed::new();
        f.steady(WARM);
        f.shift = jump;
        f.take();
        f.off = OFF - jump - miss;
        let (_, rel) = f.take();
        assert_eq!(
            rel == AudioStepRelease::RelabelPending,
            resolves,
            "issue 1381: a receiver step {miss} ns off the jump"
        );
    }
    // a receiver step that does not match the jump leaves it pending until the bound
    let mut f = Feed::new();
    f.steady(WARM);
    f.shift = jump;
    f.take();
    f.off = OFF - 100_000_000;
    let (off, rel) = f.take();
    assert_eq!((off, rel), (OFF - jump, AudioStepRelease::None));
    assert!(f.s.active && f.s.relabel_pending);
    // an offset that DRIFTS (every packet under the 2 ms step minimum) never resolves it, even onto
    // the held offset: a jump of one packet + 1 ms, then 0.5 ms of drift per packet
    let mut f = Feed::new();
    f.steady(WARM);
    let jump = PACKET as i64 + 1_000_000;
    f.shift = jump;
    f.take();
    assert!(f.s.active && f.s.relabel_pending);
    for _ in 0..6 {
        f.off -= 500_000;
        assert_eq!(
            f.take().1,
            AudioStepRelease::None,
            "a drift never releases it"
        );
    }
    // the flag outlives a release (the log's pending=), so a RUNNING pending is active && flag
    assert!(
        f.s.active && f.s.relabel_pending && (f.off - f.s.held_off_ns).unsigned_abs() < PACKET,
        "issue 1381: no offset step: still pending although the drift landed within one packet"
    );
}

#[test]
fn a_pause_a_dup_or_a_skipped_slot_inside_a_pending_never_moves_its_held_offset_1381() {
    // review round 1: the pending reused the skew hold's fold, so every stamp jump over 2 ms moved the
    // held offset -- a 500 ms pause inside the pending window was folded away, the receiver's step then
    // missed the held offset by the pause and the pending ran to the bound (484 ms placed). Only a
    // relabel-shaped jump (over one packet, continuous arrival) moves it now: a pause (its arrival
    // gap), a duplicated slot (one packet back, 3 ms later) or a skipped slot (one packet on, after a
    // gap) keep it, and this box's own step still resolves the pending within one packet.
    let step = 682_474_000_i64;
    let jump = relabel_jump(step);
    let r = step - jump;
    for event in ["pause 500 ms", "dup", "skipped slot"] {
        let mut f = Feed::new();
        f.steady(WARM);
        f.shift = jump;
        f.early = r as u64;
        f.take();
        for _ in 0..5 {
            f.take();
        }
        let held = f.s.held_off_ns;
        match event {
            "pause 500 ms" => f.k += 15,
            "skipped slot" => f.k += 1,
            _ => {
                // the previous slot's stamp again, 3 ms after it
                let raw = (WALL + (f.k - 1) * PACKET).wrapping_add(f.shift as u64);
                let now = BASE + (f.k - 1) * PACKET - f.early + 3_000_000;
                let (off, rel) =
                    audio_step_hold(&mut f.s, true, f.off, raw, PACKET, now, false, MIN);
                assert_eq!((off, rel), (held, AudioStepRelease::None), "{event}");
            }
        }
        for _ in 0..5 {
            let (off, rel) = f.take();
            assert_eq!((off, rel), (held, AudioStepRelease::None), "{event}");
        }
        assert!(
            f.s.active && f.s.relabel_pending && f.s.held_off_ns == held,
            "issue 1381: {event} inside a pending relabel must keep its held offset: {:?}",
            f.s
        );
        f.off = OFF - step;
        let (off, rel) = f.take();
        assert_eq!(
            (off, rel),
            (OFF - step, AudioStepRelease::RelabelPending),
            "issue 1381: {event}: this box's own step must still resolve the pending"
        );
        assert_eq!(audio_step_residual_ns(f.s.held_off_ns, off), -r, "{event}");
    }
    // a second relabel-shaped jump inside the pending (the sender stepped again) does move it
    let mut f = Feed::new();
    f.steady(WARM);
    f.shift = jump;
    f.take();
    f.shift += jump;
    let (off, _) = f.take();
    assert_eq!(
        off,
        OFF - 2 * jump,
        "issue 1381: a relabel-shaped jump moves the held offset"
    );
}

#[test]
fn a_late_step_packet_of_a_raw_clock_sender_folds_back_on_the_next_packet_1381() {
    // review round 2: a raw-clock sender (its stamps are its submission wall) whose box steps first
    // submits the step-carrying packet L late -- its stamp and its arrival. The pending takes S + L
    // as its jump; the next on-time packet moves the stamps back by -L, a sub-packet move that folds
    // like the skew hold's. This box's own step then resolves the pending with no residual (left in
    // the held offset, +L was booked on the slew and sat under the ASRC's band for minutes).
    let step = 682_474_000_i64;
    for late in [3_000_000_u64, 8_000_000, 14_000_000] {
        let mut f = Feed::new();
        f.steady(WARM);
        f.shift = step;
        let (raw, now) = (f.raw().wrapping_add(late), f.now() + late);
        let (off, rel) = audio_step_hold(&mut f.s, true, f.off, raw, PACKET, now, false, MIN);
        assert!(
            rel == AudioStepRelease::None
                && f.s.active
                && f.s.relabel_pending
                && off == OFF - step - late as i64,
            "issue 1381: late {late}: the pending starts on S + L: {:?}",
            f.s
        );
        f.k += 1;
        let (off, rel) = f.take();
        assert_eq!(
            (off, rel),
            (OFF - step, AudioStepRelease::None),
            "issue 1381: late {late}: the on-time packet folds the -L back"
        );
        for _ in 0..10 {
            assert_eq!(
                f.take(),
                (OFF - step, AudioStepRelease::None),
                "late {late}"
            );
        }
        f.off = OFF - step;
        let (off, rel) = f.take();
        assert_eq!(
            (off, rel, audio_step_residual_ns(f.s.held_off_ns, off)),
            (OFF - step, AudioStepRelease::RelabelPending, 0),
            "issue 1381: late {late}: this box's step resolves the pending with no residual"
        );
    }
}

#[test]
fn a_skew_hold_after_a_pending_relabel_takes_the_ordinary_releases_1381() {
    // the kept pending flag is cleared when the next hold starts: a receiver-first step after a
    // resolved pending relabel is an ordinary skew hold, released followed when the stamps jump
    let jump = relabel_jump(682_474_000);
    let mut f = Feed::new();
    f.steady(WARM);
    f.shift = jump;
    f.take();
    f.off = OFF - 682_474_000;
    assert_eq!(f.take().1, AudioStepRelease::RelabelPending);
    f.steady(40);
    f.off -= 89_703_000;
    let (off, rel) = f.take();
    assert_eq!(rel, AudioStepRelease::None);
    assert!(
        f.s.active && !f.s.relabel_pending,
        "an ordinary skew hold: {:?}",
        f.s
    );
    assert_eq!(off, OFF - 682_474_000);
    f.shift += 89_703_000;
    let (off, rel) = f.take();
    assert_eq!(
        (off, rel),
        (f.off, AudioStepRelease::Followed),
        "issue 1381: the ordinary follow release applies again"
    );
}

#[test]
fn a_pending_relabel_ends_on_a_timeline_reset_or_outside_timecode_1381() {
    let jump = relabel_jump(682_474_000);
    let mut f = Feed::new();
    f.steady(WARM);
    f.shift = jump;
    f.take();
    let r = f.take_with(true, true);
    assert_eq!(r, (OFF, AudioStepRelease::Reset));
    assert!(
        !f.s.active && f.s.relabel_pending,
        "kept for the log's pending="
    );
    let mut f = Feed::new();
    f.steady(WARM);
    f.shift = jump;
    f.take();
    let r = f.take_with(false, false);
    assert_eq!(r, (OFF, AudioStepRelease::Reset));
    assert!(
        !f.s.active && f.s.relabel_pending,
        "kept for the log's pending="
    );
    // and a timeline-reset packet never starts one
    let mut f = Feed::new();
    f.steady(WARM);
    f.shift = jump;
    assert!(f.starts());
    let r = f.take_with(true, true);
    assert_eq!(r, (OFF, AudioStepRelease::None));
    assert!(!f.s.active);
}

#[test]
fn a_dup_jitter_pause_leap_joint_step_or_late_follow_never_starts_a_pending_1381() {
    let p = PACKET as i64;
    // (stamp shift from here on, arrival shift, live offset) on the packet after a steady feed
    let cases: [(&str, i64, i64, i64); 7] = [
        ("dup", -p, p - 3_000_000, OFF),
        ("submission jitter", 3_000_000, 0, OFF),
        ("pause 500 ms", 500_000_000, -500_000_000, OFF),
        ("stamp leap", 80_000_000, -p, OFF),
        ("restart 3 s", 3_000_000_000, -3_000_000_000, OFF),
        ("joint step", 666_666_666, 0, OFF - 682_474_000),
        // the receiver steps on the same packet the stamps jump by a different amount: the skew
        // hold's start, never a pending relabel (its offset moved)
        (
            "receiver step + partial stamp jump",
            100_000_000,
            0,
            OFF - 682_474_000,
        ),
    ];
    for (name, shift, arrive, off) in cases {
        let mut f = Feed::new();
        f.steady(WARM);
        f.shift = shift;
        f.off = off;
        // arrive > 0 = earlier than the grid, < 0 = later
        let now = (f.now() as i64 - arrive) as u64;
        let raw = f.raw();
        assert!(
            !audio_step_relabel_pending_starts(&f.s, true, off, raw, PACKET, now, MIN),
            "issue 1381: {name} must keep today's path"
        );
        audio_step_hold(&mut f.s, true, off, raw, PACKET, now, false, MIN);
        assert!(
            !f.s.relabel_pending,
            "issue 1381: {name} started a pending relabel"
        );
    }
    // no previous packet, outside timecode mode, inside a running hold
    let s = AudioStepHold::default();
    assert!(!audio_step_relabel_pending_starts(
        &s, true, OFF, WALL, PACKET, BASE, MIN
    ));
    let mut f = Feed::new();
    f.steady(WARM);
    f.shift = relabel_jump(682_474_000);
    assert!(!audio_step_relabel_pending_starts(
        &f.s,
        false,
        OFF,
        f.raw(),
        PACKET,
        f.now(),
        MIN
    ));
    // a receiver-first step whose sender never follows: held, then released at the bound (placed);
    // the sender's LATE follow then brings the stamps' age BACK to the nominal -- a follow, never a
    // pending relabel (the heavy-jitter early release of the step bench is the same shape)
    let mut f = Feed::new();
    f.steady(WARM);
    f.off = OFF - 682_474_000;
    let mut released = false;
    for _ in 0..400 {
        released |= f.take().1 == AudioStepRelease::Timeout;
    }
    assert!(released && !f.s.active);
    f.shift = 682_474_000;
    assert!(
        !f.starts(),
        "issue 1381: a late follow brings the age back to the nominal: never a pending relabel"
    );
    f.take();
    assert!(!f.s.active);
}

#[test]
fn the_ingest_predicate_and_the_hold_start_agree_1381() {
    // one scripted feed through every shape: whenever the ingest's predicate reads true on the state
    // before the hold, the hold starts a pending relabel on that packet, and never otherwise
    let mut f = Feed::new();
    let mut script: Vec<(i64, i64, u64)> = Vec::new(); // (shift, off, early) per packet
    let (mut shift, mut off) = (0_i64, OFF);
    for i in 0..1200_u64 {
        match i {
            60 => shift += relabel_jump(260_000_000),
            80 => off -= 260_000_000,
            200 => shift -= PACKET as i64,
            300 => shift += relabel_jump(-1_500_000_000),
            700 => off += 1_500_000_000,
            800 => {
                shift += relabel_jump(2_500_000_000);
                off -= 2_500_000_000;
            }
            900 => off -= 50_000_000,
            1000 => shift += 50_000_000,
            _ => {}
        }
        script.push((shift, off, 0));
    }
    let mut pendings = 0;
    for (shift, off, early) in script {
        f.shift = shift;
        f.off = off;
        f.early = early;
        let predicted = f.starts();
        let was_active = f.s.active;
        let (_, rel) = f.take();
        let started = !was_active && f.s.active && f.s.relabel_pending;
        assert_eq!(
            predicted,
            started,
            "issue 1381: packet {}: the ingest predicate and the hold disagree",
            f.k - 1
        );
        if started {
            assert_eq!(rel, AudioStepRelease::None);
            pendings += 1;
        }
    }
    assert!(
        pendings >= 2,
        "the script reaches {pendings} pending relabels"
    );
}

#[test]
fn a_relabel_books_its_whole_remainder_on_the_slew_when_it_appends_1381() {
    // the ROZHODNUTÉ: every remainder, under the timecode ASRC's 16.7 ms band or over it
    for mv in [-15_807_333_i64, -26_666_667, -1, 0, 13_333_333] {
        assert_eq!(audio_relabel_book_ns(mv, true, true, true), mv);
    }
    for (relabel, appended, tc) in [
        (false, true, true),
        (true, false, true),
        (true, true, false),
        (false, false, false),
    ] {
        assert_eq!(
            audio_relabel_book_ns(-15_807_333, relabel, appended, tc),
            0,
            "issue 1381: relabel {relabel} appended {appended} asrc_tc {tc} books nothing"
        );
    }
    assert_eq!(audio_relabel_book_ns(i64::MIN, true, true, true), i64::MIN);
}

#[test]
fn the_pending_release_has_its_own_token_and_never_places_1381() {
    assert_eq!(AudioStepRelease::RelabelPending as u8, 4);
    assert_eq!(AudioStepRelease::RelabelPending.token(), "relabel-pending");
    for residual in [0, -15_807_333, PACKET as i64 + 1, i64::MIN] {
        assert!(!audio_step_release_places(
            AudioStepRelease::RelabelPending,
            residual,
            PACKET
        ));
    }
    assert_eq!(AUDIO_RELABEL_ARRIVAL_JITTER_NS, 15_000_000);
}

// Slice 3 (design 5902870861, ROZHODNUTÉ 5902983227): a sender-first step of ONE slot (N = +1).

/// The one-slot sender-first steps: S from just over one slot to just under two (N = +1).
const ONE_SLOT_STEPS: [i64; 5] = [35_000_000, 40_000_000, 50_000_000, 60_000_000, 66_000_000];

/// The two stamp jumps an N = +1 relabel makes on the sender's 100 ns per-second grid (its 30 fps
/// slots are 33 333 300 / 33 333 300 / 33 333 400 ns): one packet + 34 ns on two grid positions,
/// one packet − 66 ns on the third.
const ONE_SLOT_JUMPS: [i64; 2] = [PACKET as i64 + 34, PACKET as i64 - 66];

#[test]
fn a_one_slot_forward_jump_with_continuous_arrival_is_a_pending_relabel_1381() {
    let p = PACKET;
    let pi = p as i64;
    let j = AUDIO_RELABEL_ARRIVAL_JITTER_NS;
    assert_eq!(
        AUDIO_RELABEL_FORWARD_TOLERANCE_NS, 100,
        "one NDI timecode unit"
    );
    // forward: one packet − 100 ns and up, continuous arrival (the emit re-phased r earlier)
    for jump in [pi - 66, pi + 34, pi - 100, pi, pi + 1] {
        assert!(
            audio_relabel_pending(jump, p - 6_666_733, p, MIN),
            "issue 1381: a forward stamp jump of {jump} ns with continuous arrival is a pending \
             relabel"
        );
    }
    assert!(
        !audio_relabel_pending(pi - 101, p, p, MIN),
        "issue 1381: one ns more than one NDI unit under one packet is not a slot"
    );
    // the arrival still decides: a skipped slot jumps the same one packet, but its arrival gaps
    assert!(!audio_relabel_pending(pi - 66, 2 * p, p, MIN));
    assert!(!audio_relabel_pending(pi + 34, 2 * p, p, MIN));
    assert!(audio_relabel_pending(pi - 66, p + j, p, MIN));
    assert!(!audio_relabel_pending(pi - 66, p + j + 1, p, MIN));
    // backward still needs MORE than one packet: a duplicated slot and an N = −1 relabel are
    // exactly one packet back, and nothing less than that
    for jump in [-pi, -(pi - 66), -(pi - 100), -(pi + 1) + 1] {
        assert!(
            !audio_relabel_pending(jump, p, p, MIN),
            "issue 1381: a backward stamp jump of {jump} ns keeps today's path"
        );
    }
    assert!(audio_relabel_pending(-pi - 1, p, p, MIN));
    // packets shorter than the tolerance: the 2 ms step minimum still rules
    assert!(!audio_relabel_pending(MIN, 50, 50, MIN));
    assert!(audio_relabel_pending(MIN + 1, 50, 50, MIN));
}

#[test]
fn a_one_slot_sender_first_step_resolves_at_the_receiver_step_1381() {
    // slice 2 missed the one-packet − 66 ns jump (not over one packet): its stamps were appended and
    // this box's later step PLACED the packet (a zero-length `followed` release, residual −S)
    for step in ONE_SLOT_STEPS {
        for jump in ONE_SLOT_JUMPS {
            for lag_packets in [15_u64, 90] {
                let mut f = Feed::new();
                f.steady(WARM);
                let r = step - jump;
                f.shift = jump;
                f.early = r as u64;
                assert!(
                    f.starts(),
                    "issue 1381: step {step} jump {jump}: a one-slot sender-first step starts a \
                     pending relabel"
                );
                assert_eq!(
                    f.take(),
                    (OFF - jump, AudioStepRelease::None),
                    "step {step} jump {jump}"
                );
                assert!(f.s.active && f.s.relabel_pending && f.s.step_ns == jump);
                for _ in 0..lag_packets {
                    assert_eq!(
                        f.take(),
                        (OFF - jump, AudioStepRelease::None),
                        "step {step} jump {jump}: held on the continuous timeline"
                    );
                }
                f.off = OFF - step;
                let (off, rel) = f.take();
                assert_eq!(
                    (off, rel),
                    (OFF - step, AudioStepRelease::RelabelPending),
                    "issue 1381: step {step} jump {jump} lag {lag_packets}: this box's own step of \
                     −(one slot + r) resolves the pending"
                );
                let residual = audio_step_residual_ns(f.s.held_off_ns, off);
                assert!(
                    residual == -r && !audio_step_release_places(rel, residual, PACKET),
                    "issue 1381: step {step} jump {jump}: released with −r, never placed"
                );
                f.steady(100);
            }
        }
    }
}

#[test]
fn the_pending_age_band_is_half_a_packet_1381() {
    // the stamps must jump AWAY from this box's wall by more than HALF a packet: a one-slot step's
    // stamp age sits about one slot off (−S), so a late packet's arrival jitter no longer decides it
    let p = PACKET as i64;
    // the age is `arrival shift − stamp jump`: this arrival shift puts a −(one packet + 1 ms) jump's
    // age exactly on half a packet (p / 2 truncates, so it is spelled out, never approximated)
    let edge = p / 2 - p - 1_000_000;
    // (stamp jump, arrival later than the grid (< 0 = earlier), starts)
    let cases: [(i64, i64, bool); 5] = [
        // over one packet, 10 ms late: age −(J − 10 ms) = −24.3 ms -- inside the old one-packet band
        (p + 1_000_000, 10_000_000, true),
        // one packet − 66 ns, 5 ms late: age −28.3 ms
        (p - 66, 5_000_000, true),
        // backward, 17.7 ms early: age exactly half a packet (strict) -- and one ns more
        (-p - 1_000_000, edge, false),
        (-p - 1_000_000, edge + 1, true),
        // an age back inside the band (5 ms under half a packet): never a pending start
        (-p - 1_000_000, edge - 5_000_000, false),
    ];
    for (jump, late, starts) in cases {
        let mut f = Feed::new();
        f.steady(WARM);
        f.shift = jump;
        let now = (f.now() as i64 + late) as u64;
        assert_eq!(
            audio_step_relabel_pending_starts(&f.s, true, f.off, f.raw(), PACKET, now, MIN),
            starts,
            "issue 1381: jump {jump} arriving {late} ns late: age {} ns",
            late - jump
        );
    }
}

/// The slice-2 pending-start decision (design 5901213031) verbatim: the reference the slice-3 start
/// must contain.
fn slice2_relabel_pending(jump_ns: i64, gap_ns: u64, packet_ns: u64, min_ns: i64) -> bool {
    let jump = jump_ns.unsigned_abs();
    jump > min_ns.unsigned_abs()
        && jump > packet_ns
        && gap_ns <= packet_ns.saturating_add(AUDIO_RELABEL_ARRIVAL_JITTER_NS)
}

#[test]
fn the_slice_3_start_only_adds_the_one_slot_forward_jump_and_the_half_packet_age_1381() {
    // every packet slice 2 started a pending on still starts one, and the only new starts are a
    // forward jump of one packet − 100 ns ..= one packet, or an age between half a packet and one
    // packet. So a skipped slot (an arrival gap), a duplicated slot and an N = −1 relabel (one packet
    // back) -- none of them a slice-2 start, none in either new region -- keep the slice-2 path byte
    // for byte (the bench pins the traces too).
    let p = PACKET as i64;
    let mut jumps = vec![0_i64, 3_000_000, 10_000_000, 40_000_000, 500_000_000, 2 * p];
    for d in [0_i64, 1, 34, 66, 99, 100, 101, 1_000_000] {
        jumps.extend([p - d, p + d]);
    }
    let jumps: Vec<i64> = jumps.iter().flat_map(|&j| [j, -j]).collect();
    let (mut added_forward, mut added_age) = (0, 0);
    for jump in jumps {
        for late in [
            -20_000_000_i64,
            -(p / 2 + 1_000_001),
            -5_000_000,
            0,
            5_000_000,
            10_000_000,
            15_000_000,
            15_000_001,
            p,
        ] {
            for off_move in [0_i64, -1_000_000, -40_000_000] {
                let mut f = Feed::new();
                f.steady(WARM);
                f.shift = jump;
                f.off = OFF + off_move;
                let now = (f.now() as i64 + late) as u64;
                let raw = f.raw();
                let new =
                    audio_step_relabel_pending_starts(&f.s, true, f.off, raw, PACKET, now, MIN);
                let age_dev = audio_stamp_age_ns(now, raw, f.off).wrapping_sub(f.s.nominal_age_ns);
                let stamp_jump =
                    raw.wrapping_sub(f.s.prev_raw_ns.wrapping_add(f.s.prev_packet_ns)) as i64;
                let old = f.off.wrapping_sub(f.s.prev_off_ns).unsigned_abs() <= MIN.unsigned_abs()
                    && age_dev.unsigned_abs() > PACKET
                    && slice2_relabel_pending(
                        stamp_jump,
                        now.wrapping_sub(f.s.prev_arrival_ns),
                        PACKET,
                        MIN,
                    );
                assert!(
                    !old || new,
                    "issue 1381: jump {jump} late {late} off {off_move}: a slice-2 start must \
                     still start"
                );
                if new && !old {
                    let forward_slot = stamp_jump > 0 && stamp_jump >= p - 100 && stamp_jump <= p;
                    let age_band =
                        age_dev.unsigned_abs() > PACKET / 2 && age_dev.unsigned_abs() <= PACKET;
                    assert!(
                        forward_slot || age_band,
                        "issue 1381: jump {jump} late {late} off {off_move}: a new start outside \
                         the one-slot forward jump and the half-packet age band"
                    );
                    added_forward += usize::from(forward_slot);
                    added_age += usize::from(age_band);
                }
            }
        }
    }
    assert!(
        added_forward > 0 && added_age > 0,
        "the sweep must reach both widenings: {added_forward} forward, {added_age} age"
    );
}
