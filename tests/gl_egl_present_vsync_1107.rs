//! Patch-presence guard for #1107 — the vendored Linux/EGL winsys present must VSYNC the
//! fullscreen program projector (eglSwapInterval 1) so imag-nb's HDMI-1 IMAG scanout is
//! tear-free, WITHOUT vsyncing every other display (which would stack blocking swaps and drop
//! the 60fps render).
//!
//! Root cause (#1107, live imag-nb regression 2026-08-18): `gl_x11_egl_device_present()`
//! (gl-x11-egl.c) unconditionally called `eglSwapInterval(edisplay, 0)` before every
//! `eglSwapBuffers`, so the swap was never aligned to the panel vblank. On imag-nb (Intel
//! iGPU / modesetting, NO compositor, NO TearFree — issue 841) nothing downstream re-aligned
//! it, so horizontal motion tore on the HDMI-1 program projection. The old NVIDIA box (issue
//! 777) masked exactly this with ForceFullCompositionPipeline; Intel has nothing to mask it.
//!
//! The fix (per-display OPT-IN vsync, targeted at exactly the fullscreen program projector):
//!   - a `bool present_vsync` device flag (GL `struct gs_device`, default false = the historic
//!     interval-0 behavior), set by `device_present_set_vsync()` and READ by the EGL present as
//!     `eglSwapInterval(edisplay, device->present_vsync ? 1 : 0)`;
//!   - a NEW OPTIONAL device export `device_present_set_vsync` — because it is
//!     GRAPHICS_IMPORT_OPTIONAL, libobs-d3d11/metal do NOT implement it (NULL → `gs_present_vsync`
//!     is a no-op), so the WINDOWS D3D path (strih/stream) is byte-identical;
//!   - a `bool vsync` on `struct obs_display` + `obs_display_set_vsync()`, armed every tick in
//!     `render_display()` via `gs_present_vsync(display->vsync)` immediately before `gs_present()`;
//!   - the frontend marks ONLY a fullscreen (savedMonitor > -1) non-multiview projector, mirroring
//!     the existing #276 `isMultiview → obs_display_set_render_divisor(...,2)` pattern.
//!
//! Same vendored-source-assertion convention as `tests/gl_x11_viewport_cache_756.rs` /
//! `tests/genlock_preload.rs`: pure text checks against the checked-in vendored C/C++, no probe
//! feature / GPU context / rig — runs as a plain Tier-0 test (`cargo test --no-run` compiles it
//! locally; CI actually runs it; the vendored C itself compiles only on linux-genlock.yml).

use std::path::PathBuf;

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse whitespace so the assertions survive reformatting/re-wrapping (same convention as
/// tests/gl_x11_viewport_cache_756.rs / tests/genlock_preload.rs).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Slice the body of a `struct NAME {` ... `};` block (no nested `};` in these two structs).
fn struct_body<'a>(squished: &'a str, decl: &str) -> &'a str {
    let start = squished
        .find(decl)
        .unwrap_or_else(|| panic!("{decl} must exist"));
    let end = squished[start..]
        .find("};")
        .unwrap_or_else(|| panic!("{decl} must close"));
    &squished[start..start + end]
}

const GL_X11_EGL: &str = "vendor/obs-studio/libobs-opengl/gl-x11-egl.c";
const GL_WAYLAND_EGL: &str = "vendor/obs-studio/libobs-opengl/gl-wayland-egl.c";
const GL_SUBSYSTEM_H: &str = "vendor/obs-studio/libobs-opengl/gl-subsystem.h";
const GL_SUBSYSTEM_C: &str = "vendor/obs-studio/libobs-opengl/gl-subsystem.c";
const GRAPHICS_INTERNAL_H: &str = "vendor/obs-studio/libobs/graphics/graphics-internal.h";
const GRAPHICS_IMPORTS_C: &str = "vendor/obs-studio/libobs/graphics/graphics-imports.c";
const GRAPHICS_C: &str = "vendor/obs-studio/libobs/graphics/graphics.c";
const GRAPHICS_H: &str = "vendor/obs-studio/libobs/graphics/graphics.h";
const OBS_INTERNAL_H: &str = "vendor/obs-studio/libobs/obs-internal.h";
const OBS_DISPLAY_C: &str = "vendor/obs-studio/libobs/obs-display.c";
const OBS_H: &str = "vendor/obs-studio/libobs/obs.h";
const OBS_PROJECTOR_CPP: &str = "vendor/obs-studio/frontend/widgets/OBSProjector.cpp";

