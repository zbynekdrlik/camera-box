//! Issue 1381 (design 5882391108, piece 2) — an EXECUTABLE C-vs-Rust parity gate for the per-source
//! SKEW HOLD of the genlock timecode audio across a wall step.
//!
//! `src/genlock_audio_pairing.rs` (`audio_step_hold` and its helpers) is the Tier-0 authority; the
//! `genlock_audio_step_*` / `genlock_audio_stamp_age_ns` helpers inside the contiguous audio-pairing
//! block of `vendor/obs-studio/libobs/obs-source.c` are its port. This gate lifts that block VERBATIM
//! (the shared `tests/genlock_audio_pairing_lift/mod.rs`), compiles it under `-Wall -Wextra
//! -Wconversion -Wformat=2 -Werror`, drives the C over one scripted packet sequence from arrays and
//! requires every packet's offset, release and state to equal the Rust authority driven the same way.
//! The sequence covers every path: a step the stamps follow by a jump, a follow in pieces, a catch-up
//! with continuous stamps (a 4x burst), a sender that stepped FIRST (its stamps jumped, or it caught
//! up: a zero-length hold released on the receiver's own packet), the nominal age's in-band track
//! (both signs), its out-of-band timer and the exact re-anchor edge, the 10 s bound (and, with 20 ms
//! packets, its exact edge), a timeline reset,
//! leaving timecode mode in a hold, stamp jitter inside a hold, a step within one packet, the joint
//! step, the 2 ms boundary (with 1 ms packets, where a 2 ms jump is more than a packet) and the
//! one-packet boundary, and the two's-complement extremes. The PENDING relabel of design 5901213031
//! (the sender's box stepped first) and the relabel remainder booking have their own gate,
//! `tests/genlock_audio_relabel_pending_parity_1381.rs`. It FAILS LOUDLY when no C compiler is
//! present.

use camera_box::genlock_audio_pairing::{
    audio_relabel, audio_stamp_age_ns, audio_step_freezes_video, audio_step_hold,
    audio_step_relabel_jumps, audio_step_release_places, audio_step_residual_ns, AudioStepHold,
    AudioStepRelease, AUDIO_STEP_HOLD_MAX_NS, AUDIO_STEP_NOMINAL_REANCHOR_NS,
    AUDIO_STEP_NOMINAL_WARM_PACKETS,
};
use camera_box::genlock_wall_step::WALL_STEP_MIN_NS;

mod genlock_audio_pairing_lift;
use genlock_audio_pairing_lift::{compile, i64_lit, lift_block, run_c, run_lines};

const PACKET: u64 = 33_333_333;
const WALL: u64 = 1_790_000_000_123_456_789;
const MONO: u64 = 86_400_000_000_000;
const OFF: i64 = (MONO as i64).wrapping_sub(WALL as i64);

/// One packet: (timecode, live offset, raw stamp, packet duration, now, timeline reset).
type Pkt = (bool, i64, u64, u64, u64, bool);

