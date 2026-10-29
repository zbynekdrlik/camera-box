//! Issue 1387 slice 3 -- the strih OBS JPEG XS PROGRAM output (`vendor/distroav/src/jxs-output.cpp`)
//! and the stream side that receives it.
//!
//! CI's strih Linux build is the plugin's first real compile; this gate runs on every push:
//! 1. the pure decisions (`jxs-output-decide.h`) are compiled as-is into a truth table and run;
//! 2. the whole output is syntax-checked with g++ against the vendored libobs headers and the two
//!    FFI headers (`-Wall -Wextra -Werror`);
//! 3. the genlock contract is anchored: every frame is stamped with the SAME
//!    `genlock_emit_timecode_100ns` the NDI output stamps (one definition, declared in a shared
//!    header), the program's raw video goes to `cbjxs_send_frame` in its own pixel format, the
//!    output registers as a wall-stamping genlock sender;
//! 4. the output is registered, created from its settings file and started only when that file
//!    enables it, stopped before a video reset (profile change) and at exit; the CMake block and
//!    the strih CI job build it;
//! 5. the receiving source hands a 4:2:0 / 4:4:4 frame to libobs in its own format.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

const OUTPUT: &str = "vendor/distroav/src/jxs-output.cpp";
const DECIDE: &str = "vendor/distroav/src/jxs-output-decide.h";
const EMIT_HEADER: &str = "vendor/distroav/src/genlock-emit-timecode.h";
const NDI_OUTPUT: &str = "vendor/distroav/src/ndi-output.cpp";
const SOURCE: &str = "vendor/distroav/src/jxs-source.cpp";
const PLUGIN_MAIN: &str = "vendor/distroav/src/plugin-main.cpp";

static RUNS: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> tempfile::TempDir {
    let n = RUNS.fetch_add(1, Ordering::SeqCst);
    tempfile::Builder::new()
        .prefix(&format!("jxs_output_1387-{tag}-{n}-"))
        .tempdir()
        .unwrap()
}

fn run(cmd: &mut Command, what: &str) -> String {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("{what}: cannot run ({e}); this gate must FAIL, never skip"));
    assert!(
        out.status.success(),
        "{what} failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The text of a C function: from its signature to the first line that is only `}`.
fn function_body<'a>(src: &'a str, signature: &str) -> &'a str {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("missing `{signature}`"));
    assert_eq!(src.matches(signature).count(), 1, "`{signature}` twice");
    let end = src[start..].find("\n}\n").expect("function end") + start;
    &src[start..end]
}

const TRUTH_TABLE: &str = r#"
#include <stdio.h>
#include "@DECIDE@"
static int fails = 0;
#define EXPECT(c) do { if (!(c)) { printf("FAIL line %d: %s\n", __LINE__, #c); fails++; } } while (0)
int main(void)
{
	/* the program's stream id: 1..255, outside the CAM range by default; 0 / out of range = 100 */
	EXPECT(cb_jxs_out_stream_id(100) == 100u);
	EXPECT(cb_jxs_out_stream_id(7) == 7u);
	EXPECT(cb_jxs_out_stream_id(255) == 255u);
	EXPECT(cb_jxs_out_stream_id(0) == CB_JXS_OUT_STREAM_ID_DEFAULT);
	EXPECT(cb_jxs_out_stream_id(256) == CB_JXS_OUT_STREAM_ID_DEFAULT);
	EXPECT(cb_jxs_out_stream_id(-1) == CB_JXS_OUT_STREAM_ID_DEFAULT);
	EXPECT(CB_JXS_OUT_STREAM_ID_DEFAULT == 100u);
	/* the bpp bounds: 0 = the sender's default, 1..12, anything else 0 */
	EXPECT(cb_jxs_out_bpp(0) == 0u);
	EXPECT(cb_jxs_out_bpp(4) == 4u);
	EXPECT(cb_jxs_out_bpp(12) == 12u);
	EXPECT(cb_jxs_out_bpp(13) == 0u);
	EXPECT(cb_jxs_out_bpp(-3) == 0u);
	/* the canvas frame rate, rounded; a broken one is 0 */
	EXPECT(cb_jxs_out_fps(30.0) == 30u);
	EXPECT(cb_jxs_out_fps(29.97) == 30u);
	EXPECT(cb_jxs_out_fps(59.94) == 60u);
	EXPECT(cb_jxs_out_fps(0.0) == 0u);
	EXPECT(cb_jxs_out_fps(-30.0) == 0u);
	EXPECT(cb_jxs_out_fps(1e12) == 0u);
	/* the forwarded sender lines: everything but the sender's own 5 s status line (the output
	 * logs its own, named, from the same window) */
	EXPECT(cb_jxs_out_forward_line("jxs send: link ON to 10.77.9.204:7100 (stream id 100)"));
	EXPECT(cb_jxs_out_forward_line("jxs send: bpp 4 -> 5 (clean-10s; ...)"));
	EXPECT(!cb_jxs_out_forward_line("jxs send: fps=30.0 bpp=5 auto=on"));
	EXPECT(!cb_jxs_out_forward_line(0));
	printf("checked, %d failure(s)\n", fails);
	return fails ? 1 : 0;
}
"#;

