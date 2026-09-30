//! Issue 1381 (design 5901213031, receiver slice 2) — an EXECUTABLE C-vs-Rust parity gate for the
//! PENDING relabel (`audio_relabel_pending` / `genlock_audio_relabel_pending`,
//! `audio_step_relabel_pending_starts` / `genlock_audio_step_relabel_pending_starts`, the skew hold's
//! pending state) and the relabel remainder booked on the placement slew (`audio_relabel_book_ns` /
//! `genlock_audio_relabel_book_ns`).
//!
//! Split out of `tests/genlock_audio_step_hold_parity_1381.rs` (review round 1, the ~1000-line
//! budget). Like it, this gate lifts the contiguous audio-pairing block of
//! `vendor/obs-studio/libobs/obs-source.c` VERBATIM (the shared `tests/genlock_audio_pairing_lift/mod.rs`),
//! compiles it under `-Wall -Wextra -Wconversion -Wformat=2 -Werror`, and requires the scalar vectors
//! and one pending script to give the same result from the C and the Rust authority. The script
//! covers: pending relabels resolved 15 and 90 packets later, a receiver step that misses and then
//! resolves, the one-packet and drift resolution edges, a pause / dup / skipped slot inside a
//! pending, a dup / pause / leap that never starts one, a reset packet, a pending ended by a reset
//! and by leaving timecode, the 10 s bound, and a late follow. It FAILS LOUDLY when no C compiler
//! is present.

use camera_box::genlock_audio_pairing::{
    audio_relabel_book_ns, audio_relabel_pending, audio_step_hold,
    audio_step_relabel_pending_starts, AudioStepHold, AudioStepRelease, AUDIO_STEP_HOLD_MAX_NS,
    AUDIO_STEP_NOMINAL_WARM_PACKETS,
};
use camera_box::genlock_wall_step::WALL_STEP_MIN_NS;

mod genlock_audio_pairing_lift;
use genlock_audio_pairing_lift::{compile, i64_lit, lift_block, run_lines};

const PACKET: u64 = 33_333_333;
const WALL: u64 = 1_790_000_000_123_456_789;
const MONO: u64 = 86_400_000_000_000;
const OFF: i64 = (MONO as i64).wrapping_sub(WALL as i64);

/// One packet: (timecode, live offset, raw stamp, packet duration, now, timeline reset).
type Pkt = (bool, i64, u64, u64, u64, bool);

/// Packets that warm the nominal age up after a seed (plus the seed itself).
const WARM: u64 = AUDIO_STEP_NOMINAL_WARM_PACKETS as u64 + 1;

/// A packet outside timecode mode: clears the state.
fn clear(v: &mut Vec<Pkt>) {
    v.push((false, 7, WALL, PACKET, MONO, false));
}

/// A relabelling sender's stamp jump at a wall step: N = floor(S / slot) slots, toward −∞.
fn relabel_stamp_jump(step_ns: i64) -> i64 {
    let n = (i128::from(step_ns) * 30).div_euclid(1_000_000_000) as i64;
    n * 1_000_000_000 / 30
}

