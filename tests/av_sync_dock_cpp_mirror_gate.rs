//! #398 — permanent gate: the vendored dock's C++ decode logic MUST match the proven Rust.
//!
//! The A/V-sync dock's audio decode (`camera-box-audio.hpp`) and video-QR geometry
//! (`camera-box-video.hpp`) are byte-for-byte C++ MIRRORS of the Tier-0-tested Rust in
//! `src/av_sync_dock.rs` / `src/qpsk_marker.rs` — because the deployed dock cannot be rig-verified in
//! CI and the norihiro demod it replaces was silently broken at the rig's c=1 (the #398 bug). Those
//! headers are dependency-free (STL only), so this test COMPILES AND RUNS the committed self-test
//! (`vendor/av-sync-dock/test/camera-box-selftest.cpp`) with `g++ -std=c++11` on every push: it
//! round-trips all 256 QPSK indices through the C++ `cb_decode_markers` from golden emitter output,
//! and cross-checks the streaming decoder, rolling cluster, top-band geometry, Otsu, and downscale
//! against the SAME results the Rust proves. A drift between the C++ the dock ships and the Rust
//! decode fails HERE, in the fast Linux CI, not silently on the rig.
//!
//! (The full OBS/quirc plugin build is the separate `windows-genlock-fast.yml` compile gate; this
//! gate is specifically the pure-logic equivalence the plugin build cannot check.)

use std::path::PathBuf;
use std::process::Command;

fn manifest(rel: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), rel].iter().collect()
}

#[test]
fn cpp_dock_decode_mirror_matches_rust() {
    let src = manifest("vendor/av-sync-dock/test/camera-box-selftest.cpp");
    assert!(
        src.exists(),
        "the C++ mirror self-test must exist: {}",
        src.display()
    );

    // Compile the dependency-free self-test (pulls in camera-box-audio.hpp + camera-box-video.hpp).
    let out_bin = std::env::temp_dir().join(format!(
        "camera-box-selftest-{}-{}",
        std::process::id(),
        env!("CARGO_PKG_VERSION")
    ));
    let compile = Command::new("g++")
        .args(["-std=c++11", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(&src)
        .arg("-o")
        .arg(&out_bin)
        .output()
        .expect("spawn g++ (install build-essential) — the C++ mirror gate needs a C++11 compiler");
    assert!(
        compile.status.success(),
        "the dock C++ mirror headers must compile clean (-std=c++11 -Wall -Wextra -Werror):\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // Run it: exit 0 + "ALL PASS" means the C++ decode logic reproduces the Rust results exactly.
    let run = Command::new(&out_bin)
        .output()
        .expect("run the compiled camera-box self-test");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let _ = std::fs::remove_file(&out_bin);
    assert!(
        run.status.success() && stdout.contains("ALL PASS"),
        "the dock C++ decode mirror must match the Rust (all 256 QPSK indices round-trip + \
         cluster/geometry/otsu/downscale cross-checks). Output:\n{stdout}{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