#[test]
fn the_pure_decisions_hold_their_truth_table_compiled_from_the_shipped_header() {
    let dir = scratch("truth");
    let src = dir.path().join("truth.c");
    let bin = dir.path().join("truth.bin");
    fs::write(
        &src,
        TRUTH_TABLE.replace("@DECIDE@", &repo(DECIDE).display().to_string()),
    )
    .unwrap();
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    run(
        Command::new(&cc)
            .args(["-std=c11", "-Wall", "-Wextra", "-Wconversion", "-Werror"])
            .arg(&src)
            .arg("-o")
            .arg(&bin),
        "compile the truth table",
    );
    let out = run(&mut Command::new(&bin), "the truth table");
    assert!(out.contains("checked, 0 failure(s)"), "{out}");
}

#[test]
fn the_whole_output_compiles_against_the_vendored_libobs_and_both_ffi_headers() {
    let dir = scratch("syntax");
    let inc = dir.path().join("inc");
    fs::create_dir_all(&inc).unwrap();
    fs::write(
        inc.join("obsconfig.h"),
        "#pragma once\n#define OBS_RELEASE_CANDIDATE 0\n#define OBS_BETA 0\n",
    )
    .unwrap();
    let cxx = std::env::var("CXX").unwrap_or_else(|_| "g++".to_string());
    for file in [OUTPUT, SOURCE] {
        run(
            Command::new(&cxx)
                .args([
                    "-std=c++17",
                    "-fsyntax-only",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-DCAMERA_BOX_JXS_SOURCE=1",
                ])
                .arg("-I")
                .arg(&inc)
                .arg("-I")
                .arg(repo("vendor/obs-studio/libobs"))
                .arg("-I")
                .arg(repo("vendor/distroav/src"))
                .arg("-I")
                .arg(repo("jxs/recv-ffi/include"))
                .arg("-I")
                .arg(repo("jxs/send-ffi/include"))
                .arg(repo(file)),
            &format!("g++ -fsyntax-only {file}"),
        );
    }
}