/// The scripted sequence. `k` counts packets from 0; the receiver's clock and the sender's stamps
/// advance one packet per packet unless a scenario moves them.
fn sequence() -> Vec<Pkt> {
    let mut v: Vec<Pkt> = Vec::new();
    let mut k: u64 = 0;
    // a 682 ms forward step the stamps follow by a jump 120 packets later
    for _ in 0..5 {
        push(&mut v, &mut k, OFF, 0, 0, false);
    }
    for _ in 0..120 {
        push(&mut v, &mut k, OFF - 682_474_000, 0, 0, false);
    }
    for _ in 0..5 {
        push(&mut v, &mut k, OFF - 682_474_000, 682_474_000, 0, false);
    }
    // a backward 400 ms step followed in two pieces, then within one packet
    for _ in 0..3 {
        push(
            &mut v,
            &mut k,
            OFF - 682_474_000 + 400_000_000,
            682_474_000,
            0,
            false,
        );
    }
    for f in [-200_000_000_i64, -390_000_000, -400_000_000] {
        push(
            &mut v,
            &mut k,
            OFF - 682_474_000 + 400_000_000,
            682_474_000 + f,
            0,
            false,
        );
    }
    clear(&mut v);
    // a catch-up: the stamps stay continuous, 40 packets later a 4x burst catches the step up
    let mut t = MONO + k * PACKET;
    for i in 0..83_u64 {
        let off = if i < 3 { OFF } else { OFF - 682_474_000 };
        v.push((true, off, WALL + k * PACKET, PACKET, t, false));
        k += 1;
        t += if i < 43 { PACKET } else { PACKET / 4 };
    }
    clear(&mut v);
    // review round 1: the sender stepped FIRST -- its stamps jumped, the receiver steps 2 s later,
    // past the nominal's warm-up (design 5901213031: a pending relabel from the stamp jump on,
    // resolved by the receiver's own packet)
    for _ in 0..WARM {
        push(&mut v, &mut k, OFF, 0, 0, false);
    }
    for _ in 0..60 {
        push(&mut v, &mut k, OFF, 682_474_000, 0, false);
    }
    for _ in 0..4 {
        push(&mut v, &mut k, OFF - 682_474_000, 682_474_000, 0, false);
    }
    clear(&mut v);
    // ... or it caught up first (a 4x burst), then the receiver's step
    let mut t = MONO + k * PACKET;
    for i in 0..(WARM + 42) {
        let off = if i < WARM + 38 {
            OFF
        } else {
            OFF - 682_474_000
        };
        v.push((true, off, WALL + k * PACKET, PACKET, t, false));
        k += 1;
        t += if (WARM..WARM + 28).contains(&i) {
            PACKET / 4
        } else {
            PACKET
        };
    }
    clear(&mut v);
    // the nominal age: jittered arrivals move it in band (both signs, a remainder the division
    // truncates toward zero); stamps 100 ms ahead (after a 100 s pause: with continuous arrival the
    // jump would be a pending relabel, design 5901213031) leave the band and start the timer; packets
    // 100 s apart reach the re-anchor at exactly 600 s; the receiver's later step then HOLDS (the
    // stamps are off the re-anchored nominal)
    let mut t = MONO + k * PACKET;
    let jitter: [i64; 5] = [0, 3_000_001, -5_000_003, 1_234_567, -2_000_000];
    for i in 0..(WARM as usize + 20) {
        let now = (t as i64 + jitter[i % 5]) as u64;
        v.push((true, OFF, WALL + k * PACKET, PACKET, now, false));
        k += 1;
        t += PACKET;
    }
    let far = 100_000_000_000_u64;
    for n in 1..9_u64 {
        let raw = (WALL + k * PACKET + n * far).wrapping_add(100_000_000);
        v.push((true, OFF, raw, PACKET, t + n * far, false));
    }
    let t_end = t + 8 * far;
    let raw_end = WALL + k * PACKET + 8 * far + 100_000_000;
    for j in 1..4_u64 {
        v.push((
            true,
            OFF - 100_000_000,
            raw_end + j * PACKET,
            PACKET,
            t_end + j * PACKET,
            false,
        ));
    }
    k = (t_end - MONO) / PACKET + 4;
    clear(&mut v);
    // the 10 s bound, up to one packet past its exact edge
    for _ in 0..3 {
        push(&mut v, &mut k, OFF, 0, 0, false);
    }
    let bound = AUDIO_STEP_HOLD_MAX_NS.div_ceil(PACKET) + 2;
    for _ in 0..bound {
        push(&mut v, &mut k, OFF - 682_474_000, 0, 0, false);
    }
    clear(&mut v);
    // a timeline reset inside a hold, and a reset packet that never starts one
    for _ in 0..3 {
        push(&mut v, &mut k, OFF, 0, 0, false);
    }
    push(&mut v, &mut k, OFF + 89_703_000, 0, 0, false);
    push(&mut v, &mut k, OFF + 89_703_000, 0, 0, true);
    push(&mut v, &mut k, OFF - 300_000_000, 0, 0, true);
    // leaving timecode mode inside a hold
    push(&mut v, &mut k, OFF, 0, 0, false);
    push(&mut v, &mut k, OFF - 682_474_000, 0, 0, false);
    clear(&mut v);
    clear(&mut v);
    // no hold: jitter at and just under the 2 ms threshold, a step within one packet, the joint step,
    // the exact one-packet residual
    for off in [OFF, OFF + WALL_STEP_MIN_NS, OFF, OFF + 1_999_999, OFF] {
        push(&mut v, &mut k, off, 0, 0, false);
    }
    let p = PACKET as i64;
    push(&mut v, &mut k, OFF - p, 0, 0, false);
    push(&mut v, &mut k, OFF - p - 51_000_000, 51_000_000, 0, false);
    push(&mut v, &mut k, OFF - p - 85_000_000, 51_000_000, 0, false);
    clear(&mut v);
    // stamp jitter under the 2 ms threshold inside a hold never moves the held offset
    for _ in 0..3 {
        push(&mut v, &mut k, OFF, 0, 0, false);
    }
    for i in 0..6 {
        let jitter = if i % 2 == 0 { 900_000 } else { -900_000 };
        push(&mut v, &mut k, OFF - 682_474_000, jitter, 0, false);
    }
    push(&mut v, &mut k, OFF - 682_474_000, 682_474_000, 0, false);
    clear(&mut v);
    // 20 ms packets: one lands on start + 10 s to the nanosecond (the bound's exact edge)
    for j in 0..3 {
        push_at(&mut v, j, EDGE_PACKET, OFF, 0);
    }
    for j in 3..3 + AUDIO_STEP_HOLD_MAX_NS / EDGE_PACKET + 2 {
        push_at(&mut v, j, EDGE_PACKET, OFF - 682_474_000, 0);
    }
    clear(&mut v);
    // 1 ms packets: a jump of exactly 2 ms starts no hold, 1 ns more does (then the stamps follow)
    for j in 0..3 {
        push_at(&mut v, j, 1_000_000, OFF, 0);
    }
    push_at(&mut v, 3, 1_000_000, OFF + WALL_STEP_MIN_NS, 0);
    push_at(&mut v, 4, 1_000_000, OFF + WALL_STEP_MIN_NS, 0);
    let d = WALL_STEP_MIN_NS + 1;
    push_at(&mut v, 5, 1_000_000, OFF + WALL_STEP_MIN_NS + d, 0);
    push_at(&mut v, 6, 1_000_000, OFF + WALL_STEP_MIN_NS + d, -d);
    clear(&mut v);
    // arithmetic edges: an out-of-band packet at now = 0 still starts the timer (a 0 would read as
    // in band) -- the warm-up spent at now = 0 too -- then the two's-complement extremes
    for j in 0..=WARM {
        v.push((true, OFF, WALL + j * PACKET, PACKET, 0, false));
    }
    v.push((true, OFF, WALL + PACKET + 10_000_000_000, PACKET, 0, false));
    v.push((
        true,
        OFF,
        WALL + 2 * PACKET + 10_000_000_000,
        PACKET,
        PACKET,
        false,
    ));
    clear(&mut v);
    v.push((true, i64::MAX, u64::MAX, 2, 5, false));
    v.push((true, i64::MIN, 1, 2, 6, false));
    v.push((true, 0, 0, u64::MAX, u64::MAX, false));
    v.push((true, i64::MIN, u64::MAX, 1, 0, false));
    v
}

