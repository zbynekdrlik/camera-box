//! #293 — behavioral regression test for the multiview anti-starvation floor.
//! #756 — extended with the hard CADENCE FLOOR regression (frame_counter % render_divisor).
//!
//! #278 decoupled the heavy strih Multiview from the 60fps program render by skipping a
//! throttleable monitoring display (render_divisor > 1) when its measured render cost would
//! not fit the budget remaining after the program this tick. A single 4-live-cam Multiview
//! render (~18-23ms) ALONE exceeds the ~15ms budget, so the skip fired EVERY tick and the
//! Multiview FROZE solid for a whole live event (#293). The decouple must THROTTLE the
//! monitoring display, never disable it.
//!
//! #756 live finding (imag-nb, 2026-07-15): the #278/#293 ADAPTIVE budget gate is a SOFT
//! throttle — it only skips when the display's measured cost genuinely does not fit the
//! remaining per-tick budget. On imag the Multiview render (~6.7-10.45ms EWMA) fits
//! comfortably under the ~15ms (90%) budget on nearly every tick (elapsed-before-MV ~3.6ms +
//! ewma ~6.7-10.45ms = ~10.3-14.05ms <= 15ms), so the adaptive gate almost never fires and the
//! Multiview renders EVERY tick at full 60fps — the render_divisor=2 marker is set correctly
//! (proven by reading OBSProjector.cpp) but never actually HALVES the render cost, because the
//! gate that would enforce that is budget-conditional, not cadence-based. This defeats the
//! purpose of the divisor: monitoring cost is never reduced when the display is cheap enough to
//! always fit.
//!
//! Fix: a hard CADENCE FLOOR layered on top of (never replacing) the existing budget gate.
//! `obs_display_should_skip()` now also takes the display's own `frame_counter` (incremented
//! every tick, mirroring the pre-#278 `#276` per-instance counter) and unconditionally skips
//! whenever `frame_counter % render_divisor != 0` — REGARDLESS of measured cost or budget
//! headroom. The existing budget-based skip (and its #293 anti-starvation cap) still applies
//! on top, on the cadence-eligible ticks, so an over-budget display is still guaranteed to
//! render within K+1 ticks. Program/preview (render_divisor <= 1) are NEVER cadence-throttled.
//!
//! The skip decision is the pure, OBS-dependency-free `obs_display_should_skip()` in
//! vendor/obs-studio/libobs/obs-display-budget.h — the EXACT function the production graphics
//! thread (render_display() in obs-display.c) calls. This test compiles + runs that real
//! header in a tiny C harness (no probe feature, no OBS core, plain `cc`), driving the same
//! skip/render loop render_display() runs.
//!
//! RED (before the #756 fix): `obs_display_should_skip()` has no `frame_counter` parameter and
//! no cadence term -> this harness (which calls it with the NEW 6-arg signature and asserts a
//! light, comfortably-under-budget monitoring display is throttled to ~1/divisor of the tick
//! rate) fails to COMPILE against the unfixed header -> this test FAILS (compile error, a valid
//! RED per this repo's established pattern for coupled header+harness changes — e.g. #756's own
//! gl_x11_viewport_cache_756.rs). GREEN (after the fix): the header gains the `frame_counter`
//! parameter + the modulo term, the harness compiles and all assertions pass.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The C harness: includes the REAL production header and replays render_display()'s
/// skip/render loop (skip -> bump the per-display counter; render -> reset it), maintaining a
/// per-display `frame_counter` that increments every tick (mirroring
/// `display->render_frame_counter++` in obs-display.c), then checks the liveness,
/// program-sacred, AND cadence-floor invariants. Exits 0 on success, non-zero with a message.
const HARNESS_C: &str = r#"
#include "obs-display-budget.h"
#include <stdio.h>
#include <stdint.h>

/* Replay the render_display() loop for `ticks` ticks against a fixed display profile.
 * `frame_counter` increments every tick BEFORE the check (mirrors
 * `display->render_frame_counter++` happening in obs-display.c ahead of the should-skip call).
 * Returns the longest run of consecutive skips; writes the render count to *renders. */
