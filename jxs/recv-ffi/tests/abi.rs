//! Issue 1387 slice 2 -- the C ABI of jxs-recv-ffi is pinned against its hand-written header.
//!
//! 1. The system C compiler compiles `include/camera_box_jxs.h` and prints every struct size and
//!    field offset; each must equal the Rust `#[repr(C)]` twin's.
//! 2. Every function the header declares is exported by the library, and nothing else is.
//! 3. The static library links into a `-Wl,-z,defs` shared object (what DistroAV's MODULE is),
//!    proving it is position-independent and that the native libraries this test names are the
//!    complete list (the strih build reads rustc's own list instead, linux-genlock.yml).
#![cfg(target_os = "linux")]

use jxs_recv_ffi::{CbjxsFrame, CbjxsStats, CBJXS_ABI_VERSION};
use std::mem::{offset_of, size_of};
use std::path::{Path, PathBuf};
use std::process::Command;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn header() -> String {
    std::fs::read_to_string(crate_dir().join("include/camera_box_jxs.h")).unwrap()
}

fn cc(args: &[&str]) {
    let out = Command::new("cc").args(args).output().expect("cc runs");
    assert!(
        out.status.success(),
        "cc {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

const FRAME_FIELDS: [&str; 14] = [
    "width",
    "height",
    "y",
    "u",
    "v",
    "y_stride",
    "uv_stride",
    "timecode_100ns",
    "first_arrival_100ns",
    "last_arrival_100ns",
    "decoded_at_100ns",
    "lost_slices",
    "camera_id",
    "reserved",
];

const STATS_FIELDS: [&str; 17] = [
    "frames",
    "concealed_frames",
    "lost_slices",
    "undecodable_frames",
    "datagrams",
    "bytes",
    "duplicate",
    "stale",
    "malformed",
    "resyncs",
    "foreign",
    "reports_sent",
    "report_errors",
    "header_changes",
    "dropped_unread",
    "camera_id",
    "reserved",
];

fn rust_frame_offset(f: &str) -> usize {
    match f {
        "width" => offset_of!(CbjxsFrame, width),
        "height" => offset_of!(CbjxsFrame, height),
        "y" => offset_of!(CbjxsFrame, y),
        "u" => offset_of!(CbjxsFrame, u),
        "v" => offset_of!(CbjxsFrame, v),
        "y_stride" => offset_of!(CbjxsFrame, y_stride),
        "uv_stride" => offset_of!(CbjxsFrame, uv_stride),
        "timecode_100ns" => offset_of!(CbjxsFrame, timecode_100ns),
        "first_arrival_100ns" => offset_of!(CbjxsFrame, first_arrival_100ns),
        "last_arrival_100ns" => offset_of!(CbjxsFrame, last_arrival_100ns),
        "decoded_at_100ns" => offset_of!(CbjxsFrame, decoded_at_100ns),
        "lost_slices" => offset_of!(CbjxsFrame, lost_slices),
        "camera_id" => offset_of!(CbjxsFrame, camera_id),
        "reserved" => offset_of!(CbjxsFrame, reserved),
        other => panic!("unknown frame field {other}"),
    }
}

fn rust_stats_offset(f: &str) -> usize {
    match f {
        "frames" => offset_of!(CbjxsStats, frames),
        "concealed_frames" => offset_of!(CbjxsStats, concealed_frames),
        "lost_slices" => offset_of!(CbjxsStats, lost_slices),
        "undecodable_frames" => offset_of!(CbjxsStats, undecodable_frames),
        "datagrams" => offset_of!(CbjxsStats, datagrams),
        "bytes" => offset_of!(CbjxsStats, bytes),
        "duplicate" => offset_of!(CbjxsStats, duplicate),
        "stale" => offset_of!(CbjxsStats, stale),
        "malformed" => offset_of!(CbjxsStats, malformed),
        "resyncs" => offset_of!(CbjxsStats, resyncs),
        "foreign" => offset_of!(CbjxsStats, foreign),
        "reports_sent" => offset_of!(CbjxsStats, reports_sent),
        "report_errors" => offset_of!(CbjxsStats, report_errors),
        "header_changes" => offset_of!(CbjxsStats, header_changes),
        "dropped_unread" => offset_of!(CbjxsStats, dropped_unread),
        "camera_id" => offset_of!(CbjxsStats, camera_id),
        "reserved" => offset_of!(CbjxsStats, reserved),
        other => panic!("unknown stats field {other}"),
    }
}

#[test]
fn every_size_and_offset_of_the_header_matches_the_rust_structs() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = String::from("#include <stdio.h>\n#include <stddef.h>\n#include \"camera_box_jxs.h\"\nint main(void) {\n");
    c.push_str("printf(\"abi %u\\n\", CBJXS_ABI_VERSION);\n");
    c.push_str("printf(\"frame.size %zu\\n\", sizeof(cbjxs_frame));\n");
    for f in FRAME_FIELDS {
        c.push_str(&format!(
            "printf(\"frame.{f} %zu\\n\", offsetof(cbjxs_frame, {f}));\n"
        ));
    }
    c.push_str("printf(\"stats.size %zu\\n\", sizeof(cbjxs_stats));\n");
    for f in STATS_FIELDS {
        c.push_str(&format!(
            "printf(\"stats.{f} %zu\\n\", offsetof(cbjxs_stats, {f}));\n"
        ));
    }
    c.push_str("return 0;\n}\n");
    let src = dir.path().join("layout.c");
    let bin = dir.path().join("layout");
    std::fs::write(&src, c).unwrap();
    let inc = format!("-I{}", crate_dir().join("include").display());
    cc(&[
        "-std=c11",
        "-Wall",
        "-Werror",
        &inc,
        src.to_str().unwrap(),
        "-o",
        bin.to_str().unwrap(),
    ]);
    let out = Command::new(&bin).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let got = |key: &str| -> usize {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{key} ")))
            .unwrap_or_else(|| panic!("{key} missing in:\n{text}"))
            .parse()
            .unwrap()
    };
    assert_eq!(got("abi") as u32, CBJXS_ABI_VERSION);
    assert_eq!(got("frame.size"), size_of::<CbjxsFrame>());
    for f in FRAME_FIELDS {
        assert_eq!(
            got(&format!("frame.{f}")),
            rust_frame_offset(f),
            "cbjxs_frame.{f}"
        );
    }
    assert_eq!(got("stats.size"), size_of::<CbjxsStats>());
    for f in STATS_FIELDS {
        assert_eq!(
            got(&format!("stats.{f}")),
            rust_stats_offset(f),
            "cbjxs_stats.{f}"
        );
    }
}

/// `name(` of every function prototype in `text` (lines that start a declaration).
fn header_functions(text: &str) -> Vec<String> {
    let mut v: Vec<String> = text
        .lines()
        .filter(|l| !l.starts_with(' ') && !l.starts_with('\t') && !l.starts_with('#'))
        .filter_map(|l| {
            // the identifier right before the first '(' (after a return type like `cbjxs_receiver *`)
            let head = &l[..l.find('(')?];
            let start = head
                .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .map_or(0, |i| i + 1);
            let name = &head[start..];
            name.starts_with("cbjxs_").then(|| name.to_string())
        })
        .collect();
    v.sort();
    v
}

#[test]
fn the_header_declares_exactly_the_functions_the_library_exports() {
    let declared = header_functions(&header());
    let lib = std::fs::read_to_string(crate_dir().join("src/lib.rs")).unwrap();
    let mut exported: Vec<String> = lib
        .match_indices("pub unsafe extern \"C\" fn ")
        .chain(lib.match_indices("pub extern \"C\" fn "))
        .map(|(i, m)| {
            let rest = &lib[i + m.len()..];
            rest[..rest.find('(').unwrap()].to_string()
        })
        .collect();
    exported.sort();
    assert_eq!(declared, exported);
    assert_eq!(
        declared,
        [
            "cbjxs_abi_version",
            "cbjxs_close",
            "cbjxs_get_stats",
            "cbjxs_local_port",
            "cbjxs_next_frame",
            "cbjxs_open",
            "cbjxs_take_error",
        ]
    );
    for name in &declared {
        let at = lib.find(&format!(" fn {name}(")).unwrap();
        assert!(
            lib[..at]
                .rfind("#[no_mangle]\npub")
                .is_some_and(|a| at - a < 64),
            "{name} is #[no_mangle]"
        );
    }
}

/// The static library cargo built for this test run: `deps/libjxs_recv_ffi-<hash>.a` next to the
/// test binary (a test build never uplifts it to the profile dir). The newest one, should an
/// older build's copy still lie there.
fn staticlib() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let deps: &Path = exe.parent().unwrap();
    let a = std::fs::read_dir(deps)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n.starts_with("libjxs_recv_ffi-") && n.ends_with(".a")
        })
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .unwrap_or_else(|| panic!("no libjxs_recv_ffi-*.a in {}", deps.display()));
    assert!(a.is_file());
    a
}

