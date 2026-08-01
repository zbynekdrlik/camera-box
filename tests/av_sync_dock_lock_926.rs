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
        CbDockLockAction a = c.decide(false, -52.2, 950, 1000000000ull);
        CHECK(!a.apply, "unlocked must always Hold, regardless of how wrong the last offset was");
    }

    // (2) Already in the never-early zone ([0,1)) -- both boundary-ish values Hold.
    {
        CbDockLockCorrector c;
        CHECK(!c.decide(true, 0.0, 950, 1000000000ull).apply, "ts=0.0 already converged");
        CbDockLockCorrector c2;
        CHECK(!c2.decide(true, 0.9, 950, 1000000000ull).apply, "ts=0.9 already converged");
    }

    // (3) A large audio-early error, step-clamped to the 5ms default budget, moves in the
    // CORRECT direction (reduce the delay) by exactly the step, never overshoots.
    {
        CbDockLockCorrector c;
        CbDockLockAction a = c.decide(true, -52.2, 950, 1000000000ull);
        CHECK(a.apply, "a -52.2ms error must trigger a correction");
        CHECK(a.new_delay_ms == 945, "must reduce by exactly the 5ms step budget");
    }

    // (4) Hardware floor: wants to go below 3ms -> clamps to exactly 3.
    {
        CbDockLockCorrector c(50, 30.0);
        CbDockLockAction a = c.decide(true, -10.0, 5, 1000000000ull);
        CHECK(a.apply && a.new_delay_ms == 3, "must clamp at the hardware floor");
    }
    // Already pinned at the floor -- Hold, not a pointless re-write of the same value.
    {
        CbDockLockCorrector c(50, 30.0);
        CbDockLockAction a = c.decide(true, -10.0, 3, 1000000000ull);
        CHECK(!a.apply, "already at the floor with no room to correct further must Hold");
    }

    // (5) Hardware ceiling: wants to go above 2000ms -> clamps to exactly 2000.
    {
        CbDockLockCorrector c(50, 30.0);
        CbDockLockAction a = c.decide(true, 10.0, 1998, 1000000000ull);
        CHECK(a.apply && a.new_delay_ms == 2000, "must clamp at the hardware ceiling");
    }
    {
        CbDockLockCorrector c(50, 30.0);
        CbDockLockAction a = c.decide(true, 10.0, 2000, 1000000000ull);
        CHECK(!a.apply, "already at the ceiling with no room to correct further must Hold");
    }

    // (6) Cooldown: a second correction within min_reapply_s of the first must Hold; after the
    // cooldown elapses (measured from the LAST APPLIED write) it applies again.
    {
        CbDockLockCorrector c(5, 30.0);
        CbDockLockAction a1 = c.decide(true, -52.2, 950, 1000000000ull); // t=1s
        CHECK(a1.apply, "first correction must apply");
        CbDockLockAction a2 = c.decide(true, -47.2, 945, 11000000000ull); // t=11s (10s later)
        CHECK(!a2.apply, "within the 30s cooldown must Hold, even though further correction is due");
        CbDockLockAction a3 = c.decide(true, -47.2, 945, 32000000000ull); // t=32s (31s after a1)
        CHECK(a3.apply, "cooldown elapsed -- must apply again");
    }

    // (7) Excess audio-lateness (not forbidden, but not minimal either) is nudged back toward 0.
    {
        CbDockLockCorrector c(50, 30.0);
        CbDockLockAction a = c.decide(true, 42.0, 950, 1000000000ull);
        CHECK(a.apply && a.new_delay_ms == 992, "42ms of excess audio-lateness must be reduced");
    }

    // (8) The never-early invariant, swept across a range of offsets/current-delays with an
    // effectively-unclamped step budget, mirroring the Rust property test.
    {
        double offsets[] = {-523.7, -100.0, -52.2, -10.4, -1.0, -0.1, 0.0, 0.5, 3.3, 10.0, 42.9, 100.0, 900.0};
        int32_t currents[] = {3, 50, 500, 950, 1000, 1500, 1999, 2000};
        for (size_t oi = 0; oi < sizeof(offsets) / sizeof(offsets[0]); oi++) {
            for (size_t ci = 0; ci < sizeof(currents) / sizeof(currents[0]); ci++) {
                CbDockLockCorrector c(100000, 30.0);
                CbDockLockAction a = c.decide(true, offsets[oi], currents[ci], 1000000000ull);
                int32_t new_delay = a.apply ? a.new_delay_ms : currents[ci];
                bool hit_rail = new_delay == 3 || new_delay == 2000;
                if (!hit_rail) {
                    double delta_applied = (double)(new_delay - currents[ci]);
                    double ts_new = offsets[oi] - delta_applied;
                    CHECK(ts_new >= -1e-9 && ts_new < 1.0 + 1e-9,
                          "never-early invariant violated for some (offset, current) pair");
                }
            }
        }
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