/// Packets that warm the nominal age up after a seed (plus the seed itself).
const WARM: u64 = AUDIO_STEP_NOMINAL_WARM_PACKETS as u64 + 1;

/// One timecode packet of the sequence (see [`sequence`]).
fn push(v: &mut Vec<Pkt>, k: &mut u64, off: i64, follow: i64, early: u64, reset: bool) {
    let now = MONO + *k * PACKET - early;
    let raw = (WALL + *k * PACKET).wrapping_add(follow as u64);
    v.push((true, off, raw, PACKET, now, reset));
    *k += 1;
}

/// Packets of the exact-edge scenarios: a separate clock 1000 s later, packet `j` of `pkt` ns.
const EDGE_BASE: u64 = 1_000_000_000_000;
const EDGE_PACKET: u64 = 20_000_000;
const _: () = assert!(AUDIO_STEP_HOLD_MAX_NS.is_multiple_of(EDGE_PACKET));

fn push_at(v: &mut Vec<Pkt>, j: u64, pkt: u64, off: i64, follow: i64) {
    let now = MONO + EDGE_BASE + j * pkt;
    let raw = (WALL + EDGE_BASE + j * pkt).wrapping_add(follow as u64);
    v.push((true, off, raw, pkt, now, false));
}

/// A packet outside timecode mode: clears the state.
fn clear(v: &mut Vec<Pkt>) {
    v.push((false, 7, WALL, PACKET, MONO, false));
}

