//! Issue 1387 slice 2 -- the strih OBS JPEG XS source (`vendor/distroav/src/jxs-source.cpp`).
//!
//! CI's strih Linux build is the plugin's first real compile; this gate runs on every push:
//! 1. the pure decisions (`jxs-source-decide.h`) are compiled as-is into a truth table and run;
//! 2. the whole source is syntax-checked with g++ against the vendored libobs headers and the
//!    jxs-recv-ffi header (`-Wall -Wextra -Werror`);
//! 3. the genlock contract is anchored: the I422 frame stamped with timecode x 100 through
//!    `obs_source_output_video`, the genlock exports, an `update()` that never blocks, the
//!    registration and the CMake option that only the strih build turns on;
//! 4. libobs' audit line under a JPEG XS input's name is read by the existing parser, and the
//!    source's own status line is never mistaken for one.

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

const SOURCE: &str = "vendor/distroav/src/jxs-source.cpp";
const DECIDE: &str = "vendor/distroav/src/jxs-source-decide.h";

static RUNS: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> tempfile::TempDir {
    let n = RUNS.fetch_add(1, Ordering::SeqCst);
    tempfile::Builder::new()
        .prefix(&format!("jxs_source_1387-{tag}-{n}-"))
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

const TRUTH_TABLE: &str = r#"
#include <stdio.h>
#include "@DECIDE@"
static int fails = 0;
#define EXPECT(c) do { if (!(c)) { printf("FAIL line %d: %s\n", __LINE__, #c); fails++; } } while (0)
int main(void)
{
	/* the libobs stamp = the sender's genlock timecode x 100 (epoch ns), like ndi-source */
	EXPECT(cb_jxs_timestamp_ns(17618456000000000LL) == 1761845600000000000ULL);
	EXPECT(cb_jxs_timestamp_ns(1) == 100u);
	EXPECT(cb_jxs_timestamp_ns(0) == 0u);
	EXPECT(cb_jxs_timestamp_ns(-5) == 0u);
	EXPECT(cb_jxs_timestamp_ns(INT64_MAX) == 0u);
	/* the genlock latency clamp of ndi-source.cpp: [3, 2000] */
	EXPECT(cb_jxs_clamp_latency_ms(-1) == 3u);
	EXPECT(cb_jxs_clamp_latency_ms(2) == 3u);
	EXPECT(cb_jxs_clamp_latency_ms(3) == 3u);
	EXPECT(cb_jxs_clamp_latency_ms(50) == 50u);
	EXPECT(cb_jxs_clamp_latency_ms(2000) == 2000u);
	EXPECT(cb_jxs_clamp_latency_ms(2001) == 2000u);
	EXPECT(cb_jxs_clamp_latency_ms(-4294967296LL) == 3u);
	/* the settings */
	EXPECT(cb_jxs_port(0) == 7000u);
	EXPECT(cb_jxs_port(65536) == 7000u);
	EXPECT(cb_jxs_port(-1) == 7000u);
	EXPECT(cb_jxs_port(1) == 1u);
	EXPECT(cb_jxs_port(7005) == 7005u);
	EXPECT(cb_jxs_port(65535) == 65535u);
	EXPECT(cb_jxs_camera(0) == 0u);
	EXPECT(cb_jxs_camera(5) == 5u);
	EXPECT(cb_jxs_camera(255) == 255u);
	EXPECT(cb_jxs_camera(256) == 0u);
	EXPECT(cb_jxs_camera(-1) == 0u);
	EXPECT(cb_jxs_max_bpp(8) == 8u);
	EXPECT(cb_jxs_max_bpp(12) == 12u);
	EXPECT(cb_jxs_max_bpp(13) == 0u);
	EXPECT(cb_jxs_max_bpp(-2) == 0u);
	/* only a receiver setting reopens the receiver */
	struct cb_jxs_config a = {7000, 5, 0};
	struct cb_jxs_config b = a;
	EXPECT(!cb_jxs_reopen_needed(a, b));
	b.port = 7001;
	EXPECT(cb_jxs_reopen_needed(a, b));
	b = a; b.camera = 3;
	EXPECT(cb_jxs_reopen_needed(a, b));
	b = a; b.max_bpp = 6;
	EXPECT(!cb_jxs_reopen_needed(a, b)); /* the cap goes into the next report, no reopen */
	/* bits per pixel x 100 from the window's payload: 1080p at 8 bpp = 2073600 bytes a frame */
	EXPECT(cb_jxs_bpp_x100(30ull * 2073600u, 30, 1920, 1080) == 800u);
	EXPECT(cb_jxs_bpp_x100(30ull * 1036800u, 30, 1920, 1080) == 400u);
	EXPECT(cb_jxs_bpp_x100(30ull * 2073600u - 3000u, 30, 1920, 1080) == 800u);
	EXPECT(cb_jxs_bpp_x100(30ull * 2071000u, 30, 1920, 1080) == 799u);
	EXPECT(cb_jxs_bpp_x100(1000, 0, 1920, 1080) == 0u);
	EXPECT(cb_jxs_bpp_x100(1000, 3, 0, 1080) == 0u);
	EXPECT(cb_jxs_bpp_x100(UINT64_MAX, 1, 1, 1) == UINT32_MAX);
	/* the LOCK indicator's connected flag: frames within the last 3 s */
	EXPECT(!cb_jxs_connected(10, 0));
	EXPECT(cb_jxs_connected(5000000000ULL, 2000000001ULL));
	EXPECT(!cb_jxs_connected(5000000000ULL, 2000000000ULL));
	EXPECT(cb_jxs_connected(5, 9));
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
fn the_whole_source_compiles_against_the_vendored_libobs_and_the_ffi_header() {
    let dir = scratch("syntax");
    let inc = dir.path().join("inc");
    fs::create_dir_all(&inc).unwrap();
    fs::write(
        inc.join("obsconfig.h"),
        "#pragma once\n#define OBS_RELEASE_CANDIDATE 0\n#define OBS_BETA 0\n",
    )
    .unwrap();
    let cxx = std::env::var("CXX").unwrap_or_else(|_| "g++".to_string());
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
            .arg(repo(SOURCE)),
        "g++ -fsyntax-only jxs-source.cpp",
    );
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

#[test]
fn the_frame_reaches_libobs_as_i422_stamped_with_the_genlock_timecode_like_an_ndi_frame() {
    let s = read(SOURCE);
    assert!(s.contains("#define JXS_SOURCE_ID \"camera_box_jxs_source\""));
    let out = function_body(&s, "static void jxs_output_frame(");
    for needle in [
        "frame.format = VIDEO_FORMAT_I422;",
        "frame.data[0] = const_cast<uint8_t *>(f.y);",
        "frame.data[1] = const_cast<uint8_t *>(f.u);",
        "frame.data[2] = const_cast<uint8_t *>(f.v);",
        "frame.linesize[1] = f.uv_stride;",
        "frame.timestamp = cb_jxs_timestamp_ns(f.timecode_100ns);",
        "VIDEO_CS_709, VIDEO_RANGE_PARTIAL, VIDEO_FORMAT_I422",
        "obs_source_output_video(s->source, &frame);",
    ] {
        assert!(out.contains(needle), "jxs_output_frame lacks `{needle}`");
    }
    // the thread is the only caller, and it polls the FFI with the bounded timeout
    let thread = function_body(&s, "static void jxs_thread(");
    assert!(thread.contains("cbjxs_next_frame(rx, JXS_POLL_MS, &f)"));
    assert!(thread.contains("jxs_output_frame(s, f);"));
    assert!(s.contains("#define JXS_POLL_MS 100u"));
    let info = function_body(&s, "struct obs_source_info create_jxs_source_info()");
    assert!(info.contains("info.id = JXS_SOURCE_ID;"));
    assert!(info.contains("OBS_SOURCE_ASYNC_VIDEO"));
}

#[test]
fn the_genlock_whitelist_keys_apply_through_the_runtime_resolved_exports() {
    let s = read(SOURCE);
    for export in [
        "\"obs_source_set_genlock_fifo\"",
        "\"obs_source_set_genlock_latency_ms\"",
        "\"obs_source_set_genlock_connected\"",
    ] {
        assert!(s.contains(export), "{export} not resolved");
    }
    // the same keys as ndi-source.cpp's whitelist, so OBS-WS tooling pins them the same way
    let ndi = read("vendor/distroav/src/ndi-source.cpp");
    for key in ["\"genlock_fifo\"", "\"genlock_latency_ms_src\""] {
        assert!(s.contains(key) && ndi.contains(key), "{key}");
    }
    // no burn on this source: the burn sweeps enumerate ndi_source inputs only, a burn left on here
    // would escape every leak guard
    assert!(!s.contains("\"genlock_burn\"") && !s.contains("set_genlock_burn"));
    let defaults = function_body(&s, "static void jxs_source_getdefaults(");
    assert!(defaults.contains("obs_data_set_default_bool(settings, PROP_GENLOCK_FIFO, true);"));
    assert!(defaults.contains(
        "obs_data_set_default_int(settings, PROP_GENLOCK_LATENCY_MS_SRC, CB_JXS_LATENCY_MS_MIN);"
    ));
    let update = function_body(&s, "static void jxs_source_update(");
    assert!(
        update.contains("g.set_fifo(s->source, obs_data_get_bool(settings, PROP_GENLOCK_FIFO));")
    );
    assert!(update.contains(
        "cb_jxs_clamp_latency_ms(obs_data_get_int(settings, PROP_GENLOCK_LATENCY_MS_SRC))"
    ));
    // update runs on the graphics thread: it stores and returns (the issue 1320 render freeze)
    for banned in ["join", "cbjxs_", "sleep", "std::thread"] {
        assert!(!update.contains(banned), "update() must never `{banned}`");
    }
    assert!(update.contains("s->config_gen++;"));
    // the decide header's latency bounds are ndi-source.cpp's
    let decide = read(DECIDE);
    assert!(decide.contains("#define CB_JXS_LATENCY_MS_MIN 3 "));
    assert!(decide.contains("#define CB_JXS_LATENCY_MS_MAX 2000 "));
    assert!(ndi.contains("#define PROP_GENLOCK_LATENCY_MS_MIN 3 "));
    assert!(ndi.contains("#define PROP_GENLOCK_SOURCE_LATENCY_MS_MAX 2000 "));
}

#[test]
fn destroy_stops_and_joins_the_receiver_thread_which_closes_the_receiver() {
    let s = read(SOURCE);
    let destroy = function_body(&s, "static void jxs_source_destroy(");
    let stop = destroy.find("s->running = false;").unwrap();
    let join = destroy.find("s->thread.join();").unwrap();
    let del = destroy.find("delete s;").unwrap();
    assert!(stop < join && join < del);
    let thread = function_body(&s, "static void jxs_thread(");
    assert!(thread.contains("while (s->running.load())"));
    let tail = &thread[thread
        .rfind("if (rx)\n\t\tcbjxs_close(rx);")
        .expect("closed on exit")..];
    assert!(tail.contains("jxs_set_connected(s, false);"));
}

#[test]
fn the_source_is_registered_and_built_only_where_the_strih_build_turns_it_on() {
    let main = read("vendor/distroav/src/plugin-main.cpp");
    assert_eq!(
        main.matches("obs_register_source(&jxs_source_info);")
            .count(),
        1
    );
    let reg = main.find("obs_register_source(&jxs_source_info);").unwrap();
    let guard = main[..reg].rfind("#ifdef CAMERA_BOX_JXS_SOURCE").unwrap();
    assert!(
        !main[guard..reg].contains("#endif"),
        "the registration is guarded"
    );
    let cm = read("vendor/distroav/CMakeLists.txt");
    assert!(cm.contains(
        "option(CAMERA_BOX_JXS_SOURCE \"camera-box issue 1387: build the JPEG XS source (needs jxs-recv-ffi)\" OFF)"
    ));
    let block_start = cm.find("if(CAMERA_BOX_JXS_SOURCE)").unwrap();
    let block = &cm[block_start..block_start + cm[block_start..].find("\nendif()").unwrap()];
    assert_eq!(cm.matches("src/jxs-source.cpp").count(), 1);
    for needle in [
        "target_sources(${CMAKE_PROJECT_NAME} PRIVATE src/jxs-source.cpp",
        "target_compile_definitions(${CMAKE_PROJECT_NAME} PRIVATE CAMERA_BOX_JXS_SOURCE=1)",
        "\"LINKER:-z,noexecstack\"",
        "\"LINKER:--exclude-libs,${_camera_box_jxs_lib_name}\"",
        "message(FATAL_ERROR \"CAMERA_BOX_JXS_SOURCE is Linux-only (issue 1387)\")",
        "CAMERA_BOX_JXS_FFI_NATIVE_LIBS",
    ] {
        assert!(block.contains(needle), "the CMake block lacks `{needle}`");
    }
}

#[test]
fn the_audit_line_of_a_jpeg_xs_input_parses_and_the_status_line_is_not_one() {
    let line = "info: genlock-fifo audit 'JXS cam5': received=300 consumed=300 underruns=0 \
                dropped_due=0 relocks=0 late_holds=0 backward_steps=0";
    let a = camera_box::jitter_audit::parse_audit_line(line).expect("an audit line");
    assert_eq!(a.source, "JXS cam5");
    assert_eq!((a.received, a.underruns, a.relocks), (300, 0, 0));
    let s = read(SOURCE);
    assert!(s.contains("\"jxs-source '%s': fps=%.2f frames=%\" PRIu64"));
    let status = "info: [distroav] jxs-source 'JXS cam5': fps=30.00 frames=150 lost_slices=0 \
                  concealed=0 undecodable=0 reopens=0 decoder_reopens=0 bpp=8.00 reports=5";
    assert!(camera_box::jitter_audit::parse_audit_line(status).is_none());
}

#[test]
fn the_connected_flag_follows_the_frames_on_every_path_and_max_bpp_never_reopens() {
    let s = read(SOURCE);
    let thread = function_body(&s, "static void jxs_thread(");
    // libobs starts a source connected: start from true so the first evaluation pushes "not
    // connected" for a source no sender has reached
    assert!(thread.contains("bool connected = true;"));
    assert!(!thread.contains("bool connected = false;"));
    // every path that loops evaluates it: the failed open, the failed socket and the poll
    assert!(thread.contains("auto note_connected = [&](uint64_t now) {"));
    assert_eq!(
        thread.matches("note_connected(").count(),
        3,
        "three calls of the lambda"
    );
    for path in [
        "open_failure_logged = true;\n\t\t\t\tnote_connected(os_gettime_ns());",
        "rx = nullptr;\n\t\t\tnote_connected(now);\n\t\t\tjxs_sleep(",
    ] {
        assert!(
            thread.contains(path),
            "a failure path lacks the connected update: {path}"
        );
    }
    // a new max bpp goes through the report, never a reopen
    assert!(thread.contains("cbjxs_set_max_bpp(rx, cfg.max_bpp);"));
    assert!(
        thread.contains("cbjxs_recv_buffer_bytes(rx)"),
        "the start line names the buffer"
    );
}
