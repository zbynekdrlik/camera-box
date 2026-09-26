//! Issue 1372 — an EXECUTABLE C-vs-Rust parity gate for the render tick's wall-step detector and
//! its one-tick re-grid.
//!
//! `vendor/obs-studio/libobs/obs-genlock-wall-step.h` is pure `<stdint.h>`, so it is `#include`d
//! as-is (never a retyped copy), compiled under `-Wall -Wextra -Wconversion -Wformat=2 -Werror`,
//! and driven through the same sequences as the Tier-0 Rust authority
//! `camera_box::genlock_wall_step`: the bracket offset read, the detector over whole read sequences
//! (seed, trusted / preempted reads, the logged −51 ms date step, the exact threshold, a slow
//! drift, the i64 extremes) and the deadline over the clamp edges with and without a re-grid. Every
//! printed value must be byte-identical. `cc` is required — per the project's test-strictness rule
//! this FAILS LOUDLY rather than skipping when the toolchain is missing.

use camera_box::genlock_wall_step::{
    deadline_ns, wall_offset_ns, WallStepState, MAX_SLEW_NS, READ_MAX_NS, WALL_STEP_MIN_NS,
};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const HEADER: &str = "vendor/obs-studio/libobs/obs-genlock-wall-step.h";
const OBS_VIDEO: &str = "vendor/obs-studio/libobs/obs-video.c";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

const WALL0: u64 = 1_790_378_227_000_000_000; // the logged step (epoch ns)
const MONO0: u64 = 123_456_789_000_000;

/// `(mono_before, wall, mono_after)` read sequences, each fed to ONE fresh detector.
fn sequences() -> Vec<Vec<(u64, u64, u64)>> {
    let i = 33_333_333_u64;
    let mut out = Vec::new();
    // the logged −51.039 ms date step, 30 fps ticks, 1–3 µs brackets
    let mut s = Vec::new();
    for k in 0..40_u64 {
        let mono = MONO0 + k * i;
        let mut wall = WALL0 + k * i;
        if k >= 20 {
            wall -= 51_039_000;
        }
        s.push((mono, wall + 1_000, mono + 1_000 + (k % 3) * 1_000));
    }
    out.push(s);
    // a positive step, then a step back, the exact threshold and one ns over it
    out.push(vec![
        (MONO0, WALL0, MONO0 + 100),
        (MONO0 + i, WALL0 + i + 51_000_000, MONO0 + i + 100),
        (MONO0 + 2 * i, WALL0 + 2 * i, MONO0 + 2 * i + 100),
        (
            MONO0 + 3 * i,
            WALL0 + 3 * i + WALL_STEP_MIN_NS as u64,
            MONO0 + 3 * i + 100,
        ),
        (
            MONO0 + 4 * i,
            WALL0 + 4 * i + 2 * WALL_STEP_MIN_NS as u64 + 1,
            MONO0 + 4 * i + 100,
        ),
    ]);
    // preempted / backward brackets between good reads
    out.push(vec![
        (MONO0, WALL0, MONO0 + 100),
        (
            MONO0 + i,
            WALL0 + i + 9_000_000,
            MONO0 + i + READ_MAX_NS + 1,
        ),
        (MONO0 + 2 * i, WALL0 + 2 * i, MONO0 + 2 * i + READ_MAX_NS),
        (MONO0 + 3 * i + 10, WALL0 + 3 * i, MONO0 + 3 * i),
        (MONO0 + 4 * i, WALL0 + 4 * i - 3_000_000, MONO0 + 4 * i + 7),
    ]);
    // a 1000 ppm drift over 3000 ticks: never a step
    out.push(
        (0..3000_u64)
            .map(|k| {
                let mono = MONO0 + k * i;
                (mono, WALL0 + k * (i + 33_333), mono + 500)
            })
            .collect(),
    );
    // wrap-around extremes of the offset (the i64 cast and the saturating magnitude)
    out.push(vec![
        (0, 0, 0),
        (0, u64::MAX / 2 + 1, 0),
        (0, 0, 0),
        (5, u64::MAX, 5),
    ]);
    out
}