fn state_line(off: i64, rel: u8, s: &AudioStepHold) -> String {
    format!(
        "{off} {rel} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
        u8::from(s.active),
        s.prev_off_ns,
        s.prev_raw_ns,
        s.prev_packet_ns,
        s.nominal_age_ns,
        s.nominal_dev_since_ns,
        s.nominal_warm,
        s.held_off_ns,
        s.start_ns,
        s.step_ns,
        s.prev_arrival_ns,
        u8::from(s.relabel_pending),
        s.unmatched_ns,
        s.unmatched_at_ns
    )
}

#[test]
fn c_audio_step_hold_matches_the_rust_authority_1381() {
    let seq = sequence();
    let b = |v: bool| u8::from(v);
    let tcs: Vec<String> = seq.iter().map(|p| b(p.0).to_string()).collect();
    let offs: Vec<String> = seq.iter().map(|p| i64_lit(p.1)).collect();
    let raws: Vec<String> = seq.iter().map(|p| format!("{}ull", p.2)).collect();
    let pkts: Vec<String> = seq.iter().map(|p| format!("{}ull", p.3)).collect();
    let nows: Vec<String> = seq.iter().map(|p| format!("{}ull", p.4)).collect();
    let resets: Vec<String> = seq.iter().map(|p| b(p.5).to_string()).collect();
    let body = format!(
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
    int64_t unmatched = 0;
    uint64_t unmatched_at = 0;
    for (size_t i = 0; i < sizeof(tc) / sizeof(tc[0]); i++) {{
        int64_t use = 0;
        const int rel = genlock_audio_step_hold(&active, &prev_off, &prev_raw, &prev_pkt, &nominal, &dev_since,
                                                &warm, &held, &start, &step, &prev_arrival, &pending, &unmatched,
                                                &unmatched_at, tc[i] != 0, off[i], raw[i], pkt[i], now[i],
                                                rst[i] != 0, {min}ll, &use);
        printf("%lld %d %d %lld %llu %llu %lld %llu %u %lld %llu %lld %llu %d %lld %llu\n", (long long)use, rel,
               active ? 1 : 0, (long long)prev_off, (unsigned long long)prev_raw, (unsigned long long)prev_pkt,
               (long long)nominal, (unsigned long long)dev_since, warm, (long long)held,
               (unsigned long long)start, (long long)step, (unsigned long long)prev_arrival, pending ? 1 : 0,
               (long long)unmatched, (unsigned long long)unmatched_at);
    }}
"#,
        tcs.join(", "),
        offs.join(", "),
        raws.join(", "),
        pkts.join(", "),
        nows.join(", "),
        resets.join(", "),
        min = WALL_STEP_MIN_NS
    );
    let out = run_c(&body, "step_hold_1381");
    let mut s = AudioStepHold::default();
    let mut want: Vec<String> = Vec::new();
    for &(tc, off, raw, pkt, now, reset) in &seq {
        let (use_off, rel) =
            audio_step_hold(&mut s, tc, off, raw, pkt, now, reset, WALL_STEP_MIN_NS);
        want.push(state_line(use_off, rel as u8, &s));
    }
    assert_eq!(out.len(), want.len(), "issue 1381: line count");
    for (i, (c, r)) in out.iter().zip(&want).enumerate() {
        assert_eq!(
            c, r,
            "issue 1381: genlock_audio_step_hold diverged from the Rust authority at packet {i}"
        );
    }
    // the sequence must reach every release and a real hold, or the gate proves less than it says
    let field = |l: &String, i: usize| l.split(' ').nth(i).map(str::to_string);
    for rel in [
        AudioStepRelease::Followed,
        AudioStepRelease::Timeout,
        AudioStepRelease::Reset,
        AudioStepRelease::RelabelPending,
    ] {
        assert!(
            want.iter()
                .any(|l| field(l, 1) == Some((rel as u8).to_string())),
            "issue 1381: the sequence never reaches {rel:?}"
        );
    }
    assert!(
        want.iter()
            .filter(|l| field(l, 2).as_deref() == Some("1"))
            .count()
            > 300,
        "issue 1381: the sequence must hold for the whole 10 s bound"
    );
    // review round 1: a zero-length hold (released Followed on a packet that never held), the
    // re-anchor (a nominal that jumps while no hold runs) and the out-of-band timer at now = 0
    let mut zero_length = false;
    let mut reanchor = false;
    for w in want.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if field(a, 2).as_deref() == Some("0")
            && field(b, 1) == Some((AudioStepRelease::Followed as u8).to_string())
        {
            zero_length = true;
        }
        let (na, nb) = (field(a, 6), field(b, 6));
        let parse = |x: Option<String>| x.and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        if field(b, 2).as_deref() == Some("0")
            && (parse(nb) - parse(na)).unsigned_abs() >= 100_000_000
            && field(a, 7).as_deref() != Some("0")
            && field(b, 7).as_deref() == Some("0")
        {
            reanchor = true;
        }
    }
    assert!(
        zero_length && reanchor && want.iter().any(|l| field(l, 7).as_deref() == Some("1")),
        "issue 1381: the sequence must reach a zero-length hold, a re-anchor and the now = 0 timer"
    );
    // ROZHODNUTÉ 5903945145: a hold that ended without the stamps jumping remembers its step
    assert!(
        want.iter().any(|l| field(l, 14).as_deref() != Some("0")),
        "issue 1381: the sequence must reach a remembered unmatched step"
    );
    assert_eq!(AUDIO_STEP_NOMINAL_REANCHOR_NS, 6 * 100_000_000_000);
}

