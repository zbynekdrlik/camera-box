//! #1146 — observability guard for the #1107 fullscreen-program-projector present-vsync.
//!
//! Root cause (#1146, live imag-nb 2026-08-20): the #1107 EGL vsync fix (the fullscreen HDMI
//! program projector presents with `eglSwapInterval(1)`, tear-free) is deployed and correctly
//! targeted, but it is INVISIBLE — `gl_x11_egl_device_present()` logs ONLY on `eglSwapInterval`
//! FAILURE, never the armed state. `strings` on the deployed `libobs*.so` finds no marker, so
//! nothing (operator, drift-guard, the E2E `[0/8]` preflight) can confirm from the OBS log
//! whether present-vsync is actually armed on the program projector. That unverifiability is
//! why the tear reads as "raz dobre raz zle": the mechanism cannot be checked.
//!
//! The fix (this ticket): `obs_display_set_vsync()` (libobs, the single source of truth for the
//! per-display decision, called ONLY from OBSProjector.cpp) emits a one-shot `projector-vsync:`
//! log line WHEN the display's vsync flag actually CHANGES — so the fullscreen program projector
//! logs exactly one `ARMED` line at open, the multiview (flag never changes from its false
//! default) logs nothing, and the hot per-tick `gs_present_vsync()` arm path is untouched (no
//! per-frame spam). The armed state becomes machine-readable from the OBS log.
//!
//! Same vendored-source-assertion convention as `tests/gl_egl_present_vsync_1107.rs`: pure text
//! checks against the checked-in vendored C, no probe feature / GPU / rig — a plain Tier-0 test
//! (CI runs it; the vendored C compiles on linux-genlock.yml).

use std::path::PathBuf;

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse whitespace so the assertions survive reformatting/re-wrapping (same convention as
/// tests/gl_egl_present_vsync_1107.rs).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const OBS_DISPLAY_C: &str = "vendor/obs-studio/libobs/obs-display.c";
const GL_X11_EGL: &str = "vendor/obs-studio/libobs-opengl/gl-x11-egl.c";

// ---- the observability log line itself ------------------------------------------------------

#[test]
fn obs_display_set_vsync_emits_the_projector_vsync_marker() {
    let src = squish(&vendor_file(OBS_DISPLAY_C));
    assert!(
        src.contains(
            "\"projector-vsync: present-vsync %s (GL/EGL swap interval %d; no-op on D3D11)\""
        ),
        "{OBS_DISPLAY_C}: #1146 — obs_display_set_vsync() must emit a `projector-vsync:` \
         LOG_INFO line so drift-guard / the E2E [0/8] preflight can read the armed present-vsync \
         state from the OBS log (the #1107 present path only logs on eglSwapInterval FAILURE). \
         The `no-op on D3D11` wording keeps it honest: on the Windows D3D backend the optional \
         device_present_set_vsync export is NULL, so the flag is a no-op there."
    );
    assert!(
        src.contains("blog(LOG_INFO, \"projector-vsync:")
            || src.contains("blog( LOG_INFO, \"projector-vsync:"),
        "{OBS_DISPLAY_C}: #1146 — the marker must be a LOG_INFO line (visible in the default OBS \
         log), not a debug/verbose level a stripped log would drop."
    );
}

// ---- one-shot-on-change: no per-frame spam --------------------------------------------------

#[test]
fn the_marker_is_gated_on_an_actual_change_not_logged_every_call() {
    let src = squish(&vendor_file(OBS_DISPLAY_C));
    assert!(
        src.contains("const bool changed = display->vsync != vsync;"),
        "{OBS_DISPLAY_C}: #1146 — the log must be gated on the flag ACTUALLY changing \
         (`const bool changed = display->vsync != vsync;`), so it fires once per projector \
         open/close, never per call. (obs_display_set_vsync is the right home for one-shot \
         semantics; device_present_set_vsync is re-armed every present tick and would spam.)"
    );
    assert!(
        src.contains("if (changed) blog(LOG_INFO, \"projector-vsync:"),
        "{OBS_DISPLAY_C}: #1146 — the projector-vsync line must be emitted only `if (changed)`."
    );
}

// ---- the #1107 behavior + wiring it guards must stay intact ----------------------------------

#[test]
fn the_underlying_display_flag_store_is_preserved() {
    // The #1107 mechanism (obs_display_set_vsync stores display->vsync, read by render_display's
    // per-tick gs_present_vsync arm) must NOT be broken by adding the log — the store line stays.
    let src = squish(&vendor_file(OBS_DISPLAY_C));
    assert!(
        src.contains("display->vsync = vsync;"),
        "{OBS_DISPLAY_C}: #1146 — obs_display_set_vsync() must still STORE the flag \
         (`display->vsync = vsync;`) — the observability log is additive, not a replacement."
    );
}

#[test]
fn the_egl_present_still_reads_the_flag_for_the_swap_interval() {
    // Belt-and-suspenders: the observability is worthless if a vendored rebase drops the actual
    // #1107 EGL swap-interval read this ticket exists to make VERIFIABLE. Pin it here too.
    let src = squish(&vendor_file(GL_X11_EGL));
    assert!(
        src.contains("eglSwapInterval(device->plat->edisplay, device->present_vsync ? 1 : 0)"),
        "{GL_X11_EGL}: #1146 — the #1107 EGL present must still pick the swap interval from \
         device->present_vsync (1 vsync / 0 immediate). If this is gone, the projector-vsync \
         marker would report an armed flag that no longer changes the actual scanout."
    );
}