/// `(step_ns, target − stock)` per tick, each sequence on ONE fresh state: the re-grid decision
/// with a missed re-grid (pending), a landing, a step inside the clamp, and the i64 extremes.
fn regrid_sequences() -> Vec<Vec<(i64, i64)>> {
    let m = MAX_SLEW_NS;
    vec![
        vec![
            (0, 100_000),
            (-51_039_000, 15_700_000),
            (0, 15_700_000),
            (0, 50_000),
            (0, 15_700_000),
        ],
        vec![
            (51_000_000, -17_600_000),
            (0, -17_600_000),
            (0, -17_600_000),
            (0, 0),
        ],
        vec![(3_000_000, 1_000_000), (0, 5_000_000)],
        vec![
            (-3_000_000, m),
            (0, m + 1),
            (-3_000_000, m + 1),
            (0, -m - 1),
            (0, -m),
        ],
        vec![(i64::MIN, i64::MAX), (0, i64::MIN), (0, 0)],
    ]
}

/// `(target, stock)` pairs around the ±2 ms clamp edges and far beyond it.
fn deadline_vectors() -> Vec<(u64, u64)> {
    let stock = MONO0;
    let m = MAX_SLEW_NS as u64;
    [
        0,
        1,
        m - 1,
        m,
        m + 1,
        15_700_000,
        33_333_333,
        3_600_000_000_000,
    ]
    .iter()
    .flat_map(|&d| [(stock + d, stock), (stock - d, stock)])
    .collect()
}

fn rust_trace() -> Vec<String> {
    let mut out = Vec::new();
    for (n, seq) in sequences().iter().enumerate() {
        let mut s = WallStepState::new();
        for &(b, w, a) in seq {
            let off = wall_offset_ns(b, w, a).map_or("none".to_string(), |o| o.to_string());
            let step = s.observe(b, w, a);
            out.push(format!("seq {n} off {off} step {step} steps {}", s.steps()));
        }
    }
    for (t, st) in deadline_vectors() {
        out.push(format!(
            "deadline {t} {st} {} {}",
            deadline_ns(t, st, false),
            deadline_ns(t, st, true)
        ));
    }
    for (n, seq) in regrid_sequences().iter().enumerate() {
        let mut s = WallStepState::new();
        for &(step, d) in seq {
            let stock = MONO0;
            let target = stock.wrapping_add(d as u64);
            let r = s.regrid_due(step, target, stock);
            out.push(format!(
                "regrid {n} {step} {d} {} {}",
                u8::from(r),
                u8::from(s.regrid_pending())
            ));
        }
    }
    out
}

/// An i64 as a C expression (`INT64_MIN` has no literal form).
fn c_i64(v: i64) -> String {
    if v == i64::MIN {
        "INT64_MIN".to_string()
    } else {
        format!("{v}LL")
    }
}

fn c_harness() -> String {
    let mut body = String::new();
    for (n, seq) in sequences().iter().enumerate() {
        body.push_str("\t{\n\t\tstruct genlock_wall_step_state s = {0, 0, 0, 0};\n");
        for &(b, w, a) in seq {
            body.push_str(&format!(
                "\t\tobserve_line({n}, &s, {b}ULL, {w}ULL, {a}ULL);\n"
            ));
        }
        body.push_str("\t}\n");
    }
    for (t, st) in deadline_vectors() {
        body.push_str(&format!("\tdeadline_line({t}ULL, {st}ULL);\n"));
    }
    for (n, seq) in regrid_sequences().iter().enumerate() {
        body.push_str("\t{\n\t\tstruct genlock_wall_step_state s = {0, 0, 0, 0};\n");
        for &(step, d) in seq {
            body.push_str(&format!(
                "\t\tregrid_line({n}, &s, {}, {});\n",
                c_i64(step),
                c_i64(d)
            ));
        }
        body.push_str("\t}\n");
    }
    format!(
        r#"#include <stdio.h>
#include <inttypes.h>
#include "obs-genlock-wall-step.h"

static void observe_line(int n, struct genlock_wall_step_state *s, uint64_t b, uint64_t w, uint64_t a)
{{
	int64_t off = 0;
	const int ok = genlock_wall_offset_ns(b, w, a, &off);
	const int64_t step = genlock_wall_step_observe(s, b, w, a);
	if (ok)
		printf("seq %d off %" PRId64 " step %" PRId64 " steps %" PRIu64 "\n", n, off, step, s->steps);
	else
		printf("seq %d off none step %" PRId64 " steps %" PRIu64 "\n", n, step, s->steps);
}}

static void regrid_line(int n, struct genlock_wall_step_state *s, int64_t step, int64_t d)
{{
	const uint64_t stock = {mono0}ULL;
	const uint64_t target = stock + (uint64_t)d;
	const int r = genlock_wall_step_regrid_due(s, step, target, stock);
	printf("regrid %d %" PRId64 " %" PRId64 " %d %d\n", n, step, d, r ? 1 : 0, s->regrid_pending ? 1 : 0);
}}

static void deadline_line(uint64_t t, uint64_t st)
{{
	printf("deadline %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 "\n", t, st,
	       genlock_wall_step_deadline_ns(t, st, 0), genlock_wall_step_deadline_ns(t, st, 1));
}}

int main(void)
{{
{body}	return 0;
}}
"#,
        mono0 = MONO0
    )
}