static int run_loop(uint32_t divisor, uint64_t ewma, uint64_t elapsed, uint64_t budget,
                    int ticks, int *renders)
{
    uint32_t consec = 0;
    uint32_t frame_counter = 0;
    int max_consec = 0;
    int r = 0;
    for (int i = 0; i < ticks; i++) {
        frame_counter++;
        if (obs_display_should_skip(divisor, frame_counter, ewma, elapsed, budget, consec)) {
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
     *     it must render at least once and never be skipped more than K ticks in a row, even
     *     with the cadence floor layered on top. */
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

    /* (2) The program output (render_divisor <= 1) must NEVER be throttled by the cadence
     *     floor OR the budget gate, even over budget, across every frame_counter value. */
    int prog_renders = 0;
    int prog_max = run_loop(1, 22000000ULL, 9000000ULL, budget, ticks, &prog_renders);
    if (prog_renders != ticks || prog_max != 0) {
        fprintf(stderr, "FAIL: program render throttled (renders=%d/%d, longest skip run=%d)\n",
                prog_renders, ticks, prog_max);
        return 3;
    }

    /* (3) A not-yet-warmed-up display (ewma==0) must render to measure (never pre-starved),
     *     even on a frame_counter value the cadence floor would otherwise force-skip. */
    if (obs_display_should_skip(2, 1 /* odd: a cadence-skip position for divisor=2 */,
                                0, 9000000ULL, budget, 0)) {
        fprintf(stderr, "FAIL: cold (ewma==0) display skipped before its cost was measured\n");
        return 4;
    }

    /* (4) #756 THE CORE REGRESSION: a light, comfortably-under-budget monitoring display must
     *     NOT render every tick any more -- it must be throttled to the cadence floor
     *     (1/divisor of the tick rate), REGARDLESS of how cheap its measured cost is. This is
     *     the exact imag-nb live finding: MV ewma ~6.7-10.45ms fits under the ~15ms budget on
     *     nearly every tick, so the old budget-only gate rendered it every tick. */
    int light_renders = 0;
    int light_max = run_loop(2, 2000000ULL, 1000000ULL, budget, ticks, &light_renders);
    if (light_renders == ticks) {
        fprintf(stderr, "FAIL: light monitoring display still renders EVERY tick (%d/%d) -- "
                "the cadence floor is not throttling a cheap, always-under-budget display\n",
                light_renders, ticks);
        return 5;
    }
    if (light_renders != ticks / 2) {
        fprintf(stderr, "FAIL: light monitoring display (divisor=2) rendered %d/%d ticks, "
                "expected exactly ticks/2=%d (a hard 1/divisor cadence)\n",
                light_renders, ticks, ticks / 2);
        return 6;
    }
    if (light_max > 1) {
        fprintf(stderr, "FAIL: light monitoring display's cadence-forced skip run was %d "
                "consecutive ticks (expected at most 1 -- every other tick renders)\n",
                light_max);
        return 7;
    }

    /* (5) The cadence floor generalizes to divisor=3 (not just the hard-coded 2 the multiview
     *     currently uses) -- a light display should render close to ticks/3, never every tick. */
    int div3_renders = 0;
    int div3_max = run_loop(3, 2000000ULL, 1000000ULL, budget, ticks, &div3_renders);
    if (div3_renders != ticks / 3) {
        fprintf(stderr, "FAIL: light monitoring display (divisor=3) rendered %d/%d ticks, "
                "expected exactly ticks/3=%d\n",
                div3_renders, ticks, ticks / 3);
        return 8;
    }
    if (div3_max > 2) {
        fprintf(stderr, "FAIL: light monitoring display (divisor=3) cadence-forced skip run "
                "was %d consecutive ticks (expected at most divisor-1=2)\n",
                div3_max);
        return 9;
    }

    printf("OK K=%d over-budget: renders=%d max_consec_skips=%d; light divisor=2: "
           "renders=%d/%d; light divisor=3: renders=%d/%d\n",
           K, renders, max_consec, light_renders, ticks, div3_renders, ticks);
    return 0;
}
"#;

#[test]
fn over_budget_monitoring_display_never_starves_and_light_display_is_cadence_throttled() {
    let libobs = manifest_dir().join("vendor/obs-studio/libobs");
    let header = libobs.join("obs-display-budget.h");
    assert!(
        header.exists(),
        "#293/#756: missing pure skip-decision header {} — render_display()'s budget skip must \
         be extracted into an OBS-dependency-free header so it is unit-testable.",
        header.display()
    );

    // Unique temp workspace so parallel test runs don't collide.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = std::env::temp_dir().join(format!(
        "obs_display_budget_{}_{}",
        std::process::id(),
        stamp
    ));
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
        "#293/#756: the skip-decision header failed to compile with {cc} — this harness calls \
         obs_display_should_skip() with the NEW 6-arg (frame_counter-carrying) signature; a \
         compile failure here means the #756 cadence-floor parameter has not landed yet \
         (expected RED state before the fix):\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin).output().expect("run compiled harness");
    let _ = std::fs::remove_dir_all(&work); // best-effort cleanup
    assert!(
        run.status.success(),
        "#293/#756: anti-starvation + cadence-floor harness FAILED (exit {:?}). An over-budget \
         monitoring display must render within OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS+1 ticks (never \
         freeze); a LIGHT (always-under-budget) monitoring display must be throttled to a hard \
         1/render_divisor cadence (never render every tick); the program stays full-rate \
         throughout.\nstdout: {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}