// ---- the two EGL present sites read the device flag -----------------------------------------

#[test]
fn x11_egl_present_vsyncs_off_the_device_flag() {
    let src = squish(&vendor_file(GL_X11_EGL));
    assert!(
        src.contains("eglSwapInterval(device->plat->edisplay, device->present_vsync ? 1 : 0)"),
        "{GL_X11_EGL}: #1107 — gl_x11_egl_device_present() must set eglSwapInterval from \
         device->present_vsync (1 vsync / 0 immediate), NOT hardcode 0. A hardcoded 0 re-tears \
         the imag-nb HDMI-1 program projection (no compositor, no TearFree on Intel)."
    );
    assert!(
        !src.contains("eglSwapInterval(device->plat->edisplay, 0)"),
        "{GL_X11_EGL}: #1107 — the old unconditional eglSwapInterval(..., 0) is back."
    );
}

#[test]
fn wayland_egl_present_vsyncs_off_the_device_flag() {
    let src = squish(&vendor_file(GL_WAYLAND_EGL));
    assert!(
        src.contains("eglSwapInterval(plat->display, device->present_vsync ? 1 : 0)"),
        "{GL_WAYLAND_EGL}: #1107 — the wayland twin must read device->present_vsync too (parity \
         with the x11 present), NOT hardcode eglSwapInterval(..., 0)."
    );
    assert!(
        !src.contains("eglSwapInterval(plat->display, 0)"),
        "{GL_WAYLAND_EGL}: #1107 — the old unconditional eglSwapInterval(..., 0) is back."
    );
}

// ---- the device flag + its setter -----------------------------------------------------------

#[test]
fn gs_device_carries_the_present_vsync_flag() {
    let src = squish(&vendor_file(GL_SUBSYSTEM_H));
    let body = struct_body(&src, "struct gs_device {");
    assert!(
        body.contains("bool present_vsync;"),
        "{GL_SUBSYSTEM_H}: #1107 — struct gs_device must carry `bool present_vsync;` (default \
         false via bzalloc = the historic interval-0 behavior)."
    );
}

#[test]
fn device_present_set_vsync_impl_stores_the_flag() {
    let src = squish(&vendor_file(GL_SUBSYSTEM_C));
    assert!(
        src.contains("void device_present_set_vsync(gs_device_t *device, bool vsync)"),
        "{GL_SUBSYSTEM_C}: #1107 — device_present_set_vsync() export must exist so \
         graphics-imports.c can GRAPHICS_IMPORT_OPTIONAL it."
    );
    assert!(
        src.contains("device->present_vsync = vsync;"),
        "{GL_SUBSYSTEM_C}: #1107 — device_present_set_vsync() must store the flag on the device."
    );
}

// ---- the graphics-layer plumbing: OPTIONAL export keeps D3D11 byte-identical -----------------

#[test]
fn graphics_vtable_has_the_optional_present_set_vsync_member() {
    let src = squish(&vendor_file(GRAPHICS_INTERNAL_H));
    assert!(
        src.contains("void (*device_present_set_vsync)(gs_device_t *device, bool vsync);"),
        "{GRAPHICS_INTERNAL_H}: #1107 — the gs_exports vtable must carry device_present_set_vsync."
    );
}

#[test]
fn present_set_vsync_is_imported_optional_so_d3d11_is_untouched() {
    let src = squish(&vendor_file(GRAPHICS_IMPORTS_C));
    assert!(
        src.contains("GRAPHICS_IMPORT_OPTIONAL(device_present_set_vsync);"),
        "{GRAPHICS_IMPORTS_C}: #1107 — device_present_set_vsync MUST be imported as OPTIONAL \
         (NOT GRAPHICS_IMPORT): the D3D11/Metal backends do not define it, and a mandatory \
         import would fail their module load. Optional → NULL → gs_present_vsync no-op → the \
         Windows strih/stream D3D present path is byte-identical."
    );
    assert!(
        !src.contains("GRAPHICS_IMPORT(device_present_set_vsync);"),
        "{GRAPHICS_IMPORTS_C}: #1107 — device_present_set_vsync must be OPTIONAL, not mandatory."
    );
}

