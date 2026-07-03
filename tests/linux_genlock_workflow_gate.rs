//! #460 — the vendored genlock OBS + DistroAV must build on LINUX (imag-nb, Ubuntu 24.04) too,
//! not just Windows (strih/stream). Research confirmed the patch surface is already Linux-ready
//! (every platform-specific genlock/DistroAV patch already carries a working non-Windows
//! fallback branch) — the missing piece was purely a CI workflow to prove it compiles and to
//! produce a deployable artifact.
//!
//! `.github/workflows/linux-genlock.yml` mirrors windows-genlock.yml's shape: a fast, narrowed
//! compile-check job (proves the vendored C/C++ compiles with gcc/g++ — the thing ci.yml's
//! Rust-only `test` job cannot do) plus the full production bundle job (real OBS + DistroAV,
//! staged + manifested + uploaded). Deploying that artifact to imag-nb is #458's remaining
//! scope, NOT this workflow's.
//!
//! These are STRUCTURAL guards on the workflow YAML — they fail loudly if a future edit drops a
//! job, its trigger path, or the configure/build/stage plumbing it needs. They run on every
//! push, on any host, with no OBS/DistroAV toolchain. The DEFINITIVE end-to-end proof is the CI
//! job itself actually compiling + staging the vendored C/C++ on a real ubuntu-24.04 runner.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

const WF: &str = ".github/workflows/linux-genlock.yml";

/// Both jobs must exist and run on ubuntu-24.04 — imag-nb's own distro (noble), so the build's
/// glibc/library ABI matches the deploy target #458 will swap the artifact onto.
#[test]
fn both_jobs_run_on_ubuntu_24_04() {
    let wf = read(WF);
    assert!(
        wf.contains("linux-distroav-compile-check:"),
        "#460: {WF} must define the linux-distroav-compile-check job — the fast pre-merge \
         compile gate for the vendored C/C++ on Linux."
    );
    assert!(
        wf.contains("linux-genlock-build:"),
        "#460: {WF} must define the linux-genlock-build job — the full production bundle."
    );
    assert_eq!(
        wf.matches("runs-on: ubuntu-24.04").count(),
        2,
        "#460: both jobs must pin runs-on: ubuntu-24.04 (imag-nb's own distro, noble) — a \
         different Ubuntu version risks a glibc/ABI mismatch against the deploy target."
    );
}

/// The compile-check job is the FIRST job (per #460): it must prove the vendored C/C++ —
/// including every non-Windows fallback branch the genlock patches carry — actually compiles.
/// The full bundle job must depend on it so a trivial C++ error fails fast, before the full build.
#[test]
fn full_build_depends_on_the_compile_check() {
    let wf = read(WF);
    assert!(
        wf.contains("needs: [linux-distroav-compile-check]"),
        "#460: linux-genlock-build must `needs: [linux-distroav-compile-check]` so a vendored \
         C++ compile error fails in minutes (the narrowed lane) rather than after the full \
         ~2h production build."
    );
}

/// Both jobs must configure DistroAV against an installed OBS SDK prefix — the #392 trick
/// (install --component Development) so DistroAV's config-mode find_package(libobs) /
/// find_package(obs-frontend-api) resolve. Mirrors windows-genlock.yml.
#[test]
fn distroav_configured_against_installed_obs_sdk() {
    let wf = read(WF);
    assert_eq!(
        wf.matches("--component Development").count(),
        2,
        "#460: both jobs must `cmake --install ... --component Development` (the #392 trick) — \
         without it, libobsConfig.cmake / obs-frontend-apiConfig.cmake never land in the SDK \
         prefix and DistroAV's find_package(libobs REQUIRED) fails to resolve."
    );
    assert_eq!(
        wf.matches("CMAKE_PREFIX_PATH").count(),
        2,
        "#460: both DistroAV configure steps must set -DCMAKE_PREFIX_PATH=<installed OBS SDK> \
         so DistroAV links against the genlock-patched libobs, not a system OBS."
    );
}

/// Browser/CEF must stay OFF — imag-nb needs no browser source, and CEF is the dominant time
/// cost on the Windows builds; skipping it entirely on Linux (no obs-deps prebuilt bundle
/// download either) is the whole reason this workflow can run on every push instead of being
/// workflow_dispatch-only like the 150-min windows-genlock.yml.
#[test]
fn browser_disabled_in_both_jobs() {
    let wf = read(WF);
    assert_eq!(
        wf.matches("-DENABLE_BROWSER=OFF").count(),
        2,
        "#460: both OBS configure steps must pass -DENABLE_BROWSER=OFF — CEF is the dominant \
         cost on the Windows builds and imag-nb needs no browser source. Keep it off in both \
         the compile-check lane and the full bundle."
    );
}

