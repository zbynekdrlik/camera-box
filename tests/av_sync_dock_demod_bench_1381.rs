//! Issue 1381 — the av-sync dock's audio marker demod must be cheap enough to run beside OBS, and
//! the faster decode must report exactly what the old whole-window decode reported.
//!
//! Live 27.9.2026 the dock demodulated the resolume cg OBS program mix on libobs's audio thread,
//! re-decoding its whole 3-marker window on every 1024-frame push; with music on program the mixer
//! fell 13-22 s behind real time. `tests/c/av_sync_dock_demod_bench_1381.cpp` (the investigation
//! bench, promoted) checks the decoder against the pre-1381 whole-window decode on the marker
//! fixtures (every index, a rig-cadence track over silence and white noise, both channels of the
//! real stereo mbc fixture) and measures the worker's decode cost per push in thread CPU time. This
//! test compiles it with g++ and runs it on every CI run.
//!
//! Default features, no rig: g++ is the only tool.

use std::path::{Path, PathBuf};
use std::process::Command;

const BENCH: &str = "tests/c/av_sync_dock_demod_bench_1381.cpp";
const FIXTURE: &str = "tests/fixtures/mbc-stereo-skew-1367/mbc-stereo-2s.wav";

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// The compiled bench in its own temp dir, removed when the test ends, pass or panic.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: a temp directory that cannot be removed must not fail the test it served.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_dock_demod_matches_the_whole_window_decode_and_fits_its_budget() {
    let dir = std::env::temp_dir().join(format!(
        "av-sync-dock-demod-bench-1381-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create the scratch dir");
    let scratch = Scratch(dir);
    let bin = scratch.0.join("bench");
    let out = Command::new("g++")
        .args([
            "-std=c++11",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-pthread",
        ])
        .arg(format!("-I{}", repo("vendor/av-sync-dock/src").display()))
        .arg(format!("-I{}", repo("vendor/av-sync-dock/test").display()))
        .arg(repo(BENCH))
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn g++ (install build-essential) for the dock demod bench");
    assert!(
        out.status.success(),
        "the dock demod bench must compile clean:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .arg(repo(FIXTURE))
        .output()
        .expect("run the dock demod bench");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains("ALL PASS"),
        "the dock demod must report what the whole-window decode reported and stay within its \
         per-push budget (issue 1381). Output:\n{stdout}{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