#[test]
fn every_frame_carries_the_one_genlock_emit_stamp_the_ndi_output_uses() {
    // ONE definition, in ndi-output.cpp, declared in the shared header both outputs include
    let header = read(EMIT_HEADER);
    assert!(header.contains("int64_t genlock_emit_timecode_100ns(double framerate);"));
    let ndi = read(NDI_OUTPUT);
    assert!(ndi.contains("#include \"genlock-emit-timecode.h\""));
    assert!(
        !ndi.contains("static int64_t genlock_emit_timecode_100ns("),
        "the stamp is shared, not file-static"
    );
    assert_eq!(
        ndi.matches("int64_t genlock_emit_timecode_100ns(double framerate)\n{")
            .count(),
        1
    );
    // the anchored lines of the NDI output stay as they were (#78, #1009 gates)
    assert!(ndi.contains("video_frame.timecode = genlock_emit_timecode_100ns(o->video_framerate);"));
    assert!(ndi.contains("return genlock_floor_boundary_100ns(genlock_wall_now_100ns(), fps);"));
    let out = read(OUTPUT);
    assert!(out.contains("#include \"genlock-emit-timecode.h\""));
    assert!(
        !out.contains("genlock_floor_boundary_100ns") && !out.contains("system_clock"),
        "no second copy of the stamp"
    );
    let raw = function_body(&out, "static void jxs_output_rawvideo(");
    for needle in [
        "pic.timecode_100ns = genlock_emit_timecode_100ns(o->framerate);",
        "pic.format = o->send_format;",
        "pic.planes[i] = frame->data[i];",
        "pic.strides[i] = frame->linesize[i];",
        "cbjxs_send_frame(o->tx, &pic)",
        "std::lock_guard<std::mutex> lock(o->tx_mutex);",
    ] {
        assert!(raw.contains(needle), "jxs_output_rawvideo lacks `{needle}`");
    }
    // the copy is all it does on libobs' video thread: no wait, no sleep
    for banned in ["sleep", "join", "cbjxs_send_open", "cbjxs_send_close"] {
        assert!(!raw.contains(banned), "raw_video must never `{banned}`");
    }
}

#[test]
fn the_program_pixel_format_maps_onto_the_sender_formats_and_nothing_else_starts() {
    let out = read(OUTPUT);
    let map = function_body(&out, "static uint32_t jxs_output_send_format(");
    for (obs, send) in [
        ("VIDEO_FORMAT_NV12", "CBJXS_SEND_NV12"),
        ("VIDEO_FORMAT_I420", "CBJXS_SEND_I420"),
        ("VIDEO_FORMAT_I422", "CBJXS_SEND_I422"),
        ("VIDEO_FORMAT_I444", "CBJXS_SEND_I444"),
    ] {
        assert!(
            map.contains(&format!("case {obs}:\n\t\treturn {send};")),
            "{obs} -> {send}"
        );
    }
    assert!(map.contains("default:\n\t\treturn 0;"));
    let start = function_body(&out, "static bool jxs_output_start(");
    for needle in [
        "jxs_output_send_format(video_output_get_format(video))",
        "cbjxs_send_open(&cfg, err, sizeof err)",
        "obs_output_begin_data_capture(o->output, 0)",
        "obs_output_set_genlock_wall_stamping(o->output, true);",
        "obs_output_set_last_error(o->output,",
    ] {
        assert!(start.contains(needle), "start lacks `{needle}`");
    }
    // a failed capture start leaves no sender behind (the #1185 leak class)
    let fail = &start[start.find("if (!obs_output_begin_data_capture").unwrap()..];
    assert!(fail[..fail.find("return false;").unwrap()].contains("cbjxs_send_close(tx);"));
    let stop = function_body(&out, "static void jxs_output_stop(");
    let end = stop
        .find("obs_output_end_data_capture(o->output);")
        .unwrap();
    let close = stop.find("cbjxs_send_close(").unwrap();
    assert!(
        end < close,
        "no raw_video can run once the sender is closed"
    );
    assert!(stop.contains("obs_output_set_genlock_wall_stamping(o->output, false);"));
    let info = function_body(&out, "struct obs_output_info create_jxs_output_info()");
    for needle in [
        "info.id = JXS_OUTPUT_ID;",
        "info.flags = OBS_OUTPUT_VIDEO;",
        "info.raw_video = jxs_output_rawvideo;",
    ] {
        assert!(info.contains(needle), "{needle}");
    }
    assert!(out.contains("#define JXS_OUTPUT_ID \"camera_box_jxs_output\""));
}

