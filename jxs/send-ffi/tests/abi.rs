//! Issue 1387 slice 3 -- the C ABI of jxs-send-ffi is pinned against its hand-written header:
//! the system C compiler compiles `include/camera_box_jxs_send.h` and prints every struct size,
//! field offset and constant, each must equal the Rust side; the header declares exactly the
//! functions the library exports, each `#[no_mangle]`. (That the one DistroAV archive carries
//! these symbols is pinned by jxs-recv-ffi's `tests/abi.rs` shared-object link.)
#![cfg(target_os = "linux")]

use jxs_send_ffi::*;
use std::mem::{offset_of, size_of};
use std::path::PathBuf;
use std::process::Command;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn header() -> String {
    std::fs::read_to_string(crate_dir().join("include/camera_box_jxs_send.h")).unwrap()
}

/// `(C expression, Rust value)` pairs: every size, offset and constant of the header.
fn pins() -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = vec![
        (
            "CBJXS_SEND_ABI_VERSION".into(),
            CBJXS_SEND_ABI_VERSION as usize,
        ),
        ("CBJXS_SEND_I420".into(), CBJXS_SEND_I420 as usize),
        ("CBJXS_SEND_I422".into(), CBJXS_SEND_I422 as usize),
        ("CBJXS_SEND_I444".into(), CBJXS_SEND_I444 as usize),
        ("CBJXS_SEND_NV12".into(), CBJXS_SEND_NV12 as usize),
        ("CBJXS_SEND_LOG_ERROR".into(), 1),
        ("CBJXS_SEND_LOG_WARN".into(), 2),
        ("CBJXS_SEND_LOG_INFO".into(), 3),
        (
            "sizeof(cbjxs_send_config)".into(),
            size_of::<CbjxsSendConfig>(),
        ),
        (
            "sizeof(cbjxs_send_picture)".into(),
            size_of::<CbjxsSendPicture>(),
        ),
        (
            "sizeof(cbjxs_send_stats)".into(),
            size_of::<CbjxsSendStats>(),
        ),
    ];
    macro_rules! off {
        ($c:literal, $t:ty, $($f:ident),*) => {
            $(v.push((format!("offsetof({}, {})", $c, stringify!($f)), offset_of!($t, $f)));)*
        };
    }
    off!(
        "cbjxs_send_config",
        CbjxsSendConfig,
        target,
        cores,
        emit_fps,
        stream_id,
        bpp_min,
        bpp_max,
        reserved
    );
    off!(
        "cbjxs_send_picture",
        CbjxsSendPicture,
        format,
        width,
        height,
        reserved,
        planes,
        strides,
        reserved2,
        timecode_100ns
    );
    off!(
        "cbjxs_send_stats",
        CbjxsSendStats,
        seq,
        window_s,
        frames,
        bytes,
        dropped_busy,
        send_errors,
        encode_errors,
        reports,
        encode_ms_avg,
        encode_ms_max,
        first_slice_ms_avg,
        copy_ms_avg,
        copy_ms_max,
        report_age_s,
        bpp,
        auto_bpp,
        reserved
    );
    v
}

#[test]
fn every_size_offset_and_constant_of_the_header_matches_the_rust_side() {
    let dir = tempfile::tempdir().unwrap();
    let pins = pins();
    let mut c = String::from(
        "#include <stdio.h>\n#include <stddef.h>\n#include \"camera_box_jxs_send.h\"\nint main(void) {\n",
    );
    for (expr, _) in &pins {
        c.push_str(&format!("printf(\"%zu\\n\", (size_t)({expr}));\n"));
    }
    c.push_str("return 0;\n}\n");
    let src = dir.path().join("layout.c");
    let bin = dir.path().join("layout");
    std::fs::write(&src, c).unwrap();
    let inc = format!("-I{}", crate_dir().join("include").display());
    let out = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Werror", &inc])
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("cc runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = String::from_utf8(Command::new(&bin).output().unwrap().stdout).unwrap();
    let got: Vec<usize> = printed.lines().map(|l| l.parse().unwrap()).collect();
    assert_eq!(got.len(), pins.len());
    for ((expr, want), got) in pins.iter().zip(got) {
        assert_eq!(got, *want, "{expr}");
    }
}

/// `name` of every function prototype in `text` that starts with `cbjxs_`.
fn header_functions(text: &str) -> Vec<String> {
    let mut v: Vec<String> = text
        .lines()
        .filter(|l| !l.starts_with(' ') && !l.starts_with('\t') && !l.starts_with('#'))
        .filter(|l| !l.starts_with("typedef"))
        .filter_map(|l| {
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
            "cbjxs_send_abi_version",
            "cbjxs_send_close",
            "cbjxs_send_frame",
            "cbjxs_send_get_stats",
            "cbjxs_send_open",
            "cbjxs_send_set_log",
            "cbjxs_send_take_error",
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
