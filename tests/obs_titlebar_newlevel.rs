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

/// Issue 1357: the Linux OBS boxes (strih-lx, imag) install the bundle into the `/usr`
/// prefix (`/usr/bin/obs`), so the two exe-relative Windows-layout candidates resolve to
/// `/GENLOCK_BUILD_SHA.txt` / `/usr/bin/GENLOCK_BUILD_SHA.txt` — neither exists, and the
/// title read `newlevel.media build unknown` on every Linux box. The deploy writes the
/// marker at the canonical Linux marker home `/opt/obs-genlock/GENLOCK_BUILD_SHA.txt`
/// (`genlock_write_markers`, `GENLOCK_MARKER_DIR`), so `NewlevelBuildSha()` must try that
/// absolute path — compiled ONLY under `__linux__` and tried AFTER the exe-relative
/// candidates, so the Windows build is byte-identical in behaviour.
#[test]
fn titlebar_reads_the_linux_marker_home_after_the_exe_relative_candidates_1357() {
    let src = squish(&vendor_file(OBS_BASIC));
    const LINUX_MARKER: &str = r#""/opt/obs-genlock/GENLOCK_BUILD_SHA.txt""#;
    // Anchor on the actual READ call, not the bare path literal, so a path left behind in a
    // comment inside the `#ifdef` block can never satisfy the guard on its own.
    const LINUX_READ: &str = r#"NewlevelReadShaMarker("/opt/obs-genlock/GENLOCK_BUILD_SHA.txt")"#;

    // Slice the NewlevelBuildSha() body: from its definition to the UpdateTitleBar() that
    // follows it (the helper is defined directly above its only caller).
    let start = src
        .find("static std::string NewlevelBuildSha()")
        .unwrap_or_else(|| panic!("{OBS_BASIC}: NewlevelBuildSha() definition missing"));
    let rest = &src[start..];
    let end = rest
        .find("void OBSBasic::UpdateTitleBar()")
        .unwrap_or_else(|| panic!("{OBS_BASIC}: UpdateTitleBar() not found after the helper"));
    let body = &rest[..end];

    let exe_rel = body.find(r#""../../GENLOCK_BUILD_SHA.txt""#).unwrap_or_else(|| {
        panic!("{OBS_BASIC}: the exe-relative install-root candidate is gone from NewlevelBuildSha()")
    });
    let exe_call = body.find("os_get_executable_path_ptr(").unwrap_or_else(|| {
        panic!("{OBS_BASIC}: NewlevelBuildSha() no longer resolves candidates via os_get_executable_path_ptr")
    });
    let linux = body.find(LINUX_READ).unwrap_or_else(|| {
        panic!(
            "{OBS_BASIC}: NewlevelBuildSha() does not read the Linux marker home \
             via {LINUX_READ} — every Linux OBS box (/usr/bin/obs) titles itself \
             'newlevel.media build unknown' (issue 1357). Re-add the __linux__ candidate."
        )
    });
    assert_eq!(
        body.matches(LINUX_READ).count(),
        1,
        "{OBS_BASIC}: the Linux marker read must appear exactly once in NewlevelBuildSha()"
    );

    // Tried AFTER the exe-relative candidates (Windows order unchanged, Linux falls back).
    assert!(
        linux > exe_rel && linux > exe_call,
        "{OBS_BASIC}: the Linux marker candidate must be tried AFTER the exe-relative \
         candidates so the Windows lookup order is byte-identical (issue 1357)"
    );

    // Compiled ONLY under __linux__: the path sits between an `#ifdef __linux__` that comes
    // after the exe-relative lookup and the next `#endif`.
    let ifdef = body[..linux].rfind("#ifdef __linux__").unwrap_or_else(|| {
        panic!(
            "{OBS_BASIC}: the Linux marker candidate is not guarded by `#ifdef __linux__` — \
             it must never be compiled into the Windows obs64.exe (issue 1357)"
        )
    });
    assert!(
        ifdef > exe_call,
        "{OBS_BASIC}: the `#ifdef __linux__` guarding the Linux marker candidate must come \
         after the exe-relative candidate lookup (issue 1357)"
    );
    assert!(
        !body[ifdef..linux].contains("#endif"),
        "{OBS_BASIC}: the Linux marker candidate lies outside its `#ifdef __linux__` block"
    );
    assert!(
        body[linux..].contains("#endif"),
        "{OBS_BASIC}: the `#ifdef __linux__` block around the Linux marker is not closed"
    );

    // An ABSOLUTE path read directly — never fed through os_get_executable_path_ptr, which
    // would re-root it under the exe dir.
    assert!(
        !body.contains(&format!("os_get_executable_path_ptr({LINUX_MARKER}")),
        "{OBS_BASIC}: the Linux marker home is absolute — it must be read directly, not \
         resolved relative to the executable (issue 1357)"
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