/// Pending relabels and every shape that must NOT start one, on top of the hold script. The sender
/// re-phases its emit r earlier at each relabel (cumulative), so the stamps' age returns to its
/// nominal once this box's own step follows.
fn pending_sequence() -> Vec<Pkt> {
    let mut v: Vec<Pkt> = Vec::new();
    let mut k: u64 = 3_000_000;
    let (mut off, mut shift, mut early) = (OFF, 0_i64, 0_u64);
    let take = |v: &mut Vec<Pkt>, k: &mut u64, off: i64, shift: i64, early: u64, reset: bool| {
        let now = MONO + *k * PACKET - early;
        let raw = (WALL + *k * PACKET).wrapping_add(shift as u64);
        v.push((true, off, raw, PACKET, now, reset));
        *k += 1;
    };
    for _ in 0..WARM + 5 {
        take(&mut v, &mut k, off, shift, early, false);
    }
    // the sender steps first, this box follows 15 or 90 packets later (0.5 s / 3 s)
    for (step, lag) in [
        (260_000_000_i64, 15_u64),
        (682_474_000, 90),
        (-1_500_000_000, 15),
        (2_500_000_000, 90),
    ] {
        let jump = relabel_stamp_jump(step);
        shift += jump;
        early += (step - jump) as u64;
        for _ in 0..=lag {
            take(&mut v, &mut k, off, shift, early, false);
        }
        off -= step;
        for _ in 0..5 {
            take(&mut v, &mut k, off, shift, early, false);
        }
    }
    // a receiver step that misses the jump (still pending), then the rest of it (resolved)
    let jump = relabel_stamp_jump(682_474_000);
    shift += jump;
    early += (682_474_000 - jump) as u64;
    take(&mut v, &mut k, off, shift, early, false);
    off -= 100_000_000;
    take(&mut v, &mut k, off, shift, early, false);
    off -= 582_474_000;
    for _ in 0..5 {
        take(&mut v, &mut k, off, shift, early, false);
    }
    // the resolution edges: a receiver step exactly one packet off the jump stays pending (strict),
    // the rest of it resolves; then a jump of one packet + 1 ms whose offset only DRIFTS (0.5 ms per
    // packet, never a step) onto the held offset stays pending
    let p = PACKET as i64;
    shift += 666_666_666;
    take(&mut v, &mut k, off, shift, early, false);
    off -= 666_666_666 + p;
    take(&mut v, &mut k, off, shift, early, false);
    off += p;
    for _ in 0..3 {
        take(&mut v, &mut k, off, shift, early, false);
    }
    shift += p + 1_000_000;
    take(&mut v, &mut k, off, shift, early, false);
    for _ in 0..6 {
        off -= 500_000;
        take(&mut v, &mut k, off, shift, early, false);
    }
    clear(&mut v);
    for _ in 0..WARM + 5 {
        take(&mut v, &mut k, off, shift, early, false);
    }
    // review round 1: a pause, a duplicated slot and a skipped slot INSIDE a pending never move its
    // held offset (only a relabel-shaped jump does), so this box's own step still resolves it
    let jump = relabel_stamp_jump(682_474_000);
    shift += jump;
    early += (682_474_000 - jump) as u64;
    for _ in 0..4 {
        take(&mut v, &mut k, off, shift, early, false);
    }
    k += 15;
    take(&mut v, &mut k, off, shift, early, false);
    let (dup_raw, dup_now) = {
        let l = v.last().expect("a packet");
        (l.2, l.4 + 3_000_000)
    };
    v.push((true, off, dup_raw, PACKET, dup_now, false));
    take(&mut v, &mut k, off, shift, early, false);
    k += 1;
    take(&mut v, &mut k, off, shift, early, false);
    off -= 682_474_000;
    for _ in 0..3 {
        take(&mut v, &mut k, off, shift, early, false);
    }
    // a duplicated slot (the same stamp 3 ms later), a pause, a stamp leap: never pending
    let (dup_raw, dup_now) = {
        let l = v.last().expect("a packet");
        (l.2, l.4 + 3_000_000)
    };
    v.push((true, off, dup_raw, PACKET, dup_now, false));
    take(&mut v, &mut k, off, shift, early, false);
    k += 15;
    take(&mut v, &mut k, off, shift, early, false);
    k += 1;
    shift += 47_000_000;
    take(&mut v, &mut k, off, shift, early, false);
    for _ in 0..3 {
        take(&mut v, &mut k, off, shift, early, false);
    }
    clear(&mut v);
    // a pending relabel's fingerprint on a packet that reset the timeline (the ingest did not
    // continue it): never a pending start
    for _ in 0..WARM + 5 {
        take(&mut v, &mut k, off, shift, early, false);
    }
    shift += relabel_stamp_jump(2_500_000_000);
    take(&mut v, &mut k, off, shift, early, true);
    take(&mut v, &mut k, off, shift, early, false);
    clear(&mut v);
    // a pending relabel ended by a timeline reset, and one ended by leaving timecode mode
    for end_by_reset in [true, false] {
        for _ in 0..WARM + 5 {
            take(&mut v, &mut k, off, shift, early, false);
        }
        shift += relabel_stamp_jump(682_474_000);
        take(&mut v, &mut k, off, shift, early, false);
        take(&mut v, &mut k, off, shift, early, false);
        if end_by_reset {
            take(&mut v, &mut k, off, shift, early, true);
        }
        clear(&mut v);
    }
    // a pending relabel no receiver step follows: released at the bound (one packet past it)
    for _ in 0..WARM + 5 {
        take(&mut v, &mut k, off, shift, early, false);
    }
    shift += relabel_stamp_jump(-682_474_000);
    for _ in 0..AUDIO_STEP_HOLD_MAX_NS.div_ceil(PACKET) + 3 {
        take(&mut v, &mut k, off, shift, early, false);
    }
    clear(&mut v);
    // a receiver-first step whose sender never follows (held, then placed at the bound); the
    // sender's LATE follow brings the age back to the nominal: never a pending relabel
    for _ in 0..WARM + 5 {
        take(&mut v, &mut k, off, shift, early, false);
    }
    off -= 682_474_000;
    for _ in 0..AUDIO_STEP_HOLD_MAX_NS.div_ceil(PACKET) + 3 {
        take(&mut v, &mut k, off, shift, early, false);
    }
    shift += 682_474_000;
    for _ in 0..3 {
        take(&mut v, &mut k, off, shift, early, false);
    }
    v
}

