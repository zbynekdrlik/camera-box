//! #955 — extract the dock-lock Write/Suggest/RailWarn/Quiet decision into a pure, testable seam.
//!
//! `sync-test-output.cpp` (the OBS glue, compiled ONLY on the 150-min windows-genlock.yml, invisible
//! to normal PR CI) has always derived what to log/write from a `CbDockLockAction` inline, as an
//! if/else-if/else chain: `act.apply && may_actuate` -> write; `act.apply && !may_actuate` ->
//! monitor-only suggestion (#942); `!act.apply && offset_ms < 0 && pinned at a rail` -> a rail-pinned
//! warning (#926 fix-up finding 9); otherwise nothing. Every existing regression guard on this chain
//! (`tests/genlock_preload.rs`'s vendored-source checks) is a source-TEXT grep — the #942 fix-up
//! review's own counter-example (moving the actuator write into the monitor-only branch) proved a
//! text anchor can be silently defeated by a real regression.
//!
//! Fix: `camerabox::CbDockLockOutcome cb_dock_lock_outcome(act, may_actuate, offset_ms, current_ms)`
//! (camera-box-audio.hpp) is a byte-identical pure extraction of that SAME chain — mirrors
//! `src/av_sync_dock.rs::DockLockOutcome`/`dock_lock_outcome()`. `decide()` itself and its hold-band
//! math are UNCHANGED (`.claude/rules/dock-lock-hold-band.md`); only the branch NAME is extracted.
//! Same twin-harness pattern as `tests/av_sync_dock_audit_log.rs` (#634) and
//! `tests/av_sync_dock_lock_926.rs` (#926) — compile+run a tiny real C++ program against the real
//! production header, off-rig, no vendored-OBS build needed.
//!
//! RED (before the fix): `CbDockLockOutcome`/`cb_dock_lock_outcome` do not exist in
//! camera-box-audio.hpp -> this harness fails to COMPILE. GREEN (after the fix): it compiles and
//! every scenario below matches the SAME cases `src/av_sync_dock.rs`'s own unit tests pin.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const HARNESS_CPP: &str = r#"
#include "camera-box-audio.hpp"
#include <cstdio>

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
    // (1) Apply + may_actuate=true -> Write.
    {
        CbDockLockAction act;
        act.apply = true;
        act.new_delay_ms = 945;
        CHECK(cb_dock_lock_outcome(act, true, -52.2, 950) == CbDockLockOutcome::Write,
              "Apply + may_actuate=true must be Write");
    }

    // (2) Apply + may_actuate=false -> Suggest (#942 monitor-only).
    {
        CbDockLockAction act;
        act.apply = true;
        act.new_delay_ms = 945;
        CHECK(cb_dock_lock_outcome(act, false, -52.2, 950) == CbDockLockOutcome::Suggest,
              "Apply + may_actuate=false must be Suggest");
    }

    // (3) Hold + audio still early (offset<0) + pinned at a hardware rail -> RailWarn.
    {
        CbDockLockAction hold;
        CHECK(cb_dock_lock_outcome(hold, false, -20.0, CB_DOCK_LOCK_LATENCY_MIN_MS) ==
                      CbDockLockOutcome::RailWarn,
              "Hold + offset<0 + pinned at floor must be RailWarn");
        CHECK(cb_dock_lock_outcome(hold, false, -20.0, CB_DOCK_LOCK_LATENCY_MAX_MS) ==
                      CbDockLockOutcome::RailWarn,
              "Hold + offset<0 + pinned at ceiling must be RailWarn");
    }

    // (4) Hold + not pinned -> Quiet. A NON-negative offset never triggers RailWarn even while
    // pinned at a rail -- the check is specifically "audio still EARLY".
    {
        CbDockLockAction hold;
        CHECK(cb_dock_lock_outcome(hold, false, 5.0, 950) == CbDockLockOutcome::Quiet,
              "Hold + not pinned must be Quiet");
        CHECK(cb_dock_lock_outcome(hold, false, 5.0, CB_DOCK_LOCK_LATENCY_MIN_MS) ==
                      CbDockLockOutcome::Quiet,
              "Hold + offset>=0 + pinned at a rail must still be Quiet (not RailWarn)");
    }

    // (5) may_actuate is irrelevant on the Hold arm.
    {
        CbDockLockAction hold;
        CHECK(cb_dock_lock_outcome(hold, true, 5.0, 950) == CbDockLockOutcome::Quiet,
              "may_actuate must not affect the Hold arm's outcome");
    }

    std::printf("av_sync_dock_outcome_955: %d FAILURE(S)\n", g_failures);
    return g_failures == 0 ? 0 : 1;
}
"#;

#[test]
fn cb_dock_lock_outcome_mirrors_the_rust_branch_selection_byte_identically() {
    let src_dir = manifest_dir().join("vendor/av-sync-dock/src");
    let header = src_dir.join("camera-box-audio.hpp");
    assert!(
        header.exists(),
        "#955: missing pure audio header {} — the dock-lock outcome decision must live there so \
         it stays unit-testable off-rig.",
        header.display()
    );

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = std::env::temp_dir().join(format!(
        "av_sync_dock_outcome_955_{}_{}",
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
        "#955: camera-box-audio.hpp (+ CbDockLockOutcome/cb_dock_lock_outcome) failed to compile \
         with {cxx}:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin).output().expect("run compiled harness");
    let _ = std::fs::remove_dir_all(&work); // best-effort cleanup
    assert!(
        run.status.success(),
        "#955: cb_dock_lock_outcome regression harness FAILED (exit {:?}).\nstdout: {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}