#[test]
fn the_output_logs_a_start_line_a_status_line_and_the_senders_own_lines() {
    let out = read(OUTPUT);
    assert!(out.contains("\"jxs-output '%s': started -- "));
    assert!(
        out.contains("\"jxs-output '%s': fps=%.2f frames=%\" PRIu64 \" dropped_busy=%\" PRIu64")
    );
    let status = function_body(&out, "static void jxs_output_log_status(");
    for field in [
        "encode_ms_avg/max",
        "bpp=",
        "reports=",
        "copy_ms_avg/max",
        "report_age_s",
    ] {
        assert!(status.contains(field), "status line lacks {field}");
    }
    // the sender's own lines (link ON, encoder open, bpp steps, the wedge alarm) reach the OBS log
    assert_eq!(
        out.matches("cbjxs_send_set_log(jxs_output_forward_log)")
            .count(),
        1,
        "installed once, by the output"
    );
    assert!(!read(PLUGIN_MAIN).contains("cbjxs_send_set_log("));
    let fwd = function_body(&out, "static void jxs_output_forward_log(");
    assert!(fwd.contains("cb_jxs_out_forward_line(message)"));
    // the marker is never mistaken for a libobs genlock audit line
    let status_line = "info: [distroav] jxs-output 'camera-box JPEG XS program': fps=30.00 \
                       frames=150 dropped_busy=0 encode_ms_avg/max=7.10/9.00 bpp=6 reports=5";
    assert!(camera_box::jitter_audit::parse_audit_line(status_line).is_none());
}

#[test]
fn the_output_is_created_from_its_settings_file_and_started_only_when_it_says_so() {
    let out = read(OUTPUT);
    let init = function_body(&out, "void camera_box_jxs_output_init()");
    for needle in [
        "obs_module_config_path(\"jxs-output.json\")",
        "obs_data_create_from_json_file_safe(path, \"bak\")",
        "obs_output_create(JXS_OUTPUT_ID, JXS_OUTPUT_NAME, settings, nullptr)",
        "obs_data_get_bool(file, \"enabled\")",
        "obs_output_start(g_jxs_output)",
    ] {
        assert!(init.contains(needle), "init lacks `{needle}`");
    }
    let started = init.find("obs_output_start(g_jxs_output)").unwrap();
    let gate = init[..started]
        .rfind("if (enabled")
        .expect("start gated on enabled");
    assert!(gate < started);
    let deinit = function_body(&out, "void camera_box_jxs_output_deinit()");
    let stop = deinit.find("obs_output_stop(g_jxs_output);").unwrap();
    let release = deinit.find("obs_output_release(g_jxs_output);").unwrap();
    assert!(stop < release);
    assert!(out.contains("#define JXS_OUTPUT_NAME \"camera-box JPEG XS program\""));
}