#[test]
fn c_audio_relabel_pending_matches_the_rust_authority_1381() {
    let block = lift_block();
    for helper in [
        "static inline bool genlock_audio_relabel_pending(",
        "static inline bool genlock_audio_step_relabel_pending_starts(",
        "static inline int64_t genlock_audio_relabel_book_ns(",
    ] {
        assert!(
            block.contains(helper),
            "issue 1381: `{helper}` is no longer inside the contiguous audio-pairing block"
        );
    }
    let p = PACKET;
    let m = WALL_STEP_MIN_NS;
    let pendings: Vec<(i64, u64, u64, i64)> = vec![
        (233_333_333, p - 26_666_667, p, m),
        (666_666_666, p - 15_807_334, p, m),
        (-1_500_000_000, p, p, m),
        (2_500_000_000, p, p, m),
        (666_666_666, p + 15_000_000, p, m),
        (666_666_666, p + 15_000_001, p, m),
        (-(p as i64), 3_000_000, p, m),
        (p as i64, p, p, m),
        (p as i64 + 1, p, p, m),
        (-(p as i64) - 1, 0, p, m),
        (3_000_000, p, p, m),
        (500_000_000, 533_333_333, p, m),
        (80_000_000, 66_666_666, p, m),
        (m, 1_000_000, 1_000_000, m),
        (m + 1, 1_000_000, 1_000_000, m),
        (i64::MIN, 0, 1, 0),
        (i64::MIN, u64::MAX, u64::MAX, 0),
        (i64::MAX, u64::MAX, u64::MAX - 1, m),
        (5, 0, 1, i64::MIN),
    ];
    let books: Vec<(i64, bool, bool, bool)> = vec![
        (-15_807_333, true, true, true),
        (-26_666_667, true, true, true),
        (13_333_333, true, true, true),
        (-15_807_333, false, true, true),
        (-15_807_333, true, false, true),
        (-15_807_333, true, true, false),
        (i64::MIN, true, true, true),
        (i64::MAX, false, false, false),
    ];
    let b = |v: bool| u8::from(v);
    let mut body = String::new();
    for (sj, gap, pkt, min) in &pendings {
        body.push_str(&format!(
            "    printf(\"%d\\n\", genlock_audio_relabel_pending({}, {gap}ull, {pkt}ull, {}) ? 1 : 0);\n",
            i64_lit(*sj),
            i64_lit(*min)
        ));
    }
    for (mv, rl, ap, tc) in &books {
        body.push_str(&format!(
            "    printf(\"%lld\\n\", (long long)genlock_audio_relabel_book_ns({}, {}, {}, {}));\n",
            i64_lit(*mv),
            b(*rl),
            b(*ap),
            b(*tc)
        ));
    }
    let seq = pending_sequence();
    let tcs: Vec<String> = seq.iter().map(|p| b(p.0).to_string()).collect();
    let offs: Vec<String> = seq.iter().map(|p| i64_lit(p.1)).collect();
    let raws: Vec<String> = seq.iter().map(|p| format!("{}ull", p.2)).collect();
    let pkts: Vec<String> = seq.iter().map(|p| format!("{}ull", p.3)).collect();
    let nows: Vec<String> = seq.iter().map(|p| format!("{}ull", p.4)).collect();
    let resets: Vec<String> = seq.iter().map(|p| b(p.5).to_string()).collect();
    body.push_str(&format!(
        r#"    static const int tc[] = {{ {} }};
    static const int64_t off[] = {{ {} }};
    static const uint64_t raw[] = {{ {} }};
    static const uint64_t pkt[] = {{ {} }};
    static const uint64_t now[] = {{ {} }};
    static const int rst[] = {{ {} }};
    bool active = false, pending = false;
    int64_t prev_off = 0, nominal = 0, held = 0, step = 0;
    uint64_t prev_raw = 0, prev_pkt = 0, dev_since = 0, start = 0, prev_arrival = 0;
    uint32_t warm = 0;
    for (size_t i = 0; i < sizeof(tc) / sizeof(tc[0]); i++) {{
        int64_t use = 0;
        const bool starts = genlock_audio_step_relabel_pending_starts(active, prev_off, prev_raw, prev_pkt,
                                                                      prev_arrival, nominal, tc[i] != 0, off[i],
                                                                      raw[i], pkt[i], now[i], {min}ll);
        const int release = genlock_audio_step_hold(&active, &prev_off, &prev_raw, &prev_pkt, &nominal, &dev_since,
                                                    &warm, &held, &start, &step, &prev_arrival, &pending, tc[i] != 0,
                                                    off[i], raw[i], pkt[i], now[i], rst[i] != 0, {min}ll, &use);
        printf("%d %d %lld %d %d %lld %lld %llu\n", starts ? 1 : 0, release, (long long)use, active ? 1 : 0,
               pending ? 1 : 0, (long long)held, (long long)step, (unsigned long long)prev_arrival);
    }}
"#,
        tcs.join(", "),
        offs.join(", "),
        raws.join(", "),
        pkts.join(", "),
        nows.join(", "),
        resets.join(", "),
        min = WALL_STEP_MIN_NS
    ));
    let out = run_lines(&compile(&block, &body, "relabel_pending_1381"));
    let mut want: Vec<String> = pendings
        .iter()
        .map(|&(sj, gap, pkt, min)| u8::from(audio_relabel_pending(sj, gap, pkt, min)).to_string())
        .collect();
    want.extend(
        books
            .iter()
            .map(|&(mv, rl, ap, tc)| audio_relabel_book_ns(mv, rl, ap, tc).to_string()),
    );
    let mut s = AudioStepHold::default();
    let (mut starts_n, mut resolved, mut timed_out, mut reset) = (0, 0, 0, 0);
    for &(tc, off, raw, pkt, now, rst) in &seq {
        let starts =
            audio_step_relabel_pending_starts(&s, tc, off, raw, pkt, now, WALL_STEP_MIN_NS);
        // the flag is kept after a release (the log's pending=): a running pending needs `active`
        let was_pending = s.active && s.relabel_pending;
        let (use_off, release) =
            audio_step_hold(&mut s, tc, off, raw, pkt, now, rst, WALL_STEP_MIN_NS);
        starts_n += usize::from(starts && s.active && s.relabel_pending);
        if was_pending {
            match release {
                AudioStepRelease::RelabelPending => resolved += 1,
                AudioStepRelease::Timeout => timed_out += 1,
                AudioStepRelease::Reset => reset += 1,
                _ => {}
            }
        }
        want.push(format!(
            "{} {} {use_off} {} {} {} {} {}",
            u8::from(starts),
            release as u8,
            u8::from(s.active),
            u8::from(s.relabel_pending),
            s.held_off_ns,
            s.step_ns,
            s.prev_arrival_ns
        ));
    }
    assert_eq!(out.len(), want.len(), "issue 1381: line count");
    for (i, (c, r)) in out.iter().zip(&want).enumerate() {
        assert_eq!(
            c, r,
            "issue 1381: the C pending relabel diverged from the Rust authority at line {i}"
        );
    }
    // the script must reach every pending path, or the gate proves less than it says
    assert!(
        starts_n >= 9 && resolved >= 6 && timed_out >= 1 && reset >= 2,
        "issue 1381: the script reaches {starts_n} pending starts, {resolved} resolved, {timed_out} \
         timed out, {reset} reset"
    );
}
