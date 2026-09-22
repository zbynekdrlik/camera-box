//! Patch-presence guard for #1352 — on Linux the vendored OBS projector hosts its GL
//! display in a native CHILD of a plain toplevel window, instead of BEING the toplevel
//! X window.
//!
//! Background (measured live on strih-lx, 22.9.2026): under XWayland + NVIDIA PRIME render
//! offload, an OBS projector whose GL surface IS the X toplevel window
//! (`OBSProjector : OBSQTDisplay`, constructed `OBSQTDisplay(widget, Qt::Window)`) blocks the
//! graphics thread ~0.5 s per present (avg render 513 ms, program lag 93 %, multiview 1.8 fps —
//! identical windowed / fullscreen / no-always-on-top). Reparenting the projector into a child
//! window (`xdotool windowreparent`) drops the stall to lag 0 %, avg render 23 ms, MV 29.8 fps.
//! The fix (design issuecomment-5778588966, Prístup 1): on Linux the single creation site
//! (`OBSBasic::OpenProjector`) wraps the projector in a plain host toplevel and constructs the
//! projector as its native CHILD (`Qt::Widget` flags); every toplevel-only call inside
//! `OBSProjector` is routed through a `window()`-based `Toplevel()` accessor (which IS `this`
//! when unhosted, so the Windows/no-host path is behaviorally byte-identical).
//!
//! This is a SOURCE-level guard, not a runtime test (the vendored Qt/C++ compiles only on CI,
//! per the project's Tier-0 policy). It defends against a future `git subtree pull` upstream
//! release-bump silently reverting the child-host wiring or re-introducing a bare-`this`
//! toplevel call. Same vendored-source-assertion convention as
//! tests/obs_unclean_shutdown_auto_normal_1195.rs / tests/gl_egl_present_vsync_1107.rs.

use std::path::PathBuf;

fn repo_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so the assertions survive
/// reformatting (e.g. an upstream merge re-indenting a line).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Occurrences of `needle` in `hay` (non-overlapping).
fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

const PROJECTOR: &str = "vendor/obs-studio/frontend/widgets/OBSProjector.cpp";
const PROJECTOR_HPP: &str = "vendor/obs-studio/frontend/widgets/OBSProjector.hpp";
const CREATION_SITE: &str = "vendor/obs-studio/frontend/widgets/OBSBasic_Projectors.cpp";

// ---------------------------------------------------------------------------
// 1. The single creation site hosts the projector in a child window on Linux.
// ---------------------------------------------------------------------------

#[test]
fn creation_site_is_linux_gated() {
    let s = squish(&repo_file(CREATION_SITE));
    assert!(
        s.contains("#if defined(__linux__)"),
        "{CREATION_SITE}: the #1352 child-host wiring must be Linux-gated with \
         `#if defined(__linux__)` so the Windows/macOS build keeps the toplevel projector; \
         a subtree pull likely dropped the gate."
    );
    // The Windows / non-Linux path stays the byte-identical toplevel construction.
    assert!(
        s.contains("#else") && s.contains("new OBSProjector(nullptr, source, monitor, type)"),
        "{CREATION_SITE}: the non-Linux `#else` branch must keep \
         `new OBSProjector(nullptr, source, monitor, type)` (the toplevel projector, unchanged)."
    );
}

#[test]
fn creation_site_builds_a_host_toplevel_with_the_projector_as_child() {
    let s = squish(&repo_file(CREATION_SITE));

    for (needle, why) in [
        (
            "new QWidget(nullptr, Qt::Window)",
            "the plain host toplevel that owns the projector's GL child",
        ),
        (
            "setAttribute(Qt::WA_DeleteOnClose, true)",
            "the host is WA_DeleteOnClose so closing it tears down the projector",
        ),
        (
            "new QVBoxLayout(host)",
            "a QVBoxLayout on the host so the child projector fills it",
        ),
        (
            "setContentsMargins(0, 0, 0, 0)",
            "the zero-margin layout so the GL child fills the whole host client area",
        ),
        (
            "new OBSProjector(host, source, monitor, type)",
            "the projector constructed as a CHILD of the host (host parent, not nullptr)",
        ),
        (
            "setFocusProxy(projector)",
            "the host forwards focus to the projector so the Escape QAction still fires",
        ),
        (
            "installEventFilter(projector)",
            "the projector filters the host's Close event so the multiviewProjectors / \
             SaveProjectors bookkeeping runs before the host is deleted",
        ),
    ] {
        assert!(
            s.contains(needle),
            "{CREATION_SITE}: missing `{needle}` — {why}. The #1352 child-host wiring is \
             incomplete (a subtree pull likely reverted it)."
        );
    }
}

