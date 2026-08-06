//! #953 — the dock's DISPLAYED "SUGGESTED" advice must target true alignment (offset -> 0, in the
//! gate's own convention), never the actuator-era 1.5*mad resting place, and never a meaningless
//! +/-5ms step.
//!
//! #952 (closed, binding) established that the dock's own native offset convention
//! (`ts = audio_ts - video_ts`) is sign-inverted versus the gate's authoritative convention
//! (`offset_ms = video_time - audio_time`, `scripts/av_sync_calibrate.py::required_delay_ms` /
//! `src/qpsk_marker.rs::required_delay_ms`) — `dock ~= -gate - 55` empirically. This ticket fixes
//! the SIGN half (a pure negation, `cb_dock_lock_display_offset_ms`) and replaces the SUGGESTED
//! value with a fresh, uncapped alignment-target computation (`cb_dock_lock_suggested_target`) —
//! `CbDockLockCorrector::decide()` itself (its hold-band/step/cooldown math,
//! `.claude/rules/dock-lock-hold-band.md`) is UNTOUCHED. Live evidence this fixes: the SUGGESTED
//! log line was a CONSTANT "current - 5" for measured offsets spanning -3.5 to -101.4ms (#953
//! comment 2026-08-05), because it reused `decide()`'s own `CB_DOCK_LOCK_MAX_STEP_MS`-capped
//! `act.new_delay_ms` — a limiter that only makes sense for a closed-loop actuator that converges
//! over many ticks, meaningless for a value nothing ever applies (#942).
//!
//! Same twin-harness pattern as `tests/av_sync_dock_audit_log.rs` (#634) /
//! `tests/av_sync_dock_lock_926.rs` (#926) / `tests/av_sync_dock_outcome_955.rs` (#955) — compile +
//! run a tiny real C++ program against the real production header, off-rig.
//!
//! RED (before the fix): `cb_dock_lock_display_offset_ms`/`CbDockLockSuggestion`/
//! `cb_dock_lock_suggested_target` do not exist in camera-box-audio.hpp -> fails to COMPILE.
//! GREEN: it compiles and every scenario matches `src/av_sync_dock.rs`'s own unit tests.

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
    // (1) Sign fix: cb_dock_lock_display_offset_ms is a pure negation, dock -> gate convention.
    CHECK(cb_dock_lock_display_offset_ms(-55.0) == 55.0, "must negate -55 -> 55");
    CHECK(cb_dock_lock_display_offset_ms(30.0) == -30.0, "must negate 30 -> -30");
    CHECK(cb_dock_lock_display_offset_ms(0.0) == 0.0, "must negate 0 -> 0");

    // (2) Quiet inside the noise band -- NOT the pre-#953 constant "-5ms" bug.
    {
        CbDockLockSuggestion s1 = cb_dock_lock_suggested_target(3.0, 10.0, 931);
        CHECK(!s1.has_value, "offset=3.0 inside margin=10.0 must be quiet");
        CbDockLockSuggestion s2 = cb_dock_lock_suggested_target(-9.9, 10.0, 931);
        CHECK(!s2.has_value, "offset=-9.9 inside margin=10.0 must be quiet");
    }

    // (3) #953's own live evidence: gate-convention offset -55 at current=931 must target FULL
    // alignment (931 - (-55) = 986), never a +/-5ms step and never the 1.5*mad resting place.
    {
        CbDockLockSuggestion s = cb_dock_lock_suggested_target(-55.0, 24.6, 931);
        CHECK(s.has_value && s.target_ms == 986, "must target true alignment (986), not a 5ms step");
    }

    // (4) Direction matches the gate's own required_delay_ms convention: positive (video lags) ->
    // REDUCE the delay; negative (video leads) -> INCREASE it.
    {
        CbDockLockSuggestion s1 = cb_dock_lock_suggested_target(30.0, 5.0, 1000);
        CHECK(s1.has_value && s1.target_ms == 970, "positive offset must reduce the delay");
        CbDockLockSuggestion s2 = cb_dock_lock_suggested_target(-30.0, 5.0, 1000);
        CHECK(s2.has_value && s2.target_ms == 1030, "negative offset must increase the delay");
    }

    // (5) Clamps to the DistroAV hardware rails.
    {
        CbDockLockSuggestion s1 = cb_dock_lock_suggested_target(5000.0, 5.0, 10);
        CHECK(s1.has_value && s1.target_ms == CB_DOCK_LOCK_LATENCY_MIN_MS, "must clamp at the floor");
        CbDockLockSuggestion s2 = cb_dock_lock_suggested_target(-5000.0, 5.0, 1990);
        CHECK(s2.has_value && s2.target_ms == CB_DOCK_LOCK_LATENCY_MAX_MS, "must clamp at the ceiling");
    }

    // (6) Non-finite offset is always quiet, never UB.
    {
        double bad[] = {std::nan(""), std::numeric_limits<double>::infinity(),
                        -std::numeric_limits<double>::infinity()};
        for (size_t i = 0; i < sizeof(bad) / sizeof(bad[0]); i++) {
            CbDockLockSuggestion s = cb_dock_lock_suggested_target(bad[i], 10.0, 931);
            CHECK(!s.has_value, "non-finite offset must always be quiet");
        }
    }

    // (7) The quiet check is a STRICT `<` -- an offset exactly AT the margin is already outside
    // the noise band and must produce a real suggestion, not be swallowed as "aligned".
    {
        CbDockLockSuggestion s1 = cb_dock_lock_suggested_target(10.0, 10.0, 931);
        CHECK(s1.has_value && s1.target_ms == 921, "offset==margin must not be quiet (positive)");
        CbDockLockSuggestion s2 = cb_dock_lock_suggested_target(-10.0, 10.0, 931);
        CHECK(s2.has_value && s2.target_ms == 941, "offset==margin must not be quiet (negative)");
    }

    // (8) A non-finite mad_ms falls back to CB_DOCK_LOCK_MIN_MARGIN_MS (1.0), not "no noise
    // floor at all".
    {
        CbDockLockSuggestion s1 = cb_dock_lock_suggested_target(0.5, std::nan(""), 931);
        CHECK(!s1.has_value, "offset inside the 1.0ms fallback margin must be quiet");
        CbDockLockSuggestion s2 = cb_dock_lock_suggested_target(5.0, std::nan(""), 931);
        CHECK(s2.has_value && s2.target_ms == 926, "offset outside the fallback margin must suggest");
    }

    std::printf("av_sync_dock_suggested_target_953: %d FAILURE(S)\n", g_failures);
    return g_failures == 0 ? 0 : 1;
}
"#;

#[test]
fn cb_dock_lock_suggested_target_matches_the_rust_alignment_formula() {
    let src_dir = manifest_dir().join("vendor/av-sync-dock/src");
    let header = src_dir.join("camera-box-audio.hpp");
    assert!(
        header.exists(),
        "#953: missing pure audio header {} — the dock-lock suggestion logic must live there so \
         it stays unit-testable off-rig.",
        header.display()
    );

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = std::env::temp_dir().join(format!(
        "av_sync_dock_suggested_target_953_{}_{}",
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
        "#953: camera-box-audio.hpp (+ cb_dock_lock_display_offset_ms/cb_dock_lock_suggested_target) \
         failed to compile with {cxx}:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin).output().expect("run compiled harness");
    let _ = std::fs::remove_dir_all(&work); // best-effort cleanup
    assert!(
        run.status.success(),
        "#953: cb_dock_lock_suggested_target regression harness FAILED (exit {:?}).\nstdout: {}\n\
         stderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}