fn c_trace() -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("genlock_wall_step_parity_1372");
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
fn c_wall_step_matches_the_rust_authority_1372() {
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
            "issue 1372: {HEADER} diverges from src/genlock_wall_step.rs at line {i}"
        );
    }
    // The sequences must reach the paths they claim to cover.
    let has = |needle: &str| r.iter().any(|l| l.contains(needle));
    let logged_step = r.iter().any(|l| {
        l.split(" step ")
            .nth(1)
            .and_then(|t| t.split(' ').next())
            .and_then(|v| v.parse::<i64>().ok())
            .is_some_and(|v| (v + 51_039_000).abs() <= 2_000)
    });
    assert!(
        logged_step
            && has("off none")
            && r.iter().filter(|l| l.contains(" step 0 ")).count() > 3000
            && r.iter().any(|l| l.starts_with("regrid 0 0 15700000 1 1")),
        "issue 1372: the parity sequences no longer exercise the step, an untrusted read and the \
         long no-step drift"
    );
}

/// The render tick calls the header — a bracketed read into the detector, and the deadline through
/// the re-grid decision — and keeps its own 2 ms clamp define equal to the header's.
/// The render-tick wiring both the Rust guard below and the pwsh guard in both `windows-genlock*.yml`
/// workflows require in `obs-video.c` (squished). ONE list, so the two copies cannot drift apart —
/// review round 2: the pwsh copy still required the round-0 `return` line and would have failed the
/// Windows build.
const RENDER_TICK_WIRING: [&str; 5] = [
    "#include \"obs-genlock-wall-step.h\"",
    "const int64_t wall_step_ns = genlock_wall_step_observe(&wall_step, mono_before, wall, mono);",
    "const int regrid = genlock_wall_step_regrid_due(&wall_step, wall_step_ns, target, stock);",
    "const uint64_t deadline = genlock_wall_step_deadline_ns(target, stock, regrid);",
    "#define GENLOCK_MAX_SLEW_NS ((int)GENLOCK_WALL_STEP_MAX_SLEW_NS)",
];

const WINDOWS_WORKFLOWS: [&str; 2] = [
    ".github/workflows/windows-genlock.yml",
    ".github/workflows/windows-genlock-fast.yml",
];

#[test]
fn render_tick_uses_the_wall_step_regrid_1372() {
    let src = fs::read_to_string(repo(OBS_VIDEO)).expect("read obs-video.c");
    let squished: String = src.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in RENDER_TICK_WIRING.into_iter().chain([
        "const uint64_t mono_before = os_gettime_ns(); const uint64_t wall = genlock_wall_ns(); const uint64_t mono = os_gettime_ns();",
        "genlock-regrid: the wall clock stepped",
    ]) {
        assert!(
            squished.contains(needle),
            "issue 1372: {OBS_VIDEO} lost `{needle}` — the render tick no longer re-grids a wall \
             step in one tick (it would slew 2 ms per tick with the sender stamps off phase)"
        );
    }
    let header = fs::read_to_string(repo(HEADER)).expect("read the header");
    assert!(
        header.contains("#define GENLOCK_WALL_STEP_MAX_SLEW_NS 2000000LL"),
        "issue 1372: the header's slew clamp is no longer the render tick's 2 ms"
    );
    assert_eq!(MAX_SLEW_NS, 2_000_000);
}

#[test]
fn windows_workflows_guard_the_same_render_tick_wiring_1372() {
    for wf in WINDOWS_WORKFLOWS {
        let text = fs::read_to_string(repo(wf)).unwrap_or_else(|e| panic!("read {wf}: {e}"));
        for needle in RENDER_TICK_WIRING {
            assert!(
                text.contains(&format!("$vid1355 -notmatch [regex]::Escape('{needle}')")),
                "issue 1372: {wf} no longer guards `{needle}` in obs-video.c — its pwsh copy of \
                 render_tick_uses_the_wall_step_regrid_1372 drifted from the Rust guard"
            );
        }
        assert!(
            !text.contains("wall_step_ns != 0);"),
            "issue 1372: {wf} still requires the round-0 re-grid line, which obs-video.c no longer \
             has — the Windows build would fail at the source guard"
        );
    }
}