#[test]
fn external_geometry_persistence_routes_through_the_host_window() {
    let s = squish(&repo_file(CREATION_SITE));

    // Geometry persistence must operate on the HOST toplevel (`projector->window()`), not the
    // child projector, or a windowed projector's saved position is lost. `window()==projector`
    // when unhosted, so this stays byte-identical on Windows.
    for routed in [
        "projector->window()->saveGeometry()",
        "projector->window()->restoreGeometry(",
        "projector->window()->normalGeometry()",
        "projector->window()->setGeometry(",
    ] {
        assert!(
            s.contains(routed),
            "{CREATION_SITE}: geometry persistence must be routed through the host toplevel \
             (`{routed}`); a child projector's geometry is meaningless for the window position."
        );
    }
    // The old bare-projector geometry calls must be gone.
    for bare in [
        "projector->saveGeometry()",
        "projector->restoreGeometry(",
        "projector->normalGeometry()",
    ] {
        assert!(
            !s.contains(bare),
            "{CREATION_SITE}: `{bare}` still calls geometry on the child projector instead of \
             its host toplevel (`projector->window()->...`) — window geometry will not persist."
        );
    }
}

// ---------------------------------------------------------------------------
// 2. The ctor selects Qt::Widget (child) when hosted, Qt::Window otherwise.
// ---------------------------------------------------------------------------

#[test]
fn ctor_selects_child_flags_when_hosted() {
    let s = squish(&repo_file(PROJECTOR));

    // The old unconditional toplevel construction must be gone.
    assert!(
        !s.contains("OBSQTDisplay(widget, Qt::Window)"),
        "{PROJECTOR}: the ctor still hard-codes `OBSQTDisplay(widget, Qt::Window)` — it must \
         select the flags via `projectorWindowFlags(widget)` (Qt::Widget when a host parent \
         exists on Linux, Qt::Window otherwise)."
    );
    assert!(
        s.contains("OBSQTDisplay(widget, projectorWindowFlags(widget))"),
        "{PROJECTOR}: the ctor must construct the base via \
         `OBSQTDisplay(widget, projectorWindowFlags(widget))`."
    );

    // The flag helper: Linux-gated, returns Qt::Widget for a hosted (non-null) parent and
    // Qt::Window otherwise (the Windows/no-host path).
    assert!(
        s.contains("projectorWindowFlags("),
        "{PROJECTOR}: the `projectorWindowFlags()` helper (the child-vs-toplevel flag decision) \
         is missing."
    );
    assert!(
        s.contains("#if defined(__linux__)"),
        "{PROJECTOR}: the flag helper must be Linux-gated with `#if defined(__linux__)` so the \
         Windows build keeps Qt::Window."
    );
    assert!(
        s.contains("return Qt::Widget;") && s.contains("return Qt::Window;"),
        "{PROJECTOR}: the flag helper must return `Qt::Widget` (hosted child) on Linux and \
         `Qt::Window` (toplevel) otherwise."
    );
}

// ---------------------------------------------------------------------------
// 3. Every toplevel-only call inside OBSProjector is routed through Toplevel().
// ---------------------------------------------------------------------------

#[test]
fn toplevel_accessor_is_defined() {
    let hpp = squish(&repo_file(PROJECTOR_HPP));
    let cpp = squish(&repo_file(PROJECTOR));
    assert!(
        hpp.contains("QWidget *Toplevel();"),
        "{PROJECTOR_HPP}: the `Toplevel()` accessor declaration is missing."
    );
    assert!(
        cpp.contains("QWidget *OBSProjector::Toplevel() { return window(); }"),
        "{PROJECTOR}: `Toplevel()` must be defined as `return window();` (which IS `this` when \
         the projector is unhosted — the Windows/no-host byte-identical path)."
    );
}

