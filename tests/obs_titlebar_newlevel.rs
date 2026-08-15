//! Patch-presence guard for #152 / #1018 — the genlocked OBS build stamps its
//! **newlevel.media build identity + deployed commit SHA** into the main-window title bar.
//!
//! Background: the production boxes (strih/stream) run a custom vendored OBS. Operators
//! must be able to tell at a glance, from the running window itself, that the box is on
//! the newlevel.media build and WHICH build it is (version-integrity epic #125). The
//! title is composed in `OBSBasic::UpdateTitleBar()` (vendor/obs-studio); the patch
//! appends ` - newlevel.media build <short-sha>` after the OBS version string.
//!
//! #1018: the identity used to be the compiler `__DATE__` reformatted to ISO — but OBS
//! builds the frontend with `/Brepro` (reproducible builds), which blanks `__DATE__` to a
//! short placeholder, so the title read "newlevel.media build unknown" on every production
//! build. It now reads the deployed commit SHA from `GENLOCK_BUILD_SHA.txt` (the marker
//! every deploy writes at the install root) — resolved from obs64.exe's own directory via
//! `os_get_executable_path_ptr`, never the process cwd — and shows the short SHA. The pure
//! formatting is in NewlevelBuildSha.hpp (unit-tested by tests/obs_titlebar_newlevel_sha_parse.rs).
//!
//! This is a SOURCE-level guard, not a runtime test: the genlock patches live in the
//! vendored C++ (`git log -- vendor/` is the patch series, per vendor/README.md). The
//! risk this test defends against is a future `git subtree pull --squash` upstream
//! release-bump (the `/update-av-stack` flow, #44) silently re-importing upstream's stock
//! `UpdateTitleBar()` and dropping the newlevel.media marker on the production boxes. If
//! that happens, CI fails loudly here — exactly the "report conflicts loudly" contract of
//! the monorepo. Same vendored-source-assertion convention as
//! tests/obs_updater_disabled.rs and tests/av_stack_update.rs.

use std::path::PathBuf;

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so the assertions survive
/// reformatting (e.g. an upstream merge re-indenting a line). Mirrors the `-replace '\s+',
/// ' '` the pwsh workflow gates apply, so the Rust + YAML guards check the same token.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const OBS_BASIC: &str = "vendor/obs-studio/frontend/widgets/OBSBasic.cpp";
const WINDOWS_GENLOCK_WF: &str = ".github/workflows/windows-genlock.yml";
const WINDOWS_GENLOCK_FAST_WF: &str = ".github/workflows/windows-genlock-fast.yml";

#[test]
fn titlebar_carries_newlevel_media_build_marker_and_sha() {
    let src = squish(&vendor_file(OBS_BASIC));

    // The call-site marker: UpdateTitleBar() appends the newlevel.media build identity +
    // the deployed commit SHA to the window title.
    assert!(
        src.contains(r#"name << " - newlevel.media build " << NewlevelBuildSha();"#),
        "{OBS_BASIC}: the #152/#1018 newlevel.media build marker is missing from the OBS \
         window title (UpdateTitleBar). A `git subtree pull` upstream bump likely restored \
         the stock title and dropped it; re-apply the genlock title patch."
    );

    // The build-id helper exists.
    assert!(
        src.contains("static std::string NewlevelBuildSha()"),
        "{OBS_BASIC}: the #1018 NewlevelBuildSha() helper is gone — the title would lose \
         its build identity. Re-apply the genlock title patch."
    );

    // #1018: the identity is READ from the deployed GENLOCK_BUILD_SHA.txt marker, resolved
    // relative to the executable (never the process cwd), NOT derived from the compiler
    // `__DATE__` (which /Brepro blanks).
    assert!(
        src.contains("GENLOCK_BUILD_SHA.txt"),
        "{OBS_BASIC}: the #1018 title no longer reads GENLOCK_BUILD_SHA.txt — the deployed \
         build id would be lost. Re-apply the patch."
    );
    assert!(
        src.contains("os_get_executable_path_ptr("),
        "{OBS_BASIC}: the #1018 title no longer resolves GENLOCK_BUILD_SHA.txt relative to \
         the executable (os_get_executable_path_ptr) — a cwd-relative read regresses to the \
         #1018 'unknown' bug on a shortcut launch. Re-apply the patch."
    );
    assert!(
        !src.contains("const std::string d = __DATE__;"),
        "{OBS_BASIC}: the compiler __DATE__ build-date mechanism is back — it is blanked by \
         OBS's /Brepro reproducible build and always renders 'unknown' (#1018). The title \
         must read the deployed SHA from GENLOCK_BUILD_SHA.txt instead."
    );
}

#[test]
fn windows_genlock_workflows_gate_on_the_titlebar_marker() {
    // The canonical guard is the test above, but this crate is Linux-only
    // (v4l/alsa/evdev) and cannot compile on the windows-2022 runner, so BOTH Windows
    // workflows re-assert the same source tokens in pwsh BEFORE their build (the FULL
    // windows-genlock.yml builds the frontend where OBSBasic.cpp lives; the FAST
    // windows-genlock-fast.yml does NOT build the frontend but still source-text-gates
    // the token, same lock-step convention as the #276/#278 OBSProjector gate). Keep the
    // two pwsh gates in lock-step with the canonical assertion: drop the source check from
    // either workflow and CI fails here.
    for wf in [WINDOWS_GENLOCK_WF, WINDOWS_GENLOCK_FAST_WF] {
        let src = squish(&vendor_file(wf));
        assert!(
            src.contains(r#"name << " - newlevel.media build " << NewlevelBuildSha();"#),
            "{wf}: the production build no longer asserts the #152/#1018 newlevel.media \
             title marker — a future subtree bump could ship a stock title with no build \
             identity while the build still passes. Re-add the pwsh source gate (lock-step)."
        );
        assert!(
            src.contains("GENLOCK_BUILD_SHA.txt"),
            "{wf}: the production build no longer asserts the #1018 GENLOCK_BUILD_SHA.txt \
             read in the OBS title. Re-add the pwsh source gate (lock-step)."
        );
    }
}
