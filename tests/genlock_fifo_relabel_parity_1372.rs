//! Issue 1372 part B — an EXECUTABLE C-vs-Rust parity gate for the receive-FIFO relabel.
//!
//! `vendor/obs-studio/libobs/obs-genlock-fifo-relabel.h` is pure (stdint/stdbool/stddef + the
//! wall-step header), so it is `#include`d as-is (never a retyped copy), compiled under
//! `-Wall -Wextra -Wconversion -Wformat=2 -Werror`, and driven through the same vectors as the
//! Tier-0 Rust authority `camera_box::genlock_fifo_relabel`: the continuity / step-match / jump
//! predicates over a grid of deltas, steps, source steps and frames (the i64 extremes included),
//! the recency of a remembered jump, the one-latency window, the booking over read sequences, the
//! plan over queues (old, a boundary at every position, a duplicate / a gap at the boundary, the
//! negative-step overlap, an empty queue, nothing presented), the arrival window over sequences,
//! and whole apply + receive scenarios through the queue callbacks. Every printed value must be
//! byte-identical. `cc` is required — per the project's test-strictness rule this FAILS LOUDLY
//! rather than skipping when the toolchain is missing.

use camera_box::genlock_fifo_relabel::{
    arrival_add, continuous, delta_carries_step, dev_ns, jump_recorded, plan,
    sender_stepped_before, step_relabels, window_ns, Arrival, Booking, RelabelState,
};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const HEADER: &str = "vendor/obs-studio/libobs/obs-genlock-fifo-relabel.h";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

const I30: u64 = 33_333_333;
const I60: u64 = 16_666_667;
const S: i64 = 1_600_000_000;
const S_RAW: i64 = 1_543_160_000;
const W0: u64 = 1_791_338_400_000_000_000;
const MONO0: u64 = 123_456_789_000_000;
/// Every scenario books at MONO0 + I30 + 1000 (`reads_for`): the previous release a tick before,
/// the release that applies just after.
const RELEASE: u64 = MONO0 + 5_000;
const APPLY_MONO: u64 = MONO0 + I30 + 2_000;

fn b(v: bool) -> u8 {
    u8::from(v)
}

/// An i64 as a C expression (`INT64_MIN` has no literal form).
fn c_i64(v: i64) -> String {
    if v == i64::MIN {
        "INT64_MIN".to_string()
    } else {
        format!("{v}LL")
    }
}

fn deltas() -> Vec<i64> {
    let i = I30 as i64;
    vec![
        0,
        i,
        2 * i,
        3 * i,
        -i,
        I60 as i64,
        S + i,
        S,
        S + 2 * i,
        S + 3 * i,
        S - i,
        -S + i,
        -S,
        S_RAW + i,
        700_000_000,
        -5_000_000_000,
        50_000_000,
        49_999_999,
        50_000_001,
        66_666_666,
        i64::MIN,
        i64::MAX,
    ]
}

fn steps() -> Vec<i64> {
    vec![
        S,
        -S,
        S_RAW,
        200_000_000,
        -200_000_000,
        100_000_000,
        99_999_999,
        -100_000_000,
        -51_039_000,
        0,
        i64::MIN,
    ]
}

const SRCS: [u64; 4] = [0, I30, I60, 16_666_666];
/// `(reserve, presented age)` window vectors around the 2 s age cap.
const WINDOW_VECTORS: [(u64, u64); 7] = [
    (3_000_000, 70_000_000),
    (1_026_000_000, 5),
    (0, 0),
    (3_000_000, 1_999_999_999),
    (3_000_000, 2_000_000_001),
    (3_000_000, 10_800_000_000_000),
    (2_500_000_000, 10_800_000_000_000),
];
const FRAMES: [u64; 3] = [I30, 0, I60];

