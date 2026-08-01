//! #926 — the LIVE dock's genlock_latency_ms_src auto-corrector must never leave audio early.
//!
//! `camerabox::CbDockLockCorrector` (camera-box-audio.hpp) is the C++ mirror of
//! `src/av_sync_dock.rs::DockLockCorrector` — see that module's own doc comment for the closed
//! -form proof (`new_delay = current_delay + floor(ts_ms)` always lands the resulting offset in
//! `[0, 1)`, i.e. 0ms or audio late by under 1ms, never negative/"audio early"). This harness
//! cross-checks the C++ port against the SAME scenarios the Rust unit tests already pin, via the
//! same twin-harness pattern `tests/av_sync_dock_audit_log.rs` (#634) established: compile+run a
//! tiny real C++ program against the real production header, off-rig, no vendored-OBS build
//! needed (the dock's own frontend build is CI-only, invisible to a normal PR — see this repo's
//! CLAUDE.md genlock GOTCHA).

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const HARNESS_CPP: &str = r#"
#include "camera-box-audio.hpp"
#include <cstdio>
#include <cmath>
#include <limits>

using namespace camerabox;

static int g_failures = 0;

#define CHECK(cond, msg)                                                          \
    do {                                                                          \
        if (!(cond)) {                                                            \
            std::fprintf(stderr, "FAIL (%s:%d): %s\n", __FILE__, __LINE__, msg);  \
            g_failures++;                                                         \
        }                                                                         \
    } while (false)

