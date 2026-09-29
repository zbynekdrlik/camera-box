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
//! with continuous stamps, the 10 s bound (and, with 20 ms packets, its exact edge), a timeline reset,
//! leaving timecode mode in a hold, stamp jitter inside a hold, a step within one packet, the joint
//! step, the 2 ms boundary (with 1 ms packets, where a 2 ms jump is more than a packet) and the
//! one-packet boundary, and the two's-complement extremes. It FAILS LOUDLY when no C compiler is
//! present.

use camera_box::genlock_audio_pairing::{
    audio_stamp_age_ns, audio_step_hold, audio_step_release_places, audio_step_residual_ns,
    AudioStepHold, AudioStepRelease, AUDIO_STEP_HOLD_MAX_NS,
};
use camera_box::genlock_wall_step::WALL_STEP_MIN_NS;

mod genlock_audio_pairing_lift;
use genlock_audio_pairing_lift::{i64_lit, run_c};

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
    // a catch-up: the stamps stay continuous, 40 packets later they arrive the step early
    for _ in 0..3 {
        push(&mut v, &mut k, OFF, 0, 0, false);
    }
    for i in 0..60_u64 {
        let early = if i >= 40 { 682_474_000 } else { 0 };
        push(&mut v, &mut k, OFF - 682_474_000, 0, early, false);
    }
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
    // two's-complement extremes
    v.push((true, i64::MAX, u64::MAX, 2, 5, false));
    v.push((true, i64::MIN, 1, 2, 6, false));
    v.push((true, 0, 0, u64::MAX, u64::MAX, false));
    v.push((true, i64::MIN, u64::MAX, 1, 0, false));
    v
}

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
        "{off} {rel} {} {} {} {} {} {} {} {} {}",
        u8::from(s.active),
        s.prev_off_ns,
        s.prev_raw_ns,
        s.prev_packet_ns,
        s.prev_age_ns,
        s.base_age_ns,
        s.held_off_ns,
        s.start_ns,
        s.step_ns
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
    bool active = false;
    int64_t prev_off = 0, prev_age = 0, base_age = 0, held = 0, step = 0;
    uint64_t prev_raw = 0, prev_pkt = 0, start = 0;
    for (size_t i = 0; i < sizeof(tc) / sizeof(tc[0]); i++) {{
        int64_t use = 0;
        const int rel = genlock_audio_step_hold(&active, &prev_off, &prev_raw, &prev_pkt, &prev_age, &base_age,
                                                &held, &start, &step, tc[i] != 0, off[i], raw[i], pkt[i],
                                                now[i], rst[i] != 0, {min}ll, &use);
        printf("%lld %d %d %lld %llu %llu %lld %lld %lld %llu %lld\n", (long long)use, rel, active ? 1 : 0,
               (long long)prev_off, (unsigned long long)prev_raw, (unsigned long long)prev_pkt,
               (long long)prev_age, (long long)base_age, (long long)held, (unsigned long long)start,
               (long long)step);
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
    let places: [(u8, i64, u64); 10] = [
        (0, -682_474_000, PACKET),
        (1, -682_474_000, PACKET),
        (1, PACKET as i64, PACKET),
        (1, PACKET as i64 + 1, PACKET),
        (1, -(PACKET as i64) - 1, PACKET),
        (2, 89_703_000, PACKET),
        (2, 1, PACKET),
        (3, -682_474_000, PACKET),
        (1, i64::MIN, u64::MAX),
        (2, i64::MIN, u64::MAX - 1),
    ];
    let rel_of = |c: u8| match c {
        1 => AudioStepRelease::Followed,
        2 => AudioStepRelease::Timeout,
        3 => AudioStepRelease::Reset,
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
    for rel in 0..5_u8 {
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
    for rel in 0..5_u8 {
        want.push(rel_of(rel).token().to_string());
    }
    assert_eq!(
        out, want,
        "issue 1381: a C skew-hold scalar helper diverged from the Rust authority"
    );
}