/// `(prev, stamps, step, src, frame, stepped_before)` plan vectors.
#[allow(clippy::type_complexity)]
fn plan_vectors() -> Vec<(u64, Vec<u64>, i64, u64, u64, bool)> {
    let mut out = Vec::new();
    for step in [S, -S, S_RAW, 200_000_000, -200_000_000] {
        let base = W0 - 40 * I30;
        // all old, prev present / absent, stepped before or not
        let old: Vec<u64> = (1..=6u64).map(|k| base + k * I30).collect();
        for stepped in [false, true] {
            out.push((base, old.clone(), step, I30, I30, stepped));
            out.push((0, old.clone(), step, I30, I30, stepped));
        }
        // a boundary at every position (0 = between prev and the head)
        for at in 0..=6usize {
            let q: Vec<u64> = (1..=6u64)
                .map(|k| {
                    let ts = base + k * I30;
                    if (k as usize) > at {
                        ts.wrapping_add(step as u64)
                    } else {
                        ts
                    }
                })
                .collect();
            out.push((base, q, step, I30, I30, false));
        }
        // a duplicate and a one-slot gap at the boundary
        let mut dup = old.clone();
        dup[3] = dup[2].wrapping_add(step as u64);
        dup[4] = dup[3] + I30;
        dup[5] = dup[4] + I30;
        out.push((base, dup, step, I30, I30, false));
        let mut gap = old.clone();
        gap[3] = (dup_base(&old) + 2 * I30).wrapping_add(step as u64);
        gap[4] = gap[3] + I30;
        gap[5] = gap[4] + I30;
        out.push((base, gap, step, I30, I30, false));
        // a 60 fps camera into the 30 fps canvas, unknown source step
        let cam: Vec<u64> = (1..=8u64).map(|k| base + k * I60).collect();
        out.push((base, cam.clone(), step, I60, I30, false));
        out.push((base, cam, step, 0, I30, false));
        // an empty queue
        out.push((base, Vec::new(), step, I30, I30, false));
        out.push((0, Vec::new(), step, I30, I30, true));
    }
    // the negative-step overlap: 54 old, 6 new stamped |S| back (values overlap)
    let mut q: Vec<u64> = (0..54u64).map(|k| W0 - 60 * I30 + k * I30).collect();
    let last = *q.last().expect("non-empty");
    for k in 1..=6u64 {
        q.push((last + k * I30).wrapping_add((-S) as u64));
    }
    out.push((W0 - 61 * I30, q, -S, I30, I30, false));
    // a non-booked jump inside the queue (700 ms) is no boundary
    let mut q: Vec<u64> = (1..=4u64).map(|k| W0 + k * I30).collect();
    q.push(q[3] + 700_000_000);
    out.push((W0, q, S, I30, I30, false));
    out
}

fn dup_base(old: &[u64]) -> u64 {
    old[2]
}

/// `(initial arrival, [(prev, stamp, src)])` arrival sequences.
#[allow(clippy::type_complexity)]
fn arrival_vectors() -> Vec<(Arrival, Vec<(u64, u64, u64)>)> {
    let until = W0 + S as u64 + 70_000_000;
    let open = |step: i64| Arrival {
        step_ns: step,
        until_ns: until,
        frame_ns: I30,
        old_epoch: true,
    };
    let raw = W0 - 3_000_000;
    let rel = raw + S as u64;
    vec![
        // old frames relabelled until the window ends
        (
            open(S),
            (1..=4u64)
                .map(|k| (rel + (k - 1) * I30, raw + k * I30, I30))
                .collect(),
        ),
        // the sender steps on the second frame; nothing after
        (
            open(S),
            vec![
                (rel, raw + I30, I30),
                (rel + I30, rel + 2 * I30, I30),
                (rel + 2 * I30, raw + 3 * I30, I30),
            ],
        ),
        // a real jump, then nothing
        (
            open(S),
            vec![(rel, raw + 700_000_000, I30), (raw + 700_000_000, raw, I30)],
        ),
        // a negative step and a duplicate / a gap on the old timeline
        (
            open(-S),
            vec![
                (raw - S as u64, raw, I30),
                (raw - S as u64, raw + 2 * I30, I30),
            ],
        ),
        // unknown source step (0), a closed window, no previous stamp
        (open(S), vec![(rel, raw + I30, 0)]),
        (
            Arrival {
                old_epoch: false,
                ..open(S)
            },
            vec![(rel, raw + I30, I30)],
        ),
        (open(S), vec![(0, raw + I30, I30)]),
        (Arrival::default(), vec![(rel, raw + I30, I30)]),
        // a relabelled stamp exactly AT the window end is still taken
        (
            open(S),
            vec![(until - I30, until.wrapping_sub(S as u64), I30)],
        ),
        // a raw and a relabelled delta exactly as far off the step: the sender has stepped
        (
            Arrival {
                step_ns: 2 * I30 as i64,
                until_ns: u64::MAX,
                frame_ns: I30,
                old_epoch: true,
            },
            vec![(rel, rel, I30)],
        ),
        // extremes
        (
            Arrival {
                step_ns: i64::MIN,
                until_ns: u64::MAX,
                frame_ns: u64::MAX,
                old_epoch: true,
            },
            vec![(1, u64::MAX, u64::MAX), (u64::MAX, 0, 0)],
        ),
    ]
}