#[test]
fn c_audio_step_freezes_video_matches_the_rust_authority_1381() {
    let m = AUDIO_STEP_HOLD_MAX_NS;
    let vectors: [(bool, u64, u64); 6] = [
        (true, 100, 100),
        (true, 100, 100 + m - 1),
        (true, 100, 100 + m),
        (false, 0, 5),
        (true, u64::MAX, 3),
        (true, 5, 3),
    ];
    let mut body = String::new();
    for (a, st, now) in &vectors {
        body.push_str(&format!(
            "    printf(\"%d\\n\", genlock_audio_step_freezes_video({}, {st}ull, {now}ull) ? 1 : 0);\n",
            u8::from(*a)
        ));
    }
    let out = run_c(&body, "step_freeze_1381");
    let want: Vec<String> = vectors
        .iter()
        .map(|&(a, st, now)| u8::from(audio_step_freezes_video(a, st, now)).to_string())
        .collect();
    assert_eq!(
        out, want,
        "issue 1381: genlock_audio_step_freezes_video diverged from the Rust authority"
    );
    assert_eq!(want, ["1", "1", "0", "0", "1", "0"]);
}

#[test]
fn c_audio_step_scalars_match_the_rust_authority_1381() {
    let ages: [(u64, u64, i64); 5] = [
        (MONO, WALL, OFF),
        (MONO, WALL - 682_474_000, OFF - 682_474_000),
        (0, 1, 0),
        (u64::MAX, 0, i64::MIN),
        (5, u64::MAX, i64::MAX),
    ];
    let residuals: [(i64, i64); 4] = [
        (OFF, OFF - 682_474_000),
        (OFF, OFF + 1),
        (i64::MIN, i64::MAX),
        (i64::MAX, i64::MIN),
    ];
    let places: [(u8, i64, u64); 20] = [
        (0, -682_474_000, PACKET),
        (1, -682_474_000, PACKET),
        (1, PACKET as i64, PACKET),
        (1, PACKET as i64 + 1, PACKET),
        (1, -(PACKET as i64) - 1, PACKET),
        (2, 89_703_000, PACKET),
        (2, 1, PACKET),
        // ROZHODNUTÉ 5903945145 point 2: a timeout applies any move once, one packet − 66 ns too
        (2, PACKET as i64 - 66, PACKET),
        (2, -(PACKET as i64 - 66), PACKET),
        (2, 0, PACKET),
        // review round 3: one slot (one packet − 100 ns) is the edge; a folded pending's sub-slot
        // residual is booked
        (2, PACKET as i64 - 100, PACKET),
        (2, PACKET as i64 - 101, PACKET),
        (2, -(PACKET as i64 - 100), PACKET),
        (2, -(PACKET as i64 - 101), PACKET),
        (2, -966, PACKET),
        (3, -682_474_000, PACKET),
        (1, i64::MIN, u64::MAX),
        (2, i64::MIN, u64::MAX - 1),
        (4, -15_807_333, PACKET),
        (4, PACKET as i64 + 1, PACKET),
    ];
    let rel_of = |c: u8| match c {
        1 => AudioStepRelease::Followed,
        2 => AudioStepRelease::Timeout,
        3 => AudioStepRelease::Reset,
        4 => AudioStepRelease::RelabelPending,
        _ => AudioStepRelease::None,
    };
    let mut body = String::new();
    for (n, r, o) in &ages {
        body.push_str(&format!(
            "    printf(\"%lld\\n\", (long long)genlock_audio_stamp_age_ns({n}ull, {r}ull, {}));\n",
            i64_lit(*o)
        ));
    }
    for (h, l) in &residuals {
        body.push_str(&format!(
            "    printf(\"%lld\\n\", (long long)genlock_audio_step_residual_ns({}, {}));\n",
            i64_lit(*h),
            i64_lit(*l)
        ));
    }
    for (rel, res, pkt) in &places {
        body.push_str(&format!(
            "    printf(\"%d\\n\", genlock_audio_step_release_places({rel}, {}, {pkt}ull) ? 1 : 0);\n",
            i64_lit(*res)
        ));
    }
    for rel in 0..6_u8 {
        body.push_str(&format!(
            "    printf(\"%s\\n\", genlock_audio_step_release_token({rel}));\n"
        ));
    }
    let out = run_c(&body, "step_scalars_1381");
    let mut want: Vec<String> = Vec::new();
    for (n, r, o) in &ages {
        want.push(audio_stamp_age_ns(*n, *r, *o).to_string());
    }
    for (h, l) in &residuals {
        want.push(audio_step_residual_ns(*h, *l).to_string());
    }
    for (rel, res, pkt) in &places {
        want.push(u8::from(audio_step_release_places(rel_of(*rel), *res, *pkt)).to_string());
    }
    for rel in 0..6_u8 {
        want.push(rel_of(rel).token().to_string());
    }
    assert_eq!(
        out, want,
        "issue 1381: a C skew-hold scalar helper diverged from the Rust authority"
    );
}

