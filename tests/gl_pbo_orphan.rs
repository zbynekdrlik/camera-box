//! Patch-presence guard for #501/#505 — the vendored Linux GL texture-upload path must
//! ORPHAN its persistent Pixel Unpack Buffer (PBO) on every map, not block on it.
//!
//! Root cause (#505, deep code investigation): `vendor/obs-studio/libobs-opengl/gl-texture2d.c`
//! allocates ONE persistent PBO per dynamic 2D texture (`create_pixel_unpack_buffer`) and reuses
//! it every frame with no re-specification. `gs_texture_map()` then called the bare
//! `glMapBuffer(GL_PIXEL_UNPACK_BUFFER, GL_WRITE_ONLY)` on that buffer — since the driver cannot
//! hand back a CPU-writable pointer until the GPU has finished consuming the PREVIOUS frame's
//! upload from that same storage, this becomes an *implicit* CPU<->GPU sync/fence. Measured live
//! on imag-nb: a 6-source OBS multiview (each source re-uploads its async texture every render)
//! cost ~24ms/frame with the CPU at 101% of one core and the GPU idle 7-9% — a classic
//! "GL buffer streaming without orphaning" stall. The Windows/D3D11 backend never pays this cost
//! because it maps with `D3D11_MAP_WRITE_DISCARD`, which never blocks (see
//! `vendor/obs-studio/libobs-d3d11/d3d11-subsystem.cpp`).
//!
//! The fix: map with `glMapBufferRange(..., GL_MAP_WRITE_BIT | GL_MAP_INVALIDATE_BUFFER_BIT)`
//! instead of the bare `glMapBuffer`. `GL_MAP_INVALIDATE_BUFFER_BIT` tells the driver the entire
//! buffer's prior contents may be discarded, so it can hand back a *fresh* backing allocation
//! immediately instead of waiting on the GPU — the GL analog of `D3D11_MAP_WRITE_DISCARD`. This
//! is NOT a new idiom in this codebase: `vendor/obs-studio/libobs-opengl/gl-helpers.c`'s
//! `update_buffer()` (used for vertex/index buffer streaming) already uses exactly this pattern,
//! with the comment "glMapBufferRange with these flags will actually give far better performance
//! than a plain glMapBuffer call" — `gs_texture_map`'s PBO path was simply never brought in line
//! with it.
//!
//! Investigation note (documented here, not silently assumed): `gl-texture3d.c` also allocates a
//! persistent unpack PBO (`create_pixel_unpack_buffer` for `gs_texture_3d`), but this vendored
//! OBS has NO `gs_texture_map`/`gs_voltexture_map` implementation for `GS_TEXTURE_3D` on the GL
//! backend at all — `gs_texture_map()` (gl-texture2d.c) hard-checks `is_texture_2d()` and fails
//! for any other texture type. So the 3D PBO is allocated but never `glMapBuffer`'d anywhere in
//! this tree today; there is no live instance of the anti-pattern to mirror-fix there. This is
//! asserted below so a future change that DOES add a 3D map path is required to update this test.
//!
//! Same vendored-source-assertion convention as `tests/obs_titlebar_newlevel.rs` — pure text
//! checks against the checked-in vendored C, no probe feature / GPU context needed, so this runs
//! as a plain Tier-0 test (`cargo test --no-run` compiles it locally; CI actually runs it).

