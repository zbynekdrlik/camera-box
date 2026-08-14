//! #1003 — an EXECUTABLE C-vs-Rust parity gate for the phase-continuity relock selection.
//!
//! `src/genlock_backlog.rs` is the Tier-0 authority and `vendor/obs-studio/libobs/obs-source.c`
//! is the production port; the two are required to be numerically identical. Every other
//! guard in this repo asserts that by STATIC TEXT ANCHOR (see
//! `tests/genlock_release_cadence.rs`), which proves the C still *says* the right thing but
//! never that it *computes* the right thing — and the vendored C is compiled only by the
//! Windows/Linux genlock workflows, so nothing else executes it at all.
//!
//! This gate closes that hole cheaply: it lifts the four `#1003` helpers VERBATIM out of
//! obs-source.c, compiles them standalone against a minimal `obs_source_t` stub, runs the C
//! selector over a spread of vectors, and requires byte-identical indices from
//! [`camera_box::genlock_backlog::relock_select_nearest`] on the same inputs. A divergence
//! introduced on either side — a flipped comparison, a lost saturation guard, an off-by-one —
//! fails here in seconds instead of surviving to a live rig.
//!
//! It deliberately does NOT try to compile libobs: only the self-contained helpers, which need
//! nothing but `<stdint.h>`/`<stddef.h>` and the stub. `cc` is required (present on every
//! `ubuntu-latest` runner and on the self-hosted dev boxes); per the project's test-strictness
//! rule this FAILS LOUDLY rather than skipping if the toolchain is missing — a parity test
//! that silently passes when it never ran is worse than no test.

use camera_box::genlock_backlog::{
    relock_anchor_age_ns, relock_select_nearest, should_converge_phase,
};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";

/// The stub the lifted helpers need: only the fields they actually touch.
const PRELUDE: &str = r#"#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
struct obs_source_frame { uint64_t timestamp; };
typedef struct obs_source {
    struct { struct obs_source_frame **array; size_t num; } async_frames;
    uint64_t genlock_phase_anchor_ns;
} obs_source_t;
"#;

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Lift the `#1003` helper block verbatim from the vendored C.
fn lift_helpers() -> String {
    let path = repo(OBS_SOURCE);
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = src
        .find("static inline uint64_t genlock_abs_diff_ns(")
        .unwrap_or_else(|| {
            panic!(
                "#1003: {OBS_SOURCE} no longer defines genlock_abs_diff_ns — the phase-continuity \
             helpers are gone, so there is nothing to check parity against."
            )
        });
    let last = src
        .find("static inline uint64_t genlock_phase_anchor_from_present(")
        .unwrap_or_else(|| {
            panic!("#1003: {OBS_SOURCE} no longer defines genlock_phase_anchor_from_present")
        });
    let end = src[last..]
        .find("\n}\n")
        .map(|i| last + i + 3)
        .expect("#1003: genlock_phase_anchor_from_present has no closing brace");
    assert!(
        end > start,
        "#1003: the helper block in {OBS_SOURCE} is not contiguous — the lift would splice \
         unrelated code. Keep the four #1003 helpers adjacent."
    );
    src[start..end].to_string()
}

/// The vectors both sides must agree on: hand-picked edges, exact ties, and a deterministic
/// spread. Each is `(queue length, anchor_ns, ABSOLUTE wall_now_ns, stamp grid)`.
fn vectors() -> Vec<(usize, u64, u64, u64)> {
    // 33_333_300 = the sender's 100ns-truncated 30fps grid; 16_666_600 the 60fps one.
    let mut v: Vec<(usize, u64, u64, u64)> = vec![
        (1, 0, W0, 33_333_300),                   // degenerate single-frame queue
        (1, 923_000_000, W0 + 7, 33_333_300),     // single frame, anchor set
        (2, 933_342_267, W0 + 13, 33_333_300),    // the live steady anchor
        (28, 923_000_000, W0, 33_333_300),        // the live steady depth
        (40, 0, W0 + 5, 33_333_300),              // deep queue, anchor UNSET
        (40, 1, W0 + 5, 33_333_300),              // deep queue, anchor BELOW the hold (floored)
        (30, 2_000_000_000, W0 + 11, 33_333_300), // anchor far DEEPER than the queue spans
        (12, 933_000_000, W0 + 3, 16_666_600),    // a 60fps sender grid
        (64, 923_000_000, W0 + 29, 33_333_300),   // long queue
    ];
    // EXACT-TIE vectors — the only ones that can distinguish the "ties toward the OLDER
    // frame" contract (a strict `<`) from a `<=` that would silently prefer the newer one.
    // Without them this gate is BLIND to that flip: verified by mutating the C compare to
    // `<=`, against which an earlier revision of this test still passed on all 129 vectors.
    // A tie needs the target EXACTLY midway between two stamps, i.e.
    // `wall - age == BASE + i*grid + grid/2` — solvable because both rig grids are even.
    for (i, grid, anchor) in [
        (7usize, 33_333_300u64, 923_000_000u64),
        (3, 33_333_300, 1_100_000_000),
        (11, 16_666_600, 923_000_000),
        (0, 33_333_300, 1_000_000_000),
    ] {
        let age = anchor.max(LATENCY_MS as u64 * 1_000_000);
        // target == BASE + i*grid + grid/2 == exactly midway between stamp i and stamp i+1.
        v.push((i + 4, anchor, BASE + i as u64 * grid + grid / 2 + age, grid));
    }
    // A deterministic LCG spread, so the gate covers more than the cases someone thought of.
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    for _ in 0..120 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let n = (x >> 33) as usize % 48 + 1;
        let anchor = if (x >> 17) & 3 == 0 {
            0
        } else {
            (x >> 20) % 1_500_000_000
        };
        let wall = W0 + (x >> 7) % 40_000_000;
        let grid = if x & 1 == 0 { 33_333_300 } else { 16_666_600 };
        v.push((n, anchor, wall, grid));
    }
    v
}

