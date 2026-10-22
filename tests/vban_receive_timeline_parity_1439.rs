//! Issue 1439 — EXECUTABLE C-vs-Rust parity gate for the obs-vban receive timeline.
//!
//! The patched obs-vban source (`vendor/obs-vban/src/vban-source.c`) asks the pure decision in
//! `vendor/obs-vban/src/vban-receive-timeline.h` for every packet's stamp. The plugin only compiles
//! in the genlock CI builds, so this gate buys the verification back on any Linux box:
//!
//! 1. It `#include`s the SHIPPED header by its absolute path (nothing is retyped) into a driver and
//!    compiles it with `cc -std=c11 -ffp-contract=off -Wall -Wextra -Wconversion -Wsign-conversion
//!    -Wformat=2 -Werror` (the fit is in doubles; no contraction keeps C and Rust bit-identical).
//! 2. It replays scripted packet sequences: two-clock senders at several rates, packet sizes and
//!    clock offsets with burst jitter, losses, late packets exactly at and one ns past their stamp,
//!    backwards and duplicate indexes, jumps of exactly two seconds and one packet more, an index
//!    wrap, Receive Buffer and format changes, resets, packets on the outlier line and one ns either
//!    side, a sender beyond the ±500 ppm clamp, a stale anchor beyond R and a starved fit.
//! 3. It requires the C decision and state after every packet to equal the Tier-0 authority
//!    `src/vban_receive_timeline.rs` exactly, the doubles compared bit for bit.
//!
//! Further tests here pin the wiring: the source stamps from the timeline and outputs silence on
//! it, it stays a directly stamped source on the genlock ASRC's STAMP basis (the libobs decision
//! lifted and run), and the Linux strih bundle builds and stages the plugin.
//!
//! The Rust authority is included by `#[path]`, so this file is std-only. It runs under
//! `cargo test` in CI AND standalone, with no cargo:
//!
//! ```text
//! CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/vban_receive_timeline_parity_1439.rs -o /tmp/t && /tmp/t
//! ```
//!
//! `cc` is required. Per the project's test-strictness rule this FAILS LOUDLY rather than
//! skipping when the toolchain is missing.

#[allow(dead_code)]
#[path = "../src/vban_receive_timeline.rs"]
mod vban_receive_timeline;

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use vban_receive_timeline::{
    clamp_reserve_ms, round_half_away, samples_to_ns, scale_ppb, Decision, Event, Reason, Timeline,
    FIT_WINDOW_S, JUMP_S, OUTLIER_MS, RESERVE_MS_MAX, STALE_PACKETS,
};

const HEADER: &str = "vendor/obs-vban/src/vban-receive-timeline.h";
const SOURCE: &str = "vendor/obs-vban/src/vban-source.c";
const MS: u64 = 1_000_000;

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("issue 1439: read {rel}: {e}"))
}

/// A scratch dir named by pid + a per-process counter (std only), removed on drop.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> Scratch {
    let n = SCRATCH.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "vban_receive_timeline_1439-{tag}-{}-{n}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create the scratch dir");
    Scratch(dir)
}

