//! Issues 1372 + 1381 — EXECUTABLE C-vs-Rust parity gate for the obs-vban send pacing.
//!
//! The patched obs-vban send thread (`vendor/obs-vban/src/vban-output-thread.c`) calls the pure
//! decision in `vendor/obs-vban/src/vban-pacing.h` on every wake. That plugin only compiles on the
//! Windows CI runner, so this gate buys the verification back on Linux:
//!
//! 1. It `#include`s the SHIPPED header by its absolute path (nothing is retyped) into a driver
//!    and compiles it with `cc -Wall -Wextra -Wconversion -Wsign-conversion -Wformat=2 -Werror`.
//! 2. It drives scripted wakes (`now`, `buffered`) that land on every boundary of the fixed
//!    timeline (issue 1381): a packet one sample short, the anchor instant and one ns before it, a
//!    deadline and one ns before it, several due packets in one wake, a slot waiting for its audio
//!    one ns before and exactly at the grace end, the catch-up cap instants, a silence episode one
//!    sample below and exactly at the resume depth, the stale repay one sample short and exactly
//!    met, the buffer ceiling and one sample over it, the schedule ceiling and one ns over it, and
//!    other rates, packet sizes and targets (clamped ones included), retargets while running,
//!    starved and silent.
//! 3. It requires the C decision and state after every wake to equal the Tier-0 authority
//!    `src/vban_pacing.rs` exactly.
//!
//! The Rust authority is included by `#[path]`, so this file is std-only. It runs under
//! `cargo test` in CI AND standalone, with no cargo:
//!
//! ```text
//! CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/vban_pacing_parity_1372.rs -o /tmp/t && /tmp/t
//! ```
//!
//! `cc` is required. Per the project's test-strictness rule this FAILS LOUDLY rather than
//! skipping when the toolchain is missing.

#[allow(dead_code)]
#[path = "../src/vban_pacing.rs"]
mod vban_pacing;

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use vban_pacing::{clamp_target_ms, ns_to_samples, samples_to_ns, wait_ms, Pacing, Step};

const HEADER: &str = "vendor/obs-vban/src/vban-pacing.h";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// One scripted wake: `now`, the buffered samples, and a target change applied just before it.
#[derive(Clone, Copy)]
struct Wake {
    now: u64,
    buffered: u64,
    retarget: Option<i64>,
}

/// One scripted output: its config and the wakes it sees.
struct Script {
    target_ms: i64,
    packet_samples: u32,
    rate: u32,
    wakes: Vec<Wake>,
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

/// Builds a wake script while running the Rust model, so the next wake can land on the model's
/// own deadlines, grace ends, catch-up instants and repay edges.
struct Builder {
    p: Pacing,
    wakes: Vec<Wake>,
    buffered: u64,
}

impl Builder {
    fn new(target_ms: i64, packet_samples: u32, rate: u32) -> Self {
        Builder {
            p: Pacing::new(target_ms, packet_samples, rate),
            wakes: Vec::new(),
            buffered: 0,
        }
    }

    /// One wake with `buffered` samples (and a target change just before it); the model then
    /// consumes what it sent or dropped. Returns the decision.
    fn wake(&mut self, now: u64, buffered: u64, retarget: Option<i64>) -> Step {
        self.wakes.push(Wake {
            now,
            buffered,
            retarget,
        });
        if let Some(t) = retarget {
            self.p.retarget(t);
        }
        let s = self.p.step(now, buffered);
        self.buffered =
            buffered - s.drop_samples - u64::from(s.send) * u64::from(self.p.packet_samples);
        s
    }

