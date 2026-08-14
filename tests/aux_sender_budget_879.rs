//! #879 — EXECUTABLE gates for the aux-NDI-sender render-budget priority mechanism.
//!
//! Two things are proven here by COMPILING and RUNNING the shipped vendored bytes (not by a
//! static text anchor, which only proves the C still *says* the right thing, never that it
//! *computes* it):
//!
//! 1. `obs_effective_render_divisor()` in `vendor/obs-studio/libobs/obs-display-budget.h` is
//!    byte-for-byte numerically identical to the Tier-0 authority
//!    [`camera_box::render_budget::effective_render_divisor`] over a spread of vectors. A flipped
//!    clamp or a dropped rounding term diverges here in seconds.
//!
//! 2. `obs_aux_sender_should_skip()` in `vendor/obs-studio/libobs/obs.c` — lifted VERBATIM and
//!    compiled against a tiny `obs` global stub + the REAL header — holds the invariants the
//!    ticket demands: program priority (never-warmed / not-ticking / program-divisor-0 never
//!    skip), budget gating (fits -> render, over -> skip), and the #293 anti-starvation cap
//!    (an over-budget sender renders within K+1 ticks — never freezes).
//!
//! `cc` is required (present on every runner + the self-hosted dev boxes). Per test-strictness
//! this FAILS LOUDLY rather than skipping if the toolchain is missing — a gate that silently
//! passes without running is worse than none.

use camera_box::render_budget::effective_render_divisor;
use std::path::PathBuf;
use std::process::Command;

fn libobs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/obs-studio/libobs")
}

fn workdir(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let d = std::env::temp_dir().join(format!(
        "aux_budget_879_{tag}_{}_{}",
        std::process::id(),
        stamp
    ));
    std::fs::create_dir_all(&d).expect("create temp workdir");
    d
}

fn compile(src: &PathBuf, bin: &PathBuf) {
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args(["-std=c11", "-Wall", "-Wextra", "-O1", "-I"])
        .arg(libobs())
        .arg(src)
        .arg("-o")
        .arg(bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "#879: could not run the C compiler `{cc}` ({e}). This gate compiles the shipped \
                 vendored bytes; the vendored tree is otherwise built only by the genlock \
                 workflows, so this test is the only pre-CI check of them."
            )
        });
    assert!(
        out.status.success(),
        "#879: the vendored header/seam failed to compile with {cc} under -Wall -Wextra:\n\
         --- cc stderr ---\n{}\n--- harness ---\n{}",
        String::from_utf8_lossy(&out.stderr),
        std::fs::read_to_string(src).unwrap_or_default(),
    );
}

const DIV_HARNESS: &str = r#"
#include "obs-display-budget.h"
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
int main(int argc, char **argv)
{
    for (int i = 1; i + 1 < argc; i += 2) {
        uint32_t cfg = (uint32_t)strtoul(argv[i], NULL, 10);
        uint64_t iv = (uint64_t)strtoull(argv[i + 1], NULL, 10);
        printf("%u\n", obs_effective_render_divisor(cfg, iv));
    }
    return 0;
}
"#;

#[test]
fn c_effective_divisor_matches_rust_authority_879() {
    let header = libobs().join("obs-display-budget.h");
    assert!(
        header.exists(),
        "#879: missing {} — the pure effective-divisor helper the aux path reuses is gone.",
        header.display()
    );

    // A spread across program (0), configured 1/2/3, and canvas rates from 10fps to 120fps,
    // plus interval 0 (video not running).
    let cfgs: [u32; 4] = [0, 1, 2, 3];
    let intervals_ns: [u64; 7] = [
        0,           // video not running
        100_000_000, // 10 fps
        33_333_333,  // 30 fps
        20_000_000,  // 50 fps
        16_666_666,  // 60 fps
        11_111_111,  // 90 fps
        8_333_333,   // 120 fps
    ];
    let mut vectors: Vec<(u32, u64)> = Vec::new();
    for &c in &cfgs {
        for &iv in &intervals_ns {
            vectors.push((c, iv));
        }
    }

    let work = workdir("div");
    let src = work.join("div.c");
    let bin = work.join("div");
    std::fs::write(&src, DIV_HARNESS).expect("write harness");
    compile(&src, &bin);

    let mut args: Vec<String> = Vec::new();
    for &(c, iv) in &vectors {
        args.push(c.to_string());
        args.push(iv.to_string());
    }
    let run = Command::new(&bin)
        .args(&args)
        .output()
        .expect("run div harness");
    let _ = std::fs::remove_dir_all(&work);
    assert!(run.status.success(), "#879: div harness exited nonzero");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let got: Vec<u32> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().parse::<u32>().expect("parse C output"))
        .collect();
    assert_eq!(
        got.len(),
        vectors.len(),
        "#879: C printed {} results for {} vectors",
        got.len(),
        vectors.len()
    );
    for (i, &(c, iv)) in vectors.iter().enumerate() {
        let rust = effective_render_divisor(c, iv);
        assert_eq!(
            got[i], rust,
            "#879 PARITY DIVERGENCE: obs_effective_render_divisor({c}, {iv}) = {} (C) vs {} (Rust \
             authority). The vendored C must mirror src/render_budget.rs byte-for-byte.",
            got[i], rust
        );
    }
}

