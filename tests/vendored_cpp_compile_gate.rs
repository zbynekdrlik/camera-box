//! #101 — the vendored OBS/DistroAV C++ (vendor/obs-studio, vendor/distroav) must get a
//! PRE-MERGE compile gate, not only a post-merge production build.
//!
//! Background: the per-PR `ci.yml` is Rust-only (cargo), and this repo fires CI on
//! `push:[dev,main]` (deliberately NOT `pull_request`, per #157 single-fire), so the
//! push-to-dev run IS the pre-merge gate. A C++ compile error in the vendored tree
//! therefore passed every PR gate and reached main, where only the manual 150-min
//! Windows genlock build caught it (#97's `genlock_source_drop_cap()` used ~700 lines
//! before its definition → MSVC C2220; all PR checks were green, the manual build failed).
//!
//! #101 first closed the gap with a DistroAV compile-check in the Windows fast workflow. The
//! Windows genlock workflows were retired by issue 1505 (the Windows boxes run the pinned obs
//! release), so the gate that remains is `linux-genlock.yml`'s `linux-distroav-compile-check`
//! job: on push to dev when `vendor/distroav/**` (or the vendored OBS) changes, it builds
//! libobs, installs it as an SDK prefix and builds the DistroAV plugin against it, with
//! ENABLE_BROWSER OFF so it stays minutes-not-hours.
//!
//! These are STRUCTURAL guards on the workflow YAML: they fail loudly if a future edit drops
//! the DistroAV compile job, its trigger path, or the configure/build/install plumbing it
//! needs. The DEFINITIVE end-to-end proof is the CI job itself compiling the vendored C++.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

const LINUX: &str = ".github/workflows/linux-genlock.yml";
const JOB: &str = "\n  linux-distroav-compile-check:\n";

/// The `linux-distroav-compile-check` job's text: from its key to the next top-level job key
/// (a line indented by exactly two spaces that ends in `:`).
fn compile_job() -> String {
    let wf = read(LINUX);
    let start = wf
        .find(JOB)
        .unwrap_or_else(|| panic!("#101: {LINUX} lost its linux-distroav-compile-check job"));
    let body = &wf[start + JOB.len()..];
    let mut end = body.len();
    let mut at = 0;
    for line in body.split_inclusive('\n') {
        let key = line.trim_end();
        if key.len() > 2
            && key.starts_with("  ")
            && !key.starts_with("   ")
            && !key.trim_start().starts_with('#')
            && key.ends_with(':')
        {
            end = at;
            break;
        }
        at += line.len();
    }
    wf[start..start + JOB.len() + end].to_string()
}

/// The pre-merge workflow must carry a DistroAV compile-check job (#101). We assert by the
/// build target the job must run.
#[test]
fn compile_job_builds_the_distroav_plugin() {
    let wf = compile_job();
    assert!(
        wf.contains("vendor/distroav"),
        "#101: {LINUX}'s compile-check job must reference vendor/distroav — a DistroAV \
         compile-check is required so a C++ error in ndi-*.cpp / the burn filter is caught \
         pre-merge, not only by a post-merge production build."
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
         links against the genlock-patched libobs."
    );
    // It must actually BUILD DistroAV in the distroav working directory (that is the C++
    // compile that catches the error).
    assert!(
        wf.contains("working-directory: vendor/distroav"),
        "#101: the gate must run a cmake build with working-directory: vendor/distroav — \
         that build is what compiles the vendored DistroAV C++."
    );
}

/// Speed budget: the compile gate is a CHECK, not the full production bundle. It must turn
/// OFF the CEF/browser dependency (the bulk of a full build) so it stays in the
/// minutes-not-hours window.
#[test]
fn compile_job_disables_browser_for_speed() {
    let wf = compile_job();
    assert!(
        wf.contains("-DENABLE_BROWSER=OFF"),
        "#101: the compile gate must configure OBS with -DENABLE_BROWSER=OFF — the CEF/browser \
         download+build is the bulk of a full build and is not needed to compile-check the \
         vendored C++. Keep the gate fast (compile-check, not full bundle)."
    );
}

/// The gate must FIRE on a push that touches the vendored DistroAV tree, so a DistroAV C++
/// change is actually compile-checked before it can be merged to main (#101).
#[test]
fn linux_workflow_triggers_on_distroav_changes() {
    let wf = read(LINUX);
    // The push-path filter must include the DistroAV tree.
    assert!(
        wf.contains("vendor/distroav/**"),
        "#101: {LINUX} `on.push.paths` must include `vendor/distroav/**` so a DistroAV-only C++ \
         change triggers the compile gate."
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