    fn script(self, target_ms: i64, packet_samples: u32, rate: u32) -> Script {
        Script {
            target_ms,
            packet_samples,
            rate,
            wakes: self.wakes,
        }
    }
}

/// A wake script steered by the Rust model so that it keeps landing on the boundaries.
fn steered_script(seed: u64, target_ms: i64, packet_samples: u32, rate: u32, n: usize) -> Script {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut bld = Builder::new(target_ms, packet_samples, rate);
    let ps = u64::from(bld.p.packet_samples);
    let block = 1024u64;
    let targets = [-5i64, 0, 19, 20, 30, 64, 90, 200, 250];
    let (mut now, mut wake) = (1_000_000_000u64, 0u64);
    for _ in 0..n {
        let p = &bld.p;
        let deadline = p.deadline_ns(p.n_sent);
        let (running, grace, ceiling_ns, catchup) =
            (p.running, p.grace_ns, p.ceiling_ns, p.catchup_ns);
        let (target, stale, resume) = (p.target_samples, p.stale_samples, p.resume_samples());
        let ceiling = p.ceiling_samples;
        let (pick, rare, small, big) = (
            rng.below(12),
            rng.below(8),
            rng.below(3_000_000),
            rng.below(40_000_000),
        );
        let candidate = match pick {
            0 | 1 if wake != 0 => wake,
            2 if wake != 0 => wake.saturating_sub(1),
            3 if wake != 0 => wake + small,
            4 if wake != 0 => wake + big,
            5 if running => deadline + grace,
            6 if running => (deadline + grace).saturating_sub(1),
            7 if running && rare == 0 => deadline + ceiling_ns + (small & 1),
            8 if catchup != 0 => catchup,
            _ => now + rng.below(12_000_000),
        };
        now = now.max(candidate);
        let prev = bld.buffered;
        let buffered = match rng.below(18) {
            0 => ps.saturating_sub(1),
            1 => ps,
            2 => ceiling,
            3 => ceiling + 1,
            4 => 0,
            5 => prev + 3 * block,
            6 => prev.saturating_sub(rng.below(ps)),
            7 => resume.saturating_sub(1),
            8 => resume,
            9 | 10 => target + stale + ps * rng.below(3) + rng.below(2),
            _ => prev + block * rng.below(2),
        };
        let retarget = if rng.below(20) == 0 {
            Some(targets[rng.below(targets.len() as u64) as usize])
        } else {
            None
        };
        wake = bld.wake(now, buffered, retarget).wake_ns;
    }
    bld.script(target_ms, packet_samples, rate)
}

/// Hand-written wakes that hit each boundary in order (64 ms, 239-sample packets, 48 kHz).
fn boundary_script() -> Script {
    let mut b = Builder::new(64, 239, 48_000);
    let g = b.p.grace_ns;
    b.wake(1_000_000_000, 0, None);
    b.wake(1_000_000_000, 238, None);
    b.wake(1_000_000_000, 239, None); // primed: anchor at 1.064 s
    b.wake(1_064_000_000 - 1, 4096, None);
    b.wake(1_064_000_000, 4096, None); // packet 0
    let d1 = b.p.deadline_ns(1);
    b.wake(d1 - 1, 3857, None);
    b.wake(d1, 3857, None); // packet 1

    // Packets 2, 3 and 4 in one wake: the audio is there.
    let d4 = b.p.deadline_ns(4);
    b.wake(d4, 3618, None);
    // Slot 5 waits for its audio; one ns before the grace end it is still short, then it comes.
    let d5 = b.p.deadline_ns(5);
    b.wake(d5, 100, None);
    b.wake(d5 + g - 1, 238, None);
    let mut next = b.wake(d5 + g - 1, 239 * 12, None).wake_ns; // a late send, capped catch-up
    while b.p.catchup_ns != 0 {
        next = b.wake(next, 239 * 12, None).wake_ns;
    }
    // Starve again; exactly at the grace end with no audio: a silence episode.
    let dn = b.p.deadline_ns(b.p.n_sent).max(next);
    b.wake(dn, 0, None);
    let dn = b.p.deadline_ns(b.p.n_sent);
    next = b.wake(dn + g, 0, None).wake_ns;
    // Silence one sample below the resume depth, then audio exactly at it.
    let resume = b.p.resume_samples();
    for _ in 0..30 {
        next = b.wake(next, resume - 1, None).wake_ns;
    }
    next = b.wake(next, resume, None).wake_ns;
    // Back on schedule; the repay one sample short, then exactly met, then the drop.
    while b.p.catchup_ns != 0 {
        next = b.wake(next, b.p.target_samples + 239, None).wake_ns;
    }
    let (t, stale) = (b.p.target_samples, b.p.stale_samples);
    next = b.wake(next, t + stale + 239 - 1, None).wake_ns;
    next = b.wake(next, t + stale + 239, None).wake_ns;
    next = b.wake(next, t + stale + 2 * 239, None).wake_ns;
    // The buffer ceiling exactly, then one sample over it.
    next = b.wake(next, b.p.ceiling_samples, None).wake_ns;
    next = b.wake(next, b.p.ceiling_samples + 1, None).wake_ns;
    // The schedule ceiling exactly (a silence episode), then one ns over it (a resync).
    let dn = b.p.deadline_ns(b.p.n_sent).max(next);
    b.wake(dn + b.p.ceiling_ns, 0, None);
    let dn = b.p.deadline_ns(b.p.n_sent);
    b.wake(dn + b.p.ceiling_ns + 1, 0, None);
    b.script(64, 239, 48_000)
}

/// Target changes up, down, to the same value and to a value that clamps; a large drop during a
/// dip; and a retarget while starved and while silent (64 ms, 239-sample packets, 48 kHz).
fn retarget_script() -> Script {
    let mut b = Builder::new(64, 239, 48_000);
    b.wake(1_000_000_000, 12_000, None);
    let mut next = b.wake(1_064_000_000, 12_000, None).wake_ns;
    for rt in [
        Some(100),
        None,
        None,
        Some(40),
        None,
        Some(40),
        Some(10),
        None,
        Some(0),
        None,
    ] {
        next = b.wake(next, 6_000, rt).wake_ns;
    }
    next = b.wake(next, 9_000, Some(200)).wake_ns;
    next = b.wake(next, 12_000, None).wake_ns;
    next = b.wake(next, 3_000, Some(20)).wake_ns;
    let d = b.p.deadline_ns(b.p.n_sent).max(next);
    b.wake(d, 0, None); // starved
    b.wake(d + 1, 0, Some(64)); // up while starved
    let d = b.p.deadline_ns(b.p.n_sent).max(d + 1);
    let s = b.wake(d + b.p.grace_ns, 0, None); // silent
    b.wake(s.wake_ns, 0, Some(30)); // down while silent
    b.script(64, 239, 48_000)
}

fn scripts() -> Vec<Script> {
    let mut v = vec![boundary_script(), retarget_script()];
    let configs: [(i64, u32, u32); 8] = [
        (64, 239, 48_000),
        (20, 256, 44_100),
        (200, 179, 48_000),
        (0, 239, 48_000),
        (19, 1, 8_000),
        (201, 256, 96_000),
        (-3, 359, 32_000),
        (37, 0, 0),
    ];
    for (i, &(t, ps, rate)) in configs.iter().enumerate() {
        for seed in 0..3u64 {
            v.push(steered_script(i as u64 * 16 + seed + 1, t, ps, rate, 1_500));
        }
    }
    v
}

const CLAMP_INPUTS: [i64; 10] = [-100, -1, 0, 1, 19, 20, 64, 200, 201, 100_000];
const NS_INPUTS: [(u64, u32); 7] = [
    (0, 48_000),
    (239, 48_000),
    (48_000, 48_000),
    (1_587_600_001, 44_100),
    (u64::MAX / 2_000_000_000, 48_000),
    (5, 0),
    (123_456_789_012, 192_000),
];
const NS2S_INPUTS: [(u64, u32); 6] = [
    (0, 48_000),
    (4_979_166, 48_000),
    (4_979_167, 48_000),
    (86_400_000_000_001, 48_000),
    (999_999_999, 44_100),
    (7, 0),
];
const WAIT_INPUTS: [(u64, u64); 8] = [
    (5, 0),
    (5_000_000, 5_000_000),
    (5_000_000, 4_000_000),
    (5_000_000, 5_000_001),
    (0, 3_000_000),
    (0, 3_000_001),
    (0, 10_000_000),
    (0, 250_000_000),
];

fn line(p: &Pacing, s: Step) -> String {
    format!(
        "{} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
        s.send,
        s.silence,
        s.drop_samples,
        s.wake_ns,
        u8::from(s.wait_audio),
        u8::from(p.running),
        u8::from(p.primed),
        p.prime_ns,
        p.t0_ns,
        p.n_sent,
        p.catchup_ns,
        u8::from(p.starved),
        u8::from(p.silent),
        p.stale_samples,
        u8::from(p.repay_ready),
        p.pending_drop_samples,
        p.late_sends,
        p.discontinuities,
        p.silence_samples,
        p.discarded_samples,
        p.resyncs,
        p.late_max_ns,
        p.target_ms,
        p.target_samples
    )
}

fn rust_trace(scripts: &[Script]) -> Vec<String> {
    let mut out = Vec::new();
    for &ms in &CLAMP_INPUTS {
        out.push(format!("clamp {}", clamp_target_ms(ms)));
    }
    for &(s, r) in &NS_INPUTS {
        out.push(format!("ns {}", samples_to_ns(s, r)));
    }
    for &(ns, r) in &NS2S_INPUTS {
        out.push(format!("ns2s {}", ns_to_samples(ns, r)));
    }
    for &(now, wake) in &WAIT_INPUTS {
        out.push(format!("wait {}", wait_ms(now, wake)));
    }
    for sc in scripts {
        let mut p = Pacing::new(sc.target_ms, sc.packet_samples, sc.rate);
        out.push(format!(
            "init {} {} {} {} {} {} {} {} {}",
            p.target_ms,
            p.target_ns,
            p.target_samples,
            p.grace_ns,
            p.ceiling_samples,
            p.ceiling_ns,
            p.half_packet_ns,
            p.packet_samples,
            p.rate
        ));
        for (i, wk) in sc.wakes.iter().enumerate() {
            if let Some(t) = wk.retarget {
                p.retarget(t);
            }
            let s = p.step(wk.now, wk.buffered);
            out.push(line(&p, s));
            if i % 7 == 6 {
                out.push(format!("take {}", p.take_late_max_ns()));
            }
        }
    }
    out
}

fn c_driver(scripts: &[Script]) -> String {
    let header = repo(HEADER);
    let mut c = String::new();
    writeln!(c, "#include <stdio.h>\n#include <inttypes.h>").unwrap();
    writeln!(c, "#include \"{}\"", header.display()).unwrap();
    writeln!(
        c,
        "static void line(const struct vban_pacing *p, struct vban_pacing_step s)\n{{\n\
         \tprintf(\"%\" PRIu32 \" %\" PRIu32 \" %\" PRIu64 \" %\" PRIu64 \" %d %d %d %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %d %d %\" PRIu64 \" %d %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu32 \" %\" PRIu64 \"\\n\",\n\
         \t       s.send, s.silence, s.drop_samples, s.wake_ns, s.wait_audio ? 1 : 0, p->running ? 1 : 0,\n\
         \t       p->primed ? 1 : 0, p->prime_ns, p->t0_ns, p->n_sent, p->catchup_ns, p->starved ? 1 : 0,\n\
         \t       p->silent ? 1 : 0, p->stale_samples, p->repay_ready ? 1 : 0, p->pending_drop_samples,\n\
         \t       p->late_sends, p->discontinuities, p->silence_samples, p->discarded_samples, p->resyncs,\n\
         \t       p->late_max_ns, p->target_ms, p->target_samples);\n}}"
    )
    .unwrap();
    writeln!(
        c,
        "struct wake {{ uint64_t now; uint64_t buffered; int has_retarget; int64_t retarget; }};"
    )
    .unwrap();
    for (i, sc) in scripts.iter().enumerate() {
        writeln!(c, "static const struct wake wakes_{i}[] = {{").unwrap();
        for wk in &sc.wakes {
            let (has, rt) = wk.retarget.map_or((0, 0), |t| (1, t));
            writeln!(c, "\t{{{}ULL, {}ULL, {has}, {rt}LL}},", wk.now, wk.buffered).unwrap();
        }
        writeln!(c, "}};").unwrap();
    }
    writeln!(c, "int main(void)\n{{").unwrap();
    writeln!(c, "\tstruct vban_pacing p;\n\tstruct vban_pacing_step s;").unwrap();
    for ms in CLAMP_INPUTS {
        writeln!(
            c,
            "\tprintf(\"clamp %\" PRIu32 \"\\n\", vban_pacing_clamp_target_ms({ms}LL));"
        )
        .unwrap();
    }
    for (s, r) in NS_INPUTS {
        writeln!(
            c,
            "\tprintf(\"ns %\" PRIu64 \"\\n\", vban_pacing_samples_to_ns({s}ULL, {r}U));"
        )
        .unwrap();
    }
    for (ns, r) in NS2S_INPUTS {
        writeln!(
            c,
            "\tprintf(\"ns2s %\" PRIu64 \"\\n\", vban_pacing_ns_to_samples({ns}ULL, {r}U));"
        )
        .unwrap();
    }
    for (now, wake) in WAIT_INPUTS {
        writeln!(
            c,
            "\tprintf(\"wait %\" PRIu32 \"\\n\", vban_pacing_wait_ms({now}ULL, {wake}ULL));"
        )
        .unwrap();
    }
    for (i, sc) in scripts.iter().enumerate() {
        writeln!(
            c,
            "\tvban_pacing_init(&p, {}LL, {}U, {}U);",
            sc.target_ms, sc.packet_samples, sc.rate
        )
        .unwrap();
        writeln!(
            c,
            "\tprintf(\"init %\" PRIu32 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu32 \" %\" PRIu32 \"\\n\", p.target_ms, p.target_ns, p.target_samples, p.grace_ns, p.ceiling_samples, p.ceiling_ns, p.half_packet_ns, p.packet_samples, p.rate);"
        )
        .unwrap();
        writeln!(
            c,
            "\tfor (size_t i = 0; i < sizeof(wakes_{i}) / sizeof(wakes_{i}[0]); i++) {{\n\
             \t\tif (wakes_{i}[i].has_retarget)\n\
             \t\t\tvban_pacing_retarget(&p, wakes_{i}[i].retarget);\n\
             \t\ts = vban_pacing_step(&p, wakes_{i}[i].now, wakes_{i}[i].buffered);\n\
             \t\tline(&p, s);\n\
             \t\tif (i % 7 == 6)\n\
             \t\t\tprintf(\"take %\" PRIu64 \"\\n\", vban_pacing_take_late_max_ns(&p));\n\
             \t}}"
        )
        .unwrap();
    }
    writeln!(c, "\treturn 0;\n}}").unwrap();
    c
}

fn c_trace(scripts: &[Script]) -> Vec<String> {
    let header = repo(HEADER);
    assert!(
        header.is_file(),
        "issue 1372: {HEADER} is missing — the obs-vban send pacing is gone and VBAN leaves in \
         bursts again (one packet per wake)"
    );
    let dir = std::env::temp_dir().join(format!("vban_pacing_parity_1372_{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let driver = dir.join("driver.c");
    let bin = dir.join("driver.bin");
    fs::write(&driver, c_driver(scripts)).expect("write the parity driver");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wsign-conversion",
            "-Wformat=2",
            "-Werror",
            "-O1",
        ])
        .arg(&driver)
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
         -Wextra -Wconversion -Wsign-conversion -Wformat=2 -Werror:\n--- cc stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("issue 1372: the compiled parity driver failed to execute");
    let _ = fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "issue 1372: the parity driver exited non-zero"
    );
    String::from_utf8(run.stdout)
        .expect("driver stdout is utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn c_vban_pacing_matches_the_rust_authority_1372() {
    let scripts = scripts();
    let c = c_trace(&scripts);
    let r = rust_trace(&scripts);
    assert_eq!(
        c.len(),
        r.len(),
        "issue 1372: C and Rust traces differ in length"
    );
    let diffs: Vec<String> = c
        .iter()
        .zip(&r)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .take(10)
        .map(|(i, (a, b))| format!("line {i}: C `{a}` != Rust `{b}`"))
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1372: the vendored obs-vban pacing diverges from src/vban_pacing.rs:\n{}",
        diffs.join("\n")
    );
}