/// Compiles C source `c` (plus `extra` flags) and returns its stdout lines.
fn compile_and_run(tag: &str, c: &str, extra: &[&str]) -> Vec<String> {
    let dir = scratch(tag);
    let driver = dir.0.join("driver.c");
    let bin = dir.0.join("driver.bin");
    fs::write(&driver, c).expect("write the driver");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=c11",
            "-ffp-contract=off",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wsign-conversion",
            "-Wformat=2",
            "-Werror",
            "-O1",
        ])
        .args(extra)
        .arg(&driver)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1439: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 shipped C; it must FAIL rather than skip. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "issue 1439: the {tag} driver does NOT COMPILE under -Wall -Wextra -Wconversion \
         -Wsign-conversion -Wformat=2 -Werror:\n--- cc stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("issue 1439: the compiled driver failed to execute");
    assert!(
        run.status.success(),
        "issue 1439: the {tag} driver exited non-zero"
    );
    String::from_utf8(run.stdout)
        .expect("driver stdout is utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Scripts

/// One scripted packet: its arrival, its header fields, the Receive Buffer, and whether the source
/// was reconfigured right before it (`vban_recv_init`).
#[derive(Clone, Copy)]
struct Pkt {
    now: u64,
    nu: u32,
    frames: u32,
    rate: u32,
    channels: u32,
    reserve_ns: u64,
    reset: bool,
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Builds a packet script while running the Rust model, so that a packet can be steered onto the
/// model's own stamps and lines.
struct Builder {
    t: Timeline,
    pkts: Vec<Pkt>,
    now: u64,
}

impl Builder {
    fn new() -> Self {
        Builder {
            t: Timeline::new(),
            pkts: Vec::new(),
            now: 0,
        }
    }

    fn send(&mut self, p: Pkt) -> Decision {
        let p = Pkt {
            now: p.now.max(self.now),
            ..p
        };
        self.now = p.now;
        self.pkts.push(p);
        if p.reset {
            self.t = Timeline::new();
        }
        self.t
            .packet(p.now, p.nu, p.frames, p.rate, p.channels, p.reserve_ns)
    }
}

/// A two-clock sender (`ppm` off, positive = fast) with burst jitter, steered onto the boundaries.
/// The events that keep the timeline running (losses, packets on their stamp or on the outlier
/// line) are frequent, the re-anchoring ones rare, so the fit becomes valid between them.
#[allow(clippy::too_many_arguments)]
fn steered_script(
    seed: u64,
    ppm: f64,
    frames: u32,
    rate: u32,
    channels: u32,
    reserve_ms: u64,
    seconds: u64,
    events: bool,
) -> Vec<Pkt> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut b = Builder::new();
    let pkt_ns = f64::from(frames) * 1e9 / (f64::from(rate) * (1.0 + ppm * 1e-6));
    let n = seconds * u64::from(rate) / u64::from(frames);
    let t0 = 3_000_000_000u64;
    let mut reserve = reserve_ms * MS;
    let mut channels = channels;
    let mut nu: u32 = (seed as u32).wrapping_mul(7919);
    let mut burst = t0 as f64;
    let mut k: u64 = 0;
    while k < n {
        let mut reset = false;
        let mut this_nu = nu.wrapping_add(1);
        let mut lost = 0u64;
        let mut forced: Option<u64> = None;
        if events && b.t.anchored {
            let pos_next = b.t.next_pos;
            let ev = rng.below(4000);
            let rare = rng.below(10) == 0;
            match ev {
                // a loss of 1..3 packets
                0..=79 => lost = 1 + rng.below(3),
                // exactly on its stamp, and (rarely) one ns past it
                80..=99 => forced = Some(b.t.stamp_at(pos_next)),
                100..=104 if rare => forced = Some(b.t.stamp_at(pos_next) + 1),
                // on the outlier line and one ns either side
                105..=164 => {
                    let end = pos_next + u64::from(frames);
                    let line = b.t.reference(end) + (OUTLIER_MS * MS) as f64;
                    let y = (line.ceil() as u64 + rng.below(3)).saturating_sub(1);
                    forced = Some((b.t.anchor_ns + y).min(b.t.stamp_at(pos_next)));
                }
                // a network duplicate or a reorder within the stale window: an EXTRA packet whose
                // slot has passed (dropped); the sender's own counter and schedule are untouched
                165 | 166 => {
                    let back = if ev == 165 {
                        0
                    } else {
                        1 + rng.below(u64::from(STALE_PACKETS)) as u32
                    };
                    let now = b.now + 1 + rng.below(500_000);
                    b.send(Pkt {
                        now,
                        nu: nu.wrapping_sub(back),
                        frames,
                        rate,
                        channels,
                        reserve_ns: reserve,
                        reset: false,
                    });
                }
                // an index far behind the stale window: a sender restart from there
                172 if rare => this_nu = nu.wrapping_sub(STALE_PACKETS + 1 + rng.below(9) as u32),
                // a loss of exactly two seconds of audio, or one packet more
                167 | 168 if rare => {
                    lost = JUMP_S * u64::from(rate) / u64::from(frames) + u64::from(ev == 168)
                }
                169 if rare => reserve = (10 + rng.below(80)) * MS,
                170 if rare => channels = 1 + (channels % 2),
                171 if rare => reset = true,
                _ => {}
            }
        }
        // a lost packet is a slot the sender produced: the next one arrives on its own schedule
        this_nu = this_nu.wrapping_add(lost as u32);
        k += lost;
        let ready = t0 as f64 + (k + 1) as f64 * pkt_ns;
        while burst < ready {
            burst += if rng.below(70) == 0 {
                5.0e6 + rng.below(25_000_000) as f64
            } else {
                5.0e6 + rng.below(2_000_000) as f64
            };
        }
        let now = forced.unwrap_or(burst as u64 + 300_000 + rng.below(400_000));
        b.send(Pkt {
            now,
            nu: this_nu,
            frames,
            rate,
            channels,
            reserve_ns: reserve,
            reset,
        });
        nu = this_nu;
        k += 1;
    }
    b.pkts
}

/// Each re-anchoring event once, on a clean 48 kHz sender whose fit is already valid: a loss of
/// exactly two seconds (silence), a packet on its stamp and one ns past it, a duplicate, a
/// backwards index, a jump of two seconds and one packet, another R, another channel count, a reset.
fn boundary_script() -> Vec<Pkt> {
    let mut b = Builder::new();
    let mut k: u64 = 0;
    let mut nu: u32 = 1000;
    let mut channels = 2;
    let mut reserve = 40 * MS;
    let mut reset = false;
    let clean =
        |b: &mut Builder, k: &mut u64, nu: &mut u32, n: u64, ch: u32, r: u64, reset: &mut bool| {
            for _ in 0..n {
                b.send(Pkt {
                    now: 2_000_000_000 + samples_to_ns((*k + 1) * 256, 48_000),
                    nu: *nu,
                    frames: 256,
                    rate: 48_000,
                    channels: ch,
                    reserve_ns: r,
                    reset: *reset,
                });
                *reset = false;
                *k += 1;
                *nu = nu.wrapping_add(1);
            }
        };
    clean(
        &mut b,
        &mut k,
        &mut nu,
        12 * 375,
        channels,
        reserve,
        &mut reset,
    );
    // exactly two seconds lost: silence, the timeline runs on
    k += 375;
    nu = nu.wrapping_add(375);
    clean(
        &mut b,
        &mut k,
        &mut nu,
        4 * 375,
        channels,
        reserve,
        &mut reset,
    );
    // on its stamp, then one ns past it
    let at = b.t.stamp_at(b.t.next_pos);
    b.send(Pkt {
        now: at,
        nu,
        frames: 256,
        rate: 48_000,
        channels,
        reserve_ns: reserve,
        reset: false,
    });
    k += 1;
    nu = nu.wrapping_add(1);
    let late = b.t.stamp_at(b.t.next_pos) + 1;
    b.send(Pkt {
        now: late,
        nu,
        frames: 256,
        rate: 48_000,
        channels,
        reserve_ns: reserve,
        reset: false,
    });
    k += 1;
    nu = nu.wrapping_add(1);
    clean(&mut b, &mut k, &mut nu, 375, channels, reserve, &mut reset);
    // a duplicate and a packet exactly STALE_PACKETS behind (both dropped), then one more behind (a
    // restart); a dropped packet takes no slot, so the next clean packet follows the last real one
    let last = nu.wrapping_sub(1);
    for back in [0, STALE_PACKETS] {
        b.send(Pkt {
            now: 2_000_000_000 + samples_to_ns(k * 256, 48_000) + 1_000_000,
            nu: last.wrapping_sub(back),
            frames: 256,
            rate: 48_000,
            channels,
            reserve_ns: reserve,
            reset: false,
        });
    }
    clean(&mut b, &mut k, &mut nu, 375, channels, reserve, &mut reset);
    nu = nu.wrapping_sub(STALE_PACKETS + 2);
    clean(&mut b, &mut k, &mut nu, 375, channels, reserve, &mut reset);
    // two seconds and one packet lost: a sender restart
    k += 376;
    nu = nu.wrapping_add(376);
    clean(&mut b, &mut k, &mut nu, 375, channels, reserve, &mut reset);
    reserve = 25 * MS;
    clean(&mut b, &mut k, &mut nu, 375, channels, reserve, &mut reset);
    channels = 1;
    clean(&mut b, &mut k, &mut nu, 375, channels, reserve, &mut reset);
    reset = true;
    clean(&mut b, &mut k, &mut nu, 375, channels, reserve, &mut reset);
    b.pkts
}

/// A clean sender `ppm` off for `seconds`, anchored `anchor_late_ns` late.
fn clean_script(ppm: f64, seconds: u64, anchor_late_ns: u64, reserve_ms: u64) -> Vec<Pkt> {
    let mut b = Builder::new();
    let pkt_ns = 256.0 * 1e9 / (48_000.0 * (1.0 + ppm * 1e-6));
    for k in 0..(seconds * 48_000 / 256) {
        let mut now = 2_000_000_000 + ((k + 1) as f64 * pkt_ns) as u64;
        if k == 0 {
            now += anchor_late_ns;
        }
        b.send(Pkt {
            now,
            nu: k as u32,
            frames: 256,
            rate: 48_000,
            channels: 2,
            reserve_ns: reserve_ms * MS,
            reset: false,
        });
    }
    b.pkts
}

/// Every packet after the anchor 20 ms late: within R, but an outlier, so the fit starves.
fn starve_script() -> Vec<Pkt> {
    let mut b = Builder::new();
    for k in 0..800u64 {
        let late = if k == 0 { 0 } else { 20 * MS };
        b.send(Pkt {
            now: 2_000_000_000 + samples_to_ns((k + 1) * 256, 48_000) + late,
            nu: k as u32,
            frames: 256,
            rate: 48_000,
            channels: 2,
            reserve_ns: 40 * MS,
            reset: false,
        });
    }
    b.pkts
}

/// The index wraps through u32::MAX, with a loss across the wrap.
fn wrap_script() -> Vec<Pkt> {
    let mut b = Builder::new();
    let mut nu = u32::MAX - 20;
    for k in 0..60u64 {
        if k == 30 {
            nu = nu.wrapping_add(2);
        }
        b.send(Pkt {
            now: 2_000_000_000 + samples_to_ns((k + 1) * 256, 48_000),
            nu,
            frames: 256,
            rate: 48_000,
            channels: 2,
            reserve_ns: 40 * MS,
            reset: false,
        });
        nu = nu.wrapping_add(1);
    }
    b.pkts
}

fn scripts() -> Vec<Vec<Pkt>> {
    let mut v = vec![
        boundary_script(),
        clean_script(-100.0, 30, 0, 40),
        clean_script(600.0, 16, 0, 40),
        clean_script(-499.0, 30, 0, 40),
        clean_script(0.0, 16, 60 * MS, 40),
        starve_script(),
        wrap_script(),
    ];
    let configs: [(f64, u32, u32, u32, u64, u64); 6] = [
        (50.0, 256, 48_000, 2, 40, 60),
        (-50.0, 256, 48_000, 2, 40, 60),
        (-50.0, 103, 96_000, 2, 40, 20),
        (1.4, 128, 44_100, 1, 20, 40),
        (-20.0, 64, 8_000, 2, 100, 60),
        (0.0, 239, 48_000, 8, 10, 40),
    ];
    for (i, &(ppm, frames, rate, ch, r, secs)) in configs.iter().enumerate() {
        v.push(steered_script(
            i as u64 + 1,
            ppm,
            frames,
            rate,
            ch,
            r,
            secs,
            true,
        ));
    }
    v
}

// ---------------------------------------------------------------------------------------------
// Traces

const CLAMP_INPUTS: [i64; 9] = [-100, -1, 0, 1, 9, 10, 40, 500, 501];
const NS_INPUTS: [(u64, u32); 6] = [
    (0, 48_000),
    (256, 48_000),
    (103, 96_000),
    (1_587_600_001, 44_100),
    (5, 0),
    (123_456_789_012, 192_000),
];
const SCALE_INPUTS: [(u64, i64); 7] = [
    (0, 50_000),
    (1_000_000_000, 50_000),
    (1_000_000_000, -50_000),
    (999, 1_000),
    (999, -1_000),
    (28_800_000_000_000, 500_000),
    (5_333_333, -123_457),
];
const ROUND_INPUTS: [f64; 8] = [0.0, 0.49, 0.5, 2.5, -0.5, -2.5, -2.4, 123_456.499_999];

fn bits(x: f64) -> u64 {
    x.to_bits()
}

fn line(t: &Timeline, d: Decision) -> String {
    let b = &t.buckets[(t.last_k.rem_euclid(FIT_WINDOW_S as i64)) as usize];
    format!(
        "{} {} {} {} {} {} {} | {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} | {} | {} {} {} {} {} {} {} {} | {}",
        d.event as i32,
        d.reason as i32,
        d.silence_samples,
        d.silence_pos,
        d.stamp_ns,
        d.lateness_ns,
        d.late_ns,
        u8::from(t.anchored),
        t.rate,
        t.channels,
        t.reserve_ns,
        t.anchor_ns,
        t.anchor_end,
        t.last_nu,
        t.next_pos,
        t.base_stamp_ns,
        t.base_pos,
        t.step_ppb,
        t.last_k,
        t.last_fold_pos,
        u8::from(t.fit_valid),
        t.fit_ppb,
        bits(t.fit_b),
        bits(t.fit_xbar),
        bits(t.fit_ybar),
        t.phase_ns,
        t.received,
        t.lost,
        t.silence_samples,
        t.late,
        t.re_anchors,
        t.stale,
        b.k,
        b.n,
        b.x0,
        b.y0,
        bits(b.su),
        bits(b.sv),
        bits(b.suu),
        bits(b.suv),
        t.stamp_at(t.next_pos + 1000),
    )
}

fn bucket_dump(t: &Timeline) -> String {
    let mut s = String::from("buckets");
    for b in &t.buckets {
        write!(
            s,
            " {} {} {} {} {} {} {} {}",
            b.k,
            b.n,
            b.x0,
            b.y0,
            bits(b.su),
            bits(b.sv),
            bits(b.suu),
            bits(b.suv)
        )
        .unwrap();
    }
    s
}

fn rust_trace(scripts: &[Vec<Pkt>]) -> Vec<String> {
    let mut out = Vec::new();
    for &ms in &CLAMP_INPUTS {
        out.push(format!("clamp {}", clamp_reserve_ms(ms)));
    }
    for &(s, r) in &NS_INPUTS {
        out.push(format!("ns {}", samples_to_ns(s, r)));
    }
    for &(ns, ppb) in &SCALE_INPUTS {
        out.push(format!("scale {}", scale_ppb(ns, ppb)));
    }
    for &x in &ROUND_INPUTS {
        out.push(format!("round {}", round_half_away(x)));
    }
    for sc in scripts {
        let mut t = Timeline::new();
        for (i, p) in sc.iter().enumerate() {
            if p.reset {
                t = Timeline::new();
            }
            let d = t.packet(p.now, p.nu, p.frames, p.rate, p.channels, p.reserve_ns);
            out.push(line(&t, d));
            if i % 97 == 96 {
                out.push(bucket_dump(&t));
            }
        }
    }
    out
}

fn c_driver(scripts: &[Vec<Pkt>]) -> String {
    let header = repo(HEADER);
    let mut c = String::new();
    writeln!(
        c,
        "#include <stdio.h>\n#include <inttypes.h>\n#include <string.h>"
    )
    .unwrap();
    writeln!(c, "#include \"{}\"", header.display()).unwrap();
    c.push_str(
        r#"
static uint64_t bits(double x)
{
	uint64_t u;
	memcpy(&u, &x, sizeof(u));
	return u;
}

static void line(const struct vban_recv_timeline *t, struct vban_recv_decision d)
{
	const struct vban_recv_bucket *b = &t->buckets[(size_t)(((t->last_k % VBAN_RECV_FIT_WINDOW_S) + VBAN_RECV_FIT_WINDOW_S) % VBAN_RECV_FIT_WINDOW_S)];
	printf("%d %d %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRId64 " %" PRId64 " | ", d.event, d.reason, d.silence_samples,
	       d.silence_pos, d.stamp_ns, d.lateness_ns, d.late_ns);
	printf("%d %" PRIu32 " %" PRIu32 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu32 " %" PRIu64 " %" PRIu64 " %" PRIu64
	       " %" PRId64 " %" PRId64 " %" PRIu64 " %d %" PRId64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRId64 " %" PRIu64
	       " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " | ",
	       t->anchored ? 1 : 0, t->rate, t->channels, t->reserve_ns, t->anchor_ns, t->anchor_end, t->last_nu,
	       t->next_pos, t->base_stamp_ns, t->base_pos, t->step_ppb, t->last_k, t->last_fold_pos,
	       t->fit_valid ? 1 : 0, t->fit_ppb, bits(t->fit_b), bits(t->fit_xbar), bits(t->fit_ybar), t->phase_ns,
	       t->received, t->lost, t->silence_samples, t->late, t->re_anchors);
	printf("%" PRIu64 " | ", t->stale);
	printf("%" PRId64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " | %" PRIu64 "\n",
	       b->k, b->n, b->x0, b->y0, bits(b->su), bits(b->sv), bits(b->suu), bits(b->suv),
	       vban_recv_stamp_at(t, t->next_pos + 1000));
}

static void bucket_dump(const struct vban_recv_timeline *t)
{
	printf("buckets");
	for (size_t i = 0; i < VBAN_RECV_FIT_WINDOW_S; i++) {
		const struct vban_recv_bucket *b = &t->buckets[i];
		printf(" %" PRId64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64, b->k,
		       b->n, b->x0, b->y0, bits(b->su), bits(b->sv), bits(b->suu), bits(b->suv));
	}
	printf("\n");
}

struct pkt {
	uint64_t now;
	uint32_t nu, frames, rate, channels;
	uint64_t reserve_ns;
	int reset;
};
"#,
    );
    for (i, sc) in scripts.iter().enumerate() {
        writeln!(c, "static const struct pkt script_{i}[] = {{").unwrap();
        for p in sc {
            writeln!(
                c,
                "\t{{{}ULL, {}U, {}U, {}U, {}U, {}ULL, {}}},",
                p.now,
                p.nu,
                p.frames,
                p.rate,
                p.channels,
                p.reserve_ns,
                i32::from(p.reset)
            )
            .unwrap();
        }
        writeln!(c, "}};").unwrap();
    }
    writeln!(c, "static struct vban_recv_timeline t;\nint main(void)\n{{").unwrap();
    for ms in CLAMP_INPUTS {
        writeln!(
            c,
            "\tprintf(\"clamp %\" PRIu32 \"\\n\", vban_recv_clamp_reserve_ms({ms}LL));"
        )
        .unwrap();
    }
    for (s, r) in NS_INPUTS {
        writeln!(
            c,
            "\tprintf(\"ns %\" PRIu64 \"\\n\", vban_recv_samples_to_ns({s}ULL, {r}U));"
        )
        .unwrap();
    }
    for (ns, ppb) in SCALE_INPUTS {
        writeln!(
            c,
            "\tprintf(\"scale %\" PRId64 \"\\n\", vban_recv_scale_ppb({ns}ULL, {ppb}LL));"
        )
        .unwrap();
    }
    for x in ROUND_INPUTS {
        writeln!(
            c,
            "\t{{ uint64_t u = {}ULL; double x; memcpy(&x, &u, sizeof(x)); printf(\"round %\" PRId64 \"\\n\", vban_recv_round(x)); }}",
            x.to_bits()
        )
        .unwrap();
    }
    for i in 0..scripts.len() {
        writeln!(
            c,
            "\tvban_recv_init(&t);\n\
             \tfor (size_t i = 0; i < sizeof(script_{i}) / sizeof(script_{i}[0]); i++) {{\n\
             \t\tconst struct pkt *p = &script_{i}[i];\n\
             \t\tif (p->reset)\n\
             \t\t\tvban_recv_init(&t);\n\
             \t\tconst struct vban_recv_decision d = vban_recv_packet(&t, p->now, p->nu, p->frames, p->rate, p->channels, p->reserve_ns);\n\
             \t\tline(&t, d);\n\
             \t\tif (i % 97 == 96)\n\
             \t\t\tbucket_dump(&t);\n\
             \t}}"
        )
        .unwrap();
    }
    writeln!(c, "\treturn 0;\n}}").unwrap();
    c
}

#[test]
fn c_receive_timeline_matches_the_rust_authority_1439() {
    assert!(
        repo(HEADER).is_file(),
        "issue 1439: {HEADER} is missing -- the obs-vban source stamps at arrival again"
    );
    let scripts = scripts();
    let c = compile_and_run("parity", &c_driver(&scripts), &[]);
    let r = rust_trace(&scripts);
    assert_eq!(
        c.len(),
        r.len(),
        "issue 1439: C and Rust traces differ in length"
    );
    let diffs: Vec<String> = c
        .iter()
        .zip(&r)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .take(5)
        .map(|(i, (a, b))| format!("line {i}:\n  C    `{a}`\n  Rust `{b}`"))
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1439: the vendored receive timeline diverges from src/vban_receive_timeline.rs:\n{}",
        diffs.join("\n")
    );
}

