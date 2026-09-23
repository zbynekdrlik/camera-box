//! Patch-presence guard for #1358 — the vendored OBS display widget (`OBSQTDisplay`, the base of
//! every preview AND projector window) DEBOUNCES its resize events, so an interactive window drag
//! calls `obs_display_resize` once with the final size instead of on every configure event.
//!
//! Background (measured live on strih-lx, 23.9.2026): dragging the multiview projector window
//! 1851×1011 → 1657×901 stalled the PROGRAM render (`program-render-audit lagged=80`, then 54,
//! then 15 of 150 frames). `obs_display_resize` only stores `next_cx/next_cy`; the graphics thread
//! then resizes the swap chain (`gs_resize`) on its next render of that display, and a drag emits
//! a resize per step, so the swap chain was reallocated on almost every frame on the ONE graphics
//! thread the program render shares. The fix (design issuecomment-5793018306, Prístup 1): a
//! single-shot `QTimer` in the widget; `resizeEvent` (re)starts it, its slot applies the final
//! pixel size once and emits `DisplayResized`; the FIRST resize after the display is created stays
//! immediate. Same code on every platform (no Linux-only branch).
//!
//! SOURCE-level guard, not a runtime test (the vendored Qt/C++ compiles only on CI, Tier-0). It
//! defends against an upstream `git subtree pull` silently restoring the per-event resize. Same
//! vendored-source-assertion convention as tests/obs_projector_child_host_1352.rs.

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

/// The body of the free function / method whose definition line starts with `signature`
/// (up to the first line that is exactly `}` after it).
fn fn_body(src: &str, signature: &str) -> String {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("{DISPLAY}: `{signature}` definition not found"));
    let rest = &src[start..];
    let end = rest
        .find("\n}\n")
        .unwrap_or_else(|| panic!("{DISPLAY}: end of `{signature}` body not found"));
    rest[..end].to_string()
}

const DISPLAY: &str = "vendor/obs-studio/frontend/widgets/OBSQTDisplay.cpp";
const DISPLAY_HPP: &str = "vendor/obs-studio/frontend/widgets/OBSQTDisplay.hpp";

#[test]
fn header_declares_the_debounce_timer_the_immediate_flag_and_the_apply_method() {
    let h = squish(&repo_file(DISPLAY_HPP));
    for (needle, why) in [
        (
            "class QTimer;",
            "the QTimer forward declaration (the header stays light; OBSQTDisplay.cpp includes <QTimer>)",
        ),
        (
            "QTimer *resizeDebounce = nullptr;",
            "the single-shot debounce timer member",
        ),
        (
            "bool resizeImmediate = false;",
            "the first-resize-after-create flag (a new display never renders at a stale size)",
        ),
        (
            "void ApplyDisplayResize();",
            "the ONE place the debounced size reaches obs_display_resize",
        ),
    ] {
        assert!(
            h.contains(needle),
            "{DISPLAY_HPP}: missing `{needle}` — {why} (#1358 resize debounce)."
        );
    }
}

#[test]
fn ctor_builds_a_single_shot_timer_wired_to_the_apply_slot() {
    let s = squish(&repo_file(DISPLAY));
    assert!(
        s.contains("#include <QTimer>"),
        "{DISPLAY}: must `#include <QTimer>` for the #1358 debounce timer."
    );
    let ctor = squish(&fn_body(
        &repo_file(DISPLAY),
        "OBSQTDisplay::OBSQTDisplay(QWidget *parent",
    ));
    for (needle, why) in [
        (
            "resizeDebounce = new QTimer(this);",
            "the timer is a child QObject of the widget (freed with it)",
        ),
        (
            "resizeDebounce->setSingleShot(true);",
            "single-shot: one apply per quiet period",
        ),
        (
            "connect(resizeDebounce, &QTimer::timeout, this, &OBSQTDisplay::ApplyDisplayResize);",
            "the timeout drives the one apply slot",
        ),
    ] {
        assert!(
            ctor.contains(needle),
            "{DISPLAY}: the OBSQTDisplay ctor is missing `{needle}` — {why} (#1358)."
        );
    }
}