#[test]
fn the_scripts_reach_every_boundary_1381() {
    // A parity gate is only as good as the states it visits (the libobs tie-break lesson).
    let scripts = scripts();
    let names = [
        "exact-deadline wakes",
        "multi-packet wakes",
        "waits for late audio",
        "late sends",
        "capped catch-up wakes",
        "silence episodes",
        "resumes from silence",
        "stale repays",
        "buffer-ceiling resyncs",
        "schedule-ceiling resyncs",
        "at-ceiling depths",
        "running retargets up",
        "running retargets down",
    ];
    let mut hits = [0usize; 13];
    for sc in &scripts {
        let mut p = Pacing::new(sc.target_ms, sc.packet_samples, sc.rate);
        let mut last = Step::default();
        for wk in &sc.wakes {
            if last.wake_ns != 0 && wk.now == last.wake_ns && !last.wait_audio && p.running {
                hits[0] += 1;
            }
            if wk.buffered == p.ceiling_samples {
                hits[10] += 1;
            }
            if let Some(t) = wk.retarget {
                let before = (p.target_ms, p.running);
                p.retarget(t);
                hits[11] += usize::from(before.1 && p.target_ms > before.0);
                hits[12] += usize::from(before.1 && p.target_ms < before.0);
            }
            let (late, disc, silent, stale, resyncs) = (
                p.late_sends,
                p.discontinuities,
                p.silent,
                p.stale_samples,
                p.resyncs,
            );
            let s = p.step(wk.now, wk.buffered);
            hits[1] += usize::from(s.send + s.silence >= 2);
            hits[2] += usize::from(s.wait_audio && s.wake_ns != 0);
            hits[3] += usize::from(p.late_sends > late);
            hits[4] += usize::from(p.catchup_ns != 0 && s.send > 0);
            hits[5] +=
                usize::from(s.silence > 0 && p.discontinuities > disc && p.resyncs == resyncs);
            hits[6] += usize::from(silent && !p.silent && s.send > 0);
            hits[7] += usize::from(stale > 0 && p.stale_samples < stale && p.resyncs == resyncs);
            if p.resyncs > resyncs {
                if wk.buffered > p.ceiling_samples {
                    hits[8] += 1;
                } else {
                    hits[9] += 1;
                }
            }
            last = s;
        }
    }
    for (name, n) in names.iter().zip(hits) {
        assert!(n >= 5, "issue 1381: the parity scripts hit only {n} {name}");
    }
}