int main()
{
    // (1) Unlocked (real event, no test signal) never touches the actuator.
    {
        CbDockLockCorrector c;
        CbDockLockAction a = c.decide(false, -52.2, 5.0, 950, 1000000000ull);
        CHECK(!a.apply, "unlocked must always Hold, regardless of how wrong the last offset was");
    }

    // (2) Already in the safety-margin zone ([5,6) with mad_ms=5.0) -- both boundary-ish values Hold.
    {
        CbDockLockCorrector c;
        CHECK(!c.decide(true, 5.0, 5.0, 950, 1000000000ull).apply, "ts=5.0 already converged (margin=5)");
        CbDockLockCorrector c2;
        CHECK(!c2.decide(true, 5.9, 5.0, 950, 1000000000ull).apply, "ts=5.9 already converged (margin=5)");
    }

    // (3) A large audio-early error, step-clamped to the 5ms default budget, moves in the
    // CORRECT direction (reduce the delay) by exactly the step, never overshoots.
    {
        CbDockLockCorrector c;
        CbDockLockAction a = c.decide(true, -52.2, 5.0, 950, 1000000000ull);
        CHECK(a.apply, "a -52.2ms error must trigger a correction");
        CHECK(a.new_delay_ms == 945, "must reduce by exactly the 5ms step budget");
    }

    // (4) Hardware floor: wants to go below 3ms -> clamps to exactly 3.
    {
        CbDockLockCorrector c(50, 30.0);
        CbDockLockAction a = c.decide(true, -10.0, 5.0, 5, 1000000000ull);
        CHECK(a.apply && a.new_delay_ms == 3, "must clamp at the hardware floor");
    }
    // Already pinned at the floor -- Hold, not a pointless re-write of the same value.
    {
        CbDockLockCorrector c(50, 30.0);
        CbDockLockAction a = c.decide(true, -10.0, 5.0, 3, 1000000000ull);
        CHECK(!a.apply, "already at the floor with no room to correct further must Hold");
    }
    // (4b) #926 fix-up finding 9: pinned at the floor with the invariant STILL violated (a real
    // hardware limit, explicitly asserted rather than silently excluded).
    {
        CbDockLockCorrector c(5, 30.0);
        CbDockLockAction a = c.decide(true, -20.0, 5.0, 3, 1000000000ull);
        CHECK(!a.apply, "pinned at the floor -- no room to correct a -20ms error further");
    }

    // (5) Hardware ceiling: wants to go above 2000ms -> clamps to exactly 2000.
    {
        CbDockLockCorrector c(50, 30.0);
        CbDockLockAction a = c.decide(true, 10.0, 5.0, 1998, 1000000000ull);
        CHECK(a.apply && a.new_delay_ms == 2000, "must clamp at the hardware ceiling");
    }
    {
        CbDockLockCorrector c(50, 30.0);
        CbDockLockAction a = c.decide(true, 10.0, 5.0, 2000, 1000000000ull);
        CHECK(!a.apply, "already at the ceiling with no room to correct further must Hold");
    }

    // (6) Cooldown: a second correction within min_reapply_s of the first must Hold; after the
    // cooldown elapses (measured from the LAST APPLIED write) it applies again.
    {
        CbDockLockCorrector c(5, 30.0);
        CbDockLockAction a1 = c.decide(true, -52.2, 5.0, 950, 1000000000ull); // t=1s
        CHECK(a1.apply, "first correction must apply");
        CbDockLockAction a2 = c.decide(true, -47.2, 5.0, 945, 11000000000ull); // t=11s (10s later)
        CHECK(!a2.apply, "within the 30s cooldown must Hold, even though further correction is due");
        CbDockLockAction a3 = c.decide(true, -47.2, 5.0, 945, 32000000000ull); // t=32s (31s after a1)
        CHECK(a3.apply, "cooldown elapsed -- must apply again");
    }

    // (7) Excess audio-lateness (not forbidden, but not minimal either) is nudged back toward the
    // 5ms safety margin (not a bare 0 -- #926 fix-up finding 3).
    {
        CbDockLockCorrector c(50, 30.0);
        CbDockLockAction a = c.decide(true, 42.0, 5.0, 950, 1000000000ull);
        CHECK(a.apply && a.new_delay_ms == 987, "42ms of excess audio-lateness must be reduced to the margin");
    }

    // (8) The never-below-margin invariant, swept across a range of offsets/mads/current-delays
    // with an effectively-unclamped step budget, mirroring the Rust property test.
    {
        double offsets[] = {-523.7, -100.0, -52.2, -10.4, -1.0, -0.1, 0.0, 0.5, 3.3, 10.0, 42.9, 100.0, 900.0};
        int32_t currents[] = {3, 50, 500, 950, 1000, 1500, 1999, 2000};
        double mads[] = {0.1, 1.0, 5.0, 25.0, 50.0};
        for (size_t oi = 0; oi < sizeof(offsets) / sizeof(offsets[0]); oi++) {
            for (size_t ci = 0; ci < sizeof(currents) / sizeof(currents[0]); ci++) {
                for (size_t mi = 0; mi < sizeof(mads) / sizeof(mads[0]); mi++) {
                    CbDockLockCorrector c(100000, 30.0);
                    CbDockLockAction a = c.decide(true, offsets[oi], mads[mi], currents[ci], 1000000000ull);
                    int32_t new_delay = a.apply ? a.new_delay_ms : currents[ci];
                    bool hit_rail = new_delay == 3 || new_delay == 2000;
                    if (!hit_rail) {
                        double delta_applied = (double)(new_delay - currents[ci]);
                        double ts_new = offsets[oi] - delta_applied;
                        double margin = mads[mi] < 1.0 ? 1.0 : (mads[mi] > 25.0 ? 25.0 : mads[mi]);
                        CHECK(ts_new >= margin - 1e-9 && ts_new < margin + 1.0 + 1e-9,
                              "never-below-margin invariant violated for some (offset, mad, current) triple");
                    } else {
                        CHECK(new_delay == 3 || new_delay == 2000, "hit_rail flag disagrees with new_delay");
                    }
                }
            }
        }
    }

    // (9) #926 fix-up finding 5: non-finite offset_ms must always Hold, never crash/UB.
    {
        double bad[] = {std::nan(""), std::numeric_limits<double>::infinity(),
                        -std::numeric_limits<double>::infinity()};
        for (size_t i = 0; i < sizeof(bad) / sizeof(bad[0]); i++) {
            CbDockLockCorrector c(5, 30.0);
            CHECK(!c.decide(true, bad[i], 5.0, 950, 1000000000ull).apply,
                  "non-finite offset_ms must always Hold");
        }
        // An astronomically large but still-FINITE offset must not crash and must move in the
        // correct, step-capped direction.
        CbDockLockCorrector c(5, 30.0);
        CbDockLockAction a = c.decide(true, 1e18, 5.0, 950, 1000000000ull);
        CHECK(a.apply && a.new_delay_ms == 955, "huge finite positive offset -- step-capped increase");
        CbDockLockCorrector c2(5, 30.0);
        CbDockLockAction a2 = c2.decide(true, -1e18, 5.0, 950, 1000000000ull);
        CHECK(a2.apply && a2.new_delay_ms == 945, "huge finite negative offset -- step-capped decrease");
    }

    // (10) #926 fix-up finding 3: the margin CLAMPS at both ends -- a tiny/zero mad floors at
    // CB_DOCK_LOCK_MIN_MARGIN_MS, a huge mad ceilings at CB_CLUSTER_MAX_MAD_MS.
    {
        CbDockLockCorrector c(100000, 30.0);
        CbDockLockAction a = c.decide(true, 10.0, 0.0, 950, 1000000000ull);
        CHECK(a.apply, "10ms excess must be corrected even with mad=0");
        double ts_new = 10.0 - (double)(a.new_delay_ms - 950);
        CHECK(ts_new >= CB_DOCK_LOCK_MIN_MARGIN_MS - 1e-9 && ts_new < CB_DOCK_LOCK_MIN_MARGIN_MS + 1.0 + 1e-9,
              "margin must clamp at the 1ms floor, not a bare 0");

        CbDockLockCorrector c2(100000, 30.0);
        CbDockLockAction a2 = c2.decide(true, 100.0, 500.0, 950, 1000000000ull);
        CHECK(a2.apply, "100ms excess must be corrected even with an absurd mad");
        double ts_new2 = 100.0 - (double)(a2.new_delay_ms - 950);
        CHECK(ts_new2 >= CB_CLUSTER_MAX_MAD_MS - 1e-9 && ts_new2 < CB_CLUSTER_MAX_MAD_MS + 1.0 + 1e-9,
              "margin must clamp at CB_CLUSTER_MAX_MAD_MS");
    }

    // (11) #926 fix-up finding 10: the DEFAULT-constructed corrector (dock()-equivalent) must
    // behave IDENTICALLY to an explicit (CB_DOCK_LOCK_MAX_STEP_MS, CB_DOCK_LOCK_MIN_REAPPLY_S)
    // one, and the constants themselves must hold their documented values.
    {
        CHECK(CB_DOCK_LOCK_MAX_STEP_MS == 5, "CB_DOCK_LOCK_MAX_STEP_MS must mirror Rust's DOCK_LOCK_MAX_STEP_MS");
        CHECK(CB_DOCK_LOCK_MIN_REAPPLY_S == 30.0,
              "CB_DOCK_LOCK_MIN_REAPPLY_S must mirror Rust's DOCK_LOCK_MIN_REAPPLY_S");
        CHECK(CB_DOCK_LOCK_LATENCY_MIN_MS == 3,
              "CB_DOCK_LOCK_LATENCY_MIN_MS must mirror Rust's DOCK_LOCK_LATENCY_MIN_MS");
        CHECK(CB_DOCK_LOCK_LATENCY_MAX_MS == 2000,
              "CB_DOCK_LOCK_LATENCY_MAX_MS must mirror Rust's DOCK_LOCK_LATENCY_MAX_MS");
        CHECK(CB_DOCK_LOCK_MIN_MARGIN_MS == 1.0,
              "CB_DOCK_LOCK_MIN_MARGIN_MS must mirror Rust's DOCK_LOCK_MIN_MARGIN_MS");

        CbDockLockCorrector cdef;
        CbDockLockCorrector cexplicit(5, 30.0);
        CbDockLockAction adef = cdef.decide(true, -52.2, 5.0, 950, 1000000000ull);
        CbDockLockAction aexplicit = cexplicit.decide(true, -52.2, 5.0, 950, 1000000000ull);
        CHECK(adef.apply == aexplicit.apply && adef.new_delay_ms == aexplicit.new_delay_ms,
              "default-constructed CbDockLockCorrector must match the explicit-args constructor (dock() parity)");
    }

    // (12) #926 fix-up finding 1/7: RollingOffsetCluster::rebase() shifts every retained sample
    // immediately, so the window reflects the post-correction state right away.
    {
        RollingOffsetCluster c = RollingOffsetCluster::dock();
        uint64_t t = 0;
        CbAvOffset last;
        last.ok = false;
        for (int i = 0; i < (int)(CB_CLUSTER_MIN_MATCHED + 4); i++) {
            t += 100000000ull;
            last = c.push(t, -52.2);
        }
        CHECK(last.ok && std::fabs(last.offset_ms - (-52.2)) < 1e-9, "locked at -52.2ms before rebase");
        CHECK(last.mad_ms < 1.0, "tight cluster before rebase");

        c.rebase(-53.0); // matches the closed-form single-shot correction (delta = floor(-52.2) = -53)

        t += 100000000ull;
        CbAvOffset after = c.push(t, 0.8); // the SAME already-shifted value a fresh marker would read
        CHECK(after.ok && std::fabs(after.offset_ms - 0.8) < 1e-6,
              "rebase must shift retained samples so the window reads the post-correction value");
        CHECK(after.mad_ms < 1.0, "rebasing must not inflate dispersion (finding 7)");
    }

    if (g_failures == 0) {
        std::printf("av_sync_dock_lock_926: ALL PASS\n");
        return 0;
    }
    std::printf("av_sync_dock_lock_926: %d FAILURE(S)\n", g_failures);
    return 1;
}
"#;