#[test]
fn debounce_interval_is_a_short_quiet_period() {
    let ctor = squish(&fn_body(
        &repo_file(DISPLAY),
        "OBSQTDisplay::OBSQTDisplay(QWidget *parent",
    ));
    let key = "resizeDebounce->setInterval(";
    let at = ctor.find(key).unwrap_or_else(|| {
        panic!("{DISPLAY}: the ctor must set the debounce interval via `{key}ms)`")
    });
    let tail = &ctor[at + key.len()..];
    let ms: u32 = tail[..tail.find(')').expect("setInterval( has no closing paren")]
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("{DISPLAY}: debounce interval is not an integer literal: {e}"));
    // Long enough to coalesce a drag's configure-event burst (tens of ms apart), short enough that
    // the stretched previous-size frame after the release is invisible to the operator.
    assert!(
        (50..=300).contains(&ms),
        "{DISPLAY}: debounce interval {ms} ms is outside 50..=300 ms (design: ~150 ms)."
    );
}

#[test]
fn resize_event_restarts_the_timer_and_never_resizes_directly() {
    let body = fn_body(&repo_file(DISPLAY), "void OBSQTDisplay::resizeEvent(");
    let s = squish(&body);
    assert!(
        !s.contains("obs_display_resize("),
        "{DISPLAY}: resizeEvent must NOT call obs_display_resize itself — every drag step would \
         reallocate the swap chain on the graphics thread again (#1358). Body:\n{body}"
    );
    assert!(
        s.contains("resizeDebounce->start();"),
        "{DISPLAY}: resizeEvent must (re)start the debounce timer (#1358). Body:\n{body}"
    );
    assert!(
        !s.contains("emit DisplayResized();"),
        "{DISPLAY}: resizeEvent must leave `emit DisplayResized()` to ApplyDisplayResize so the \
         preview layout and the swap-chain size change together (#1358). Body:\n{body}"
    );
    // The first resize after creation is applied at once (and cancels any pending apply).
    for needle in [
        "if (resizeImmediate) {",
        "resizeImmediate = false;",
        "resizeDebounce->stop();",
        "ApplyDisplayResize();",
    ] {
        assert!(
            s.contains(needle),
            "{DISPLAY}: resizeEvent's first-resize-after-create branch is missing `{needle}` (#1358). \
             Body:\n{body}"
        );
    }
    // Unified design: the same code on every platform.
    assert!(
        !body.contains("#if"),
        "{DISPLAY}: resizeEvent must not be platform-gated — the debounce is the same on every \
         OS (#1358 / the unified-design rule). Body:\n{body}"
    );
}

#[test]
fn apply_slot_resizes_the_live_display_and_emits_display_resized() {
    let body = fn_body(
        &repo_file(DISPLAY),
        "void OBSQTDisplay::ApplyDisplayResize(",
    );
    let s = squish(&body);
    for needle in [
        "if (isVisible() && display) {",
        "QSize size = GetPixelSize(this);",
        "obs_display_resize(display, size.width(), size.height());",
        "emit DisplayResized();",
    ] {
        assert!(
            s.contains(needle),
            "{DISPLAY}: ApplyDisplayResize is missing `{needle}` (#1358). Body:\n{body}"
        );
    }
}

#[test]
fn create_display_arms_the_immediate_first_resize() {
    let body = fn_body(&repo_file(DISPLAY), "void OBSQTDisplay::CreateDisplay(");
    let s = squish(&body);
    let created = s
        .find("display = obs_display_create(&info, backgroundColor);")
        .unwrap_or_else(|| panic!("{DISPLAY}: CreateDisplay no longer calls obs_display_create"));
    let armed = s.find("resizeImmediate = true;").unwrap_or_else(|| {
        panic!("{DISPLAY}: CreateDisplay must set `resizeImmediate = true;` (#1358). Body:\n{body}")
    });
    assert!(
        armed > created,
        "{DISPLAY}: `resizeImmediate = true;` must come AFTER obs_display_create (only a display \
         that was really created arms the immediate first resize, #1358)."
    );
}

#[test]
fn only_the_apply_slot_and_the_create_paths_call_obs_display_resize() {
    let src = repo_file(DISPLAY);
    // 1 = the ctor's visibleChanged lambda, 2 = the ctor's screenChanged lambda (both create /
    // visibility paths), 3 = ApplyDisplayResize. A 4th call is a regression back to per-event resize.
    let n = src.matches("obs_display_resize(").count();
    assert_eq!(
        n, 3,
        "{DISPLAY}: expected exactly 3 obs_display_resize( call sites (visibleChanged, \
         screenChanged, ApplyDisplayResize), found {n} (#1358)."
    );
}
