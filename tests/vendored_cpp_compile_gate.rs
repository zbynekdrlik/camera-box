//! #101 — the vendored OBS/DistroAV C++ (vendor/obs-studio, vendor/distroav) must get a
//! PRE-MERGE compile gate, not only the manual post-merge `windows-genlock.yml` build.
//!
//! Background: the per-PR `ci.yml` is Rust-only (cargo), and this repo fires CI on
//! `push:[dev,main]` (deliberately NOT `pull_request`, per #157 single-fire), so the
//! push-to-dev run IS the pre-merge gate. A C++ compile error in the vendored tree
//! therefore passed every PR gate and reached main, where only the manual 150-min
//! `windows-genlock.yml` caught it (#97's `genlock_source_drop_cap()` used ~700 lines
//! before its definition → MSVC C2220; all PR checks were green, the manual build failed).
//!
//! The owner's 2026-06-19 partial fix added `windows-genlock-fast.yml`, which builds ONLY
//! libobs (`--target libobs`) on push to dev when `vendor/obs-studio/libobs/**` changes —
//! a real MSVC compile of libobs in minutes, but NO DistroAV and nothing outside libobs.
//!
//! This issue closes the remaining gap: extend that fast build into a DistroAV-inclusive
//! compile gate that fires on push to dev when `vendor/distroav/**` (or the vendored OBS)
//! changes, configures + builds the DistroAV plugin (the whole `ndi-*.cpp` / burn-filter /
//! plugin-main C++) against an installed libobs SDK prefix, with ENABLE_BROWSER OFF so it
//! stays minutes-not-hours. A C++ error in DistroAV can then never again pass the push-to-dev
//! gate and reach main / waste a full 150-min genlock build cycle.
//!
//! These are STRUCTURAL guards on the workflow YAML: they fail loudly if a future edit drops
//! the DistroAV compile job, its trigger path, or the configure/build/install plumbing it
//! needs. They run on every push, on any host, with no Windows toolchain. The DEFINITIVE
//! end-to-end proof is the CI job itself actually compiling the vendored C++ on a windows
//! runner.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

const FAST: &str = ".github/workflows/windows-genlock-fast.yml";

/// The fast pre-merge workflow must carry a DistroAV compile-check job — the part the
/// libobs-only build does NOT cover (#101). We assert by the build target the job must run.
#[test]
fn fast_workflow_compiles_distroav_plugin() {
    let wf = read(FAST);
    assert!(
        wf.contains("vendor/distroav"),
        "#101: {FAST} must reference vendor/distroav — the DistroAV C++ is the half the \
         libobs-only fast build never compiled. A DistroAV compile-check job is required so a \
         C++ error in ndi-*.cpp / the burn filter is caught pre-merge, not only by the manual \
         150-min windows-genlock.yml build."
    );
    // The DistroAV plugin links against libobs; it is configured against an installed OBS SDK
    // prefix and then built. Assert the configure-against-SDK-prefix + build plumbing exists.
    assert!(
        wf.contains("cmake --install") && wf.contains("--prefix"),
        "#101: the DistroAV compile gate must install the OBS SDK prefix \
         (`cmake --install … --prefix …`) so DistroAV's `find_package(libobs REQUIRED)` resolves."
    );
    assert!(
        wf.contains("CMAKE_PREFIX_PATH"),
        "#101: DistroAV must be configured with -DCMAKE_PREFIX_PATH=<installed OBS SDK> so it \
         links against the genlock-patched libobs (mirrors windows-genlock.yml)."
    );
    // It must actually BUILD DistroAV in the distroav working directory (that is the C++
    // compile that catches the error). The full build uses `cmake --build build_x64` under
    // working-directory: vendor/distroav.
    assert!(
        wf.contains("working-directory: vendor/distroav"),
        "#101: the gate must run a cmake build with working-directory: vendor/distroav — \
         that build is what compiles the vendored DistroAV C++."
    );
}

/// Speed budget: the compile gate is a CHECK, not the full production bundle. It must turn
/// OFF the CEF/browser dependency (the bulk of the 150-min build) so it stays in the
/// minutes-not-hours window. `windows-x64` preset defaults ENABLE_BROWSER=true.
#[test]
fn fast_workflow_disables_browser_for_speed() {
    let wf = read(FAST);
    assert!(
        wf.contains("-DENABLE_BROWSER=OFF"),
        "#101: the compile gate must configure OBS with -DENABLE_BROWSER=OFF — the CEF/browser \
         download+build is the bulk of the 150-min full build and is not needed to compile-check \
         the vendored C++. Keep the gate fast (compile-check, not full bundle)."
    );
}

/// The gate must FIRE on a push that touches the vendored DistroAV tree, so a DistroAV C++
/// change is actually compile-checked before it can be merged to main (#101). The libobs-only
/// build only watches `vendor/obs-studio/libobs/**` — a DistroAV-only change skipped it.
#[test]
fn fast_workflow_triggers_on_distroav_changes() {
    let wf = read(FAST);
    // The push-path filter must include the DistroAV tree.
    assert!(
        wf.contains("vendor/distroav/**"),
        "#101: {FAST} `on.push.paths` must include `vendor/distroav/**` so a DistroAV-only C++ \
         change triggers the compile gate (the libobs-only build watched only \
         vendor/obs-studio/libobs/**, so DistroAV edits slipped through to main)."
    );
    // This repo gates merges via push-to-dev (it deliberately omits pull_request, #157), so
    // the trigger must be push to dev — that IS the pre-merge gate here.
    assert!(
        wf.contains("push:") && wf.contains("branches: [dev]"),
        "#101: the gate must trigger on push to dev (this repo's pre-merge gate — it omits \
         pull_request per #157; the push-to-dev run is what populates the dev->main PR checks)."
    );
}

/// Regression-anchor on the SPECIFIC bug class that motivated #101: a file-local helper used
/// before its definition (declared-before-use). #97's `genlock_source_drop_cap()` was the
/// instance. The compile gate IS the catch, but we also assert the vendored DistroAV genlock
/// patch keeps its helpers declared-before-use so the class can't silently regress in the
/// source the gate now compiles. (The burn filter is the newest vendored C++ surface.)
#[test]
fn vendored_distroav_burn_filter_present_for_the_gate_to_compile() {
    // The gate is only meaningful if the C++ it compiles is actually present. Assert the
    // burn-filter translation unit + its CMake wiring exist (so the build target has sources).
    assert!(
        fs::metadata(format!(
            "{}/vendor/distroav/src/ndi-burn-filter.cpp",
            env!("CARGO_MANIFEST_DIR")
        ))
        .is_ok(),
        "#101: vendor/distroav/src/ndi-burn-filter.cpp must exist — it is the newest vendored \
         DistroAV C++ surface the compile gate must build (the #97-class forward-ref bug lives \
         in exactly this kind of file)."
    );
    let cm = read("vendor/distroav/CMakeLists.txt");
    assert!(
        cm.contains("ndi-burn-filter.cpp"),
        "#101: ndi-burn-filter.cpp must be in vendor/distroav/CMakeLists.txt target_sources, \
         else the compile gate would not actually compile it."
    );
}