/// Booking read sequences `(mono_before, wall, mono_after)`, each on ONE fresh booking.
fn booking_sequences() -> Vec<Vec<(u64, u64, u64)>> {
    let mut out = Vec::new();
    for step in [S, -S, -51_039_000i64] {
        let mut s = Vec::new();
        for k in 0..20u64 {
            let mono = MONO0 + k * I30;
            let mut wall = W0 + k * I30;
            if k >= 10 {
                wall = wall.wrapping_add(step as u64);
            }
            s.push((mono, wall, mono + 1_000 + (k % 3) * 500));
        }
        // a preempted read, then a second step
        s.push((MONO0 + 20 * I30, W0, MONO0 + 20 * I30 + 200_000));
        s.push((
            MONO0 + 21 * I30,
            (W0 + 21 * I30).wrapping_add(step as u64) + 200_000_000,
            MONO0 + 21 * I30 + 900,
        ));
        out.push(s);
    }
    let ns = 1_000_000_000u64;
    // a stale reference (2 s, then 3 h, of no trusted read), a step after reads resumed
    out.push(vec![
        (MONO0, W0, MONO0 + 1_000),
        (
            MONO0 + 2 * ns,
            (W0 + 2 * ns).wrapping_add(S as u64),
            MONO0 + 2 * ns + 1_000,
        ),
        (
            MONO0 + 3 * 3600 * ns,
            (W0 + 3 * 3600 * ns).wrapping_add(S as u64) + 150_000_000,
            MONO0 + 3 * 3600 * ns + 1_000,
        ),
        (
            MONO0 + 3 * 3600 * ns + I30,
            (W0 + 3 * 3600 * ns + I30).wrapping_add(S as u64) + 350_000_000,
            MONO0 + 3 * 3600 * ns + I30 + 1_000,
        ),
    ]);
    // an untrusted read 0.9 s in never refreshes the reference; exactly 1 s still books
    out.push(vec![
        (MONO0, W0, MONO0 + 1_000),
        (MONO0 + 900_000_000, W0 + 900_000_000, MONO0 + 900_500_000),
        (
            MONO0 + 1_200_000_000,
            (W0 + 1_200_000_000).wrapping_add(S as u64),
            MONO0 + 1_200_001_000,
        ),
    ]);
    out.push(vec![
        (MONO0, W0, MONO0 + 1_000),
        (
            MONO0 + ns,
            (W0 + ns).wrapping_add(S as u64),
            MONO0 + ns + 1_000,
        ),
    ]);
    out
}

/// One whole apply + receive scenario: the queue, the locked boundary, the last received stamp,
/// the reads that book, then `(interval, src, reserve, wall_now)` for the apply and the arrivals.
struct Scenario {
    queue: Vec<u64>,
    boundary: u64,
    rx_last: u64,
    pre_arrivals: Vec<(u64, u64)>,
    /// The source's previous release (an apply of the empty booking), `None` = never released.
    prev_release: Option<u64>,
    reads: Vec<(u64, u64, u64)>,
    interval: u64,
    src: u64,
    reserve: u64,
    wall_now: u64,
    arrivals: Vec<(u64, u64)>,
}

fn reads_for(step: i64) -> Vec<(u64, u64, u64)> {
    vec![
        (MONO0, W0 - I30, MONO0 + 1_000),
        (
            MONO0 + I30,
            W0.wrapping_add(step as u64),
            MONO0 + I30 + 1_000,
        ),
    ]
}