#[test]
fn the_output_is_registered_and_wired_into_the_frontend_lifecycle_only_in_the_strih_build() {
    let main = read(PLUGIN_MAIN);
    let guarded = |needle: &str| {
        assert_eq!(main.matches(needle).count(), 1, "{needle}");
        let at = main.find(needle).unwrap();
        let guard = main[..at]
            .rfind("#ifdef CAMERA_BOX_JXS_SOURCE")
            .unwrap_or_else(|| panic!("{needle} unguarded"));
        assert!(!main[guard..at].contains("#endif"), "{needle} unguarded");
    };
    guarded("obs_register_output(&jxs_output_info);");
    // created after the NDI outputs at load, stopped before a video reset and at exit, re-created
    // after a profile change
    assert_eq!(
        main.matches("camera_box_jxs_output_init();").count()
            - main.matches("void camera_box_jxs_output_init();").count(),
        2,
        "two calls besides the declaration"
    );
    assert_eq!(
        main.matches("camera_box_jxs_output_deinit();").count()
            - main.matches("void camera_box_jxs_output_deinit();").count(),
        2,
        "two calls besides the declaration"
    );
    let lambda = &main[main.find("OBS_FRONTEND_EVENT_FINISHED_LOADING) {").unwrap()..];
    let exit = lambda.find("OBS_FRONTEND_EVENT_EXIT").unwrap();
    let changing = lambda.find("OBS_FRONTEND_EVENT_PROFILE_CHANGING").unwrap();
    let changed = lambda.find("OBS_FRONTEND_EVENT_PROFILE_CHANGED").unwrap();
    let first_init = lambda.find("camera_box_jxs_output_init();").unwrap();
    assert!(lambda[..exit].contains("main_output_init();") && first_init < exit);
    let first_deinit = lambda.find("camera_box_jxs_output_deinit();").unwrap();
    assert!(exit < first_deinit && first_deinit < changing);
    let second_deinit = lambda[first_deinit + 1..]
        .find("camera_box_jxs_output_deinit();")
        .unwrap()
        + first_deinit
        + 1;
    assert!(changing < second_deinit && second_deinit < changed);
    let second_init = lambda[first_init + 1..]
        .find("camera_box_jxs_output_init();")
        .unwrap()
        + first_init
        + 1;
    assert!(changed < second_init);

    let cm = read("vendor/distroav/CMakeLists.txt");
    let block_start = cm.find("if(CAMERA_BOX_JXS_SOURCE)").unwrap();
    let block = &cm[block_start..block_start + cm[block_start..].find("\nendif()").unwrap()];
    for needle in [
        "src/jxs-output.cpp",
        "src/jxs-output-decide.h",
        "src/genlock-emit-timecode.h",
        "CAMERA_BOX_JXS_SEND_FFI_INCLUDE",
        "${CAMERA_BOX_JXS_SEND_FFI_INCLUDE}/camera_box_jxs_send.h",
    ] {
        assert!(block.contains(needle), "the CMake block lacks `{needle}`");
    }
    assert_eq!(cm.matches("src/jxs-output.cpp").count(), 1);
}

#[test]
fn the_strih_build_hands_the_sender_header_to_distroav_and_checks_the_output_is_in() {
    let wf = read(".github/workflows/linux-genlock.yml");
    assert!(
        wf.contains("-DCAMERA_BOX_JXS_SEND_FFI_INCLUDE=\"$GITHUB_WORKSPACE/jxs/send-ffi/include\"")
    );
    assert!(wf.contains("grep -aq 'camera_box_jxs_output' \"$distroav_so\""));
    // the ONE archive carries the sender: its symbols are checked in the built archive
    assert!(wf.contains("nm -g --defined-only target/release/libjxs_recv_ffi.a"));
    assert!(wf.contains("cbjxs_send_open"));
    for p in [
        "'src/jxs_send.rs'",
        "'src/jxs_send/**'",
        "'src/affinity.rs'",
    ] {
        assert!(wf.contains(p), "trigger path {p}");
    }
    let ci = read(".github/workflows/ci.yml");
    assert!(ci.contains("run: cargo clippy -p jxs-send-ffi --all-targets -- -D warnings"));
    assert!(ci.contains("run: cargo nextest run -p jxs-send-ffi"));
}

#[test]
fn the_receiving_source_hands_a_420_or_444_frame_to_libobs_in_its_own_format() {
    let s = read(SOURCE);
    let map = function_body(&s, "static enum video_format jxs_video_format(");
    for (chroma, fmt) in [
        ("CBJXS_CHROMA_420", "VIDEO_FORMAT_I420"),
        ("CBJXS_CHROMA_422", "VIDEO_FORMAT_I422"),
        ("CBJXS_CHROMA_444", "VIDEO_FORMAT_I444"),
    ] {
        assert!(
            map.contains(&format!("case {chroma}:\n\t\treturn {fmt};")),
            "{chroma} -> {fmt}"
        );
    }
    assert!(map.contains("default:\n\t\treturn VIDEO_FORMAT_NONE;"));
    let out = function_body(&s, "static void jxs_output_frame(");
    assert!(out.contains("const enum video_format format = jxs_video_format(f.chroma);"));
    assert!(out.contains("frame.format = format;"));
    assert!(out.contains("VIDEO_CS_709, VIDEO_RANGE_PARTIAL, format,"));
    assert!(
        out.contains("if (format == VIDEO_FORMAT_NONE)"),
        "an unknown sampling is never handed to libobs"
    );
}