#[test]
fn the_paced_obs_vban_is_built_staged_and_wired_1372() {
    let read = |rel: &str| {
        fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("issue 1372: read {rel}: {e}"))
    };
    let full = read(".github/workflows/windows-genlock.yml");
    let build = full
        .find("- name: Build obs-vban (issue 1372)")
        .expect("issue 1372: windows-genlock.yml no longer builds the vendored obs-vban");
    let stage = full
        .find("- name: Stage artifact")
        .expect("windows-genlock.yml lost its Stage artifact step");
    assert!(
        build < stage,
        "issue 1372: obs-vban must be built before the bundle is staged"
    );
    for needle in [
        "working-directory: vendor/obs-vban",
        "-Filter obs-vban.dll",
        "Copy-Item $vbandll.FullName \"stage/obs-plugins/64bit/\"",
        "Copy-Item -Recurse \"vendor/obs-vban/data/*\" \"stage/data/obs-plugins/obs-vban/\"",
        "- name: Assert obs-vban send pacing present (issue 1372)",
    ] {
        assert!(
            full.contains(needle),
            "issue 1372: windows-genlock.yml lost `{needle}`"
        );
    }
    let fast = read(".github/workflows/windows-genlock-fast.yml");
    for needle in [
        "- 'vendor/obs-vban/**'",
        "- name: Compile-check obs-vban (build the plugin, issue 1372)",
        "- name: Assert obs-vban send pacing present (issue 1372)",
    ] {
        assert!(
            fast.contains(needle),
            "issue 1372: windows-genlock-fast.yml lost `{needle}`"
        );
    }
    // Both pwsh assert steps look for the CURRENT status line (issue 1381), not the old one.
    for (name, yml) in [
        ("windows-genlock.yml", &full),
        ("windows-genlock-fast.yml", &fast),
    ] {
        assert!(
            yml.contains("[regex]::Escape('\"obs-vban pacing: depth_ms=%.1f late_sends=%\"')"),
            "issue 1381: {name}'s pacing assert does not look for the fixed-timeline status line"
        );
        assert!(
            !yml.contains("underflows=%"),
            "issue 1381: {name} still asserts the retired underflow status line"
        );
    }

    // The send thread asks the pure decision, sleeps to the deadline or waits on the audio event,
    // sends silence packets, advances the frame counter across every dropped packet, and its
    // status line keeps a marker no other obs-vban line contains.
    let thread = read("vendor/obs-vban/src/vban-output-thread.c");
    for needle in [
        "#include \"vban-pacing.h\"",
        "vban_pacing_step(&pacing, now,",
        "pacing_sleep_until(&sleeper, wake_ns);",
        "os_sleepto_ns(deadline_ns);",
        "os_event_timedwait(v->event, vban_pacing_wait_ms(os_gettime_ns(), wake_ns));",
        "memset(t->payload, 0, n);",
        "send_silence(&t, vban_buf, nbs, sample_size, &addr);",
        "t.header->nuFrame += (uint32_t)(d.drop_samples / nbs);",
        "\"obs-vban pacing: depth_ms=%.1f late_sends=%\"",
        " silence_ms=%.1f discarded_ms=%.1f resyncs=%\"",
        " dest=%u.%u.%u.%u:%u stream='%.*s'\"",
    ] {
        assert!(
            thread.contains(needle),
            "issue 1381: vban-output-thread.c lost `{needle}`"
        );
    }
    // The retired overflow drop, trim and their counters are gone for good.
    let header = read(HEADER);
    for gone in [
        "OVERFLOW_HEADROOM",
        "TRIM_WINDOW",
        "trims",
        "underflows",
        "overflows",
    ] {
        assert!(
            !header.contains(gone) && !thread.contains(gone),
            "issue 1381: `{gone}` is back in the obs-vban pacing"
        );
    }
    let mut marker_lines = 0;
    for f in fs::read_dir(repo("vendor/obs-vban/src")).unwrap() {
        let text = fs::read_to_string(f.unwrap().path()).unwrap();
        marker_lines += text.matches("obs-vban pacing:").count();
    }
    assert_eq!(
        marker_lines, 1,
        "issue 1372: `obs-vban pacing:` must appear on exactly one log line (the 10 s status)"
    );
    let output = read("vendor/obs-vban/src/vban-output.c");
    assert!(
        output.contains(
            "obs_data_set_default_int(data, \"pacing_target_ms\", VBAN_PACING_TARGET_MS_DEFAULT);"
        ),
        "issue 1372: the output settings lost the pacing_target_ms default"
    );
}