#[test]
fn every_toplevel_only_call_is_routed_through_the_accessor() {
    let s = squish(&repo_file(PROJECTOR));

    // For each toplevel-only method with no substring collision, every call must be routed:
    // count(method) == count(Toplevel()->method).
    for method in [
        "setWindowFlags(",
        "setWindowTitle(",
        "showFullScreen()",
        "showNormal()",
        "setGeometry(",
        "isFullScreen()",
        "isMaximized()",
        "resize(",
        "windowHandle()",
    ] {
        let total = count(&s, method);
        let routed = count(&s, &format!("Toplevel()->{method}"));
        assert!(
            total > 0 && total == routed,
            "{PROJECTOR}: {total} call(s) to `{method}` but only {routed} routed through \
             `Toplevel()->{method}` — every toplevel-only call must go through the accessor so \
             a hosted (child) projector drives its host toplevel, not itself."
        );
    }

    // `geometry()` collides with the screen's `->geometry()` and `setGeometry(`, so anchor the
    // one `this`-geometry call precisely (prevGeometry save in OpenFullScreenProjector).
    assert!(
        s.contains("prevGeometry = Toplevel()->geometry()"),
        "{PROJECTOR}: the windowed-geometry save must read the host toplevel \
         (`prevGeometry = Toplevel()->geometry()`)."
    );
    assert!(
        !s.contains("prevGeometry = geometry()"),
        "{PROJECTOR}: `prevGeometry = geometry()` still reads the child's geometry instead of \
         the host toplevel's."
    );

    // `screen()` (ScreenRemoved fullscreen-tracking) must read the host toplevel's screen.
    assert!(
        s.contains("Toplevel()->screen()"),
        "{PROJECTOR}: ScreenRemoved must compare against the host toplevel's screen \
         (`Toplevel()->screen()`)."
    );
    assert!(
        !s.contains("this->screen()"),
        "{PROJECTOR}: `this->screen()` still reads the child's screen instead of the host \
         toplevel's."
    );

    // The isOBSProjectorWindow property is set on the toplevel's window handle (guarded, since
    // the host window is not realized at ctor time on the Linux hosted path).
    assert!(
        s.contains("Toplevel()->windowHandle()") && s.contains("isOBSProjectorWindow"),
        "{PROJECTOR}: the `isOBSProjectorWindow` property must be set on \
         `Toplevel()->windowHandle()`."
    );
}

// ---------------------------------------------------------------------------
// 4. The host is torn down when the projector is, and the projector's close
//    bookkeeping runs when the host is closed (no leaked toplevel / dangling ptr).
// ---------------------------------------------------------------------------

#[test]
fn host_teardown_is_wired_both_directions() {
    let hpp = squish(&repo_file(PROJECTOR_HPP));
    let cpp = squish(&repo_file(PROJECTOR));

    // The projector filters the host's events (declared override).
    assert!(
        hpp.contains("bool eventFilter(QObject *watched, QEvent *event) override;"),
        "{PROJECTOR_HPP}: the `eventFilter` override (to catch the host's Close event) is missing."
    );
    // The re-entrancy guard so the two close directions don't recurse / double-free.
    assert!(
        hpp.contains("bool closing = false;"),
        "{PROJECTOR_HPP}: the `closing` re-entrancy guard is missing."
    );
    // The host is closed via QEvent::Close in the filter, and deleted with the projector.
    assert!(
        cpp.contains("QEvent::Close"),
        "{PROJECTOR}: the eventFilter must handle `QEvent::Close` (the host window's close) so \
         DeleteProjector's bookkeeping runs before the host is deleted."
    );
    assert!(
        cpp.contains("deleteLater()"),
        "{PROJECTOR}: the projector's destructor must tear down its host toplevel \
         (`Toplevel()->deleteLater()`) so no empty host window is left behind."
    );
}
