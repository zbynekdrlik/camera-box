//! #293 — behavioral regression test for the multiview anti-starvation floor.
//!
//! #278 decoupled the heavy strih Multiview from the 60fps program render by skipping a
//! throttleable monitoring display (render_divisor > 1) when its measured render cost would
//! not fit the budget remaining after the program this tick. A single 4-live-cam Multiview
//! render (~18-23ms) ALONE exceeds the ~15ms budget, so the skip fired EVERY tick and the
//! Multiview FROZE solid for a whole live event (#293). The decouple must THROTTLE the
//! monitoring display, never disable it.
//!
//! The skip decision is the pure, OBS-dependency-free `obs_display_should_skip()` in
//! vendor/obs-studio/libobs/obs-display-budget.h — the EXACT function the production graphics
//! thread (render_display() in obs-display.c) calls. This test compiles + runs that real
//! header in a tiny C harness (no probe feature, no OBS core, plain `cc`), driving the same
//! skip/render loop render_display() runs, and asserts an over-budget monitoring display
//! NEVER starves: it renders at least once within OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS+1 ticks,
//! while the program (render_divisor <= 1) is never throttled.
//!
//! RED (before the fix): the header has no liveness floor -> the over-budget display is
//! skipped on every tick -> the harness exits non-zero -> this test FAILS. GREEN (after the
//! fix): the floor forces a render after K consecutive skips -> the harness passes.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The C harness: includes the REAL production header and replays render_display()'s
/// skip/render loop (skip -> bump the per-display counter; render -> reset it), then checks
/// the liveness + program-sacred invariants. Exits 0 on success, non-zero with a message.
const HARNESS_C: &str = r#"
#include "obs-display-budget.h"
#include <stdio.h>
#include <stdint.h>

/* Replay the render_display() loop for `ticks` ticks against a fixed display profile.
 * Returns the longest run of consecutive skips; writes the render count to *renders. */
static int run_loop(uint32_t divisor, uint64_t ewma, uint64_t elapsed, uint64_t budget,
                    int ticks, int *renders)
{
    uint32_t consec = 0;
    int max_consec = 0;
    int r = 0;
    for (int i = 0; i < ticks; i++) {
        if (obs_display_should_skip(divisor, ewma, elapsed, budget, consec)) {
            consec++;
            if ((int)consec > max_consec)
                max_consec = (int)consec;
        } else {
            r++;
            consec = 0; /* a real render clears the skip run (#293) */
        }
    }
    *renders = r;
    return max_consec;
}

int main(void)
{
    const uint64_t interval = 16666667ULL;            /* 60fps frame interval (ns) */
    const uint64_t budget = interval - interval / 10; /* 90% safety margin */
    const int K = (int)OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS;
    const int ticks = 240;

    /* (1) A heavy over-budget Multiview (ewma 22ms, 9ms already elapsed) must NEVER freeze:
     *     it must render at least once and never be skipped more than K ticks in a row. */
    int renders = 0;
    int max_consec = run_loop(2, 22000000ULL, 9000000ULL, budget, ticks, &renders);
    if (renders == 0) {
        fprintf(stderr, "FAIL: over-budget multiview NEVER rendered over %d ticks (frozen)\n", ticks);
        return 1;
    }
    if (max_consec > K) {
        fprintf(stderr, "FAIL: over-budget multiview starved %d consecutive ticks (> K=%d)\n",
                max_consec, K);
        return 2;
    }

    /* (2) The program output (render_divisor <= 1) must NEVER be throttled, even over budget. */
    int prog_renders = 0;
    int prog_max = run_loop(1, 22000000ULL, 9000000ULL, budget, ticks, &prog_renders);
    if (prog_renders != ticks || prog_max != 0) {
        fprintf(stderr, "FAIL: program render throttled (renders=%d/%d, longest skip run=%d)\n",
                prog_renders, ticks, prog_max);
        return 3;
    }

    /* (3) A not-yet-warmed-up display (ewma==0) must render to measure (never pre-starved). */
    if (obs_display_should_skip(2, 0, 9000000ULL, budget, 0)) {
        fprintf(stderr, "FAIL: cold (ewma==0) display skipped before its cost was measured\n");
        return 4;
    }

    /* (4) A light, under-budget monitoring display renders every tick. */
    int light_renders = 0;
    int light_max = run_loop(2, 2000000ULL, 1000000ULL, budget, ticks, &light_renders);
    if (light_renders != ticks || light_max != 0) {
        fprintf(stderr, "FAIL: under-budget display skipped (renders=%d/%d, skip run=%d)\n",
                light_renders, ticks, light_max);
        return 5;
    }

    printf("OK K=%d over-budget: renders=%d max_consec_skips=%d\n", K, renders, max_consec);
    return 0;
}
"#;

#[test]
fn over_budget_monitoring_display_never_starves() {
    let libobs = manifest_dir().join("vendor/obs-studio/libobs");
    let header = libobs.join("obs-display-budget.h");
    assert!(
        header.exists(),
        "#293: missing pure skip-decision header {} — render_display()'s budget skip must be \
         extracted into an OBS-dependency-free header so it is unit-testable.",
        header.display()
    );

    // Unique temp workspace so parallel test runs don't collide.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = std::env::temp_dir().join(format!("obs_display_budget_{}_{}", std::process::id(), stamp));
    std::fs::create_dir_all(&work).expect("create temp workdir");
    let src = work.join("harness.c");
    let bin = work.join("harness");
    std::fs::write(&src, HARNESS_C).expect("write C harness");

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let compile = Command::new(&cc)
        .arg("-std=c11")
        .arg("-Wall")
        .arg("-I")
        .arg(&libobs)
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| panic!("failed to invoke C compiler '{cc}': {e}"));
    assert!(
        compile.status.success(),
        "#293: the skip-decision header failed to compile with {cc}:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin).output().expect("run compiled harness");
    let _ = std::fs::remove_dir_all(&work); // best-effort cleanup
    assert!(
        run.status.success(),
        "#293: anti-starvation harness FAILED (exit {:?}). An over-budget monitoring display \
         must render within OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS+1 ticks (never freeze), while the \
         program stays full-rate.\nstdout: {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}
