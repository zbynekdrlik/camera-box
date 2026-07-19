//! #793 — obs-websocket GetStats segfault: `video_output_get_skipped_frames(NULL)` /
//! `video_output_get_total_frames(NULL)` crash in `get_const_root` when a WebSocket client
//! (seed bootstrap or Bitfocus Companion) connects during the boot window while
//! `obs_get_video()` is still NULL. Proven by the 2026-07-18 11:00 core dump backtrace:
//!
//! ```text
//! #0 get_const_root (video=0x0)                       video-io.c:380
//! #1 video_output_get_skipped_frames (video=0x0)      video-io.c:618
//! #2 obs-websocket.so (GetStats, Qt pooled thread)
//! ```
//!
//! The neighbouring getters (`get_format`/`get_width`/`get_height`/`get_frame_rate`) already
//! carry upstream's NULL guard; these two were the unguarded stragglers. This test pins the
//! guard into the vendored source so a vendor bump can never silently drop it again.

use std::fs;

const VIDEO_IO: &str = "vendor/obs-studio/libobs/media-io/video-io.c";

/// Extract the body of a top-level C function starting at its exact signature line.
fn fn_body<'a>(src: &'a str, signature: &str) -> &'a str {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("signature `{signature}` not found in {VIDEO_IO}"));
    let rest = &src[start..];
    let open = rest.find('{').expect("opening brace");
    let mut depth = 0usize;
    for (i, c) in rest[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[..open + i + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces after `{signature}`");
}

#[test]
fn skipped_and_total_frames_getters_are_null_safe_793() {
    let src = fs::read_to_string(VIDEO_IO).expect("read vendored video-io.c");

    for sig in [
        "uint32_t video_output_get_skipped_frames(const video_t *video)",
        "uint32_t video_output_get_total_frames(const video_t *video)",
    ] {
        let body = fn_body(&src, sig);
        let guard = body.find("if (!video)").unwrap_or_else(|| {
            panic!("#793 regression: `{sig}` lost its NULL guard — GetStats on a booting OBS segfaults")
        });
        let deref = body
            .find("get_const_root")
            .expect("getter still routes through get_const_root");
        assert!(
            guard < deref,
            "#793 regression: `{sig}` must check `if (!video)` BEFORE get_const_root"
        );
    }
}

#[test]
fn neighbour_getters_keep_their_upstream_null_guards() {
    // The fix follows the existing idiom — if upstream ever restructures these, the #793
    // guard style must be re-checked rather than blindly merged.
    let src = fs::read_to_string(VIDEO_IO).expect("read vendored video-io.c");
    let body = fn_body(
        &src,
        "double video_output_get_frame_rate(const video_t *video)",
    );
    assert!(
        body.contains("if (!video)"),
        "upstream frame_rate NULL guard disappeared — re-audit all video_output_* getters (#793)"
    );
}

#[test]
fn canvas_mix_detaches_before_free_and_obs_get_video_is_null_safe_793() {
    // Second #793 flavor (garbage pointer, "segfault at 7e1"): obs_canvas_clear_mix used to
    // free the mix BEFORE nulling canvas->mix, so obs_get_video() on the obs-websocket pooled
    // thread could read freed memory. Pin the detach-before-free order + the NULL-safe getter.
    let canvas =
        fs::read_to_string("vendor/obs-studio/libobs/obs-canvas.c").expect("read obs-canvas.c");
    let body = fn_body(&canvas, "void obs_canvas_clear_mix(obs_canvas_t *canvas)");
    let detach = body
        .find("canvas->mix = NULL")
        .expect("detach assignment present");
    let free = body.find("obs_free_video_mix").expect("free call present");
    assert!(
        detach < free,
        "#793 regression: obs_canvas_clear_mix must DETACH canvas->mix BEFORE obs_free_video_mix"
    );

    let obs_c = fs::read_to_string("vendor/obs-studio/libobs/obs.c").expect("read obs.c");
    let getter = fn_body(&obs_c, "video_t *obs_get_video(void)");
    assert!(
        getter.contains("mix ? mix->video : NULL"),
        "#793 regression: obs_get_video lost its NULL-safe canvas/mix chain"
    );
}