// Issue 1381 (design 5900385541) — the RELABEL decision (`audio_relabel` / `genlock_audio_relabel`)
// and its two jumps read from the skew-hold state (`audio_step_relabel_jumps` /
// `genlock_audio_step_relabel_jumps`).

/// A relabelling sender's stamp jump at a wall step: N = floor(S / slot) slots, toward −∞.
fn relabel_stamp_jump(step_ns: i64) -> i64 {
    let n = (i128::from(step_ns) * 30).div_euclid(1_000_000_000) as i64;
    n * 1_000_000_000 / 30
}

/// Relabels in both shapes on top of the hold script: at each step the stamps jump N slots on the
/// receiver's step packet (joint) or one packet after it (split: the hold starts on the step packet).
fn relabel_sequence() -> Vec<Pkt> {
    let mut v = sequence();
    clear(&mut v);
    let mut k: u64 = 1_000_000;
    let mut off = OFF;
    let mut shift = 0_i64;
    let take = |v: &mut Vec<Pkt>, k: &mut u64, off: i64, shift: i64| {
        let now = MONO + *k * PACKET;
        let raw = (WALL + *k * PACKET).wrapping_add(shift as u64);
        v.push((true, off, raw, PACKET, now, false));
        *k += 1;
    };
    for _ in 0..WARM + 5 {
        take(&mut v, &mut k, off, shift);
    }
    for split in [false, true] {
        for step in [
            260_000_000_i64,
            682_474_000,
            -1_500_000_000,
            2_500_000_000,
            -20_000_000,
            89_703_000,
            -682_474_000,
        ] {
            off -= step;
            if split {
                take(&mut v, &mut k, off, shift);
            }
            shift += relabel_stamp_jump(step);
            for _ in 0..5 {
                take(&mut v, &mut k, off, shift);
            }
        }
    }
    v
}

