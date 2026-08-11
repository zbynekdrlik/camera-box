//! #1005 — corrected_video_ts validity predicate in the C++ mirror.
//!
//! `RollingOffsetCluster`'s two camera-box emit sites in `sync-test-output.cpp` used to CLAMP a
//! negative corrected video timestamp to 0 instead of dropping the event, silently manufacturing
//! a garbage whole-timeline-scale `sync_found` offset. `cb_corrected_video_ts_is_valid`
//! (`camera-box-audio.hpp`) is the pure boundary predicate both call sites now consult — mirrors
//! `src/av_sync_dock.rs::corrected_video_ts_is_valid` byte-for-byte (see that module's own
//! `corrected_video_ts_is_valid_accepts_only_strictly_positive_1005` test, the Rust-side proof of
//! the identical boundary).
//!
//! RED (before the fix): `cb_corrected_video_ts_is_valid` does not exist in
//! camera-box-audio.hpp -> this harness fails to COMPILE. GREEN (after the fix): it compiles and
//! every boundary assertion below passes.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const HARNESS_CPP: &str = r##"
#include "camera-box-audio.hpp"
#include <cstdio>
#include <cstdint>
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
    // Preserves the OLD clamp's own boundary exactly (`> 0` was always the "keep as-is" side).
    CHECK(cb_corrected_video_ts_is_valid(1), "a small positive value must be valid");
    CHECK(cb_corrected_video_ts_is_valid(1000000), "an ordinary positive value must be valid");
    CHECK(cb_corrected_video_ts_is_valid(std::numeric_limits<int64_t>::max()),
          "INT64_MAX must be valid");
    CHECK(!cb_corrected_video_ts_is_valid(0), "exactly zero must be invalid (the old clamp target)");
    CHECK(!cb_corrected_video_ts_is_valid(-1), "a small negative value must be invalid");
    CHECK(!cb_corrected_video_ts_is_valid(-1000000), "an ordinary negative value must be invalid");
    CHECK(!cb_corrected_video_ts_is_valid(std::numeric_limits<int64_t>::min()),
          "INT64_MIN must be invalid");

    if (g_failures == 0) {
        std::printf("av_sync_dock_video_ts_valid_1005: ALL PASS\n");
        return 0;
    }
    std::printf("av_sync_dock_video_ts_valid_1005: %d FAILURE(S)\n", g_failures);
    return 1;
}
"##;

#[test]
fn cb_corrected_video_ts_is_valid_matches_the_rust_boundary_1005() {
    let src_dir = manifest_dir().join("vendor/av-sync-dock/src");
    let header = src_dir.join("camera-box-audio.hpp");
    assert!(
        header.exists(),
        "#1005: missing pure audio header {} — the A/V-sync dock's cluster/lock logic must live \
         there so it stays unit-testable off-rig.",
        header.display()
    );

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = std::env::temp_dir().join(format!(
        "av_sync_dock_video_ts_valid_1005_{}_{}",
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
        "#1005: camera-box-audio.hpp (cb_corrected_video_ts_is_valid) failed to compile with \
         {cxx}:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin).output().expect("run compiled harness");
    let _ = std::fs::remove_dir_all(&work); // best-effort cleanup
    assert!(
        run.status.success(),
        "#1005: cb_corrected_video_ts_is_valid boundary harness FAILED (exit {:?}).\nstdout: \
         {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}
