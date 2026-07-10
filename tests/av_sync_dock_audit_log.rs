//! #634 — audit logging for the A/V-sync dock's lock state (camera-box-audio.hpp).
//!
//! The #398 dock silently applies a new A/V-offset (`sync_found`/`audio_marker_found`) whenever
//! `RollingOffsetCluster::push()` returns a trusted (`ok=true`) cluster estimate, and silently
//! says nothing when the estimate is untrusted (`ok=false`) — there is NO log trail at all for
//! when the offset locks, unlocks, or is updated. That makes a live desync (like the closed
//! #529) undiagnosable from logs alone: an operator/agent reading the OBS log after the fact has
//! no record of what the dock believed the offset was, or when its lock state changed.
//!
//! Fix: `camerabox::CbLockAuditTracker` (camera-box-audio.hpp) is a pure, OBS/Qt-free state
//! machine that takes each `CbAvOffset` estimate the dock already computes and returns a
//! `CbLockAuditEvent` describing what (if anything) should be logged — a state transition
//! (`Locked`/`Unlocked`) or a meaningfully-changed offset while still locked (`Updated`), each
//! carrying the offset/matched/mad_ms that justify it (the "source of the value" #634 asks for).
//! The OBS-facing glue (sync-test-output.cpp) just calls `push()` and `blog()`s the result — kept
//! trivial per this repo's CLAUDE.md rule (the OBS frontend/dock only compiles on the 150-min
//! windows-genlock.yml, invisible to normal PR CI, so all non-trivial logic lives in this pure
//! header and is unit-tested off-rig via the same twin-harness pattern as
//! `tests/obs_titlebar_newlevel_parse.rs`).
//!
//! RED (before the fix): `CbLockAuditTracker` does not exist in camera-box-audio.hpp -> this
//! harness fails to COMPILE. GREEN (after the fix): it compiles and every transition assertion
//! below passes.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The C++ harness: includes the REAL production header and drives `CbLockAuditTracker` through
/// a sequence of `CbAvOffset` estimates covering every transition #634 asks the dock to log.
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

static CbAvOffset ok(double offset_ms, size_t matched, double mad_ms)
{
    CbAvOffset e;
    e.ok = true;
    e.offset_ms = offset_ms;
    e.matched = matched;
    e.mad_ms = mad_ms;
    return e;
}

static CbAvOffset none()
{
    CbAvOffset e;
    e.ok = false;
    e.offset_ms = 0.0;
    e.matched = 0;
    e.mad_ms = 0.0;
    return e;
}

int main()
{
    // (1) Never locks: every push is "still measuring" -> never anything to log.
    {
        CbLockAuditTracker t;
        for (int i = 0; i < 5; i++) {
            CbLockAuditEvent ev = t.push(none());
            CHECK(ev.kind == CbLockEventKind::None, "never-locked series must never emit an event");
        }
    }

    // (2) First trusted estimate -> Locked, carrying offset/matched/mad ("source of the value").
    {
        CbLockAuditTracker t;
        CbLockAuditEvent ev = t.push(ok(-70.2, 12, 8.5));
        CHECK(ev.kind == CbLockEventKind::Locked, "first ok=true estimate must be Locked");
        CHECK(std::fabs(ev.offset_ms - (-70.2)) < 1e-9, "Locked event must carry the offset");
        CHECK(ev.matched == 12, "Locked event must carry matched count");
        CHECK(std::fabs(ev.mad_ms - 8.5) < 1e-9, "Locked event must carry mad_ms");
    }

    // (3) Already locked, same offset within tolerance -> no repeat spam (None).
    {
        CbLockAuditTracker t(5.0);
        t.push(ok(-70.2, 12, 8.5));
        CbLockAuditEvent ev = t.push(ok(-71.0, 13, 8.1)); // moved 0.8ms, under 5ms tol
        CHECK(ev.kind == CbLockEventKind::None, "an unchanged-within-tolerance offset must not re-log");
    }

    // (4) Already locked, offset moves beyond tolerance -> Updated (the "new offset applied" case).
    {
        CbLockAuditTracker t(5.0);
        t.push(ok(-70.2, 12, 8.5));
        CbLockAuditEvent ev = t.push(ok(-40.0, 10, 9.0)); // moved 30.2ms, over 5ms tol
        CHECK(ev.kind == CbLockEventKind::Updated, "an offset change beyond tolerance must be Updated");
        CHECK(std::fabs(ev.offset_ms - (-40.0)) < 1e-9, "Updated event must carry the NEW offset");
    }

    // (5) Locked, then loses trust (ok=false) -> Unlocked, carrying the LAST known offset.
    {
        CbLockAuditTracker t;
        t.push(ok(12.5, 20, 4.0));
        CbLockAuditEvent ev = t.push(none());
        CHECK(ev.kind == CbLockEventKind::Unlocked, "ok flipping to false while locked must be Unlocked");
        CHECK(std::fabs(ev.offset_ms - 12.5) < 1e-9, "Unlocked event must carry the last known offset");
    }

    // (6) Unlocked stays quiet on subsequent still-untrusted pushes (no repeat Unlocked spam).
    {
        CbLockAuditTracker t;
        t.push(ok(12.5, 20, 4.0));
        t.push(none()); // Unlocked
        CbLockAuditEvent ev = t.push(none());
        CHECK(ev.kind == CbLockEventKind::None, "a second consecutive ok=false must not re-emit Unlocked");
    }

    // (7) Re-lock after an unlock is Locked again (not Updated — it's a fresh acquisition).
    {
        CbLockAuditTracker t;
        t.push(ok(12.5, 20, 4.0));
        t.push(none()); // Unlocked
        CbLockAuditEvent ev = t.push(ok(13.0, 9, 3.0));
        CHECK(ev.kind == CbLockEventKind::Locked, "re-acquiring lock after an unlock must be Locked");
    }

    if (g_failures == 0) {
        std::printf("av_sync_dock_audit_log: ALL PASS\n");
        return 0;
    }
    std::printf("av_sync_dock_audit_log: %d FAILURE(S)\n", g_failures);
    return 1;
}
"#;

#[test]
fn cb_lock_audit_tracker_logs_lock_unlock_and_offset_updates() {
    let src_dir = manifest_dir().join("vendor/av-sync-dock/src");
    let header = src_dir.join("camera-box-audio.hpp");
    assert!(
        header.exists(),
        "#634: missing pure audio header {} — the A/V-sync dock's cluster/lock logic must live \
         there so it stays unit-testable off-rig.",
        header.display()
    );

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = std::env::temp_dir().join(format!(
        "av_sync_dock_audit_log_{}_{}",
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
        "#634: camera-box-audio.hpp (+ CbLockAuditTracker) failed to compile with {cxx}:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin).output().expect("run compiled harness");
    let _ = std::fs::remove_dir_all(&work); // best-effort cleanup
    assert!(
        run.status.success(),
        "#634: CbLockAuditTracker regression harness FAILED (exit {:?}).\nstdout: {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}