use std::path::PathBuf;

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse whitespace so the assertions survive reformatting/re-wrapping (same convention as
/// tests/obs_titlebar_newlevel.rs / tests/genlock_preload.rs).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const GL_TEXTURE2D: &str = "vendor/obs-studio/libobs-opengl/gl-texture2d.c";
const GL_TEXTURE3D: &str = "vendor/obs-studio/libobs-opengl/gl-texture3d.c";
const GL_HELPERS: &str = "vendor/obs-studio/libobs-opengl/gl-helpers.c";

#[test]
fn texture2d_map_no_longer_blocks_on_a_bare_glmapbuffer() {
    let src = squish(&vendor_file(GL_TEXTURE2D));
    assert!(
        !src.contains("glMapBuffer(GL_PIXEL_UNPACK_BUFFER, GL_WRITE_ONLY)"),
        "{GL_TEXTURE2D}: #505 — gs_texture_map() is back to the bare, un-orphaned \
         glMapBuffer(GL_PIXEL_UNPACK_BUFFER, GL_WRITE_ONLY) call. On Linux this forces an \
         implicit CPU<->GPU sync every texture upload (measured ~24ms/frame stall on a 6-source \
         multiview, imag-nb). Re-apply the glMapBufferRange + INVALIDATE_BUFFER_BIT patch."
    );
}

#[test]
fn texture2d_map_uses_glmapbufferrange_with_invalidate_buffer_bit() {
    let src = squish(&vendor_file(GL_TEXTURE2D));
    assert!(
        src.contains(
            "*ptr = glMapBufferRange(GL_PIXEL_UNPACK_BUFFER, 0, pixel_unpack_buffer_size(tex2d), \
             GL_MAP_WRITE_BIT | GL_MAP_INVALIDATE_BUFFER_BIT);"
        ),
        "{GL_TEXTURE2D}: #505 — gs_texture_map() must map the persistent PBO with \
         glMapBufferRange(GL_PIXEL_UNPACK_BUFFER, 0, <size>, GL_MAP_WRITE_BIT | \
         GL_MAP_INVALIDATE_BUFFER_BIT) so the driver can hand back a fresh allocation instead of \
         blocking on the GPU finishing the previous frame's upload. Re-apply the patch."
    );
    assert!(
        src.contains(r#"gl_success("glMapBufferRange")"#),
        "{GL_TEXTURE2D}: #505 — the error-check after mapping must reference \"glMapBufferRange\" \
         (the old \"glMapBuffer\" gl_success() label would now be misleading/wrong)."
    );
}

/// The byte-size formula (previously inlined only in `create_pixel_unpack_buffer`) must be a
/// single shared helper so `gs_texture_map`'s `glMapBufferRange` call and the buffer's actual
/// allocation size can never silently diverge.
#[test]
fn pixel_unpack_buffer_size_is_a_single_helper_shared_by_create_and_map() {
    let src = squish(&vendor_file(GL_TEXTURE2D));
    assert!(
        src.contains("static GLsizeiptr pixel_unpack_buffer_size("),
        "{GL_TEXTURE2D}: #505 — the shared pixel_unpack_buffer_size() helper is gone; \
         create_pixel_unpack_buffer() and gs_texture_map() must compute the PBO byte size from \
         exactly one place so they cannot drift apart."
    );
    assert!(
        src.matches("pixel_unpack_buffer_size(").count() >= 3,
        // 1 definition + create_pixel_unpack_buffer() call-site + gs_texture_map() call-site.
        // >= (not ==) so an incidental future doc-comment mentioning the function name with a
        // literal "(" can't spuriously fail this test — only a REGRESSION (a call-site removed
        // or the size recomputed inline again) can.
        "{GL_TEXTURE2D}: #505 — expected at least one definition and two call-sites of \
         pixel_unpack_buffer_size() (create_pixel_unpack_buffer + gs_texture_map)."
    );
}

/// Documents (and pins) that this fix is NOT a novel idiom: `gl-helpers.c`'s `update_buffer()`
/// already streams vertex/index buffers via glMapBufferRange + GL_MAP_INVALIDATE_BUFFER_BIT. If
/// this precedent ever regresses back to a bare glMapBuffer, the same imag-nb-class stall risk
/// applies to whatever calls update_buffer() too.
#[test]
fn the_invalidate_buffer_idiom_is_the_established_precedent_in_this_codebase() {
    let src = squish(&vendor_file(GL_HELPERS));
    assert!(
        src.contains(
            "glMapBufferRange(target, 0, size, GL_MAP_WRITE_BIT | GL_MAP_INVALIDATE_BUFFER_BIT)"
        ),
        "{GL_HELPERS}: update_buffer() no longer maps with glMapBufferRange + \
         GL_MAP_INVALIDATE_BUFFER_BIT — this was the established precedent #505's PBO fix \
         mirrors. If this regressed, re-check whether it also reintroduces a CPU-stall."
    );
}

/// #505 investigation finding: gl-texture3d.c's dynamic-texture PBO is allocated but this
/// vendored OBS has NO glMapBuffer/glMapBufferRange call anywhere in the file — 3D textures
/// cannot be mapped at all on the GL backend (`gs_texture_map` in gl-texture2d.c hard-fails any
/// non-2D texture via `is_texture_2d()`). So there is no live instance of the #505 anti-pattern
/// to mirror-fix here today. This test PINS that finding: if a future change adds a map call to
/// this file, it must go through the same glMapBufferRange + GL_MAP_INVALIDATE_BUFFER_BIT idiom
/// as gl-texture2d.c (see the two tests above) — this failing is the trigger to apply it here too.
#[test]
fn texture3d_has_no_live_map_call_to_mirror_fix_yet() {
    let src = vendor_file(GL_TEXTURE3D);
    assert!(
        !src.contains("glMapBuffer"),
        "{GL_TEXTURE3D}: a glMapBuffer/glMapBufferRange call was added to this file. #505 found \
         gl-texture3d.c had no reachable 3D-texture map path (gs_texture_map is 2D-only), so the \
         PBO-orphaning fix was applied only to gl-texture2d.c. Now that a map call exists here, \
         apply the SAME glMapBufferRange(..., GL_MAP_WRITE_BIT | GL_MAP_INVALIDATE_BUFFER_BIT) \
         fix (see texture2d_map_uses_glmapbufferrange_with_invalidate_buffer_bit) instead of a \
         bare glMapBuffer, or it will reproduce the same CPU-stall anti-pattern for 3D textures."
    );
}
