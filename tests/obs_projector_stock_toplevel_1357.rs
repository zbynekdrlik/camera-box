//! Guard for issue 1357: the vendored OBS projector runs the STOCK upstream toplevel window
//! path on every OS. Issue 1352 used to host it in a Linux-only child window, and this guard
//! keeps that from coming back.
//!
//! Background:
//! - Issue 1352 worked around an XWayland + NVIDIA-PRIME-offload present stall on strih-lx with
//!   two mechanisms. The vendored one built a plain host toplevel in `OBSBasic::OpenProjector`
//!   under a Linux-only gate, made the projector its `Qt::Widget` child, and routed every
//!   toplevel call through a `Toplevel()` accessor. The runtime one was the `strih-mv-host`
//!   X11 reparenting helper.
//! - On 23.9.2026 strih-lx moved to openbox on plain Xorg with NVIDIA as the primary provider,
//!   so no box runs XWayland or PRIME offload any more.
//! - The hosted shape itself turned the projector black when "always on top" was toggled at
//!   runtime. `SetAlwaysOnTop` does `setWindowFlags` + `show` on the host toplevel, Qt
//!   recreates the host's native window, and the child's native GL window loses its parent.
//!
//! Issue 1357 (one unified design, no per-box exception) removed both mechanisms. The projector
//! is again `OBSQTDisplay(widget, Qt::Window)`, created parentless at the one creation site, and
//! every runtime always-on-top call acts on the projector window itself, exactly as upstream
//! OBS and the Windows build do.
//!
//! This is a SOURCE-level guard, not a runtime test: the vendored Qt/C++ compiles only on CI,
//! per the project's Tier-0 policy. It is pure `std`, so it runs offline via plain `rustc --test`.

use std::path::PathBuf;

fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn repo_file(rel: &str) -> String {
    let p = repo_path(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space, so the assertions survive a
/// re-indent (for example an upstream subtree pull reformatting a line).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const PROJECTOR: &str = "vendor/obs-studio/frontend/widgets/OBSProjector.cpp";
const PROJECTOR_HPP: &str = "vendor/obs-studio/frontend/widgets/OBSProjector.hpp";
const CREATION_SITE: &str = "vendor/obs-studio/frontend/widgets/OBSBasic_Projectors.cpp";
const PREVIEW: &str = "vendor/obs-studio/frontend/widgets/OBSBasic_Preview.cpp";
const WIDGETS_DIR: &str = "vendor/obs-studio/frontend/widgets";

// ---------------------------------------------------------------------------
// 1. The projector is the stock toplevel window on every OS.
// ---------------------------------------------------------------------------

#[test]
fn projector_ctor_is_the_stock_toplevel_window() {
    let s = squish(&repo_file(PROJECTOR));
    assert!(
        s.contains(": OBSQTDisplay(widget, Qt::Window),"),
        "{PROJECTOR}: the projector must be constructed as the stock toplevel \
         `OBSQTDisplay(widget, Qt::Window)` on every OS (issue 1357 removed the Linux child-host)"
    );
    assert!(
        !s.contains("projectorWindowFlags"),
        "{PROJECTOR}: the per-OS `projectorWindowFlags` choice (the issue-1352 Linux `Qt::Widget` \
         child) must be gone -- Linux and Windows run one projector window path"
    );
    assert!(
        !s.contains("Toplevel()"),
        "{PROJECTOR}: no toplevel call may be routed through a host-toplevel accessor any more; \
         the projector IS its own toplevel"
    );
}

#[test]
fn projector_header_has_no_host_toplevel_plumbing() {
    let s = squish(&repo_file(PROJECTOR_HPP));
    for token in ["Toplevel()", "bool closing", "eventFilter"] {
        assert!(
            !s.contains(token),
            "{PROJECTOR_HPP}: `{token}` belongs to the retired issue-1352 host-toplevel plumbing \
             (accessor, two-direction teardown guard, host Close filter) and must be gone"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. The single creation site builds one parentless toplevel projector, no host window.
// ---------------------------------------------------------------------------

#[test]
fn creation_site_builds_one_parentless_projector_on_every_os() {
    let s = squish(&repo_file(CREATION_SITE));
    assert!(
        s.contains("OBSProjector *projector = new OBSProjector(nullptr, source, monitor, type);"),
        "{CREATION_SITE}: OpenProjector must construct the projector parentless (stock upstream)"
    );
    assert!(
        !s.contains("__linux__"),
        "{CREATION_SITE}: the creation site must have no Linux-only branch -- one projector path \
         on every OS (the issue-1352 host was Linux-gated here)"
    );
    for token in [
        "QVBoxLayout",
        "new OBSProjector(host",
        "installEventFilter(projector)",
    ] {
        assert!(
            !s.contains(token),
            "{CREATION_SITE}: `{token}` is the retired issue-1352 host toplevel and must be gone"
        );
    }
    assert!(
        !s.contains("projector->window()"),
        "{CREATION_SITE}: geometry must be saved/restored on the projector itself, not on a host \
         toplevel reached through window()"
    );
}

// ---------------------------------------------------------------------------
// 3. The RUNTIME always-on-top toggles act on the projector window itself.
//    (The black-projector defect: SetAlwaysOnTop on a host toplevel recreated its native window
//    and orphaned the hosted child's GL window.)
// ---------------------------------------------------------------------------

#[test]
fn runtime_always_on_top_acts_on_the_projector_itself() {
    let proj = squish(&repo_file(PROJECTOR));
    assert!(
        proj.contains("SetAlwaysOnTop(this, isAlwaysOnTop);"),
        "{PROJECTOR}: OBSProjector::SetIsAlwaysOnTop must toggle the projector window itself"
    );
    let preview = squish(&repo_file(PREVIEW));
    assert!(
        preview.contains("SetAlwaysOnTop(projectors[i], top);"),
        "{PREVIEW}: OBSBasic::UpdateProjectorAlwaysOnTop must toggle each projector window itself"
    );
}

/// Every `SetAlwaysOnTop(` CALL in the vendored frontend widgets passes the window itself --
/// never a host toplevel reached through `window()` or a `Toplevel()` accessor.
#[test]
fn no_set_always_on_top_call_goes_through_a_host_toplevel() {
    let dir = repo_path(WIDGETS_DIR);
    let mut checked = 0usize;
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "cpp"))
        .collect();
    entries.sort();
    for path in entries {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let s = squish(&text);
        let mut rest = s.as_str();
        while let Some(at) = rest.find("SetAlwaysOnTop(") {
            let call = &rest[at..];
            let end = call.find(';').unwrap_or(call.len());
            let stmt = &call[..end];
            assert!(
                !stmt.contains("window()") && !stmt.contains("Toplevel("),
                "{}: `{stmt}` routes always-on-top through a host toplevel -- that recreates the \
                 host's native window and blacks out a hosted projector (issue 1357)",
                path.display()
            );
            checked += 1;
            rest = &call["SetAlwaysOnTop(".len()..];
        }
    }
    // OBSProjector, UpdateProjectorAlwaysOnTop, the main-window toggle and the startup call.
    assert!(
        checked >= 4,
        "expected at least 4 SetAlwaysOnTop( call sites under {WIDGETS_DIR}, found {checked} -- \
         the scan lost its anchors"
    );
}