/// The link flags the strih DistroAV build uses for this library (`vendor/distroav/CMakeLists.txt`):
/// `-z noexecstack` because SVT's nasm objects carry no `.note.GNU-stack`, which otherwise marks
/// the whole shared object as needing an EXECUTABLE stack -- glibc 2.41+ (the Ubuntu 26.04 strih
/// box) then refuses to dlopen it; `--exclude-libs` keeps every Rust and SVT symbol out of the
/// plugin's dynamic symbol table.
fn link_into_shared_object(dir: &Path, flags: &[&str]) -> PathBuf {
    let src = dir.join("use.c");
    std::fs::write(
        &src,
        "#include \"camera_box_jxs.h\"\n\
         int cb_use(void) {\n\
           char err[64];\n\
           cbjxs_receiver *rx = cbjxs_open(0, 0, 0, err, sizeof err);\n\
           cbjxs_frame f; cbjxs_stats s; char e[8];\n\
           int r = cbjxs_next_frame(rx, 0, &f) + cbjxs_get_stats(rx, &s) + (int)cbjxs_take_error(rx, e, sizeof e)\n\
                   + (int)cbjxs_local_port(rx) + (int)cbjxs_abi_version();\n\
           cbjxs_close(rx);\n\
           return r;\n\
         }\n",
    )
    .unwrap();
    let so = dir.join("libuse.so");
    let a = staticlib();
    let inc = format!("-I{}", crate_dir().join("include").display());
    let mut args: Vec<&str> = vec!["-shared", "-fPIC", "-Wl,-z,defs"];
    args.extend_from_slice(flags);
    args.extend_from_slice(&[&inc, src.to_str().unwrap(), a.to_str().unwrap()]);
    // the list `rustc --print native-static-libs` prints for this crate
    args.extend_from_slice(&[
        "-lpthread",
        "-lm",
        "-ldl",
        "-lasound",
        "-lgcc_s",
        "-lutil",
        "-lrt",
        "-lc",
    ]);
    args.extend_from_slice(&["-o", so.to_str().unwrap()]);
    cc(&args);
    so
}