/// The correct CMakePresets.json presets must be used — `ubuntu-ci` for OBS (binaryDir
/// build_ubuntu) and `ubuntu-ci-x86_64` for DistroAV (binaryDir build_x86_64, the only preset
/// with a matching buildPreset so `cmake --build --preset ubuntu-ci-x86_64` resolves).
#[test]
fn uses_the_real_cmake_presets() {
    let wf = read(WF);
    assert!(
        wf.contains("cmake --preset ubuntu-ci\n") || wf.contains("cmake --preset ubuntu-ci\\"),
        "#460: OBS must be configured with `cmake --preset ubuntu-ci` (vendor/obs-studio's own \
         CMakePresets.json Linux CI preset — Ninja + RelWithDebInfo + ccache)."
    );
    assert!(
        wf.contains("cmake --preset ubuntu-ci-x86_64"),
        "#460: DistroAV must be configured with `cmake --preset ubuntu-ci-x86_64` \
         (vendor/distroav's own CMakePresets.json Linux CI preset)."
    );
    assert!(
        wf.contains("cmake --build --preset ubuntu-ci-x86_64"),
        "#460: DistroAV must be BUILT via `cmake --build --preset ubuntu-ci-x86_64` — the \
         ubuntu-ci-x86_64 configurePreset has a matching buildPreset (unlike OBS's ubuntu-ci, \
         which has none — that one builds via `cmake --build build_ubuntu` directly)."
    );
}

/// The full bundle job must produce the named artifact the #460 acceptance criteria checks for,
/// and the fast lane must upload its own hot-swap artifact — mirroring the Windows
/// obs-genlock-windows-x64 / distroav-fast-dll pair.
#[test]
fn uploads_the_named_artifacts() {
    let wf = read(WF);
    assert!(
        wf.contains("name: obs-genlock-linux-x86_64"),
        "#460: linux-genlock-build must upload the artifact named obs-genlock-linux-x86_64 — \
         this exact name is the #460 acceptance criteria's proof-of-build."
    );
    assert!(
        wf.contains("name: distroav-linux-fast-so"),
        "#460: linux-distroav-compile-check must upload a fast hot-swap artifact (mirrors the \
         Windows distroav-fast-dll pattern) so a DistroAV-only change can be verified quickly."
    );
    assert!(
        wf.contains("if-no-files-found: error"),
        "#460: both artifact uploads must fail loudly if the expected files are missing, not \
         silently upload an empty artifact."
    );
}

/// The #120 per-component SHA manifest must be generated + self-consistency-checked for the
/// full bundle, reusing scripts/genlock-manifest.sh UNCHANGED (already unit-proven on the Linux
/// `test` job via tests/genlock_manifest.rs) — same anti-#119 guarantee as the Windows bundle.
#[test]
fn full_bundle_generates_and_checks_the_sha_manifest() {
    let wf = read(WF);
    assert!(
        wf.contains("scripts/genlock-manifest.sh --stage stage --out stage/BUNDLE_MANIFEST.json"),
        "#460: linux-genlock-build must generate stage/BUNDLE_MANIFEST.json via the shared \
         scripts/genlock-manifest.sh (#120) — same anti-#119 stale-bytes guarantee as Windows."
    );
    assert!(
        wf.contains("scripts/genlock-manifest.sh --check stage/BUNDLE_MANIFEST.json --stage stage"),
        "#460: linux-genlock-build must self-consistency-check the manifest it just generated \
         (exit 21 on sha-drift/extra/missing file) before uploading the artifact."
    );
}

/// The workflow must trigger on push to dev over the vendored OBS/DistroAV paths (so a Linux
/// build proves out automatically on every vendor change, same push-to-dev pre-merge-gate model
/// as windows-genlock-fast.yml) AND remain workflow_dispatch-able for an on-demand re-run.
#[test]
fn triggers_on_vendor_changes_and_dispatch() {
    let wf = read(WF);
    assert!(
        wf.contains("workflow_dispatch:"),
        "#460: {WF} must support workflow_dispatch."
    );
    assert!(
        wf.contains("branches: [dev]"),
        "#460: {WF} must trigger on push to dev — this repo's pre-merge gate (push omits \
         pull_request per #157)."
    );
    for path in [
        "vendor/obs-studio/**",
        "vendor/distroav/**",
        "scripts/genlock-manifest.sh",
    ] {
        assert!(
            wf.contains(path),
            "#460: {WF} on.push.paths must include `{path}` so a change there re-proves the \
             Linux build."
        );
    }
}

/// Neither job should re-duplicate the dozens of Windows pwsh "assert genlock patch present"
/// text-token guards — those already run as real Rust tests on every push via ci.yml's `test`
/// job (tests/genlock_preload.rs, distroav_genlock_lockdown.rs, etc.). This workflow's unique
/// job is proving actual COMPILATION on Linux, not re-asserting source tokens.
#[test]
fn does_not_duplicate_the_windows_pwsh_guard_pattern() {
    let wf = read(WF);
    assert!(
        !wf.contains("shell: pwsh"),
        "#460: {WF} is a Linux (bash) workflow — a `shell: pwsh` step would indicate an \
         accidental copy-paste of a Windows guard step that belongs in tests/*.rs instead."
    );
}