fn scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    for step in [S, S_RAW, -S, 200_000_000, -51_039_000] {
        // the receiver stepped first: a deep queue, then late old arrivals, then the sender steps
        let presented = W0 - 1_026_000_000;
        let queue: Vec<u64> = (1..=31u64).map(|k| presented + k * I30).collect();
        let last = queue[30];
        let mut arrivals = Vec::new();
        for k in 1..=3u64 {
            arrivals.push((last + k * I30, MONO0 + I30 + k));
        }
        arrivals.push(((last + 4 * I30).wrapping_add(step as u64), MONO0 + I30 + 9));
        arrivals.push((last + 5 * I30, MONO0 + I30 + 10));
        out.push(Scenario {
            queue,
            boundary: presented + I30,
            rx_last: last,
            pre_arrivals: Vec::new(),
            prev_release: Some(RELEASE),
            reads: reads_for(step),
            interval: I30,
            src: I30,
            reserve: 1_026_000_000,
            wall_now: W0.wrapping_add(step as u64) + 5_000_000,
            arrivals,
        });
        // the sender stepped first, its new frames already presented: the jump is remembered on
        // arrival before the booking
        let old = W0 - 2 * I30;
        let first_new = (old + I30).wrapping_add(step as u64);
        out.push(Scenario {
            queue: vec![first_new + I30, first_new + 2 * I30],
            boundary: first_new + I30,
            rx_last: old,
            pre_arrivals: vec![(first_new, MONO0 + I30 - 30_000_000)],
            prev_release: Some(RELEASE),
            reads: reads_for(step),
            interval: I30,
            src: I30,
            reserve: 3_000_000,
            wall_now: W0.wrapping_add(step as u64),
            arrivals: vec![(first_new + 3 * I30, MONO0 + I30 + 5)],
        });
        // a 60 fps camera with an unknown source step, unlocked
        let cam: Vec<u64> = (1..=4u64).map(|k| W0 - 60 * I60 + k * I60).collect();
        let cam_last = cam[3];
        out.push(Scenario {
            queue: cam,
            boundary: 0,
            rx_last: cam_last,
            pre_arrivals: Vec::new(),
            prev_release: Some(RELEASE),
            reads: reads_for(step),
            interval: I30,
            src: 0,
            reserve: 3_000_000,
            wall_now: W0.wrapping_add(step as u64),
            arrivals: vec![
                (cam_last + I60, MONO0 + I30 + 1),
                (cam_last + 2 * I60 + 700_000_000, MONO0 + I30 + 2),
                // a 70 ms jump with no learned step: remembered (|70 ms| over 50 ms)
                (cam_last + 2 * I60 + 770_000_000, MONO0 + I30 + 3),
            ],
        });
    }
    // a source that was not releasing at the step: never released, silent for an hour, and one
    // exactly at the 1 s bound (still releasing); a stale boundary from before the silence
    for prev_release in [
        None,
        Some(APPLY_MONO - 3_600_000_000_000),
        Some(APPLY_MONO - 1_000_000_000),
    ] {
        let fresh: Vec<u64> = (1..=3u64).map(|k| W0 - 4 * I30 + k * I30).collect();
        out.push(Scenario {
            rx_last: fresh[2],
            queue: fresh,
            boundary: W0 - 3_600_000_000_000,
            pre_arrivals: Vec::new(),
            prev_release,
            reads: reads_for(S),
            interval: I30,
            src: I30,
            reserve: 3_000_000,
            wall_now: W0 + S as u64,
            arrivals: vec![(W0, MONO0 + I30 + 9)],
        });
    }
    // no booking at all, and an unknown interval
    out.push(Scenario {
        queue: vec![W0, W0 + I30],
        boundary: W0,
        rx_last: W0 + I30,
        pre_arrivals: Vec::new(),
        prev_release: Some(RELEASE),
        reads: vec![
            (MONO0, W0, MONO0 + 100),
            (MONO0 + I30, W0 + I30, MONO0 + I30 + 100),
        ],
        interval: I30,
        src: I30,
        reserve: 3_000_000,
        wall_now: W0 + I30,
        arrivals: vec![(W0 + 2 * I30, MONO0 + I30)],
    });
    out.push(Scenario {
        queue: vec![W0, W0 + I30],
        boundary: W0,
        rx_last: W0 + I30,
        pre_arrivals: Vec::new(),
        prev_release: Some(RELEASE),
        reads: reads_for(S),
        interval: 0,
        src: I30,
        reserve: 3_000_000,
        wall_now: W0 + S as u64,
        arrivals: vec![(W0 + 2 * I30, MONO0 + I30)],
    });
    out
}

fn fmt_state(st: &RelabelState) -> String {
    format!(
        "seq {} step {} until {} frame {} old {} jump {} at {} relabelled {} release {}",
        st.seq,
        st.arrival.step_ns,
        st.arrival.until_ns,
        st.arrival.frame_ns,
        b(st.arrival.old_epoch),
        st.jump_ns,
        st.jump_mono_ns,
        st.relabelled,
        st.last_release_mono_ns
    )
}

