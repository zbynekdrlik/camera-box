//! Issue 1372 — EXECUTABLE C-vs-Rust parity gate for the obs-vban send pacing.
//!
//! The patched obs-vban send thread (`vendor/obs-vban/src/vban-output-thread.c`) calls the pure
//! decision in `vendor/obs-vban/src/vban-pacing.h` on every wake. That plugin only compiles on the
//! Windows CI runner, so this gate buys the verification back on Linux:
//!
//! 1. It `#include`s the SHIPPED header by its absolute path (nothing is retyped) into a driver
//!    and compiles it with `cc -Wall -Wextra -Wconversion -Wsign-conversion -Wformat=2 -Werror`.
//! 2. It drives scripted wakes (`now`, `buffered`) that land on every boundary: a packet one
//!    sample short, exactly one packet, the anchor instant and one ns before it, a deadline and
//!    one ns before it, several due packets in one wake, an underflow part way through a burst,
//!    exactly the overflow limit and one sample over it, and other rates, packet sizes and targets
//!    (clamped ones included).
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
use vban_pacing::{clamp_target_ms, samples_to_ns, Pacing, Step, TRIM_WINDOW_NS};

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

fn w(now: u64, buffered: u64) -> Wake {
    Wake {
        now,
        buffered,
        retarget: None,
    }
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
/// own deadlines and window boundaries.
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
        let candidate = match (wake != 0, rng.below(8)) {
            (true, 0) | (true, 1) => wake,
            (true, 2) => wake.saturating_sub(1),
            (true, 3) => wake + rng.below(3_000_000),
            (true, 4) => wake + rng.below(40_000_000),
            _ => now + rng.below(12_000_000),
        };
        now = now.max(candidate);
        let prev = bld.buffered;
        let buffered = match rng.below(14) {
            0 => ps.saturating_sub(1),
            1 => ps,
            2 => bld.p.overflow_samples,
            3 => bld.p.overflow_samples + 1,
            4 => 0,
            5 => prev + 3 * block,
            6 => prev.saturating_sub(rng.below(ps)),
            7 | 8 => bld.p.trim_threshold_samples + ps + rng.below(2),
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
    let p = Pacing::new(64, 239, 48_000);
    let d = |n: u64| 1_064_000_000 + samples_to_ns(n * 239, 48_000);
    let wakes = vec![
        w(1_000_000_000, 0),
        w(1_000_000_000, 238),
        w(1_000_000_000, 239), // primed: anchor at 1.064 s
        w(1_064_000_000 - 1, 4096),
        w(1_064_000_000, 4096), // packet 0
        w(d(1) - 1, 4096 - 239),
        w(d(1), 4096 - 239), // packet 1
        w(d(4), 4096 - 478), // packets 2, 3, 4 in one wake
        w(d(5) - 1, p.overflow_samples),
        w(d(5) - 1, p.overflow_samples + 1), // overflow: drop to the target
        w(d(9), 239 * 2 + 5),                // packets 5, 6, then an underflow
        w(d(9) + 1, 5),
        w(d(9) + 2, 239), // re-primed
        w(d(9) + 2 + 64_000_000, 239),
    ];
    Script {
        target_ms: 64,
        packet_samples: 239,
        rate: 48_000,
        wakes,
    }
}

/// The trim window closing one ns early and exactly on time, then target changes up, down, to
/// the same value and to a value that clamps (64 ms, 239-sample packets, 48 kHz).
fn trim_and_retarget_script() -> Script {
    let mut bld = Builder::new(64, 239, 48_000);
    let full = 12_000;
    bld.wake(1_000_000_000, full, None);
    bld.wake(1_064_000_000, full, None);
    let t0 = bld.p.t0_ns;
    let mut next = bld.p.deadline_ns(bld.p.n_sent);
    while next < t0 + TRIM_WINDOW_NS - 1 {
        next = bld.wake(next, full, None).wake_ns;
    }
    bld.wake(t0 + TRIM_WINDOW_NS - 1, full, None);
    bld.wake(t0 + TRIM_WINDOW_NS, full, None); // the window closes: trimmed to the target
    let mut next = bld.p.deadline_ns(bld.p.n_sent);
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
        next = bld.wake(next, 6_000, rt).wake_ns;
    }
    // A whole window whose minimum sits exactly ON the threshold: never trimmed.
    let on = bld.p.trim_threshold_samples + u64::from(bld.p.packet_samples);
    let until = next + TRIM_WINDOW_NS + 100_000_000;
    while next < until {
        next = bld.wake(next, on, None).wake_ns;
    }
    // A window closing on a wake that sees LESS than the window minimum: the trim stops at the
    // target (review round 2), and a wake with less than a packet above the target drops nothing
    // and counts no trim.
    for low in [5_000, 3_100] {
        loop {
            let p = &bld.p;
            let closes_high = p.win_open
                && next >= p.win_start_ns + TRIM_WINDOW_NS
                && p.win_min_samples > p.trim_threshold_samples;
            if closes_high {
                break;
            }
            next = bld.wake(next, 12_000, None).wake_ns;
        }
        next = bld.wake(next, low, None).wake_ns;
    }
    // A large retarget down during a dip: dropped only down to the new target.
    next = bld.wake(next, 9_000, Some(200)).wake_ns;
    next = bld.wake(next, 12_000, None).wake_ns;
    next = bld.wake(next, 3_000, Some(20)).wake_ns;
    bld.wake(next, 3_000, None);
    bld.script(64, 239, 48_000)
}