#[test]
fn c_aux_sender_should_skip_holds_invariants_879() {
    // Lift obs_aux_sender_should_skip() VERBATIM from the shipped obs.c and compile it against a
    // tiny obs-global stub + os_gettime_ns stub + the REAL header, then drive the invariants.
    let obs_c = std::fs::read_to_string(libobs().join("obs.c")).expect("read obs.c");
    let sig = "bool obs_aux_sender_should_skip(";
    let start = obs_c.find(sig).unwrap_or_else(|| {
        panic!("#879: obs.c no longer defines {sig} — the aux budget seam is gone.")
    });
    // The function body has no nested braces (all ifs are single-statement), so the first
    // "\n}" after the signature is its closing brace.
    let rest = &obs_c[start..];
    let end = rest.find("\n}").unwrap_or_else(|| {
        panic!("#879: could not find the end of obs_aux_sender_should_skip in obs.c")
    }) + 2;
    let lifted = &rest[..end];

    let harness = format!(
        r#"
#include "obs-display-budget.h"
#include <stdint.h>
#include <stdbool.h>
#include <stdio.h>

static uint64_t g_now;
static uint64_t os_gettime_ns(void) {{ return g_now; }}
struct video_stub {{ uint64_t video_frame_interval_ns; uint64_t graphics_frame_start_ns; }};
static struct {{ struct video_stub video; }} _obs = {{{{0, 0}}}};
static struct {{ struct video_stub video; }} *obs = &_obs;

/* ---- lifted VERBATIM from obs.c ---- */
{lifted}
/* ---- end lifted ---- */

int main(void)
{{
    int fail = 0;
    const uint64_t IV30 = 33333333ULL; /* 30fps canvas -> effective divisor 1 (pure budget) */
    obs->video.video_frame_interval_ns = IV30;
    obs->video.graphics_frame_start_ns = 1000;

    /* not warmed (ewma 0): always render (measure once) */
    g_now = 1000 + 5000000ULL;
    if (obs_aux_sender_should_skip(2, 1, 0, 0)) {{ printf("FAIL: warmup skipped\n"); fail = 1; }}

    /* fits budget: 5ms elapsed + 5ms ewma = 10ms <= 30ms budget -> render */
    if (obs_aux_sender_should_skip(2, 1, 5000000, 0)) {{ printf("FAIL: fit-budget skipped\n"); fail = 1; }}

    /* over budget: 28ms elapsed + 5ms ewma = 33ms > 30ms budget -> skip (consec < K) */
    g_now = 1000 + 28000000ULL;
    if (!obs_aux_sender_should_skip(2, 1, 5000000, 0)) {{ printf("FAIL: over-budget not skipped\n"); fail = 1; }}

    /* over budget but already skipped K in a row -> render (never freeze, #293) */
    if (obs_aux_sender_should_skip(2, 1, 5000000, 3)) {{ printf("FAIL: froze at K consecutive skips\n"); fail = 1; }}

    /* program marker (divisor 0): never skip, even over budget */
    if (obs_aux_sender_should_skip(0, 1, 5000000, 0)) {{ printf("FAIL: program (divisor 0) skipped\n"); fail = 1; }}

    /* not ticking (tick_start 0): never skip */
    obs->video.graphics_frame_start_ns = 0;
    if (obs_aux_sender_should_skip(2, 1, 5000000, 0)) {{ printf("FAIL: skipped while not ticking\n"); fail = 1; }}

    if (!fail) printf("all aux-sender invariants hold\n");
    return fail;
}}
"#
    );

    let work = workdir("seam");
    let src = work.join("seam.c");
    let bin = work.join("seam");
    std::fs::write(&src, harness).expect("write seam harness");
    compile(&src, &bin);
    let run = Command::new(&bin).output().expect("run seam harness");
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        run.status.success(),
        "#879: aux-sender seam invariants FAILED (exit {:?}). Program priority (never-warmed / \
         not-ticking / divisor-0 never skip), budget gating, and the #293 never-freeze cap must \
         all hold.\nstdout: {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}
