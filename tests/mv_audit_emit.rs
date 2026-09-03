//! #771 — vendored-source guard for the multiview-audit fps observability line.
//!
//! `render_display()` (obs-display.c) is the ONLY place a Multiview projector renders, and
//! before #771 nothing emitted its ACTUAL render cadence — so "at what fps is the multiview
//! running" could not be answered from the OBS log. #771 adds a per-projector
//! `multiview-audit:` line every ~5s (real renders / window) plus a pure `target − tol`
//! floor the E2E gate + drift-guard apply.
//!
//! This guard is STD-ONLY (no `use camera_box`, not probe-gated) so it gives an observable
//! local RED→GREEN via the standalone-rustc recipe (`.claude/rules/vendored-libobs-change-safety.md`,
//! #1026), even though the vendored C only compiles on CI:
//!
//! ```text
//! CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/mv_audit_emit.rs -o /tmp/t && /tmp/t
//! ```
//!
//! The load-bearing anchors are ALSO mirrored into `tests/genlock_preload.rs` (the canonical
//! CI probe-gated guard) and BOTH `windows-genlock.yml` + `windows-genlock-fast.yml` pwsh
//! gates (lock-step, per the vendored-libobs-change-safety rule), so a future `git subtree
//! pull` cannot silently revert the change.

use std::fs;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

const OBS_DISPLAY: &str = "vendor/obs-studio/libobs/obs-display.c";
const OBS_BUDGET: &str = "vendor/obs-studio/libobs/obs-display-budget.h";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";
const OBS_H: &str = "vendor/obs-studio/libobs/obs.h";
const MULTIVIEW_CPP: &str = "vendor/obs-studio/frontend/components/Multiview.cpp";
const OBS_PROJECTOR_CPP: &str = "vendor/obs-studio/frontend/widgets/OBSProjector.cpp";

#[test]
fn render_display_emits_the_multiview_audit_line() {
    let src = read(OBS_DISPLAY);
    // The exact format string the drift-guard / rig-health-audit / E2E preflight parse.
    assert!(
        src.contains(
            "\"multiview-audit: monitor=%u divisor=%u rendered_fps=%.1f target=%.0f floor=%.1f cx=%u cy=%u pre_mv_ms=%.2f pre_mv_max_ms=%.2f mv_ewma_ms=%.2f budget_ms=%.2f mv_cells=%u mv_cell_ms=%.2f mv_cell_max_ms=%.2f mv_top1=%s:%.2f mv_top2=%s:%.2f\""
        ),
        "{OBS_DISPLAY}: #771 multiview-audit blog line gone — MV render fps is no longer visible in the OBS log."
    );
}

#[test]
fn render_display_counts_real_renders_and_windows_at_5s() {
    let src = read(OBS_DISPLAY);
    assert!(
        src.contains("display->render_audit_render_count++;"),
        "{OBS_DISPLAY}: #771 real-render counter not bumped — rendered_fps would always read 0."
    );
    assert!(
        src.contains("if (audit_elapsed >= MULTIVIEW_AUDIT_WINDOW_NS)"),
        "{OBS_DISPLAY}: #771 5s audit window gate gone — the multiview-audit line would not emit periodically."
    );
    assert!(
        src.contains("display->render_audit_id = ++next_audit_id;"),
        "{OBS_DISPLAY}: #771 stable per-projector audit id assignment gone — monitor=N would be unstable."
    );
}

#[test]
fn budget_header_carries_the_pure_floor_and_window_constants() {
    let hdr = read(OBS_BUDGET);
    assert!(
        hdr.contains("static inline double obs_multiview_floor_fps(double target_fps)"),
        "{OBS_BUDGET}: #771/#776/#1212 pure target floor helper gone (or grew params back) — the C log line and the Rust gate would diverge. #1212 retired the issue-1110 area sentinel, so the helper takes only target_fps."
    );
    assert!(
        !hdr.contains("MULTIVIEW_FLOOR_MAX_CALIBRATED_AREA_PX"),
        "{OBS_BUDGET}: #1212 issue-1110 area sentinel constant is back — the floor must be area-independent (a 4K MV holds median 30fps, floor 28), so the constant and its report-only branch must be gone."
    );
    assert!(
        hdr.contains("#define MULTIVIEW_AUDIT_WINDOW_NS 5000000000ULL"),
        "{OBS_BUDGET}: #771 5s audit window constant gone."
    );
    assert!(
        hdr.contains("#define MULTIVIEW_AUDIT_FLOOR_TOLERANCE_FPS 2.0"),
        "{OBS_BUDGET}: #771 floor tolerance constant gone."
    );
}