const BASE: u64 = 10_000_000_000_000;
/// The reference wall instant the non-tie vectors sit around.
const W0: u64 = BASE + 9_999_999_900;
const LATENCY_MS: u32 = 923;

#[test]
fn c_relock_selection_matches_the_rust_authority_1003() {
    let helpers = lift_helpers();
    let vs = vectors();

    // --- build the C harness -------------------------------------------------------
    let mut c = String::new();
    c.push_str(PRELUDE);
    c.push_str(&helpers);
    c.push_str("int main(void){\n    struct obs_source_frame f[256];\n    struct obs_source_frame *pf[256];\n    obs_source_t s;\n");
    for (n, anchor, wall, grid) in &vs {
        c.push_str(&format!(
            "    {{ size_t n={n}; uint64_t g={grid}ULL, w={}ULL;\n\
             \x20     for (size_t i=0;i<n;i++) {{ f[i].timestamp = {BASE}ULL + (uint64_t)i*g; pf[i]=&f[i]; }}\n\
             \x20     s.async_frames.array=pf; s.async_frames.num=n; s.genlock_phase_anchor_ns={anchor}ULL;\n\
             \x20     printf(\"%zu\\n\", genlock_relock_select_nearest(&s, w, {LATENCY_MS}));\n    }}\n",
            wall
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("genlock_parity_1003");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let cfile = dir.join("parity.c");
    let bin = dir.join("parity.bin");
    fs::write(&cfile, &c).expect("write the parity harness");

    // --- compile (loudly, never skipped) --------------------------------------------
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args(["-std=gnu99", "-Wall", "-Wextra", "-Werror", "-O1"])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "#1003: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 vendored #1003 helpers to prove the C and the Rust authority agree \
                 numerically; it must FAIL rather than skip when the toolchain is absent (a \
                 parity test that silently passes without running is worse than none). \
                 Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1003: the vendored C helpers lifted from {OBS_SOURCE} do NOT COMPILE standalone \
         under -Wall -Wextra -Werror. The vendored tree is otherwise built only by the \
         genlock workflows, so this is very likely a real compile error heading for CI:\n\
         --- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );

    // --- run + compare ---------------------------------------------------------------
    let run = Command::new(&bin)
        .output()
        .expect("#1003: the compiled parity harness failed to execute");
    assert!(
        run.status.success(),
        "#1003: the parity harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let c_out: Vec<usize> = stdout
        .lines()
        .map(|l| {
            l.trim()
                .parse()
                .expect("harness printed a non-integer index")
        })
        .collect();
    assert_eq!(
        c_out.len(),
        vs.len(),
        "#1003: the harness printed {} indices for {} vectors",
        c_out.len(),
        vs.len()
    );

    let mut diffs = Vec::new();
    for (i, ((n, anchor, wall, grid), got_c)) in vs.iter().zip(&c_out).enumerate() {
        let q: Vec<u64> = (0..*n).map(|j| BASE + j as u64 * grid).collect();
        let got_rs = relock_select_nearest(&q, *wall, relock_anchor_age_ns(*anchor, LATENCY_MS));
        if got_rs != *got_c {
            diffs.push(format!(
                "  vector {i}: n={n} anchor={anchor} wall={wall} grid={grid} -> C {got_c}, Rust {got_rs}"
            ));
        }
        assert!(
            got_rs < *n,
            "#1003: the Rust selector returned an out-of-range index {got_rs} for a \
             {n}-frame queue"
        );
    }
    assert!(
        diffs.is_empty(),
        "#1003: the vendored C relock selection DIVERGED from the Tier-0 Rust authority on \
         {} of {} vectors. These two are required to be numerically identical — the Rust one \
         is unit-tested and the C one is what actually ships to the rig, so a divergence \
         means the deployed behaviour is not the behaviour any test covers:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
}

/// #1049 — the SAME executable-parity discipline for the phase-convergence decision. Lifts the
/// self-contained `genlock_phase_converge_due` helper VERBATIM from obs-source.c, compiles it
/// standalone under `-Werror`, and requires byte-identical booleans from
/// [`camera_box::genlock_backlog::should_converge_phase`] over a spread of vectors — a flipped
/// comparison, a lost saturation guard, or a wrong `interval/n` quantum fails here in seconds
/// rather than surviving to the rig.
fn lift_converge_helper() -> String {
    let path = repo(OBS_SOURCE);
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = src
        .find("static inline bool genlock_phase_converge_due(")
        .unwrap_or_else(|| {
            panic!(
                "#1049: {OBS_SOURCE} no longer defines genlock_phase_converge_due — the phase \
                 convergence helper is gone, so there is nothing to check parity against."
            )
        });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("#1049: genlock_phase_converge_due has no closing brace");
    src[start..end].to_string()
}

/// Lift a `#define NAME <value>` line VERBATIM from the vendored C so the parity harness compiles
/// against the SHIPPED constant, never a hard-coded copy that could silently drift (issue-1049
/// review finding 🟡2). Returns the whole `#define …` line.
fn lift_define(name: &str) -> String {
    let src = fs::read_to_string(repo(OBS_SOURCE)).expect("read obs-source.c");
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with(&format!("#define {name} ")) {
            return t.to_string();
        }
    }
    panic!("#1049: {OBS_SOURCE} no longer defines {name} — parity harness cannot lift it");
}

/// `(wall_now, boundary, newest_stamp, latency_ms, interval, n, ticks_since_drain)`. `newest_stamp`
/// is the freshest queued frame's capture stamp — its age `wall - newest` is the achievable floor.
fn converge_vectors() -> Vec<(u64, u64, u64, u32, u64, u32, u64)> {
    let i30 = 33_333_333u64;
    let i60 = 16_666_667u64;
    let w = 1_000_000_000_000u64;
    // Most vectors use newest == wall (floor 0 -> target = reserve); the floor-path vectors set a
    // large skew so `floor > reserve` and the target becomes the floor.
    let mut v: Vec<(u64, u64, u64, u32, u64, u32, u64)> = vec![
        (w, w - (20 * 1_000_000 + 2 * i30), w, 20, i30, 2, 100), // over threshold, throttle met
        (w, w - 20_000_000, w, 20, i30, 2, 100),                 // held AT configured -> inert
        (w, w - (20 * 1_000_000 + 2 * i30), w, 20, i30, 2, 29),  // throttle NOT met
        (w, w - (20 * 1_000_000 + 2 * i30), w, 20, i30, 2, 30),  // throttle exactly met
        (
            w,
            w - (20 * 1_000_000 + i30 / 2 + 5_000_000 + 1),
            w,
            20,
            i30,
            2,
            100,
        ), // n=2 quantum edge, over
        (
            w,
            w - (20 * 1_000_000 + i30 / 2 + 5_000_000),
            w,
            20,
            i30,
            2,
            100,
        ), // n=2 quantum edge, at (inert)
        (
            w,
            w - (20 * 1_000_000 + i30 / 2 + 5_000_000 + 1),
            w,
            20,
            i30,
            1,
            100,
        ), // same age, n=1 -> inert
        (
            w,
            w - (1000 * 1_000_000 + 8_000_000),
            w - 8_000_000,
            1000,
            i30,
            1,
            1_000_000,
        ), // deep hold -> inert
        (
            w,
            w - (1000 * 1_000_000 + 2 * i30),
            w - 8_000_000,
            1000,
            i30,
            1,
            100,
        ), // deep walked a frame -> fires
        (w, 0, w, 20, i30, 2, 100),                              // unlocked boundary -> false
        (w, w - 500_000_000, w, 20, 0, 2, 100),                  // degenerate interval -> false
        (w, w + i30, w, 20, i30, 2, 100),                        // boundary ahead of wall -> false
        (w, w - (20 * 1_000_000 + 2 * i60), w, 20, i60, 2, 100), // a 60fps canvas source grid
        (w, w - (20 * 1_000_000 + 3 * i30), w, 20, i30, 0, 100), // source_multiple 0 floors to 1
        // FLOOR-PATH vectors (#1049 review): a large skew floors the achievable phase above reserve.
        (
            w,
            w - (40_000_000 + i30 / 2),
            w - 40_000_000,
            3,
            i30,
            2,
            100,
        ), // natural phase at floor+quantum -> inert
        (
            w,
            w - (40_000_000 + 2 * i30),
            w - 40_000_000,
            3,
            i30,
            2,
            100,
        ), // 2 frames over floor -> fires
        (w, w - (40_000_000 + i30), w - 40_000_000, 3, i30, 1, 100), // n=1: floor + 1 canvas frame -> inert
        (w, w + i30, w + i30, 20, i30, 2, 100), // newest ahead of wall (floor saturates to 0) -> target=reserve
    ];
    // A deterministic LCG spread over the argument space, now including the floor axis.
    let mut x: u64 = 0x1234_5678_9abc_def1;
    for _ in 0..120 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let latency = ((x >> 20) % 1200) as u32;
        let interval = if x & 1 == 0 { i30 } else { i60 };
        let n = ((x >> 3) % 3) as u32; // 0..2 (0 exercises the floor)
        let ticks = (x >> 7) % 60;
        let over = (x >> 40) % 3 * interval; // 0, 1 or 2 quanta over configured
        let boundary = w.saturating_sub(latency as u64 * 1_000_000 + over + (x >> 50) % 4_000_000);
        let skew = (x >> 45) % 60_000_000; // 0..60ms transport floor
        let newest = w.saturating_sub(skew);
        v.push((w, boundary, newest, latency, interval, n, ticks));
    }
    v
}

#[test]
fn c_phase_convergence_matches_the_rust_authority_1049() {
    let helper = lift_converge_helper();
    let vs = converge_vectors();

    // Lift the two constants from the SHIPPED C, never hard-code them (review 🟡2).
    let mut c = format!(
        "#include <stdint.h>\n#include <stddef.h>\n#include <stdbool.h>\n#include <stdio.h>\n{}\n{}\n",
        lift_define("GENLOCK_PHASE_PIN_HYSTERESIS_NS"),
        lift_define("GENLOCK_DRAIN_MIN_TICK_INTERVAL"),
    );
    c.push_str(&helper);
    c.push_str("int main(void){\n");
    for (wall, boundary, newest, latency, interval, n, ticks) in &vs {
        c.push_str(&format!(
            "    printf(\"%d\\n\", genlock_phase_converge_due({wall}ULL, {boundary}ULL, {newest}ULL, \
             {latency}, {interval}ULL, {n}, {ticks}ULL));\n"
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("genlock_converge_parity_1049");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let cfile = dir.join("converge.c");
    let bin = dir.join("converge.bin");
    fs::write(&cfile, &c).expect("write the parity harness");

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args(["-std=gnu99", "-Wall", "-Wextra", "-Werror", "-O1"])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "#1049: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 vendored genlock_phase_converge_due to prove the C and the Rust authority agree; \
                 it must FAIL rather than skip when the toolchain is absent. Install a C compiler \
                 or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1049: genlock_phase_converge_due lifted from {OBS_SOURCE} does NOT COMPILE standalone \
         under -Wall -Wextra -Werror:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin)
        .output()
        .expect("#1049: the compiled parity harness failed to execute");
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let c_out: Vec<bool> = stdout.lines().map(|l| l.trim() == "1").collect();
    assert_eq!(
        c_out.len(),
        vs.len(),
        "#1049: harness printed the wrong count"
    );

    let mut diffs = Vec::new();
    for (i, ((wall, boundary, newest, latency, interval, n, ticks), got_c)) in
        vs.iter().zip(&c_out).enumerate()
    {
        let got_rs =
            should_converge_phase(*wall, *boundary, *newest, *latency, *interval, *n, *ticks);
        if got_rs != *got_c {
            diffs.push(format!(
                "  vector {i}: wall={wall} boundary={boundary} newest={newest} latency={latency} \
                 interval={interval} n={n} ticks={ticks} -> C {got_c}, Rust {got_rs}"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1049: the vendored C phase-convergence decision DIVERGED from the Tier-0 Rust authority \
         on {} of {} vectors — the deployed shed is not the behaviour the unit tests cover:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
}
