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

/// Slice ONE top-level job's YAML block: from its exact 2-space-indented `  <name>:` key up to the
/// next 2-space-indented job key (a line starting with two spaces then an ASCII letter) or end of
/// file. Never a bare `.find("runs-on")` — issue 1317 gives the strih job a DIFFERENT runner image
/// than the imag jobs, so a whole-file `runs-on` read would attribute the wrong runner to a job.
fn job_block(wf: &str, name: &str) -> String {
    let key = format!("  {name}:");
    let lines: Vec<&str> = wf.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim_end() == key)
        .unwrap_or_else(|| panic!("job {name} not found in {WF}"));
    let mut end = lines.len();
    for (i, l) in lines.iter().enumerate().skip(start + 1) {
        let b = l.as_bytes();
        if b.len() >= 3 && &b[0..2] == b"  " && b[2].is_ascii_alphabetic() {
            end = i;
            break;
        }
    }
    lines[start..end].join("\n")
}

/// The value after `<prefix>` on the one block line carrying it, trimmed of quotes — used to pin the
/// strih runner literal and the `STRIH_TARGET_RELEASE` env literal EQUAL.
fn value_after(block: &str, prefix: &str) -> String {
    block
        .lines()
        .find_map(|l| l.trim().strip_prefix(prefix))
        .unwrap_or_else(|| panic!("no line with prefix {prefix:?} in block:\n{block}"))
        .trim()
        .trim_matches('\'')
        .to_string()
}

/// issue 1317 (owner ROZHODNUTÉ 18.9.): the strih-lx notebook runs Ubuntu 26.04 (resolute) —
/// noble's ffmpeg/Qt sonames (`libavcodec60`/Qt 6.4) do not exist there, so the strih bundle must
/// be built on the 26.04 runner. The strih JOB BLOCK pins `runs-on: ubuntu-26.04`; the imag-parity
/// full build + the DistroAV compile-check stay on ubuntu-24.04 (imag-nb's noble). All three jobs
/// still exist.
#[test]
fn strih_job_runs_on_2604_the_imag_jobs_stay_2404() {
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
    assert!(
        wf.contains("linux-genlock-build-strih:"),
        "issue 1317: {WF} must define the linux-genlock-build-strih job — the strih-lx full bundle."
    );

    let strih = job_block(&wf, "linux-genlock-build-strih");
    assert!(
        strih.contains("runs-on: ubuntu-26.04"),
        "issue 1317: the strih job must run on ubuntu-26.04 (the box's release); block:\n{strih}"
    );
    assert!(
        !strih.contains("runs-on: ubuntu-24.04"),
        "issue 1317: the strih job must NOT stay on ubuntu-24.04; block:\n{strih}"
    );

    let imag = job_block(&wf, "linux-genlock-build");
    assert!(
        imag.contains("runs-on: ubuntu-24.04"),
        "the imag-parity full build stays on ubuntu-24.04 (imag-nb noble); block:\n{imag}"
    );
    let cc = job_block(&wf, "linux-distroav-compile-check");
    assert!(
        cc.contains("runs-on: ubuntu-24.04"),
        "the DistroAV compile-check stays on ubuntu-24.04; block:\n{cc}"
    );

    assert_eq!(
        wf.matches("runs-on: ubuntu-24.04").count(),
        2,
        "issue 1317: exactly TWO jobs (compile-check + imag-parity) stay on ubuntu-24.04."
    );
    assert_eq!(
        wf.matches("runs-on: ubuntu-26.04").count(),
        1,
        "issue 1317: exactly ONE job (the strih variant) runs on ubuntu-26.04."
    );
}

/// issue 1317: the strih Stage step writes a `TARGET-RELEASE: ubuntu-26.04` line into
/// `STRIH_BUILD_FLAGS.txt`, single-sourced from a job-level `STRIH_TARGET_RELEASE` env. `runs-on`
/// cannot read job env, so the two release literals must be PINNED EQUAL by this test — otherwise a
/// runner bump and a marker bump could silently drift apart and a bundle would claim the wrong
/// release.
#[test]
fn strih_stage_writes_target_release_marker_pinned_equal_to_the_runner() {
    let wf = read(WF);
    let strih = job_block(&wf, "linux-genlock-build-strih");

    assert!(
        strih.contains("TARGET-RELEASE"),
        "issue 1317: the strih Stage step must write a TARGET-RELEASE line into \
         STRIH_BUILD_FLAGS.txt; block:\n{strih}"
    );

    let runner = value_after(&strih, "runs-on:");
    let target_release = value_after(&strih, "STRIH_TARGET_RELEASE:");
    assert_eq!(
        runner, target_release,
        "issue 1317: the strih runs-on literal and the STRIH_TARGET_RELEASE env literal must be \
         EQUAL (runs-on can't read job env, so this test is what keeps them pinned together)."
    );
    assert_eq!(
        runner, "ubuntu-26.04",
        "issue 1317: the pinned strih release is ubuntu-26.04 (owner ROZHODNUTÉ 18.9.)."
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
    // issue 1317: the strih-lx full bundle is the THIRD job that builds DistroAV against its own
    // installed OBS SDK prefix, so every count below is 3 (compile-check + imag-parity + strih).
    assert_eq!(
        wf.matches("--component Development").count(),
        3,
        "#460/1317: all three jobs must `cmake --install ... --component Development` (the #392 \
         trick) — without it, libobsConfig.cmake / obs-frontend-apiConfig.cmake never land in the \
         SDK prefix and DistroAV's find_package(libobs REQUIRED) fails to resolve."
    );
    assert_eq!(
        wf.matches("CMAKE_PREFIX_PATH").count(),
        3,
        "#460/1317: all three DistroAV configure steps must set -DCMAKE_PREFIX_PATH=<installed OBS \
         SDK> so DistroAV links against the genlock-patched libobs, not a system OBS."
    );
}

/// Browser/CEF stays OFF in the compile-check lane AND the imag-parity full bundle — imag-nb needs
/// no browser source, and skipping CEF there keeps those two jobs fast (runnable on every push, not
/// workflow_dispatch-only like the 150-min windows-genlock.yml). The strih variant
/// (linux-genlock-build-strih) DELIBERATELY enables browser + fetches CEF as of issue 1317 — its ON
/// coverage lives in tests/python/test_linux_genlock_strih_cef_1317.py. The count stays 2 because the
/// strih configure uses the `${{ env.STRIH_ENABLE_BROWSER }}` form, not a literal `-DENABLE_BROWSER=OFF`.
#[test]
fn browser_disabled_in_both_jobs() {
    let wf = read(WF);
    assert_eq!(
        wf.matches("-DENABLE_BROWSER=OFF").count(),
        2,
        "#460/#1317: the compile-check lane and the imag-parity full bundle must each pass \
         -DENABLE_BROWSER=OFF (imag-nb needs no browser source). The strih variant is \
         intentionally ON via the env-var form and is covered by the 1317 python tests."
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