fn tool(cmd: &str, args: &[&str]) -> String {
    let out = Command::new(cmd).args(args).output().expect(cmd);
    assert!(out.status.success(), "{cmd} {args:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn gnu_stack_flags(so: &Path) -> String {
    let ph = tool("readelf", &["-lW", so.to_str().unwrap()]);
    let line = ph
        .lines()
        .find(|l| l.trim_start().starts_with("GNU_STACK"))
        .unwrap_or_else(|| panic!("no GNU_STACK header:\n{ph}"));
    line.split_whitespace().nth(6).unwrap().to_string()
}

#[test]
fn the_static_library_links_into_a_shared_object_with_no_undefined_symbol() {
    let dir = tempfile::tempdir().unwrap();
    let a = staticlib();
    let exclude = format!(
        "-Wl,--exclude-libs,{}",
        a.file_name().unwrap().to_str().unwrap()
    );
    let so = link_into_shared_object(dir.path(), &["-Wl,-z,noexecstack", &exclude]);
    assert_eq!(gnu_stack_flags(&so), "RW", "a non-executable stack");
    let exported = tool("nm", &["-D", "--defined-only", so.to_str().unwrap()]);
    let names: Vec<&str> = exported
        .lines()
        .filter_map(|l| l.split_whitespace().nth(2))
        .collect();
    assert_eq!(names, ["cb_use"], "no library symbol leaks into the plugin");
}