fn rust_trace() -> Vec<String> {
    let mut out = Vec::new();
    for d in deltas() {
        for src in SRCS {
            out.push(format!(
                "dev {d} {src} {} {}",
                dev_ns(d, src),
                b(jump_recorded(d, src))
            ));
            for frame in FRAMES {
                out.push(format!(
                    "cont {d} {src} {frame} {}",
                    b(continuous(d, src, frame))
                ));
                for s in steps() {
                    out.push(format!(
                        "carry {d} {s} {src} {frame} {}",
                        b(delta_carries_step(d, s, src, frame))
                    ));
                }
            }
        }
    }
    for s in steps() {
        out.push(format!("relabels {s} {}", b(step_relabels(s))));
    }
    for (jump, at, s, win) in [
        (-S + I30 as i64, MONO0, -S, 1_026_000_000u64),
        (-S + I30 as i64, MONO0 - 1_026_000_000, -S, 1_026_000_000),
        (-S + I30 as i64, MONO0 - 1_026_000_001, -S, 1_026_000_000),
        (700_000_000, MONO0, -S, 1_026_000_000),
        (0, MONO0, S, 1_026_000_000),
        (S + I30 as i64, u64::MAX - 5, S, u64::MAX),
    ] {
        out.push(format!(
            "before {jump} {at} {s} {win} {}",
            b(sender_stepped_before(jump, at, s, I30, I30, MONO0, win))
        ));
    }
    for (r, a) in WINDOW_VECTORS {
        out.push(format!("window {r} {a} {}", window_ns(r, a)));
    }
    for (n, seq) in booking_sequences().iter().enumerate() {
        let mut bk = Booking::new();
        for &(mb, w, ma) in seq {
            let booked = bk.observe(mb, w, ma);
            out.push(format!(
                "book {n} {} {} {} {} {} {}",
                b(booked),
                bk.seq,
                bk.step_ns,
                bk.wall_ns,
                bk.mono_ns,
                bk.last_mono_ns
            ));
        }
    }
    for (n, (prev, q, s, src, frame, stepped)) in plan_vectors().iter().enumerate() {
        let p = plan(*prev, q, *s, *src, *frame, *stepped);
        out.push(format!(
            "plan {n} {} {} {}",
            p.queue_old,
            b(p.prev_old),
            b(p.newest_old)
        ));
    }
    for (n, (init, seq)) in arrival_vectors().iter().enumerate() {
        let mut a = *init;
        for &(prev, stamp, src) in seq {
            let add = arrival_add(&mut a, prev, stamp, src);
            out.push(format!("arrive {n} {add} {}", b(a.old_epoch)));
        }
    }
    for (n, sc) in scenarios().iter().enumerate() {
        let mut st = RelabelState::default();
        let mut bk = Booking::new();
        let mut queue = sc.queue.clone();
        let mut boundary = sc.boundary;
        let mut rx_last = sc.rx_last;
        for &(stamp, mono) in &sc.pre_arrivals {
            let got = st.receive(rx_last, stamp, sc.src, mono);
            rx_last = got;
            out.push(format!("pre {n} {got} {}", fmt_state(&st)));
        }
        if let Some(m) = sc.prev_release {
            let none = Booking::new();
            st.apply(
                &none,
                &mut queue,
                &mut boundary,
                &mut rx_last,
                sc.interval,
                sc.src,
                sc.reserve,
                sc.wall_now,
                m,
            );
        }
        for &(mb, w, ma) in &sc.reads {
            bk.observe(mb, w, ma);
        }
        let p = st.apply(
            &bk,
            &mut queue,
            &mut boundary,
            &mut rx_last,
            sc.interval,
            sc.src,
            sc.reserve,
            sc.wall_now,
            APPLY_MONO,
        );
        let plan_s = p.map_or("none".to_string(), |p| {
            format!("{} {} {}", p.queue_old, b(p.prev_old), b(p.newest_old))
        });
        out.push(format!(
            "apply {n} {plan_s} boundary {boundary} rx {rx_last} {}",
            fmt_state(&st)
        ));
        let q: Vec<String> = queue.iter().map(u64::to_string).collect();
        out.push(format!("queue {n} {}", q.join(" ")));
        // a second apply of the same booking changes nothing
        let again = st.apply(
            &bk,
            &mut queue,
            &mut boundary,
            &mut rx_last,
            sc.interval,
            sc.src,
            sc.reserve,
            sc.wall_now,
            APPLY_MONO,
        );
        out.push(format!("again {n} {}", b(again.is_some())));
        for &(stamp, mono) in &sc.arrivals {
            let got = st.receive(rx_last, stamp, sc.src, mono);
            rx_last = got;
            out.push(format!("recv {n} {got} {}", fmt_state(&st)));
        }
    }
    out
}

fn c_arr(v: &[u64]) -> String {
    if v.is_empty() {
        return "{0}".to_string();
    }
    let items: Vec<String> = v.iter().map(|x| format!("{x}ULL")).collect();
    format!("{{{}}}", items.join(", "))
}

