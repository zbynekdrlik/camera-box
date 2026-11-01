//! Issue 1387: stopping a recording must not hold the graphics lock across all its resources.
//!
//! The render loop holds that lock ~24 ms of each 33 ms tick. E2E 41351930723: the strih StopRecord
//! made one render tick late (lagged=1) and the JPEG XS program lost a frame (dropped_busy=1), in all
//! three runs of 1.11.2026. So each texture and interop resource takes its own short hold.
use std::path::Path;

fn body(rel: &str, signature: &str) -> String {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap();
    let start = text.find(signature).expect(signature);
    let rest = &text[start..];
    rest[..rest.find("\n}\n").expect("the function's end")].to_string()
}

#[test]
fn the_interop_resources_are_unregistered_one_graphics_hold_each() {
    let free = body(
        "vendor/obs-studio/plugins/obs-nvenc/nvenc-opengl.c",
        "void cuda_opengl_free(",
    );
    let loop_at = free.find("for (size_t i = 0;").expect("the loop");
    assert!(
        !free[..loop_at].contains("obs_enter_graphics"),
        "no hold around the whole loop"
    );
    let inner = &free[loop_at..];
    assert!(inner.find("obs_enter_graphics").unwrap() < inner.find("res_y)").unwrap());
    assert!(inner.contains("obs_leave_graphics"));
}

#[test]
fn the_stop_drains_the_textures_one_graphics_hold_at_a_time() {
    let stop = body("vendor/obs-studio/libobs/obs.c", "void stop_gpu_encode(");
    assert!(
        stop.contains("free_gpu_encoding_step(video)"),
        "the stop drains by steps"
    );
    let step = body(
        "vendor/obs-studio/libobs/obs-video-gpu-encode.c",
        "bool free_gpu_encoding_step(",
    );
    assert!(
        step.contains("video->gpu_encoders.num"),
        "a start during the drain ends it"
    );
    assert_eq!(
        step.matches("gs_texture_destroy(frame.tex);").count(),
        1,
        "one frame per step"
    );
}

#[test]
fn the_stop_reports_its_holds() {
    let stop = body("vendor/obs-studio/libobs/obs.c", "void stop_gpu_encode(");
    assert!(stop.contains("gpu-encode-stop:"));
    let free = body(
        "vendor/obs-studio/plugins/obs-nvenc/nvenc-opengl.c",
        "void cuda_opengl_free(",
    );
    assert!(free.contains("gpu-encode-stop:"));
}