#[test]
fn c_audio_relabel_matches_the_rust_authority_1381() {
    let block = lift_block();
    for helper in [
        "static inline bool genlock_audio_relabel(",
        "static inline bool genlock_audio_step_relabel_jumps(",
    ] {
        assert!(
            block.contains(helper),
            "issue 1381: `{helper}` is no longer inside the contiguous audio-pairing block"
        );
    }
    let p = PACKET as i64;
    let m = WALL_STEP_MIN_NS;
    let scalars: Vec<(i64, i64, u64, i64)> = vec![
        (233_333_333, -260_000_000, PACKET, m),
        (666_666_666, -682_474_000, PACKET, m),
        (-1_500_000_000, 1_500_000_000, PACKET, m),
        (2_500_000_000, -2_500_000_000, PACKET, m),
        (-33_333_333, 20_000_000, PACKET, m),
        (0, -682_474_000, PACKET, m),
        (80_000_000, 0, PACKET, m),
        (m, -m, PACKET, m),
        (m + 1, -m - 1, PACKET, m),
        (m + 1, -m, PACKET, m),
        (66_666_667, -66_666_667 - p, PACKET, m),
        (66_666_667, -66_666_667 - p + 1, PACKET, m),
        (66_666_667, -66_666_667 + p, PACKET, m),
        (i64::MIN, i64::MIN, u64::MAX, m),
        (i64::MIN, i64::MAX, 2, 0),
        (i64::MAX, 1, u64::MAX, i64::MIN),
        (5, -5, 1, -3),
    ];
    let seq = relabel_sequence();
    let b = |v: bool| u8::from(v);
    let mut body = String::new();
    for (sj, oj, pkt, min) in &scalars {
        body.push_str(&format!(
            "    printf(\"%d\\n\", genlock_audio_relabel({}, {}, {pkt}ull, {}) ? 1 : 0);\n",
            i64_lit(*sj),
            i64_lit(*oj),
            i64_lit(*min)
        ));
    }
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
    int64_t unmatched = 0;
    uint64_t unmatched_at = 0;
    for (size_t i = 0; i < sizeof(tc) / sizeof(tc[0]); i++) {{
        int64_t sj = 0, oj = 0, use = 0;
        const bool have = genlock_audio_step_relabel_jumps(active, prev_off, prev_raw, prev_pkt, held, tc[i] != 0,
                                                           off[i], raw[i], &sj, &oj);
        const bool rel = have && genlock_audio_relabel(sj, oj, pkt[i], {min}ll);
        const int release = genlock_audio_step_hold(&active, &prev_off, &prev_raw, &prev_pkt, &nominal, &dev_since,
                                                    &warm, &held, &start, &step, &prev_arrival, &pending,
                                                    &unmatched, &unmatched_at, tc[i] != 0, off[i], raw[i], pkt[i],
                                                    now[i], rst[i] != 0, {min}ll, &use);
        printf("%d %lld %lld %d %d %lld\n", have ? 1 : 0, (long long)sj, (long long)oj, rel ? 1 : 0, release,
               (long long)use);
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
    let out = run_lines(&compile(&block, &body, "relabel_1381"));
    let mut want: Vec<String> = scalars
        .iter()
        .map(|&(sj, oj, pkt, min)| u8::from(audio_relabel(sj, oj, pkt, min)).to_string())
        .collect();
    let mut s = AudioStepHold::default();
    let mut relabels = 0;
    let mut released_by_relabel = 0;
    for &(tc, off, raw, pkt, now, reset) in &seq {
        let jumps = audio_step_relabel_jumps(&s, tc, off, raw);
        let (sj, oj) = jumps.unwrap_or((0, 0));
        let rel = jumps.is_some() && audio_relabel(sj, oj, pkt, WALL_STEP_MIN_NS);
        let was_active = s.active;
        let (use_off, release) =
            audio_step_hold(&mut s, tc, off, raw, pkt, now, reset, WALL_STEP_MIN_NS);
        if rel {
            relabels += 1;
            // a relabel never starts a hold, and one that ends a running hold ends it FOLLOWED
            // without a placement
            assert!(!s.active, "issue 1381: a relabel packet started a hold");
            if was_active {
                assert_eq!(release, AudioStepRelease::Followed);
                assert!(!audio_step_release_places(
                    release,
                    audio_step_residual_ns(s.held_off_ns, off),
                    pkt
                ));
                released_by_relabel += 1;
            }
        }
        want.push(format!(
            "{} {sj} {oj} {} {} {use_off}",
            u8::from(jumps.is_some()),
            u8::from(rel),
            release as u8
        ));
    }
    assert_eq!(out.len(), want.len(), "issue 1381: line count");
    for (i, (c, r)) in out.iter().zip(&want).enumerate() {
        assert_eq!(
            c, r,
            "issue 1381: the C relabel diverged from the Rust authority at line {i}"
        );
    }
    // the script must reach relabels in both shapes, or the gate proves less than it says
    assert!(
        relabels >= 12 && released_by_relabel >= 5,
        "issue 1381: the script reaches {relabels} relabels, {released_by_relabel} ending a hold"
    );
}