fn c_harness() -> String {
    let mut body = String::new();
    body.push_str("\tstatic const int64_t deltas[] = {");
    body.push_str(
        &deltas()
            .iter()
            .map(|&d| c_i64(d))
            .collect::<Vec<_>>()
            .join(", "),
    );
    body.push_str("};\n\tstatic const int64_t steps[] = {");
    body.push_str(
        &steps()
            .iter()
            .map(|&d| c_i64(d))
            .collect::<Vec<_>>()
            .join(", "),
    );
    body.push_str(&format!(
        "}};\n\tstatic const uint64_t srcs[] = {};\n\tstatic const uint64_t frames[] = {};\n",
        c_arr(&SRCS),
        c_arr(&FRAMES)
    ));
    body.push_str(
        "\tfor (size_t a = 0; a < sizeof deltas / sizeof deltas[0]; a++)\n\
         \t\tfor (size_t c = 0; c < 4; c++) {\n\
         \t\t\tprintf(\"dev %\" PRId64 \" %\" PRIu64 \" %\" PRIu64 \" %d\\n\", deltas[a], srcs[c],\n\
         \t\t\t       genlock_fifo_relabel_dev_ns(deltas[a], srcs[c]),\n\
         \t\t\t       genlock_fifo_relabel_jump_recorded(deltas[a], srcs[c]) ? 1 : 0);\n\
         \t\t\tfor (size_t f = 0; f < 3; f++) {\n\
         \t\t\t\tprintf(\"cont %\" PRId64 \" %\" PRIu64 \" %\" PRIu64 \" %d\\n\", deltas[a], srcs[c], frames[f],\n\
         \t\t\t\t       genlock_fifo_relabel_continuous(deltas[a], srcs[c], frames[f]) ? 1 : 0);\n\
         \t\t\t\tfor (size_t s = 0; s < sizeof steps / sizeof steps[0]; s++)\n\
         \t\t\t\t\tprintf(\"carry %\" PRId64 \" %\" PRId64 \" %\" PRIu64 \" %\" PRIu64 \" %d\\n\", deltas[a],\n\
         \t\t\t\t\t       steps[s], srcs[c], frames[f],\n\
         \t\t\t\t\t       genlock_fifo_relabel_delta_carries_step(deltas[a], steps[s], srcs[c], frames[f]) ? 1 : 0);\n\
         \t\t\t}\n\
         \t\t}\n\
         \tfor (size_t s = 0; s < sizeof steps / sizeof steps[0]; s++)\n\
         \t\tprintf(\"relabels %\" PRId64 \" %d\\n\", steps[s], genlock_fifo_relabel_step_relabels(steps[s]) ? 1 : 0);\n",
    );
    for (jump, at, s, win) in [
        (-S + I30 as i64, MONO0, -S, 1_026_000_000u64),
        (-S + I30 as i64, MONO0 - 1_026_000_000, -S, 1_026_000_000),
        (-S + I30 as i64, MONO0 - 1_026_000_001, -S, 1_026_000_000),
        (700_000_000, MONO0, -S, 1_026_000_000),
        (0, MONO0, S, 1_026_000_000),
        (S + I30 as i64, u64::MAX - 5, S, u64::MAX),
    ] {
        body.push_str(&format!(
            "\tbefore_line({}, {at}ULL, {}, {win}ULL);\n",
            c_i64(jump),
            c_i64(s)
        ));
    }
    for (r, a) in WINDOW_VECTORS {
        body.push_str(&format!(
            "\tprintf(\"window %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \"\\n\", (uint64_t){r}ULL, \
             (uint64_t){a}ULL, genlock_fifo_relabel_window_ns({r}ULL, {a}ULL));\n"
        ));
    }
    for (n, seq) in booking_sequences().iter().enumerate() {
        body.push_str(
            "\t{\n\t\tstruct genlock_fifo_relabel_booking bk;\n\t\tmemset(&bk, 0, sizeof bk);\n",
        );
        for &(mb, w, ma) in seq {
            body.push_str(&format!(
                "\t\tbook_line({n}, &bk, {mb}ULL, {w}ULL, {ma}ULL);\n"
            ));
        }
        body.push_str("\t}\n");
    }
    for (n, (prev, q, s, src, frame, stepped)) in plan_vectors().iter().enumerate() {
        body.push_str(&format!(
            "\t{{\n\t\tuint64_t q[] = {};\n\t\tplan_line({n}, {prev}ULL, q, {}, {}, {src}ULL, \
             {frame}ULL, {});\n\t}}\n",
            c_arr(q),
            q.len(),
            c_i64(*s),
            if *stepped { "true" } else { "false" }
        ));
    }
    for (n, (init, seq)) in arrival_vectors().iter().enumerate() {
        body.push_str(&format!(
            "\t{{\n\t\tstruct genlock_fifo_relabel_arrival a = {{{}, {}ULL, {}ULL, {}}};\n",
            c_i64(init.step_ns),
            init.until_ns,
            init.frame_ns,
            if init.old_epoch { "true" } else { "false" }
        ));
        for &(prev, stamp, src) in seq {
            body.push_str(&format!(
                "\t\tarrive_line({n}, &a, {prev}ULL, {stamp}ULL, {src}ULL);\n"
            ));
        }
        body.push_str("\t}\n");
    }
    for (n, sc) in scenarios().iter().enumerate() {
        body.push_str(&format!(
            "\t{{\n\t\tstruct genlock_fifo_relabel_state st;\n\t\tmemset(&st, 0, sizeof st);\n\
             \t\tstruct genlock_fifo_relabel_booking bk;\n\t\tmemset(&bk, 0, sizeof bk);\n\
             \t\tuint64_t q[] = {};\n\t\tconst size_t qn = {};\n\
             \t\tuint64_t boundary = {}ULL;\n\t\tuint64_t rx_last = {}ULL;\n",
            c_arr(&sc.queue),
            sc.queue.len(),
            sc.boundary,
            sc.rx_last
        ));
        for &(stamp, mono) in &sc.pre_arrivals {
            body.push_str(&format!(
                "\t\trx_last = genlock_fifo_relabel_receive(&st, rx_last, {stamp}ULL, {}ULL, {mono}ULL);\n\
                 \t\tprintf(\"pre {n} %\" PRIu64 \" \", rx_last);\n\t\tstate_line(&st);\n",
                sc.src
            ));
        }
        if let Some(m) = sc.prev_release {
            body.push_str(&format!(
                "\t\trelease_line(&st, q, qn, &boundary, &rx_last, {}ULL, {}ULL, {}ULL, {}ULL, \
                 {m}ULL);\n",
                sc.interval, sc.src, sc.reserve, sc.wall_now
            ));
        }
        for &(mb, w, ma) in &sc.reads {
            body.push_str(&format!(
                "\t\t(void)genlock_fifo_relabel_book(&bk, {mb}ULL, {w}ULL, {ma}ULL);\n"
            ));
        }
        body.push_str(&format!(
            "\t\tapply_line({n}, &st, &bk, q, qn, &boundary, &rx_last, {}ULL, {}ULL, {}ULL, {}ULL, \
             {}ULL);\n",
            sc.interval, sc.src, sc.reserve, sc.wall_now, APPLY_MONO
        ));
        for &(stamp, mono) in &sc.arrivals {
            body.push_str(&format!(
                "\t\trx_last = genlock_fifo_relabel_receive(&st, rx_last, {stamp}ULL, {}ULL, {mono}ULL);\n\
                 \t\tprintf(\"recv {n} %\" PRIu64 \" \", rx_last);\n\t\tstate_line(&st);\n",
                sc.src
            ));
        }
        body.push_str("\t}\n");
    }
    format!(
        r#"#include <inttypes.h>
#include <stdio.h>
#include <string.h>
#include "obs-genlock-fifo-relabel.h"

static uint64_t q_get(const void *ctx, size_t i)
{{
	return ((const uint64_t *)ctx)[i];
}}

static void q_set(void *ctx, size_t i, uint64_t ts)
{{
	((uint64_t *)ctx)[i] = ts;
}}

static void before_line(int64_t jump, uint64_t at, int64_t s, uint64_t win)
{{
	printf("before %" PRId64 " %" PRIu64 " %" PRId64 " %" PRIu64 " %d\n", jump, at, s, win,
	       genlock_fifo_relabel_sender_stepped_before(jump, at, s, {i30}ULL, {i30}ULL, {mono0}ULL, win) ? 1 : 0);
}}

static void book_line(int n, struct genlock_fifo_relabel_booking *bk, uint64_t mb, uint64_t w, uint64_t ma)
{{
	const bool booked = genlock_fifo_relabel_book(bk, mb, w, ma);
	printf("book %d %d %" PRIu64 " %" PRId64 " %" PRIu64 " %" PRIu64 " %" PRIu64 "\n", n, booked ? 1 : 0, bk->seq,
	       bk->step_ns, bk->wall_ns, bk->mono_ns, bk->last_mono_ns);
}}

static void plan_line(int n, uint64_t prev, uint64_t *q, size_t qn, int64_t s, uint64_t src, uint64_t frame,
		      bool stepped)
{{
	const struct genlock_fifo_relabel_queue queue = {{q, qn, q_get, q_set}};
	const struct genlock_fifo_relabel_plan p = genlock_fifo_relabel_plan(prev, &queue, s, src, frame, stepped);
	printf("plan %d %zu %d %d\n", n, p.queue_old, p.prev_old ? 1 : 0, p.newest_old ? 1 : 0);
}}

static void arrive_line(int n, struct genlock_fifo_relabel_arrival *a, uint64_t prev, uint64_t stamp, uint64_t src)
{{
	const int64_t add = genlock_fifo_relabel_arrival_add(a, prev, stamp, src);
	printf("arrive %d %" PRId64 " %d\n", n, add, a->old_epoch ? 1 : 0);
}}

static void state_line(const struct genlock_fifo_relabel_state *st)
{{
	printf("seq %" PRIu64 " step %" PRId64 " until %" PRIu64 " frame %" PRIu64 " old %d jump %" PRId64
	       " at %" PRIu64 " relabelled %" PRIu64 " release %" PRIu64 "\n",
	       st->seq, st->arrival.step_ns, st->arrival.until_ns, st->arrival.frame_ns, st->arrival.old_epoch ? 1 : 0,
	       st->jump_ns, st->jump_mono_ns, st->relabelled, st->last_release_mono_ns);
}}

static void release_line(struct genlock_fifo_relabel_state *st, uint64_t *q, size_t qn, uint64_t *boundary,
			 uint64_t *rx_last, uint64_t interval, uint64_t src, uint64_t reserve, uint64_t wall_now,
			 uint64_t mono)
{{
	const struct genlock_fifo_relabel_queue queue = {{q, qn, q_get, q_set}};
	struct genlock_fifo_relabel_booking none;
	memset(&none, 0, sizeof none);
	struct genlock_fifo_relabel_plan p = {{0, false, false}};
	(void)genlock_fifo_relabel_apply(st, &none, &queue, boundary, rx_last, interval, src, reserve, wall_now, mono,
					 &p);
}}

static void apply_line(int n, struct genlock_fifo_relabel_state *st, const struct genlock_fifo_relabel_booking *bk,
		       uint64_t *q, size_t qn, uint64_t *boundary, uint64_t *rx_last, uint64_t interval, uint64_t src,
		       uint64_t reserve, uint64_t wall_now, uint64_t mono_now)
{{
	const struct genlock_fifo_relabel_queue queue = {{q, qn, q_get, q_set}};
	struct genlock_fifo_relabel_plan p = {{0, false, false}};
	const bool applied =
		genlock_fifo_relabel_apply(st, bk, &queue, boundary, rx_last, interval, src, reserve, wall_now, mono_now, &p);
	if (applied)
		printf("apply %d %zu %d %d boundary %" PRIu64 " rx %" PRIu64 " ", n, p.queue_old, p.prev_old ? 1 : 0,
		       p.newest_old ? 1 : 0, *boundary, *rx_last);
	else
		printf("apply %d none boundary %" PRIu64 " rx %" PRIu64 " ", n, *boundary, *rx_last);
	state_line(st);
	printf("queue %d", n);
	for (size_t i = 0; i < qn; i++)
		printf(" %" PRIu64, q[i]);
	printf("\n");
	struct genlock_fifo_relabel_plan p2 = {{0, false, false}};
	const bool again =
		genlock_fifo_relabel_apply(st, bk, &queue, boundary, rx_last, interval, src, reserve, wall_now, mono_now, &p2);
	printf("again %d %d\n", n, again ? 1 : 0);
}}

int main(void)
{{
{body}	return 0;
}}
"#,
        i30 = I30,
        mono0 = MONO0
    )
}