fn scripts() -> Vec<Script> {
    let mut v = vec![boundary_script(), trim_and_retarget_script()];
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

fn line(p: &Pacing, send: u32, drop: u64, wake: u64) -> String {
    format!(
        "{send} {drop} {wake} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
        u8::from(p.running),
        u8::from(p.primed),
        p.prime_ns,
        p.t0_ns,
        p.n_sent,
        p.underflows,
        p.overflows,
        p.late_max_ns,
        p.trims,
        p.target_ms,
        p.target_samples,
        u8::from(p.win_open),
        p.win_start_ns,
        p.win_min_samples,
        p.pending_trim_samples
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
    for sc in scripts {
        let mut p = Pacing::new(sc.target_ms, sc.packet_samples, sc.rate);
        out.push(format!(
            "init {} {} {} {} {} {} {}",
            p.target_ms,
            p.target_ns,
            p.target_samples,
            p.overflow_samples,
            p.trim_threshold_samples,
            p.packet_samples,
            p.rate
        ));
        for (i, wk) in sc.wakes.iter().enumerate() {
            if let Some(t) = wk.retarget {
                p.retarget(t);
            }
            let s = p.step(wk.now, wk.buffered);
            out.push(line(&p, s.send, s.drop_samples, s.wake_ns));
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
         \tprintf(\"%\" PRIu32 \" %\" PRIu64 \" %\" PRIu64 \" %d %d %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu32 \" %\" PRIu64 \" %d %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \"\\n\",\n\
         \t       s.send, s.drop_samples, s.wake_ns, p->running ? 1 : 0, p->primed ? 1 : 0, p->prime_ns, p->t0_ns,\n\
         \t       p->n_sent, p->underflows, p->overflows, p->late_max_ns, p->trims, p->target_ms, p->target_samples,\n\
         \t       p->win_open ? 1 : 0, p->win_start_ns, p->win_min_samples, p->pending_trim_samples);\n}}"
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
    for (i, sc) in scripts.iter().enumerate() {
        writeln!(
            c,
            "\tvban_pacing_init(&p, {}LL, {}U, {}U);",
            sc.target_ms, sc.packet_samples, sc.rate
        )
        .unwrap();
        writeln!(
            c,
            "\tprintf(\"init %\" PRIu32 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu64 \" %\" PRIu32 \" %\" PRIu32 \"\\n\", p.target_ms, p.target_ns, p.target_samples, p.overflow_samples, p.trim_threshold_samples, p.packet_samples, p.rate);"
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
fn the_scripts_reach_every_boundary_1372() {
    // A parity gate is only as good as the states it visits (the libobs tie-break lesson).
    let scripts = scripts();
    let mut hits = [0usize; 8];
    for sc in &scripts {
        let mut p = Pacing::new(sc.target_ms, sc.packet_samples, sc.rate);
        let mut wake = 0u64;
        for wk in &sc.wakes {
            if wake != 0 && wk.now == wake && p.running {
                hits[0] += 1;
            }
            if wk.buffered == p.overflow_samples {
                hits[1] += 1;
            }
            if let Some(t) = wk.retarget {
                let before = (p.target_ms, p.running);
                p.retarget(t);
                hits[2] += usize::from(before.1 && p.target_ms > before.0);
                hits[3] += usize::from(before.1 && p.target_ms < before.0);
            }
            let (u, o, tr) = (p.underflows, p.overflows, p.trims);
            let s = p.step(wk.now, wk.buffered);
            hits[4] += usize::from(s.send >= 2);
            hits[5] += usize::from(p.underflows > u);
            hits[6] += usize::from(p.overflows > o);
            hits[7] += usize::from(p.trims > tr);
            wake = s.wake_ns;
        }
    }
    let names = [
        "exact-deadline wakes",
        "at-limit depths",
        "running retargets up",
        "running retargets down",
        "multi-packet wakes",
        "underflows",
        "overflows",
        "trims",
    ];
    for (name, n) in names.iter().zip(hits) {
        assert!(n >= 5, "issue 1372: the parity scripts hit only {n} {name}");
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

    // The send thread asks the pure decision and sleeps to the deadline; its status line keeps a
    // marker no other obs-vban line contains.
    let thread = read("vendor/obs-vban/src/vban-output-thread.c");
    for needle in [
        "#include \"vban-pacing.h\"",
        "vban_pacing_step(&pacing, now,",
        "pacing_sleep_until(&sleeper, wake_ns);",
        "os_sleepto_ns(deadline_ns);",
        "\"obs-vban pacing: depth_ms=%.1f underflows=%\"",
    ] {
        assert!(
            thread.contains(needle),
            "issue 1372: vban-output-thread.c lost `{needle}`"
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