#[test]
fn gs_present_vsync_wrapper_is_null_guarded() {
    let src = squish(&vendor_file(GRAPHICS_C));
    assert!(
        src.contains("if (graphics->exports.device_present_set_vsync)")
            && src.contains("graphics->exports.device_present_set_vsync(graphics->device, vsync);"),
        "{GRAPHICS_C}: #1107 — gs_present_vsync() must NULL-check the optional export before \
         calling it (D3D11/Metal leave it NULL)."
    );
    let hdr = squish(&vendor_file(GRAPHICS_H));
    assert!(
        hdr.contains("EXPORT void gs_present_vsync(bool vsync);"),
        "{GRAPHICS_H}: #1107 — gs_present_vsync must be EXPORTed."
    );
}

// ---- the obs_display flag, its setter, and the per-tick arm ----------------------------------

#[test]
fn obs_display_carries_the_vsync_flag() {
    let src = squish(&vendor_file(OBS_INTERNAL_H));
    let body = struct_body(&src, "struct obs_display {");
    assert!(
        body.contains("bool vsync;"),
        "{OBS_INTERNAL_H}: #1107 — struct obs_display must carry `bool vsync;`."
    );
}

#[test]
fn obs_display_set_vsync_impl_and_export_exist() {
    let src = squish(&vendor_file(OBS_DISPLAY_C));
    assert!(
        src.contains("void obs_display_set_vsync(obs_display_t *display, bool vsync)")
            && src.contains("display->vsync = vsync;"),
        "{OBS_DISPLAY_C}: #1107 — obs_display_set_vsync() must store the flag on the display."
    );
    let api = squish(&vendor_file(OBS_H));
    assert!(
        api.contains("EXPORT void obs_display_set_vsync(obs_display_t *display, bool vsync);"),
        "{OBS_H}: #1107 — obs_display_set_vsync must be EXPORTed so the frontend can link it."
    );
}

#[test]
fn render_display_arms_present_vsync_immediately_before_gs_present() {
    let src = squish(&vendor_file(OBS_DISPLAY_C));
    assert!(
        src.contains("gs_present_vsync(display->vsync); gs_present();"),
        "{OBS_DISPLAY_C}: #1107 — render_display() must call gs_present_vsync(display->vsync) \
         IMMEDIATELY before gs_present(), re-armed every tick per display (single graphics \
         thread → per-display-correct even across swapchain recreation)."
    );
}

// ---- the frontend marks ONLY the fullscreen non-multiview (program) projector ---------------

#[test]
fn projector_marks_only_the_fullscreen_nonmultiview_program_display() {
    let src = squish(&vendor_file(OBS_PROJECTOR_CPP));
    assert!(
        src.contains("if (savedMonitor > -1 && !isMultiview)"),
        "{OBS_PROJECTOR_CPP}: #1107 — vsync must be armed ONLY for a fullscreen \
         (savedMonitor > -1) NON-multiview projector — the program output. `render_divisor <= 1` \
         is NOT a usable discriminator: the OBS main-window preview is also divisor-0, on a \
         DIFFERENT monitor (eDP-1) than the program projector (HDMI-1), so vsyncing it would \
         stack a second blocking present and risk dropping the 60fps render."
    );
    assert!(
        src.contains("obs_display_set_vsync(GetDisplay(), true)"),
        "{OBS_PROJECTOR_CPP}: #1107 — the fullscreen non-multiview projector must call \
         obs_display_set_vsync(GetDisplay(), true), mirroring the #276 isMultiview→divisor mark."
    );
}

#[test]
fn projector_rearms_vsync_on_the_runtime_fullscreen_windowed_toggles() {
    // The DisplayCreated mark runs ONCE; the runtime toggles mutate fullscreen state after, so
    // they must re-arm/clear vsync or a windowed→fullscreen program tears again and a
    // fullscreen→windowed projector keeps a needless blocking present (#1107 review 🟡).
    let src = squish(&vendor_file(OBS_PROJECTOR_CPP));
    assert!(
        src.contains("obs_display_set_vsync(GetDisplay(), type != ProjectorType::Multiview);"),
        "{OBS_PROJECTOR_CPP}: #1107 — OpenFullScreenProjector() must re-arm vsync for a \
         non-multiview (program) projector when it becomes fullscreen at runtime."
    );
    assert!(
        src.contains("obs_display_set_vsync(GetDisplay(), false);"),
        "{OBS_PROJECTOR_CPP}: #1107 — OpenWindowedProjector() must CLEAR vsync so a windowed \
         projector does not keep a blocking vblank present acquired while fullscreen."
    );
}