#[test]
fn the_scripts_reach_every_boundary_1439() {
    // A parity gate is only as good as the states it visits (the libobs tie-break lesson).
    let names = [
        "anchors",
        "losses (silence)",
        "late re-anchors",
        "packets exactly on their stamp",
        "backward re-anchors",
        "jump re-anchors",
        "format re-anchors",
        "config re-anchors",
        "fit re-anchors: rate beyond the clamp",
        "fit re-anchors: phase beyond R",
        "fit re-anchors: starved",
        "fits becoming valid",
        "stamp step changes",
        "outlier exclusions",
        "packets within 1 ns of the outlier line",
        "index wraps",
        "losses of exactly two seconds",
        "stale duplicates (dropped)",
        "stale reorders at the window's edge (dropped)",
        "backward re-anchors one past the stale window",
    ];
    let mut hits = [0usize; 20];
    for sc in scripts() {
        let mut t = Timeline::new();
        for p in &sc {
            if p.reset {
                t = Timeline::new();
            }
            let (anchored, valid, step, last_nu) = (t.anchored, t.fit_valid, t.step_ppb, t.last_nu);
            let folds = t.buckets.iter().map(|b| b.n).sum::<u64>();
            // which fit check would fail: the model's update run on a copy, exactly when the packet
            // takes the timeline into a new bucket (the only time the real decision refits)
            let delta = p.nu.wrapping_sub(t.last_nu);
            let same = anchored
                && p.rate == t.rate
                && p.channels == t.channels
                && p.reserve_ns == t.reserve_ns
                && delta != 0
                && delta < 0x8000_0000;
            let (mut stamp, mut clamp_fail, mut phase_fail, mut line_y) = (0, false, false, 0.0);
            if same {
                let lost = u64::from(delta - 1);
                let pos = t.next_pos + lost * u64::from(p.frames);
                let end = pos + u64::from(p.frames);
                if (end / u64::from(p.rate)) as i64 > t.last_k {
                    let mut probe = t.clone();
                    if !probe.update(t.next_pos) {
                        let refit = probe.fit_valid != t.fit_valid
                            || probe.fit_b.to_bits() != t.fit_b.to_bits();
                        phase_fail = refit;
                        clamp_fail = !refit;
                    }
                }
                stamp = t.stamp_at(pos);
                line_y = t.reference(end) + (OUTLIER_MS * MS) as f64;
            }
            let d = t.packet(p.now, p.nu, p.frames, p.rate, p.channels, p.reserve_ns);
            let back = last_nu.wrapping_sub(p.nu);
            match (d.event, d.reason) {
                (Event::Stale, _) if back == 0 => hits[17] += 1,
                (Event::Stale, _) if back == STALE_PACKETS => hits[18] += 1,
                (Event::Reanchor, Reason::Backward) if back == STALE_PACKETS + 1 => {
                    hits[4] += 1;
                    hits[19] += 1;
                }
                (Event::Anchor, _) => hits[0] += 1,
                (Event::Reanchor, Reason::Late) => hits[2] += 1,
                (Event::Reanchor, Reason::Backward) => hits[4] += 1,
                (Event::Reanchor, Reason::Jump) => hits[5] += 1,
                (Event::Reanchor, Reason::Format) => hits[6] += 1,
                (Event::Reanchor, Reason::Config) => hits[7] += 1,
                (Event::Reanchor, Reason::Fit) => {
                    if clamp_fail {
                        hits[8] += 1;
                    } else if phase_fail {
                        hits[9] += 1;
                    } else {
                        hits[10] += 1;
                    }
                }
                _ => {}
            }
            if d.silence_samples > 0 {
                hits[1] += 1;
                if d.silence_samples == JUMP_S * u64::from(p.rate) {
                    hits[16] += 1;
                }
            }
            if d.event == Event::Packet && same && p.now == stamp {
                hits[3] += 1;
            }
            hits[11] += usize::from(!valid && t.fit_valid);
            hits[12] += usize::from(d.event == Event::Packet && t.step_ppb != step);
            let folded = t.buckets.iter().map(|b| b.n).sum::<u64>();
            if d.event == Event::Packet && same {
                let y = (p.now - t.anchor_ns) as f64;
                hits[13] += usize::from(folded == folds && y > line_y);
                hits[14] += usize::from((y - line_y).abs() <= 1.0);
            }
            hits[15] += usize::from(d.event == Event::Packet && same && p.nu < last_nu);
        }
    }
    for (name, n) in names.iter().zip(hits) {
        assert!(
            n >= 1,
            "issue 1439: the parity scripts hit no {name}: {hits:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The source stays on the genlock ASRC's STAMP basis (design addendum 6338457001)

/// Lifts a `static inline` helper from `obs-source.c` by its signature, up to the first `\n}\n`.
fn lift(src: &str, signature: &str) -> String {
    let a = src
        .find(signature)
        .unwrap_or_else(|| panic!("issue 1439: obs-source.c lost `{signature}`"));
    assert_eq!(
        src.matches(signature).count(),
        1,
        "issue 1439: `{signature}` is not unique in obs-source.c"
    );
    let b = src[a..].find("\n}\n").expect("the helper's end") + a + 3;
    src[a..b].to_string()
}

/// The libobs decision itself, run for the vban source's state: it must read STAMP, and the
/// stamps the timeline hands OBS (R ahead of the fitted arrival, R up to the setting's maximum)
/// must read as direct stamps against MAX_TS_VAR.
#[test]
fn the_vban_source_is_a_directly_stamped_source_on_the_asrc_stamp_basis_1439() {
    let obs_source = read("vendor/obs-studio/libobs/obs-source.c");
    let internal = read("vendor/obs-studio/libobs/obs-internal.h");
    let max_ts_var: u64 = internal
        .lines()
        .find_map(|l| l.trim().strip_prefix("#define MAX_TS_VAR "))
        .expect("issue 1439: obs-internal.h lost MAX_TS_VAR")
        .trim()
        .trim_end_matches("ULL")
        .parse()
        .expect("MAX_TS_VAR is a plain number");
    let mut defines = String::new();
    for name in [
        "GENLOCK_AUDIO_HOLD_OFF",
        "GENLOCK_AUDIO_HOLD_LATENCY",
        "GENLOCK_AUDIO_HOLD_TIMECODE",
        "GENLOCK_AUDIO_HOLD_PENDING",
        "GENLOCK_AUDIO_ASRC_BASIS_ARRIVAL",
        "GENLOCK_AUDIO_ASRC_BASIS_TIMECODE",
        "GENLOCK_AUDIO_ASRC_BASIS_STAMP",
    ] {
        let l = obs_source
            .lines()
            .find(|l| l.starts_with(&format!("#define {name} ")))
            .unwrap_or_else(|| panic!("issue 1439: obs-source.c lost #define {name}"));
        defines.push_str(l);
        defines.push('\n');
    }
    let hold = lift(&obs_source, "static inline int genlock_audio_hold_mode(");
    let placement = lift(
        &obs_source,
        "static inline int genlock_audio_asrc_placement(",
    );
    let reserve_max_ns = RESERVE_MS_MAX as u64 * MS;
    let c = format!(
        "#include <stdbool.h>\n#include <stdint.h>\n#include <stdio.h>\n{defines}\n{hold}\n{placement}\n\
         static uint64_t diff(uint64_t a, uint64_t b) {{ return a > b ? a - b : b - a; }}\n\
         int main(void)\n{{\n\
         \tconst uint64_t now = 5000000000ULL;\n\
         \tconst uint64_t aheads[3] = {{10000000ULL, 40000000ULL, {reserve_max_ns}ULL}};\n\
         \tfor (int i = 0; i < 3; i++) {{\n\
         \t\tconst bool direct = diff(now + aheads[i], now) < {max_ts_var}ULL;\n\
         \t\t/* the vban source: no genlock FIFO, no latency, not monitor-only, ASRC on + a resampler */\n\
         \t\tconst int mode = genlock_audio_hold_mode(false, 0u, true, 0u, false);\n\
         \t\tprintf(\"basis %d %d\\n\", i, genlock_audio_asrc_placement(mode, direct, false, true));\n\
         \t}}\n\
         \tprintf(\"monitor-only %d\\n\", genlock_audio_asrc_placement(genlock_audio_hold_mode(false, 0u, true, 0u, false), true, true, true));\n\
         \tprintf(\"stamp %d arrival %d\\n\", GENLOCK_AUDIO_ASRC_BASIS_STAMP, GENLOCK_AUDIO_ASRC_BASIS_ARRIVAL);\n\
         \treturn 0;\n}}\n"
    );
    let out = compile_and_run("asrc", &c, &["-Wno-unused-function"]);
    let stamp_arrival = out.last().expect("output").clone();
    let mut it = stamp_arrival.split_whitespace();
    let stamp: i32 = it.nth(1).unwrap().parse().unwrap();
    let arrival: i32 = it.nth(1).unwrap().parse().unwrap();
    for (i, l) in out.iter().take(3).enumerate() {
        assert_eq!(
            l,
            &format!("basis {i} {stamp}"),
            "issue 1439: a vban stamp R ahead must keep the source on the ASRC STAMP basis: {out:?}"
        );
    }
    assert_eq!(
        out[3],
        format!("monitor-only {arrival}"),
        "issue 1439: the control (a monitor-only source) must read the arrival basis: {out:?}"
    );

    // The source's own flags and stamping keep it there.
    let src = read(SOURCE);
    let squished = src.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        ".output_flags = OBS_SOURCE_AUDIO | OBS_SOURCE_DO_NOT_DUPLICATE,",
        "audio.timestamp = d.stamp_ns; obs_source_output_audio(s->context, &audio);",
        "silence.timestamp = vban_recv_stamp_at(&s->timeline, d->silence_pos + done);",
    ] {
        assert!(
            squished.contains(needle),
            "issue 1439: vban-source.c lost `{needle}`"
        );
    }
    let mut plugin = String::new();
    for f in fs::read_dir(repo("vendor/obs-vban/src")).unwrap() {
        plugin.push_str(&fs::read_to_string(f.unwrap().path()).unwrap());
    }
    for banned in [
        "obs_source_set_asrc_enabled",
        "obs_source_set_genlock_fifo",
        "obs_source_set_monitoring_type",
        "obs_source_set_audio_mixers",
    ] {
        assert!(
            !plugin.contains(banned),
            "issue 1439: obs-vban calls `{banned}`: it must never move its source off the ASRC path"
        );
    }
    assert!(
        obs_source.contains("source->asrc_enabled = true;"),
        "issue 1439: libobs no longer turns the ASRC on for every source by default"
    );
}

// ---------------------------------------------------------------------------------------------
// Wiring

#[test]
fn the_receive_timeline_is_wired_into_the_source_1439() {
    let src = read(SOURCE);
    let squished = src.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "#include \"vban-receive-timeline.h\"",
        "vban_recv_packet(&s->timeline, now, header->nuFrame, audio.frames,",
        "if (os_atomic_exchange_bool(&s->timeline_reset, false)) vban_recv_init(&s->timeline);",
        "(uint64_t)os_atomic_load_long(&s->receive_buffer_ms) * VBAN_RECV_NS_PER_MS;",
        "vban_recv_clamp_reserve_ms((int64_t)obs_data_get_int(settings, \"receive_buffer_ms\"))",
        "obs_data_set_default_int(data, \"receive_buffer_ms\", VBAN_RECV_RESERVE_MS_DEFAULT);",
        "obs_properties_add_int(props, \"receive_buffer_ms\",",
        // the silence keeps the packet's own sample format (review round 1: a format change resets
        // libobs's resampler and the genlock ASRC), U8 silence is 0x80
        "if (d.silence_samples > 0) vban_src_output_silence(s, &d, &audio);",
        ".format = like->format,",
        "memset(s->silence.array, like->format == AUDIO_FORMAT_U8BIT ? 0x80 : 0, plane_bytes);",
        "\"obs-vban receive: received=%\" PRIu64 \" lost=%\" PRIu64 \" late=%\" PRIu64 \" re_anchors=%\" PRIu64",
        "\"obs-vban receive re-anchor: source '%s' reason=%s",
        "\"obs-vban receive anchored: source '%s'",
    ] {
        assert!(
            squished.contains(needle),
            "issue 1439: vban-source.c lost `{needle}`"
        );
    }
    // The order inside the callback: the decision, then the silence, then the packet.
    let at = |n: &str| {
        squished
            .find(n)
            .unwrap_or_else(|| panic!("issue 1439: `{n}` missing"))
    };
    assert!(
        at("vban_recv_packet(&s->timeline") < at("vban_src_output_silence(s, &d,")
            && at("vban_src_output_silence(s, &d,") < at("audio.timestamp = d.stamp_ns;"),
        "issue 1439: the callback must decide, then output the silence, then the packet"
    );
    // The stock arrival stamping and the copy-padding are gone for good.
    for gone in [
        "Padding packet",
        "lost_ns",
        "os_gettime_ns() - (uint64_t)audio.frames",
        ".format = AUDIO_FORMAT_FLOAT_PLANAR,",
    ] {
        assert!(
            !src.contains(gone),
            "issue 1439: `{gone}` is back in vban-source.c"
        );
    }
    let mut status_lines = 0;
    for f in fs::read_dir(repo("vendor/obs-vban/src")).unwrap() {
        status_lines += fs::read_to_string(f.unwrap().path())
            .unwrap()
            .matches("obs-vban receive: ")
            .count();
    }
    assert_eq!(
        status_lines, 1,
        "issue 1439: `obs-vban receive: ` must appear on exactly one log line (the 10 s status)"
    );
    let locale = read("vendor/obs-vban/data/locale/en-US.ini");
    assert!(
        locale.contains("VBAN.src.prop.receive_buffer_ms="),
        "issue 1439: the Receive Buffer property has no label"
    );
}

/// The Linux strih bundle builds the vendored obs-vban against the genlocked SDK and stages it next
/// to DistroAV; a vendor/obs-vban change re-runs the Linux build.
#[test]
fn the_linux_strih_bundle_builds_and_stages_obs_vban_1439() {
    let wf = read(".github/workflows/linux-genlock.yml");
    assert!(
        wf.contains("      - 'vendor/obs-vban/**'"),
        "issue 1439: linux-genlock.yml must trigger on vendor/obs-vban/**"
    );
    let job = wf
        .find("  linux-genlock-build-strih:")
        .expect("the strih job");
    let strih = &wf[job..];
    let at = |n: &str| {
        strih
            .find(n)
            .unwrap_or_else(|| panic!("issue 1439: the strih job lost `{n}`"))
    };
    let build_dav = at("      - name: Build DistroAV");
    let configure =
        at("      - name: Configure obs-vban against the genlocked OBS SDK (issue 1439)");
    let build = at("      - name: Build obs-vban (issue 1439)");
    let stage = at("      - name: Stage artifact");
    assert!(
        build_dav < configure && configure < build && build < stage,
        "issue 1439: the strih job must configure + build obs-vban after DistroAV, before staging"
    );
    for needle in [
        "working-directory: vendor/obs-vban",
        "-DCMAKE_PREFIX_PATH=\"$GITHUB_WORKSPACE/obs-sdk\"",
        "find vendor/obs-vban/build_ubuntu -name 'obs-vban.so' -not -path '*/CMakeFiles/*'",
        "cp \"$vban_so\" \"$plugin_lib_dir/\"",
        "cp -a vendor/obs-vban/data/. \"$data_root/obs-vban/\"",
    ] {
        assert!(
            strih.contains(needle),
            "issue 1439: the strih job lost `{needle}`"
        );
    }
    // The imag-parity Linux bundle stays without it (imag consumes no VBAN).
    let imag = &wf[wf.find("  linux-genlock-build:").unwrap()..job];
    assert!(
        !imag.contains("obs-vban"),
        "issue 1439: the imag-parity Linux bundle must not build obs-vban"
    );
}

/// Both Windows genlock builds compile the same source: their pwsh gate checks the receive side too.
#[test]
fn both_windows_builds_assert_the_receive_timeline_1439() {
    for name in ["windows-genlock.yml", "windows-genlock-fast.yml"] {
        let yml = read(&format!(".github/workflows/{name}"));
        for needle in [
            "Test-Path \"vendor/obs-vban/src/vban-receive-timeline.h\"",
            "[regex]::Escape('#include \"vban-receive-timeline.h\"')",
            "[regex]::Escape('vban_recv_packet(&s->timeline, now, header->nuFrame,')",
            "[regex]::Escape('\"obs-vban receive: received=%\"')",
            "$source -match [regex]::Escape('Padding packet')",
        ] {
            assert!(
                yml.contains(needle),
                "issue 1439: {name}'s obs-vban assert lost `{needle}`"
            );
        }
    }
}