#[test]
fn render_display_floor_call_is_area_independent_1212() {
    // #1212: the emit site feeds ONLY target_fps into the area-independent floor (the issue-1110
    // render-area sentinel is retired). The cx=/cy= fields stay on the printed audit line for
    // observability, but they no longer enter the floor computation.
    let src = read(OBS_DISPLAY);
    assert!(
        src.contains("obs_multiview_floor_fps(target_fps)"),
        "{OBS_DISPLAY}: #1212 emit site floor call must take only target_fps (the area sentinel is retired)."
    );
    assert!(
        !src.contains("obs_multiview_floor_fps(target_fps, display->cx, display->cy)"),
        "{OBS_DISPLAY}: #1212 the issue-1110 area-aware floor call (passing display->cx/cy) is back — the floor must be area-independent."
    );
    // The audit line itself must still carry the render area for observability.
    assert!(
        src.contains("cx=%u cy=%u"),
        "{OBS_DISPLAY}: #771/#1212 the multiview-audit line no longer prints cx=/cy= — the render area is still useful observability."
    );
}

#[test]
fn display_struct_carries_the_audit_window_fields() {
    let hdr = read(OBS_INTERNAL);
    for field in [
        "uint32_t render_audit_id;",
        "uint64_t render_audit_window_start_ns;",
        "uint32_t render_audit_render_count;",
        // camera-box #1260: budget-gate phase-split accumulators for pre_mv_ms/pre_mv_max_ms.
        "uint64_t render_audit_pre_mv_sum_ns;",
        "uint64_t render_audit_pre_mv_max_ns;",
        "uint32_t render_audit_tick_count;",
        // camera-box #1260 lever (1): per-cell MV instrumentation accumulators.
        "uint64_t render_audit_cell_sum_ns;",
        "uint64_t render_audit_cell_max_ns;",
        "uint32_t render_audit_cell_render_count;",
        "uint32_t render_audit_cell_count;",
        "uint64_t render_audit_top1_ns;",
        "uint64_t render_audit_top2_ns;",
        "char render_audit_top1_name[64];",
        "char render_audit_top2_name[64];",
    ] {
        assert!(
            hdr.contains(field),
            "{OBS_INTERNAL}: #771/#1260 obs_display.{field} missing — the per-projector audit window has nowhere to live."
        );
    }
}

#[test]
fn render_display_and_frontend_carry_the_per_cell_instrumentation_1260() {
    // camera-box #1260 lever (1): the per-cell MV instrumentation spans the frontend (times each
    // scene cell) + libobs (folds + emits). Guard the whole chain so a subtree pull / a revert of
    // any leg is caught — the same lock-step discipline as the #771/#1260 audit-line anchors above.
    let disp = read(OBS_DISPLAY);
    // libobs emits the appended per-cell tokens on the audit line ...
    assert!(
        disp.contains("mv_cells=%u mv_cell_ms=%.2f mv_cell_max_ms=%.2f mv_top1=%s:%.2f mv_top2=%s:%.2f"),
        "{OBS_DISPLAY}: #1260 per-cell tokens (mv_cells/mv_cell_ms/mv_top1/...) gone from the multiview-audit line — the MV phase can no longer be attributed per cell."
    );
    // ... and defines the publish API the frontend feeds.
    assert!(
        disp.contains("void obs_display_report_multiview_cells(obs_display_t *display"),
        "{OBS_DISPLAY}: #1260 obs_display_report_multiview_cells() definition gone — the frontend cannot publish per-cell timing."
    );
    assert!(
        read(OBS_H).contains("EXPORT void obs_display_report_multiview_cells(obs_display_t *display"),
        "{OBS_H}: #1260 obs_display_report_multiview_cells() EXPORT declaration gone — the frontend TU cannot link against it."
    );
    // The FRONTEND times the scene-cell render and publishes it.
    let mv = read(MULTIVIEW_CPP);
    assert!(
        mv.contains("cellSumNs += cell_dt;"),
        "{MULTIVIEW_CPP}: #1260 per-cell scene-render timing gone — the mv_cell_ms aggregate would always be 0."
    );
    assert!(
        mv.contains("obs_display_report_multiview_cells(display,"),
        "{MULTIVIEW_CPP}: #1260 ReportCellStats no longer publishes to libobs — the audit line's per-cell tokens would stay empty."
    );
    // The projector callback invokes the publish after Render, on the graphics thread.
    assert!(
        read(OBS_PROJECTOR_CPP).contains("ReportCellStats(window->GetDisplay())"),
        "{OBS_PROJECTOR_CPP}: #1260 the multiview draw callback no longer calls ReportCellStats — nothing feeds the per-cell window."
    );
}
