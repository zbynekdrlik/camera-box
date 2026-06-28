//! Patch-presence guard for #152 — the genlocked OBS build stamps its **newlevel.media
//! build identity + compile date** into the main-window title bar.
//!
//! Background: the production boxes (strih/stream) run a custom vendored OBS. Operators
//! must be able to tell at a glance, from the running window itself, that the box is on
//! the newlevel.media build and WHICH build it is (version-integrity epic #125). The
//! title is composed in `OBSBasic::UpdateTitleBar()` (vendor/obs-studio); the patch
//! appends ` - newlevel.media build <YYYY-MM-DD>` after the OBS version string. The date
//! is the compiler's `__DATE__` reformatted to ISO by the file-local `NewlevelBuildDate()`
//! helper — injected at build time, NEVER hardcoded.
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
fn titlebar_carries_newlevel_media_build_marker_and_date() {
    let src = squish(&vendor_file(OBS_BASIC));

    // The call-site marker: UpdateTitleBar() appends the newlevel.media build identity +
    // the (compile-time) build date to the window title.
    assert!(
        src.contains(r#"name << " - newlevel.media build " << NewlevelBuildDate();"#),
        "{OBS_BASIC}: the #152 newlevel.media build marker is missing from the OBS window \
         title (UpdateTitleBar). A `git subtree pull` upstream bump likely restored the \
         stock title and dropped it; re-apply the genlock #152 title patch."
    );

    // The build-date helper exists and derives the date from the compiler at build time —
    // `__DATE__`, never a hardcoded literal.
    assert!(
        src.contains("static std::string NewlevelBuildDate()"),
        "{OBS_BASIC}: the #152 NewlevelBuildDate() helper is gone — the title would lose \
         its build date. Re-apply the genlock #152 title patch."
    );
    assert!(
        src.contains("const std::string d = __DATE__;"),
        "{OBS_BASIC}: the #152 build-date injection no longer reads the compiler `__DATE__` \
         — the date must be injected at build time, never hardcoded. Re-apply the patch."
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
            src.contains(r#"name << " - newlevel.media build " << NewlevelBuildDate();"#),
            "{wf}: the production build no longer asserts the #152 newlevel.media title \
             marker — a future subtree bump could ship a stock title with no build identity \
             while the build still passes. Re-add the pwsh source gate (lock-step)."
        );
        assert!(
            src.contains("const std::string d = __DATE__;"),
            "{wf}: the production build no longer asserts the #152 `__DATE__` build-date \
             injection. Re-add the pwsh source gate (lock-step)."
        );
    }
}