fn c_trace() -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("genlock_fifo_relabel_parity_1372");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let harness = dir.join("harness.c");
    let bin = dir.join("harness.bin");
    fs::write(&harness, c_harness()).expect("write the parity harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu11",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wformat=2",
            "-Werror",
            "-O1",
        ])
        .arg("-I")
        .arg(repo("vendor/obs-studio/libobs"))
        .arg(&harness)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1372: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 vendored {HEADER} to prove the C and the Rust authority agree; it must FAIL rather \
                 than skip. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "issue 1372: {HEADER} (+ the parity driver) does NOT COMPILE standalone under -Wall \
         -Wextra -Wconversion -Wformat=2 -Werror:\n--- cc stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("issue 1372: the compiled parity harness failed to execute");
    assert!(
        run.status.success(),
        "issue 1372: the parity harness exited non-zero"
    );
    String::from_utf8(run.stdout)
        .expect("harness stdout is utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn c_fifo_relabel_matches_the_rust_authority_1372() {
    let c = c_trace();
    let r = rust_trace();
    assert_eq!(
        c.len(),
        r.len(),
        "issue 1372: C {} vs Rust {} trace lines",
        c.len(),
        r.len()
    );
    for (i, (cl, rl)) in c.iter().zip(&r).enumerate() {
        assert_eq!(
            cl, rl,
            "issue 1372: {HEADER} diverges from src/genlock_fifo_relabel.rs at line {i}"
        );
    }
    // The vectors must reach the paths they claim to cover.
    let count = |p: &str| r.iter().filter(|l| l.starts_with(p)).count();
    let has = |needle: &str| r.iter().any(|l| l.contains(needle));
    assert!(
        count("carry ") > 2000
            && r.iter().any(|l| l.starts_with("carry ") && l.ends_with(" 1"))
            && r.iter().any(|l| l.starts_with("plan ") && l.ends_with(" 3 1 0"))
            && r.iter().any(|l| l.starts_with("plan ") && l.ends_with(" 0 0 0"))
            && r.iter().any(|l| l.starts_with("arrive ") && l.contains(" 1600000000 1"))
            && has("apply 0 31 1 1")
            // the -S sender-first scenario: the remembered jump, nothing relabelled
            && has("apply 7 0 0 0")
            && r.iter().any(|l| l.starts_with("apply ") && l.contains(" none "))
            && !r.iter().any(|l| l.starts_with("again ") && l.ends_with(" 1")),
        "issue 1372: the parity vectors no longer exercise a boundary, a relabelled arrival, the \
         sender-first plan and an apply that does nothing"
    );
}