#[test]
fn cb_dock_lock_corrector_never_leaves_audio_early() {
    let src_dir = manifest_dir().join("vendor/av-sync-dock/src");
    let header = src_dir.join("camera-box-audio.hpp");
    assert!(
        header.exists(),
        "#926: missing pure audio header {} — the A/V-sync dock's lock-correction logic must \
         live there so it stays unit-testable off-rig.",
        header.display()
    );

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = std::env::temp_dir().join(format!(
        "av_sync_dock_lock_926_{}_{}",
        std::process::id(),
        stamp
    ));
    std::fs::create_dir_all(&work).expect("create temp workdir");
    let src = work.join("harness.cpp");
    let bin = work.join("harness");
    std::fs::write(&src, HARNESS_CPP).expect("write C++ harness");

    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".to_string());
    let compile = Command::new(&cxx)
        .arg("-std=c++17")
        .arg("-Wall")
        .arg("-I")
        .arg(&src_dir)
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| panic!("failed to invoke C++ compiler '{cxx}': {e}"));
    assert!(
        compile.status.success(),
        "#926: camera-box-audio.hpp (+ CbDockLockCorrector) failed to compile with {cxx}:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin).output().expect("run compiled harness");
    let _ = std::fs::remove_dir_all(&work); // best-effort cleanup
    assert!(
        run.status.success(),
        "#926: CbDockLockCorrector regression harness FAILED (exit {:?}).\nstdout: {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}
