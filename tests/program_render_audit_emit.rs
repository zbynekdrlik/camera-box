//! #1029 — vendored-source guard for the program-render observability line.
//!
//! `obs_graphics_thread_loop()` (obs-video.c) is where the PROGRAM output is composited and its
//! frame counters (`obs->video.total_frames` / `.lagged_frames`) advance. Before #1029 nothing
//! emitted the program-render cadence, so "at what fps is the PROGRAM rendering, and how many
//! frames did it skip" could not be answered from the OBS log — only the transient WS GetStats
//! carried it, and its `activeFps` lies during a stall (#935). #1029 adds a `program-render-audit:`
//! line every ~5s (real frame-counter deltas), so a burn-square forward JUMP (#1029) is
//! attributable to the render path (`lagged>0`) durably and offline, alongside the existing
//! `genlock-fifo audit` (FIFO) and `multiview-audit` (monitoring-surface render) lines.
//!
//! This guard is STD-ONLY (no `use camera_box`, not probe-gated) so it gives an observable local
//! RED→GREEN via the standalone-rustc recipe (`.claude/rules/vendored-libobs-change-safety.md`,
//! #1026), even though the vendored C only compiles on CI:
//!
//! ```text
//! CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/program_render_audit_emit.rs -o /tmp/t && /tmp/t
//! ```

use std::fs;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

const OBS_VIDEO: &str = "vendor/obs-studio/libobs/obs-video.c";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";

#[test]
fn graphics_loop_emits_the_program_render_audit_line() {
    let src = read(OBS_VIDEO);
    // The exact format string the parser (`src/program_render_audit.rs`) and any future forensic
    // consumer read. Mirror it byte-identically in the parser if this ever changes.
    assert!(
        src.contains(
            "\"program-render-audit: render_fps=%.1f target_fps=%.1f avg_frame_ms=%.2f lagged=%u total=%u\""
        ),
        "{OBS_VIDEO}: #1029 program-render-audit blog line gone — program-output renderSkipped is no longer visible in the OBS log."
    );
}

#[test]
fn graphics_loop_windows_the_audit_and_reads_the_real_counters() {
    let src = read(OBS_VIDEO);
    assert!(
        src.contains("#define PROGRAM_RENDER_AUDIT_WINDOW_NS 5000000000ULL"),
        "{OBS_VIDEO}: #1029 ~5s program-render audit window constant gone."
    );
    assert!(
        src.contains("if (prg_audit_elapsed >= PROGRAM_RENDER_AUDIT_WINDOW_NS)"),
        "{OBS_VIDEO}: #1029 program-render audit window gate gone — the line would not emit periodically."
    );
    // The HONEST rate comes from the real total_frames counter delta, NOT a canvas-fps gauge.
    assert!(
        src.contains("obs->video.total_frames - context->program_render_audit_total_at_start")
            && src.contains("obs->video.lagged_frames - context->program_render_audit_lagged_at_start"),
        "{OBS_VIDEO}: #1029 audit no longer reads total_frames/lagged_frames deltas — render_fps/lagged would be wrong."
    );
}

#[test]
fn graphics_context_carries_the_program_render_audit_fields() {
    let hdr = read(OBS_INTERNAL);
    for field in [
        "uint64_t program_render_audit_window_start_ns;",
        "uint32_t program_render_audit_total_at_start;",
        "uint32_t program_render_audit_lagged_at_start;",
    ] {
        assert!(
            hdr.contains(field),
            "{OBS_INTERNAL}: #1029 obs_graphics_context.{field} missing — the program-render audit window has nowhere to live."
        );
    }
}
