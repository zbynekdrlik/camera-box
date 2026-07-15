//! Patch-presence guard for #756 Fix B — the vendored Linux/X11 EGL swap-chain code must CACHE
//! the client (viewport) size per swap chain instead of doing a BLOCKING `xcb_get_geometry`
//! round-trip to Xorg on every `device_set_viewport()` call.
//!
//! Root cause (#756, live autopsy of a real imag-nb wedge, 2026-07-15): `gl_getclientsize()`
//! (`gl-nix.c`) has exactly ONE caller — `device_set_viewport()` (`gl-subsystem.c:1372`),
//! invoked from `gs_viewport_pop()` for EVERY async source's texrender pop, i.e. potentially
//! dozens of times per frame with multiple open displays/projectors (imag runs a Multiview +
//! Program projector, each rendering several NDI camera sources). The stock
//! `gl_x11_egl_getclientsize()` answered every one of those calls with a fresh, SYNCHRONOUS
//! `xcb_get_geometry` round-trip to the X server.
//!
//! Captured live during the real collapse (thread stack from the on-box wedge snapshot): the
//! `libobs: graphic` thread parked in `xcb_wait_for_reply()` inside exactly this call chain
//! (`device_set_viewport -> gl_x11_egl_getclientsize -> get_window_geometry ->
//! xcb_get_geometry_reply`), while the GPU sat locked at a healthy clock (~19% util, 0.02s
//! GSP-RPC probe) and the CPU was idle — i.e. 100% wait-on-Xorg, not compute-bound. As crashed
//! GL clients accumulated (a related #756 teardown-segfault issue, tracked separately),
//! degraded Xorg reply latency multiplied straight into the render budget: program fps
//! collapsed from 60 to under 15.
//!
//! The fix: cache the client size in `struct gl_windowinfo` (gl-x11-egl.c), seeded once at
//! swap-chain creation (`gl_x11_egl_platform_init_swapchain`, from a geometry query it performs
//! anyway) and refreshed by `gl_x11_egl_update()` — which OBS's own render loop already calls
//! exactly when the display's real size changes (`render_display_begin()`, obs-display.c, only
//! calls `gs_resize()` when `display->cx/cy` actually differ from the incoming size). This keeps
//! the cache exactly in sync with the real window size on every genuine resize while eliminating
//! the per-viewport-set X round-trip entirely.
//!
//! Same vendored-source-assertion convention as `tests/gl_pbo_orphan.rs` / `tests/
//! obs_titlebar_newlevel.rs` — pure text checks against the checked-in vendored C, no probe
//! feature / GPU context needed, runs as a plain Tier-0 test (`cargo test --no-run` compiles it
//! locally; CI actually runs it).

use std::path::PathBuf;

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse whitespace so the assertions survive reformatting/re-wrapping (same convention as
/// tests/gl_pbo_orphan.rs / tests/genlock_preload.rs).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const GL_X11_EGL: &str = "vendor/obs-studio/libobs-opengl/gl-x11-egl.c";

#[test]
fn getclientsize_no_longer_does_a_blocking_x_round_trip() {
    let src = vendor_file(GL_X11_EGL);
    // Extract just the getclientsize function body so this assertion is scoped to the hot
    // per-viewport-set path, not the (legitimate, one-time) geometry query still used at
    // swap-chain creation.
    let start = src
        .find("static void gl_x11_egl_getclientsize")
        .expect("gl_x11_egl_getclientsize must exist");
    let body_start = src[start..].find('{').expect("function body must open");
    let abs_start = start + body_start;
    let body_end = src[abs_start..]
        .find("\n}\n")
        .expect("function body must close");
    let body = &src[abs_start..abs_start + body_end];

    assert!(
        !body.contains("get_window_geometry") && !body.contains("xcb_get_geometry"),
        "{GL_X11_EGL}: #756 Fix B — gl_x11_egl_getclientsize() is back to a blocking \
         xcb_get_geometry round-trip. This function is called dozens of times per frame \
         (device_set_viewport, gl-subsystem.c:1372) -- a live wedge on imag-nb proved this \
         stalls the render thread on Xorg reply latency, collapsing fps from 60 to under 15 \
         with GPU/CPU both idle. Must read a cached size instead. Body was:\n{body}"
    );
}

#[test]
fn getclientsize_reads_the_cached_size_fields() {
    let src = squish(&vendor_file(GL_X11_EGL));
    assert!(
        src.contains("*width = swap->wi->cached_cx;")
            && src.contains("*height = swap->wi->cached_cy;"),
        "{GL_X11_EGL}: #756 Fix B — gl_x11_egl_getclientsize() must read swap->wi->cached_cx/cy"
    );
}

#[test]
fn gl_windowinfo_struct_carries_the_cache_fields() {
    let src = squish(&vendor_file(GL_X11_EGL));
    let start = src
        .find("struct gl_windowinfo {")
        .expect("struct gl_windowinfo must exist");
    let end = src[start..]
        .find("};")
        .expect("struct gl_windowinfo must close");
    let body = &src[start..start + end];
    assert!(
        body.contains("uint32_t cached_cx;") && body.contains("uint32_t cached_cy;"),
        "{GL_X11_EGL}: #756 Fix B — struct gl_windowinfo must carry cached_cx/cached_cy fields: \
         {body}"
    );
}

#[test]
fn cache_is_seeded_at_swapchain_creation_and_refreshed_on_resize() {
    let src = squish(&vendor_file(GL_X11_EGL));
    // Seeded in gl_x11_egl_platform_init_swapchain (from the geometry it already fetches).
    assert!(
        src.contains("swap->wi->cached_cx = geometry->width;")
            && src.contains("swap->wi->cached_cy = geometry->height;"),
        "{GL_X11_EGL}: #756 Fix B — gl_x11_egl_platform_init_swapchain() must seed the cache \
         from the geometry it already queries at swap-chain creation (no extra X round-trip)"
    );
    // Refreshed in gl_x11_egl_update (the resize path OBS's render loop drives).
    assert!(
        src.contains("device->cur_swap->wi->cached_cx = device->cur_swap->info.cx;")
            && src.contains("device->cur_swap->wi->cached_cy = device->cur_swap->info.cy;"),
        "{GL_X11_EGL}: #756 Fix B — gl_x11_egl_update() must refresh the cache from \
         device->cur_swap->info.cx/cy on every real resize (device_resize() in gl-subsystem.c \
         always updates info.cx/cy BEFORE calling gl_update(), so these are always the NEW size)"
    );
}

#[test]
fn windowinfo_create_zero_initializes_so_the_cache_never_reads_uninitialized_bytes() {
    let src = squish(&vendor_file(GL_X11_EGL));
    assert!(
        src.contains("return bzalloc(sizeof(struct gl_windowinfo));"),
        "{GL_X11_EGL}: #756 Fix B — gl_x11_egl_windowinfo_create() must use bzalloc (not \
         bmalloc) now that the struct carries cache fields, so cached_cx/cy start at a \
         well-defined 0 rather than uninitialized heap bytes if ever read before the seed/\
         refresh sites run"
    );
}
